//! Minimal EDID 1.x base-block parser.
//!
//! Only the fields needed to identify a physical monitor are decoded: the manufacturer,
//! product code, serial numbers, model name, and manufacture date. That is enough to
//! recognise the same panel arriving over two different cables. The screen size is decoded
//! too, since a GUI that draws monitors to scale needs it and nothing else reports it
//! without a display server.

use crate::error::{Error, Result};

/// The identity of a physical display, decoded from its EDID base block.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EdidId {
    /// Three-letter PNP manufacturer ID, e.g. `DEL`, `BNQ`.
    pub manufacturer: String,
    /// Vendor-assigned product code.
    pub product_code: u16,
    /// 32-bit binary serial number from the base block. Often zero.
    pub serial: u32,
    /// Serial number from a type-0xFF descriptor, if the display provides one.
    pub serial_string: Option<String>,
    /// Model name from a type-0xFC descriptor, if the display provides one.
    pub model: Option<String>,
    /// Year of manufacture.
    pub year: u16,
    /// Week of manufacture, `0` when unspecified.
    pub week: u8,
}

impl EdidId {
    /// A stable, human-typable key that identifies this display.
    ///
    /// Built only from EDID fields, so it survives replugging, reboots, and the monitor
    /// moving to a different port — which is exactly what a saved GUI profile needs.
    pub fn key(&self) -> String {
        let tail = match (&self.serial_string, self.serial) {
            (Some(s), _) if !s.is_empty() && s.bytes().any(|b| b != b'0') => s.clone(),
            (_, 0) => format!("{}w{}", self.year, self.week),
            (_, n) => format!("{n:08X}"),
        };
        format!("{}-{:04X}-{}", self.manufacturer, self.product_code, tail)
    }

    /// The best available human-readable name for this display.
    pub fn display_name(&self) -> String {
        self.model
            .clone()
            .unwrap_or_else(|| format!("{} {:04X}", self.manufacturer, self.product_code))
    }

    /// Whether this identity is too weak to tell two units of the same model apart.
    ///
    /// True when the display reports neither a binary serial nor a meaningful serial
    /// string. Two such monitors of the same model are indistinguishable by EDID alone,
    /// so path grouping has to be treated as a guess.
    pub fn is_ambiguous(&self) -> bool {
        let has_string_serial = self
            .serial_string
            .as_deref()
            .is_some_and(|s| s.bytes().any(|b| b != b'0' && !b.is_ascii_whitespace()));
        self.serial == 0 && !has_string_serial
    }
}

/// A parsed EDID, keeping the raw bytes alongside the decoded identity.
#[derive(Debug, Clone)]
pub struct Edid {
    /// The identity decoded from the base block.
    pub id: EdidId,
    /// Screen size in millimetres, when the display reports one.
    ///
    /// Deliberately kept out of [`EdidId`]: size describes the panel, not which panel it
    /// is, and two units of the same model share it. A GUI drawing monitors to scale wants
    /// it; identity matching does not.
    pub size_mm: Option<(u16, u16)>,
    /// The raw EDID bytes exactly as read.
    pub raw: Vec<u8>,
}

impl Edid {
    /// Parse an EDID base block.
    ///
    /// Accepts blobs longer than 128 bytes (extension blocks are kept in [`Edid::raw`]
    /// but not decoded). The base-block checksum is verified.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 128 {
            return Err(Error::Edid(format!(
                "need at least 128 bytes, got {}",
                bytes.len()
            )));
        }
        const MAGIC: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        if bytes[..8] != MAGIC {
            return Err(Error::Edid("bad header magic".into()));
        }
        let sum = bytes[..128].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        if sum != 0 {
            return Err(Error::Edid(format!(
                "base block checksum is {sum:#04x}, expected 0x00"
            )));
        }

        let manufacturer = decode_pnp_id(bytes[8], bytes[9]);
        let product_code = u16::from_le_bytes([bytes[10], bytes[11]]);
        let serial = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        let week = bytes[16];
        // Byte 17 holds the year offset from 1990. A week of 0xFF marks byte 17 as a
        // model year rather than a manufacture year; both decode the same way here.
        let year = 1990 + u16::from(bytes[17]);

        let mut model = None;
        let mut serial_string = None;
        for chunk in bytes[54..126].chunks_exact(18) {
            // A descriptor is a display descriptor (rather than a timing block) when
            // its first two bytes — the pixel clock — are zero.
            if chunk[0] != 0 || chunk[1] != 0 || chunk[2] != 0 {
                continue;
            }
            let text = || decode_descriptor_text(&chunk[5..18]);
            match chunk[3] {
                0xFC => model = text(),
                0xFF => serial_string = text(),
                _ => {}
            }
        }

        Ok(Self {
            id: EdidId {
                manufacturer,
                product_code,
                serial,
                serial_string,
                model,
                year,
                week: if week == 0xFF { 0 } else { week },
            },
            size_mm: decode_size_mm(bytes),
            raw: bytes.to_vec(),
        })
    }

    /// The screen diagonal in inches, when the display reports its size.
    pub fn diagonal_inches(&self) -> Option<f64> {
        let (w, h) = self.size_mm?;
        let (w, h) = (f64::from(w), f64::from(h));
        Some((w * w + h * h).sqrt() / 25.4)
    }
}

/// Decode the packed three-letter PNP manufacturer ID from EDID bytes 8 and 9.
///
/// The two bytes hold three 5-bit letters, big-endian, where 1 maps to `A`.
fn decode_pnp_id(hi: u8, lo: u8) -> String {
    let packed = u16::from_be_bytes([hi, lo]);
    let letter = |shift: u16| -> char {
        let v = ((packed >> shift) & 0x1F) as u8;
        if (1..=26).contains(&v) {
            (b'A' + v - 1) as char
        } else {
            '?'
        }
    };
    [letter(10), letter(5), letter(0)].into_iter().collect()
}

/// Decode the screen size in millimetres.
///
/// The first detailed timing descriptor carries the size to the millimetre, so it is
/// preferred over bytes 21 and 22, which hold the same figure rounded to whole
/// centimetres. Both are zero on displays that decline to report a size at all — projectors
/// and some televisions — and on those the size is genuinely unknown rather than zero.
fn decode_size_mm(bytes: &[u8]) -> Option<(u16, u16)> {
    for chunk in bytes[54..126].chunks_exact(18) {
        // A non-zero pixel clock is what marks a descriptor as a timing block rather than
        // a display descriptor; only timing blocks carry the image size.
        if chunk[0] == 0 && chunk[1] == 0 {
            continue;
        }
        let width = u16::from(chunk[12]) | (u16::from(chunk[14] >> 4) << 8);
        let height = u16::from(chunk[13]) | (u16::from(chunk[14] & 0x0F) << 8);
        if width > 0 && height > 0 {
            return Some((width, height));
        }
    }

    let (width, height) = (u16::from(bytes[21]) * 10, u16::from(bytes[22]) * 10);
    (width > 0 && height > 0).then_some((width, height))
}

/// Decode a 13-byte EDID descriptor string, which is 0x0A-terminated and space-padded.
fn decode_descriptor_text(raw: &[u8]) -> Option<String> {
    let end = raw.iter().position(|&b| b == 0x0A).unwrap_or(raw.len());
    let text: String = raw[..end]
        .iter()
        .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { ' ' })
        .collect();
    let trimmed = text.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic but structurally valid EDID for tests.
    fn make_edid(mfg: [u8; 2], product: u16, serial: u32, name: Option<&str>) -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        e[8] = mfg[0];
        e[9] = mfg[1];
        e[10..12].copy_from_slice(&product.to_le_bytes());
        e[12..16].copy_from_slice(&serial.to_le_bytes());
        e[16] = 4;
        e[17] = 32; // 2022
        e[18] = 1;
        e[19] = 4;
        if let Some(n) = name {
            let d = &mut e[54..72];
            d[3] = 0xFC;
            let bytes = n.as_bytes();
            d[5..5 + bytes.len()].copy_from_slice(bytes);
            if 5 + bytes.len() < 18 {
                d[5 + bytes.len()] = 0x0A;
            }
        }
        let sum = e[..127].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        e[127] = sum.wrapping_neg();
        e
    }

    #[test]
    fn parses_manufacturer_product_and_name() {
        // 0x10AC packs as "DEL".
        let raw = make_edid([0x10, 0xAC], 0x428C, 808990293, Some("DELL G3223Q"));
        let edid = Edid::parse(&raw).expect("valid edid");
        assert_eq!(edid.id.manufacturer, "DEL");
        assert_eq!(edid.id.product_code, 0x428C);
        assert_eq!(edid.id.serial, 808990293);
        assert_eq!(edid.id.model.as_deref(), Some("DELL G3223Q"));
        assert_eq!(edid.id.year, 2022);
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut raw = make_edid([0x10, 0xAC], 1, 1, None);
        raw[127] = raw[127].wrapping_add(1);
        assert!(Edid::parse(&raw).is_err());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut raw = make_edid([0x10, 0xAC], 1, 1, None);
        raw[0] = 0x01;
        assert!(Edid::parse(&raw).is_err());
    }

    /// Overwrite descriptor slot `slot` with a detailed timing block of the given size.
    fn set_timing_descriptor(edid: &mut [u8], slot: usize, width_mm: u16, height_mm: u16) {
        let d = &mut edid[54 + slot * 18..54 + slot * 18 + 18];
        // Any non-zero pixel clock marks this as a timing block rather than a descriptor.
        d[0] = 0x01;
        d[12] = width_mm as u8;
        d[13] = height_mm as u8;
        d[14] = (((width_mm >> 8) as u8) << 4) | ((height_mm >> 8) as u8 & 0x0F);
        let sum = edid[..127].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        edid[127] = sum.wrapping_neg();
    }

    #[test]
    fn physical_size_prefers_the_millimetre_figure_in_a_timing_descriptor() {
        let mut raw = make_edid([0x10, 0xAC], 0x428C, 1, Some("DELL G3223Q"));
        // Whole centimetres in bytes 21/22 disagree slightly, as they do on real displays.
        raw[21] = 71;
        raw[22] = 40;
        set_timing_descriptor(&mut raw, 1, 708, 399);
        let edid = Edid::parse(&raw).expect("valid edid");
        assert_eq!(edid.size_mm, Some((708, 399)));
        // 708 x 399 mm is the 32-inch panel this was captured from.
        assert_eq!(edid.diagonal_inches().map(|d| d.round()), Some(32.0));
    }

    #[test]
    fn physical_size_falls_back_to_the_centimetre_fields() {
        // Displays whose descriptors are all text still report size in bytes 21 and 22.
        let mut raw = make_edid([0x26, 0xCD], 0x9040, 0, Some("27M2V"));
        raw[21] = 60;
        raw[22] = 34;
        let sum = raw[..127].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        raw[127] = sum.wrapping_neg();
        let edid = Edid::parse(&raw).expect("valid edid");
        assert_eq!(edid.size_mm, Some((600, 340)));
    }

    #[test]
    fn a_display_reporting_no_size_yields_none() {
        // Projectors and some televisions leave every size field at zero.
        let raw = make_edid([0x10, 0xAC], 1, 1, None);
        let edid = Edid::parse(&raw).expect("valid edid");
        assert_eq!(edid.size_mm, None);
        assert_eq!(edid.diagonal_inches(), None);
    }

    #[test]
    fn zero_serial_is_flagged_ambiguous() {
        let raw = make_edid([0x26, 0xCD], 0x9040, 0, Some("27M2V"));
        let edid = Edid::parse(&raw).expect("valid edid");
        assert!(edid.id.is_ambiguous());
        // The key must still be stable, falling back to the manufacture date.
        assert!(edid.id.key().ends_with("2022w4"));
    }

    #[test]
    fn serial_number_makes_key_unambiguous() {
        let raw = make_edid([0x10, 0xAC], 0x428C, 808990293, None);
        let edid = Edid::parse(&raw).expect("valid edid");
        assert!(!edid.id.is_ambiguous());
        assert_eq!(edid.id.key(), "DEL-428C-30383655");
    }
}

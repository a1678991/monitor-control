//! Per-model deviations from the MCCS specification, keyed by EDID identity.
//!
//! Entries record hardware findings and model-specific mappings. Any mappings taken
//! from related displays rather than tested hardware are documented at the entry.

use crate::edid::EdidId;
use crate::vcp::InputSource;

/// How a display deviates from standard MCCS behaviour.
#[derive(Debug, Clone, Copy)]
pub struct MonitorQuirks {
    /// Input-source values this display really uses, when they differ from MCCS.
    ///
    /// Writes use these values. Reliable reads use the same table; displays with
    /// unreliable input read-back must not have their reports interpreted as current.
    pub input_codes: &'static [(InputSource, u8)],
    /// Inputs whose mappings were confirmed with a visible signal on this model.
    /// Other configured inputs remain selectable but are not claimed as verified.
    pub verified_inputs: &'static [InputSource],

    /// VCP feature used to write the input selector.
    pub input_vcp: u8,
    /// Source byte inside the input write frame (not the I2C slave address).
    pub input_source_address: u8,
    /// Whether the input register reports the input actually visible on the panel.
    pub input_readback_reliable: bool,

    /// The display's input selector can desynchronise from its video pipeline when
    /// DDC/CI input switches arrive in quick succession, leaving the OSD claiming one
    /// input while the screen shows another (or nothing). Recovery needs a manual input
    /// cycle on the OSD. Callers must therefore write an input switch once and never
    /// retry it in a loop.
    pub fragile_input_switching: bool,
}

/// A quirk entry: which displays it applies to, and what applies.
struct Entry {
    /// EDID manufacturer ID, e.g. `IOC`.
    manufacturer: &'static str,
    /// EDID product code.
    product_code: u16,
    quirks: MonitorQuirks,
}

/// Everything currently known. Kept sorted by manufacturer for readability.
static ENTRIES: &[Entry] = &[
    // LG 32GX870B-B, EDID name "LG ULTRAGEAR+" (2026-09 hardware tests).
    // Private input writes use source byte 0x50 and VCP 0xF4. HDMI2 selects the
    // MacBook; DP1 restores Linux. DDC remains reachable from the inactive DP cable,
    // but 0x60 keeps reporting DP1 and 0xF4 keeps reporting 0x06 on either input.
    // HDMI1 (0x90) and USB-C (0xD1) follow the mappings reported for LG 45GX950A-B
    // and other single-DP/USB-C LG displays in ddcutil's LG input-switching wiki.
    // Both ports were disconnected here, so their mappings remain unverified locally.
    Entry {
        manufacturer: "GSM",
        product_code: 0x7889,
        quirks: MonitorQuirks {
            input_codes: &[
                (InputSource::Hdmi1, 0x90),
                (InputSource::Hdmi2, 0x91),
                (InputSource::DisplayPort1, 0xd0),
                (InputSource::UsbC, 0xd1),
            ],
            verified_inputs: &[InputSource::DisplayPort1, InputSource::Hdmi2],
            input_vcp: 0xf4,
            input_source_address: 0x50,
            input_readback_reliable: false,
            fragile_input_switching: false,
        },
    },
    // INNOCN 27M2V (MStar scaler). Verified against hardware 2026-08:
    //   - Reports and accepts input values in a private encoding; the MCCS values its
    //     capabilities string advertises (0x0F/0x10/0x11/0x12) are silently ignored on
    //     write. DisplayPort is 0x08 (confirmed switch and read-back). 0x05, 0x06, 0x07
    //     and 0x09 each select *some* other input, but which is which could not be
    //     safely established because of the desync bug below, so they are not mapped.
    //   - Rapid DDC/CI input switching wedges the scaler: the OSD shows the selected
    //     input while the video pipeline stays on the old one, until a person cycles the
    //     input from the OSD.
    //   - In HDR (BT.2100) mode the brightness register accepts and stores writes but
    //     the panel does not apply them; only the OSD's own control changes the picture.
    Entry {
        manufacturer: "IOC",
        product_code: 0x9040,
        quirks: MonitorQuirks {
            input_codes: &[(InputSource::DisplayPort1, 0x08)],
            verified_inputs: &[InputSource::DisplayPort1],
            input_vcp: 0x60,
            input_source_address: 0x51,
            input_readback_reliable: true,
            fragile_input_switching: true,
        },
    },
];

/// Look up quirks for a display identity.
pub fn for_display(id: &EdidId) -> Option<&'static MonitorQuirks> {
    ENTRIES
        .iter()
        .find(|e| e.manufacturer == id.manufacturer && e.product_code == id.product_code)
        .map(|e| &e.quirks)
}

impl MonitorQuirks {
    /// Translate a named input to the value this display's firmware expects.
    ///
    /// Raw values ([`InputSource::Other`]) pass through untouched — they are the escape
    /// hatch for inputs the table has not mapped yet.
    pub fn encode_input(&self, input: InputSource) -> Option<u8> {
        if let InputSource::Other(v) = input {
            return Some(v);
        }
        self.input_codes
            .iter()
            .find(|(src, _)| *src == input)
            .map(|(_, v)| *v)
    }

    /// Interpret a value the display reported as an input source.
    ///
    /// Values outside the table stay raw rather than being forced through the MCCS
    /// interpretation, which is known to be wrong for displays that have a quirk entry.
    pub fn decode_input(&self, value: u8) -> InputSource {
        self.input_codes
            .iter()
            .find(|(_, v)| *v == value)
            .map(|(src, _)| *src)
            .unwrap_or(InputSource::Other(value))
    }

    /// The inputs this table can address by name.
    pub fn known_inputs(&self) -> impl Iterator<Item = InputSource> + '_ {
        self.input_codes.iter().map(|(src, _)| *src)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn innocn_27m2v() -> EdidId {
        EdidId {
            manufacturer: "IOC".into(),
            product_code: 0x9040,
            serial: 0,
            serial_string: Some("0000000000000".into()),
            model: Some("27M2V".into()),
            year: 2024,
            week: 28,
        }
    }

    #[test]
    fn the_27m2v_is_recognised() {
        let quirks = for_display(&innocn_27m2v()).expect("entry exists");
        assert!(quirks.fragile_input_switching);
    }

    #[test]
    fn displayport_maps_to_the_verified_internal_code() {
        let quirks = for_display(&innocn_27m2v()).unwrap();
        assert_eq!(quirks.encode_input(InputSource::DisplayPort1), Some(0x08));
        assert_eq!(quirks.decode_input(0x08), InputSource::DisplayPort1);
    }

    #[test]
    fn unmapped_named_inputs_are_refused_not_guessed() {
        // Sending the MCCS value for HDMI-1 is known to be ignored, and guessing an
        // internal code risks wedging the scaler; the only honest answer is "unknown".
        let quirks = for_display(&innocn_27m2v()).unwrap();
        assert_eq!(quirks.encode_input(InputSource::Hdmi1), None);
    }

    #[test]
    fn raw_values_always_pass_through() {
        let quirks = for_display(&innocn_27m2v()).unwrap();
        assert_eq!(quirks.encode_input(InputSource::Other(0x06)), Some(0x06));
        // An unmapped report stays raw instead of being mislabelled via MCCS.
        assert_eq!(quirks.decode_input(0x06), InputSource::Other(0x06));
    }

    #[test]
    fn displays_without_an_entry_get_no_quirks() {
        let mut other = innocn_27m2v();
        other.manufacturer = "DEL".into();
        assert!(for_display(&other).is_none());
    }

    #[test]
    fn lg_profile_uses_private_writes_and_does_not_trust_input_reads() {
        let mut id = innocn_27m2v();
        id.manufacturer = "GSM".into();
        id.product_code = 0x7889;
        id.model = Some("LG ULTRAGEAR+".into());
        let q = for_display(&id).unwrap();
        assert_eq!((q.input_vcp, q.input_source_address), (0xf4, 0x50));
        assert_eq!(q.encode_input(InputSource::DisplayPort1), Some(0xd0));
        assert_eq!(q.encode_input(InputSource::Hdmi2), Some(0x91));
        assert_eq!(q.encode_input(InputSource::Hdmi1), Some(0x90));
        assert_eq!(q.encode_input(InputSource::UsbC), Some(0xd1));
        assert_eq!(q.encode_input(InputSource::DisplayPort2), None);
        assert!(!q.input_readback_reliable);
        // The shared LG EDID name must not apply the quirk to other LG models.
        id.product_code = 0x7888;
        assert!(for_display(&id).is_none());
    }
}

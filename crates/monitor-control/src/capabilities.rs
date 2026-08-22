//! Parsing the capabilities string a display advertises.
//!
//! The string is a flat sequence of `name(value)` pairs whose values may themselves nest,
//! for example:
//!
//! ```text
//! (prot(monitor)type(LCD)model(G3223Q)cmds(01 02 03)vcp(10 60(0F 11 12) DF)mccs_ver(2.1))
//! ```
//!
//! The `vcp` segment is the useful part: it lists the features the display implements, and
//! for non-continuous features the exact values it accepts. A GUI can use that to offer
//! only the inputs a monitor really has, instead of the full MCCS table.

use std::collections::BTreeMap;

use crate::vcp::InputSource;

/// A parsed capabilities string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Capabilities {
    /// The `model` field, when present.
    pub model: Option<String>,
    /// The `type` field, e.g. `LCD`.
    pub display_type: Option<String>,
    /// The MCCS version from `mccs_ver`.
    pub mccs_version: Option<String>,
    /// DDC/CI command opcodes the display implements, from `cmds`.
    pub commands: Vec<u8>,
    /// Implemented VCP features, mapped to the discrete values each accepts.
    ///
    /// An empty value list means the feature is continuous, or that the display did not
    /// enumerate its values.
    pub vcp: BTreeMap<u8, Vec<u8>>,
    /// The original string, kept so callers can inspect fields this struct drops.
    pub raw: String,
}

impl Capabilities {
    /// Parse a capabilities string.
    ///
    /// Unparsable input yields a [`Capabilities`] whose fields are empty but whose
    /// [`Capabilities::raw`] is preserved: displays emit malformed capabilities often
    /// enough that failing outright would be less useful than a partial answer.
    pub fn parse(text: &str) -> Self {
        let mut caps = Self { raw: text.to_string(), ..Default::default() };

        // Unwrap the outer pair of parentheses the display normally supplies. The two
        // sides are stripped independently, because a capabilities string truncated
        // mid-transfer keeps its opening paren but loses the closing one, and the fields
        // that did arrive are still worth reporting.
        let mut body = text.trim();
        if let Some(rest) = body.strip_prefix('(') {
            body = rest;
            let opens = body.bytes().filter(|&b| b == b'(').count();
            let closes = body.bytes().filter(|&b| b == b')').count();
            // Only drop a trailing paren that has no opener left to match it, so a body
            // like `vcp(10)` keeps the paren that belongs to `vcp`.
            if closes > opens && let Some(rest) = body.strip_suffix(')') {
                body = rest;
            }
        }

        for (key, value) in segments(body) {
            match key.as_str() {
                "model" => caps.model = Some(value.trim().to_string()),
                "type" => caps.display_type = Some(value.trim().to_string()),
                "mccs_ver" => caps.mccs_version = Some(value.trim().to_string()),
                "cmds" => caps.commands = parse_hex_list(&value),
                "vcp" => caps.vcp = parse_vcp_list(&value),
                _ => {}
            }
        }
        caps
    }

    /// Whether the display implements a VCP feature.
    pub fn supports(&self, code: u8) -> bool {
        self.vcp.contains_key(&code)
    }

    /// The input sources this display accepts, in the order it listed them.
    ///
    /// Empty when the display implements input switching but does not enumerate values,
    /// which is not the same as not supporting it — check [`Capabilities::supports`] with
    /// [`crate::code::INPUT_SOURCE`] to tell those apart.
    pub fn input_sources(&self) -> Vec<InputSource> {
        self.vcp
            .get(&crate::vcp::code::INPUT_SOURCE)
            .map(|values| values.iter().copied().map(InputSource::from_value).collect())
            .unwrap_or_default()
    }
}

/// Split a capabilities body into `name(value)` pairs, respecting nested parentheses.
fn segments(body: &str) -> Vec<(String, String)> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        // A key runs from here to the next '('. Anything before it that is not a key
        // character is separator noise.
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'(' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key = body[key_start..i].trim().to_ascii_lowercase();

        // Walk to the matching close paren, tracking depth so nested groups stay intact.
        let value_start = i + 1;
        let mut depth = 1;
        i += 1;
        while i < bytes.len() && depth > 0 {
            match bytes[i] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        // `i` now sits one past the closing paren, so the value ends just before it.
        let value_end = if depth == 0 { i - 1 } else { bytes.len() };
        if !key.is_empty() {
            out.push((key, body[value_start..value_end].to_string()));
        }
    }
    out
}

/// Parse a whitespace-separated list of hex bytes, ignoring anything unparsable.
fn parse_hex_list(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .filter_map(|t| u8::from_str_radix(t, 16).ok())
        .collect()
}

/// Parse the `vcp(...)` body into features and their enumerated values.
///
/// Entries look like `10` for a continuous feature and `60(0F 11 12)` for one that lists
/// the values it accepts.
fn parse_vcp_list(text: &str) -> BTreeMap<u8, Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = BTreeMap::new();
    let mut i = 0;

    while i < bytes.len() {
        // Skip separators between entries.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
            i += 1;
        }
        if start == i {
            // Not a hex digit and not whitespace: skip it so a stray byte cannot stall
            // the scan.
            i += 1;
            continue;
        }
        let Ok(code) = u8::from_str_radix(&text[start..i], 16) else {
            continue;
        };

        let mut values = Vec::new();
        if i < bytes.len() && bytes[i] == b'(' {
            let value_start = i + 1;
            let mut depth = 1;
            i += 1;
            while i < bytes.len() && depth > 0 {
                match bytes[i] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            let value_end = if depth == 0 { i - 1 } else { bytes.len() };
            values = parse_hex_list(&text[value_start..value_end]);
        }
        out.insert(code, values);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real capabilities string reported by the DELL G3223Q on this machine.
    const DELL: &str = "(prot(monitor)type(LCD)model(G3223Q)cmds(01 02 03 07 0C E3 F3)\
vcp(02 04 05 08 10 12 14(01 05 08 0B 0C) 16 18 1A 52 60(0F 11 12 ) 62 AC AE B2 B6 C6 C8 C9 \
CA CC(02 0A 03 04 08 09 0D 06 ) D6(01 04 05) DC(00 05 ) DF E0 E1 E2(00 20 21 22 2F 04 1E 1F \
0E 12 0B 1B 14 27 23 24 3A ) E3 E4(00 01) E5 E8 E9(00 01 02 21 22 24 29 2A 2D 2E ) EA \
F0(0A 0D 0E 0F 10 11 13 31 32 34 36 ) F1 F2 F4(10 11 12 13 14 15 16 17 20 21 22 30 31 32 33 \
40 41 42 43 44 45 ) FD)mswhql(1)asset_eep(40)mccs_ver(2.1))";

    #[test]
    fn parses_a_real_capabilities_string() {
        let caps = Capabilities::parse(DELL);
        assert_eq!(caps.model.as_deref(), Some("G3223Q"));
        assert_eq!(caps.display_type.as_deref(), Some("LCD"));
        assert_eq!(caps.mccs_version.as_deref(), Some("2.1"));
        assert_eq!(caps.commands, vec![0x01, 0x02, 0x03, 0x07, 0x0C, 0xE3, 0xF3]);
    }

    #[test]
    fn extracts_the_inputs_the_display_actually_has() {
        // This is what lets a GUI show three inputs instead of the whole MCCS table.
        let caps = Capabilities::parse(DELL);
        assert_eq!(
            caps.input_sources(),
            vec![InputSource::DisplayPort1, InputSource::Hdmi1, InputSource::Hdmi2]
        );
    }

    #[test]
    fn continuous_features_parse_with_no_value_list() {
        let caps = Capabilities::parse(DELL);
        assert!(caps.supports(0x10));
        assert_eq!(caps.vcp.get(&0x10), Some(&vec![]));
        assert!(!caps.supports(0x11));
    }

    #[test]
    fn nested_groups_do_not_leak_into_the_next_feature() {
        let caps = Capabilities::parse(DELL);
        // 0x14 lists five values and must not swallow 0x16 that follows it.
        assert_eq!(caps.vcp.get(&0x14), Some(&vec![0x01, 0x05, 0x08, 0x0B, 0x0C]));
        assert!(caps.supports(0x16));
        assert!(caps.supports(0x18));
    }

    #[test]
    fn a_display_listing_no_inputs_yields_an_empty_list() {
        let caps = Capabilities::parse("(vcp(10 12))");
        assert!(caps.input_sources().is_empty());
        assert!(!caps.supports(crate::vcp::code::INPUT_SOURCE));
    }

    #[test]
    fn a_truncated_string_still_yields_what_arrived() {
        // Capabilities arrive in fragments, so a cut-off string is a real case.
        let caps = Capabilities::parse("(vcp(10 60(0F 11");
        assert_eq!(caps.raw, "(vcp(10 60(0F 11");
        assert!(caps.supports(0x10));
        assert_eq!(caps.input_sources(), vec![InputSource::DisplayPort1, InputSource::Hdmi1]);
    }

    #[test]
    fn an_unwrapped_string_keeps_its_last_group() {
        // Without an outer paren, the trailing ')' belongs to `vcp` and must survive.
        let caps = Capabilities::parse("vcp(10 60(0F 11 12))");
        assert!(caps.supports(0x10));
        assert_eq!(
            caps.input_sources(),
            vec![InputSource::DisplayPort1, InputSource::Hdmi1, InputSource::Hdmi2]
        );
    }

    #[test]
    fn empty_input_does_not_panic() {
        assert_eq!(Capabilities::parse("").vcp.len(), 0);
        assert_eq!(Capabilities::parse("()").vcp.len(), 0);
    }
}

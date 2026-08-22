//! VCP feature codes and the named values that go with them.

use crate::error::Error;

/// Well-known MCCS VCP feature codes.
pub mod code {
    /// Brightness / luminance, continuous.
    pub const BRIGHTNESS: u8 = 0x10;
    /// Contrast, continuous.
    pub const CONTRAST: u8 = 0x12;
    /// Red video gain, continuous.
    pub const RED_GAIN: u8 = 0x16;
    /// Green video gain, continuous.
    pub const GREEN_GAIN: u8 = 0x18;
    /// Blue video gain, continuous.
    pub const BLUE_GAIN: u8 = 0x1A;
    /// Input source select, non-continuous.
    pub const INPUT_SOURCE: u8 = 0x60;
    /// Speaker volume, continuous.
    pub const VOLUME: u8 = 0x62;
    /// Power mode, non-continuous.
    pub const POWER_MODE: u8 = 0xD6;
    /// MCCS version the display implements.
    pub const VCP_VERSION: u8 = 0xDF;
}

/// A human-readable label for a well-known VCP code.
pub fn code_name(code: u8) -> Option<&'static str> {
    Some(match code {
        code::BRIGHTNESS => "Brightness",
        code::CONTRAST => "Contrast",
        code::RED_GAIN => "Red gain",
        code::GREEN_GAIN => "Green gain",
        code::BLUE_GAIN => "Blue gain",
        code::INPUT_SOURCE => "Input source",
        code::VOLUME => "Volume",
        code::POWER_MODE => "Power mode",
        code::VCP_VERSION => "VCP version",
        _ => return None,
    })
}

/// A value of VCP feature `0x60`, Input Source.
///
/// The standard MCCS values are named; anything else is preserved in
/// [`InputSource::Other`], because vendors routinely define their own — USB-C inputs in
/// particular are unstandardised and differ between manufacturers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(into = "String"))]
pub enum InputSource {
    /// Analog video (R/G/B) 1, MCCS `0x01`.
    Vga1,
    /// Analog video (R/G/B) 2, MCCS `0x02`.
    Vga2,
    /// Digital video (DVI) 1, MCCS `0x03`.
    Dvi1,
    /// Digital video (DVI) 2, MCCS `0x04`.
    Dvi2,
    /// Composite video 1, MCCS `0x05`.
    Composite1,
    /// Composite video 2, MCCS `0x06`.
    Composite2,
    /// S-Video 1, MCCS `0x07`.
    SVideo1,
    /// S-Video 2, MCCS `0x08`.
    SVideo2,
    /// Tuner 1, MCCS `0x09`.
    Tuner1,
    /// Tuner 2, MCCS `0x0A`.
    Tuner2,
    /// Tuner 3, MCCS `0x0B`.
    Tuner3,
    /// Component video (YPbPr) 1, MCCS `0x0C`.
    Component1,
    /// Component video (YPbPr) 2, MCCS `0x0D`.
    Component2,
    /// Component video (YPbPr) 3, MCCS `0x0E`.
    Component3,
    /// DisplayPort 1, MCCS `0x0F`.
    DisplayPort1,
    /// DisplayPort 2, MCCS `0x10`.
    DisplayPort2,
    /// HDMI 1, MCCS `0x11`.
    Hdmi1,
    /// HDMI 2, MCCS `0x12`.
    Hdmi2,
    /// A value outside the MCCS table, kept verbatim.
    Other(u8),
}

/// Every standard input, paired with its MCCS value and the names accepted for it.
///
/// The first name in each list is the canonical one used when printing.
const INPUT_TABLE: &[(InputSource, u8, &[&str])] = &[
    (InputSource::Vga1, 0x01, &["vga1", "vga", "analog1"]),
    (InputSource::Vga2, 0x02, &["vga2", "analog2"]),
    (InputSource::Dvi1, 0x03, &["dvi1", "dvi"]),
    (InputSource::Dvi2, 0x04, &["dvi2"]),
    (InputSource::Composite1, 0x05, &["composite1", "composite"]),
    (InputSource::Composite2, 0x06, &["composite2"]),
    (InputSource::SVideo1, 0x07, &["svideo1", "svideo"]),
    (InputSource::SVideo2, 0x08, &["svideo2"]),
    (InputSource::Tuner1, 0x09, &["tuner1", "tuner"]),
    (InputSource::Tuner2, 0x0A, &["tuner2"]),
    (InputSource::Tuner3, 0x0B, &["tuner3"]),
    (InputSource::Component1, 0x0C, &["component1", "component"]),
    (InputSource::Component2, 0x0D, &["component2"]),
    (InputSource::Component3, 0x0E, &["component3"]),
    (InputSource::DisplayPort1, 0x0F, &["dp1", "dp", "displayport1", "displayport"]),
    (InputSource::DisplayPort2, 0x10, &["dp2", "displayport2"]),
    (InputSource::Hdmi1, 0x11, &["hdmi1", "hdmi"]),
    (InputSource::Hdmi2, 0x12, &["hdmi2"]),
];

impl InputSource {
    /// The MCCS value for this input.
    pub fn value(self) -> u8 {
        match self {
            Self::Other(v) => v,
            other => INPUT_TABLE
                .iter()
                .find(|(src, _, _)| *src == other)
                .map(|(_, v, _)| *v)
                .expect("every non-Other variant appears in INPUT_TABLE"),
        }
    }

    /// Interpret an MCCS value as an input source.
    ///
    /// Only the low byte of VCP `0x60` carries meaning; the caller is responsible for
    /// having discarded the high byte.
    pub fn from_value(value: u8) -> Self {
        INPUT_TABLE
            .iter()
            .find(|(_, v, _)| *v == value)
            .map(|(src, _, _)| *src)
            .unwrap_or(Self::Other(value))
    }

    /// The canonical name for this input.
    pub fn name(self) -> String {
        match self {
            Self::Other(v) => format!("{v:#04x}"),
            other => INPUT_TABLE
                .iter()
                .find(|(src, _, _)| *src == other)
                .map(|(_, _, names)| names[0].to_string())
                .expect("every non-Other variant appears in INPUT_TABLE"),
        }
    }

    /// Every canonical input name, for help text and GUI pickers.
    pub fn all_names() -> impl Iterator<Item = (&'static str, u8)> {
        INPUT_TABLE.iter().map(|(_, v, names)| (names[0], *v))
    }
}

impl std::str::FromStr for InputSource {
    type Err = Error;

    /// Parse an input by name (`hdmi1`, `dp2`) or by raw value (`0x11`, `17`).
    ///
    /// Raw values are accepted so vendor-specific inputs stay reachable without the
    /// crate needing a table for every manufacturer.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let norm: String = s
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| !matches!(c, '-' | '_' | ' '))
            .collect();

        if let Some((src, _, _)) = INPUT_TABLE
            .iter()
            .find(|(_, _, names)| names.contains(&norm.as_str()))
        {
            return Ok(*src);
        }

        let raw = norm
            .strip_prefix("0x")
            .map(|hex| u8::from_str_radix(hex, 16))
            .unwrap_or_else(|| norm.parse::<u8>());

        raw.map(Self::from_value).map_err(|_| {
            Error::Parse(format!(
                "unknown input source `{s}`; expected a name such as `hdmi1` or `dp1`, \
                 or a raw value such as `0x11`"
            ))
        })
    }
}

impl std::fmt::Display for InputSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name())
    }
}

impl From<InputSource> for String {
    fn from(v: InputSource) -> Self {
        v.name()
    }
}

/// How a requested value relates to the current one.
///
/// Lets a caller say "10 brighter" without first reading the display, which is what
/// makes a `brightness +10` hotkey a single command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueChange {
    /// Set the feature to exactly this value.
    Absolute(u16),
    /// Add this signed amount to the current value.
    Relative(i32),
    /// Set the feature to this percentage of its advertised maximum.
    Percent(u8),
}

impl ValueChange {
    /// Resolve this change against a display's current value and maximum.
    ///
    /// The result is always clamped into `0..=maximum`, so a relative step at either end
    /// of the range saturates instead of wrapping.
    pub fn resolve(self, current: u16, maximum: u16) -> u16 {
        match self {
            Self::Absolute(v) => v.min(maximum),
            Self::Relative(delta) => (i64::from(current) + i64::from(delta))
                .clamp(0, i64::from(maximum)) as u16,
            Self::Percent(p) => {
                let scaled = f64::from(maximum) * f64::from(p.min(100)) / 100.0;
                scaled.round() as u16
            }
        }
    }
}

impl std::str::FromStr for ValueChange {
    type Err = Error;

    /// Parse `50`, `+10`, `-10`, or `75%`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Some(pct) = s.strip_suffix('%') {
            return pct
                .trim()
                .parse::<u8>()
                .map(Self::Percent)
                .map_err(|_| Error::Parse(format!("`{s}` is not a percentage between 0 and 100")));
        }
        // A leading sign is what distinguishes a step from an absolute value, so it has
        // to be checked before parsing rather than being stripped as whitespace would be.
        if s.starts_with('+') || s.starts_with('-') {
            return s
                .parse::<i32>()
                .map(Self::Relative)
                .map_err(|_| Error::Parse(format!("`{s}` is not a signed step")));
        }
        s.parse::<u16>()
            .map(Self::Absolute)
            .map_err(|_| {
                Error::Parse(format!(
                    "`{s}` is not a value; expected `50`, `+10`, `-10`, or `75%`"
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_names_match_the_shell_scripts_they_replace() {
        // The scripts being replaced used 0x0f, 0x11, and 0x12 directly.
        assert_eq!("dp1".parse::<InputSource>().unwrap().value(), 0x0F);
        assert_eq!("hdmi1".parse::<InputSource>().unwrap().value(), 0x11);
        assert_eq!("hdmi2".parse::<InputSource>().unwrap().value(), 0x12);
    }

    #[test]
    fn input_parsing_ignores_separators_and_case() {
        for s in ["HDMI-1", "hdmi_1", "Hdmi 1", "hdmi1"] {
            assert_eq!(s.parse::<InputSource>().unwrap(), InputSource::Hdmi1);
        }
    }

    #[test]
    fn raw_input_values_stay_reachable() {
        // Vendor-specific inputs such as Dell's USB-C have no standard name.
        assert_eq!("0x1b".parse::<InputSource>().unwrap(), InputSource::Other(0x1B));
        assert_eq!("27".parse::<InputSource>().unwrap(), InputSource::Other(0x1B));
        assert_eq!(InputSource::Other(0x1B).name(), "0x1b");
    }

    #[test]
    fn unknown_input_names_are_rejected() {
        assert!("thunderbolt".parse::<InputSource>().is_err());
        assert!("999".parse::<InputSource>().is_err());
    }

    #[test]
    fn every_named_input_round_trips() {
        for (name, value) in InputSource::all_names() {
            let parsed = name.parse::<InputSource>().expect("name parses");
            assert_eq!(parsed.value(), value);
            assert_eq!(parsed.name(), name);
            assert_eq!(InputSource::from_value(value), parsed);
        }
    }

    #[test]
    fn relative_changes_clamp_at_both_ends() {
        assert_eq!(ValueChange::Relative(10).resolve(95, 100), 100);
        assert_eq!(ValueChange::Relative(-10).resolve(5, 100), 0);
        assert_eq!(ValueChange::Relative(10).resolve(50, 100), 60);
    }

    #[test]
    fn absolute_values_clamp_to_the_advertised_maximum() {
        assert_eq!(ValueChange::Absolute(200).resolve(50, 100), 100);
        assert_eq!(ValueChange::Absolute(80).resolve(50, 100), 80);
    }

    #[test]
    fn percentages_scale_against_a_non_hundred_maximum() {
        assert_eq!(ValueChange::Percent(50).resolve(0, 80), 40);
        assert_eq!(ValueChange::Percent(100).resolve(0, 80), 80);
    }

    #[test]
    fn value_change_parsing_distinguishes_steps_from_absolutes() {
        assert_eq!("50".parse::<ValueChange>().unwrap(), ValueChange::Absolute(50));
        assert_eq!("+10".parse::<ValueChange>().unwrap(), ValueChange::Relative(10));
        assert_eq!("-10".parse::<ValueChange>().unwrap(), ValueChange::Relative(-10));
        assert_eq!("75%".parse::<ValueChange>().unwrap(), ValueChange::Percent(75));
    }
}

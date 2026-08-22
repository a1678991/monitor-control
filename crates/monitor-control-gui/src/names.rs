//! Words for things the library names in its own terms.
//!
//! The library speaks the command line's vocabulary — `dp1`, `hdmi2` — because that is what
//! `monitorctl` takes on a command line. A window is read by a person, so it says
//! "DisplayPort 1". The two are kept apart deliberately: a command copied out of this window
//! has to use the token, and a label in it has to use the name.

use monitor_control::InputSource;

use crate::state::MonitorId;

/// The full name of an input, as a person would say it.
pub fn input_name(input: InputSource) -> String {
    use InputSource::*;
    match input {
        Vga1 => "VGA 1".into(),
        Vga2 => "VGA 2".into(),
        Dvi1 => "DVI 1".into(),
        Dvi2 => "DVI 2".into(),
        Composite1 => "Composite 1".into(),
        Composite2 => "Composite 2".into(),
        SVideo1 => "S-Video 1".into(),
        SVideo2 => "S-Video 2".into(),
        Tuner1 => "Tuner 1".into(),
        Tuner2 => "Tuner 2".into(),
        Tuner3 => "Tuner 3".into(),
        Component1 => "Component 1".into(),
        Component2 => "Component 2".into(),
        Component3 => "Component 3".into(),
        DisplayPort1 => "DisplayPort 1".into(),
        DisplayPort2 => "DisplayPort 2".into(),
        Hdmi1 => "HDMI 1".into(),
        Hdmi2 => "HDMI 2".into(),
        // A vendor-specific value, which no table can name. Saying "Unknown input" and giving
        // the number is honest; printing the number twice, as `0x1b (0x1b)`, is not.
        Other(value) => format!("Unknown input {value:#04x}"),
    }
}

/// The short form that fits on a chip.
pub fn input_chip(input: InputSource) -> String {
    use InputSource::*;
    match input {
        DisplayPort1 => "DP 1".into(),
        DisplayPort2 => "DP 2".into(),
        Hdmi1 => "HDMI 1".into(),
        Hdmi2 => "HDMI 2".into(),
        Dvi1 => "DVI 1".into(),
        Dvi2 => "DVI 2".into(),
        Vga1 => "VGA 1".into(),
        Vga2 => "VGA 2".into(),
        Other(value) => format!("{value:#04x}"),
        other => input_name(other),
    }
}

/// The `monitorctl` command that reproduces one monitor's brightness.
///
/// Long `--key`, never `-k`, and never `--bus`: these lines end up in a keybinding
/// configuration where they have to be readable a year later, and a bus number is exactly the
/// thing this project exists to stop people hard-coding.
pub fn brightness_command(id: &MonitorId, percent: f32, key_is_unique: bool) -> String {
    format!(
        "monitorctl brightness set {}% {}",
        percent.round() as i32,
        long_selector(id, key_is_unique)
    )
}

/// The command that sets every monitor to a percentage of its own maximum.
pub fn master_command(percent: f32) -> String {
    format!("monitorctl brightness set {}%", percent.round() as i32)
}

/// The command that switches one monitor's input.
pub fn input_command(id: &MonitorId, input: InputSource, key_is_unique: bool) -> String {
    format!("monitorctl input set {} {}", input.name(), long_selector(id, key_is_unique))
}

/// The command that sets one monitor's contrast.
pub fn contrast_command(id: &MonitorId, value: u16, key_is_unique: bool) -> String {
    format!("monitorctl contrast set {value} {}", long_selector(id, key_is_unique))
}

/// The command that sets one monitor's speaker volume.
///
/// Through `vcp` because `monitorctl` has no `volume` subcommand, and a copied command that
/// does not exist is worse than a longer one that does.
pub fn volume_command(id: &MonitorId, value: u16, key_is_unique: bool) -> String {
    format!(
        "monitorctl vcp set {:#04x} {value} {}",
        monitor_control::code::VOLUME,
        long_selector(id, key_is_unique)
    )
}

/// How a command addresses one monitor.
fn long_selector(id: &MonitorId, key_is_unique: bool) -> String {
    if key_is_unique {
        format!("--key {}", id.key)
    } else {
        // Two displays of the same model with no serial number share a key. `Selector` ANDs its
        // fields, so naming the cable as well is what makes the command land on the right one.
        format!("--key {} --connector {}", id.key, id.connector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> MonitorId {
        MonitorId { key: "DEL-428C-F1W36H3".into(), connector: "card1-DP-3".into() }
    }

    #[test]
    fn a_unique_key_is_the_whole_selector() {
        assert_eq!(
            brightness_command(&id(), 44.0, true),
            "monitorctl brightness set 44% --key DEL-428C-F1W36H3"
        );
    }

    #[test]
    fn a_shared_key_names_the_cable_as_well() {
        // Two identical displays with no serial number produce the same key, and a command
        // that could land on either of them is worse than a longer one.
        assert_eq!(
            brightness_command(&id(), 44.0, false),
            "monitorctl brightness set 44% --key DEL-428C-F1W36H3 --connector card1-DP-3"
        );
    }

    #[test]
    fn volume_goes_through_the_vcp_subcommand() {
        // `monitorctl` has no `volume` subcommand; a copied command that does not run is worse
        // than a longer one that does.
        assert_eq!(
            volume_command(&id(), 18, true),
            "monitorctl vcp set 0x62 18 --key DEL-428C-F1W36H3"
        );
    }

    #[test]
    fn a_vendor_input_is_named_once_not_twice() {
        assert_eq!(input_name(InputSource::Other(0x1B)), "Unknown input 0x1b");
        assert_eq!(input_chip(InputSource::Other(0x1B)), "0x1b");
    }

    #[test]
    fn commands_use_the_command_line_token_not_the_readable_name() {
        assert_eq!(
            input_command(&id(), InputSource::Hdmi2, true),
            "monitorctl input set hdmi2 --key DEL-428C-F1W36H3"
        );
        assert_eq!(input_name(InputSource::Hdmi2), "HDMI 2");
    }
}

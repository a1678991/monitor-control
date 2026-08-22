//! Command-line surface definition.

use clap::{Args, Parser, Subcommand};
use monitor_control::{InputSource, Selector, ValueChange};

/// Control monitor brightness and input source over DDC/CI.
#[derive(Debug, Parser)]
#[command(
    name = "monitorctl",
    version,
    about,
    long_about = "Control monitor brightness and input source over DDC/CI.\n\n\
        Monitors reachable over more than one cable are grouped into a single display, \
        and commands automatically use whichever path actually carries DDC/CI. Run \
        `monitorctl paths` to see every route and why the unusable ones were ruled out.",
    after_help = "With no selector, a command applies to every detected monitor.\n\n\
        EXAMPLES:\n  \
        monitorctl detect                     Show monitors and their working paths\n  \
        monitorctl paths                      Show every route, including dead ones\n  \
        monitorctl brightness set 50          Set all monitors to 50\n  \
        monitorctl brightness set +10         Step every monitor up by 10\n  \
        monitorctl brightness set 80 -m DELL  Set only the DELL\n  \
        monitorctl input set hdmi2 -m DELL    Switch the DELL to HDMI 2\n  \
        monitorctl vcp get 0xDF               Read a raw VCP feature"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    #[command(flatten)]
    pub global: GlobalOpts,

    #[command(flatten)]
    pub selector: SelectorOpts,
}

/// Options accepted by every subcommand.
#[derive(Debug, Args, Clone)]
pub struct GlobalOpts {
    /// Emit machine-readable JSON instead of formatted text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Retries for each DDC/CI exchange before a path is judged dead.
    #[arg(long, global = true, default_value_t = 3, value_name = "N")]
    pub retries: u32,

    /// Scale every DDC/CI delay. Raise it for displays that answer unreliably.
    #[arg(long, global = true, default_value_t = 1.0, value_name = "FACTOR")]
    pub sleep_multiplier: f64,

    /// Reject replies whose checksum does not use the specification's seed.
    #[arg(long, global = true)]
    pub strict_checksum: bool,

    /// Treat each connector as its own monitor instead of grouping by EDID.
    #[arg(long, global = true)]
    pub no_group: bool,

    /// Print what would be sent without touching any monitor.
    #[arg(long, global = true)]
    pub dry_run: bool,
}

/// Flags narrowing a command to particular monitors.
///
/// Every field is `global` so that `brightness set 50 -m DELL` parses the same as
/// `-m DELL brightness set 50`; a selector attached only to the parent command would be
/// rejected once a leaf subcommand such as `set` took over parsing.
#[derive(Debug, Args, Clone, Default)]
pub struct SelectorOpts {
    /// Match monitors whose model name contains this text.
    #[arg(short, long, global = true, value_name = "TEXT")]
    pub model: Option<String>,

    /// Match monitors whose serial number contains this text.
    #[arg(short, long, global = true, value_name = "TEXT")]
    pub serial: Option<String>,

    /// Match the monitor reachable on this I2C bus number.
    #[arg(short, long, global = true, value_name = "N")]
    pub bus: Option<u32>,

    /// Match monitors behind a DRM connector containing this text, e.g. DP-1.
    #[arg(short, long, global = true, value_name = "NAME")]
    pub connector: Option<String>,

    /// Match the monitor with this exact stable key, as shown by `detect`.
    #[arg(short = 'k', long, global = true, value_name = "KEY")]
    pub key: Option<String>,
}

impl From<SelectorOpts> for Selector {
    fn from(o: SelectorOpts) -> Self {
        Self {
            model: o.model,
            serial: o.serial,
            bus: o.bus,
            connector: o.connector,
            key: o.key,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List monitors and the path each one is controlled through.
    Detect {
        /// Probe every path rather than stopping at the first that works.
        #[arg(long)]
        probe_all: bool,
    },

    /// Show every discovered path to each monitor and its probe result.
    Paths,

    /// Get or set brightness (VCP 0x10).
    Brightness {
        #[command(subcommand)]
        action: Option<LevelAction>,
    },

    /// Get or set contrast (VCP 0x12).
    Contrast {
        #[command(subcommand)]
        action: Option<LevelAction>,
    },

    /// Get or set the active input source (VCP 0x60).
    Input {
        #[command(subcommand)]
        action: Option<InputAction>,
    },

    /// Read or write a raw VCP feature.
    Vcp {
        #[command(subcommand)]
        action: VcpAction,
    },

    /// Read the capabilities string a monitor advertises.
    Capabilities {
        /// Print the unparsed string exactly as the display sent it.
        #[arg(long)]
        raw: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum LevelAction {
    /// Read the current value.
    Get,
    /// Write a value: absolute (50), relative (+10, -10), or a percentage (75%).
    Set {
        /// The new value.
        #[arg(value_name = "VALUE", value_parser = parse_value_change)]
        value: ValueChange,
    },
}

#[derive(Debug, Subcommand)]
pub enum InputAction {
    /// Read the current input.
    Get,
    /// Switch to another input, by name (hdmi1, dp1) or raw value (0x11).
    Set {
        /// The input to switch to.
        #[arg(value_name = "INPUT", value_parser = parse_input_source)]
        input: InputSource,
    },
    /// List the input names this tool understands.
    List,
}

#[derive(Debug, Subcommand)]
pub enum VcpAction {
    /// Read a VCP feature.
    Get {
        /// The feature code, e.g. 0x10 or 16.
        #[arg(value_name = "CODE", value_parser = parse_u8_auto)]
        code: u8,
    },
    /// Write a VCP feature.
    Set {
        /// The feature code, e.g. 0x60 or 96.
        #[arg(value_name = "CODE", value_parser = parse_u8_auto)]
        code: u8,

        /// The raw value to write, e.g. 0x11 or 17.
        #[arg(value_name = "VALUE", value_parser = parse_u16_auto)]
        value: u16,
    },
}

/// Parse an integer written in decimal or with a `0x` prefix.
///
/// VCP codes are documented in hex but typed either way, so both are accepted rather than
/// forcing the user to remember which this tool wants.
fn parse_u8_auto(s: &str) -> Result<u8, String> {
    let s = s.trim();
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u8::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.map_err(|_| format!("`{s}` is not a byte; expected 0-255 or 0x00-0xFF"))
}

fn parse_u16_auto(s: &str) -> Result<u16, String> {
    let s = s.trim();
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u16::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.map_err(|_| format!("`{s}` is not a 16-bit value; expected 0-65535 or 0x0000-0xFFFF"))
}

fn parse_value_change(s: &str) -> Result<ValueChange, String> {
    s.parse::<ValueChange>().map_err(|e| e.to_string())
}

fn parse_input_source(s: &str) -> Result<InputSource, String> {
    s.parse::<InputSource>().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn integers_parse_in_decimal_and_hex() {
        assert_eq!(parse_u8_auto("0x10").unwrap(), 16);
        assert_eq!(parse_u8_auto("16").unwrap(), 16);
        assert_eq!(parse_u8_auto("0XDF").unwrap(), 0xDF);
        assert!(parse_u8_auto("300").is_err());
        assert_eq!(parse_u16_auto("0xFFFF").unwrap(), 65535);
    }

    #[test]
    fn a_bare_feature_command_reads_rather_than_writes() {
        // `monitorctl brightness` with no action must not need a value.
        let cli = Cli::try_parse_from(["monitorctl", "brightness"]).expect("parses");
        assert!(matches!(cli.command, Command::Brightness { action: None, .. }));
    }

    #[test]
    fn relative_brightness_steps_parse() {
        let cli = Cli::try_parse_from(["monitorctl", "brightness", "set", "+10"]).expect("parses");
        match cli.command {
            Command::Brightness { action: Some(LevelAction::Set { value }), .. } => {
                assert_eq!(value, ValueChange::Relative(10));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn a_selector_after_a_leaf_subcommand_is_accepted() {
        // The natural phrasing puts the selector last, after `set` has taken over parsing.
        let cli = Cli::try_parse_from(["monitorctl", "brightness", "set", "50", "-m", "DELL"])
            .expect("parses");
        assert_eq!(Selector::from(cli.selector).model.as_deref(), Some("DELL"));
    }

    #[test]
    fn a_selector_before_the_subcommand_is_equally_accepted() {
        let cli = Cli::try_parse_from(["monitorctl", "-m", "DELL", "brightness", "set", "50"])
            .expect("parses");
        assert_eq!(Selector::from(cli.selector).model.as_deref(), Some("DELL"));
    }

    #[test]
    fn global_flags_are_accepted_after_the_subcommand() {
        let cli = Cli::try_parse_from(["monitorctl", "detect", "--json"]).expect("parses");
        assert!(cli.global.json);
    }
}

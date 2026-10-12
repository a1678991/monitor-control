//! Physical monitors, the paths that reach them, and the operations they support.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::capabilities::Capabilities;
use crate::ddc::{DdcConfig, I2cDdc, VcpValue};
use crate::discovery::{self, ConnectorKind, PathSource};
use crate::edid::{Edid, EdidId};
use crate::error::{DdcError, Error, Result};
use crate::quirks::{self, MonitorQuirks};
use crate::vcp::{self, InputSource, ValueChange, code};

/// What a probe learned about one path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "state", rename_all = "kebab-case"))]
pub enum PathStatus {
    /// Not yet probed.
    #[default]
    Unprobed,
    /// The display answers DDC/CI over this path.
    Working {
        /// The MCCS version reported by feature `0xDF`, when the display implements it.
        vcp_version: Option<(u8, u8)>,
    },
    /// The display is on the bus but returns a null message.
    ///
    /// Nearly always means DDC/CI is switched off in the monitor's OSD menu. Unlike
    /// [`PathStatus::Unresponsive`], this path would work if the user changed a setting.
    DdcDisabled,
    /// Nothing answered on this path.
    Unresponsive,
    /// The path could not be opened at all, e.g. for lack of permission.
    Unavailable {
        /// What went wrong.
        reason: String,
    },
}

impl PathStatus {
    /// Whether DDC/CI commands can be sent over this path.
    pub fn is_working(&self) -> bool {
        matches!(self, Self::Working { .. })
    }

    /// A short label for display.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unprobed => "unprobed",
            Self::Working { .. } => "working",
            Self::DdcDisabled => "ddc-disabled",
            Self::Unresponsive => "unresponsive",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

/// One route from this machine to a monitor: an I2C bus behind a DRM connector.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MonitorPath {
    /// The I2C bus number.
    pub bus: u32,
    /// The I2C device node.
    pub device: PathBuf,
    /// The DRM connector name, e.g. `card1-DP-1`.
    pub connector: String,
    /// The connector's physical kind.
    pub connector_kind: ConnectorKind,
    /// The kernel's name for the I2C adapter.
    pub adapter_name: Option<String>,
    /// How this path was discovered.
    pub source: PathSource,
    /// What the last probe found.
    pub status: PathStatus,
}

impl MonitorPath {
    /// A compact description, e.g. `card1-DP-1 via /dev/i2c-9`.
    pub fn describe(&self) -> String {
        format!("{} via /dev/i2c-{}", self.connector, self.bus)
    }
}

/// A physical monitor, together with every path that reaches it.
///
/// A monitor with two cables to the same machine appears once here, with one entry in
/// [`Monitor::paths`] per cable. Only some of those paths may carry DDC/CI, which is why
/// operations go through [`Monitor::get_vcp`] rather than being addressed to a bus.
#[derive(Debug)]
pub struct Monitor {
    /// The identity decoded from the display's EDID.
    pub id: EdidId,
    /// Every discovered route to this monitor.
    pub paths: Vec<MonitorPath>,
    /// The raw EDID, kept for callers that need fields this crate does not decode.
    pub edid: Vec<u8>,
    /// Set when this monitor's EDID is too weak to distinguish it from another unit of
    /// the same model, meaning the grouping of paths below it is a guess.
    pub ambiguous_grouping: bool,

    /// Index into `paths` of the route currently used for commands.
    preferred: Option<usize>,
    /// The open channel for `preferred`, kept so bus timing is tracked across calls.
    channel: Option<I2cDdc>,
    config: DdcConfig,
}

impl Monitor {
    /// The best available human-readable name.
    pub fn name(&self) -> String {
        self.id.display_name()
    }

    /// A stable key identifying this monitor across reboots and replugging.
    pub fn key(&self) -> String {
        self.id.key()
    }

    /// The path currently used for commands, if one is known to work.
    pub fn active_path(&self) -> Option<&MonitorPath> {
        self.preferred.map(|i| &self.paths[i])
    }

    /// Whether any path is known to carry DDC/CI.
    pub fn is_controllable(&self) -> bool {
        self.paths.iter().any(|p| p.status.is_working())
    }

    /// Probe every path and record what each one supports.
    ///
    /// Probing puts real traffic on each bus, so it is deliberately separate from
    /// discovery. Once a working path is found, the remaining paths are still probed so
    /// callers can show the user the full picture; use [`Monitor::ensure_path`] instead
    /// to stop at the first path that answers.
    pub fn probe_all(&mut self) {
        for index in 0..self.paths.len() {
            let status = self.probe_path(index);
            self.paths[index].status = status;
            if self.preferred.is_none() && self.paths[index].status.is_working() {
                self.preferred = Some(index);
            }
        }
    }

    /// Find a working path, probing lazily and stopping at the first success.
    ///
    /// This is the cheap path for a GUI that only wants to set brightness: it avoids
    /// touching buses it does not need.
    pub fn ensure_path(&mut self) -> Result<usize> {
        if let Some(index) = self.preferred
            && self.paths[index].status.is_working()
        {
            return Ok(index);
        }

        // Prefer paths that already answered, then ones never tried. Paths known to be
        // dead come last but are still retried, since a monitor that was asleep or on
        // another input during the last probe may answer now.
        let mut order: Vec<usize> = (0..self.paths.len()).collect();
        order.sort_by_key(|&i| match self.paths[i].status {
            PathStatus::Working { .. } => 0,
            PathStatus::Unprobed => 1,
            PathStatus::DdcDisabled => 2,
            PathStatus::Unresponsive => 3,
            PathStatus::Unavailable { .. } => 4,
        });

        for index in order {
            let status = self.probe_path(index);
            let works = status.is_working();
            self.paths[index].status = status;
            if works {
                self.preferred = Some(index);
                return Ok(index);
            }
        }

        self.preferred = None;
        self.channel = None;
        Err(Error::NoWorkingPath {
            name: self.name(),
            detail: self.path_summary(),
        })
    }

    /// A one-line summary of why no path works, for error messages.
    fn path_summary(&self) -> String {
        if self.paths.is_empty() {
            return "no paths discovered".to_string();
        }
        self.paths
            .iter()
            .map(|p| format!("{}: {}", p.describe(), p.status.label()))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Probe one path by asking for the MCCS version.
    ///
    /// Any well-formed reply proves the link works — including the display saying it does
    /// not implement `0xDF`, which is a protocol-level answer and so still evidence that
    /// DDC/CI is alive.
    fn probe_path(&mut self, index: usize) -> PathStatus {
        // Drop any cached channel for this bus so the probe opens a fresh descriptor.
        self.channel = None;

        let mut channel = match I2cDdc::open_path(
            self.paths[index].device.clone(),
            self.config.clone(),
        ) {
            Ok(c) => c,
            Err(e) => return PathStatus::Unavailable { reason: e.to_string() },
        };

        match channel.get_vcp(code::VCP_VERSION) {
            Ok(v) => PathStatus::Working {
                vcp_version: Some(((v.current >> 8) as u8, v.current as u8)),
            },
            Err(Error::Ddc { source: DdcError::UnsupportedFeature { .. }, .. })
            | Err(Error::Ddc { source: DdcError::FeatureMismatch { .. }, .. }) => {
                PathStatus::Working { vcp_version: None }
            }
            Err(Error::Ddc { source: DdcError::NullMessage, .. }) => PathStatus::DdcDisabled,
            Err(Error::Ddc { .. }) => PathStatus::Unresponsive,
            Err(e) => PathStatus::Unavailable { reason: e.to_string() },
        }
    }

    /// Run an operation over a working path, failing over to another path if one dies.
    ///
    /// A path that worked a moment ago can stop answering when the monitor sleeps or
    /// switches input. Rather than surfacing that as an error, this drops the dead path
    /// and re-runs the operation on the next candidate.
    fn with_channel<T>(
        &mut self,
        op: impl Fn(&mut I2cDdc) -> Result<T>,
    ) -> Result<T> {
        // Two rounds: one over the current path, one over whatever `ensure_path` finds
        // after the first is marked dead. A third would only repeat the second.
        for round in 0..2 {
            let index = self.ensure_path()?;

            if self.channel.as_ref().map(|c| c.path() != self.paths[index].device) != Some(false) {
                self.channel = Some(I2cDdc::open_path(
                    self.paths[index].device.clone(),
                    self.config.clone(),
                )?);
            }

            let channel = self.channel.as_mut().expect("channel opened above");
            match op(channel) {
                Ok(value) => return Ok(value),
                // A definitive answer from the display is not a transport problem, so
                // trying another cable to the same panel would give the same answer.
                Err(e @ Error::Ddc { source: DdcError::UnsupportedFeature { .. }, .. }) => {
                    return Err(e);
                }
                // A persistent null message on one feature means the display declined
                // that request — displays that answer brightness but null on input
                // source exist (INNOCN 27M2V). The path still works; failing it over
                // would take down features that were fine a moment ago.
                Err(e @ Error::Ddc { source: DdcError::NullMessage, .. }) => {
                    return Err(e);
                }
                Err(e) => {
                    self.channel = None;
                    self.paths[index].status = match &e {
                        Error::Ddc { .. } => PathStatus::Unresponsive,
                        other => PathStatus::Unavailable { reason: other.to_string() },
                    };
                    if self.preferred == Some(index) {
                        self.preferred = None;
                    }
                    if round == 1 {
                        return Err(e);
                    }
                }
            }
        }
        unreachable!("the loop returns on both rounds")
    }

    /// Send a state-changing command once. A failed transfer must not be replayed on
    /// another path: the display may already have switched away during the transfer.
    fn with_channel_once<T>(
        &mut self,
        op: impl FnOnce(&mut I2cDdc) -> Result<T>,
    ) -> Result<T> {
        let index = self.ensure_path()?;
        if self.channel.as_ref().map(|c| c.path() != self.paths[index].device) != Some(false) {
            self.channel = Some(I2cDdc::open_path(
                self.paths[index].device.clone(),
                self.config.clone(),
            )?);
        }
        let result = op(self.channel.as_mut().expect("channel opened above"));
        if result.is_err() {
            self.channel = None;
            self.preferred = None;
            self.paths[index].status = PathStatus::Unprobed;
        }
        result
    }

    /// Read a VCP feature.
    pub fn get_vcp(&mut self, code: u8) -> Result<VcpValue> {
        self.with_channel(|c| c.get_vcp(code))
    }

    /// Write a VCP feature, verifying it landed when the display allows.
    ///
    /// Single writes get silently dropped on flaky scalers (measured at roughly 1 in 12
    /// on an MStar-based display), so the write is read back and retried a bounded number
    /// of times. Two cases intentionally skip verification:
    ///
    /// - Input source (`0x60` or a model's private selector): switching input usually
    ///   takes this path down with it, and at least one scaler wedges — OSD saying one
    ///   input, screen showing another — when switches arrive in quick succession.
    ///   Input is written exactly once.
    /// - Displays that cannot report the feature back (null message or unsupported): the
    ///   write is sent and trusted.
    pub fn set_vcp(&mut self, code: u8, value: u16) -> Result<()> {
        if code == code::INPUT_SOURCE || self.quirks().is_some_and(|q| q.input_vcp == code) {
            return self.with_channel_once(move |c| c.set_vcp(code, value));
        }

        // One initial write plus two verified retries puts the odds of all three
        // dropping (at the observed ~8% drop rate) below one in a thousand.
        const WRITE_ATTEMPTS: u32 = 3;

        let mut last_seen = None;
        for _ in 0..WRITE_ATTEMPTS {
            self.with_channel(move |c| c.set_vcp(code, value))?;
            match self.get_vcp(code) {
                Ok(read_back) if read_back.current == value => return Ok(()),
                Ok(read_back) => last_seen = Some(read_back.current),
                // The display cannot report this feature back; trust the write.
                Err(Error::Ddc {
                    source: DdcError::UnsupportedFeature { .. } | DdcError::NullMessage,
                    ..
                }) => return Ok(()),
                // The path died mid-verify; with_channel already recorded that, and the
                // next attempt's set will fail over or surface the error.
                Err(_) => last_seen = None,
            }
        }

        match last_seen {
            // The display consistently reports a different value: it accepted the frame
            // and refused the change, the way a monitor in HDR mode pins brightness or a
            // scaler ignores values outside its real range. Retrying cannot help, and
            // pretending success would hide exactly the failure the caller asked about.
            Some(actual) => Err(Error::Ddc {
                path: self
                    .active_path()
                    .map(|p| p.device.clone())
                    .unwrap_or_default(),
                source: DdcError::Malformed(format!(
                    "display did not apply the write: {code:#04x} reads back {actual} after                      writing {value}"
                )),
            }),
            None => self.with_channel(move |c| c.set_vcp(code, value)),
        }
    }

    /// Apply a relative, absolute, or percentage change to a continuous feature.
    ///
    /// Reads the current value first so a relative step has something to work from, and
    /// clamps the result to the range the display advertises. Returns the value written.
    pub fn adjust_vcp(&mut self, code: u8, change: ValueChange) -> Result<u16> {
        let current = self.get_vcp(code)?;
        let target = change.resolve(current.current, current.maximum);
        if target > current.maximum {
            return Err(Error::ValueOutOfRange {
                code,
                value: target,
                max: current.maximum,
            });
        }
        self.set_vcp(code, target)?;
        Ok(target)
    }

    /// Read the display's brightness.
    pub fn brightness(&mut self) -> Result<VcpValue> {
        self.get_vcp(code::BRIGHTNESS)
    }

    /// Change the display's brightness, returning the value written.
    pub fn set_brightness(&mut self, change: ValueChange) -> Result<u16> {
        self.adjust_vcp(code::BRIGHTNESS, change)
    }

    /// Read the display's contrast.
    pub fn contrast(&mut self) -> Result<VcpValue> {
        self.get_vcp(code::CONTRAST)
    }

    /// Change the display's contrast, returning the value written.
    pub fn set_contrast(&mut self, change: ValueChange) -> Result<u16> {
        self.adjust_vcp(code::CONTRAST, change)
    }

    /// Quirks recorded for this display model, if any.
    pub fn quirks(&self) -> Option<&'static MonitorQuirks> {
        quirks::for_display(&self.id)
    }

    /// Read the currently selected input.
    ///
    /// Only the low byte of VCP `0x60` is meaningful; the high byte is vendor-specific.
    /// Displays with a quirk entry report in their own encoding, which is translated
    /// here; an unmapped value is returned raw rather than mislabelled.
    /// A display known to return stale input reports yields
    /// [`Error::InputReadbackUnavailable`]; raw [`Monitor::get_vcp`] remains available
    /// for diagnostics.
    pub fn input_source(&mut self) -> Result<InputSource> {
        if self.quirks().is_some_and(|q| !q.input_readback_reliable) {
            return Err(Error::InputReadbackUnavailable { name: self.name() });
        }
        let value = self.get_vcp(code::INPUT_SOURCE)?;
        let low = value.low_byte();
        Ok(match self.quirks() {
            Some(q) => q.decode_input(low),
            None => InputSource::from_value(low),
        })
    }

    /// Switch the display to another input.
    ///
    /// Note that switching a monitor away from this machine's input usually takes the
    /// path this command travelled with it, so a follow-up read will fail. That is the
    /// command working, not failing.
    ///
    /// On displays with a quirk entry the named input is translated to the value the
    /// firmware really uses. Asking for a name the table cannot translate is an error —
    /// sending the standard value instead would be silently ignored at best, and on
    /// fragile scalers an experimental wrong value can wedge the input selector. Raw
    /// values (`InputSource::Other`) always pass through for people probing deliberately.
    pub fn set_input_source(&mut self, input: InputSource) -> Result<()> {
        let value = match self.quirks() {
            Some(q) => q.encode_input(input).ok_or_else(|| {
                let known: Vec<String> =
                    q.known_inputs().map(|i| i.to_string()).collect();
                Error::Parse(format!(
                    "`{}` cannot switch to `{input}` by name: this display uses non-standard \
                     input codes and only [{}] are configured. Raw numeric values remain \
                     available for deliberate experiments",
                    self.name(),
                    if known.is_empty() { "none".to_string() } else { known.join(", ") },
                ))
            })?,
            None => input.value().ok_or_else(|| Error::Parse(format!(
                "`{}` has no model-specific mapping for `{input}`",
                self.name()
            )))?,
        };
        if let Some(q) = self.quirks() {
            return self.with_channel_once(|c| {
                c.set_vcp_with_source(q.input_vcp, u16::from(value), q.input_source_address)
            });
        }
        self.set_vcp(code::INPUT_SOURCE, u16::from(value))
    }

    /// Read the display's capabilities string.
    pub fn capabilities(&mut self) -> Result<String> {
        self.with_channel(|c| c.capabilities())
    }

    /// Read and parse the display's capabilities.
    ///
    /// The parsed form tells a caller which features this particular display implements.
    /// For input switching prefer [`Monitor::available_inputs`]: a capabilities string
    /// can advertise input values the firmware ignores, and that method knows when not
    /// to trust it.
    pub fn parsed_capabilities(&mut self) -> Result<Capabilities> {
        Ok(Capabilities::parse(&self.capabilities()?))
    }

    /// The configured named inputs, preferring model profiles over capabilities.
    ///
    /// For displays with a quirk entry this is the model's configured list — the capabilities
    /// string of such displays advertises standard values the firmware silently ignores,
    /// which is precisely why the quirk exists. For everything else it is the list the
    /// display advertises. An empty answer means the display gave no usable list, not
    /// that switching is impossible; raw values may still work.
    ///
    /// This is the method a GUI input menu should be built from.
    pub fn available_inputs(&mut self) -> Result<Vec<InputSource>> {
        if let Some(q) = self.quirks() {
            return Ok(q.known_inputs().collect());
        }
        Ok(self.parsed_capabilities()?.input_sources())
    }
}

/// Everything attached to this machine, grouped by physical monitor.
#[derive(Debug)]
pub struct MonitorManager {
    monitors: Vec<Monitor>,
}

impl MonitorManager {
    /// Enumerate connected displays without generating any I2C traffic.
    ///
    /// Paths reaching the same physical monitor are grouped together. Grouping keys on the
    /// EDID identity rather than the raw bytes, because a monitor legitimately reports
    /// slightly different EDID extension blocks on HDMI than on DisplayPort.
    pub fn discover(config: DdcConfig) -> Result<Self> {
        Self::build(config, true)
    }

    /// As [`MonitorManager::discover`], but treating every path as its own monitor.
    ///
    /// Useful when two units of the same model report identical EDIDs and would otherwise
    /// be merged into one.
    pub fn discover_ungrouped(config: DdcConfig) -> Result<Self> {
        Self::build(config, false)
    }

    fn build(config: DdcConfig, group: bool) -> Result<Self> {
        let connectors = discovery::discover_connectors()?;

        // A BTreeMap keeps the output order stable between runs, which matters for a CLI
        // whose output people script against.
        let mut groups: BTreeMap<String, (Edid, Vec<MonitorPath>)> = BTreeMap::new();

        for conn in connectors {
            let paths: Vec<MonitorPath> = conn
                .buses
                .into_iter()
                .map(|bus| MonitorPath {
                    bus: bus.bus,
                    device: bus.device,
                    connector: conn.name.clone(),
                    connector_kind: conn.kind,
                    adapter_name: bus.adapter_name,
                    source: bus.source,
                    status: PathStatus::Unprobed,
                })
                .collect();

            let key = if group {
                conn.edid.id.key()
            } else {
                format!("{}@{}", conn.edid.id.key(), conn.name)
            };

            groups
                .entry(key)
                .or_insert_with(|| (conn.edid.clone(), Vec::new()))
                .1
                .extend(paths);
        }

        let monitors = groups
            .into_values()
            .map(|(edid, paths)| {
                // Only warn about weak identity when it actually merged distinct
                // connectors; a single-connector monitor is never mis-grouped.
                let distinct_connectors = paths
                    .iter()
                    .map(|p| p.connector.as_str())
                    .collect::<std::collections::BTreeSet<_>>()
                    .len();
                Monitor {
                    ambiguous_grouping: distinct_connectors > 1 && edid.id.is_ambiguous(),
                    id: edid.id,
                    edid: edid.raw,
                    paths,
                    preferred: None,
                    channel: None,
                    config: config.clone(),
                }
            })
            .collect();

        Ok(Self { monitors })
    }

    /// Probe every path of every monitor.
    pub fn probe_all(&mut self) {
        for m in &mut self.monitors {
            m.probe_all();
        }
    }

    /// The discovered monitors.
    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
    }

    /// The discovered monitors, mutably.
    pub fn monitors_mut(&mut self) -> &mut [Monitor] {
        &mut self.monitors
    }

    /// Take ownership of the discovered monitors, consuming the manager.
    ///
    /// Each [`Monitor`] carries its own paths, channel, and configuration, so once moved
    /// out they can be driven independently — one worker thread per monitor, say, so a
    /// display that is asleep and slow to time out cannot hold up the others. Selection by
    /// [`Selector`] is no longer available afterwards; match on [`Monitor::key`] instead.
    pub fn into_monitors(self) -> Vec<Monitor> {
        self.monitors
    }

    /// Indices of the monitors matching `selector`.
    ///
    /// Returns indices rather than references so the caller can take each monitor mutably
    /// in turn without borrowing the whole manager for the duration.
    pub fn select(&self, selector: &Selector) -> Vec<usize> {
        (0..self.monitors.len())
            .filter(|&i| selector.matches(&self.monitors[i]))
            .collect()
    }

    /// As [`MonitorManager::select`], but an empty result is an error.
    pub fn select_required(&self, selector: &Selector) -> Result<Vec<usize>> {
        let found = self.select(selector);
        if found.is_empty() {
            return Err(Error::NoMatch(selector.describe()));
        }
        Ok(found)
    }
}

/// Which monitors an operation applies to.
///
/// An empty selector matches everything, which makes "set every display's brightness" the
/// default — the behaviour of the shell scripts this replaces.
#[derive(Debug, Clone, Default)]
pub struct Selector {
    /// Case-insensitive substring of the model name.
    pub model: Option<String>,
    /// Case-insensitive substring of the serial number.
    pub serial: Option<String>,
    /// Exact I2C bus number of any of the monitor's paths.
    pub bus: Option<u32>,
    /// Case-insensitive substring of a DRM connector name.
    pub connector: Option<String>,
    /// Exact stable key, as produced by [`EdidId::key`].
    pub key: Option<String>,
}

impl Selector {
    /// Whether every set field matches this monitor.
    pub fn matches(&self, m: &Monitor) -> bool {
        let contains = |haystack: &str, needle: &str| {
            haystack.to_lowercase().contains(&needle.to_lowercase())
        };

        if let Some(model) = &self.model {
            let name = m.id.display_name();
            let raw = m.id.model.clone().unwrap_or_default();
            if !contains(&name, model) && !contains(&raw, model) {
                return false;
            }
        }
        if let Some(serial) = &self.serial {
            let text = m.id.serial_string.clone().unwrap_or_default();
            if !contains(&text, serial) && m.id.serial.to_string() != *serial {
                return false;
            }
        }
        if let Some(bus) = self.bus
            && !m.paths.iter().any(|p| p.bus == bus)
        {
            return false;
        }
        if let Some(connector) = &self.connector
            && !m.paths.iter().any(|p| contains(&p.connector, connector))
        {
            return false;
        }
        if let Some(key) = &self.key
            && !m.id.key().eq_ignore_ascii_case(key)
        {
            return false;
        }
        true
    }

    /// Whether no field is set, meaning this selector matches every monitor.
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.serial.is_none()
            && self.bus.is_none()
            && self.connector.is_none()
            && self.key.is_none()
    }

    /// A human-readable rendering, for error messages.
    pub fn describe(&self) -> String {
        if self.is_empty() {
            return "<all monitors>".to_string();
        }
        let mut parts = Vec::new();
        if let Some(v) = &self.model {
            parts.push(format!("model~{v}"));
        }
        if let Some(v) = &self.serial {
            parts.push(format!("serial~{v}"));
        }
        if let Some(v) = self.bus {
            parts.push(format!("bus={v}"));
        }
        if let Some(v) = &self.connector {
            parts.push(format!("connector~{v}"));
        }
        if let Some(v) = &self.key {
            parts.push(format!("key={v}"));
        }
        parts.join(" ")
    }
}

/// A named VCP code, for the CLI's `vcp` subcommand and GUI feature lists.
pub fn describe_vcp(code: u8) -> String {
    match vcp::code_name(code) {
        Some(name) => format!("{code:#04x} ({name})"),
        None => format!("{code:#04x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lg_stale_input_is_never_reported_as_current() {
        let mut monitor = Monitor {
            id: EdidId {
                manufacturer: "GSM".into(), product_code: 0x7889,
                serial: 1, serial_string: None, model: Some("LG ULTRAGEAR+".into()),
                year: 2026, week: 1,
            },
            paths: vec![], edid: vec![], ambiguous_grouping: false,
            preferred: None, channel: None, config: DdcConfig::default(),
        };
        // No transport is even opened: a successful stale read must not escape.
        assert!(matches!(monitor.input_source(), Err(Error::InputReadbackUnavailable { .. })));
        assert_eq!(monitor.available_inputs().unwrap(), vec![
            InputSource::Hdmi1, InputSource::Hdmi2, InputSource::DisplayPort1, InputSource::UsbC,
        ]);
        assert!(matches!(monitor.set_input_source(InputSource::DisplayPort2), Err(Error::Parse(_))));
        // Ordinary monitors still attempt a real read instead of inheriting this quirk.
        monitor.id.product_code = 0x7888;
        assert!(matches!(monitor.set_input_source(InputSource::UsbC), Err(Error::Parse(_))));
        assert!(matches!(monitor.input_source(), Err(Error::NoWorkingPath { .. })));
    }
}

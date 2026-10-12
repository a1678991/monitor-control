//! What the window knows about each monitor.
//!
//! Deliberately separate from [`monitor_control::Monitor`]: that type owns an open file
//! descriptor and has to stay on the worker thread driving it, while the window needs
//! something it can clone, hold across frames, and draw from without blocking. Everything
//! here is a snapshot a worker sent over a channel.

use std::collections::BTreeMap;

use monitor_control::{Capabilities, EdidId, InputSource, MonitorPath, code};

/// What identifies one monitor to the window.
///
/// The EDID key alone is not enough. With grouping switched off, two connectors carrying the
/// same weak EDID — the same model, no serial number — produce two monitors whose
/// [`monitor_control::Monitor::key`] is the same string. Addressing a worker by key would
/// then send one monitor's writes to the other, so the connector goes in as a tiebreaker.
///
/// Used instead of a position in a list because that position changes when a monitor is
/// plugged in or unplugged, and a write already in flight must not follow the index onto
/// whichever monitor inherited it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MonitorId {
    /// The stable, EDID-derived key, as `monitorctl detect` prints it.
    pub key: String,
    /// The first connector this monitor is reachable on.
    pub connector: String,
}


/// One continuous VCP feature — brightness, contrast, volume — as last read.
#[derive(Debug, Clone)]
pub struct Feature {
    /// The value the display reported.
    pub current: u16,
    /// The largest value the display accepts.
    ///
    /// Read from the display rather than assumed: plenty of monitors report a maximum that
    /// is not 100, and a slider bounded at 100 would refuse the top of their range.
    pub maximum: u16,
    /// Sequence number of the most recent request the worker has finished with.
    ///
    /// Compared against what the window last sent, so a value arriving from a read cannot
    /// undo a newer drag the user is still in the middle of.
    pub applied_seq: u64,
    /// Set when the last exchange for this feature failed.
    ///
    /// The value above is then the last one actually read, which may no longer be true. The
    /// window draws it differently rather than presenting it as fact.
    pub stale: bool,
    /// Why the last attempt on this feature failed, if it did.
    pub error: Option<String>,
}


/// The display's input source, as last read.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    /// The input the display says it is showing.
    pub current: Option<InputSource>,
    /// An input that has been asked for but not confirmed.
    ///
    /// Kept apart from [`InputState::current`] rather than overwriting it, because a switch
    /// that fails would otherwise leave the window claiming an input the display never
    /// moved to.
    pub requested: Option<InputSource>,
    /// Last command sent when the display cannot reliably report its visible input.
    /// This is not a confirmed current input and is not persisted across app restarts.
    pub last_sent: Option<InputSource>,
    /// Sequence number of the most recent switch the worker has finished with.
    pub applied_seq: u64,
    /// Why the last attempt to read or set the input failed, if it did.
    pub error: Option<String>,
    /// Set after a switch that moved the display to another machine's input.
    ///
    /// Not an error: sending a monitor to another input takes the DDC/CI path with it, so
    /// the follow-up read failing is the command having worked.
    pub switched_away: bool,
}

/// How a monitor is answering, reduced to the states that call for different actions.
///
/// Split by *what the user would do next* rather than by severity. A display with DDC/CI
/// switched off needs a walk to its own menu; one whose device node cannot be opened needs a
/// udev rule; one that is silent needs waking. Collapsing any two of those into one colour
/// sends people to the wrong fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// Nothing has been read from it yet.
    Unread,
    /// Commands are getting through.
    Ready,
    /// The display answers but declines, which means DDC/CI is off in its own menu.
    MenuOff,
    /// The device node could not be opened, which means permissions.
    NoPermission,
    /// Nothing answered: asleep, or showing another machine.
    Silent,
    /// Deliberately switched to another input by this window.
    Away,
}


/// Everything the window draws for one monitor.
#[derive(Debug, Clone)]
pub struct MonitorState {
    /// What addresses this monitor.
    pub id: MonitorId,
    /// The identity decoded from the EDID.
    pub edid: EdidId,
    /// Panel size in millimetres, when the EDID reports one.
    pub size_mm: Option<(u16, u16)>,
    /// Set when two connectors reporting the same weak EDID were merged on a guess.
    pub ambiguous_grouping: bool,
    /// Every route to this monitor and what the last probe found on it.
    pub paths: Vec<MonitorPath>,
    /// Index into [`MonitorState::paths`] of the route commands are going over.
    pub active_path: Option<usize>,
    /// Continuous features, keyed by VCP code. Absent means the display does not have it.
    pub features: BTreeMap<u8, Feature>,
    /// The input source.
    pub input: InputState,
    /// The parsed capabilities string, when the display would supply one.
    pub caps: Option<Capabilities>,
    /// Why reading capabilities failed, if it did.
    ///
    /// Worth keeping apart from [`MonitorState::last_error`]: a display can refuse the
    /// capabilities request and still answer every other command, as some do.
    pub caps_error: Option<String>,
    /// The most recent failure that was not specific to one feature.
    pub last_error: Option<String>,
    /// Whether a worker is mid-operation on this monitor.
    pub busy: bool,
    /// Whether this monitor has been read at least once.
    pub loaded: bool,
}

impl MonitorState {
    /// A newly discovered monitor, before anything has been read from it.
    pub fn new(
        id: MonitorId,
        edid: EdidId,
        size_mm: Option<(u16, u16)>,
        ambiguous_grouping: bool,
        paths: Vec<MonitorPath>,
    ) -> Self {
        Self {
            id,
            edid,
            size_mm,
            ambiguous_grouping,
            paths,
            active_path: None,
            features: BTreeMap::new(),
            input: InputState::default(),
            caps: None,
            caps_error: None,
            last_error: None,
            busy: false,
            loaded: false,
        }
    }

    /// The best available human-readable name.
    pub fn name(&self) -> String {
        self.edid.display_name()
    }

    /// How this monitor is answering.
    pub fn condition(&self) -> Condition {
        use monitor_control::PathStatus;

        if self.paths.iter().any(|p| p.status.is_working()) {
            return if self.input.switched_away { Condition::Away } else { Condition::Ready };
        }
        if !self.loaded {
            return Condition::Unread;
        }
        if self.input.switched_away {
            return Condition::Away;
        }
        // A single route that could not be opened is enough: it is the same fix whether the
        // others are dead for that reason or another.
        if self.paths.iter().any(|p| matches!(p.status, PathStatus::Unavailable { .. })) {
            return Condition::NoPermission;
        }
        if self.paths.iter().any(|p| matches!(p.status, PathStatus::DdcDisabled)) {
            return Condition::MenuOff;
        }
        Condition::Silent
    }

    /// The route commands are going over, if one is known to work.
    pub fn active(&self) -> Option<&MonitorPath> {
        self.active_path.and_then(|i| self.paths.get(i))
    }

    /// The connector names this monitor is reachable on, deduplicated and in order.
    pub fn connectors(&self) -> Vec<&str> {
        let mut names: Vec<&str> = Vec::new();
        for path in &self.paths {
            if !names.contains(&path.connector.as_str()) {
                names.push(&path.connector);
            }
        }
        names
    }

    /// The screen diagonal in inches, when the EDID reports a size.
    pub fn diagonal_inches(&self) -> Option<f64> {
        let (w, h) = self.size_mm?;
        let (w, h) = (f64::from(w), f64::from(h));
        Some((w * w + h * h).sqrt() / 25.4)
    }

    /// The inputs to offer in a picker.
    ///
    /// Model quirks take precedence over capabilities, which can advertise inputs the
    /// firmware ignores. Without either source, offer the standard input table.
    ///
    /// The input currently selected is always included, even when the display never listed
    /// it: monitors do report values they did not advertise, and dropping the current value
    /// from its own picker would be worse than showing a number nobody recognises.
    pub fn input_choices(&self) -> Vec<InputSource> {
        let mut choices: Vec<InputSource> =
            if let Some(q) = monitor_control::quirks::for_display(&self.edid) {
                q.known_inputs().collect()
            } else {
                match &self.caps {
                    Some(caps) if !caps.input_sources().is_empty() => caps.input_sources(),
                    _ => InputSource::all_names()
                        .map(|(_, value)| InputSource::from_value(value))
                        .collect(),
                }
            };
        if let Some(current) = self.input.current
            && !choices.contains(&current)
        {
            choices.insert(0, current);
        }
        choices
    }

    /// Whether a quirk or capabilities list is available instead of the full table.
    pub fn enumerated_inputs(&self) -> bool {
        monitor_control::quirks::for_display(&self.edid).is_some()
            || self.caps.as_ref().is_some_and(|c| !c.input_sources().is_empty())
    }

    /// Whether the display implements input switching, as far as anything can tell.
    ///
    /// A refused capabilities string is not evidence either way, so the answer then rests on
    /// whether the input could be read at all.
    pub fn supports_input(&self) -> bool {
        if let Some(q) = monitor_control::quirks::for_display(&self.edid) {
            return !q.input_codes.is_empty();
        }
        match &self.caps {
            Some(caps) => caps.supports(code::INPUT_SOURCE),
            None => self.input.current.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lg_picker_offers_all_four_ports_even_without_a_current_input() {
        let edid = EdidId {
            manufacturer: "GSM".into(), product_code: 0x7889,
            serial: 1, serial_string: None, model: Some("LG ULTRAGEAR+".into()),
            year: 2026, week: 1,
        };
        let mut state = MonitorState::new(
            MonitorId { key: edid.key(), connector: "card1-DP-1".into() },
            edid, None, false, vec![],
        );
        // The LG advertises a misleading list. Neither it nor a missing capabilities
        // reply should hide the configured selector or replace USB-C with DP2.
        for caps in [None, Some(Capabilities::parse("(vcp(60(11 12 0F 10)))"))] {
            state.caps = caps;
            assert!(state.supports_input());
            assert!(state.enumerated_inputs());
            assert_eq!(state.input_choices(), vec![
                InputSource::Hdmi1, InputSource::Hdmi2, InputSource::DisplayPort1, InputSource::UsbC,
            ]);
            assert_eq!(state.input.current, None);
        }
    }
}

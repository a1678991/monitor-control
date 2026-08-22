//! The window: what it knows, and how the pieces fit together.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use eframe::egui;
use monitor_control::{DdcConfig, InputSource, code};
use monitor_layout::{Layout, LayoutWatcher, MonitorIdentity};

use crate::backend::{Backend, Event, Request, access_denied};
use crate::desk::Screen;
use crate::state::{Condition, MonitorId, MonitorState};
use crate::{band, desk, rack, theme};

/// How often to ask the desktop whether the arrangement has changed.
///
/// Every source this can come from is a local socket or a handful of sysfs reads. Nothing here
/// touches an I2C bus or wakes a display, so an idle window costs nothing that matters.
const LAYOUT_POLL: Duration = Duration::from_secs(2);

/// How far one step moves a continuous feature, in the display's own units.
pub const STEP: i32 = 5;

/// How many points of scrolling one step is worth.
///
/// egui smooths a wheel notch across several frames, so each frame carries a fraction of a
/// step; rounding each fraction on its own would round most of them away.
pub const SCROLL_POINTS_PER_STEP: f32 = 30.0;

/// How long a transient note stays in the status rail.
const NOTE_LIFETIME: Duration = Duration::from_secs(5);

/// The little that is worth remembering between runs.
///
/// Deliberately not the brightness levels: those live in the displays, which is where the
/// truth is, and a window that wrote remembered values over them at startup would be changing
/// hardware nobody asked it to touch.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Settings {
    group: bool,
    show_desk: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { group: true, show_desk: true }
    }
}

/// A value written to a display that it has not answered for yet.
///
/// While one of these is outstanding the window shows what was asked for rather than what the
/// last read returned. Without it, dragging a fader would fight the reads coming back: each one
/// would snap the handle to where the display was half a second ago.
#[derive(Debug, Clone, Copy)]
pub struct Pending {
    /// The value being shown.
    pub value: u16,
    /// Which write this was, so an older answer cannot clear a newer request.
    pub seq: u64,
}

/// A drag in progress on the desk plan.
#[derive(Debug, Clone, Copy)]
pub struct DeskDrag {
    /// Which monitor is being dragged.
    pub monitor: usize,
    /// The value it was at when the pointer went down.
    pub anchor_value: u16,
    /// Where the pointer went down.
    pub anchor_y: f32,
    /// How far the pointer has to travel to sweep the whole range, in points.
    ///
    /// The drawn height of the rectangle, not the resolution of the screen it stands for: the
    /// gesture happens on the plan, so a full sweep of the rectangle is a full sweep of the
    /// range. Floored, because a thumbnail-sized rectangle would otherwise turn every pixel
    /// into ten units.
    pub span: f32,
}

/// The window.
pub struct App {
    backend: Option<Backend>,
    /// One entry per monitor, in channel order — which is desk order, left to right.
    pub monitors: Vec<MonitorState>,
    /// The order channels were last assigned in, so numbering does not shuffle under the user.
    order: Vec<MonitorId>,
    /// An order the desk has been reporting that differs from the one in use, and for how long.
    pending_order: Option<(Vec<MonitorId>, u8)>,
    /// Whether the order in use was derived from a real arrangement.
    ///
    /// False until the first one arrives, and the numbering then adopts it at once rather than
    /// waiting out the settling period meant for a source that is flapping.
    order_from_layout: bool,
    /// Where the desktop puts each screen.
    pub layout: Layout,
    /// The screens as they will be drawn, with mirrored ones already merged.
    pub screens: Vec<Screen>,
    /// Which monitor is selected, as an index into `monitors`.
    pub selected: Option<usize>,
    /// Which row has its details open.
    pub expanded: Option<usize>,
    /// Writes that have not come back yet.
    pending: HashMap<(MonitorId, u8), Pending>,
    /// Source of the sequence numbers in [`Pending`].
    seq: u64,
    config: DdcConfig,
    /// Whether connectors are grouped by display.
    pub group: bool,
    /// Why there is nothing to show, when there is nothing to show.
    pub startup_error: Option<String>,
    layout_rx: Receiver<Layout>,
    _watcher: LayoutWatcher,
    wake: Arc<dyn Fn() + Send + Sync>,

    /// Where the master sits, as a percentage. See [`App::set_master_percent`].
    pub master_percent: Option<f32>,
    /// Each monitor's distance from the master when the current drag began.
    ///
    /// Keyed by identity rather than by row: a hotplug part-way through a drag re-sorts the
    /// list, and an offset that followed a row number would then be applied to whichever
    /// monitor inherited it.
    master_offsets: Option<Vec<(MonitorId, f32)>>,
    /// The level a late-answering monitor should join at, and when it was set.
    master_target: Option<(f32, Instant)>,
    /// The monitors this master interaction has already written to.
    ///
    /// Without it, a monitor that was deliberately trimmed below the others would be dragged
    /// back up to the master's own level a moment after every master move — which is exactly
    /// the flattening the offset behaviour exists to prevent.
    master_joined: HashSet<MonitorId>,

    /// Whether the device nodes can be opened at all.
    pub access_denied: bool,
    /// Whether the permission notice is expanded.
    pub show_permission: bool,
    /// Whether the keys card is open.
    pub show_keys: bool,
    /// Whether the desk plan is shown.
    pub show_desk: bool,

    /// A drag in progress on the desk plan.
    pub desk_drag: Option<DeskDrag>,
    /// Wheel movement over a screen that has not yet added up to a step.
    pub scroll_accumulated: f32,
    /// Which screen that accumulated scrolling belongs to.
    pub scroll_over: Option<String>,
    /// Set when the selection came from the plan, so the matching row scrolls in.
    pub scroll_to_selected: bool,
    /// A row whose fader should take keyboard focus on the next frame.
    focus_request: Option<usize>,

    /// A short message for the status rail, and when it was set.
    note: Option<(String, Instant)>,
    /// When something was last copied, so the button can say so.
    copied_at: Option<Instant>,
    /// Text waiting for a frame in which to reach the clipboard.
    pending_clipboard: Option<String>,
}

impl App {
    /// Build the window's state and start reading the displays.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());

        let settings: Settings = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, eframe::APP_KEY))
            .unwrap_or_default();

        let config = DdcConfig::default();
        let (backend, monitors, startup_error) =
            match Backend::start(config.clone(), settings.group, wake.clone()) {
                Ok(backend) => {
                    let monitors = backend.initial_states().to_vec();
                    backend.broadcast(Request::Refresh);
                    (Some(backend), monitors, None)
                }
                Err(e) => (None, Vec::new(), Some(e.to_string())),
            };

        // Deliberately empty. Reading the arrangement means a round trip to the display
        // server, and a compositor that is slow to answer would hold the window closed for as
        // long as it took. The watcher reads immediately on its own thread and delivers the
        // first answer within milliseconds; until then the rows are all there, drawn from
        // sysfs, and only the plan is missing.
        let layout = Layout {
            outputs: Vec::new(),
            source: monitor_layout::LayoutSource::Sysfs,
            warnings: Vec::new(),
        };

        let (layout_tx, layout_rx) = mpsc::channel();
        let watcher_wake = wake.clone();
        let watcher = LayoutWatcher::spawn(LAYOUT_POLL, &layout, move |layout| {
            if layout_tx.send(layout).is_ok() {
                watcher_wake();
            }
        });

        let mut app = Self {
            backend,
            monitors,
            order: Vec::new(),
            pending_order: None,
            order_from_layout: false,
            layout,
            screens: Vec::new(),
            selected: None,
            expanded: None,
            pending: HashMap::new(),
            seq: 0,
            config,
            group: settings.group,
            startup_error,
            layout_rx,
            _watcher: watcher,
            wake,
            master_percent: None,
            master_offsets: None,
            master_target: None,
            master_joined: HashSet::new(),
            access_denied: false,
            show_permission: true,
            show_keys: false,
            show_desk: settings.show_desk,
            desk_drag: None,
            scroll_accumulated: 0.0,
            scroll_over: None,
            scroll_to_selected: false,
            focus_request: None,
            note: None,
            copied_at: None,
            pending_clipboard: None,
        };
        app.recheck_access();
        app.rebuild();
        app.selected = (!app.monitors.is_empty()).then_some(0);
        app
    }

    // ---- lifecycle ------------------------------------------------------

    /// Re-run discovery and start fresh workers.
    pub fn restart(&mut self) {
        // Retired rather than dropped here: joining a worker takes as long as whatever exchange
        // it is in the middle of, and doing that on this thread would freeze the window every
        // time a monitor is plugged in or unplugged.
        if let Some(backend) = self.backend.take() {
            backend.retire();
        }
        self.pending.clear();
        // Every one of these is a position in a list that is about to be replaced.
        self.expanded = None;
        self.desk_drag = None;
        self.focus_request = None;
        self.master_offsets = None;
        self.forget_master_target();
        let selected = self.selected.and_then(|i| self.monitors.get(i)).map(|m| m.id.clone());

        match Backend::start(self.config.clone(), self.group, self.wake.clone()) {
            Ok(backend) => {
                self.monitors = backend.initial_states().to_vec();
                backend.broadcast(Request::Refresh);
                self.backend = Some(backend);
                self.startup_error = None;
            }
            Err(e) => {
                self.monitors.clear();
                self.startup_error = Some(e.to_string());
            }
        }

        // No `detect()` here: the only caller that does not already have a fresh layout is the
        // explicit Refresh, which fetches one itself. Asking again would put a round trip to
        // the display server on the drawing thread every time a monitor is plugged in.
        self.recheck_access();
        self.rebuild();
        // Follow the same physical monitor, not the same position in a list that has changed.
        self.selected = selected
            .and_then(|id| self.monitors.iter().position(|m| m.id == id))
            .or_else(|| (!self.monitors.is_empty()).then_some(0));
    }

    /// Ask every monitor to re-read itself.
    pub fn refresh(&mut self) {
        self.pending.clear();
        if let Some(backend) = &self.backend {
            backend.broadcast(Request::Refresh);
        }
        self.recheck_access();
        self.rebuild();
    }

    /// Ask one monitor to re-read itself.
    pub fn refresh_one(&mut self, index: usize) {
        let Some(id) = self.monitors.get(index).map(|m| m.id.clone()) else { return };
        self.pending.retain(|(key, _), _| key != &id);
        if let Some(backend) = &self.backend {
            backend.send(&id, Request::Refresh);
        }
    }

    /// Probe every route to every monitor.
    pub fn probe_all(&mut self) {
        if let Some(backend) = &self.backend {
            backend.broadcast(Request::ProbeAll);
        }
    }

    /// Switch between one row per display and one row per cable.
    pub fn set_grouping(&mut self, group: bool) {
        if self.group != group {
            self.group = group;
            self.restart();
        }
    }

    /// Whether this account can open the device nodes at all.
    fn recheck_access(&mut self) {
        let paths: Vec<_> = self.monitors.iter().flat_map(|m| m.paths.clone()).collect();
        self.access_denied = access_denied(&paths);
    }

    /// Recompute channel order and the drawable screens.
    fn rebuild(&mut self) {
        self.assign_channels();

        let identities: Vec<MonitorIdentity<'_>> = self
            .monitors
            .iter()
            .map(|m| MonitorIdentity {
                key: std::borrow::Cow::Borrowed(&m.id.key),
                connectors: m.connectors(),
                manufacturer: Some(&m.edid.manufacturer),
                model: m.edid.model.as_deref(),
                serial: m.edid.serial_string.as_deref(),
            })
            .collect();
        let matches = self.layout.match_identities(&identities);

        let mut screens: Vec<Screen> = Vec::new();
        for (output, matched) in self.layout.outputs.iter().zip(&matches) {
            // Two outputs at the same place are showing the same thing, which is what mirroring
            // is; they belong in one rectangle rather than one on top of the other.
            if let Some(existing) = screens
                .iter_mut()
                .find(|s| s.position == output.position && s.size == output.size)
            {
                existing.connectors.push_str(" + ");
                existing.connectors.push_str(&output.connector);
                existing.primary |= output.primary;
                // A second, different panel showing the same thing in the same place is a
                // mirror. It keeps its row and must not be reported as unused.
                if let Some(index) = *matched
                    && existing.monitor != Some(index)
                    && !existing.mirrored.contains(&index)
                {
                    existing.mirrored.push(index);
                }
                continue;
            }
            screens.push(Screen {
                position: output.position,
                size: output.size,
                connectors: output.connector.clone(),
                monitor: *matched,
                mirrored: Vec::new(),
                primary: output.primary,
                mode: output.describe_mode(),
                linked: false,
            });
        }

        // A monitor on two screens is one panel plugged in twice, which is worth saying on both
        // rectangles rather than leaving the user to wonder which one is real.
        for index in 0..screens.len() {
            let Some(monitor) = screens[index].monitor else { continue };
            let count = screens.iter().filter(|s| s.monitor == Some(monitor)).count();
            screens[index].linked = count > 1;
        }

        self.screens = screens;
    }

    /// Number the channels left to right across the desk.
    ///
    /// Sticky: the order is recomputed only when the set of monitors changes, or when the desk
    /// has been reporting a different order for two consecutive polls. A layout source that
    /// flaps would otherwise reshuffle the console the whole design is built on.
    fn assign_channels(&mut self) {
        let desired = self.desk_order();
        let current: Vec<MonitorId> = self.monitors.iter().map(|m| m.id.clone()).collect();

        let same_set: HashSet<&MonitorId> = current.iter().collect();
        let known: HashSet<&MonitorId> = self.order.iter().collect();
        let set_changed = same_set != known;

        let have_layout = !self.layout.outputs.is_empty();
        let adopt = if set_changed || (have_layout && !self.order_from_layout) {
            // Either the monitors changed, or this is the first real arrangement to arrive:
            // the settling period exists to absorb a flapping source, not to make the window
            // spend four seconds numbered in discovery order.
            self.pending_order = None;
            true
        } else if desired == self.order {
            self.pending_order = None;
            false
        } else {
            match &mut self.pending_order {
                Some((order, seen)) if *order == desired => {
                    *seen += 1;
                    *seen >= 2
                }
                _ => {
                    self.pending_order = Some((desired.clone(), 1));
                    false
                }
            }
        };

        if adopt {
            // Silent the first time: numbering the channels is not a renumbering.
            let renumbered = self.order_from_layout && self.order != desired;
            self.order = desired;
            self.order_from_layout = have_layout;
            self.pending_order = None;
            if renumbered {
                self.set_note("channels renumbered");
            }
        }

        // Sort the monitors into the adopted order; anything not in it keeps its relative place
        // at the end, which is where a monitor the desktop is not using belongs.
        let rank = |id: &MonitorId| self.order.iter().position(|o| o == id).unwrap_or(usize::MAX);
        let before: Vec<MonitorId> = self.monitors.iter().map(|m| m.id.clone()).collect();
        let selected = self.selected.and_then(|i| self.monitors.get(i)).map(|m| m.id.clone());
        let expanded = self.expanded.and_then(|i| self.monitors.get(i)).map(|m| m.id.clone());
        self.monitors.sort_by_key(|m| rank(&m.id));

        // Everything that holds a row number has to follow the monitor it meant, or it now
        // points at somebody else.
        let after: Vec<MonitorId> = self.monitors.iter().map(|m| m.id.clone()).collect();
        self.selected = selected.and_then(|id| self.monitors.iter().position(|m| m.id == id));
        self.expanded = expanded.and_then(|id| self.monitors.iter().position(|m| m.id == id));
        if before != after {
            // A drag in flight is abandoned rather than remapped: the pointer is somewhere on a
            // rectangle that has just moved, and continuing it would write to the wrong panel.
            self.desk_drag = None;
            self.focus_request = None;
        }
    }

    /// The order the desk puts the monitors in: left to right, top row first.
    fn desk_order(&self) -> Vec<MonitorId> {
        let identities: Vec<MonitorIdentity<'_>> = self
            .monitors
            .iter()
            .map(|m| MonitorIdentity {
                key: std::borrow::Cow::Borrowed(&m.id.key),
                connectors: m.connectors(),
                manufacturer: Some(&m.edid.manufacturer),
                model: m.edid.model.as_deref(),
                serial: m.edid.serial_string.as_deref(),
            })
            .collect();
        let matches = self.layout.match_identities(&identities);

        // The leftmost, topmost screen a monitor appears on is where it sits on the desk.
        let mut placed: Vec<(usize, (i32, i32))> = Vec::new();
        for (output, matched) in self.layout.outputs.iter().zip(&matches) {
            let Some(index) = matched else { continue };
            let corner = (output.position.1, output.position.0);
            match placed.iter_mut().find(|(i, _)| i == index) {
                Some((_, best)) if corner < *best => *best = corner,
                Some(_) => {}
                None => placed.push((*index, corner)),
            }
        }
        // Band by vertical position so a row of screens with slightly different heights still
        // reads left to right rather than by however many pixels they are offset by.
        let band = 400;
        placed.sort_by_key(|(_, (y, x))| (y / band, *x, *y));

        let mut order: Vec<MonitorId> = placed
            .into_iter()
            .map(|(index, _)| self.monitors[index].id.clone())
            .collect();
        for monitor in &self.monitors {
            if !order.contains(&monitor.id) {
                order.push(monitor.id.clone());
            }
        }
        order
    }

    /// Take everything the workers and the watcher have reported.
    fn drain(&mut self) {
        let mut states = Vec::new();
        if let Some(backend) = &self.backend {
            for Event::State { id, state } in backend.drain() {
                states.push((id, state));
            }
        }
        let arrived = !states.is_empty();
        for (id, state) in states {
            if let Some(slot) = self.monitors.iter_mut().find(|m| m.id == id) {
                *slot = *state;
            }
        }

        let mut relayout = false;
        while let Ok(layout) = self.layout_rx.try_recv() {
            let before = connector_set(&self.layout);
            let after = connector_set(&layout);
            self.layout = layout;
            if before == after {
                relayout = true;
            } else {
                // A different set of connectors means monitors were plugged or unplugged, so
                // the workers are driving the wrong set.
                self.set_note("monitors changed");
                self.restart();
                return;
            }
        }
        if relayout {
            self.rebuild();
        }

        self.clear_settled_writes();
        if arrived {
            self.join_late_monitors();
            self.recheck_access();
            self.resync_master();
        }
    }

    /// Drop pending writes the displays have caught up with.
    fn clear_settled_writes(&mut self) {
        let monitors = &self.monitors;
        self.pending.retain(|(id, feature), pending| {
            let Some(monitor) = monitors.iter().find(|m| &m.id == id) else { return false };
            match monitor.features.get(feature) {
                Some(state) => state.applied_seq < pending.seq,
                // The feature vanished: the display said it does not have it.
                None => false,
            }
        });
    }

    /// Write the master's level to a monitor that answered after the master was moved.
    ///
    /// Stated as a future rather than rendered as a present: until the write lands, the row
    /// says it is joining, and never shows a level it has not read.
    fn join_late_monitors(&mut self) {
        let Some((target, at)) = self.master_target else { return };
        if at.elapsed() > Duration::from_secs(3) {
            self.forget_master_target();
            return;
        }
        for index in 0..self.monitors.len() {
            if self.monitors[index].condition() != Condition::Ready {
                continue;
            }
            // A monitor the master has already written to keeps whatever offset it was given.
            // Only one that was still reading when the master moved joins at the master's own
            // level, because it has no offset to preserve.
            if self.master_joined.contains(&self.monitors[index].id) {
                continue;
            }
            if self.pending_for(index, code::BRIGHTNESS).is_some() {
                continue;
            }
            let maximum = f32::from(self.maximum(index, code::BRIGHTNESS));
            self.master_joined.insert(self.monitors[index].id.clone());
            self.write_level(index, code::BRIGHTNESS, (maximum * target / 100.0).round() as u16);
        }
    }

    /// Stop late arrivals joining a level nobody asked for any more.
    fn forget_master_target(&mut self) {
        self.master_target = None;
        self.master_joined.clear();
    }

    // ---- values ---------------------------------------------------------

    fn pending_for(&self, index: usize, feature: u8) -> Option<&Pending> {
        let id = &self.monitors.get(index)?.id;
        self.pending.get(&(id.clone(), feature))
    }

    /// The value to draw: what was asked for, else what was last read.
    pub fn displayed(&self, index: usize, feature: u8) -> Option<u16> {
        if let Some(pending) = self.pending_for(index, feature) {
            return Some(pending.value);
        }
        Some(self.monitors.get(index)?.features.get(&feature)?.current)
    }

    /// The value the display has actually confirmed, when a write is still outstanding.
    pub fn confirmed(&self, index: usize, feature: u8) -> Option<u16> {
        self.pending_for(index, feature)?;
        Some(self.monitors.get(index)?.features.get(&feature)?.current)
    }

    /// Whether the last exchange for a feature failed, and why.
    pub fn failure(&self, index: usize, feature: u8) -> Option<String> {
        self.monitors.get(index)?.features.get(&feature)?.error.clone()
    }

    /// Whether a feature's value is no longer known to be true.
    pub fn is_stale(&self, index: usize, feature: u8) -> bool {
        self.monitors
            .get(index)
            .and_then(|m| m.features.get(&feature))
            .is_some_and(|f| f.stale)
    }

    /// The largest value a feature accepts on one monitor.
    pub fn maximum(&self, index: usize, feature: u8) -> u16 {
        self.monitors
            .get(index)
            .and_then(|m| m.features.get(&feature))
            .map(|f| f.maximum.max(1))
            .unwrap_or(100)
    }

    /// One monitor's brightness as a percentage of its own maximum.
    pub fn percent_of(&self, index: usize) -> Option<f32> {
        let value = self.displayed(index, code::BRIGHTNESS)?;
        let maximum = f32::from(self.maximum(index, code::BRIGHTNESS));
        (maximum > 0.0).then(|| f32::from(value) * 100.0 / maximum)
    }

    /// The monitors that are answering and have a brightness control.
    pub fn answering_indices(&self) -> Vec<usize> {
        (0..self.monitors.len())
            .filter(|&i| {
                self.monitors[i].condition() == Condition::Ready
                    && self.monitors[i].features.contains_key(&code::BRIGHTNESS)
            })
            .collect()
    }

    /// The mean brightness across the monitors that are answering.
    pub fn average_percent(&self) -> Option<f32> {
        let indices = self.answering_indices();
        if indices.is_empty() {
            return None;
        }
        let total: f32 = indices.iter().filter_map(|&i| self.percent_of(i)).sum();
        Some(total / indices.len() as f32)
    }

    /// The distance between the brightest and dimmest monitor, in percentage points.
    pub fn spread_points(&self) -> f32 {
        let values: Vec<f32> =
            self.answering_indices().iter().filter_map(|&i| self.percent_of(i)).collect();
        match (
            values.iter().cloned().fold(f32::INFINITY, f32::min),
            values.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        ) {
            (lo, hi) if lo.is_finite() && hi.is_finite() => hi - lo,
            _ => 0.0,
        }
    }

    /// Whether this monitor's key alone addresses it unambiguously.
    pub fn key_is_unique(&self, index: usize) -> bool {
        let Some(state) = self.monitors.get(index) else { return true };
        self.monitors.iter().filter(|m| m.id.key == state.id.key).count() == 1
    }

    /// Whether the desktop is using this monitor.
    pub fn on_desk(&self, index: usize) -> bool {
        self.screens
            .iter()
            .any(|s| s.monitor == Some(index) || s.mirrored.contains(&index))
    }

    // ---- writes ---------------------------------------------------------

    /// Write a feature, showing the new value immediately.
    ///
    /// Anything the user does to one monitor ends the master's claim on late arrivals: having
    /// trimmed a display by hand, they do not want the master writing over it a second later.
    pub fn set_level(&mut self, index: usize, feature: u8, value: u16) {
        self.forget_master_target();
        self.write_level(index, feature, value);
    }

    /// Write a feature without disturbing the master's pending level.
    fn write_level(&mut self, index: usize, feature: u8, value: u16) {
        let value = value.min(self.maximum(index, feature));
        if self.displayed(index, feature) == Some(value) {
            return;
        }
        // A tool that changes hardware should be able to say what it changed and why. Off
        // unless asked for; `RUST_BACKTRACE=1` adds the call site.
        if std::env::var_os("MONITORCTL_GUI_TRACE").is_some() {
            eprintln!(
                "write channel {} feature {feature:#04x} -> {value}{}",
                index + 1,
                match std::backtrace::Backtrace::capture().status() {
                    std::backtrace::BacktraceStatus::Captured =>
                        format!("\n{}", std::backtrace::Backtrace::capture()),
                    _ => String::new(),
                }
            );
        }
        let Some(id) = self.monitors.get(index).map(|m| m.id.clone()) else { return };

        self.seq += 1;
        self.pending.insert((id.clone(), feature), Pending { value, seq: self.seq });
        if let Some(backend) = &self.backend {
            backend.send(&id, Request::SetLevel { code: feature, value, seq: self.seq });
        }
    }

    /// Move a feature by a signed amount from where it is being shown.
    pub fn step_level(&mut self, index: usize, feature: u8, delta: i32) {
        let Some(current) = self.displayed(index, feature) else { return };
        let maximum = i64::from(self.maximum(index, feature));
        let target = (i64::from(current) + i64::from(delta)).clamp(0, maximum) as u16;
        self.set_level(index, feature, target);
    }

    /// Switch one monitor to another input.
    ///
    /// Deliberately not optimistic about the result: the requested input is recorded separately
    /// from the current one, so a switch that fails cannot leave the window claiming an input
    /// the display never moved to.
    pub fn set_input(&mut self, index: usize, input: InputSource) {
        let Some(id) = self.monitors.get(index).map(|m| m.id.clone()) else { return };
        self.seq += 1;
        if let Some(state) = self.monitors.get_mut(index) {
            state.input.requested = Some(input);
        }
        if let Some(backend) = &self.backend {
            backend.send(&id, Request::SetInput { input, seq: self.seq });
        }
    }

    // ---- the master -----------------------------------------------------

    /// Record how far each monitor sits from the master, so a drag can preserve it.
    pub fn begin_master_drag(&mut self) {
        let Some(level) = self.master_percent else { return };
        self.master_offsets = Some(
            self.answering_indices()
                .into_iter()
                .filter_map(|i| {
                    let id = self.monitors.get(i)?.id.clone();
                    self.percent_of(i).map(|p| (id, p - level))
                })
                .collect(),
        );
    }

    /// Forget the offsets once the drag ends.
    pub fn end_master_drag(&mut self) {
        self.master_offsets = None;
    }

    /// Move the whole desk to a new level, keeping the differences between monitors.
    ///
    /// An offset rather than a broadcast. Someone who has spent a week trimming one panel five
    /// points below the others should not have that undone by the control they reach for every
    /// evening; flattening is still available, but it is a separate button that says so.
    pub fn set_master_percent(&mut self, level: f32) {
        let level = level.clamp(0.0, 100.0);
        self.master_percent = Some(level);
        self.master_target = Some((level, Instant::now()));
        self.master_joined.clear();

        let offsets = match &self.master_offsets {
            Some(offsets) => offsets.clone(),
            None => {
                self.begin_master_drag();
                self.master_offsets.clone().unwrap_or_default()
            }
        };

        for (id, target) in offset_targets(level, &offsets) {
            let Some(index) = self.monitors.iter().position(|m| m.id == id) else { continue };
            let maximum = f32::from(self.maximum(index, code::BRIGHTNESS));
            self.master_joined.insert(id);
            self.write_level(index, code::BRIGHTNESS, (maximum * target / 100.0).round() as u16);
        }
    }

    /// Step the master by a number of percentage points.
    pub fn nudge_master(&mut self, delta: f32) {
        let Some(level) = self.master_percent else { return };
        self.begin_master_drag();
        self.set_master_percent(level + delta);
        self.end_master_drag();
    }

    /// Set every monitor to exactly the master's percentage, discarding the differences.
    pub fn level_all(&mut self) {
        let Some(level) = self.master_percent else { return };
        self.master_target = Some((level, Instant::now()));
        self.master_joined.clear();
        for index in self.answering_indices() {
            let maximum = f32::from(self.maximum(index, code::BRIGHTNESS));
            if let Some(state) = self.monitors.get(index) {
                self.master_joined.insert(state.id.clone());
            }
            self.write_level(index, code::BRIGHTNESS, (maximum * level / 100.0).round() as u16);
        }
        self.set_note("levelled");
    }

    /// Follow the monitors again once nothing is in flight and nobody is holding the handle.
    fn resync_master(&mut self) {
        if self.master_offsets.is_some() || !self.pending.is_empty() {
            return;
        }
        self.master_percent = self.average_percent().map(f32::round);
    }

    // ---- selection, notes, clipboard ------------------------------------

    /// Select a monitor and scroll its row into view.
    pub fn select(&mut self, index: usize) {
        self.selected = Some(index);
        self.scroll_to_selected = true;
        self.focus_request = Some(index);
    }

    /// Whether this row asked for keyboard focus.
    pub fn take_focus_request(&mut self, index: usize) -> bool {
        if self.focus_request == Some(index) {
            self.focus_request = None;
            return true;
        }
        false
    }

    /// Put text on the clipboard.
    pub fn copy(&mut self, text: String) {
        self.copied_at = Some(Instant::now());
        self.pending_clipboard = Some(text);
    }

    /// Whether something was copied recently enough to say so on a button.
    pub fn copied_recently(&self) -> bool {
        self.copied_at.is_some_and(|at| at.elapsed() < Duration::from_millis(1500))
    }

    /// Say that the plan cannot be rearranged from here.
    ///
    /// Dragging a rectangle sideways is a reasonable thing to try, and silence would leave the
    /// user thinking the window was broken rather than that the gesture belongs elsewhere.
    pub fn set_read_only_note(&mut self) {
        self.set_note("The plan is read-only — arrange screens in your desktop's Display settings");
    }

    /// Show a short message in the status rail.
    fn set_note(&mut self, text: &str) {
        self.note = Some((text.to_string(), Instant::now()));
    }

    /// The status rail's transient message, while it is still fresh.
    pub fn transient_note(&self) -> Option<&str> {
        self.note
            .as_ref()
            .filter(|(_, at)| at.elapsed() < NOTE_LIFETIME)
            .map(|(text, _)| text.as_str())
    }

    /// A value that advances with time, for the busy sweep.
    pub fn animation_phase(&self, ctx: &egui::Context) -> f32 {
        ctx.input(|i| i.time as f32) * 0.6
    }

    /// Whether any worker is mid-exchange.
    pub fn busy(&self) -> bool {
        self.monitors.iter().any(|m| m.busy)
    }

    /// Turn wheel movement over a fader into steps.
    pub fn wheel_over_fader(&mut self, ui: &egui::Ui, index: usize) {
        let delta = ui.input(|i| i.smooth_scroll_delta.y);
        if delta.abs() < 0.01 {
            return;
        }
        // Consumed here, or the scroll area the rack sits in would move as well.
        ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        let key = format!("fader-{index}");
        if self.scroll_over.as_deref() != Some(&key) {
            self.scroll_over = Some(key);
            self.scroll_accumulated = 0.0;
        }
        self.scroll_accumulated += delta;
        while self.scroll_accumulated.abs() >= SCROLL_POINTS_PER_STEP {
            let direction = self.scroll_accumulated.signum();
            self.scroll_accumulated -= direction * SCROLL_POINTS_PER_STEP;
            self.step_level(index, code::BRIGHTNESS, direction as i32 * STEP);
        }
    }

    /// Act on the keys that are not tied to a widget.
    ///
    /// Every bare-key binding is guarded on egui not wanting the keyboard, so typing an exact
    /// value into a readout does not also jump channels and level the desk.
    /// The width left for the fader once the fixed columns and gaps are taken out.
    ///
    /// Measured from the whole row rather than from what is left mid-row: `available_width`
    /// shrinks as cells are allocated, so subtracting the fixed columns from it would take
    /// them off twice.
    pub fn fader_width(&self, total: f32) -> f32 {
        (total - crate::theme::column::fixed() - 6.0 * crate::theme::space::COLUMN).max(120.0)
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};

        let busy_elsewhere = ctx.egui_wants_keyboard_input();

        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            if self.show_keys {
                self.show_keys = false;
            } else if !busy_elsewhere {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        if busy_elsewhere {
            return;
        }

        // The channel number already on screen is the hint; nothing has to be memorised that is
        // not printed.
        const DIGITS: [(Key, usize); 9] = [
            (Key::Num1, 0),
            (Key::Num2, 1),
            (Key::Num3, 2),
            (Key::Num4, 3),
            (Key::Num5, 4),
            (Key::Num6, 5),
            (Key::Num7, 6),
            (Key::Num8, 7),
            (Key::Num9, 8),
        ];
        for (key, index) in DIGITS {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key)) && index < self.monitors.len()
            {
                self.select(index);
            }
        }

        // Vertical arrows are handled here rather than left to egui's focus walk, so "next
        // channel" means the next channel rather than whatever widget happens to be next.
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown))
            && let Some(index) = self.selected
            && index + 1 < self.monitors.len()
        {
            self.select(index + 1);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp))
            && let Some(index) = self.selected
            && index > 0
        {
            self.select(index - 1);
        }

        // Sliders lock the horizontal arrows and leave these, so they are ours to define.
        if let Some(index) = self.selected {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::PageUp)) {
                self.step_level(index, code::BRIGHTNESS, 10);
            }
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::PageDown)) {
                self.step_level(index, code::BRIGHTNESS, -10);
            }
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Home)) {
                self.set_level(index, code::BRIGHTNESS, 0);
            }
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::End)) {
                let maximum = self.maximum(index, code::BRIGHTNESS);
                self.set_level(index, code::BRIGHTNESS, maximum);
            }
        }

        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::R)) {
            self.refresh();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::R))
            && let Some(index) = self.selected
        {
            self.refresh_one(index);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::L)) {
            self.level_all();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::M)) {
            self.show_desk = !self.show_desk;
            self.set_note(if self.show_desk { "desk plan shown (M)" } else { "desk plan hidden (M)" });
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::E))
            && let Some(index) = self.selected
        {
            self.expanded = if self.expanded == Some(index) { None } else { Some(index) };
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::C))
            && let Some(index) = self.selected
            && let Some(percent) = self.percent_of(index)
        {
            let unique = self.key_is_unique(index);
            let command = crate::names::brightness_command(&self.monitors[index].id, percent, unique);
            self.copy(command);
            self.set_note("command copied");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::C))
            && let Some(percent) = self.master_percent
        {
            self.copy(crate::names::master_command(percent));
            self.set_note("command copied");
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Slash))
            || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Questionmark))
        {
            self.show_keys = !self.show_keys;
        }
    }
}

/// Where each monitor lands when the master moves to `level`.
///
/// The whole point of the master: it carries the desk up and down while keeping the distances
/// between the monitors, so a trim that took a week to get right is not undone by the control
/// that gets used every evening. Monitors that run into an end of the range stay there — the
/// alternative, refusing to move any of them, would make the master useless near the ends.
fn offset_targets<T: Clone>(level: f32, offsets: &[(T, f32)]) -> Vec<(T, f32)> {
    offsets
        .iter()
        .map(|(who, offset)| (who.clone(), (level + offset).clamp(0.0, 100.0)))
        .collect()
}

/// Which connectors a layout covers, for spotting a monitor being plugged or unplugged.
fn connector_set(layout: &Layout) -> HashSet<String> {
    layout.outputs.iter().map(|o| monitor_layout::normalize_connector(&o.connector)).collect()
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(
            storage,
            eframe::APP_KEY,
            &Settings { group: self.group, show_desk: self.show_desk },
        );
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        self.keyboard(ctx);

        if let Some(text) = self.pending_clipboard.take() {
            ctx.copy_text(text);
        }
        if self.busy() {
            // Keeps the per-row sweep moving. An unconditional repaint would spin a core at
            // whatever rate the display can manage.
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Declaration order is what claims the space: the status rail is pinned to the bottom
        // edge and the rack takes whatever is left in the middle.
        let margin = theme::panel_frame(ui, 14, 10);
        if band::permission_needed(self) {
            egui::Panel::top("permission")
                .frame(margin)
                .show(ui, |ui| band::permission(self, ui));
        }
        egui::Panel::bottom("status")
            .frame(theme::panel_frame(ui, 14, 4))
            .show(ui, |ui| band::status(self, ui));

        if self.show_desk {
            egui::Panel::top("desk")
                .resizable(true)
                .default_size(156.0)
                .min_size(96.0)
                .max_size(320.0)
                .frame(theme::panel_frame(ui, 14, 6))
                .show(ui, |ui| desk::show(self, ui));
        }
        egui::Panel::top("master")
            .exact_size(72.0)
            .frame(theme::panel_frame(ui, 14, 4))
            .show(ui, |ui| rack::master(self, ui));

        egui::CentralPanel::default().frame(theme::panel_frame(ui, 14, 6)).show(ui, |ui| {
            if let Some(error) = self.startup_error.clone() {
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new(format!("Couldn't list your monitors: {error}"))
                        .color(theme::Tokens::of(&ctx).fail),
                );
                if ui.button("Try again").clicked() {
                    self.restart();
                }
                return;
            }
            if self.monitors.is_empty() {
                ui.add_space(16.0);
                ui.label("No monitors found.");
                ui.label(
                    egui::RichText::new(
                        "Only monitors on a cable that carries a control channel appear here — \
                         laptop panels and some adapters don't.",
                    )
                    .small(),
                );
                ui.horizontal(|ui| {
                    if ui.button("Look again").clicked() {
                        self.restart();
                    }
                    if ui.button("Probe every route").clicked() {
                        self.probe_all();
                    }
                });
                return;
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| rack::rows(self, ui));
        });

        band::keys(self, &ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::offset_targets;

    #[test]
    fn the_master_carries_the_desk_and_keeps_the_trim() {
        // Two monitors at 65 and 60: the second was deliberately trimmed five points below.
        let offsets = [(0usize, 0.0f32), (1, -5.0)];
        let targets = offset_targets(40.0, &offsets);
        assert_eq!(targets, vec![(0, 40.0), (1, 35.0)]);
    }

    #[test]
    fn a_monitor_at_the_end_of_the_range_stops_there() {
        // Refusing to move anything because one display has bottomed out would make the
        // master useless in exactly the conditions it is reached for.
        let offsets = [(0usize, 0.0f32), (1, -20.0)];
        assert_eq!(offset_targets(10.0, &offsets), vec![(0, 10.0), (1, 0.0)]);
        assert_eq!(offset_targets(95.0, &[(0, 0.0), (1, 20.0)]), vec![(0, 95.0), (1, 100.0)]);
    }

    #[test]
    fn levelling_is_the_only_thing_that_discards_the_trim() {
        // With every offset zero — which is what the Level button writes — the desk flattens.
        let offsets = [(0usize, 0.0f32), (1, 0.0)];
        assert_eq!(offset_targets(47.0, &offsets), vec![(0, 47.0), (1, 47.0)]);
    }
}

//! The worker threads that actually talk to the displays.
//!
//! Every DDC/CI exchange blocks for tens of milliseconds, and a display that is asleep blocks
//! for a good deal longer while its retries time out. None of that can happen on the thread
//! drawing the window, so each monitor gets a thread of its own: a sleeping display delays
//! only itself, and "set every monitor to 40%" goes out on three buses at once rather than
//! three times in a row.
//!
//! Requests are coalesced before they are executed. Dragging a slider produces a request per
//! frame, and a bus that accepts about twenty writes a second would otherwise fall further
//! behind with every pixel the pointer moves — the display would keep changing for seconds
//! after the drag ended. Collapsing everything queued down to the last value per feature makes
//! the display follow the pointer instead of trailing it.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use monitor_control::{
    Capabilities, DdcConfig, DdcError, Edid, Error, InputSource, Monitor, MonitorManager,
    MonitorPath, ValueChange, code,
};

use crate::state::{Feature, MonitorId, MonitorState};

/// The continuous features the window offers a control for, in the order they are read.
///
/// Brightness first because it is what the window is for: a display that answers one request
/// before going quiet should answer that one.
pub const LEVEL_FEATURES: [(u8, &str); 3] =
    [(code::BRIGHTNESS, "Brightness"), (code::CONTRAST, "Contrast"), (code::VOLUME, "Volume")];

/// How long to wait after switching an input before reading back.
///
/// A display needs a moment to change input, and reading too early reports the old one.
const INPUT_SETTLE: Duration = Duration::from_millis(1200);

/// Something the window asks a monitor's worker to do.
#[derive(Debug, Clone)]
pub enum Request {
    /// Re-read every feature, finding a working route first.
    Refresh,
    /// Probe every route, not just enough of them to find one that works.
    ProbeAll,
    /// Write a continuous feature.
    SetLevel {
        /// The VCP feature code.
        code: u8,
        /// The value to write.
        value: u16,
        /// Identifies this write, so an older answer cannot clear a newer request.
        seq: u64,
    },
    /// Switch the display to another input.
    SetInput {
        /// The input to select.
        input: InputSource,
        /// Identifies this write.
        seq: u64,
    },
}

/// Something a worker reports back.
#[derive(Debug)]
pub enum Event {
    /// A monitor's state after some step completed.
    ///
    /// Sent after each read rather than once at the end, so controls appear as the answers
    /// arrive instead of the window sitting empty for a second.
    State {
        /// Which monitor.
        id: MonitorId,
        /// Its new state.
        state: Box<MonitorState>,
    },
}

/// A handle to one monitor's worker thread.
struct Worker {
    requests: Option<Sender<Request>>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Dropping the sender is what tells the worker to stop; joining then waits out at
        // most one in-flight exchange rather than leaving a thread writing to a display after
        // the window has gone.
        self.requests = None;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Every monitor's worker, and the channel their events arrive on.
pub struct Backend {
    workers: BTreeMap<MonitorId, Worker>,
    events: Receiver<Event>,
    /// The state each monitor was in before anything was read from it, in discovery order.
    initial: Vec<MonitorState>,
}

impl Backend {
    /// Discover the attached monitors and start a worker for each.
    ///
    /// Discovery only reads sysfs, so this is cheap enough to call on the window's own thread;
    /// everything that touches a bus happens on the workers afterwards.
    pub fn start(
        config: DdcConfig,
        group: bool,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> monitor_control::Result<Self> {
        let manager = if group {
            MonitorManager::discover(config)?
        } else {
            MonitorManager::discover_ungrouped(config)?
        };

        let (event_tx, events) = mpsc::channel();
        let mut workers = BTreeMap::new();
        let mut initial = Vec::new();

        for monitor in manager.into_monitors() {
            let state = snapshot_of(&monitor);
            let id = state.id.clone();
            initial.push(state.clone());

            let (requests, inbox) = mpsc::channel();
            let events = event_tx.clone();
            let wake = wake.clone();
            let handle = thread::Builder::new()
                .name(format!("ddc-{}", id.connector))
                .spawn(move || run_worker(monitor, state, inbox, events, wake))
                .expect("spawning a monitor worker thread");

            workers.insert(id, Worker { requests: Some(requests), handle: Some(handle) });
        }

        Ok(Self { workers, events, initial })
    }

    /// The state of each monitor before anything was read from it.
    pub fn initial_states(&self) -> &[MonitorState] {
        &self.initial
    }

    /// Send a request to one monitor.
    ///
    /// A monitor that has since been unplugged is skipped rather than reported: the window
    /// can do nothing about it, and its state already says it is gone.
    pub fn send(&self, id: &MonitorId, request: Request) {
        if let Some(worker) = self.workers.get(id)
            && let Some(requests) = &worker.requests
        {
            let _ = requests.send(request);
        }
    }

    /// Send the same request to every monitor.
    pub fn broadcast(&self, request: Request) {
        for id in self.workers.keys() {
            self.send(id, request.clone());
        }
    }

    /// Take whatever the workers have reported since the last call.
    pub fn drain(&self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            out.push(event);
        }
        out
    }

    /// Shut this backend down without waiting for it.
    ///
    /// Joining the workers takes as long as whatever exchange each one is in the middle of —
    /// up to a second or two for a display that is asleep — and doing that on the window's
    /// own thread freezes the window every time a monitor is plugged in or unplugged. The
    /// wait is handed to a thread of its own instead.
    ///
    /// The cost is that the outgoing workers can briefly overlap the incoming ones on the
    /// same bus. A collision there produces one garbled frame, which DDC/CI's checksum
    /// catches and the retry absorbs; a frozen window is the worse of the two.
    pub fn retire(mut self) {
        // Taking the workers out first means that if the thread cannot be spawned — which only
        // happens when the process is out of threads — the fallback drops an empty `Backend`
        // here rather than joining every worker on the drawing thread.
        let workers = std::mem::take(&mut self.workers);
        if thread::Builder::new()
            .name("ddc-retire".into())
            .spawn(move || drop(workers))
            .is_err()
        {
            // Nothing was moved into a thread, so the senders are already gone and the workers
            // will finish and exit on their own.
        }
    }
}

/// Whether this account can open a monitor's device nodes at all.
///
/// Checked by opening them rather than by waiting for a probe to fail, because the probe
/// takes a second and this takes microseconds — the difference between telling a first-time
/// user what to do straight away and telling them after the window has looked broken for a
/// while. Opening an I2C device node puts nothing on the bus.
pub fn access_denied(paths: &[MonitorPath]) -> bool {
    !paths.is_empty()
        && paths.iter().all(|path| {
            matches!(
                OpenOptions::new().read(true).write(true).open(&path.device),
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied
            )
        })
}

/// The state of a monitor as discovery left it, before any bus traffic.
fn snapshot_of(monitor: &Monitor) -> MonitorState {
    let id = MonitorId {
        key: monitor.key(),
        connector: monitor
            .paths
            .first()
            .map(|p| p.connector.clone())
            .unwrap_or_else(|| "?".to_string()),
    };
    MonitorState::new(
        id,
        monitor.id.clone(),
        Edid::parse(&monitor.edid).ok().and_then(|e| e.size_mm),
        monitor.ambiguous_grouping,
        monitor.paths.clone(),
    )
}

/// One monitor's worker loop.
fn run_worker(
    mut monitor: Monitor,
    mut state: MonitorState,
    inbox: Receiver<Request>,
    events: Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let id = state.id.clone();
    let publish = |state: &MonitorState| {
        if events.send(Event::State { id: id.clone(), state: Box::new(state.clone()) }).is_ok() {
            wake();
        }
    };

    while let Ok(first) = inbox.recv() {
        let batch = coalesce(first, &inbox);

        state.busy = true;
        publish(&state);

        for request in batch {
            match request {
                Request::Refresh => refresh(&mut monitor, &mut state, &publish),
                Request::ProbeAll => {
                    // The thorough option, so it also retries a capabilities string the
                    // display refused earlier — it may have been asleep at the time.
                    state.caps_error = None;
                    monitor.probe_all();
                    sync_paths(&monitor, &mut state);
                    publish(&state);
                    refresh(&mut monitor, &mut state, &publish);
                }
                Request::SetLevel { code, value, seq } => {
                    set_level(&mut monitor, &mut state, code, value, seq);
                    sync_paths(&monitor, &mut state);
                    publish(&state);
                }
                Request::SetInput { input, seq } => {
                    set_input(&mut monitor, &mut state, input, seq);
                    sync_paths(&monitor, &mut state);
                    publish(&state);
                }
            }
        }

        // An exchange that failed leaves the route it used marked dead, and a display that
        // refuses one request while answering every other one — several do — would otherwise
        // sit there looking broken until something else was asked of it.
        if !monitor.is_controllable() {
            let _ = monitor.ensure_path();
            sync_paths(&monitor, &mut state);
        }

        state.busy = false;
        publish(&state);
    }
}

/// Collapse everything already queued into the smallest batch with the same effect.
///
/// Order matters on the way out: writes go first so the display reaches the value that was
/// asked for, and a refresh queued alongside them runs last so it reads back what was
/// actually written rather than what was there before.
fn coalesce(first: Request, inbox: &Receiver<Request>) -> Vec<Request> {
    let mut pending = vec![first];
    while let Ok(request) = inbox.try_recv() {
        pending.push(request);
    }

    let mut levels: Vec<Request> = Vec::new();
    let mut input: Option<Request> = None;
    let mut refresh = false;
    let mut probe = false;

    for request in pending {
        match request {
            Request::Refresh => refresh = true,
            Request::ProbeAll => probe = true,
            Request::SetLevel { code, .. } => {
                // Only the last value written to a feature is observable, so the ones before
                // it are dropped. This is what keeps a two-second drag from queueing two
                // seconds of writes for the display to work through afterwards.
                if let Some(slot) = levels
                    .iter_mut()
                    .find(|r| matches!(r, Request::SetLevel { code: c, .. } if *c == code))
                {
                    *slot = request;
                } else {
                    levels.push(request);
                }
            }
            Request::SetInput { .. } => input = Some(request),
        }
    }

    let mut batch = levels;
    batch.extend(input);
    if probe {
        batch.push(Request::ProbeAll);
    } else if refresh {
        batch.push(Request::Refresh);
    }
    batch
}

/// Read everything the window shows for one monitor.
fn refresh(monitor: &mut Monitor, state: &mut MonitorState, publish: &impl Fn(&MonitorState)) {
    if let Err(e) = monitor.ensure_path() {
        state.last_error = Some(e.to_string());
        state.loaded = true;
        for feature in state.features.values_mut() {
            feature.stale = true;
        }
        sync_paths(monitor, state);
        publish(state);
        return;
    }

    state.last_error = None;
    sync_paths(monitor, state);
    publish(state);

    for (feature, _) in LEVEL_FEATURES {
        match monitor.get_vcp(feature) {
            Ok(value) => {
                let entry = state.features.entry(feature).or_insert(Feature {
                    current: value.current,
                    maximum: value.maximum.max(1),
                    applied_seq: 0,
                    stale: false,
                    error: None,
                });
                entry.current = value.current;
                // A maximum of zero would make every control a single point; keep the last
                // sane bound rather than trusting a reply that cannot be right.
                if value.maximum > 0 {
                    entry.maximum = value.maximum;
                }
                entry.stale = false;
                entry.error = None;
            }
            Err(e) if is_unsupported(&e) => {
                state.features.remove(&feature);
            }
            Err(e) => {
                if let Some(entry) = state.features.get_mut(&feature) {
                    entry.stale = true;
                    entry.error = Some(e.to_string());
                } else {
                    state.last_error = Some(e.to_string());
                }
            }
        }
        sync_paths(monitor, state);
        publish(state);
    }

    match monitor.input_source() {
        Err(Error::InputReadbackUnavailable { .. }) => {
            state.input.current = None;
            state.input.error = None;
        }
        Ok(input) => {
            state.input.current = Some(input);
            state.input.requested = None;
            state.input.error = None;
            state.input.switched_away = false;
        }
        Err(e) if is_unsupported(&e) => {
            state.input.current = None;
            state.input.error = None;
        }
        Err(e) => state.input.error = Some(e.to_string()),
    }
    sync_paths(monitor, state);
    publish(state);

    // Capabilities do not change, so they are read once. Displays that refuse the request
    // still answer everything else, so a failure is recorded rather than retried on every
    // refresh — it costs a second of bus time each time to learn the same thing.
    if state.caps.is_none() && state.caps_error.is_none() {
        match monitor.parsed_capabilities() {
            Ok(caps) => state.caps = Some(caps),
            Err(e) => state.caps_error = Some(e.to_string()),
        }
    }

    state.loaded = true;
    sync_paths(monitor, state);
    publish(state);
}

/// Write one continuous feature and record what the display ended up at.
fn set_level(monitor: &mut Monitor, state: &mut MonitorState, feature: u8, value: u16, seq: u64) {
    // `adjust_vcp` reads before it writes so the value can be clamped to the display's own
    // maximum, which stops the end of a control from overshooting into a rejected write.
    match monitor.adjust_vcp(feature, ValueChange::Absolute(value)) {
        Ok(written) => {
            let entry = state.features.entry(feature).or_insert(Feature {
                current: written,
                maximum: written.max(100),
                applied_seq: seq,
                stale: false,
                error: None,
            });
            entry.current = written;
            entry.applied_seq = seq;
            entry.stale = false;
            entry.error = None;
            state.last_error = None;
        }
        Err(e) if is_unsupported(&e) => {
            state.features.remove(&feature);
        }
        Err(e) => {
            let message = e.to_string();
            if let Some(entry) = state.features.get_mut(&feature) {
                // The request is finished either way, so the sequence number moves on and the
                // window stops showing the value that was asked for. What it shows instead is
                // the last value actually read, marked as no longer trustworthy, next to the
                // reason — rather than a number that quietly pretends the write landed.
                entry.applied_seq = seq;
                entry.stale = true;
                entry.error = Some(message.clone());
            }
            state.last_error = Some(message);
        }
    }
}

/// Switch the input, treating the connection dying as success.
fn set_input(monitor: &mut Monitor, state: &mut MonitorState, input: InputSource, seq: u64) {
    state.input.applied_seq = seq;
    match monitor.set_input_source(input) {
        Ok(()) => {
            state.input.error = None;
            state.input.switched_away = false;
            state.last_error = None;

            if monitor.quirks().is_some_and(|q| !q.input_readback_reliable) {
                state.input.current = None;
                state.input.requested = None;
                state.input.last_sent = Some(input);
                return;
            }

            // Waiting is cheap here: this thread has nothing else to do, and every other
            // monitor has a thread of its own.
            thread::sleep(INPUT_SETTLE);
            match monitor.input_source() {
                Ok(actual) => {
                    state.input.current = Some(actual);
                    state.input.requested = None;
                    if actual != input {
                        state.input.error = Some(format!(
                            "Requested {input}, but the display still reports {actual}"
                        ));
                    }
                }
                // Losing the connection is what switching a monitor to another machine's
                // input does. Reporting it as a failure would be exactly backwards — but so
                // would recording the requested input as the current one, since nothing
                // confirmed it.
                Err(_) => {
                    state.input.requested = Some(input);
                    state.input.switched_away = true;
                }
            }
        }
        Err(e) => {
            state.input.requested = None;
            state.input.error = Some(e.to_string());
        }
    }
}

/// Copy the library's view of the routes into the state the window draws from.
fn sync_paths(monitor: &Monitor, state: &mut MonitorState) {
    state.paths = monitor.paths.clone();
    state.active_path = monitor
        .active_path()
        .and_then(|active| monitor.paths.iter().position(|p| p.bus == active.bus));
}

/// Whether an error means the display does not implement the feature at all.
///
/// Worth telling apart from every other failure: an unsupported feature should disappear from
/// the window, while a failed exchange should leave the control in place with the reason on
/// it, because it will probably work again on the next try.
fn is_unsupported(error: &Error) -> bool {
    matches!(error, Error::Ddc { source: DdcError::UnsupportedFeature { .. }, .. })
}

/// A capabilities string reduced to what the window can say about it.
pub fn caps_summary(caps: &Capabilities) -> String {
    let mut parts = Vec::new();
    if let Some(version) = &caps.mccs_version {
        parts.push(format!("MCCS {version}"));
    }
    if let Some(kind) = &caps.display_type {
        parts.push(kind.clone());
    }
    parts.push(format!("{} features", caps.vcp.len()));
    parts.join(" · ")
}

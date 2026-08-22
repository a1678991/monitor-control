//! The arrangement as wlroots-derived compositors report it.
//!
//! `zwlr_output_management_v1` is what sway, Hyprland, river, Wayfire, and labwc expose. It
//! is a subscription rather than a query: the compositor streams the whole output state and
//! then sends `done`, and keeps streaming as things change. This module wants one answer, so
//! it opens a connection of its own, reads until the first `done`, and drops it — which also
//! saves having to negotiate the teardown requests that only exist from version 3 onwards.
//!
//! The connection is deliberately separate from the one the window is drawn on. The
//! pure-Rust backend has no process-global state, so a second connection is independent:
//! its own socket, its own object IDs, its own queue.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use wayland_client::backend::ObjectId;
use wayland_client::globals::{BindError, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, event_created_child};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

use crate::{Layout, LayoutOutput, LayoutSource, Rotation};

/// How long to wait for the compositor to finish describing its outputs.
///
/// The protocol has no timeout of its own, and a compositor that stops mid-burst would
/// otherwise block this thread for good.
const TIMEOUT: Duration = Duration::from_secs(3);

/// Read the arrangement, or `None` when this is not a Wayland session or the compositor does
/// not implement the protocol.
pub fn detect() -> Option<Result<Layout, String>> {
    // Connecting reads `WAYLAND_DISPLAY`; its absence is the ordinary answer under X11.
    let connection = Connection::connect_to_env().ok()?;

    let (globals, mut queue) = match registry_queue_init::<State>(&connection) {
        Ok(pair) => pair,
        Err(e) => return Some(Err(format!("reading the Wayland registry: {e}"))),
    };
    let handle = queue.handle();

    // The version range matters: asking for exactly 4 fails on compositors that expose 2 or
    // 3, while a range binds whatever they have.
    let manager: ZwlrOutputManagerV1 = match globals.bind(&handle, 1..=4, ()) {
        Ok(manager) => manager,
        // Mutter and KWin land here, and it is not an error — it means another source knows
        // the answer.
        Err(BindError::NotPresent | BindError::UnsupportedVersion) => return None,
    };

    let mut state = State::default();
    match pump(&connection, &mut queue, &mut state) {
        Ok(()) => {}
        Err(e) => return Some(Err(e)),
    }

    // Asking the compositor to stop is politeness rather than necessity, since dropping the
    // connection reaps everything anyway; it costs one request and stops a burst of events
    // arriving while the socket is closing.
    manager.stop();
    let _ = connection.flush();

    Some(Ok(state.build()))
}

/// Read events until the compositor says it is done, or the budget runs out.
fn pump(connection: &Connection, queue: &mut EventQueue<State>, state: &mut State) -> Result<(), String> {
    let deadline = Instant::now() + TIMEOUT;

    while !state.done {
        queue.flush().map_err(|e| format!("flushing the Wayland queue: {e}"))?;
        if queue.dispatch_pending(state).map_err(|e| format!("dispatching: {e}"))? > 0 {
            continue;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("the compositor did not finish describing its outputs".to_string());
        }
        // Another thread may have read the socket between the check above and here, in which
        // case there is nothing to prepare and the loop simply goes round again.
        let Some(guard) = queue.prepare_read() else { continue };

        let mut fds = [rustix::event::PollFd::new(connection, rustix::event::PollFlags::IN)];
        let timeout = rustix::event::Timespec {
            tv_sec: remaining.as_secs() as _,
            tv_nsec: remaining.subsec_nanos() as _,
        };
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Err("the compositor did not finish describing its outputs".to_string()),
            Ok(_) => {}
            Err(e) => return Err(format!("waiting on the Wayland socket: {e}")),
        }
        guard.read().map_err(|e| format!("reading from the Wayland socket: {e}"))?;
    }
    Ok(())
}

/// One mode, as the compositor describes it.
#[derive(Debug, Default, Clone)]
struct Mode {
    width: i32,
    height: i32,
    /// Refresh rate in millihertz, which is the unit the protocol uses.
    refresh_mhz: i32,
}

/// One output, assembled from the stream of events describing it.
#[derive(Debug, Default, Clone)]
struct Head {
    name: String,
    description: Option<String>,
    make: Option<String>,
    model: Option<String>,
    serial: Option<String>,
    physical_mm: Option<(u32, u32)>,
    enabled: bool,
    position: (i32, i32),
    transform: Rotation,
    scale: f64,
    current_mode: Option<ObjectId>,
}

#[derive(Default)]
struct State {
    heads: HashMap<ObjectId, Head>,
    /// The order heads arrived in, which is the order the compositor lists them.
    order: Vec<ObjectId>,
    modes: HashMap<ObjectId, Mode>,
    done: bool,
}

impl State {
    fn build(&self) -> Layout {
        let outputs = self
            .order
            .iter()
            .filter_map(|id| self.heads.get(id))
            // A disabled head has no place on the desk: the compositor still describes it,
            // but it occupies no part of the coordinate space.
            .filter(|head| head.enabled)
            .map(|head| {
                let mode = head.current_mode.as_ref().and_then(|id| self.modes.get(id));
                let scale = if head.scale > 0.0 { head.scale } else { 1.0 };
                let size = mode
                    .map(|m| {
                        let (mut w, mut h) =
                            (f64::from(m.width) / scale, f64::from(m.height) / scale);
                        if head.transform.is_quarter_turn() {
                            std::mem::swap(&mut w, &mut h);
                        }
                        (w.round() as u32, h.round() as u32)
                    })
                    .unwrap_or((0, 0));

                LayoutOutput {
                    connector: head.name.clone(),
                    description: head.description.clone(),
                    // The protocol reports the manufacturer as the EDID's PNP ID, matching
                    // what this crate compares against.
                    manufacturer: head.make.clone(),
                    model: head.model.clone(),
                    serial: head.serial.clone(),
                    edid_key: None,
                    position: head.position,
                    size,
                    scale,
                    rotation: head.transform,
                    // The protocol has no notion of a primary output.
                    primary: false,
                    size_mm: head.physical_mm,
                    refresh_hz: mode
                        .filter(|m| m.refresh_mhz > 0)
                        .map(|m| f64::from(m.refresh_mhz) / 1000.0),
                }
            })
            .collect();

        Layout { outputs, source: LayoutSource::WlrOutputManagement, warnings: Vec::new() }
    }
}

/// `registry_queue_init` requires this exact implementation, even though the registry has
/// nothing to say once the initial round trip has enumerated the globals.
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                let id = head.id();
                state.order.push(id.clone());
                state.heads.insert(id, Head { scale: 1.0, ..Head::default() });
            }
            // Everything the compositor has to say arrives before this, in one atomic burst.
            zwlr_output_manager_v1::Event::Done { .. } => state.done = true,
            _ => {}
        }
    }

    // Without this the library panics at runtime on the first `head` event, because it has
    // no way to know what type of object the event creates.
    event_created_child!(State, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for State {
    fn event(
        state: &mut Self,
        head: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = head.id();
        // A mode event has to be recorded against the whole state, not just this head, so it
        // is handled before the per-head borrow is taken.
        if let zwlr_output_head_v1::Event::Mode { mode } = &event {
            state.modes.insert(mode.id(), Mode::default());
        }
        if let zwlr_output_head_v1::Event::Finished = event {
            state.heads.remove(&id);
            state.order.retain(|other| other != &id);
            return;
        }

        let Some(entry) = state.heads.get_mut(&id) else { return };
        match event {
            zwlr_output_head_v1::Event::Name { name } => entry.name = name,
            zwlr_output_head_v1::Event::Description { description } => {
                entry.description = Some(description);
            }
            zwlr_output_head_v1::Event::PhysicalSize { width, height } => {
                if width > 0 && height > 0 {
                    entry.physical_mm = Some((width as u32, height as u32));
                }
            }
            // Reported as an int rather than a bool, because the protocol predates the
            // Wayland scanner growing booleans.
            zwlr_output_head_v1::Event::Enabled { enabled } => entry.enabled = enabled != 0,
            zwlr_output_head_v1::Event::CurrentMode { mode } => {
                entry.current_mode = Some(mode.id());
            }
            zwlr_output_head_v1::Event::Position { x, y } => entry.position = (x, y),
            zwlr_output_head_v1::Event::Transform { transform } => {
                entry.transform = to_rotation(transform);
            }
            zwlr_output_head_v1::Event::Scale { scale } => entry.scale = scale,
            zwlr_output_head_v1::Event::Make { make } => entry.make = Some(make),
            zwlr_output_head_v1::Event::Model { model } => entry.model = Some(model),
            zwlr_output_head_v1::Event::SerialNumber { serial_number } => {
                entry.serial = Some(serial_number);
            }
            _ => {}
        }
    }

    event_created_child!(State, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for State {
    fn event(
        state: &mut Self,
        mode: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = mode.id();
        match event {
            zwlr_output_mode_v1::Event::Size { width, height } => {
                let entry = state.modes.entry(id).or_default();
                entry.width = width;
                entry.height = height;
            }
            zwlr_output_mode_v1::Event::Refresh { refresh } => {
                state.modes.entry(id).or_default().refresh_mhz = refresh;
            }
            zwlr_output_mode_v1::Event::Finished => {
                state.modes.remove(&id);
            }
            _ => {}
        }
    }
}

/// Reduce a Wayland transform to the rotation it applies.
///
/// The flipped forms mirror what is drawn without changing which rectangle the output
/// occupies, so they collapse onto their unflipped equivalents.
fn to_rotation(transform: WEnum<wl_output::Transform>) -> Rotation {
    match transform {
        WEnum::Value(wl_output::Transform::_90 | wl_output::Transform::Flipped90) => {
            Rotation::Rotate90
        }
        WEnum::Value(wl_output::Transform::_180 | wl_output::Transform::Flipped180) => {
            Rotation::Rotate180
        }
        WEnum::Value(wl_output::Transform::_270 | wl_output::Transform::Flipped270) => {
            Rotation::Rotate270
        }
        _ => Rotation::Normal,
    }
}

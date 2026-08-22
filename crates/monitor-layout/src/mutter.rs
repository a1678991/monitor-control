//! GNOME's arrangement, from `org.gnome.Mutter.DisplayConfig` on the session bus.
//!
//! Mutter implements none of the Wayland output-management protocols, exposing this D-Bus
//! interface instead. It is also the best source GNOME offers: it answers on X11 as well as
//! Wayland, and it reports the connector name, the EDID strings, the scale, and the
//! transform — where the XWayland RandR mirror of the same layout reports mutter's synthetic
//! values and no EDID at all.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Type};

use crate::{Layout, LayoutOutput, LayoutSource, Rotation};

const SERVICE: &str = "org.gnome.Mutter.DisplayConfig";
const PATH: &str = "/org/gnome/Mutter/DisplayConfig";

/// How long to wait for mutter before giving up and letting another source answer.
///
/// A compositor busy re-probing a display can take a moment; one that has wedged would
/// otherwise hold up the whole window.
const TIMEOUT: Duration = Duration::from_secs(5);

/// `layout-mode` value meaning positions and sizes are in scaled, logical pixels.
const LAYOUT_MODE_LOGICAL: u32 = 1;

/// One monitor's identity, as mutter reports it: `(ssss)`.
#[derive(Debug, Clone, Type, Deserialize)]
struct MonitorSpec {
    connector: String,
    vendor: String,
    product: String,
    serial: String,
}

/// One mode a monitor supports: `(siiddada{sv})`.
#[derive(Debug, Clone, Type, Deserialize)]
struct MonitorMode {
    #[allow(dead_code, reason = "part of the wire format, which is positional")]
    id: String,
    width: i32,
    height: i32,
    refresh_rate: f64,
    #[allow(dead_code, reason = "part of the wire format, which is positional")]
    preferred_scale: f64,
    #[allow(dead_code, reason = "part of the wire format, which is positional")]
    supported_scales: Vec<f64>,
    properties: HashMap<String, OwnedValue>,
}

/// One physical monitor: `((ssss)a(siiddada{sv})a{sv})`.
#[derive(Debug, Clone, Type, Deserialize)]
struct MonitorInfo {
    spec: MonitorSpec,
    modes: Vec<MonitorMode>,
    properties: HashMap<String, OwnedValue>,
}

/// One position in the desktop's coordinate space: `(iiduba(ssss)a{sv})`.
#[derive(Debug, Clone, Type, Deserialize)]
struct LogicalMonitor {
    x: i32,
    y: i32,
    scale: f64,
    transform: u32,
    primary: bool,
    monitors: Vec<MonitorSpec>,
    #[allow(dead_code, reason = "part of the wire format, which is positional")]
    properties: HashMap<String, OwnedValue>,
}

/// The reply to `GetCurrentState`.
type CurrentState = (u32, Vec<MonitorInfo>, Vec<LogicalMonitor>, HashMap<String, OwnedValue>);

/// The session bus connection, kept open between polls.
///
/// Building one costs a socket, a SASL handshake, and a thread; a watcher asking every few
/// seconds should not pay that each time. A connection that has failed is dropped so the
/// next call builds a fresh one rather than retrying a dead socket forever.
static CONNECTION: Mutex<Option<Connection>> = Mutex::new(None);

/// Read GNOME's arrangement, or `None` when GNOME is not what is running here.
pub fn detect() -> Option<Result<Layout, String>> {
    let connection = match connection() {
        Ok(c) => c,
        // No session bus at all is not a failure worth reporting: it means this is not a
        // desktop session, and some other source will answer.
        Err(_) => return None,
    };

    match query(&connection) {
        Ok(state) => Some(Ok(build(state))),
        // The service being absent says nothing about the bus, and dropping a working socket
        // every two seconds only to hand-shake a new one is pure waste on any desktop that is
        // not GNOME.
        Err(e) if is_absent(&e) => None,
        Err(e) => {
            if is_disconnected(&e) {
                forget_connection();
            }
            Some(Err(e.to_string()))
        }
    }
}

/// The cached connection, building one if there is none.
fn connection() -> zbus::Result<Connection> {
    let mut slot = CONNECTION.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(connection) = slot.as_ref() {
        return Ok(connection.clone());
    }
    let connection = zbus::blocking::connection::Builder::session()?
        .method_timeout(TIMEOUT)
        .build()?;
    *slot = Some(connection.clone());
    Ok(connection)
}

/// Drop the cached connection so the next call starts over.
fn forget_connection() {
    *CONNECTION.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn query(connection: &Connection) -> zbus::Result<CurrentState> {
    let reply = connection.call_method(
        Some(SERVICE),
        PATH,
        Some(SERVICE),
        "GetCurrentState",
        &(),
    )?;
    reply.body().deserialize()
}

/// Whether this error means GNOME is simply not running, rather than something being wrong.
fn is_absent(error: &zbus::Error) -> bool {
    matches!(
        error,
        zbus::Error::MethodError(name, _, _)
            if matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.ServiceUnknown"
                    | "org.freedesktop.DBus.Error.NameHasNoOwner"
            )
    )
}

/// Whether the bus itself is gone, in which case the cached connection is worthless.
fn is_disconnected(error: &zbus::Error) -> bool {
    matches!(error, zbus::Error::Address(_) | zbus::Error::Connection(_, _) | zbus::Error::InputOutput(_))
}

/// Turn mutter's reply into a layout.
fn build(state: CurrentState) -> Layout {
    let (_serial, monitors, logical_monitors, properties) = state;

    let logical_layout = properties
        .get("layout-mode")
        .and_then(|v| u32::try_from(v).ok())
        .is_none_or(|mode| mode == LAYOUT_MODE_LOGICAL);

    let mut outputs = Vec::new();
    for logical in &logical_monitors {
        let rotation = transform_to_rotation(logical.transform);

        // A logical monitor holding several monitors is a mirrored set: every one of them
        // shows the same pixels at the same place. Each is reported separately so a caller
        // can still reach each panel; they share a rectangle, which is the truth.
        for spec in &logical.monitors {
            let info = monitors.iter().find(|m| same_monitor(&m.spec, spec));
            let mode = info.and_then(current_mode);

            let size = mode
                .map(|m| logical_size(m, logical.scale, logical_layout, rotation))
                .unwrap_or((0, 0));

            outputs.push(LayoutOutput {
                connector: spec.connector.clone(),
                description: info.and_then(|i| string_property(&i.properties, "display-name")),
                manufacturer: non_empty(&spec.vendor),
                model: non_empty(&spec.product),
                serial: non_empty(&spec.serial),
                // Mutter reports the EDID as three separate strings and never the bytes, so
                // there is nothing to derive a key from here.
                edid_key: None,
                position: (logical.x, logical.y),
                size,
                scale: logical.scale,
                rotation,
                primary: logical.primary,
                size_mm: physical_size(info),
                refresh_hz: mode.map(|m| m.refresh_rate),
            });
        }
    }

    Layout { outputs, source: LayoutSource::Mutter, warnings: Vec::new() }
}

/// Whether two specs name the same monitor.
///
/// Mutter repeats the whole four-string spec in both arrays, so this could compare all four;
/// the connector alone is enough and is what mutter itself keys on.
fn same_monitor(a: &MonitorSpec, b: &MonitorSpec) -> bool {
    a.connector == b.connector
}

/// The mode the monitor is currently in.
fn current_mode(info: &MonitorInfo) -> Option<&MonitorMode> {
    info.modes.iter().find(|m| {
        m.properties.get("is-current").and_then(|v| bool::try_from(v).ok()).unwrap_or(false)
    })
}

/// The size this monitor occupies in the desktop's coordinate space.
///
/// In the default layout mode the desktop works in scaled pixels, so a 3840-wide panel at
/// ×1.5 occupies 2560. In physical layout mode the coordinates are device pixels and the
/// mode size is already correct. A quarter turn exchanges the two axes either way.
fn logical_size(mode: &MonitorMode, scale: f64, logical_layout: bool, rotation: Rotation) -> (u32, u32) {
    let (mut width, mut height) = (mode.width.max(0) as f64, mode.height.max(0) as f64);
    if logical_layout && scale > 0.0 {
        width /= scale;
        height /= scale;
    }
    if rotation.is_quarter_turn() {
        std::mem::swap(&mut width, &mut height);
    }
    (width.round() as u32, height.round() as u32)
}

/// Mutter's transform values, which combine rotation with an optional flip.
///
/// Values 4 to 7 are the flipped forms of 0 to 3. The flip changes what is drawn, not the
/// rectangle the screen occupies, so only the quarter turns are kept.
fn transform_to_rotation(transform: u32) -> Rotation {
    match transform % 4 {
        1 => Rotation::Rotate90,
        2 => Rotation::Rotate180,
        3 => Rotation::Rotate270,
        _ => Rotation::Normal,
    }
}

/// The panel's physical size, when mutter reports one.
fn physical_size(info: Option<&MonitorInfo>) -> Option<(u32, u32)> {
    let properties = &info?.properties;
    let width = properties.get("width-mm").and_then(|v| u32::try_from(v).ok())?;
    let height = properties.get("height-mm").and_then(|v| u32::try_from(v).ok())?;
    (width > 0 && height > 0).then_some((width, height))
}

fn string_property(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    properties.get(key)?.downcast_ref::<&str>().ok().map(str::to_string)
}

/// Mutter sends an empty string where a display reports no such field.
fn non_empty(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_reduce_to_quarter_turns() {
        assert_eq!(transform_to_rotation(0), Rotation::Normal);
        assert_eq!(transform_to_rotation(1), Rotation::Rotate90);
        assert_eq!(transform_to_rotation(2), Rotation::Rotate180);
        assert_eq!(transform_to_rotation(3), Rotation::Rotate270);
        // 4..7 are the same turns with a flip, which does not move the rectangle.
        assert_eq!(transform_to_rotation(4), Rotation::Normal);
        assert_eq!(transform_to_rotation(5), Rotation::Rotate90);
        assert_eq!(transform_to_rotation(7), Rotation::Rotate270);
    }

    /// A mode of the given size, with no properties.
    fn mode(width: i32, height: i32) -> MonitorMode {
        MonitorMode {
            id: format!("{width}x{height}"),
            width,
            height,
            refresh_rate: 60.0,
            preferred_scale: 1.0,
            supported_scales: Vec::new(),
            properties: HashMap::new(),
        }
    }

    #[test]
    fn logical_sizes_divide_by_the_scale() {
        let m = mode(3840, 2160);
        assert_eq!(logical_size(&m, 1.0, true, Rotation::Normal), (3840, 2160));
        assert_eq!(logical_size(&m, 1.5, true, Rotation::Normal), (2560, 1440));
        assert_eq!(logical_size(&m, 2.0, true, Rotation::Normal), (1920, 1080));
    }

    #[test]
    fn physical_layout_mode_keeps_device_pixels() {
        // With layout-mode set to physical the positions are device pixels, so dividing by
        // the scale a second time would shrink every screen.
        let m = mode(3840, 2160);
        assert_eq!(logical_size(&m, 2.0, false, Rotation::Normal), (3840, 2160));
    }

    #[test]
    fn a_quarter_turn_exchanges_the_axes() {
        let m = mode(3840, 2160);
        assert_eq!(logical_size(&m, 1.0, true, Rotation::Rotate90), (2160, 3840));
        assert_eq!(logical_size(&m, 1.0, true, Rotation::Rotate180), (3840, 2160));
    }

    #[test]
    fn a_zero_scale_does_not_produce_infinities() {
        // Nothing should send a scale of zero, but dividing by one would poison every
        // coordinate downstream with NaN.
        let m = mode(1920, 1080);
        assert_eq!(logical_size(&m, 0.0, true, Rotation::Normal), (1920, 1080));
    }
}

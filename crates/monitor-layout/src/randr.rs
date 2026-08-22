//! The arrangement as an X11 server reports it, through RandR.
//!
//! Works on any X11 session and, because every Wayland compositor worth using runs XWayland,
//! usually works under Wayland too. That makes it a good last resort but a poor first choice:
//! under XWayland the outputs are mutter's or KWin's synthetic mirror of the real layout, the
//! physical sizes are invented, rotation is composited away before X ever sees it, and no
//! EDID is exported at all. Every native source is asked before this one for those reasons.
//!
//! RandR 1.5's `GetMonitors` is preferred over walking CRTCs: it reports the logical
//! rectangles rather than the hardware ones, so mirrored outputs collapse into a single
//! rectangle instead of appearing as duplicates, and it is the only call that says which
//! monitor is primary.

use monitor_control::Edid;
use x11rb::connection::{Connection as _, RequestConnection as _};
use x11rb::errors::ReplyError;
use x11rb::protocol::randr::{
    self, ConnectionExt as _, GetOutputInfoReply, ModeInfo, Rotation as XRotation,
};
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, Window};
use x11rb::rust_connection::RustConnection;

use crate::{Layout, LayoutOutput, LayoutSource, Rotation};

/// The RandR version this module knows how to speak.
const WANTED_VERSION: (u32, u32) = (1, 6);
/// The version that introduced `GetMonitors`, and with it logical rectangles.
const MONITORS_VERSION: (u32, u32) = (1, 5);

/// Read the arrangement from X11, or `None` when there is no X server to ask.
pub fn detect() -> Option<Result<Layout, String>> {
    // No `DISPLAY` is the ordinary answer on a Wayland session with XWayland switched off.
    let (connection, screen) = x11rb::connect(None).ok()?;

    // An absent extension is reported as a successful `None`, not as an error.
    match connection.extension_information(randr::X11_EXTENSION_NAME) {
        Ok(Some(_)) => {}
        Ok(None) => return None,
        Err(e) => return Some(Err(format!("querying X extensions: {e}"))),
    }

    let root = connection.setup().roots.get(screen)?.root;
    Some(read(&connection, root))
}

fn read(connection: &RustConnection, root: Window) -> Result<Layout, String> {
    // The server answers with the lower of what it supports and what was asked for, so this
    // is a negotiation rather than a demand.
    let version = connection
        .randr_query_version(WANTED_VERSION.0, WANTED_VERSION.1)
        .map_err(|e| format!("querying the RandR version: {e}"))?
        .reply()
        .map_err(|e| format!("querying the RandR version: {e}"))?;
    let version = (version.major_version, version.minor_version);

    let resources = connection
        // The `_current` form reads cached state. The other one re-probes every connector,
        // which can stall for a second and make displays flicker.
        .randr_get_screen_resources_current(root)
        .map_err(|e| format!("reading screen resources: {e}"))?
        .reply()
        .map_err(|e| format!("reading screen resources: {e}"))?;
    let timestamp = resources.config_timestamp;

    let mut outputs = Vec::new();

    if version >= MONITORS_VERSION
        && let Ok(cookie) = connection.randr_get_monitors(root, true)
        && let Ok(monitors) = cookie.reply()
    {
        for monitor in monitors.monitors {
            for output in &monitor.outputs {
                let Some(info) = output_info(connection, *output, timestamp) else { continue };
                let mut entry = describe(connection, *output, &info);
                entry.position = (i32::from(monitor.x), i32::from(monitor.y));
                entry.size = (u32::from(monitor.width), u32::from(monitor.height));
                entry.primary = monitor.primary;
                // `GetMonitors` reports the logical rectangle but nothing about how the
                // hardware is driven, so rotation and refresh still come from the CRTC.
                if let Some(crtc) = crtc_info(connection, info.crtc, timestamp) {
                    entry.rotation = to_rotation(crtc.rotation);
                    entry.refresh_hz = refresh_of(&resources.modes, crtc.mode);
                }
                if entry.size_mm.is_none() && monitor.width_in_millimeters > 0 {
                    entry.size_mm =
                        Some((monitor.width_in_millimeters, monitor.height_in_millimeters));
                }
                outputs.push(entry);
            }
        }
    }

    // Either the server predates `GetMonitors` or it reported nothing; fall back to reading
    // the hardware layout straight off the CRTCs.
    if outputs.is_empty() {
        let primary = connection
            .randr_get_output_primary(root)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.output);

        for output in resources.outputs {
            let Some(info) = output_info(connection, output, timestamp) else { continue };
            // A connected output with no CRTC is switched off, and asking for CRTC zero is a
            // protocol error rather than an empty answer.
            if info.crtc == 0 {
                continue;
            }
            let Some(crtc) = crtc_info(connection, info.crtc, timestamp) else { continue };

            let mut entry = describe(connection, output, &info);
            entry.position = (i32::from(crtc.x), i32::from(crtc.y));
            entry.size = (u32::from(crtc.width), u32::from(crtc.height));
            entry.rotation = to_rotation(crtc.rotation);
            entry.refresh_hz = refresh_of(&resources.modes, crtc.mode);
            entry.primary = primary == Some(output);
            outputs.push(entry);
        }
    }

    Ok(Layout { outputs, source: LayoutSource::Randr, warnings: Vec::new() })
}

/// Read one output's info, skipping anything with nothing plugged into it.
fn output_info(
    connection: &RustConnection,
    output: randr::Output,
    timestamp: u32,
) -> Option<GetOutputInfoReply> {
    let info = connection.randr_get_output_info(output, timestamp).ok()?.reply().ok()?;
    // UNKNOWN is what a driver reports when it cannot probe the link — several do for DVI and
    // for some docks. An output driving a CRTC is plainly connected whatever it says.
    let plugged = info.connection == randr::Connection::CONNECTED
        || (info.connection == randr::Connection::UNKNOWN && info.crtc != 0);
    plugged.then_some(info)
}

/// Everything about an output that does not depend on where it sits.
fn describe(
    connection: &RustConnection,
    output: randr::Output,
    info: &GetOutputInfoReply,
) -> LayoutOutput {
    let mut entry = LayoutOutput {
        connector: String::from_utf8_lossy(&info.name).into_owned(),
        size_mm: (info.mm_width > 0 && info.mm_height > 0)
            .then_some((info.mm_width, info.mm_height)),
        ..Default::default()
    };

    // A real X server hands over the EDID, which identifies the monitor exactly. XWayland
    // never does, which is why this is a bonus rather than the basis of the match.
    if let Some(bytes) = read_edid(connection, output)
        && let Ok(edid) = Edid::parse(&bytes)
    {
        entry.manufacturer = Some(edid.id.manufacturer.clone());
        entry.model = edid.id.model.clone();
        entry.serial = edid.id.serial_string.clone();
        entry.edid_key = Some(edid.id.key());
        if let Some((w, h)) = edid.size_mm {
            entry.size_mm = Some((u32::from(w), u32::from(h)));
        }
    }

    entry
}

/// Read a CRTC, or `None` when the output is not driving one.
///
/// Asking about CRTC zero is a protocol error rather than an empty answer, so the check is
/// not optional.
fn crtc_info(
    connection: &RustConnection,
    crtc: randr::Crtc,
    timestamp: u32,
) -> Option<randr::GetCrtcInfoReply> {
    if crtc == 0 {
        return None;
    }
    connection.randr_get_crtc_info(crtc, timestamp).ok()?.reply().ok()
}

/// The refresh rate of a mode, in Hz.
///
/// RandR reports timings rather than a rate: the pixel clock divided by the pixels in a whole
/// frame, borders and blanking included, is the number of frames per second.
fn refresh_of(modes: &[ModeInfo], mode: randr::Mode) -> Option<f64> {
    let mode = modes.iter().find(|m| m.id == mode)?;
    let pixels = f64::from(mode.htotal) * f64::from(mode.vtotal);
    if pixels <= 0.0 || mode.dot_clock == 0 {
        return None;
    }
    let mut hz = f64::from(mode.dot_clock) / pixels;
    // An interlaced mode sends half the lines per field, so it draws twice as often as the
    // timings alone say; a doublescan mode draws each line twice and so half as often.
    if mode.mode_flags.contains(randr::ModeFlag::INTERLACE) {
        hz *= 2.0;
    }
    if mode.mode_flags.contains(randr::ModeFlag::DOUBLE_SCAN) {
        hz /= 2.0;
    }
    Some(hz)
}

/// Read an output's EDID property.
///
/// Returns `None` for the two shapes an absent property takes: an X error, and — far more
/// commonly — a perfectly successful reply describing a property with no type and no bytes.
fn read_edid(connection: &RustConnection, output: randr::Output) -> Option<Vec<u8>> {
    // Interning with `only_if_exists` avoids creating the atom on a server where nothing has
    // ever asked for an EDID; the zero it returns then must not be passed on, since a
    // property request for atom zero is an error.
    let atom = connection.intern_atom(true, b"EDID").ok()?.reply().ok()?.atom;
    if atom == 0 {
        return None;
    }

    let mut bytes = Vec::new();
    // Lengths here count 32-bit units, so this asks for 512 bytes at a time: a base block
    // plus three extension blocks.
    const LONGS: u32 = 128;
    loop {
        let offset = (bytes.len() / 4) as u32;
        let reply = match connection
            .randr_get_output_property(output, atom, AtomEnum::ANY, offset, LONGS, false, false)
            .ok()?
            .reply()
        {
            Ok(reply) => reply,
            Err(ReplyError::X11Error(_)) => return None,
            Err(_) => return None,
        };
        if reply.type_ == 0 || reply.format != 8 || reply.num_items == 0 {
            break;
        }
        let more = reply.bytes_after > 0;
        bytes.extend_from_slice(&reply.data);
        if !more {
            break;
        }
    }

    (!bytes.is_empty()).then_some(bytes)
}

/// Reduce a RandR rotation mask to the quarter turn it contains.
///
/// It is a mask, not a value: a display that is both rotated and reflected has two bits set,
/// so comparing for equality would report no rotation at all.
fn to_rotation(rotation: XRotation) -> Rotation {
    if rotation.contains(XRotation::ROTATE90) {
        Rotation::Rotate90
    } else if rotation.contains(XRotation::ROTATE180) {
        Rotation::Rotate180
    } else if rotation.contains(XRotation::ROTATE270) {
        Rotation::Rotate270
    } else {
        Rotation::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_masks_are_read_bit_by_bit() {
        assert_eq!(to_rotation(XRotation::ROTATE0), Rotation::Normal);
        assert_eq!(to_rotation(XRotation::ROTATE90), Rotation::Rotate90);
        assert_eq!(to_rotation(XRotation::ROTATE180), Rotation::Rotate180);
        assert_eq!(to_rotation(XRotation::ROTATE270), Rotation::Rotate270);
        // A rotated and reflected output sets both bits; equality would miss the rotation.
        assert_eq!(
            to_rotation(XRotation::ROTATE90 | XRotation::REFLECT_X),
            Rotation::Rotate90
        );
    }
}

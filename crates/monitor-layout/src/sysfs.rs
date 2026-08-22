//! The arrangement of last resort, read from `/sys/class/drm`.
//!
//! Nothing in sysfs records where a monitor sits on the desk — that is the display server's
//! business, and this source exists for when there is no display server to ask, or when the
//! one that is running speaks none of the interfaces the other sources know. What sysfs does
//! know is which connectors have a display on them, what that display is, and what mode it
//! prefers, which is enough to draw correctly proportioned screens in a plausible row.
//!
//! The row is a guess and is flagged as one: [`crate::LayoutSource::Sysfs`] reports
//! [`knows_arrangement`](crate::LayoutSource::knows_arrangement) as false.

use std::fs;
use std::path::Path;

use monitor_control::Edid;

use crate::{Layout, LayoutOutput, LayoutSource};

/// Read every connected connector and lay the results out left to right.
pub fn detect() -> Result<Layout, String> {
    detect_in(Path::new("/sys/class/drm"))
}

/// As [`detect`], but rooted at an arbitrary directory so it can be tested.
pub fn detect_in(root: &Path) -> Result<Layout, String> {
    let entries =
        fs::read_dir(root).map_err(|e| format!("reading {}: {e}", root.display()))?;

    let mut found: Vec<(String, LayoutOutput)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // `cardN` and `renderD128` are devices, not connectors; a writeback connector is a
        // virtual capture endpoint with nothing plugged into it.
        if !name.starts_with("card")
            || !name.contains('-')
            || name.to_ascii_uppercase().contains("WRITEBACK")
        {
            continue;
        }
        let dir = entry.path();
        if read_trimmed(&dir.join("status")).as_deref() != Some("connected") {
            continue;
        }

        let mut output = LayoutOutput {
            connector: crate::normalize_connector(&name),
            size: preferred_mode(&dir).unwrap_or((1920, 1080)),
            ..Default::default()
        };

        // A connected connector can have an unreadable EDID while the display wakes up.
        // The connector is still worth reporting; it just goes unidentified.
        if let Ok(raw) = fs::read(dir.join("edid"))
            && let Ok(edid) = Edid::parse(&raw)
        {
            output.manufacturer = Some(edid.id.manufacturer.clone());
            output.model = edid.id.model.clone();
            output.serial = edid.id.serial_string.clone();
            output.edid_key = Some(edid.id.key());
            output.size_mm = edid.size_mm.map(|(w, h)| (u32::from(w), u32::from(h)));
        }

        found.push((name, output));
    }

    // Sorting by the sysfs name rather than the normalised one keeps DP-1 ahead of HDMI-1
    // in the same order every other tool in the project lists them.
    found.sort_by(|a, b| a.0.cmp(&b.0));

    let mut x = 0;
    let outputs = found
        .into_iter()
        .map(|(_, mut output)| {
            output.position = (x, 0);
            x += output.size.0 as i32;
            output
        })
        .collect();

    Ok(Layout { outputs, source: LayoutSource::Sysfs, warnings: Vec::new() })
}

/// The first mode listed for a connector, which the kernel orders best-first.
fn preferred_mode(dir: &Path) -> Option<(u32, u32)> {
    let modes = fs::read_to_string(dir.join("modes")).ok()?;
    let first = modes.lines().next()?.trim();
    // Modes read `3840x2160`, sometimes with a trailing `i` for interlaced.
    let (w, h) = first.split_once('x')?;
    let h = h.trim_end_matches(|c: char| !c.is_ascii_digit());
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory no other process can be using: the pid keeps concurrent
    /// `cargo test` runs (two worktrees, two users) apart, and `create_dir` rather than
    /// `create_dir_all` refuses to reuse a leftover tree instead of writing into it.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("monitor-layout-sysfs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).expect("scratch directory");
        dir
    }

    /// Build a synthetic `/sys/class/drm` tree.
    fn fake_drm(dir: &Path, connectors: &[(&str, &str, &str)]) {
        for (name, status, modes) in connectors {
            let c = dir.join(name);
            fs::create_dir_all(&c).unwrap();
            fs::write(c.join("status"), status).unwrap();
            fs::write(c.join("modes"), modes).unwrap();
        }
    }

    #[test]
    fn connected_connectors_are_laid_out_in_a_row() {
        let tmp = scratch("row");
        fake_drm(
            &tmp,
            &[
                ("card1-DP-1", "connected", "3840x2160\n1920x1080\n"),
                ("card1-HDMI-A-1", "connected", "1920x1080\n"),
                ("card1-DP-2", "disconnected", ""),
                ("card1-Writeback-1", "connected", "1920x1080\n"),
                ("card1", "", ""),
            ],
        );

        let layout = detect_in(&tmp).unwrap();
        let names: Vec<&str> = layout.outputs.iter().map(|o| o.connector.as_str()).collect();
        assert_eq!(names, ["DP-1", "HDMI-1"]);
        assert_eq!(layout.outputs[0].position, (0, 0));
        assert_eq!(layout.outputs[0].size, (3840, 2160));
        // The second screen starts where the first ends, so nothing overlaps.
        assert_eq!(layout.outputs[1].position, (3840, 0));
        assert!(!layout.is_arrangement_known());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn interlaced_modes_still_parse() {
        let tmp = scratch("interlaced");
        fake_drm(&tmp, &[("card1-HDMI-A-1", "connected", "1920x1080i\n")]);

        let layout = detect_in(&tmp).unwrap();
        assert_eq!(layout.outputs[0].size, (1920, 1080));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_connector_with_no_modes_file_still_appears() {
        let tmp = scratch("nomodes");
        let c = tmp.join("card1-DP-1");
        fs::create_dir_all(&c).unwrap();
        fs::write(c.join("status"), "connected").unwrap();

        let layout = detect_in(&tmp).unwrap();
        assert_eq!(layout.outputs.len(), 1);
        assert_eq!(layout.outputs[0].size, (1920, 1080));

        let _ = fs::remove_dir_all(&tmp);
    }
}

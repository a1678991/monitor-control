//! Enumerating the physical routes from this machine to each display.
//!
//! Discovery walks `/sys/class/drm` rather than probing I2C buses blindly. That yields the
//! connector name (`card1-DP-1`, matching what `xrandr` reports), the EDID, and the set of
//! I2C adapters that connector might carry DDC/CI on — all without putting a single byte on
//! the wire.
//!
//! One connector can offer more than one candidate bus. On DisplayPort the kernel exposes
//! both a physical I2C adapter via the `ddc` symlink and an AUX-channel adapter nested
//! inside the connector directory, and DDC/CI only works over the latter. Which one is live
//! cannot be known without asking, so discovery collects every candidate and leaves the
//! decision to a probe.

use std::fs;
use std::path::{Path, PathBuf};

use crate::edid::Edid;
use crate::error::{Error, Result};

/// Where in sysfs a candidate I2C bus came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum PathSource {
    /// An adapter nested inside the connector directory, i.e. a DisplayPort AUX channel.
    ///
    /// This is the bus DDC/CI actually works over on DisplayPort links.
    DpAux,
    /// The adapter the connector's `ddc` symlink points at.
    ///
    /// The right bus for HDMI, DVI, and VGA; present but usually inert for DisplayPort.
    DdcLink,
}

/// The physical connector kind, parsed from the DRM connector name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum ConnectorKind {
    /// An external DisplayPort connector.
    DisplayPort,
    /// An internal panel on an embedded DisplayPort link.
    EmbeddedDisplayPort,
    /// An HDMI connector.
    Hdmi,
    /// A DVI connector, either digital or integrated.
    Dvi,
    /// An analog VGA connector.
    Vga,
    /// A connector kind this crate does not classify.
    Other,
}

impl ConnectorKind {
    /// Classify a DRM connector name such as `card1-HDMI-A-1`.
    fn from_connector_name(name: &str) -> Self {
        // Strip the `cardN-` prefix so the kind is not confused by the card index.
        let tail = name.split_once('-').map_or(name, |(_, rest)| rest);
        let upper = tail.to_ascii_uppercase();
        if upper.starts_with("EDP") {
            Self::EmbeddedDisplayPort
        } else if upper.starts_with("DP") || upper.starts_with("DISPLAYPORT") {
            Self::DisplayPort
        } else if upper.starts_with("HDMI") {
            Self::Hdmi
        } else if upper.starts_with("DVI") {
            Self::Dvi
        } else if upper.starts_with("VGA") {
            Self::Vga
        } else {
            Self::Other
        }
    }
}

impl std::fmt::Display for ConnectorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::DisplayPort => "DisplayPort",
            Self::EmbeddedDisplayPort => "eDP",
            Self::Hdmi => "HDMI",
            Self::Dvi => "DVI",
            Self::Vga => "VGA",
            Self::Other => "other",
        })
    }
}

/// One candidate I2C bus reachable through a connector.
#[derive(Debug, Clone)]
pub struct CandidateBus {
    /// The I2C bus number, i.e. the `N` in `/dev/i2c-N`.
    pub bus: u32,
    /// The device node for this bus.
    pub device: PathBuf,
    /// The kernel's name for the adapter, e.g. `AMDGPU DM aux hw bus 0`.
    pub adapter_name: Option<String>,
    /// How this candidate was found.
    pub source: PathSource,
}

/// A connected DRM connector, its EDID, and the buses it might speak DDC/CI on.
#[derive(Debug, Clone)]
pub struct DiscoveredConnector {
    /// The DRM connector name, e.g. `card1-DP-1`.
    pub name: String,
    /// The connector's physical kind.
    pub kind: ConnectorKind,
    /// The EDID read from sysfs.
    pub edid: Edid,
    /// Candidate I2C buses, ordered most likely to work first.
    pub buses: Vec<CandidateBus>,
}

/// The sysfs root for DRM devices. A constant so tests can reason about it.
const DRM_ROOT: &str = "/sys/class/drm";

/// Enumerate every connected display, without generating any I2C traffic.
///
/// Connectors that are disconnected, or whose EDID is missing or unreadable, are skipped:
/// a connector with no EDID has nothing to identify or talk to.
pub fn discover_connectors() -> Result<Vec<DiscoveredConnector>> {
    discover_connectors_in(Path::new(DRM_ROOT))
}

/// As [`discover_connectors`], but rooted at an arbitrary directory so it can be tested
/// against a synthetic sysfs tree.
pub fn discover_connectors_in(root: &Path) -> Result<Vec<DiscoveredConnector>> {
    let entries = fs::read_dir(root)
        .map_err(|e| Error::io(format!("reading {}", root.display()), e))?;

    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(format!("reading {}", root.display()), e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let dir = entry.path();

        // Connector directories are `cardN-<CONNECTOR>`; `cardN` itself and `renderD*`
        // are not connectors.
        if !name.contains('-') || !name.starts_with("card") {
            continue;
        }
        // A writeback connector is a virtual capture endpoint, not a physical display.
        if name.to_ascii_uppercase().contains("WRITEBACK") {
            continue;
        }
        if read_trimmed(&dir.join("status")).as_deref() != Some("connected") {
            continue;
        }

        // A connected connector can still have an unreadable EDID, for instance while the
        // display is waking up. Skipping it is better than failing the whole scan.
        let Ok(raw) = fs::read(dir.join("edid")) else { continue };
        if raw.is_empty() {
            continue;
        }
        let Ok(edid) = Edid::parse(&raw) else { continue };

        let buses = candidate_buses(&dir);
        if buses.is_empty() {
            continue;
        }

        found.push(DiscoveredConnector {
            kind: ConnectorKind::from_connector_name(&name),
            name,
            edid,
            buses,
        });
    }

    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// Collect the I2C buses a connector might carry DDC/CI on, best candidate first.
///
/// Nested AUX adapters come first because a DisplayPort connector exposes both kinds and
/// only the AUX one answers. HDMI and DVI connectors have no nested adapter, so their
/// `ddc` symlink naturally ends up first.
fn candidate_buses(connector_dir: &Path) -> Vec<CandidateBus> {
    let mut buses: Vec<CandidateBus> = Vec::new();

    let mut push = |bus: u32, source: PathSource| {
        // The same adapter can be reachable both ways; keep whichever was found first,
        // since the ordering already encodes which interpretation to prefer.
        if buses.iter().any(|c| c.bus == bus) {
            return;
        }
        buses.push(CandidateBus {
            bus,
            device: PathBuf::from(format!("/dev/i2c-{bus}")),
            adapter_name: adapter_name(bus),
            source,
        });
    };

    if let Ok(entries) = fs::read_dir(connector_dir) {
        // Sort so a connector exposing several AUX adapters enumerates deterministically.
        let mut nested: Vec<u32> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| parse_i2c_bus_number(&e.file_name().to_string_lossy()))
            .collect();
        nested.sort_unstable();
        for bus in nested {
            push(bus, PathSource::DpAux);
        }
    }

    if let Ok(target) = fs::canonicalize(connector_dir.join("ddc"))
        && let Some(bus) = target
            .file_name()
            .and_then(|n| parse_i2c_bus_number(&n.to_string_lossy()))
    {
        push(bus, PathSource::DdcLink);
    }

    buses
}

/// Parse `i2c-4` into `4`, rejecting anything else.
fn parse_i2c_bus_number(name: &str) -> Option<u32> {
    name.strip_prefix("i2c-")?.parse().ok()
}

/// Read the kernel's human-readable name for an I2C adapter.
fn adapter_name(bus: u32) -> Option<String> {
    read_trimmed(Path::new(&format!("/sys/class/i2c-dev/i2c-{bus}/name")))
}

/// Read a sysfs attribute and trim it, returning `None` if it cannot be read.
fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_kinds_are_parsed_from_drm_names() {
        let cases = [
            ("card1-DP-1", ConnectorKind::DisplayPort),
            ("card1-HDMI-A-1", ConnectorKind::Hdmi),
            ("card0-eDP-1", ConnectorKind::EmbeddedDisplayPort),
            ("card1-DVI-D-1", ConnectorKind::Dvi),
            ("card1-VGA-1", ConnectorKind::Vga),
            ("card1-Unknown-1", ConnectorKind::Other),
        ];
        for (name, expected) in cases {
            assert_eq!(ConnectorKind::from_connector_name(name), expected, "{name}");
        }
    }

    #[test]
    fn card_index_does_not_leak_into_kind_detection() {
        // A naive prefix check against the full name would mis-read `card1-...` here.
        assert_eq!(
            ConnectorKind::from_connector_name("card10-DP-2"),
            ConnectorKind::DisplayPort
        );
    }

    #[test]
    fn i2c_bus_numbers_parse_strictly() {
        assert_eq!(parse_i2c_bus_number("i2c-4"), Some(4));
        assert_eq!(parse_i2c_bus_number("i2c-10"), Some(10));
        assert_eq!(parse_i2c_bus_number("drm_dp_aux0"), None);
        assert_eq!(parse_i2c_bus_number("i2c-"), None);
        assert_eq!(parse_i2c_bus_number("power"), None);
    }
}

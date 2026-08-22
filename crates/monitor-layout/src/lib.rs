//! Where the desktop puts each monitor.
//!
//! [`monitor_control`] answers "which monitors are attached and how do I talk to them".
//! It deliberately stops there: a DRM connector knows nothing about the arrangement, because
//! the arrangement is a property of the display server, not of the hardware. This crate
//! supplies the missing half, so a GUI can draw monitors where the user actually put them
//! rather than in an arbitrary list.
//!
//! # Sources
//!
//! There is no single interface every desktop implements, so each one is asked in turn and
//! the first that answers wins:
//!
//! | [`LayoutSource`] | Where it works |
//! | --- | --- |
//! | [`LayoutSource::WlrOutputManagement`] | sway, Hyprland, river, Wayfire, labwc |
//! | [`LayoutSource::Mutter`] | GNOME, on Wayland and on X11 |
//! | [`LayoutSource::Randr`] | any X11 session, including XWayland |
//! | [`LayoutSource::Sysfs`] | anywhere, but the arrangement is a guess |
//!
//! The sysfs source is the floor rather than a real answer: with no display server to ask,
//! it reports each connector's preferred mode and lays them out left to right in connector
//! order. [`Layout::is_arrangement_known`] tells the two cases apart so a GUI can say so
//! rather than presenting a fiction as fact.
//!
//! # Matching outputs to monitors
//!
//! A display server names a connector `DP-1`; DRM calls the same connector `card1-DP-1`;
//! and one physical monitor plugged in twice is two outputs but one
//! [`Monitor`]. [`Layout::match_monitors`] resolves all three
//! mismatches — see there for how.

#![warn(missing_docs)]

use std::borrow::Cow;
use std::collections::HashMap;

use monitor_control::Monitor;

pub mod mutter;
pub mod randr;
pub mod sysfs;
pub mod wlr;

mod watch;

pub use watch::LayoutWatcher;

/// Which interface a layout came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutSource {
    /// `zwlr_output_management_v1`, as implemented by the wlroots-derived compositors.
    WlrOutputManagement,
    /// GNOME's `org.gnome.Mutter.DisplayConfig` on the session bus.
    Mutter,
    /// The X11 RandR extension.
    Randr,
    /// `/sys/class/drm`, which knows the connectors but not the arrangement.
    Sysfs,
}

impl LayoutSource {
    /// A short label naming the interface this came from.
    pub fn label(self) -> &'static str {
        match self {
            Self::WlrOutputManagement => "wlr-output-management",
            Self::Mutter => "mutter",
            Self::Randr => "randr",
            Self::Sysfs => "sysfs",
        }
    }

    /// Whether this source reports where the user actually placed each monitor.
    ///
    /// False only for [`LayoutSource::Sysfs`], whose positions are invented.
    pub fn knows_arrangement(self) -> bool {
        !matches!(self, Self::Sysfs)
    }
}

/// How an output is rotated within the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    /// Not rotated.
    #[default]
    Normal,
    /// Rotated a quarter turn clockwise.
    Rotate90,
    /// Rotated a half turn.
    Rotate180,
    /// Rotated a quarter turn anticlockwise.
    Rotate270,
}

impl Rotation {
    /// Whether this rotation exchanges width and height.
    pub fn is_quarter_turn(self) -> bool {
        matches!(self, Self::Rotate90 | Self::Rotate270)
    }

    /// A short label, e.g. `90°`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "0°",
            Self::Rotate90 => "90°",
            Self::Rotate180 => "180°",
            Self::Rotate270 => "270°",
        }
    }
}

/// One screen, positioned the way the desktop has it.
///
/// "Screen" rather than "monitor" on purpose: a monitor cabled to the machine twice is one
/// monitor but two screens, and the desktop places each of them separately.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutOutput {
    /// The connector name the display server uses, e.g. `DP-1` or `HDMI-1`.
    pub connector: String,
    /// A human-readable description, when the source provides one.
    pub description: Option<String>,
    /// The EDID manufacturer's three-letter PNP ID, e.g. `DEL`.
    pub manufacturer: Option<String>,
    /// The EDID model name, e.g. `DELL G3223Q`.
    pub model: Option<String>,
    /// The EDID serial number string.
    pub serial: Option<String>,
    /// The stable monitor key, when the source exposes the raw EDID to derive it from.
    ///
    /// Sources that hand over EDID bytes give the strongest possible match, since this is
    /// the same key [`Monitor::key`] produces.
    pub edid_key: Option<String>,
    /// Top-left corner in the desktop's coordinate space.
    pub position: (i32, i32),
    /// Size in the desktop's coordinate space, after scaling and rotation.
    pub size: (u32, u32),
    /// The scale factor the desktop applies to this output.
    pub scale: f64,
    /// How the output is rotated.
    pub rotation: Rotation,
    /// Whether the desktop treats this output as primary.
    pub primary: bool,
    /// Physical panel size in millimetres, when known.
    pub size_mm: Option<(u32, u32)>,
    /// Refresh rate of the current mode, in Hz.
    pub refresh_hz: Option<f64>,
}

impl Default for LayoutOutput {
    fn default() -> Self {
        Self {
            connector: String::new(),
            description: None,
            manufacturer: None,
            model: None,
            serial: None,
            edid_key: None,
            position: (0, 0),
            size: (0, 0),
            // Not zero: an unset scale that divides into a size would produce infinities.
            scale: 1.0,
            rotation: Rotation::Normal,
            primary: false,
            size_mm: None,
            refresh_hz: None,
        }
    }
}

impl LayoutOutput {
    /// The rectangle this output occupies, as `(left, top, right, bottom)`.
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        let (x, y) = self.position;
        (x, y, x + self.size.0 as i32, y + self.size.1 as i32)
    }

    /// The screen diagonal in inches, when the physical size is known.
    pub fn diagonal_inches(&self) -> Option<f64> {
        let (w, h) = self.size_mm?;
        let (w, h) = (f64::from(w), f64::from(h));
        Some((w * w + h * h).sqrt() / 25.4)
    }

    /// A one-line summary such as `3840×2160 @ 120 Hz`.
    ///
    /// The size is the space the screen occupies on the desktop, which on a scaled display is
    /// smaller than the mode it is running — hence the scale on the end when there is one.
    pub fn describe_mode(&self) -> String {
        let mut text = format!("{}×{}", self.size.0, self.size.1);
        if let Some(hz) = self.refresh_hz {
            text.push_str(&format!(" @ {hz:.0} Hz"));
        }
        if (self.scale - 1.0).abs() > 0.001 {
            text.push_str(&format!(" ×{:.2}", self.scale));
        }
        text
    }
}

/// Every screen the desktop knows about, and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// The screens, in the order the source reported them.
    pub outputs: Vec<LayoutOutput>,
    /// Which interface answered.
    pub source: LayoutSource,
    /// Anything that went wrong on the way to this answer.
    ///
    /// A source failing is not fatal — the next one is tried — but it is worth showing,
    /// because "GNOME's display service did not answer" explains an arrangement that looks
    /// wrong far better than silently falling back does.
    pub warnings: Vec<String>,
}

impl Layout {
    /// Whether these positions reflect a real desktop arrangement.
    pub fn is_arrangement_known(&self) -> bool {
        self.source.knows_arrangement()
    }

    /// The bounding box of every output, as `(left, top, right, bottom)`.
    ///
    /// Returns `None` when there are no outputs, which a caller has to handle anyway before
    /// it can scale anything into a canvas.
    pub fn bounds(&self) -> Option<(i32, i32, i32, i32)> {
        self.outputs.iter().map(LayoutOutput::bounds).reduce(|a, b| {
            (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
        })
    }

    /// A value that changes whenever the arrangement does.
    ///
    /// Compared rather than inspected: a watcher polls for a new layout and needs to know
    /// whether anything moved without diffing every field itself.
    pub fn fingerprint(&self) -> String {
        let mut parts: Vec<String> = self
            .outputs
            .iter()
            .map(|o| {
                format!(
                    "{}:{},{}:{}x{}:{:.3}:{:?}:{}:{}:{}",
                    o.connector,
                    o.position.0,
                    o.position.1,
                    o.size.0,
                    o.size.1,
                    o.scale,
                    o.rotation,
                    o.primary,
                    // Two displays swapping cables moves nothing and resizes nothing, but it
                    // is exactly the change a watcher has to notice.
                    o.model.as_deref().unwrap_or(""),
                    o.serial.as_deref().unwrap_or(""),
                )
            })
            .collect();
        // The order outputs arrive in is not guaranteed to be stable between polls, and a
        // reordering is not a change the user made.
        parts.sort();
        format!("{}|{}", self.source.label(), parts.join(";"))
    }

    /// Pair each output with the monitor it belongs to.
    ///
    /// The returned vector is parallel to [`Layout::outputs`], holding an index into
    /// `monitors` or `None` for a screen with no DDC/CI-capable monitor behind it. Several
    /// outputs may share one index: that is the two-cables-one-monitor case, and it is the
    /// reason this returns a mapping rather than reordering anything.
    pub fn match_monitors(&self, monitors: &[Monitor]) -> Vec<Option<usize>> {
        let identities: Vec<MonitorIdentity<'_>> =
            monitors.iter().map(MonitorIdentity::of).collect();
        self.match_identities(&identities)
    }

    /// As [`Layout::match_monitors`], for a caller holding its own copy of the identities.
    ///
    /// Three matches are tried per output, strongest first:
    ///
    /// 1. the stable EDID key, when the source could read raw EDID bytes;
    /// 2. the connector name, normalised because DRM says `card1-HDMI-A-1` where a display
    ///    server says `HDMI-1`;
    /// 3. the EDID manufacturer, model, and serial as three separate strings, which is all
    ///    a source that never saw the raw bytes can offer.
    pub fn match_identities(&self, monitors: &[MonitorIdentity<'_>]) -> Vec<Option<usize>> {
        // Only keys that name exactly one monitor. Two units of the same model with no serial
        // number share a key, and a map keyed on that would quietly hand both screens to
        // whichever of them was inserted last.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for m in monitors {
            *counts.entry(m.key.as_ref()).or_default() += 1;
        }
        let by_key: HashMap<&str, usize> = monitors
            .iter()
            .enumerate()
            .filter(|(_, m)| counts.get(m.key.as_ref()) == Some(&1))
            .map(|(i, m)| (m.key.as_ref(), i))
            .collect();

        // One monitor owns several connectors, so this maps many names onto one index.
        let mut by_connector: HashMap<String, usize> = HashMap::new();
        for (i, m) in monitors.iter().enumerate() {
            for connector in &m.connectors {
                by_connector.entry(normalize_connector(connector)).or_insert(i);
            }
        }

        self.outputs
            .iter()
            .map(|out| {
                if let Some(key) = &out.edid_key
                    && let Some(&i) = by_key.get(key.as_str())
                {
                    return Some(i);
                }
                if let Some(&i) = by_connector.get(&normalize_connector(&out.connector))
                    // A name is the weaker evidence: two sources can number connectors
                    // differently, and an output that carries EDID strings saying this is a
                    // different display is telling the truth over the name.
                    && !monitors[i].contradicts(out)
                {
                    return Some(i);
                }
                // Ambiguity is reported as ambiguity. Two displays of the same model with no
                // serial number fit an output's description equally well, and guessing would
                // put one screen's controls on the other screen's panel.
                let mut hits = monitors.iter().enumerate().filter(|(_, m)| m.matches(out));
                match (hits.next(), hits.next()) {
                    (Some((index, _)), None) => Some(index),
                    _ => None,
                }
            })
            .collect()
    }
}

/// What matching an output to a monitor needs to know about that monitor.
///
/// Separate from [`Monitor`] so a caller that keeps its own copy — a window holding a
/// snapshot taken off a worker thread, say — can match without the live object, which owns an
/// open file descriptor and cannot leave the thread that drives it.
#[derive(Debug, Clone)]
pub struct MonitorIdentity<'a> {
    /// The stable key, as [`Monitor::key`] produces.
    ///
    /// Owned or borrowed as the caller has it: [`Monitor`] builds its key on demand, while a
    /// caller holding a snapshot has one already.
    pub key: Cow<'a, str>,
    /// Every connector this monitor is reachable on, in any spelling.
    pub connectors: Vec<&'a str>,
    /// The EDID manufacturer's three-letter PNP ID.
    pub manufacturer: Option<&'a str>,
    /// The EDID model name.
    pub model: Option<&'a str>,
    /// The EDID serial number string.
    pub serial: Option<&'a str>,
}

impl<'a> MonitorIdentity<'a> {
    /// Borrow the identifying fields of a discovered monitor.
    pub fn of(monitor: &'a Monitor) -> Self {
        Self {
            key: Cow::Owned(monitor.key()),
            connectors: monitor.paths.iter().map(|p| p.connector.as_str()).collect(),
            manufacturer: Some(&monitor.id.manufacturer),
            model: monitor.id.model.as_deref(),
            serial: monitor.id.serial_string.as_deref(),
        }
    }

    /// Whether an output's EDID strings describe this monitor.
    ///
    /// Every field the output supplies has to agree; fields it does not supply are ignored,
    /// since a source that reports no serial is not evidence that the serial differs. An
    /// output carrying no identifying strings at all matches nothing, because it would
    /// otherwise match whichever monitor happened to be first.
    fn matches(&self, out: &LayoutOutput) -> bool {
        let (manufacturer, model, serial) = self.compare(out);
        if [manufacturer, model, serial].into_iter().flatten().any(|agrees| !agrees) {
            return false;
        }
        // The manufacturer alone is not an identity: it would match every display of that make,
        // which on a desk of three Dells is exactly the wrong answer.
        model.is_some() || serial.is_some()
    }

    /// Whether an output's EDID strings say it is a *different* monitor from this one.
    ///
    /// Only the model and the serial count. A shared manufacturer proves nothing, and a field
    /// one side does not report is not evidence of disagreement.
    fn contradicts(&self, out: &LayoutOutput) -> bool {
        let (_, model, serial) = self.compare(out);
        [model, serial].into_iter().flatten().any(|agrees| !agrees)
    }

    /// Compare the three identifying strings, `None` where one side did not report one.
    fn compare(&self, out: &LayoutOutput) -> (Option<bool>, Option<bool>, Option<bool>) {
        let agree = |a: Option<&str>, b: Option<&str>| match (a, b) {
            (Some(a), Some(b)) => Some(a.trim().eq_ignore_ascii_case(b.trim())),
            _ => None,
        };
        (
            agree(out.manufacturer.as_deref(), self.manufacturer),
            agree(out.model.as_deref(), self.model),
            agree(out.serial.as_deref(), self.serial),
        )
    }
}

/// Reduce a connector name to a form comparable across the interfaces that report it.
///
/// DRM prefixes the card index and spells out the HDMI variant: `card1-HDMI-A-1`. Display
/// servers drop both: `HDMI-1`. X11 drivers other than `modesetting` spell DisplayPort out
/// in full. Normalising to the display-server form lets a sysfs connector and a compositor
/// output be compared directly.
pub fn normalize_connector(name: &str) -> String {
    let mut text = name.trim().to_ascii_uppercase();

    // `card1-DP-2` and `card10-DP-2` both lose only the card prefix, never a name that
    // merely happens to start with the letters `card`.
    if let Some(rest) = text.strip_prefix("CARD")
        && let Some(dash) = rest.find('-')
        && rest[..dash].chars().all(|c| c.is_ascii_digit())
    {
        text = rest[dash + 1..].to_string();
    }

    // HDMI-A is the only kernel connector name a display server abbreviates. HDMI-B keeps
    // its letter on both sides, so it must not be caught by the same rule.
    if let Some(rest) = text.strip_prefix("HDMI-A-") {
        text = format!("HDMI-{rest}");
    }
    if let Some(rest) = text.strip_prefix("DISPLAYPORT-") {
        text = format!("DP-{rest}");
    }

    text
}

/// The environment variable that forces one source instead of trying them in order.
///
/// Set it to a [`LayoutSource::label`] value — `mutter`, `wlr-output-management`, `randr`, or
/// `sysfs` — when auto-detection picks a source that reports the wrong thing. A desktop this
/// crate has never seen may well implement two of these interfaces and get one of them wrong.
pub const SOURCE_OVERRIDE_ENV: &str = "MONITOR_LAYOUT_SOURCE";

/// A source's entry point: `None` when the interface is not present here at all.
type Provider = fn() -> Option<Result<Layout, String>>;

/// The sources to ask, best first.
///
/// Wayland before X11: under a Wayland session the X11 answer comes from XWayland, which
/// mirrors the compositor's layout rather than being the source of it — and it drops the
/// EDID, invents the physical sizes, and reports every rotated display as upright.
const PROVIDERS: [(LayoutSource, Provider); 3] = [
    (LayoutSource::WlrOutputManagement, wlr::detect),
    (LayoutSource::Mutter, mutter::detect),
    (LayoutSource::Randr, randr::detect),
];

/// Ask each source in turn and return the first arrangement that answers.
///
/// Always returns a [`Layout`]: when nothing else answers, sysfs reports the connectors with
/// an invented arrangement, which is more useful to a GUI than an error it cannot act on.
/// Setting [`SOURCE_OVERRIDE_ENV`] restricts this to one source.
pub fn detect() -> Layout {
    let forced = std::env::var(SOURCE_OVERRIDE_ENV).ok().map(|name| {
        let name = name.trim().to_ascii_lowercase();
        let found = [
            LayoutSource::WlrOutputManagement,
            LayoutSource::Mutter,
            LayoutSource::Randr,
            LayoutSource::Sysfs,
        ]
        .into_iter()
        .find(|source| source.label() == name);
        (name, found)
    });

    let mut warnings = Vec::new();
    let mut wanted = None;
    if let Some((name, found)) = forced {
        match found {
            Some(source) => wanted = Some(source),
            None => warnings.push(format!(
                "{SOURCE_OVERRIDE_ENV} names no known source `{name}`; asking every source"
            )),
        }
    }

    for (source, provider) in PROVIDERS {
        if wanted.is_some_and(|w| w != source) {
            continue;
        }
        match provider() {
            // The interface is not present here at all, which is the normal case for two of
            // the three and not worth reporting — unless it was asked for by name.
            None => {
                if wanted.is_some() {
                    warnings.push(format!("{} is not available here", source.label()));
                }
            }
            Some(Ok(layout)) if !layout.outputs.is_empty() => {
                let mut layout = layout;
                layout.warnings.splice(0..0, warnings);
                return layout;
            }
            Some(Ok(_)) => warnings.push(format!("{} reported no outputs", source.label())),
            Some(Err(e)) => warnings.push(format!("{}: {e}", source.label())),
        }
    }

    let mut layout = sysfs::detect().unwrap_or_else(|e| {
        warnings.push(format!("sysfs: {e}"));
        Layout { outputs: Vec::new(), source: LayoutSource::Sysfs, warnings: Vec::new() }
    });
    layout.warnings.splice(0..0, warnings);
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_names_normalise_to_the_display_server_form() {
        assert_eq!(normalize_connector("card1-HDMI-A-1"), "HDMI-1");
        assert_eq!(normalize_connector("card1-DP-2"), "DP-2");
        assert_eq!(normalize_connector("card10-DP-2"), "DP-2");
        assert_eq!(normalize_connector("DP-2"), "DP-2");
        assert_eq!(normalize_connector("card0-eDP-1"), "EDP-1");
        assert_eq!(normalize_connector("DisplayPort-0"), "DP-0");
    }

    #[test]
    fn normalising_leaves_hdmi_b_alone() {
        // HDMI-B keeps its letter on both sides, unlike HDMI-A.
        assert_eq!(normalize_connector("card1-HDMI-B-1"), "HDMI-B-1");
    }

    #[test]
    fn normalising_keeps_a_name_that_merely_starts_with_card() {
        assert_eq!(normalize_connector("CARDINAL-1"), "CARDINAL-1");
    }

    #[test]
    fn bounds_span_every_output() {
        let layout = Layout {
            outputs: vec![
                LayoutOutput { position: (0, 0), size: (3840, 2160), ..Default::default() },
                LayoutOutput { position: (3840, 200), size: (1920, 1080), ..Default::default() },
            ],
            source: LayoutSource::Mutter,
            warnings: Vec::new(),
        };
        assert_eq!(layout.bounds(), Some((0, 0, 5760, 2160)));
    }

    #[test]
    fn an_empty_layout_has_no_bounds() {
        let layout =
            Layout { outputs: Vec::new(), source: LayoutSource::Sysfs, warnings: Vec::new() };
        assert_eq!(layout.bounds(), None);
    }

    #[test]
    fn fingerprints_ignore_the_order_outputs_arrive_in() {
        let a = LayoutOutput {
            connector: "DP-1".into(),
            position: (0, 0),
            size: (3840, 2160),
            ..Default::default()
        };
        let b = LayoutOutput {
            connector: "DP-2".into(),
            position: (3840, 0),
            size: (1920, 1080),
            ..Default::default()
        };
        let one = Layout {
            outputs: vec![a.clone(), b.clone()],
            source: LayoutSource::Mutter,
            warnings: Vec::new(),
        };
        let other =
            Layout { outputs: vec![b, a], source: LayoutSource::Mutter, warnings: Vec::new() };
        assert_eq!(one.fingerprint(), other.fingerprint());
    }

    /// A monitor reachable on the given DRM connectors.
    fn identity<'a>(key: &'a str, connectors: &[&'a str], model: Option<&'a str>) -> MonitorIdentity<'a> {
        MonitorIdentity {
            key: Cow::Borrowed(key),
            connectors: connectors.to_vec(),
            // The three-letter code from the key, so a test that sets up two makers gets two.
            manufacturer: Some(key.split('-').next().unwrap_or("DEL")),
            model,
            serial: None,
        }
    }

    fn output(connector: &str) -> LayoutOutput {
        LayoutOutput { connector: connector.into(), ..Default::default() }
    }

    #[test]
    fn outputs_match_monitors_across_the_drm_naming_difference() {
        // The display server says HDMI-1 where DRM says card1-HDMI-A-1.
        let monitors = [identity("DEL-1", &["card1-DP-1"], None), identity("IOC-1", &["card1-HDMI-A-1"], None)];
        let layout = Layout {
            outputs: vec![output("DP-1"), output("HDMI-1")],
            source: LayoutSource::Mutter,
            warnings: Vec::new(),
        };
        assert_eq!(layout.match_identities(&monitors), vec![Some(0), Some(1)]);
    }

    #[test]
    fn one_monitor_on_two_cables_matches_both_outputs() {
        // The case the whole crate exists for: two screens, one panel.
        let monitors = [identity("IOC-1", &["card1-DP-1", "card1-HDMI-A-1"], None)];
        let layout = Layout {
            outputs: vec![output("DP-1"), output("HDMI-1")],
            source: LayoutSource::Mutter,
            warnings: Vec::new(),
        };
        assert_eq!(layout.match_identities(&monitors), vec![Some(0), Some(0)]);
    }

    #[test]
    fn the_edid_key_outranks_the_connector_name() {
        // Connector names can be renumbered between the two sources; the key cannot.
        let monitors = [identity("DEL-A", &["card1-DP-1"], None), identity("DEL-B", &["card1-DP-2"], None)];
        let layout = Layout {
            outputs: vec![LayoutOutput {
                connector: "DP-1".into(),
                edid_key: Some("DEL-B".into()),
                ..Default::default()
            }],
            source: LayoutSource::Randr,
            warnings: Vec::new(),
        };
        assert_eq!(layout.match_identities(&monitors), vec![Some(1)]);
    }

    #[test]
    fn edid_strings_match_when_the_connector_name_does_not() {
        let monitors = [identity("DEL-1", &["card9-DP-7"], Some("DELL G3223Q"))];
        let layout = Layout {
            outputs: vec![LayoutOutput {
                connector: "Unknown-3".into(),
                manufacturer: Some("DEL".into()),
                model: Some("DELL G3223Q".into()),
                ..Default::default()
            }],
            source: LayoutSource::Mutter,
            warnings: Vec::new(),
        };
        assert_eq!(layout.match_identities(&monitors), vec![Some(0)]);
    }

    #[test]
    fn the_manufacturer_alone_is_not_an_identity() {
        // Three displays from the same maker would otherwise all attach to the first one.
        let monitors = [
            identity("DEL-A01F-2021w14", &["card0-DP-1"], None),
            identity("DEL-A020-2022w03", &["card0-DP-2"], None),
        ];
        let layout = Layout {
            // The X11 drivers other than modesetting number connectors their own way, so
            // these names never normalise onto the DRM ones.
            outputs: vec![output("DP-0"), output("DP-4")],
            source: LayoutSource::Mutter,
            warnings: Vec::new(),
        };
        assert_eq!(layout.match_identities(&monitors), vec![None, None]);
    }

    #[test]
    fn two_indistinguishable_displays_match_neither() {
        // Same model, no serial, and connector names that do not line up: nothing here can
        // tell them apart, and attaching both screens to one of them would be a guess
        // presented as a fact.
        let monitors = [
            identity("DEL-A01F-2021w14", &["card0-DP-1"], Some("DELL U2723")),
            identity("DEL-A01F-2022w03", &["card0-DP-2"], Some("DELL U2723")),
        ];
        let layout = Layout {
            outputs: vec![
                LayoutOutput {
                    connector: "DP-0".into(),
                    manufacturer: Some("DEL".into()),
                    model: Some("DELL U2723".into()),
                    ..Default::default()
                },
                LayoutOutput {
                    connector: "DP-4".into(),
                    manufacturer: Some("DEL".into()),
                    model: Some("DELL U2723".into()),
                    ..Default::default()
                },
            ],
            source: LayoutSource::Mutter,
            warnings: Vec::new(),
        };
        assert_eq!(layout.match_identities(&monitors), vec![None, None]);
    }

    #[test]
    fn a_shared_key_is_not_used_to_match() {
        // Two units of the same model with no serial number produce the same key. Matching on
        // it would hand both screens to whichever of them the map happened to keep.
        let monitors = [
            identity("IOC-9040-2024w28", &["card1-DP-9"], Some("27M2V")),
            identity("IOC-9040-2024w28", &["card1-DP-8"], Some("27M2V")),
        ];
        let layout = Layout {
            outputs: vec![LayoutOutput {
                connector: "DP-7".into(),
                edid_key: Some("IOC-9040-2024w28".into()),
                ..Default::default()
            }],
            source: LayoutSource::Randr,
            warnings: Vec::new(),
        };
        assert_eq!(layout.match_identities(&monitors), vec![None]);
    }

    #[test]
    fn edid_strings_outrank_a_connector_name_that_disagrees() {
        // Two sources can number connectors differently; a model name that says otherwise is
        // the stronger evidence.
        let monitors = [
            identity("DEL-1", &["card1-DP-1"], Some("DELL G3223Q")),
            identity("BNQ-1", &["card1-DP-2"], Some("BenQ PD3200U")),
        ];
        let layout = Layout {
            outputs: vec![LayoutOutput {
                connector: "DP-1".into(),
                manufacturer: Some("BNQ".into()),
                model: Some("BenQ PD3200U".into()),
                ..Default::default()
            }],
            source: LayoutSource::Mutter,
            warnings: Vec::new(),
        };
        assert_eq!(layout.match_identities(&monitors), vec![Some(1)]);
    }

    #[test]
    fn fingerprints_notice_two_displays_swapping_cables() {
        // Nothing moves and nothing resizes; only which panel is on which cable changes.
        let screen = |connector: &str, model: &str| LayoutOutput {
            connector: connector.into(),
            model: Some(model.into()),
            position: (0, 0),
            size: (3840, 2160),
            ..Default::default()
        };
        let before = Layout {
            outputs: vec![screen("DP-1", "A"), screen("DP-2", "B")],
            source: LayoutSource::Mutter,
            warnings: Vec::new(),
        };
        let after = Layout {
            outputs: vec![screen("DP-1", "B"), screen("DP-2", "A")],
            source: LayoutSource::Mutter,
            warnings: Vec::new(),
        };
        assert_ne!(before.fingerprint(), after.fingerprint());
    }

    #[test]
    fn an_output_that_identifies_nothing_matches_nothing() {
        // Matching on no evidence at all would silently attach the first monitor to it.
        let monitors = [identity("DEL-1", &["card1-DP-1"], Some("DELL G3223Q"))];
        let layout = Layout {
            outputs: vec![output("DP-9")],
            source: LayoutSource::Sysfs,
            warnings: Vec::new(),
        };
        assert_eq!(layout.match_identities(&monitors), vec![None]);
    }

    #[test]
    fn a_disagreeing_model_is_not_a_match() {
        let monitors = [identity("DEL-1", &["card1-DP-1"], Some("DELL G3223Q"))];
        let layout = Layout {
            outputs: vec![LayoutOutput {
                connector: "DP-9".into(),
                manufacturer: Some("DEL".into()),
                model: Some("DELL U2723".into()),
                ..Default::default()
            }],
            source: LayoutSource::Mutter,
            warnings: Vec::new(),
        };
        assert_eq!(layout.match_identities(&monitors), vec![None]);
    }

    #[test]
    fn fingerprints_change_when_a_monitor_moves() {
        let mut layout = Layout {
            outputs: vec![LayoutOutput {
                connector: "DP-1".into(),
                position: (0, 0),
                size: (3840, 2160),
                ..Default::default()
            }],
            source: LayoutSource::Mutter,
            warnings: Vec::new(),
        };
        let before = layout.fingerprint();
        layout.outputs[0].position = (1920, 0);
        assert_ne!(before, layout.fingerprint());
    }
}

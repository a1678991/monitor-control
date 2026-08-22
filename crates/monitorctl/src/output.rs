//! Rendering results as text or JSON.

use monitor_control::{
    Capabilities, Monitor, MonitorPath, PathStatus, VcpValue, monitor::describe_vcp, vcp,
};
use serde::Serialize;

/// One monitor's summary, as emitted by `detect` and `paths`.
#[derive(Serialize)]
pub struct MonitorReport {
    pub name: String,
    pub key: String,
    pub manufacturer: String,
    pub product_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    pub year: u16,
    pub controllable: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ambiguous_grouping: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_path: Option<String>,
    pub paths: Vec<PathReport>,
}

/// One route to a monitor.
#[derive(Serialize)]
pub struct PathReport {
    pub connector: String,
    pub connector_kind: String,
    pub bus: u32,
    pub device: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    pub source: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcp_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl PathReport {
    fn new(p: &MonitorPath) -> Self {
        let (vcp_version, detail) = match &p.status {
            PathStatus::Working { vcp_version } => (
                vcp_version.map(|(maj, min)| format!("{maj}.{min}")),
                None,
            ),
            PathStatus::Unavailable { reason } => (None, Some(reason.clone())),
            PathStatus::DdcDisabled => (
                None,
                Some("display returned a null message; enable DDC/CI in its OSD menu".into()),
            ),
            _ => (None, None),
        };
        Self {
            connector: p.connector.clone(),
            connector_kind: p.connector_kind.to_string(),
            bus: p.bus,
            device: p.device.display().to_string(),
            adapter: p.adapter_name.clone(),
            source: match p.source {
                monitor_control::PathSource::DpAux => "dp-aux".into(),
                monitor_control::PathSource::DdcLink => "ddc-link".into(),
            },
            status: p.status.label().to_string(),
            vcp_version,
            detail,
        }
    }
}

impl MonitorReport {
    pub fn new(m: &Monitor) -> Self {
        Self {
            name: m.name(),
            key: m.key(),
            manufacturer: m.id.manufacturer.clone(),
            product_code: m.id.product_code,
            serial: m.id.serial_string.clone().filter(|s| !s.is_empty()),
            year: m.id.year,
            controllable: m.is_controllable(),
            ambiguous_grouping: m.ambiguous_grouping,
            active_path: m.active_path().map(MonitorPath::describe),
            paths: m.paths.iter().map(PathReport::new).collect(),
        }
    }
}

/// The result of one operation against one monitor.
#[derive(Serialize)]
pub struct OpResult {
    pub monitor: String,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Outcome {
    Ok {
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        maximum: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        percent: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// A parsed capabilities string.
    Capabilities {
        capabilities: Capabilities,
    },
    Error {
        message: String,
    },
}

impl Outcome {
    pub fn value(v: &VcpValue) -> Self {
        Self::Ok {
            value: Some(v.current),
            maximum: Some(v.maximum),
            percent: v.percent().map(|p| (p * 10.0).round() / 10.0),
            text: None,
        }
    }

    pub fn written(value: u16) -> Self {
        Self::Ok { value: Some(value), maximum: None, percent: None, text: None }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self::Ok { value: None, maximum: None, percent: None, text: Some(text.into()) }
    }

    pub fn capabilities(caps: &Capabilities) -> Self {
        Self::Capabilities { capabilities: caps.clone() }
    }
}

/// Render `detect` output as text.
///
/// Only the working path is shown per monitor; `paths` exists for the full picture. A
/// monitor with no working path is called out along with what to do about it, since that
/// is the case a user most needs help with.
pub fn print_detect(reports: &[MonitorReport], show_all_paths: bool) {
    if reports.is_empty() {
        println!("No connected displays found.");
        return;
    }

    for (i, r) in reports.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let mark = if r.controllable { "" } else { "  [no DDC/CI]" };
        println!("{}{mark}", r.name);
        println!("  key:          {}", r.key);
        if let Some(serial) = &r.serial {
            println!("  serial:       {serial}");
        }

        let shown: Vec<&PathReport> = if show_all_paths {
            r.paths.iter().collect()
        } else {
            // Prefer the working paths; fall back to showing everything when none work,
            // because then the failures are the useful information.
            let working: Vec<&PathReport> =
                r.paths.iter().filter(|p| p.status == "working").collect();
            if working.is_empty() { r.paths.iter().collect() } else { working }
        };

        for p in shown {
            let version = p
                .vcp_version
                .as_ref()
                .map(|v| format!(", MCCS {v}"))
                .unwrap_or_default();
            println!(
                "  {:<16} {} ({}{version}) [{}]",
                p.connector, p.device, p.connector_kind, p.status
            );
            if let Some(detail) = &p.detail {
                println!("  {:<16} └─ {detail}", "");
            }
        }

        if r.ambiguous_grouping {
            println!(
                "  note:         this display reports no serial number, so paths were \
                 grouped by model alone; use --no-group if two identical monitors were merged"
            );
        }
        if !r.controllable {
            println!(
                "  note:         no path carries DDC/CI; check that it is enabled in the \
                 monitor's menu and that you can access its /dev/i2c-* node"
            );
        }
    }
}

/// Render the full path table as text.
pub fn print_paths(reports: &[MonitorReport]) {
    if reports.is_empty() {
        println!("No connected displays found.");
        return;
    }
    for (i, r) in reports.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("{} ({})", r.name, r.key);
        for p in &r.paths {
            let adapter = p.adapter.as_deref().unwrap_or("-");
            let active = if r.active_path.as_deref()
                == Some(&format!("{} via {}", p.connector, p.device))
            {
                " *"
            } else {
                "  "
            };
            println!(
                "{active}{:<16} {:<14} {:<10} {:<28} {}",
                p.connector, p.device, p.source, adapter, p.status
            );
            if let Some(detail) = &p.detail {
                println!("    └─ {detail}");
            }
        }
    }
    println!("\n* = path currently used for commands");
}

/// Render operation results as text, one line per monitor.
pub fn print_results(results: &[OpResult], label: &str) {
    for r in results {
        match &r.outcome {
            Outcome::Ok { value, maximum, percent, text } => {
                let rendered = if let Some(t) = text {
                    t.clone()
                } else {
                    match (value, maximum, percent) {
                        (Some(v), Some(max), Some(p)) => format!("{v} (max {max}, {p:.0}%)"),
                        (Some(v), _, _) => v.to_string(),
                        _ => "ok".to_string(),
                    }
                };
                println!("{}: {label} = {rendered}", r.monitor);
            }
            Outcome::Capabilities { capabilities } => {
                println!("{}: {label}", r.monitor);
                print_capabilities(capabilities);
            }
            Outcome::Error { message } => {
                eprintln!("{}: {message}", r.monitor);
            }
        }
    }
}

/// Render a raw VCP read as text.
pub fn format_vcp(code: u8, v: &VcpValue) -> String {
    let kind = if v.is_continuous() { "continuous" } else { "non-continuous" };
    format!(
        "{} = {} (max {}, low byte {:#04x}, {kind})",
        describe_vcp(code),
        v.current,
        v.maximum,
        v.low_byte()
    )
}

/// Render parsed capabilities as text.
///
/// The feature list is grouped rather than dumped: what a caller almost always wants from
/// this command is which inputs the display accepts, so that goes first and in full.
pub fn print_capabilities(caps: &Capabilities) {
    if let Some(model) = &caps.model {
        println!("  model:        {model}");
    }
    if let Some(kind) = &caps.display_type {
        println!("  type:         {kind}");
    }
    if let Some(version) = &caps.mccs_version {
        println!("  MCCS version: {version}");
    }

    let inputs = caps.input_sources();
    if !inputs.is_empty() {
        let rendered: Vec<String> = inputs
            .iter()
            .map(|i| format!("{i} ({:#04x})", i.value()))
            .collect();
        println!("  inputs:       {}", rendered.join(", "));
    } else if caps.supports(vcp::code::INPUT_SOURCE) {
        println!("  inputs:       supported, but the display did not enumerate them");
    }

    let named: Vec<String> = caps
        .vcp
        .keys()
        .filter_map(|&c| vcp::code_name(c).map(|n| format!("{c:#04x} {n}")))
        .collect();
    if !named.is_empty() {
        println!("  known features:");
        for feature in &named {
            println!("    {feature}");
        }
    }
    println!("  {} VCP features in total", caps.vcp.len());
}

//! Print the desktop's monitor arrangement and which monitor each screen belongs to.
//!
//! Run with `cargo run -p monitor-layout --example arrangement`. Useful for checking what
//! this crate sees on a desktop it has not been tried on before.

use monitor_control::{DdcConfig, MonitorManager};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let layout = monitor_layout::detect();

    println!("source: {}", layout.source.label());
    if !layout.is_arrangement_known() {
        println!("  (no display server answered — positions below are a guess)");
    }
    for warning in &layout.warnings {
        println!("  warning: {warning}");
    }

    // Discovery reads sysfs only, so this puts no traffic on any I2C bus.
    let manager = MonitorManager::discover(DdcConfig::default())?;
    let monitors = manager.monitors();
    let matches = layout.match_monitors(monitors);

    // Reading order: left to right, top row first.
    let mut order: Vec<usize> = (0..layout.outputs.len()).collect();
    order.sort_by_key(|&i| (layout.outputs[i].position.1, layout.outputs[i].position.0));

    println!();
    for (output, monitor) in order.iter().map(|&i| (&layout.outputs[i], &matches[i])) {
        let (x, y) = output.position;
        println!(
            "{:<10} {:>5}x{:<5} at {:>6},{:<6} {:>4}  {}",
            output.connector,
            output.size.0,
            output.size.1,
            x,
            y,
            output.rotation.label(),
            if output.primary { "primary" } else { "" },
        );
        // Not every source reports a physical size; the EDID always does, so fall back to
        // the monitor this screen was matched to.
        let diagonal = output.diagonal_inches().or_else(|| {
            let i = (*monitor)?;
            monitor_control::Edid::parse(&monitors[i].edid).ok()?.diagonal_inches()
        });
        println!(
            "           {} · {}",
            output.describe_mode(),
            diagonal.map(|d| format!("{d:.0}\"")).unwrap_or_else(|| "size unknown".into())
        );
        match monitor {
            Some(i) => println!("           DDC/CI: {} [{}]", monitors[*i].name(), monitors[*i].key()),
            None => println!("           DDC/CI: no monitor matched"),
        }
        println!();
    }

    let unmatched: Vec<String> = (0..monitors.len())
        .filter(|i| !matches.iter().any(|m| m == &Some(*i)))
        .map(|i| monitors[i].name())
        .collect();
    if !unmatched.is_empty() {
        println!("not part of the arrangement: {}", unmatched.join(", "));
    }

    Ok(())
}

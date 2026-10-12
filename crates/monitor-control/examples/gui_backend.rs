//! Sketch of how a GUI would drive this library.
//!
//! Run with `cargo run -p monitor-control --example gui_backend`.
//!
//! The shape to copy: discover once at startup, probe in the background, then keep a
//! `MonitorManager` in application state and call into it from event handlers. Discovery
//! generates no I2C traffic, so it is safe on the UI thread; probing and every get/set
//! blocks for tens of milliseconds per exchange and belongs on a worker.

use monitor_control::{DdcConfig, MonitorManager, ValueChange};

fn main() -> monitor_control::Result<()> {
    // Startup: enumerate without touching any bus, so a window can be drawn immediately.
    let mut manager = MonitorManager::discover(DdcConfig::default())?;

    println!("Discovered {} monitor(s)\n", manager.monitors().len());

    for index in 0..manager.monitors().len() {
        // Re-borrow per iteration so the loop can both read and mutate each monitor.
        let monitor = &mut manager.monitors_mut()[index];
        println!("{} [{}]", monitor.name(), monitor.key());

        // Probing decides which cable actually carries DDC/CI. A GUI would do this on a
        // worker thread and stream the results into its model.
        if monitor.ensure_path().is_err() {
            println!("  not controllable:");
            for path in &monitor.paths {
                println!("    {} -> {}", path.describe(), path.status.label());
            }
            println!();
            continue;
        }

        if let Some(path) = monitor.active_path() {
            println!("  controlled via {}", path.describe());
        }

        // Slider bounds come from the display, not from an assumed 0-100 range: plenty of
        // monitors report a maximum that is not 100.
        match monitor.brightness() {
            Ok(v) => println!("  brightness: {} of {}", v.current, v.maximum),
            Err(e) => println!("  brightness unavailable: {e}"),
        }

        // Ask which inputs are switchable by name, so a dropdown lists only real
        // options. This prefers the verified quirk table over the capabilities string,
        // because some displays advertise input values their firmware ignores.
        match monitor.available_inputs() {
            Ok(inputs) if inputs.is_empty() => {
                println!("  inputs: none verified; raw values may still work")
            }
            Ok(inputs) => {
                let names: Vec<String> = inputs.iter().map(|i| i.to_string()).collect();
                println!("  inputs: {}", names.join(", "));
            }
            Err(e) => println!("  inputs unavailable: {e}"),
        }
        println!();
    }

    // A slider drag maps onto one call; relative steps suit keyboard shortcuts.
    if let Some(monitor) = manager.monitors_mut().iter_mut().find(|m| m.is_controllable()) {
        let current = monitor.brightness()?;
        println!(
            "Example: stepping {} down by 5 and back up again",
            monitor.name()
        );
        monitor.set_brightness(ValueChange::Relative(-5))?;
        monitor.set_brightness(ValueChange::Absolute(current.current))?;
        println!("  restored to {}", current.current);
    }

    Ok(())
}

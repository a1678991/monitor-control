//! `monitorctl` — control monitor brightness and input source over DDC/CI.

mod cli;
mod output;

use std::process::ExitCode;

use clap::Parser;
use monitor_control::{
    Capabilities, DdcConfig, InputSource, Monitor, MonitorManager, Selector, code,
};

use cli::{Cli, Command, GlobalOpts, InputAction, LevelAction, VcpAction};
use output::{MonitorReport, OpResult, Outcome};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("monitorctl: {e}");
            // Walk the source chain so the underlying cause is not lost, which for I2C
            // errors is usually the part that says what to actually fix.
            let mut source = std::error::Error::source(&e);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> monitor_control::Result<ExitCode> {
    let g = cli.global.clone();
    let selector: Selector = cli.selector.into();
    let config = DdcConfig {
        retries: g.retries,
        sleep_multiplier: g.sleep_multiplier,
        strict_checksum: g.strict_checksum,
        ..DdcConfig::default()
    };

    let mut manager = if g.no_group {
        MonitorManager::discover_ungrouped(config)?
    } else {
        MonitorManager::discover(config)?
    };

    match cli.command {
        Command::Detect { probe_all } => {
            let targets = manager.select_required(&selector)?;
            for &i in &targets {
                let m = &mut manager.monitors_mut()[i];
                if probe_all {
                    m.probe_all();
                } else {
                    // A failure here is the answer, not an error: it means no path works,
                    // which the report then explains per path.
                    let _ = m.ensure_path();
                }
            }
            let reports: Vec<MonitorReport> = targets
                .iter()
                .map(|&i| MonitorReport::new(&manager.monitors()[i]))
                .collect();

            if g.json {
                print_json(&reports)?;
            } else {
                output::print_detect(&reports, probe_all);
            }
            let ok = reports.iter().any(|r| r.controllable);
            Ok(exit_code(ok || reports.is_empty()))
        }

        Command::Paths => {
            let targets = manager.select_required(&selector)?;
            for &i in &targets {
                manager.monitors_mut()[i].probe_all();
            }
            let reports: Vec<MonitorReport> = targets
                .iter()
                .map(|&i| MonitorReport::new(&manager.monitors()[i]))
                .collect();

            if g.json {
                print_json(&reports)?;
            } else {
                output::print_paths(&reports);
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::Brightness { action } => {
            level_command(&mut manager, &g, &selector, action, code::BRIGHTNESS, "brightness")
        }

        Command::Contrast { action } => {
            level_command(&mut manager, &g, &selector, action, code::CONTRAST, "contrast")
        }

        Command::Input { action } => match action {
            Some(InputAction::List) => {
                if g.json {
                    let names: Vec<_> = InputSource::all_names()
                        .map(|(n, v)| serde_json::json!({ "name": n, "value": v }))
                        .collect();
                    print_json(&names)?;
                } else {
                    println!("{:<16} VALUE", "NAME");
                    for (name, value) in InputSource::all_names() {
                        println!("{name:<16} {value:#04x}");
                    }
                    println!(
                        "\nVendor-specific inputs can be given as a raw value, e.g. `0x1b`."
                    );
                }
                Ok(ExitCode::SUCCESS)
            }
            Some(InputAction::Set { input }) => {
                let results = for_each(&mut manager, &selector, |m| {
                    if g.dry_run {
                        return Ok(Outcome::text(format!(
                            "would set input to {input} ({:#04x})",
                            input.value()
                        )));
                    }
                    m.set_input_source(input)?;
                    Ok(Outcome::text(format!("{input} ({:#04x})", input.value())))
                })?;
                report(&results, "input", &g)
            }
            _ => {
                let results = for_each(&mut manager, &selector, |m| {
                    let input = m.input_source()?;
                    Ok(Outcome::text(format!("{input} ({:#04x})", input.value())))
                })?;
                report(&results, "input", &g)
            }
        },

        Command::Vcp { action } => match action {
            VcpAction::Get { code } => {
                let results = for_each(&mut manager, &selector, |m| {
                    let v = m.get_vcp(code)?;
                    Ok(if g.json {
                        Outcome::value(&v)
                    } else {
                        Outcome::text(output::format_vcp(code, &v))
                    })
                })?;
                report(&results, "vcp", &g)
            }
            VcpAction::Set { code, value } => {
                let results = for_each(&mut manager, &selector, |m| {
                    if g.dry_run {
                        return Ok(Outcome::text(format!(
                            "would set {code:#04x} to {value} ({value:#06x})"
                        )));
                    }
                    m.set_vcp(code, value)?;
                    Ok(Outcome::written(value))
                })?;
                report(&results, &format!("vcp {code:#04x}"), &g)
            }
        },

        Command::Capabilities { raw } => {
            let results = for_each(&mut manager, &selector, |m| {
                let text = m.capabilities()?;
                if raw {
                    return Ok(Outcome::text(text));
                }
                Ok(Outcome::capabilities(&Capabilities::parse(&text)))
            })?;
            if g.json {
                print_json(&results)?;
                return Ok(exit_code(results.iter().all(is_ok)));
            }
            for r in &results {
                match &r.outcome {
                    Outcome::Ok { text: Some(t), .. } => println!("{}:\n{t}\n", r.monitor),
                    Outcome::Capabilities { capabilities } => {
                        println!("{}:", r.monitor);
                        output::print_capabilities(capabilities);
                        println!();
                    }
                    Outcome::Error { message } => eprintln!("{}: {message}", r.monitor),
                    _ => {}
                }
            }
            Ok(exit_code(results.iter().all(is_ok)))
        }
    }
}

/// Shared handling for the continuous features that only differ by VCP code.
fn level_command(
    manager: &mut MonitorManager,
    g: &GlobalOpts,
    selector: &Selector,
    action: Option<LevelAction>,
    vcp_code: u8,
    label: &str,
) -> monitor_control::Result<ExitCode> {
    let results = match action {
        Some(LevelAction::Set { value }) => for_each(manager, selector, |m| {
            if g.dry_run {
                let current = m.get_vcp(vcp_code)?;
                let target = value.resolve(current.current, current.maximum);
                return Ok(Outcome::text(format!(
                    "would set {label} to {target} (currently {})",
                    current.current
                )));
            }
            Ok(Outcome::written(m.adjust_vcp(vcp_code, value)?))
        })?,
        _ => for_each(manager, selector, |m| Ok(Outcome::value(&m.get_vcp(vcp_code)?)))?,
    };
    report(&results, label, g)
}

/// Apply an operation to every selected monitor, collecting per-monitor outcomes.
///
/// One monitor failing must not stop the others: the shell scripts this replaces set
/// brightness on several displays at once, and a sleeping third monitor should not prevent
/// the other two from changing.
fn for_each(
    manager: &mut MonitorManager,
    selector: &Selector,
    op: impl Fn(&mut Monitor) -> monitor_control::Result<Outcome>,
) -> monitor_control::Result<Vec<OpResult>> {
    let targets = manager.select_required(selector)?;

    let mut results = Vec::with_capacity(targets.len());
    for i in targets {
        let m = &mut manager.monitors_mut()[i];
        let outcome = match op(m) {
            Ok(o) => o,
            Err(e) => Outcome::Error { message: e.to_string() },
        };
        results.push(OpResult {
            monitor: m.name(),
            key: m.key(),
            path: m.active_path().map(|p| p.describe()),
            outcome,
        });
    }
    Ok(results)
}

fn report(results: &[OpResult], label: &str, g: &GlobalOpts) -> monitor_control::Result<ExitCode> {
    if g.json {
        print_json(results)?;
    } else {
        output::print_results(results, label);
    }
    Ok(exit_code(results.iter().all(is_ok)))
}

/// Whether an operation succeeded. Only the error variant counts as failure, so adding a
/// new success-carrying variant cannot silently turn a working command into exit code 1.
fn is_ok(r: &OpResult) -> bool {
    !matches!(r.outcome, Outcome::Error { .. })
}

fn exit_code(ok: bool) -> ExitCode {
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn print_json<T: serde::Serialize + ?Sized>(value: &T) -> monitor_control::Result<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| monitor_control::Error::Parse(format!("serialising output: {e}")))?;
    println!("{text}");
    Ok(())
}

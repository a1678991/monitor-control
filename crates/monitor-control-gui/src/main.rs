//! `monitorctl-gui` — a window for controlling monitor brightness and input over DDC/CI.
//!
//! The same library the command-line tool sits on, with the two things a window can do that a
//! command line cannot: show every monitor's brightness at once, and show them where they
//! actually are on the desk.

// A window with no console attached should not print to one either.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![warn(missing_docs)]

mod app;
mod backend;
mod band;
mod desk;
mod names;
mod paint;
mod rack;
mod state;
mod theme;

use std::process::ExitCode;

use eframe::egui;

/// Matches the `.desktop` file's basename, which is how Wayland compositors find the icon
/// and group the window. Changing one without the other loses both.
const APP_ID: &str = "monitor-control-gui";

fn main() -> ExitCode {
    let options = eframe::NativeOptions {
        // Remembering the window's geometry means asking the window manager where the window
        // was. On X11 that goes through winit's `inner_position`, which unwraps a
        // TranslateCoordinates reply — and by the time this runs the window is gone, so the
        // reply is a Window error and the unwrap takes the process with it. Wayland does not
        // use that code path, and there the compositor places windows anyway.
        persist_window: std::env::var_os("WAYLAND_DISPLAY").is_some(),
        viewport: egui::ViewportBuilder::default()
            .with_title("Monitor Control")
            .with_app_id(APP_ID)
            .with_inner_size([1040.0, 720.0])
            .with_min_inner_size([560.0, 420.0])
            .with_icon(icon()),
        ..Default::default()
    };

    let result = eframe::run_native(
        "Monitor Control",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(app::App::new(cc)))
        }),
    );

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("monitorctl-gui: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The window icon.
///
/// The same file the desktop entry points at, so the icon in the task bar and the icon in the
/// window cannot drift apart. Wayland compositors use the desktop entry and ignore this; X11
/// and everything else want the pixels.
fn icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("the bundled icon is a valid PNG")
}

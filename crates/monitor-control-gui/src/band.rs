//! The permission notice, the status rail, and the keys card.

use eframe::egui::{self, Align, Layout, RichText};

use crate::app::App;
use crate::state::Condition;
use crate::theme::Tokens;

/// The one-time permission change, as a block that can be pasted anywhere.
///
/// The rule is written with `printf` rather than copied out of the source tree, because the
/// person running it may be an administrator on a machine that has never seen this repository.
/// A `cp` of a path that does not exist there fails *after* the first two commands have already
/// succeeded, leaving the module loaded and no rule installed.
pub const PERMISSION_COMMANDS: &str = "\
echo i2c-dev | sudo tee /etc/modules-load.d/i2c-dev.conf
sudo modprobe i2c-dev
printf 'KERNEL==\"i2c-[0-9]*\", TAG+=\"uaccess\"\\n' \\
  | sudo tee /etc/udev/rules.d/60-ddcci-i2c.rules
sudo udevadm control --reload && sudo udevadm trigger";

/// Whether the permission notice belongs on screen.
pub fn permission_needed(app: &App) -> bool {
    app.access_denied
        || app.monitors.iter().any(|m| m.condition() == Condition::NoPermission)
}

/// The notice itself.
pub fn permission(app: &mut App, ui: &mut egui::Ui) {
    let tokens = Tokens::of(ui.ctx());

    if !app.show_permission {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Permission needed to talk to your monitors.").color(tokens.locked));
            if ui.button("Show the fix").clicked() {
                app.show_permission = true;
            }
        });
        return;
    }

    // No "DDC/CI", no "VCP", no "I2C", no "EDID": someone meeting this on their first run has
    // no reason to know any of them, and the sentence has to work without them.
    ui.label(
        RichText::new(
            "This computer isn't allowed to talk to your monitors yet. \
             It needs one permission change, once.",
        )
        .heading()
        .color(tokens.ink),
    );
    ui.label(
        RichText::new(
            "These commands load a small kernel module and add a rule that lets whoever is \
             signed in at this desk adjust the monitors — nobody else, and nothing over the \
             network. They need administrator rights, so if this machine is managed by someone \
             else, paste the whole block into a ticket.",
        )
        .small()
        .color(tokens.ink_weak),
    );

    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        // A selectable label rather than a text box: a text box would take keyboard focus and
        // swallow the single-key shortcuts for as long as it held it.
        ui.add(egui::Label::new(RichText::new(PERMISSION_COMMANDS).monospace()).selectable(true));
    });

    ui.horizontal(|ui| {
        let copied = app.copied_recently();
        if ui.button(if copied { "Copied" } else { "Copy these commands" }).clicked() {
            app.copy(PERMISSION_COMMANDS.to_string());
        }
        if ui.button("Try again").clicked() {
            app.restart();
        }
        ui.label(
            RichText::new("Then come back — this window will notice on its own.")
                .small()
                .color(tokens.ink_weak),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Hide").clicked() {
                app.show_permission = false;
            }
        });
    });

    ui.label(
        RichText::new(
            "This writes the same rule as packaging/60-ddcci-i2c.rules. Joining the \"i2c\" \
             group works too, but hands over every I2C bus on the machine rather than just the \
             ones behind your session.",
        )
        .small()
        .color(tokens.ink_weak),
    );
}

/// The line along the bottom.
pub fn status(app: &mut App, ui: &mut egui::Ui) {
    let tokens = Tokens::of(ui.ctx());

    ui.horizontal(|ui| {
        ui.label(reconciliation(app));
        if let Some(note) = app.transient_note() {
            ui.separator();
            ui.label(RichText::new(note).small().color(tokens.ink_weak));
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("?").on_hover_text("Keyboard shortcuts").clicked() {
                app.show_keys = !app.show_keys;
            }

            let mut group = app.group;
            if ui
                .checkbox(&mut group, "Group by monitor")
                .on_hover_text(
                    "One row per physical display, however many cables reach it. Turn this off \
                     when two identical displays report the same identity and are being merged \
                     into one.",
                )
                .changed()
            {
                app.set_grouping(group);
            }

            if ui
                .button("Probe every route")
                .on_hover_text(
                    "Try every cable to every monitor, not just enough to find one that works. \
                     Slower, and the only way to see why a cable is dead.",
                )
                .clicked()
            {
                app.probe_all();
            }
            if ui.button("Refresh").on_hover_text("Re-read every monitor (R)").clicked() {
                app.refresh();
            }
        });
    });
}

/// The sentence that reconciles screens, monitors, and monitors that answer.
///
/// Never a bare count. One panel on two cables makes the number of screens differ from the
/// number of monitors, and a user who spots that without an explanation reasonably concludes
/// the window has lost one.
fn reconciliation(app: &App) -> String {
    let monitors = app.monitors.len();
    if monitors == 0 {
        return "No monitors found".to_string();
    }
    if app.monitors.iter().any(|m| !m.loaded) {
        return format!("{monitors} monitors · reading");
    }

    let answering = app.answering_indices().len();
    let screens = app.screens.len();
    let mut text = String::new();

    if screens != monitors && screens > 0 {
        text.push_str(&format!("{screens} screens · "));
    }
    text.push_str(&format!(
        "{monitors} {} · ",
        if monitors == 1 { "monitor" } else { "monitors" }
    ));
    text.push_str(&if answering == monitors {
        "all answering".to_string()
    } else {
        format!("{answering} answering")
    });

    let doubled = monitors.saturating_sub(app.monitors.iter().filter(|m| m.connectors().len() == 1).count());
    if doubled > 0 && screens > monitors {
        text.push_str(if doubled == 1 {
            " — one panel on two cables"
        } else {
            " — two panels are on two cables"
        });
    }

    let off_desk = (0..monitors).filter(|i| !app.on_desk(*i)).count();
    if off_desk > 0 {
        text.push_str(&format!(" · {off_desk} not in use by the desktop"));
    }

    if answering < monitors {
        let mut kinds = Vec::new();
        let count = |c: Condition| app.monitors.iter().filter(|m| m.condition() == c).count();
        for (condition, word) in [
            (Condition::MenuOff, "menu setting"),
            (Condition::Silent, "not answering"),
            (Condition::NoPermission, "waiting on permission"),
            (Condition::Away, "showing another computer"),
        ] {
            let n = count(condition);
            if n > 0 {
                kinds.push(format!("{n} {word}"));
            }
        }
        if !kinds.is_empty() && !text.contains('—') {
            text.push_str(&format!(" — {}", kinds.join(", ")));
        }
    }

    text
}

/// The keys card, in monospace because the arrows exist only in the monospace font.
pub fn keys(app: &mut App, ctx: &egui::Context) {
    if !app.show_keys {
        return;
    }
    let mut open = true;
    egui::Window::new("Keyboard")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(RichText::new(CARD).monospace());
        });
    if !open {
        app.show_keys = false;
    }
}

/// Every binding, laid out in a font whose arrows actually exist.
const CARD: &str = "\
1 - 9    jump to that channel's fader      ← →      one step
0        jump to the master fader          PgUp/Dn  ten steps
↑ ↓      previous / next channel           Home/End min / max
E        this channel's details            M        hide the desk plan
R        re-read everything                Shift+R  re-check this monitor
L        level every monitor
C        copy this channel's command       Shift+C  copy the master's
Esc      close the window";

//! The master strip and the channel rows.
//!
//! Seven columns, shared by the master and every monitor, so the master's fader occupies
//! exactly the same span of the window as every channel's fader. That alignment is the design:
//! after a week the pointer goes to a place rather than to a control.
//!
//! A monitor that is not answering keeps its row and its columns. Only the fader cell changes —
//! into the cause, the fix, and one button — so nothing ever reflows and the eye keeps its
//! place.

use eframe::egui::{
    self, Align, CornerRadius, Layout, Rect, RichText, Sense, Shape, Stroke, StrokeKind,
    epaint::RectShape, pos2, vec2,
};
use monitor_control::{InputSource, PathSource, PathStatus, code};

use crate::app::{App, STEP};
use crate::backend::{LEVEL_FEATURES, caps_summary};
use crate::names;
use crate::paint;
use crate::state::{Condition, MonitorState};
use crate::theme::{column, space, text, Tokens};

/// Whether a widget's value changed because someone moved it.
///
/// A slider handed a value that is not on one of its steps snaps it and reports the snap
/// through [`egui::Response::changed`], exactly as if a person had done it. These controls are
/// fed values read from displays, so taking `changed` at face value writes a snapped value back
/// to the hardware the moment the window opens.
pub fn moved_by_user(response: &egui::Response) -> bool {
    response.changed()
        && (response.is_pointer_button_down_on() || response.has_focus() || response.drag_stopped())
}

/// The cause of a condition, in the words a person needs.
pub fn cause_line(condition: Condition) -> &'static str {
    match condition {
        // "DDC/CI" appears here and nowhere else in the window: it is the literal label to look
        // for on the monitor's own menu, and a euphemism would send someone hunting.
        Condition::MenuOff => "DDC/CI is switched off in this monitor's own menu.",
        Condition::Silent => "No answer. It may be asleep or showing another computer.",
        Condition::NoPermission => "This computer isn't allowed to talk to this monitor yet.",
        Condition::Away => "Showing another input. Another computer has it now.",
        Condition::Unread => "Reading…",
        Condition::Ready => "",
    }
}

/// The second line: where to look, or how long to wait.
fn fix_line(condition: Condition) -> &'static str {
    match condition {
        Condition::MenuOff => "Look under Others, System or Setup on the monitor itself.",
        Condition::Silent => "Waking one takes a few seconds.",
        Condition::Away => "Control returns when it shows this computer again.",
        Condition::NoPermission | Condition::Unread | Condition::Ready => "",
    }
}

/// The one thing to do about it.
fn action_label(condition: Condition) -> &'static str {
    match condition {
        Condition::MenuOff => "Where to look",
        Condition::NoPermission => "Fix this",
        _ => "Re-check",
    }
}

/// Where a value sits along a slider's rail, in window coordinates.
///
/// egui does not let the handle reach the ends of the rectangle: it shrinks the usable range
/// by the handle's own half-width so the handle stays inside. A mark drawn from the raw
/// rectangle is therefore a couple of points off from the handle it is supposed to line up
/// with, which is exactly the amount that makes a confirmation tick look like a bug.
fn rail_x(rail: Rect, ui: &egui::Ui, fraction: f32) -> f32 {
    let radius = rail.height() / 2.5;
    let inset = match ui.visuals().handle_shape {
        egui::style::HandleShape::Circle => radius,
        egui::style::HandleShape::Rect { aspect_ratio } => radius * aspect_ratio,
    };
    let usable = (rail.width() - 2.0 * inset).max(0.0);
    rail.left() + inset + usable * fraction.clamp(0.0, 1.0)
}

/// Lay out one row of the seven-column grid.
fn cell<R>(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        vec2(width.max(1.0), space::ROW_CONTENT),
        Layout::top_down(Align::LEFT),
        |ui| {
            ui.set_width(width.max(1.0));
            add(ui)
        },
    )
    .inner
}

/// The strip that moves the whole desk.
pub fn master(app: &mut App, ui: &mut egui::Ui) {
    let tokens = Tokens::of(ui.ctx());
    let answering = app.answering_indices();
    let fader = app.fader_width(ui.available_width());

    ui.horizontal(|ui| {
        cell(ui, column::CHANNEL, |ui| {
            ui.add_space(11.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("0").text_style(text::channel()).color(tokens.ink_weak))
            })
            .inner
            .on_hover_text("The master — press 0 to jump here");
        });

        cell(ui, column::MARK, |ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(column::MARK, space::ROW_CONTENT), Sense::hover());
            paint::mark(
                ui.painter(),
                rect,
                Condition::Ready,
                &tokens,
                app.average_percent().unwrap_or(0.0) / 100.0,
            );
        });

        cell(ui, column::NAME, |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("All monitors").text_style(text::name()).color(tokens.ink));
            ui.label(
                RichText::new(if app.average_percent().is_some() {
                    "% of each display's own top · press ? for keys"
                } else {
                    "waiting for the first reading"
                })
                .small()
                .color(tokens.ink_weak),
            );
        });

        cell(ui, fader, |ui| master_fader(app, ui, &tokens, &answering));

        cell(ui, column::READOUT, |ui| {
            ui.add_space(2.0);
            ui.with_layout(Layout::top_down(Align::RIGHT), |ui| match app.average_percent() {
                Some(percent) => {
                    ui.label(
                        RichText::new(format!("{percent:.0}%"))
                            .text_style(text::readout())
                            .color(tokens.level_ink),
                    );
                    let sub = if answering.len() < app.monitors.len() {
                        format!("{} of {}", answering.len(), app.monitors.len())
                    } else {
                        format!("spread {}", app.spread_points().round() as i32)
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(sub).text_style(text::readout_sub()).color(tokens.ink_weak),
                        )
                        .truncate(),
                    )
                    .on_hover_text(format!(
                        "The highest and lowest answering monitor are {} percentage points \
                         apart. Equal percentages are not equal brightness.",
                        app.spread_points().round() as i32
                    ));
                }
                None => {
                    ui.label(
                        RichText::new("—").text_style(text::readout()).color(tokens.ink_weak),
                    );
                }
            });
        });

        cell(ui, column::SWITCH, |ui| {
            ui.add_space(11.0);
            let enabled = app.average_percent().is_some();
            let (rect, response) = ui.allocate_exact_size(vec2(26.0, 22.0), Sense::click());
            let colour = if enabled { tokens.ink } else { tokens.ink_weak };
            let visuals = ui.style().interact(&response);
            ui.painter().rect(
                rect,
                CornerRadius::same(4),
                visuals.bg_fill,
                visuals.bg_stroke,
                StrokeKind::Inside,
            );
            paint::icon_level(ui.painter(), rect.center(), colour);
            let response = response.on_hover_text(format!(
                "Set every monitor to exactly {:.0}% of its own maximum. \
                 This discards the differences between them.",
                app.master_percent.unwrap_or(0.0)
            ));
            if enabled && response.clicked() {
                app.level_all();
            }
        });

        cell(ui, column::TOOLS, |ui| {
            ui.add_space(11.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if ui.button("−").on_hover_text("Step every monitor down 5 points").clicked() {
                    app.nudge_master(-5.0);
                }
                if ui.button("+").on_hover_text("Step every monitor up 5 points").clicked() {
                    app.nudge_master(5.0);
                }
                let percent = app.master_percent.unwrap_or(0.0);
                if ui
                    .button("Copy")
                    .on_hover_text(format!("Copies: {}", names::master_command(percent)))
                    .clicked()
                {
                    app.copy(names::master_command(percent));
                }
            });
        });
    });

}

/// The master's rail, plus one tick per channel showing the spread it preserves.
fn master_fader(app: &mut App, ui: &mut egui::Ui, tokens: &Tokens, answering: &[usize]) {
    ui.add_space(4.0);

    let Some(current) = app.master_percent else {
        // No value has been read, so there is nothing to grab and nothing to write. A rail with
        // no handle cannot be misread as zero.
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), space::RAIL), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(4), ui.visuals().widgets.inactive.bg_fill);
        return;
    };

    let mut percent = current;
    let response = ui.scope(|ui| {
        ui.visuals_mut().selection.bg_fill = tokens.level;
        ui.spacing_mut().slider_width = ui.available_width() - 4.0;
        ui.add(
            egui::Slider::new(&mut percent, 0.0..=100.0)
                .show_value(false)
                .trailing_fill(true)
                .step_by(1.0),
        )
    });
    let rail = response.response.rect;
    let slider = response.inner;

    if slider.drag_started() {
        app.begin_master_drag();
    }
    if moved_by_user(&slider) {
        app.set_master_percent(percent);
    }
    if slider.drag_stopped() {
        app.end_master_drag();
    }
    slider.on_hover_text(
        "A percentage of each display's own maximum, which is not always 100. \
         Equal percentages are not equal brightness.\n\n\
         Moves every monitor by the same amount, keeping the differences between them.",
    );

    // One tick per channel at its own level, labelled with its channel digit. Three panels that
    // do not line up are three ticks that do not line up, visible before anything is touched.
    let painter = ui.painter();
    for &index in answering {
        let Some(percent) = app.percent_of(index) else { continue };
        let x = rail_x(rail, ui, percent / 100.0);
        painter.line_segment(
            [pos2(x, rail.bottom() + 1.0), pos2(x, rail.bottom() + 8.0)],
            Stroke::new(1.5, tokens.ghost),
        );
        painter.text(
            pos2(x, rail.bottom() + 9.0),
            egui::Align2::CENTER_TOP,
            (index + 1).to_string(),
            egui::FontId::new(9.0, egui::FontFamily::Monospace),
            tokens.ink_weak,
        );
    }
}

/// Every channel row, in desk order.
pub fn rows(app: &mut App, ui: &mut egui::Ui) {
    // The bound is re-read every iteration: a control inside a row can restart the backend —
    // "Treat as two" does — and the list is shorter the moment it does.
    let mut index = 0;
    while index < app.monitors.len() {
        row(app, ui, index);
        index += 1;
    }
}

fn row(app: &mut App, ui: &mut egui::Ui, index: usize) {
    let tokens = Tokens::of(ui.ctx());
    let state = app.monitors[index].clone();
    let condition = state.condition();
    let selected = app.selected == Some(index);
    let fader = app.fader_width(ui.available_width());

    // Reserved before the row is drawn so the stripe lands behind its content rather than on
    // top of it.
    let backdrop = ui.painter().add(Shape::Noop);

    let response = ui.horizontal(|ui| {
        cell(ui, column::CHANNEL, |ui| {
            ui.add_space(11.0);
            let response = ui
                .vertical_centered(|ui| {
                    ui.label(
                        RichText::new((index + 1).to_string())
                            .text_style(text::channel())
                            .color(if selected { tokens.ink } else { tokens.ink_weak }),
                    )
                })
                .inner
                .on_hover_text(format!("Channel {} — press {} to jump here", index + 1, index + 1));
            if response.interact(Sense::click()).clicked() {
                app.select(index);
            }
        });

        cell(ui, column::MARK, |ui| {
            let (rect, _) =
                ui.allocate_exact_size(vec2(column::MARK, space::ROW_CONTENT), Sense::hover());
            let fill = app
                .displayed(index, code::BRIGHTNESS)
                .map(|v| f32::from(v) / f32::from(app.maximum(index, code::BRIGHTNESS)).max(1.0))
                .unwrap_or(0.0);
            paint::mark(ui.painter(), rect, condition, &tokens, fill);
        });

        cell(ui, column::NAME, |ui| {
            ui.add_space(4.0);
            let response = ui.add(
                egui::Label::new(
                    RichText::new(state.name()).text_style(text::name()).color(tokens.ink),
                )
                .truncate()
                .sense(Sense::click()),
            );
            if response.clicked() {
                app.select(index);
            }
            ui.add(
                egui::Label::new(RichText::new(sub_line(app, &state, index)).small().color(tokens.ink_weak))
                    .truncate(),
            );
        });

        cell(ui, fader, |ui| {
            if condition == Condition::Ready {
                brightness_fader(app, ui, index, &tokens);
            } else {
                treated_rail(app, ui, index, condition, &tokens);
            }
        });

        cell(ui, column::READOUT, |ui| readout(app, ui, index, condition, &tokens));

        cell(ui, column::SWITCH, |ui| {
            if condition == Condition::Ready && state.supports_input() {
                input_chip(app, ui, index, &state, &tokens);
            }
        });

        cell(ui, column::TOOLS, |ui| tools(app, ui, index, condition, &tokens));
    });

    let rect = response.response.rect;
    stripe(ui, backdrop, rect, index % 2 == 1, selected, &tokens);
    if state.busy {
        paint::busy_sweep(ui.painter(), rect, app.animation_phase(ui.ctx()), tokens.level);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
    }
    if app.scroll_to_selected && selected {
        app.scroll_to_selected = false;
        ui.scroll_to_rect(rect, Some(Align::Center));
    }

    if app.expanded == Some(index) {
        details(app, ui, index, &tokens);
    }
    ui.separator();
}

/// The line under a display's name.
fn sub_line(app: &App, state: &MonitorState, index: usize) -> String {
    let mut parts = vec![state.connectors().join(" + ")];
    if let Some(inches) = state.diagonal_inches() {
        parts.push(format!("{inches:.0} in"));
    }
    if let Some(screen) = app.screens.iter().find(|s| s.monitor == Some(index)) {
        parts.push(screen.mode.clone());
    } else {
        parts.push("not in use by the desktop".into());
    }
    parts.join(" · ")
}

/// A live brightness fader, with the fixed reference ticks and the confirmation tick.
fn brightness_fader(app: &mut App, ui: &mut egui::Ui, index: usize, tokens: &Tokens) {
    ui.add_space(10.0);
    let maximum = app.maximum(index, code::BRIGHTNESS);
    let Some(current) = app.displayed(index, code::BRIGHTNESS) else {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), space::RAIL), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(4), ui.visuals().widgets.inactive.bg_fill);
        return;
    };

    let mut value = current;
    let scope = ui.scope(|ui| {
        ui.visuals_mut().selection.bg_fill = tokens.level;
        ui.spacing_mut().slider_width = ui.available_width() - 4.0;
        ui.add(
            egui::Slider::new(&mut value, 0..=maximum)
                .show_value(false)
                .trailing_fill(true),
        )
    });
    let rail = scope.response.rect;
    let slider = scope.inner;

    if moved_by_user(&slider) {
        app.set_level(index, code::BRIGHTNESS, value);
    }
    let slider = slider.on_hover_text(
        "Drag, scroll, or use the arrow keys. One step is one unit of this display's own scale.",
    );

    // Fixed reference points, so muscle memory has something to land on.
    let painter = ui.painter();
    for fraction in [0.25, 0.5, 0.75] {
        let x = rail_x(rail, ui, fraction);
        painter.line_segment(
            [pos2(x, rail.bottom()), pos2(x, rail.bottom() + 4.0)],
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        );
    }

    // Where the panel actually is, while it catches up with where the pointer put the handle.
    if let Some(confirmed) = app.confirmed(index, code::BRIGHTNESS)
        && confirmed.abs_diff(current) >= 1
    {
        let x = rail_x(rail, ui, f32::from(confirmed) / f32::from(maximum).max(1.0));
        painter.line_segment(
            [pos2(x, rail.top() - 3.0), pos2(x, rail.bottom() + 3.0)],
            Stroke::new(2.0, tokens.ghost),
        );
    }

    if app.take_focus_request(index) {
        slider.request_focus();
    }
    if slider.hovered() {
        app.wheel_over_fader(ui, index);
    }
}

/// The cause, the fix, the treated rail, and one button.
fn treated_rail(
    app: &mut App,
    ui: &mut egui::Ui,
    index: usize,
    condition: Condition,
    tokens: &Tokens,
) {
    let colour = paint::condition_colour(condition, tokens);

    ui.add_space(2.0);
    ui.vertical(|ui| {
        // The cause takes the whole row bar the button, so a ten-word sentence is never cut in
        // half — the whole point of the line is that it can be read without expanding anything.
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(action_label(condition)).clicked() {
                    match condition {
                        Condition::NoPermission => app.show_permission = true,
                        Condition::MenuOff => app.expanded = Some(index),
                        _ => app.refresh_one(index),
                    }
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let cause = if condition == Condition::Away {
                        away_line(&app.monitors[index])
                    } else {
                        cause_line(condition).to_string()
                    };
                    ui.add(egui::Label::new(RichText::new(cause.clone()).color(colour)).truncate())
                        .on_hover_text(cause);
                });
            });
        });

        let fix = fix_line(condition);
        if !fix.is_empty() {
            ui.add(egui::Label::new(RichText::new(fix).small().color(tokens.ink_weak)).truncate());
        }

        let (rail, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), space::RAIL), Sense::hover());
        let painter = ui.painter();
        match condition {
            Condition::MenuOff => {
                painter.rect_filled(rail, CornerRadius::same(4), ui.visuals().widgets.inactive.bg_fill);
                paint::hatch(painter, rail, colour, 9.0);
            }
            Condition::Silent | Condition::Unread => {
                paint::dashed_rect(painter, rail, Stroke::new(1.2, colour));
            }
            Condition::NoPermission => {
                painter.rect_filled(rail, CornerRadius::same(4), ui.visuals().widgets.inactive.bg_fill);
                paint::padlock(
                    painter,
                    pos2(rail.left() + 12.0, rail.center().y),
                    colour,
                    ui.visuals().widgets.inactive.bg_fill,
                );
            }
            Condition::Away => {
                painter.rect_filled(rail, CornerRadius::same(4), ui.visuals().widgets.inactive.bg_fill);
                painter.line_segment(
                    [
                        pos2(rail.left() + 4.0, rail.center().y),
                        pos2(rail.right() - 4.0, rail.center().y),
                    ],
                    Stroke::new(1.5, colour.gamma_multiply(0.4)),
                );
            }
            Condition::Ready => {}
        }
    });
}

/// What an away monitor is showing, named rather than numbered.
fn away_line(state: &MonitorState) -> String {
    match state.input.requested.or(state.input.current) {
        Some(input) => format!("Showing {}. Another computer has it now.", names::input_name(input)),
        None => "Showing another input. Another computer has it now.".to_string(),
    }
}

/// The percentage, and the display's own units beneath it.
fn readout(app: &mut App, ui: &mut egui::Ui, index: usize, condition: Condition, tokens: &Tokens) {
    ui.add_space(2.0);
    if condition != Condition::Ready {
        let (rect, _) =
            ui.allocate_exact_size(vec2(column::READOUT, space::ROW_CONTENT - 4.0), Sense::hover());
        if condition == Condition::Unread {
            ui.painter().text(
                rect.right_center(),
                egui::Align2::RIGHT_CENTER,
                "—",
                egui::FontId::new(22.0, egui::FontFamily::Monospace),
                tokens.ink_weak,
            );
        } else {
            paint::badge(ui.painter(), rect.center(), condition, tokens);
        }
        return;
    }

    let maximum = app.maximum(index, code::BRIGHTNESS);
    let Some(current) = app.displayed(index, code::BRIGHTNESS) else { return };
    let failed = app.failure(index, code::BRIGHTNESS);
    let colour = if failed.is_some() { tokens.fail } else { tokens.level_ink };

    ui.with_layout(Layout::top_down(Align::RIGHT), |ui| {
        let mut percent = f64::from(current) * 100.0 / f64::from(maximum).max(1.0);
        let response = ui.scope(|ui| {
            ui.style_mut()
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::new(22.0, egui::FontFamily::Monospace));
            ui.visuals_mut().override_text_color = Some(colour);
            ui.add(
                egui::DragValue::new(&mut percent)
                    .speed(0.4)
                    .range(0.0..=100.0)
                    .custom_formatter(|v, _| format!("{v:.0}%"))
                    .custom_parser(|s| s.trim().trim_end_matches('%').trim().parse::<f64>().ok()),
            )
        });
        if moved_by_user(&response.inner) {
            let target = (percent / 100.0 * f64::from(maximum)).round() as u16;
            app.set_level(index, code::BRIGHTNESS, target);
        }
        response.inner.on_hover_text(format!(
            "{:.0}% — {current} of {maximum}. Click to type an exact value.",
            f64::from(current) * 100.0 / f64::from(maximum).max(1.0)
        ));

        let sub = match &failed {
            Some(_) => "not confirmed".to_string(),
            None => format!("{current} / {maximum}"),
        };
        let label = ui.label(
            RichText::new(sub)
                .text_style(text::readout_sub())
                .color(if failed.is_some() { tokens.fail } else { tokens.ink_weak }),
        );
        if let Some(message) = failed {
            label.on_hover_text(message);
        }
    });
}

/// The input picker, built from what the display says it accepts.
fn input_chip(
    app: &mut App,
    ui: &mut egui::Ui,
    index: usize,
    state: &MonitorState,
    tokens: &Tokens,
) {
    ui.add_space(11.0);
    let choices = state.input_choices();
    let current = state.input.current;
    let pending = state.input.requested;
    let enumerated = state.enumerated_inputs();
    let mut chosen: Option<InputSource> = None;

    // A guillemet rather than an arrow: the arrow characters are in Hack but not in
    // Ubuntu-Light, and this chip is drawn in the proportional family, so an arrow here comes
    // out as an empty box.
    let label = match (pending, current) {
        (Some(input), _) => format!("» {}", names::input_chip(input)),
        (None, Some(input)) => names::input_chip(input),
        (None, None) => "input".to_string(),
    };

    // Keyed by identity: an open list must stay on the monitor it was opened for, even if a
    // hotplug renumbers the rows underneath it.
    let combo = egui::ComboBox::from_id_salt(("input", &state.id.key, &state.id.connector))
        .width(46.0)
        .selected_text(RichText::new(label).small().color(tokens.ink_weak))
        .show_ui(ui, |ui| {
            for choice in &choices {
                let selected = current == Some(*choice);
                if ui
                    .selectable_label(
                        selected,
                        format!("{}   {:#04x}", names::input_name(*choice), choice.value()),
                    )
                    .clicked()
                {
                    chosen = Some(*choice);
                }
            }
            if !enumerated {
                ui.separator();
                ui.label(
                    RichText::new(
                        "This display wouldn't list its inputs, so every standard one is \
                         offered. Ones it doesn't have are ignored.",
                    )
                    .small()
                    .color(tokens.ink_weak),
                );
            }
        });
    combo.response.on_hover_text(match (pending, current) {
        (Some(input), _) => format!("Switching to {}…", names::input_name(input)),
        (None, Some(input)) => format!("Showing {} — click to switch", names::input_name(input)),
        (None, None) => "Switch this display's input".to_string(),
    });

    if let Some(input) = chosen {
        app.set_input(index, input);
    }
}

/// The row's four buttons.
fn tools(app: &mut App, ui: &mut egui::Ui, index: usize, condition: Condition, tokens: &Tokens) {
    ui.add_space(11.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let live = condition == Condition::Ready;
        if live {
            if ui.button("−").on_hover_text("Step by 5").clicked() {
                app.step_level(index, code::BRIGHTNESS, -STEP);
            }
            if ui.button("+").on_hover_text("Step by 5").clicked() {
                app.step_level(index, code::BRIGHTNESS, STEP);
            }
        } else {
            // Exactly the width the two step buttons and their gap would have taken, so the
            // icon buttons stay in the same column whatever state the row is in.
            ui.add_space(56.0);
        }

        let colour = if condition == Condition::Ready {
            tokens.ink
        } else {
            paint::condition_colour(condition, tokens)
        };
        if icon_button(ui, colour, paint::icon_recheck)
            .on_hover_text("Re-check this monitor (Shift+R)")
            .clicked()
        {
            app.refresh_one(index);
        }
        let open = app.expanded == Some(index);
        if icon_button(ui, tokens.ink, move |p, c, colour| paint::icon_chevron(p, c, colour, open))
        .on_hover_text("Contrast, volume, routes, copy command (E)")
        .clicked()
        {
            app.expanded = if open { None } else { Some(index) };
        }
    });
}

/// A 26 by 22 button with a painted icon rather than a character.
fn icon_button(
    ui: &mut egui::Ui,
    colour: egui::Color32,
    draw: impl Fn(&egui::Painter, egui::Pos2, egui::Color32),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(26.0, 22.0), Sense::click());
    let visuals = ui.style().interact(&response);
    ui.painter().rect(
        rect,
        CornerRadius::same(4),
        visuals.bg_fill,
        visuals.bg_stroke,
        StrokeKind::Inside,
    );
    draw(ui.painter(), rect.center(), colour);
    response
}

/// Paint a row's stripe into the slot reserved before its content was drawn.
fn stripe(
    ui: &egui::Ui,
    backdrop: egui::layers::ShapeIdx,
    rect: Rect,
    striped: bool,
    selected: bool,
    tokens: &Tokens,
) {
    let rect = Rect::from_min_max(
        pos2(ui.max_rect().left(), rect.top() - space::ROW),
        pos2(ui.max_rect().right(), rect.bottom() + space::ROW),
    );
    if striped {
        ui.painter().set(
            backdrop,
            RectShape::filled(rect, CornerRadius::ZERO, ui.visuals().faint_bg_color),
        );
    }
    if selected {
        // Selection is a bar, never a fill and never a dash, so it can never be mistaken for
        // one of the state treatments.
        ui.painter().rect_filled(
            Rect::from_min_max(rect.left_top(), pos2(rect.left() + 3.0, rect.bottom())),
            CornerRadius::ZERO,
            tokens.level,
        );
    }
}

/// Contrast, volume, routes, the stable key, and the copyable command.
fn details(app: &mut App, ui: &mut egui::Ui, index: usize, tokens: &Tokens) {
    let state = app.monitors[index].clone();
    let unique = app.key_is_unique(index);

    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());

        for (feature, label) in LEVEL_FEATURES {
            if feature == code::BRIGHTNESS || !state.features.contains_key(&feature) {
                continue;
            }
            let maximum = app.maximum(index, feature);
            let Some(current) = app.displayed(index, feature) else { continue };
            ui.horizontal(|ui| {
                let mut value = current;
                let response = ui.scope(|ui| {
                    ui.visuals_mut().selection.bg_fill = tokens.level;
                    ui.spacing_mut().slider_width = (ui.available_width() - 220.0).max(120.0);
                    ui.add(egui::Slider::new(&mut value, 0..=maximum).show_value(false).trailing_fill(true))
                });
                if moved_by_user(&response.inner) {
                    app.set_level(index, feature, value);
                }
                ui.label(RichText::new(format!("{current} / {maximum}")).text_style(text::readout_sub()));
                ui.label(label);
                if ui.button("Copy").on_hover_text("Copy the equivalent command").clicked() {
                    let command = if feature == code::CONTRAST {
                        names::contrast_command(&state.id, current, unique)
                    } else {
                        names::volume_command(&state.id, current, unique)
                    };
                    app.copy(command);
                }
                if let Some(message) = app.failure(index, feature) {
                    ui.label(RichText::new(format!("didn't take: {message}")).small().color(tokens.fail));
                    if ui.button("Try again").clicked() {
                        app.set_level(index, feature, current);
                    }
                }
            });
        }

        if let Some(input) = state.input.current {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Input: {}", names::input_name(input))).small());
                let command = names::input_command(&state.id, input, unique);
                if ui.button("Copy").on_hover_text(command.clone()).clicked() {
                    app.copy(command);
                }
            });
        }

        routes(ui, &state, tokens);

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("key {}", state.id.key)).monospace().color(tokens.ink_weak))
                .on_hover_text(
                    "Stable across reboots and replugging. This is what monitorctl --key takes.",
                );
            let percent = app.percent_of(index).unwrap_or(0.0);
            let command = names::brightness_command(&state.id, percent, unique);
            if ui
                .button("Copy as command")
                .on_hover_text(if unique {
                    format!(
                        "Paste this into your compositor's keybindings to skip opening this \
                         window.\n\n{command}"
                    )
                } else {
                    format!(
                        "This display shares its identity with another, so the command names \
                         the cable too.\n\n{command}"
                    )
                })
                .clicked()
            {
                app.copy(command);
            }
        });

        if state.ambiguous_grouping {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(
                        "Two cables report the same identity with no serial number. This may be \
                         one display plugged in twice, or two identical displays.",
                    )
                    .small()
                    .color(tokens.menu),
                );
                if ui.button("Treat as two").clicked() {
                    app.set_grouping(false);
                }
            });
        }

        if let Some(error) = &state.caps_error {
            ui.label(
                RichText::new(format!(
                    "This display wouldn't describe itself. Everything else about it works \
                     normally. ({error})"
                ))
                .small()
                .color(tokens.ink_weak),
            );
        } else if let Some(caps) = &state.caps {
            ui.label(RichText::new(caps_summary(caps)).small().color(tokens.ink_weak));
        }
    });
}

/// Every route to this monitor and what the last probe found on it.
fn routes(ui: &mut egui::Ui, state: &MonitorState, tokens: &Tokens) {
    let working = state.paths.iter().filter(|p| p.status.is_working()).count();
    let header = if working > 0 {
        format!("{} routes, {working} works", state.paths.len())
    } else {
        format!("{} routes, none working", state.paths.len())
    };

    egui::CollapsingHeader::new(header)
        .id_salt(("routes", &state.id.key, &state.id.connector))
        .show(ui, |ui| {
            egui::Grid::new(("routes-grid", &state.id.key)).num_columns(4).spacing([12.0, 4.0]).show(
                ui,
                |ui| {
                    for (i, path) in state.paths.iter().enumerate() {
                        let active = state.active_path == Some(i);
                        ui.label(
                            RichText::new(format!("{} {}", if active { ">" } else { " " }, path.describe()))
                                .monospace(),
                        );
                        ui.label(
                            RichText::new(match path.source {
                                PathSource::DpAux => "DisplayPort AUX",
                                PathSource::DdcLink => "DDC line",
                            })
                            .small()
                            .color(tokens.ink_weak),
                        );
                        let (word, colour, detail) = match &path.status {
                            PathStatus::Working { vcp_version } => (
                                "working",
                                tokens.ok,
                                vcp_version.map(|(a, b)| format!("MCCS {a}.{b}")),
                            ),
                            PathStatus::DdcDisabled => (
                                "menu off",
                                tokens.menu,
                                Some("The display answered and declined.".into()),
                            ),
                            PathStatus::Unavailable { reason } => {
                                ("no access", tokens.locked, Some(reason.clone()))
                            }
                            PathStatus::Unresponsive => ("no answer", tokens.silent, None),
                            PathStatus::Unprobed => ("not tried", tokens.ink_weak, None),
                        };
                        let label = ui.label(RichText::new(word).small().color(colour));
                        if let Some(detail) = detail {
                            label.on_hover_text(detail);
                        }
                        ui.label("");
                        ui.end_row();
                    }
                },
            );
            ui.label(
                RichText::new(
                    "The marked route is the one commands go over. A dead route beside a working \
                     one is normal — a DisplayPort connector exposes two buses and only one \
                     carries control.",
                )
                .small()
                .color(tokens.ink_weak),
            );
        });
}

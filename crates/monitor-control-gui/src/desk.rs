//! The desk plan: every screen drawn where the desktop puts it.
//!
//! Demoted from a control surface to a scribble strip. Its two jobs are to prove which channel
//! number is which physical panel, and to show relative level at a glance across the whole
//! desk — something a column of faders cannot do. It keeps the wheel gesture, because trimming
//! one monitor by eye wants the feedback under the pointer, not in a list below.
//!
//! It is a map and never an editor. Dragging a rectangle sideways does nothing: two programs
//! writing the desktop's layout is how you get a fight with the desktop's own settings.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontFamily, FontId, Rect, Sense, Stroke, StrokeKind,
    Vec2, pos2, vec2,
};
use monitor_control::code;

use crate::app::{App, DeskDrag, SCROLL_POINTS_PER_STEP, STEP};
use crate::paint;
use crate::state::Condition;
use crate::theme::{self, Tokens};

/// One rectangle on the plan.
pub struct Screen {
    /// Top-left corner in the desktop's coordinate space.
    pub position: (i32, i32),
    /// Size in the same space.
    pub size: (u32, u32),
    /// The connectors this rectangle covers, joined for display.
    pub connectors: String,
    /// The monitor behind it, as an index into the window's monitor list.
    pub monitor: Option<usize>,
    /// Any further monitors showing the same thing in the same place.
    ///
    /// Mirroring puts two different panels at one position. They are one rectangle, because
    /// drawing them stacked would hide all but the last, but both are still attached to the
    /// desktop and neither should be reported as unused.
    pub mirrored: Vec<usize>,
    /// Whether the desktop treats this as the primary screen.
    pub primary: bool,
    /// A description of the mode.
    pub mode: String,
    /// Whether this monitor also appears on another screen — one panel, two cables.
    pub linked: bool,
}

/// How far a pointer must move before a press counts as a drag rather than a click.
///
/// egui's own click threshold is six points, and a touchpad drifts by about that much during a
/// deliberate click. Since a mis-read click here would black out a panel and there is no undo,
/// the dead zone matches it rather than trying to be clever.
const DRAG_DEAD_ZONE: f32 = 6.0;

/// Draw the plan and act on what the pointer does to it.
pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let tokens = Tokens::of(ui.ctx());

    ui.horizontal(|ui| {
        if !app.layout.is_arrangement_known() {
            ui.label(
                egui::RichText::new(
                    "No display server answered, so the desk plan is unknown. \
                     Channels are in connector order.",
                )
                .small()
                .color(tokens.menu),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("plan from {}", app.layout.source.label()))
                    .small()
                    .color(tokens.ink_weak),
            );
        });
    });

    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::hover());
    let canvas = response.rect;
    if canvas.width() < 20.0 || canvas.height() < 20.0 {
        return;
    }

    // A canvas that is visibly not a map, when there is no map to draw. The alternative — an
    // invented left-to-right row on the usual background — is a plausible lie.
    let known = app.layout.is_arrangement_known();
    painter.rect_filled(
        canvas,
        CornerRadius::same(8),
        if known { tokens.desk_bg } else { ui.visuals().panel_fill },
    );
    if !known {
        painter.rect_stroke(
            canvas,
            CornerRadius::same(8),
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            StrokeKind::Inside,
        );
    }

    if app.screens.is_empty() {
        painter.text(
            canvas.center(),
            Align2::CENTER_CENTER,
            "no screens reported by the desktop",
            FontId::new(12.5, FontFamily::Proportional),
            tokens.ink_weak,
        );
        return;
    }

    let Some((left, top, right, bottom)) = bounds(&app.screens) else { return };
    let span = Vec2::new((right - left).max(1) as f32, (bottom - top).max(1) as f32);
    let inner = canvas.shrink(12.0);
    // One scale for both axes: a screen's proportions on the plan are its proportions on the
    // desk, which is what makes a portrait monitor recognisable at a glance.
    let scale = (inner.width() / span.x).min(inner.height() / span.y);
    let drawn = span * scale;
    let origin = inner.min + ((inner.size() - drawn) / 2.0);

    // Read once: a nested `ui.input` re-enters the same lock.
    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
    let pointer = ui.input(|i| i.pointer.interact_pos());

    let mut clicked = None;
    let mut hovered_screen: Option<String> = None;
    let mut wheel_targets: Vec<usize> = Vec::new();
    let mut drag_start: Option<(usize, f32, f32)> = None;
    let mut sideways = false;

    for index in 0..app.screens.len() {
        let screen = &app.screens[index];
        let rect = Rect::from_min_size(
            origin
                + vec2(
                    (screen.position.0 - left) as f32 * scale,
                    (screen.position.1 - top) as f32 * scale,
                ),
            vec2(screen.size.0 as f32 * scale, screen.size.1 as f32 * scale),
        )
        // A gap, so two touching screens read as two rectangles rather than one wide one.
        .shrink(3.0);
        if rect.width() < 4.0 || rect.height() < 4.0 {
            continue;
        }

        // Keyed by connector rather than by position in the list, so hover and drag stay on the
        // same screen when the list is rebuilt after a rearrangement.
        let id = ui.id().with(("screen", screen.connectors.as_str()));
        let response = ui.interact(rect, id, Sense::click_and_drag());
        let controllable = screen
            .monitor
            .and_then(|m| app.monitors.get(m))
            .is_some_and(|m| m.condition() == Condition::Ready);

        if response.hovered() {
            hovered_screen = Some(screen.connectors.clone());
            if controllable {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            }
            if scroll.abs() > 0.01
                && let Some(monitor) = screen.monitor
                && controllable
            {
                wheel_targets.push(monitor);
            }
        }

        if response.drag_started()
            && let Some(monitor) = screen.monitor
            && controllable
            && let Some(p) = pointer
        {
            drag_start = Some((monitor, p.y, rect.height()));
        }
        if response.dragged() && app.desk_drag.is_none() && response.drag_delta().x.abs() > 4.0 {
            sideways = true;
        }
        if response.clicked()
            && let Some(monitor) = screen.monitor
        {
            clicked = Some(monitor);
        }

        paint_screen(app, ui, &painter, index, rect, response.hovered(), &tokens);

        let tip = tooltip_text(app, index);
        response.on_hover_text(tip);
    }

    if sideways {
        app.set_read_only_note();
    }

    if let Some(monitor) = clicked {
        app.select(monitor);
    }
    if let Some((monitor, y, height)) = drag_start {
        let anchor = app.displayed(monitor, code::BRIGHTNESS).unwrap_or(0);
        app.desk_drag = Some(DeskDrag {
            monitor,
            anchor_value: anchor,
            anchor_y: y,
            span: height.max(150.0),
        });
    }
    continue_drag(app, ui, pointer);
    apply_wheel(app, hovered_screen, scroll, &wheel_targets);
}

/// Track an in-progress vertical drag on a screen.
///
/// Anchored to where the pointer went down rather than jumping to it: the same rectangle is
/// also the click target, so an absolute drag would let a mis-aimed press slam a panel to
/// whatever height it landed at. It also makes the value monotonic under the pointer by
/// construction, instead of by hoping the reads come back in order.
fn continue_drag(app: &mut App, ui: &egui::Ui, pointer: Option<egui::Pos2>) {
    let Some(drag) = app.desk_drag else { return };

    if !ui.input(|i| i.pointer.any_down()) {
        app.desk_drag = None;
        return;
    }
    let Some(p) = pointer else { return };
    let travel = drag.anchor_y - p.y;
    if travel.abs() < DRAG_DEAD_ZONE {
        return;
    }

    let maximum = f32::from(app.maximum(drag.monitor, code::BRIGHTNESS));
    let target = f32::from(drag.anchor_value) + travel / drag.span * maximum;
    app.set_level(drag.monitor, code::BRIGHTNESS, target.clamp(0.0, maximum).round() as u16);
}

/// Turn accumulated wheel movement over one screen into brightness steps.
fn apply_wheel(app: &mut App, over: Option<String>, delta: f32, targets: &[usize]) {
    if over.is_none() || targets.is_empty() {
        // The pointer left every screen, so a half-finished step must not land on whichever
        // one it wanders onto next.
        app.scroll_accumulated = 0.0;
        app.scroll_over = None;
        return;
    }
    if app.scroll_over != over {
        app.scroll_over = over;
        app.scroll_accumulated = 0.0;
    }

    app.scroll_accumulated += delta;
    while app.scroll_accumulated.abs() >= SCROLL_POINTS_PER_STEP {
        let direction = app.scroll_accumulated.signum();
        app.scroll_accumulated -= direction * SCROLL_POINTS_PER_STEP;
        for &monitor in targets {
            app.step_level(monitor, code::BRIGHTNESS, direction as i32 * STEP);
        }
    }
}

/// The bounding box of every screen, in desktop coordinates.
fn bounds(screens: &[Screen]) -> Option<(i32, i32, i32, i32)> {
    screens
        .iter()
        .map(|s| {
            (
                s.position.0,
                s.position.1,
                s.position.0 + s.size.0 as i32,
                s.position.1 + s.size.1 as i32,
            )
        })
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
}

/// Draw one screen.
fn paint_screen(
    app: &App,
    ui: &egui::Ui,
    painter: &egui::Painter,
    index: usize,
    rect: Rect,
    hovered: bool,
    tokens: &Tokens,
) {
    let screen = &app.screens[index];
    let state = screen.monitor.and_then(|m| app.monitors.get(m));
    let condition = state.map_or(Condition::Unread, |s| s.condition());
    let selected = screen.monitor.is_some() && screen.monitor == app.selected;

    let lit = matches!(condition, Condition::Ready);
    let base = if lit { tokens.tile_bg } else { tokens.tile_dim };
    painter.rect_filled(rect, CornerRadius::same(6), base);

    // Where the level line falls, so text below it can be drawn in ink that survives the fill.
    let mut level_y = rect.bottom();

    match condition {
        Condition::Ready => {
            if let Some(monitor) = screen.monitor
                && let Some(value) = app.displayed(monitor, code::BRIGHTNESS)
            {
                let maximum = f32::from(app.maximum(monitor, code::BRIGHTNESS)).max(1.0);
                let fraction = (f32::from(value) / maximum).clamp(0.0, 1.0);
                let stale = app.is_stale(monitor, code::BRIGHTNESS);
                if fraction > 0.0 {
                    level_y = rect.bottom() - rect.height() * fraction;
                    painter.rect_filled(
                        Rect::from_min_max(pos2(rect.left(), level_y), rect.max),
                        // Square at the top: that edge is the reading, and rounding it would
                        // read as the shape of the screen instead.
                        CornerRadius { nw: 0, ne: 0, sw: 6, se: 6 },
                        tokens.level.gamma_multiply(if stale { 0.09 } else { 0.16 + 0.22 * fraction }),
                    );
                    if fraction < 0.995 {
                        let stroke = Stroke::new(1.5, tokens.level);
                        let segment = [pos2(rect.left(), level_y), pos2(rect.right(), level_y)];
                        if stale {
                            // A value that failed to confirm is still shown, but never as fact.
                            painter.extend(egui::Shape::dashed_line(&segment, stroke, 5.0, 4.0));
                        } else {
                            painter.line_segment(segment, stroke);
                        }
                    }
                }
            }
        }
        Condition::MenuOff => paint::hatch(painter, rect, tokens.menu.gamma_multiply(0.5), 9.0),
        Condition::NoPermission => {
            paint::padlock(painter, rect.center(), tokens.locked, tokens.tile_dim);
        }
        Condition::Away | Condition::Silent | Condition::Unread => {}
    }

    let stroke = if selected {
        Stroke::new(2.0, ui.visuals().selection.bg_fill)
    } else if hovered {
        Stroke::new(1.5, ui.visuals().widgets.hovered.fg_stroke.color)
    } else {
        Stroke::new(1.0, ui.visuals().widgets.inactive.fg_stroke.color)
    };
    if matches!(condition, Condition::Silent | Condition::Away | Condition::MenuOff) {
        paint::dashed_rect(painter, rect, stroke);
    } else {
        painter.rect_stroke(rect, CornerRadius::same(6), stroke, StrokeKind::Inside);
    }

    // Text is inset on both sides so it clears the channel disc while staying centred.
    let text_width = rect.width() - 52.0;
    let centre = rect.center().x;
    let mut cursor = rect.top() + 8.0;

    // How much text there is to make room for, before any of it is drawn.
    let lines = usize::from(rect.height() > 34.0 && text_width > 48.0)
        + usize::from(rect.height() > 58.0 && text_width > 60.0)
        + usize::from(rect.height() > 84.0 && text_width > 84.0);
    // Whether the labels sit above the level, below it, or across it.
    let plate = Rect::from_min_max(
        pos2(rect.left() + 2.0, rect.top() + 3.0),
        pos2(rect.right() - 2.0, cursor + 18.0 * lines as f32 - 2.0),
    );
    let straddles = lines > 0 && level_y > plate.top() && level_y < plate.bottom();
    if straddles {
        // Only when the level boundary actually cuts through the labels. A plate drawn every
        // time would hide the fill in the top two thirds of the screen, and reading four
        // levels across the desk at a glance is what the fill is for. Translucent, so the fill
        // still reads through it — the plate settles the ground under the text without
        // pretending the panel is dimmer than it is.
        painter.rect_filled(
            plate,
            CornerRadius::same(4),
            Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 150),
        );
    }
    let on_fill = !straddles && lines > 0 && level_y <= plate.top();
    let ink = |_bottom: f32, weak: bool| match (on_fill, weak) {
        // Over a lit panel the numeral's ink is the one that was measured to hold up.
        (true, _) => tokens.level_ink,
        (false, true) => tokens.ink_weak,
        (false, false) => tokens.ink,
    };

    if rect.height() > 34.0 && text_width > 48.0 {
        let name = state.map(|s| s.name()).unwrap_or_else(|| "Unknown".into());
        painter.text(
            pos2(centre, cursor),
            Align2::CENTER_TOP,
            elide(&name, text_width, 13.0),
            FontId::new(13.0, FontFamily::Proportional),
            ink(cursor + 17.0, false),
        );
        cursor += 18.0;
    }
    if rect.height() > 58.0 && text_width > 60.0 {
        let mut line = screen.connectors.clone();
        if let Some(inches) = state.and_then(|s| s.diagonal_inches()) {
            line.push_str(&format!(" · {inches:.0} in"));
        }
        painter.text(
            pos2(centre, cursor),
            Align2::CENTER_TOP,
            elide(&line, text_width, 10.5),
            FontId::new(10.5, FontFamily::Proportional),
            ink(cursor + 14.0, true),
        );
        cursor += 15.0;
    }
    if rect.height() > 84.0 && text_width > 84.0 {
        painter.text(
            pos2(centre, cursor),
            Align2::CENTER_TOP,
            elide(&screen.mode, text_width, 10.0),
            FontId::new(10.0, FontFamily::Proportional),
            ink(cursor + 14.0, true),
        );
    }

    // The headline number, or the badge that stands in for it.
    if let Some(font) = theme::text::tile_numeral(rect.height()) {
        match condition {
            Condition::Ready => {
                if let Some(monitor) = screen.monitor
                    && let Some(value) = app.displayed(monitor, code::BRIGHTNESS)
                {
                    let maximum = f32::from(app.maximum(monitor, code::BRIGHTNESS)).max(1.0);
                    painter.text(
                        pos2(centre, rect.bottom() - 6.0),
                        Align2::CENTER_BOTTOM,
                        format!("{:.0}%", f32::from(value) * 100.0 / maximum),
                        font,
                        tokens.level_ink,
                    );
                }
            }
            Condition::Unread => {
                painter.text(
                    pos2(centre, rect.bottom() - 8.0),
                    Align2::CENTER_BOTTOM,
                    "—",
                    FontId::new(14.0, FontFamily::Proportional),
                    tokens.ink_weak,
                );
            }
            other => paint::badge(painter, pos2(centre, rect.bottom() - 14.0), other, tokens),
        }
    }

    if let Some(monitor) = screen.monitor
        && rect.width() > 34.0
        && rect.height() > 34.0
    {
        paint::channel_disc(painter, ui, rect.min + vec2(11.0, 11.0), monitor + 1, tokens);
    }
    if screen.linked && rect.width() > 40.0 {
        // Amber rather than grey when the grouping was a guess, because then the two
        // rectangles might be two displays rather than one.
        let ambiguous = state.is_some_and(|s| s.ambiguous_grouping);
        paint::link_badge(
            painter,
            rect.right_top() + vec2(-9.0, 9.0),
            if ambiguous { tokens.menu } else { tokens.ink_weak },
        );
    }
    if screen.primary && rect.width() > 30.0 {
        painter.rect_filled(
            Rect::from_min_max(
                pos2(rect.left() + 6.0, rect.bottom() - 3.0),
                pos2(rect.right() - 6.0, rect.bottom()),
            ),
            CornerRadius::ZERO,
            tokens.ink_weak,
        );
    }
}

/// Everything a hovered screen has to say.
fn tooltip_text(app: &App, index: usize) -> String {
    let screen = &app.screens[index];
    let mut lines = vec![screen.connectors.clone()];
    lines.push(format!(
        "{} at {}, {}",
        screen.mode, screen.position.0, screen.position.1
    ));
    if screen.primary {
        lines.push("primary screen".into());
    }
    if screen.linked {
        lines.push("One monitor, two cables. Both rectangles are the same panel.".into());
    }

    match screen.monitor.and_then(|m| app.monitors.get(m)) {
        None => lines.push("No monitor on this screen answers.".into()),
        Some(state) => {
            lines.push(String::new());
            lines.push(state.name());
            if let Some(inches) = state.diagonal_inches() {
                lines.push(format!("{inches:.0}-inch panel"));
            }
            match state.active() {
                Some(path) => lines.push(format!("controlled via {}", path.describe())),
                None => lines.push(crate::rack::cause_line(state.condition()).to_string()),
            }
            lines.push(format!("key {}", state.id.key));
            if state.condition() == Condition::Ready {
                lines.push(String::new());
                lines.push("Drag up or down to dim · scroll for small steps · click to select".into());
            }
        }
    }
    lines.join("\n")
}

/// Cut text that will not fit, so a label cannot spill out of its screen.
fn elide(text: &str, width: f32, font_size: f32) -> String {
    // Proportional text averages a little over half the point size per character. The exact
    // figure does not matter: this only decides when to add an ellipsis.
    let fits = (width / (font_size * 0.55)).floor().max(3.0) as usize;
    if text.chars().count() <= fits {
        return text.to_string();
    }
    let kept: String = text.chars().take(fits.saturating_sub(1)).collect();
    format!("{kept}…")
}

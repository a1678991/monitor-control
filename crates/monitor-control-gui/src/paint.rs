//! Everything the window draws by hand.
//!
//! egui bundles Ubuntu-Light and Hack, and between them they are missing most of the symbol
//! characters an interface like this would reach for: `●`, `✓`, `⇄` and their neighbours all
//! render as empty boxes. So every status mark, badge and icon here is painted with the 2D
//! painter rather than typed. That is not a workaround — it also means each mark can be a
//! *shape* rather than a colour, which is what lets the five conditions be told apart without
//! reading and without relying on hue.

use eframe::egui::{
    self, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2,
};

use crate::state::Condition;
use crate::theme::Tokens;

/// The colour that stands for a condition.
pub fn condition_colour(condition: Condition, tokens: &Tokens) -> Color32 {
    match condition {
        Condition::Ready => tokens.ok,
        Condition::MenuOff => tokens.menu,
        Condition::NoPermission => tokens.locked,
        Condition::Silent => tokens.silent,
        Condition::Away => tokens.away,
        Condition::Unread => tokens.ink_weak,
    }
}

/// A miniature of a monitor, with the condition drawn inside it.
///
/// Every mark is the same rounded rectangle, so what differs is the shape within: a filled
/// panel, a bar across the middle, a standby dot, a padlock shackle, an arrow leaving. Shape
/// first, colour second — someone who cannot tell amber from slate still sees "bar across the
/// middle" against "dot at the bottom".
pub fn mark(painter: &Painter, rect: Rect, condition: Condition, tokens: &Tokens, fill: f32) {
    let colour = condition_colour(condition, tokens);
    let body = Rect::from_center_size(rect.center(), vec2(16.0, 12.0));

    if condition == Condition::Ready {
        painter.rect_filled(body, CornerRadius::same(2), tokens.tile_bg);
        let lit = body.height() * fill.clamp(0.0, 1.0);
        if lit > 0.0 {
            painter.rect_filled(
                Rect::from_min_max(pos2(body.left(), body.bottom() - lit), body.max),
                CornerRadius { nw: 0, ne: 0, sw: 2, se: 2 },
                tokens.level,
            );
        }
        painter.rect_stroke(body, CornerRadius::same(2), Stroke::new(1.4, colour), StrokeKind::Inside);
        return;
    }

    painter.rect_stroke(body, CornerRadius::same(2), Stroke::new(1.4, colour), StrokeKind::Inside);
    let c = body.center();
    match condition {
        // A setting lives inside this thing.
        Condition::MenuOff => {
            painter.line_segment(
                [pos2(c.x - 5.0, c.y - 0.5), pos2(c.x + 5.0, c.y - 0.5)],
                Stroke::new(1.5, colour),
            );
        }
        // The standby light on a sleeping panel.
        Condition::Silent => {
            painter.circle_filled(pos2(c.x, body.bottom() - 3.0), 1.6, colour);
        }
        // A shackle, which is as much padlock as sixteen points will hold.
        Condition::NoPermission => {
            painter.add(Shape::line(
                vec![
                    pos2(c.x - 3.0, c.y + 2.0),
                    pos2(c.x - 3.0, c.y - 1.5),
                    pos2(c.x + 3.0, c.y - 1.5),
                    pos2(c.x + 3.0, c.y + 2.0),
                ],
                Stroke::new(1.4, colour),
            ));
        }
        // Leaving, not broken.
        Condition::Away => {
            painter.line_segment([pos2(c.x - 4.0, c.y), pos2(c.x + 4.0, c.y)], Stroke::new(1.3, colour));
            painter.add(Shape::line(
                vec![pos2(c.x + 1.4, c.y - 2.6), pos2(c.x + 4.2, c.y), pos2(c.x + 1.4, c.y + 2.6)],
                Stroke::new(1.3, colour),
            ));
        }
        // Nothing inside: nothing has been read.
        Condition::Unread | Condition::Ready => {}
    }
}

/// The same shapes at readout size, drawn where the numeral would be.
///
/// The mark in the row's second column is 16 points wide; a glance across a window does not
/// resolve that. This is the same information at 20, in the column the eye already goes to for
/// a value.
pub fn badge(painter: &Painter, centre: Pos2, condition: Condition, tokens: &Tokens) {
    let colour = condition_colour(condition, tokens);
    let body = Rect::from_center_size(centre, vec2(20.0, 15.0));
    painter.rect_stroke(body, CornerRadius::same(3), Stroke::new(1.7, colour), StrokeKind::Inside);

    let c = body.center();
    match condition {
        Condition::MenuOff => {
            painter.line_segment(
                [pos2(c.x - 6.4, c.y - 0.5), pos2(c.x + 6.4, c.y - 0.5)],
                Stroke::new(1.8, colour),
            );
        }
        Condition::Silent => {
            painter.circle_filled(pos2(c.x, body.bottom() - 3.6), 2.0, colour);
        }
        Condition::NoPermission => padlock(painter, c, colour, tokens.tile_bg),
        Condition::Away => {
            painter.line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], Stroke::new(1.7, colour));
            painter.add(Shape::line(
                vec![pos2(c.x + 1.6, c.y - 3.2), pos2(c.x + 5.2, c.y), pos2(c.x + 1.6, c.y + 3.2)],
                Stroke::new(1.7, colour),
            ));
        }
        Condition::Unread | Condition::Ready => {}
    }
}

/// A padlock, big enough to read as one.
pub fn padlock(painter: &Painter, centre: Pos2, colour: Color32, keyhole: Color32) {
    let body = Rect::from_center_size(centre + vec2(0.0, 3.0), vec2(13.0, 9.0));
    painter.rect_filled(body, CornerRadius::same(2), colour);
    painter.add(Shape::line(
        vec![
            centre + vec2(-4.0, -1.5),
            centre + vec2(-4.0, -6.0),
            centre + vec2(4.0, -6.0),
            centre + vec2(4.0, -1.5),
        ],
        Stroke::new(1.8, colour),
    ));
    painter.circle_filled(centre + vec2(0.0, 2.5), 1.4, keyhole);
}

/// Fill a rectangle with 45-degree stripes.
///
/// The counterpart to a solid fill, and deliberately never used as one: a warm bar and amber
/// diagonals are not confusable at any size, where two similar ambers would be.
pub fn hatch(painter: &Painter, rect: Rect, colour: Color32, pitch: f32) {
    let painter = painter.with_clip_rect(rect);
    let height = rect.height();
    let mut x = rect.left() - height;
    while x < rect.right() {
        painter.line_segment(
            [pos2(x, rect.bottom()), pos2(x + height, rect.top())],
            Stroke::new(1.0, colour),
        );
        x += pitch;
    }
}

/// A dashed outline, for a rectangle that is empty because nothing answered.
pub fn dashed_rect(painter: &Painter, rect: Rect, stroke: Stroke) {
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    for pair in corners.windows(2) {
        painter.extend(Shape::dashed_line(pair, stroke, 5.0, 4.0));
    }
}

/// The channel number, on a disc that keeps it legible over a lit panel.
pub fn channel_disc(painter: &Painter, ui: &egui::Ui, centre: Pos2, channel: usize, tokens: &Tokens) {
    painter.circle_filled(centre, 9.0, ui.visuals().panel_fill.gamma_multiply(0.85));
    painter.text(
        centre,
        egui::Align2::CENTER_CENTER,
        channel.to_string(),
        egui::FontId::new(13.0, egui::FontFamily::Monospace),
        tokens.ink,
    );
}

/// Two offset rectangles, meaning these two screens are one panel.
///
/// No line joining the pair: with a third screen between them, a line either crosses an
/// innocent rectangle or hides behind it.
pub fn link_badge(painter: &Painter, centre: Pos2, colour: Color32) {
    let back = Rect::from_min_size(centre + vec2(-7.0, -6.0), vec2(8.0, 7.0));
    let front = Rect::from_min_size(centre + vec2(-2.0, -1.0), vec2(8.0, 7.0));
    painter.rect_stroke(back, CornerRadius::same(1), Stroke::new(1.1, colour), StrokeKind::Inside);
    painter.rect_filled(front, CornerRadius::same(1), colour);
}

/// A circular arrow: re-check.
pub fn icon_recheck(painter: &Painter, centre: Pos2, colour: Color32) {
    let stroke = Stroke::new(1.4, colour);
    let radius = 4.6;
    // Five sixths of a circle, leaving a gap for the head to sit in.
    let start = std::f32::consts::TAU * 0.05;
    let sweep = std::f32::consts::TAU * 0.80;
    let at = |t: f32| {
        let angle = start + sweep * t;
        centre + vec2(radius * angle.cos(), radius * angle.sin())
    };
    painter.add(Shape::line((0u8..=24).map(|i| at(f32::from(i) / 24.0)).collect(), stroke));

    // The head points along the tangent at the end of the arc, which is what makes it read as
    // an arrow rather than as a tick that happens to be nearby.
    let tip = at(1.0);
    let angle = start + sweep;
    let along = vec2(-angle.sin(), angle.cos());
    let across = vec2(angle.cos(), angle.sin());
    painter.add(Shape::line(
        vec![
            tip - along * 3.0 + across * 2.0,
            tip,
            tip - along * 3.0 - across * 2.0,
        ],
        stroke,
    ));
}

/// A chevron: open the details.
pub fn icon_chevron(painter: &Painter, centre: Pos2, colour: Color32, open: bool) {
    let dir = if open { -1.0 } else { 1.0 };
    painter.add(Shape::line(
        vec![
            centre + vec2(-4.4, -2.0 * dir),
            centre + vec2(0.0, 2.2 * dir),
            centre + vec2(4.4, -2.0 * dir),
        ],
        Stroke::new(1.5, colour),
    ));
}

/// Two equal bars: level every monitor to the same percentage.
pub fn icon_level(painter: &Painter, centre: Pos2, colour: Color32) {
    for dy in [-2.6, 2.6] {
        painter.rect_filled(
            Rect::from_center_size(centre + vec2(0.0, dy), vec2(11.0, 1.7)),
            CornerRadius::ZERO,
            colour,
        );
    }
}

/// A sweep along the bottom edge of a row, meaning this monitor is mid-exchange.
///
/// Drawn on the row that is waiting and nowhere else, so a display taking four seconds to wake
/// never suggests the window itself is stuck.
pub fn busy_sweep(painter: &Painter, rect: Rect, phase: f32, colour: Color32) {
    let bar = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 2.0), rect.right_bottom());
    let width = bar.width() * 0.22;
    let travel = bar.width() + width;
    let x = bar.left() - width + travel * phase.fract();
    let sweep = Rect::from_min_size(pos2(x, bar.top()), vec2(width, 2.0)).intersect(bar);
    if sweep.width() > 0.0 {
        painter.rect_filled(sweep, CornerRadius::same(1), colour.gamma_multiply(0.7));
    }
}

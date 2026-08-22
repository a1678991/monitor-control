//! Colours, type, and spacing.
//!
//! Everything structural — panels, rails, buttons, text — comes from egui's own light and dark
//! palettes, so the window follows the desktop's theme. Only the colours that carry meaning
//! are defined outright, and they are defined for both themes and resolved on every frame:
//! caching them at startup is what stops a window from noticing a light/dark switch made while
//! it is open.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, TextStyle, Theme, Vec2};

/// The meaning colours, resolved for one theme.
///
/// Split by *what the user would do next* rather than by severity. A display with DDC/CI
/// switched off needs a walk to its own menu; one whose device node cannot be opened needs a
/// udev rule; one that is silent needs waking. Collapsing any two of those into a single
/// "warning" colour sends people to the wrong fix.
#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    /// Brightness: the trailing fill on a fader, the fill on a screen, the level line.
    /// Never used for text.
    pub level: Color32,
    /// The percent numeral, which sits on top of `level` and so cannot be `level`.
    pub level_ink: Color32,
    /// A display that answers.
    pub ok: Color32,
    /// DDC/CI switched off in the display's own menu. Always drawn as a 45° hatch.
    pub menu: Color32,
    /// Nothing answered: asleep, or showing another machine.
    pub silent: Color32,
    /// The device node could not be opened.
    pub locked: Color32,
    /// Deliberately switched to another input. Success, not failure.
    pub away: Color32,
    /// One exchange failed. The only red in the window, and never without a `Try again`.
    pub fail: Color32,
    /// The confirmation tick and the master's per-channel ticks.
    pub ghost: Color32,
    /// Display names.
    pub ink: Color32,
    /// Sub-lines, connectors, hints.
    pub ink_weak: Color32,
    /// The desk plan's canvas.
    pub desk_bg: Color32,
    /// An unlit screen rectangle.
    pub tile_bg: Color32,
    /// A screen that is asleep, away, or unreadable.
    pub tile_dim: Color32,
}

const LIGHT: Tokens = Tokens {
    level: Color32::from_rgb(0xA9, 0x6D, 0x12),
    level_ink: Color32::from_rgb(0x5F, 0x42, 0x0A),
    ok: Color32::from_rgb(0x15, 0x80, 0x3D),
    menu: Color32::from_rgb(0x8F, 0x4E, 0x06),
    silent: Color32::from_rgb(0x5C, 0x63, 0x6E),
    locked: Color32::from_rgb(0x2C, 0x5D, 0x9B),
    away: Color32::from_rgb(0x12, 0x66, 0xC7),
    fail: Color32::from_rgb(0xB3, 0x26, 0x1E),
    ghost: Color32::from_rgb(0x5F, 0x5F, 0x5F),
    ink: Color32::from_rgb(0x14, 0x16, 0x1A),
    ink_weak: Color32::from_rgb(0x60, 0x68, 0x74),
    desk_bg: Color32::from_rgb(0xE4, 0xE6, 0xEA),
    tile_bg: Color32::from_rgb(0xFA, 0xFB, 0xFC),
    tile_dim: Color32::from_rgb(0xEA, 0xEC, 0xEF),
};

const DARK: Tokens = Tokens {
    level: Color32::from_rgb(0xF2, 0xC0, 0x69),
    level_ink: Color32::from_rgb(0xF7, 0xD8, 0x9B),
    ok: Color32::from_rgb(0x5B, 0xC4, 0x6F),
    menu: Color32::from_rgb(0xE9, 0xA3, 0x3C),
    silent: Color32::from_rgb(0x9A, 0xA2, 0xAE),
    locked: Color32::from_rgb(0x7F, 0xA9, 0xD9),
    away: Color32::from_rgb(0x6F, 0xB0, 0xEE),
    fail: Color32::from_rgb(0xE5, 0x67, 0x5C),
    ghost: Color32::from_rgb(0xC2, 0xC2, 0xC2),
    ink: Color32::from_rgb(0xF2, 0xF3, 0xF5),
    ink_weak: Color32::from_rgb(0x94, 0x9B, 0xA6),
    desk_bg: Color32::from_rgb(0x14, 0x15, 0x18),
    tile_bg: Color32::from_rgb(0x1E, 0x21, 0x26),
    tile_dim: Color32::from_rgb(0x19, 0x1B, 0x1F),
};

impl Tokens {
    /// The palette for the theme the window is being drawn in right now.
    pub fn of(ctx: &egui::Context) -> Self {
        if ctx.theme() == Theme::Dark { DARK } else { LIGHT }
    }
}

/// Named text styles.
///
/// Every number that changes while the pointer moves is monospace: Hack's digits are tabular,
/// so a percentage going 48, 47, 46 does not shove its neighbours about. That is the reason,
/// not taste.
pub mod text {
    use super::{FontFamily, FontId, TextStyle};

    /// The brightness percentage in the readout column and on the master.
    pub fn readout() -> TextStyle {
        TextStyle::Name("readout".into())
    }
    /// The `35 / 80` line under a readout.
    pub fn readout_sub() -> TextStyle {
        TextStyle::Name("readout-sub".into())
    }
    /// A display name.
    pub fn name() -> TextStyle {
        TextStyle::Name("name".into())
    }
    /// A channel number.
    pub fn channel() -> TextStyle {
        TextStyle::Name("channel".into())
    }

    /// The font for a screen's brightness numeral at the given tile height.
    ///
    /// Three sizes rather than a continuous scale, so numerals across a desk of differently
    /// sized screens still look like the same typeface at the same few sizes.
    pub fn tile_numeral(height: f32) -> Option<FontId> {
        let size = if height >= 110.0 {
            28.0
        } else if height >= 70.0 {
            20.0
        } else if height >= 46.0 {
            15.0
        } else {
            return None;
        };
        Some(FontId::new(size, FontFamily::Proportional))
    }
}

/// The gap between the two halves of a row, and everything else on the spacing scale.
pub mod space {
    /// Between the columns of a row.
    pub const COLUMN: f32 = 10.0;
    /// A row's padding above and below its content.
    pub const ROW: f32 = 6.0;
    /// A row's content box, which sets the row pitch.
    pub const ROW_CONTENT: f32 = 44.0;
    /// The height of a fader rail.
    pub const RAIL: f32 = 12.0;
}

/// Fixed column widths for the seven-column grid the master and every row share.
///
/// That the master's fader occupies exactly the same x-range as every channel's fader is the
/// design: after a week the pointer goes to a place, not to a control.
pub mod column {
    /// The channel number.
    pub const CHANNEL: f32 = 26.0;
    /// The painted state mark.
    pub const MARK: f32 = 20.0;
    /// Display name and sub-line.
    pub const NAME: f32 = 200.0;
    /// The percentage and its sub-line.
    pub const READOUT: f32 = 92.0;
    /// The input chip.
    pub const SWITCH: f32 = 66.0;
    /// The row's buttons.
    pub const TOOLS: f32 = 128.0;

    /// Everything except the fader, which takes what is left.
    pub fn fixed() -> f32 {
        CHANNEL + MARK + NAME + READOUT + SWITCH + TOOLS
    }
}

/// A panel frame with a known inner margin.
///
/// egui's own top-panel frame uses a margin of eight by two, which is not what this layout
/// wants and would quietly make every `exact_size` four points wrong.
pub fn panel_frame(ui: &egui::Ui, horizontal: i8, vertical: i8) -> egui::Frame {
    egui::Frame::NONE
        .fill(ui.visuals().panel_fill)
        .inner_margin(egui::Margin { left: horizontal, right: horizontal, top: vertical, bottom: vertical })
}

/// The environment variable that pins the window to one theme.
///
/// Set to `light` or `dark` to override the desktop's own setting; anything else, or unset,
/// follows the desktop and switches with it.
pub const THEME_ENV: &str = "MONITORCTL_GUI_THEME";

/// Install the type scale and spacing.
///
/// Through `all_styles_mut` so it applies to both themes: setting it for the active one only
/// means the window loses its typography the moment the desktop switches.
pub fn install(ctx: &egui::Context) {
    ctx.set_theme(match std::env::var(THEME_ENV).as_deref().map(str::trim) {
        Ok("light") => egui::ThemePreference::Light,
        Ok("dark") => egui::ThemePreference::Dark,
        _ => egui::ThemePreference::System,
    });

    ctx.all_styles_mut(|style| {
        use FontFamily::{Monospace, Proportional};

        style.text_styles = [
            (TextStyle::Body, FontId::new(12.5, Proportional)),
            (TextStyle::Button, FontId::new(12.5, Proportional)),
            (TextStyle::Small, FontId::new(10.5, Proportional)),
            (TextStyle::Monospace, FontId::new(11.0, Monospace)),
            // The window manager already shows a title; the window does not need a second one.
            (TextStyle::Heading, FontId::new(14.0, Proportional)),
            (TextStyle::Name("readout".into()), FontId::new(22.0, Monospace)),
            (TextStyle::Name("readout-sub".into()), FontId::new(10.5, Monospace)),
            (TextStyle::Name("name".into()), FontId::new(14.0, Proportional)),
            (TextStyle::Name("channel".into()), FontId::new(13.0, Monospace)),
        ]
        .into();

        style.spacing.item_spacing = Vec2::new(space::COLUMN, space::ROW);
        style.spacing.button_padding = Vec2::new(8.0, 4.0);
        style.spacing.interact_size.y = 22.0;
        style.spacing.slider_rail_height = space::RAIL;
        style.spacing.slider_width = 120.0;

        // Tighter corners than egui's default; an instrument, not a card.
        for widget in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            widget.corner_radius = CornerRadius::same(4);
        }
        style.visuals.slider_trailing_fill = true;
        style.visuals.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.4 };
    });
}

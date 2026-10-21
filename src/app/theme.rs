//! Maya 2017+ dark theme: flat #444 panels, dark #2b2b2b fields, blue selection.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Vec2};

pub const PANEL: Color32 = Color32::from_gray(68);
pub const FIELD: Color32 = Color32::from_gray(43);
pub const BUTTON: Color32 = Color32::from_gray(93);
pub const HOVER: Color32 = Color32::from_gray(110);
pub const BORDER: Color32 = Color32::from_gray(43);
pub const TEXT: Color32 = Color32::from_gray(200);
pub const TEXT_DIM: Color32 = Color32::from_gray(140);
pub const SELECT: Color32 = Color32::from_rgb(82, 133, 166);

/// Viewport background, top and bottom of a very soft gradient.
pub const VIEW_TOP: Color32 = Color32::from_gray(106);
pub const VIEW_BOTTOM: Color32 = Color32::from_gray(84);

/// Wireframe colors (sRGB 0..1) used by the viewport.
pub const WIRE_SELECTED: [f32; 3] = [0.26, 1.0, 0.64];
pub const WIRE_DEFAULT: [f32; 3] = [0.0, 0.02, 0.38];

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (TextStyle::Heading, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(12.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(12.0, FontFamily::Proportional)),
        (TextStyle::Small, FontId::new(10.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(4.0, 3.0);
    style.spacing.button_padding = Vec2::new(5.0, 1.0);
    style.spacing.interact_size.y = 18.0;
    style.spacing.menu_margin = egui::Margin::same(4);

    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = FIELD;
    v.faint_bg_color = Color32::from_gray(74);
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = SELECT;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = SELECT;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.menu_corner_radius = CornerRadius::same(2);
    v.window_corner_radius = CornerRadius::same(2);

    let round = CornerRadius::same(2);
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = round;
    for (state, fill) in [(&mut w.inactive, BUTTON), (&mut w.hovered, HOVER), (&mut w.active, SELECT)] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::NONE;
        state.fg_stroke = Stroke::new(1.0, TEXT);
        state.corner_radius = round;
        state.expansion = 0.0;
    }
    w.open.bg_fill = SELECT;
    w.open.weak_bg_fill = SELECT;
    ctx.set_style(style);
}

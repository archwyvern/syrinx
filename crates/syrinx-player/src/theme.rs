//! Dark, high-contrast, nothing readable under 12 px: values and body are near-white, labels
//! one step greyer, and that is as dim as information gets.

use egui::{Color32, FontId, TextStyle};

pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe6, 0xe6);
pub const LABEL: Color32 = Color32::from_rgb(0xd8, 0xd8, 0xda);
pub const ACCENT: Color32 = Color32::from_rgb(0x7c, 0xb8, 0xff);
pub const ERROR: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);
pub const WARN: Color32 = Color32::from_rgb(0xff, 0xc8, 0x5c);
pub const WAVE: Color32 = Color32::from_rgb(0x8f, 0xc7, 0xa8);
pub const WAVE_BG: Color32 = Color32::from_rgb(0x16, 0x18, 0x1c);
pub const PENDING: Color32 = Color32::from_rgba_premultiplied(0x40, 0x40, 0x48, 0xa0);
pub const PLAYHEAD: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = Color32::from_rgb(0x2a, 0x4a, 0x6e);
    visuals.selection.stroke.color = ACCENT;
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
            (TextStyle::Heading, FontId::proportional(18.0)),
        ]
        .into_iter()
        .collect();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.slider_width = 140.0;
    });
}

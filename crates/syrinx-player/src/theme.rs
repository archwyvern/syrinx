//! Dark, high-contrast, nothing readable under 12 px: values and body are near-white, labels
//! one step greyer, and that is as dim as information gets. Surfaces step from the darkest (the
//! picture of the sound) to the lightest (the bars the controls live on). Text is Inter at its
//! regular weight: egui's own face is a light one, too thin to read as white on dark.

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, TextStyle};

pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe6, 0xe6);
pub const LABEL: Color32 = Color32::from_rgb(0xd8, 0xd8, 0xda);
pub const ACCENT: Color32 = Color32::from_rgb(0x7c, 0xb8, 0xff);
/// Text on an accent fill.
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x0e, 0x13, 0x1a);
pub const ERROR: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);
pub const WARN: Color32 = Color32::from_rgb(0xff, 0xc8, 0x5c);
pub const MUTE: Color32 = Color32::from_rgb(0xff, 0xb3, 0x47);

/// The central area.
pub const BG: Color32 = Color32::from_rgb(0x1a, 0x1c, 0x21);
/// The playlist.
pub const PANEL: Color32 = Color32::from_rgb(0x16, 0x18, 0x1c);
/// The transport and status bars.
pub const BAR: Color32 = Color32::from_rgb(0x22, 0x25, 0x2b);
/// A card on the central area: the empty state, a row being hovered.
pub const SURFACE: Color32 = Color32::from_rgb(0x24, 0x27, 0x2e);
pub const BORDER: Color32 = Color32::from_rgb(0x34, 0x38, 0x41);

pub const WAVE_BG: Color32 = Color32::from_rgb(0x11, 0x13, 0x16);
/// The part of the sound already heard, and the part still to come.
pub const WAVE_PLAYED: Color32 = Color32::from_rgb(0x9f, 0xe0, 0xbd);
pub const WAVE: Color32 = Color32::from_rgb(0x5a, 0x86, 0x70);
pub const PENDING: Color32 = Color32::from_rgba_premultiplied(0x40, 0x40, 0x48, 0xa0);
pub const PLAYHEAD: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
/// A meter below -6 dBFS; amber ([`WARN`]) above that, red ([`ERROR`]) over 0.
pub const METER: Color32 = Color32::from_rgb(0x5f, 0xd0, 0x8f);

pub fn apply(ctx: &egui::Context) {
    // Inter first; egui's faces stay behind it for the glyphs Inter lacks (the transport's
    // symbols among them).
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("Inter".into(), Arc::new(FontData::from_static(include_bytes!("../fonts/Inter-Regular.ttf"))));
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "Inter".into());
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = BG;
    visuals.selection.bg_fill = Color32::from_rgb(0x2a, 0x4a, 0x6e);
    visuals.selection.stroke.color = ACCENT;
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
            (TextStyle::Heading, FontId::proportional(22.0)),
        ]
        .into_iter()
        .collect();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.slider_width = 140.0;
    });
}

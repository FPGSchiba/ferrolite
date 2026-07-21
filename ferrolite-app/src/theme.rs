//! Dark theme + bundled fonts. Tokens from docs/design/ferrolite-design-system.md §2/§3.

use egui::{Color32, Context, FontData, FontDefinitions, FontFamily, Visuals};

pub const BG_APP: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);
pub const BG_PANEL: Color32 = Color32::from_rgb(0x17, 0x17, 0x17);
pub const BG_TITLEBAR: Color32 = Color32::from_rgb(0x16, 0x16, 0x16);
pub const BG_TOOLBAR: Color32 = Color32::from_rgb(0x1d, 0x1d, 0x1d);
pub const BG_BASE: Color32 = Color32::from_rgb(0x14, 0x14, 0x14);
pub const BG_CANVAS: Color32 = Color32::from_rgb(0x0e, 0x0e, 0x0e);
// Canonical design palette (design-system §2) — full token set kept for use across later specs.
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xdc, 0xdc, 0xdc);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x8a, 0x8a);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x6a, 0x6a, 0x6a);
pub const ACCENT: Color32 = Color32::from_rgb(0x6d, 0x97, 0xb5);
pub const ACCENT_BRIGHT: Color32 = Color32::from_rgb(0xa9, 0xc7, 0xdd);
pub const ACCENT_BG_SEL: Color32 = Color32::from_rgb(0x21, 0x2a, 0x30);
#[allow(dead_code)]
pub const ACCENT_FILL: Color32 = Color32::from_rgb(0x23, 0x2b, 0x30);
#[allow(dead_code)]
pub const ACCENT_BORDER: Color32 = Color32::from_rgb(0x34, 0x46, 0x4f);
#[allow(dead_code)]
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0xcf, 0xe0, 0xec);
#[allow(dead_code)]
pub const TEXT_ACTIVE: Color32 = Color32::from_rgb(0xea, 0xf1, 0xf6);
#[allow(dead_code)]
pub const TEXT_INACTIVE: Color32 = Color32::from_rgb(0x9a, 0x9a, 0x9a);
pub const SEMANTIC_RED: Color32 = Color32::from_rgb(0xc7, 0x54, 0x50);
pub const SEMANTIC_GREEN: Color32 = Color32::from_rgb(0x4c, 0xaf, 0x71);
pub const SEMANTIC_AMBER: Color32 = Color32::from_rgb(0xd6, 0xa8, 0x4c); // warning toasts
pub const SEMANTIC_BLUE: Color32 = Color32::from_rgb(0x5a, 0x9d, 0xd6); // info toasts
/// Rating-star fill — a bright gold so stars stay legible on light images.
pub const STAR: Color32 = Color32::from_rgb(0xf2, 0xc0, 0x4d);

pub fn install(ctx: &Context) {
    install_fonts(ctx);
    let mut v = Visuals::dark();
    v.panel_fill = BG_APP;
    v.window_fill = BG_TOOLBAR;
    v.extreme_bg_color = BG_BASE;
    v.override_text_color = Some(TEXT_PRIMARY);
    v.selection.bg_fill = ACCENT_BG_SEL;
    v.selection.stroke.color = ACCENT;
    ctx.set_visuals(v);
}

fn install_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "plex-sans".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf")),
    );
    fonts.font_data.insert(
        "plex-mono".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf")),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "plex-sans".into());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "plex-mono".into());
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    // Filled glyphs (rating stars / pick flag) under a dedicated named family, since
    // add_to_fonts registers a single variant under the "phosphor" key.
    fonts.font_data.insert(
        "phosphor-fill".into(),
        egui_phosphor::Variant::Fill.font_data(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Name("phosphor-fill".into()))
        .or_default()
        .push("phosphor-fill".into());
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_token_matches_design_system() {
        assert_eq!(ACCENT, Color32::from_rgb(109, 151, 181)); // #6d97b5
    }

    #[test]
    fn app_background_token_is_dark() {
        assert_eq!(BG_APP, Color32::from_rgb(26, 26, 26)); // #1a1a1a
    }

    #[test]
    fn accent_bright_token_matches_design_system() {
        assert_eq!(ACCENT_BRIGHT, Color32::from_rgb(169, 199, 221)); // #a9c7dd
    }

    #[test]
    fn v2_accent_tokens_match_spec() {
        assert_eq!(ACCENT_FILL, Color32::from_rgb(0x23, 0x2b, 0x30)); // #232b30
        assert_eq!(ACCENT_BORDER, Color32::from_rgb(0x34, 0x46, 0x4f)); // #34464f
        assert_eq!(ACCENT_TEXT, Color32::from_rgb(0xcf, 0xe0, 0xec)); // #cfe0ec
        assert_eq!(TEXT_ACTIVE, Color32::from_rgb(0xea, 0xf1, 0xf6)); // #eaf1f6
        assert_eq!(TEXT_INACTIVE, Color32::from_rgb(0x9a, 0x9a, 0x9a)); // #9a9a9a
    }

    #[test]
    fn semantic_red_token_matches_design_system() {
        assert_eq!(SEMANTIC_RED, Color32::from_rgb(199, 84, 80)); // #c75450
    }
}

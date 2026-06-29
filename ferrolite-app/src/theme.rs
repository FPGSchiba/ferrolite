//! Dark theme + bundled fonts. Tokens from docs/design/ferrolite-design-system.md §2/§3.

use egui::{Color32, Context, FontData, FontDefinitions, FontFamily, Visuals};

pub const BG_APP: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);
pub const BG_PANEL: Color32 = Color32::from_rgb(0x17, 0x17, 0x17);
pub const BG_TITLEBAR: Color32 = Color32::from_rgb(0x16, 0x16, 0x16);
pub const BG_TOOLBAR: Color32 = Color32::from_rgb(0x1d, 0x1d, 0x1d);
pub const BG_BASE: Color32 = Color32::from_rgb(0x14, 0x14, 0x14);
pub const BG_CANVAS: Color32 = Color32::from_rgb(0x0e, 0x0e, 0x0e);
// Canonical design palette (design-system §2) — full token set kept for use across later specs.
#[allow(dead_code)]
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xdc, 0xdc, 0xdc);
#[allow(dead_code)]
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x8a, 0x8a);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x6a, 0x6a, 0x6a);
pub const ACCENT: Color32 = Color32::from_rgb(0x6d, 0x97, 0xb5);
pub const ACCENT_BRIGHT: Color32 = Color32::from_rgb(0xa9, 0xc7, 0xdd);
pub const ACCENT_BG_SEL: Color32 = Color32::from_rgb(0x21, 0x2a, 0x30);

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
}

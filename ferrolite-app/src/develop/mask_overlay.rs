//! Canvas mask overlay: paints the composited coverage as a red tint over the
//! displayed image, then routes tool affordances (Tasks 10–12). Pure math lives
//! in `mask_affordance`; this layer only paints + routes pointer events (same
//! discipline as `crop_overlay`). Visual-tested.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::mask_ui::MaskUiState;
use ferrolite_pipeline::OpStack;

/// Paint the coverage fill (if a texture is ready + overlay is on) and route tool
/// affordances. `overlay_tex` is the app-built red-RGBA coverage texture (None
/// until first built / when no mask is selected).
pub fn show(
    ui: &mut egui::Ui,
    image_rect: egui::Rect,
    stack: &OpStack,
    mask: &mut MaskUiState,
    overlay_tex: Option<&egui::TextureHandle>,
) -> Option<EditOutcome> {
    // Fill: stretch the coverage texture over the image rect with alpha blend.
    if mask.overlay_on {
        if let Some(tex) = overlay_tex {
            ui.painter().image(
                tex.id(),
                image_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE, // the texture already carries red + per-texel alpha
            );
        }
    }
    // Affordance routing is added in Tasks 10–12.
    let _ = (stack, mask);
    None
}

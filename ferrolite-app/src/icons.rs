//! Semantic icon aliases over the bundled icon font (egui-phosphor, installed in
//! `theme::install_fonts`). UI code uses these constants (e.g. `icons::CROP`) rendered
//! in the icon font family — never raw emoji/symbol glyphs (IBM Plex + egui's emoji
//! subset don't cover them → tofu) and never new hand-drawn Painter icons. Add a new
//! icon = add one alias here, sourced from the Phosphor catalog
//! (`egui_phosphor::regular::*`).

// All names below are VERIFIED to exist in egui-phosphor 0.7.3 `regular`/`fill`.
use egui_phosphor::fill as pf;
use egui_phosphor::regular as p;

pub const ADJUST: &str = p::SLIDERS_HORIZONTAL;
pub const CROP: &str = p::CROP;
pub const MASK: &str = p::CIRCLE_HALF_TILT;
pub const HEAL: &str = p::BANDAIDS;
pub const BRUSH: &str = p::PAINT_BRUSH;
pub const LINEAR_GRADIENT: &str = p::GRADIENT;
pub const RADIAL_GRADIENT: &str = p::CIRCLE;
pub const LUMA: &str = p::CIRCLE_HALF;
pub const COLOR: &str = p::PALETTE;
// The following aliases have no consumer yet in the bin (the lib copy is `pub`, so
// dead_code doesn't fire there) — each is reserved for a later, not-yet-landed UI
// task. Scoped `#[allow(dead_code)]` per item (not a blanket module allow) so new
// aliases stay covered by the lint by default; remove the annotation once wired up.
#[allow(dead_code)] // color-range/eyedropper canvas affordance icon — future task
pub const EYEDROPPER: &str = p::EYEDROPPER;
pub const UNDO: &str = p::ARROW_COUNTER_CLOCKWISE;
pub const REDO: &str = p::ARROW_CLOCKWISE;
pub const DELETE: &str = p::TRASH;
#[allow(dead_code)] // rename/edit affordance icon — future task
pub const EDIT: &str = p::PENCIL_SIMPLE;
#[allow(dead_code)] // per-control reset arrow icon — future task
pub const RESET: &str = p::ARROW_COUNTER_CLOCKWISE;
#[allow(dead_code)] // rating stars (library grid/filmstrip) — future task
pub const STAR: &str = p::STAR; // outline (regular) — render with font()
#[allow(dead_code)] // rating stars (library grid/filmstrip) — future task
pub const STAR_FILL: &str = pf::STAR; // filled (fill variant) — render with font_fill()
#[allow(dead_code)] // pick flag (library grid/filmstrip) — future task
pub const FLAG: &str = p::FLAG; // pick flag outline — font()
#[allow(dead_code)] // pick flag (library grid/filmstrip) — future task
pub const FLAG_FILL: &str = pf::FLAG; // pick flag filled — font_fill()
#[allow(dead_code)] // reject flag (library grid/filmstrip) — future task
pub const FLAG_REJECT: &str = p::PROHIBIT; // reject — font()
#[allow(dead_code)] // combo/dropdown affordance icon — future task
pub const CARET_DOWN: &str = p::CARET_DOWN;
#[allow(dead_code)] // combo/dropdown affordance icon — future task
pub const CARET_UP: &str = p::CARET_UP;
#[allow(dead_code)] // histogram/overlay visibility toggle icon — future task
pub const OVERLAY_ON: &str = p::EYE;
#[allow(dead_code)] // histogram/overlay visibility toggle icon — future task
pub const OVERLAY_OFF: &str = p::EYE_SLASH;
pub const WARNING: &str = p::WARNING;

/// The regular icon font. `add_to_fonts(Regular)` put Phosphor Regular into the
/// `Proportional` family's fallback chain, so its PUA codepoints resolve here.
pub fn font(size: f32) -> egui::FontId {
    egui::FontId::proportional(size)
}

/// The filled icon font (rating stars / pick flag), registered under the
/// `"phosphor-fill"` named family in `theme::install_fonts`.
#[allow(dead_code)] // paired with STAR_FILL/FLAG_FILL — future task
pub fn font_fill(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name("phosphor-fill".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_alias_is_nonempty() {
        for (name, s) in [
            ("ADJUST", ADJUST),
            ("CROP", CROP),
            ("MASK", MASK),
            ("HEAL", HEAL),
            ("BRUSH", BRUSH),
            ("LINEAR_GRADIENT", LINEAR_GRADIENT),
            ("RADIAL_GRADIENT", RADIAL_GRADIENT),
            ("LUMA", LUMA),
            ("COLOR", COLOR),
            ("EYEDROPPER", EYEDROPPER),
            ("UNDO", UNDO),
            ("REDO", REDO),
            ("DELETE", DELETE),
            ("EDIT", EDIT),
            ("RESET", RESET),
            ("STAR", STAR),
            ("STAR_FILL", STAR_FILL),
            ("FLAG", FLAG),
            ("FLAG_FILL", FLAG_FILL),
            ("FLAG_REJECT", FLAG_REJECT),
            ("CARET_DOWN", CARET_DOWN),
            ("CARET_UP", CARET_UP),
            ("OVERLAY_ON", OVERLAY_ON),
            ("OVERLAY_OFF", OVERLAY_OFF),
            ("WARNING", WARNING),
        ] {
            assert!(!s.is_empty(), "icon alias {name} is empty");
        }
    }
}

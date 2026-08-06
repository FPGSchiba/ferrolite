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
pub const GRADE: &str = p::CIRCLES_THREE;
// The following aliases have no consumer yet in the bin (the lib copy is `pub`, so
// dead_code doesn't fire there) — each is reserved for a later, not-yet-landed UI
// task. Scoped `#[allow(dead_code)]` per item (not a blanket module allow) so new
// aliases stay covered by the lint by default; remove the annotation once wired up.
#[allow(dead_code)]
pub const EFFECTS: &str = p::SPARKLE; // Generic "effects" mark for enhancement tabs
pub const EYEDROPPER: &str = p::EYEDROPPER;
pub const UNDO: &str = p::ARROW_COUNTER_CLOCKWISE;
pub const REDO: &str = p::ARROW_CLOCKWISE;
pub const DELETE: &str = p::TRASH;
pub const EDIT: &str = p::PENCIL_SIMPLE;
pub const RESET: &str = p::ARROW_COUNTER_CLOCKWISE;
pub const STAR: &str = p::STAR; // outline (regular) — render with font()
pub const STAR_FILL: &str = pf::STAR; // filled (fill variant) — render with font_fill()
pub const FLAG: &str = p::FLAG; // pick flag outline — font()
pub const FLAG_FILL: &str = pf::FLAG; // pick flag filled — font_fill()
pub const FLAG_REJECT: &str = p::PROHIBIT; // reject — font()
pub const CARET_DOWN: &str = p::CARET_DOWN;
pub const CARET_UP: &str = p::CARET_UP;
pub const CARET_RIGHT: &str = p::CARET_RIGHT;
pub const OVERLAY_ON: &str = p::EYE;
pub const OVERLAY_OFF: &str = p::EYE_SLASH;
pub const WARNING: &str = p::WARNING;
pub const INFO: &str = p::INFO;
pub const NOTIFY_ERROR: &str = p::WARNING_OCTAGON; // error toast glyph
pub const CLOSE: &str = p::X; // toast dismiss button
pub const CROP_FLIP_ORIENTATION: &str = p::ARROWS_CLOCKWISE; // crop panel's landscape<->portrait toggle

// Presets / copy-paste-sync (P7). No consumer yet — Tasks 7-8 wire these into
// the preset list and the copy/paste settings menu. Scoped per-item
// `#[allow(dead_code)]` (not a blanket module allow) so each stays covered by
// the lint by default; remove the annotation once wired up.
/// Presets menu / preset list entries. Distinct from `ADJUST` (SLIDERS_HORIZONTAL) —
/// a stack of saved settings, not a single control. Reserved for Task 7.
#[allow(dead_code)]
pub const PRESET: &str = p::STACK;
/// Copy settings. Reserved for Task 8.
#[allow(dead_code)]
pub const COPY_SETTINGS: &str = p::COPY;
/// Paste settings. Reserved for Task 8.
#[allow(dead_code)]
pub const PASTE_SETTINGS: &str = p::CLIPBOARD_TEXT;

/// The regular icon font. `add_to_fonts(Regular)` put Phosphor Regular into the
/// `Proportional` family's fallback chain, so its PUA codepoints resolve here.
pub fn font(size: f32) -> egui::FontId {
    egui::FontId::proportional(size)
}

/// The filled icon font (rating stars / pick flag), registered under the
/// `"phosphor-fill"` named family in `theme::install_fonts`.
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
            ("GRADE", GRADE),
            ("EFFECTS", EFFECTS),
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
            ("CARET_RIGHT", CARET_RIGHT),
            ("OVERLAY_ON", OVERLAY_ON),
            ("OVERLAY_OFF", OVERLAY_OFF),
            ("WARNING", WARNING),
            ("INFO", INFO),
            ("NOTIFY_ERROR", NOTIFY_ERROR),
            ("CLOSE", CLOSE),
            ("CROP_FLIP_ORIENTATION", CROP_FLIP_ORIENTATION),
            ("PRESET", PRESET),
            ("COPY_SETTINGS", COPY_SETTINGS),
            ("PASTE_SETTINGS", PASTE_SETTINGS),
        ] {
            assert!(!s.is_empty(), "icon alias {name} is empty");
        }
    }
}

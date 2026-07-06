//! Per-image Masking-tool UI state + the tool enum. Pure data + tiny selection
//! helpers (unit-tested); egui rendering lives in `mask_panel`/`mask_overlay`.
//! Mirrors how `hsl_band`/`crop_active` live on `ViewerState` (survives the
//! panel's per-frame `Option` plumbing).

use ferrolite_mask::{BrushNode, CompositeMode, Rgb};

/// The unified Masking tool's active component tool. Linear/Radial are gradient
/// component types, not separate tools (design §9.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MaskTool {
    #[default]
    Brush,
    Linear,
    Radial,
    LumaRange,
    ColorRange,
}

/// An in-progress canvas gesture (between pointer-down and pointer-up). `None`
/// between gestures. Filled by the affordance routing in `mask_overlay`.
pub enum MaskGesture {
    /// Brush stroke being captured: accumulated dab nodes (normalized source coords).
    Stroke(Vec<BrushNode>),
    /// A shape handle being dragged: the component index within the mask + which
    /// handle. The concrete handle payloads are defined by the affordance modules
    /// (linear/radial); this carries the raw drag origin so the affordance can
    /// resolve the new params each frame.
    DragHandle {
        component: usize,
        handle: u32,
        origin_src: (f32, f32),
    },
}

pub struct MaskUiState {
    pub active: bool,
    pub selected: Option<usize>,
    pub tool: MaskTool,
    pub next_mode: CompositeMode,
    pub overlay_on: bool,
    pub brush_radius: f32,
    pub brush_hardness: f32,
    pub brush_flow: f32,
    pub brush_erase: bool,
    pub range_lo: f32,
    pub range_hi: f32,
    pub range_softness: f32,
    pub color_tolerance: f32,
    pub color_softness: f32,
    pub color_samples: Vec<Rgb>,
    pub gesture: Option<MaskGesture>,
    pub overlay_key: Option<u64>,
    pub rename_buf: Option<(usize, String)>,
}

impl Default for MaskUiState {
    fn default() -> Self {
        Self {
            active: false,
            selected: None,
            tool: MaskTool::default(),
            next_mode: CompositeMode::Add,
            overlay_on: true,
            brush_radius: 0.08, // fraction of the image's smaller edge
            brush_hardness: 0.5,
            brush_flow: 1.0,
            brush_erase: false,
            range_lo: 0.3,
            range_hi: 0.7,
            range_softness: 0.1,
            color_tolerance: 0.15,
            color_softness: 0.1,
            color_samples: Vec::new(),
            gesture: None,
            overlay_key: None,
            rename_buf: None,
        }
    }
}

impl MaskUiState {
    /// Keep `selected` valid against the current layer count: clamp to the last
    /// index, or clear when there are no layers.
    pub fn clamp_selection(&mut self, layer_count: usize) {
        self.selected = match (self.selected, layer_count) {
            (_, 0) => None,
            (Some(i), n) => Some(i.min(n - 1)),
            (None, _) => None,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_brush_no_selection_overlay_on() {
        let s = MaskUiState::default();
        assert!(!s.active);
        assert_eq!(s.selected, None);
        assert_eq!(s.tool, MaskTool::Brush);
        assert_eq!(s.next_mode, ferrolite_mask::CompositeMode::Add);
        assert!(s.overlay_on);
        assert!(s.gesture.is_none());
        // sane brush defaults in [0,1]-ish ranges
        assert!(s.brush_radius > 0.0 && s.brush_hardness >= 0.0 && s.brush_flow > 0.0);
    }

    #[test]
    fn clamp_selection_drops_out_of_range() {
        let mut s = MaskUiState {
            selected: Some(3),
            ..Default::default()
        };
        s.clamp_selection(2); // only indices 0,1 valid
        assert_eq!(s.selected, Some(1), "clamped to last valid index");
        s.clamp_selection(0); // no layers
        assert_eq!(s.selected, None);
        let mut s2 = MaskUiState {
            selected: Some(0),
            ..Default::default()
        };
        s2.clamp_selection(2);
        assert_eq!(s2.selected, Some(0), "in-range selection preserved");
    }
}

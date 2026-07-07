//! Per-image Masking-tool UI state + the tool enum. Pure data + tiny selection
//! helpers (unit-tested); egui rendering lives in `mask_panel`/`mask_overlay`.
//! Mirrors how `hsl_band`/`crop_active` live on `ViewerState` (survives the
//! panel's per-frame `Option` plumbing).

use ferrolite_mask::{BrushNode, CompositeMode, MaskComponent, Rgb};

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
    /// Brush stroke being captured: accumulated dab nodes (normalized source
    /// coords), plus the target `(brush_component_index, base_stroke_count)` once
    /// located/created — the live stroke is appended after the component's first
    /// `base_stroke_count` committed strokes (merge-into-active-brush; the mask's
    /// last Brush component accumulates strokes). `None` until the first dragged
    /// frame creates/locates the target.
    Stroke(Vec<BrushNode>, Option<(usize, usize)>),
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

/// The `MaskTool` that authors/edits a given component type (`None` for the
/// non-authorable `Imported` seam).
// Wired into the canvas edit-target routing in a follow-up task.
#[allow(dead_code)]
pub fn tool_for_component(c: &MaskComponent) -> Option<MaskTool> {
    match c {
        MaskComponent::Brush { .. } => Some(MaskTool::Brush),
        MaskComponent::LinearGradient { .. } => Some(MaskTool::Linear),
        MaskComponent::RadialGradient { .. } => Some(MaskTool::Radial),
        MaskComponent::LumaRange { .. } => Some(MaskTool::LumaRange),
        MaskComponent::ColorRange { .. } => Some(MaskTool::ColorRange),
        MaskComponent::Imported { .. } => None,
    }
}

/// Which component the canvas affordance for `tool` should act on: the
/// `editing` component if it exists and matches `tool`, otherwise the first
/// component matching `tool` (the create-a-fresh-one fallback). `None` if no
/// component matches.
// Wired into the canvas edit-target routing in a follow-up task.
#[allow(dead_code)]
pub fn edit_target_index(
    components: &[(MaskComponent, CompositeMode)],
    tool: MaskTool,
    editing: Option<usize>,
) -> Option<usize> {
    if let Some(i) = editing {
        if components.get(i).and_then(|(c, _)| tool_for_component(c)) == Some(tool) {
            return Some(i);
        }
    }
    components
        .iter()
        .position(|(c, _)| tool_for_component(c) == Some(tool))
}

pub struct MaskUiState {
    pub active: bool,
    pub selected: Option<usize>,
    pub tool: MaskTool,
    pub next_mode: CompositeMode,
    pub overlay_on: bool,
    /// Transient — set true by the panel while a Light+Color slider of the
    /// selected mask is being dragged; the canvas overlay hides its red fill
    /// while true so the user sees the actual effect. Reset each frame.
    pub adjusting: bool,
    pub brush_radius: f32,
    pub brush_hardness: f32,
    pub brush_flow: f32,
    pub brush_erase: bool,
    pub range_lo: f32,
    pub range_hi: f32,
    pub range_softness: f32,
    /// Wired into the radial gradient inline-edit UI in a follow-up task.
    #[allow(dead_code)]
    pub radial_feather: f32,
    /// Wired into the radial gradient inline-edit UI in a follow-up task.
    #[allow(dead_code)]
    pub radial_invert: bool,
    pub color_tolerance: f32,
    pub color_softness: f32,
    pub color_samples: Vec<Rgb>,
    /// Armed color-pick mode (the Color sub-tool's "Pick color" toggle). While true,
    /// the canvas shows a picker cursor + zoom loupe and a click samples a pixel.
    pub picking_color: bool,
    pub gesture: Option<MaskGesture>,
    pub overlay_key: Option<u64>,
    pub rename_buf: Option<(usize, String)>,
    /// The component-management modal is open for the selected mask.
    pub components_modal_open: bool,
    /// Which component index the modal is currently editing (Luma/Color), if any.
    pub editing_component: Option<usize>,
    /// While the Components window's Add section is tuning a Luma/Color component,
    /// this holds the tentative (component, mode) so the canvas overlay previews the
    /// prospective full mask. `None` = no add-preview. Reset on add/close/type change
    /// (mirrors the `components_modal_open`/`editing_component` reset sites).
    pub preview_component: Option<(MaskComponent, CompositeMode)>,
    /// Component index currently hovered in the Components modal's row list, if
    /// any. Drives both the bolded row label (modal) and a white highlight of
    /// that component's coverage drawn over the canvas (`mask_overlay`). `None`
    /// when the pointer isn't over any row / the modal is closed.
    pub highlight_component: Option<usize>,
}

impl Default for MaskUiState {
    fn default() -> Self {
        Self {
            active: false,
            selected: None,
            tool: MaskTool::default(),
            next_mode: CompositeMode::Add,
            overlay_on: true,
            adjusting: false,
            brush_radius: 0.08, // fraction of the image's smaller edge
            brush_hardness: 0.5,
            brush_flow: 1.0,
            brush_erase: false,
            range_lo: 0.3,
            range_hi: 0.7,
            range_softness: 0.1,
            radial_feather: 0.3,
            radial_invert: false,
            color_tolerance: 0.15,
            color_softness: 0.1,
            color_samples: Vec::new(),
            picking_color: false,
            gesture: None,
            overlay_key: None,
            rename_buf: None,
            components_modal_open: false,
            editing_component: None,
            preview_component: None,
            highlight_component: None,
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
        assert!(!s.adjusting);
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

    use ferrolite_mask::{CompositeMode, MaskComponent, Vec2};

    fn linear() -> MaskComponent {
        MaskComponent::LinearGradient {
            start: Vec2::new(0.0, 0.0),
            end: Vec2::new(1.0, 1.0),
        }
    }
    fn radial() -> MaskComponent {
        MaskComponent::RadialGradient {
            center: Vec2::new(0.5, 0.5),
            radius: Vec2::new(0.2, 0.2),
            rotation: 0.0,
            feather: 0.3,
            invert: false,
        }
    }
    fn brush() -> MaskComponent {
        MaskComponent::Brush { strokes: vec![] }
    }
    fn add(c: MaskComponent) -> (MaskComponent, CompositeMode) {
        (c, CompositeMode::Add)
    }

    #[test]
    fn tool_for_component_maps_every_variant() {
        assert_eq!(tool_for_component(&brush()), Some(MaskTool::Brush));
        assert_eq!(tool_for_component(&linear()), Some(MaskTool::Linear));
        assert_eq!(tool_for_component(&radial()), Some(MaskTool::Radial));
        assert_eq!(
            tool_for_component(&MaskComponent::LumaRange {
                lo: 0.0,
                hi: 1.0,
                softness: 0.0
            }),
            Some(MaskTool::LumaRange)
        );
        assert_eq!(
            tool_for_component(&MaskComponent::ColorRange {
                samples: vec![],
                tolerance: 0.1,
                softness: 0.1
            }),
            Some(MaskTool::ColorRange)
        );
        assert_eq!(
            tool_for_component(&MaskComponent::Imported {
                handle: ferrolite_mask::RasterHandle(0),
                provenance: ferrolite_mask::MaskProvenance {
                    model_id: "".into(),
                    model_version: "".into(),
                    prompt: "".into()
                },
            }),
            None
        );
    }

    #[test]
    fn edit_target_prefers_editing_when_type_matches_else_first() {
        // components: [linear#0, radial#1, linear#2]
        let comps = vec![add(linear()), add(radial()), add(linear())];
        // editing #2 (a linear) + Linear tool -> target #2 (not the first linear #0)
        assert_eq!(
            edit_target_index(&comps, MaskTool::Linear, Some(2)),
            Some(2)
        );
        // editing #1 (a radial) but Linear tool -> type mismatch -> first linear (#0)
        assert_eq!(
            edit_target_index(&comps, MaskTool::Linear, Some(1)),
            Some(0)
        );
        // no editing -> first matching
        assert_eq!(edit_target_index(&comps, MaskTool::Radial, None), Some(1));
        // editing out of range -> first matching
        assert_eq!(
            edit_target_index(&comps, MaskTool::Linear, Some(99)),
            Some(0)
        );
        // no matching component -> None
        assert_eq!(
            edit_target_index(&[add(brush())], MaskTool::Linear, None),
            None
        );
    }

    #[test]
    fn radial_edit_state_defaults() {
        let s = MaskUiState::default();
        assert_eq!(s.radial_feather, 0.3);
        assert!(!s.radial_invert);
    }
}

//! Develop module: the right adjustment panel, its interactive widgets, the
//! op-stack edit helpers + undo/redo history, and off-thread frl:ops persistence.

pub mod adjustment_panel;
// Registry core for Tasks 3-6 (the base tabs rebuilt on scoped editing).
// `SliderSpec::id` is now read outside tests too (Task 5's `EffectsTab::show`
// filters `effects_sliders()` by `id.0` prefix per section), so the module is
// fully consumed by the bin target.
pub mod adjustments;
pub mod base_tabs;
pub mod cache;
pub mod canvas;
pub mod coverage;
pub mod crop_math;
pub mod crop_overlay;
pub mod curve_math;
pub mod curve_widget;
pub mod curve_widget_parametric;
pub mod grade_widget;
pub mod histogram_widget;
pub mod history;
pub mod hsl_widget;
pub mod info;
pub mod info_overlay;
pub mod info_panel;
pub mod lens_bake;
pub mod lens_caps_ui;
pub mod lens_match;
pub mod lens_picker;
pub mod mask_affordance;
pub mod mask_components_modal;
pub mod mask_edit;
pub mod mask_overlay;
pub mod mask_overlay_color;
pub mod mask_panel;
pub mod mask_ui;
pub mod meta_read;
pub mod ops_edit;
pub mod ops_persist;
pub mod presets_menu;
pub mod preview_cache;
pub mod scope;
pub mod split;
pub mod thumb_regen;
pub mod tool;
pub mod tool_palette;
pub mod tool_panel;
pub mod tool_state;
pub mod tools;
pub mod vignette_mode;
pub mod warm_prefetch;

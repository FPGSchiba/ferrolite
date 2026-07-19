//! Develop canvas module: decouples interactive image rendering, pan/zoom calculation,
//! and canvas overlays (tool palette, floating histogram, EXIF info, crop guidelines).

pub mod overlays;
pub mod viewer;

pub use viewer::Viewer;
pub use viewer::ViewerAction;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct ViewerCanvasState {
    pub view: ferrolite_vt::ViewTransform,
    pub drag_start: Option<egui::Pos2>,
    pub crop_active_prev: bool,
}

impl Default for ViewerCanvasState {
    fn default() -> Self {
        Self {
            view: ferrolite_vt::ViewTransform {
                zoom: 1.0,
                pan: (0.0, 0.0),
            },
            drag_start: None,
            crop_active_prev: false,
        }
    }
}

//! Develop canvas module: decouples interactive image rendering, pan/zoom calculation,
//! and canvas overlays (tool palette, floating histogram, EXIF info, crop guidelines).

pub mod overlays;
pub mod viewer;

pub use viewer::Viewer;
pub use viewer::ViewerAction;

#[derive(Clone, Copy, Debug, Default)]
pub struct ViewerCanvasState {
    pub crop_active_prev: bool,
}

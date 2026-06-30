//! Single-image viewer state + pure pan/zoom input math, plus the tier-1
//! preview decode → upload → paint wiring (egui↔wgpu callback).

pub mod callback;
pub mod load;

pub use callback::{ViewerCallback, ViewerGpu};

use std::path::PathBuf;

use ferrolite_image::FileKind;
use ferrolite_jobs::JobHandle;
use ferrolite_vt::ViewTransform;

/// Preview→full crossfade duration (seconds). Short enough to read as instant,
/// long enough to avoid a hard pop.
pub const CROSSFADE_SECS: f32 = 0.15;

pub struct ViewerState {
    pub image_id: i64,
    pub path: PathBuf,
    pub kind: FileKind,
    pub view: ViewTransform,
    /// Last painted canvas size (image-space fit + zoom/pan math need it).
    pub viewport: (f32, f32),
    /// True once an `Interactive` preview decode has been submitted (one-shot).
    pub preview_requested: bool,
    /// True once the rung-1 `VirtualTexture` is uploaded and the view fitted; the
    /// VT itself lives in eframe's `callback_resources` (paint reads it there).
    pub loaded: bool,

    /// True once the tier-2 full decode has been submitted (one-shot).
    pub full_requested: bool,
    /// True once the sparse (tier-2) `VirtualTexture` is built and stored in
    /// `ViewerGpu`; the paint then drives it per frame and crossfades toward it.
    pub full_ready: bool,
    /// True while the preview→full crossfade ramp is advancing.
    pub crossfading: bool,
    /// Seconds elapsed into the active crossfade.
    pub crossfade_elapsed: f32,
    /// Terminal state: nothing more will load (preview failed AND/OR full failed,
    /// or full is ready and the crossfade is complete with no tiles pending). When
    /// set the paint loop stops requesting repaints to avoid a busy-loop.
    pub idle: bool,

    /// In-flight decode jobs (preview + full). Cancelled on navigation so a
    /// superseded image's decode does not race the newly-opened one.
    pub preview_handle: Option<JobHandle>,
    pub full_handle: Option<JobHandle>,
}

impl ViewerState {
    /// Open the viewer for the given image. The viewport size is not yet known;
    /// `ViewTransform::fit` will be called when the preview arrives.
    pub fn open(image_id: i64, path: PathBuf, kind: FileKind) -> Self {
        Self {
            image_id,
            path,
            kind,
            view: ViewTransform {
                zoom: 1.0,
                pan: (0.0, 0.0),
            },
            viewport: (0.0, 0.0),
            preview_requested: false,
            loaded: false,
            full_requested: false,
            full_ready: false,
            crossfading: false,
            crossfade_elapsed: 0.0,
            idle: false,
            preview_handle: None,
            full_handle: None,
        }
    }

    /// Begin the preview→full crossfade ramp (called when the full sparse VT
    /// becomes available).
    pub fn begin_crossfade(&mut self) {
        self.crossfading = true;
        self.crossfade_elapsed = 0.0;
    }

    /// Advance the crossfade by `dt` seconds and return the current blend factor
    /// in `[0, 1]` (0 = all preview, 1 = all full). Pure: clamps at 1.0 and, once
    /// not actively crossfading, reports 1.0 iff the full image is ready.
    pub fn tick_crossfade(&mut self, dt: f32) -> f32 {
        if !self.crossfading {
            return if self.full_ready { 1.0 } else { 0.0 };
        }
        self.crossfade_elapsed += dt;
        let factor = (self.crossfade_elapsed / CROSSFADE_SECS).clamp(0.0, 1.0);
        if factor >= 1.0 {
            self.crossfading = false;
        }
        factor
    }

    /// Cancel the in-flight decode jobs for this viewer. The sparse tile jobs
    /// are cancelled separately (they live in the `ViewerGpu` holder, owned by
    /// `callback_resources`) when that holder is dropped/replaced.
    pub fn cancel_loads(&self) {
        if let Some(h) = self.preview_handle.as_ref() {
            h.cancel();
        }
        if let Some(h) = self.full_handle.as_ref() {
            h.cancel();
        }
    }
}

/// Zoom about the cursor: keep the image point under the cursor fixed.
///
/// `pan` follows the `ViewTransform` convention: image-space px offset of the
/// viewport center (i.e. `cx = image_width/2 + pan.0` etc.).  The test
/// invariant is that the image point under the cursor does not move on screen.
// Called by the viewer input handler in Task 14.
#[allow(dead_code)]
pub fn apply_zoom(
    view: ViewTransform,
    scroll: f32,
    cursor: (f32, f32),
    viewport: (f32, f32),
) -> ViewTransform {
    let factor = (1.0 + scroll * 0.1).max(0.05);
    let new_zoom = (view.zoom * factor).clamp(0.01, 64.0);

    // The viewport-center in image space before zooming.
    let center = (view.pan.0 + viewport.0 * 0.5, view.pan.1 + viewport.1 * 0.5);
    // The image-space point currently under the cursor.
    let img_pt = (
        center.0 + (cursor.0 - viewport.0 * 0.5) / view.zoom,
        center.1 + (cursor.1 - viewport.1 * 0.5) / view.zoom,
    );
    // New viewport center so img_pt maps back to the same cursor position.
    let new_center = (
        img_pt.0 - (cursor.0 - viewport.0 * 0.5) / new_zoom,
        img_pt.1 - (cursor.1 - viewport.1 * 0.5) / new_zoom,
    );
    ViewTransform {
        zoom: new_zoom,
        // pan = center - viewport/2  (center = image origin + pan)
        pan: (
            new_center.0 - viewport.0 * 0.5,
            new_center.1 - viewport.1 * 0.5,
        ),
    }
}

/// Pan by a screen-space drag delta.
///
/// Dragging the image to the right means the viewport center moves left in
/// image space, so pan decreases.  Dividing by zoom converts screen px to
/// image px.
// Called by the viewer input handler in Task 14.
#[allow(dead_code)]
pub fn apply_pan(view: ViewTransform, drag_delta: (f32, f32)) -> ViewTransform {
    ViewTransform {
        zoom: view.zoom,
        pan: (
            view.pan.0 - drag_delta.0 / view.zoom,
            view.pan.1 - drag_delta.1 / view.zoom,
        ),
    }
}

/// Paint the viewer's central canvas: fill black, read scroll/drag input into
/// the view transform, record the viewport size, and (once the rung-1 preview
/// texture is loaded) enqueue the egui↔wgpu paint callback. `show_full` selects
/// the sparse full-res VT over the preview (swap-on-ready crossfade). Returns
/// `true` while the preview is still loading so the caller can `request_repaint`
/// for a prompt first pixel.
pub fn paint(ui: &mut egui::Ui, state: &mut ViewerState, show_full: bool) -> bool {
    let rect = ui.available_rect_before_wrap();
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::BLACK);

    let viewport = (rect.width(), rect.height());
    state.viewport = viewport;

    // Pointer interaction over the canvas: drag pans, scroll zooms about cursor.
    let resp = ui.interact(
        rect,
        ui.id().with(("viewer-canvas", state.image_id)),
        egui::Sense::click_and_drag(),
    );
    if state.loaded {
        if resp.dragged() {
            let d = resp.drag_delta();
            state.view = apply_pan(state.view, (d.x, d.y));
            // The view moved: new tiles may be needed, so wake the drive loop.
            state.idle = false;
        }
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > f32::EPSILON {
            if let Some(pos) = resp.hover_pos() {
                let cursor = (pos.x - rect.left(), pos.y - rect.top());
                // Normalize wheel notches (~50px) into the apply_zoom step scale.
                state.view = apply_zoom(state.view, scroll / 50.0, cursor, viewport);
                // Zoom changes the visible LOD/tiles: wake the drive loop.
                state.idle = false;
            }
        }
    }

    if state.loaded {
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewerCallback {
                image_id: state.image_id,
                view: state.view,
                viewport,
                show_full,
            },
        ));
        false
    } else {
        // Texture not ready yet — keep animating so the first pixel arrives fast.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_vt::ViewTransform;

    #[test]
    fn crossfade_ramps_zero_to_one_then_clamps() {
        let mut v = ViewerState::open(1, std::path::PathBuf::from("x"), FileKind::Raw);
        v.begin_crossfade();
        assert_eq!(v.tick_crossfade(0.0), 0.0);
        let mid = v.tick_crossfade(0.075); // half of 150ms
        assert!(mid > 0.4 && mid < 0.6, "about halfway");
        let done = v.tick_crossfade(1.0); // way past
        assert_eq!(done, 1.0, "clamps at 1.0");
    }

    #[test]
    fn crossfade_idle_reports_full_readiness() {
        let mut v = ViewerState::open(2, std::path::PathBuf::from("y"), FileKind::Raw);
        // Not crossfading and full not ready => 0.0 (show preview).
        assert_eq!(v.tick_crossfade(0.5), 0.0);
        // Full ready but crossfade finished => 1.0 (show full).
        v.full_ready = true;
        assert_eq!(v.tick_crossfade(0.5), 1.0);
    }

    #[test]
    fn zoom_keeps_cursor_point_stationary() {
        let v = ViewTransform {
            zoom: 1.0,
            pan: (0.0, 0.0),
        };
        let viewport = (100.0, 100.0);
        let cursor = (75.0, 50.0); // right of center
        let z = apply_zoom(v, 1.0, cursor, viewport); // scroll up = zoom in
        assert!(z.zoom > 1.0, "scroll up zooms in");
        // Cursor is right of viewport center (75 > 50), so to keep the image
        // point under the cursor fixed, the viewport center must shift right in
        // image space (pan.0 increases).
        assert!(z.pan.0 > 0.0, "zoom about off-center cursor pans toward it");
    }

    #[test]
    fn pan_translates_by_delta_over_zoom() {
        let v = ViewTransform {
            zoom: 2.0,
            pan: (0.0, 0.0),
        };
        // Drag right 20 px, up 10 px in screen space.
        // pan.0 -= 20/2 = -10; pan.1 -= (-10)/2 = +5
        let p = apply_pan(v, (20.0, -10.0));
        assert!(
            (p.pan.0 + 10.0).abs() < 1e-6,
            "screen delta / zoom, inverted for pan"
        );
        assert!((p.pan.1 - 5.0).abs() < 1e-6);
    }
}

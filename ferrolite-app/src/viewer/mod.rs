//! Single-image viewer state + pure pan/zoom input math. The two-tier load and
//! GPU wiring are layered on in later tasks.

use std::path::PathBuf;

use ferrolite_image::FileKind;
use ferrolite_vt::ViewTransform;

// Fields are read by Tasks 14–15 (preview decode + GPU wiring).
#[allow(dead_code)]
pub struct ViewerState {
    pub image_id: i64,
    pub path: PathBuf,
    pub kind: FileKind,
    pub view: ViewTransform,
}

impl ViewerState {
    /// Open the viewer for the given image. The viewport size is not yet known;
    /// `ViewTransform::fit` will be called on the first paint frame.
    pub fn open(image_id: i64, path: PathBuf, kind: FileKind) -> Self {
        Self {
            image_id,
            path,
            kind,
            view: ViewTransform {
                zoom: 1.0,
                pan: (0.0, 0.0),
            },
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

/// Stub paint: clears the canvas area. Real GPU wiring lands in Task 14.
pub fn paint(ui: &mut egui::Ui) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_vt::ViewTransform;

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

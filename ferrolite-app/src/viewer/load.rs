//! Two-tier viewer load: tier-1 embedded preview (this task) and tier-2 full
//! decode → demosaic → VT (Task 15). All decode work runs on the job system.

use std::path::PathBuf;

use ferrolite_decode::{DemosaicToRgb16f, QuadBin};
use ferrolite_image::{FileKind, ImageBuffer, LinearRgbaF32, PixelFormat};
use ferrolite_jobs::{JobHandle, JobSystem, Priority};

use crate::events::AppEvent;

/// sRGB-encoded 8-bit preview → display-linear RGBA f32.
pub fn preview_to_linear(buf: &ImageBuffer) -> LinearRgbaF32 {
    let ch = buf.format.channels();
    let n = (buf.width * buf.height) as usize;
    let mut px = Vec::with_capacity(n * 4);
    let srgb_to_lin = |u: u8| -> f32 {
        let c = u as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    for i in 0..n {
        let base = i * ch;
        let r = srgb_to_lin(buf.pixels[base]);
        let g = srgb_to_lin(buf.pixels[base + 1]);
        let b = srgb_to_lin(buf.pixels[base + 2]);
        let a = if matches!(buf.format, PixelFormat::Rgba8) {
            buf.pixels[base + 3] as f32 / 255.0
        } else {
            1.0
        };
        px.extend_from_slice(&[r, g, b, a]);
    }
    LinearRgbaF32::new(buf.width, buf.height, px).expect("preview length")
}

/// Submit an `Interactive` decode-preview job. On success it sends an
/// `AppEvent::PreviewReady { image_id, image }` and requests a repaint so the
/// UI thread picks up the decoded preview and uploads it as a rung-1 texture.
pub fn spawn_preview(
    jobs: &std::sync::Arc<JobSystem>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    path: PathBuf,
    kind: FileKind,
) -> JobHandle {
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Interactive, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        match ferrolite_decode::decode_preview(&path, kind) {
            Ok(image) => {
                let _ = tx.send(AppEvent::PreviewReady { image_id, image });
            }
            Err(e) => {
                eprintln!("ferrolite: preview decode failed for #{image_id}: {e}");
            }
        }
        ctx.request_repaint();
    })
}

/// Submit a `Visible`-priority tier-2 full-decode job: full RAW decode →
/// `QuadBin.to_linear_rgba_f32` (display-linear half-res), then send
/// `AppEvent::FullDecoded { image_id, image }`. On decode error sends
/// `AppEvent::FullFailed { image_id }` and logs.
///
/// Priority is `Visible`, not `Interactive`: the full decode is not
/// latency-critical (the tier-1 preview is already on screen), and running it
/// at top priority on the single strict-priority worker pool starved thumbnail
/// jobs (also `Visible`) — see CLAUDE.md responsiveness rules. The caller in
/// `app.rs` additionally debounces submission so fast navigation doesn't pile
/// up full-RAW decodes for images the user has already navigated past.
///
/// RAW-only: `decode_full` decodes via rawler, which has no full path for
/// Standard/JPEG images. For a Standard image the tier-1 preview already IS the
/// full-resolution image, so tier-2 is skipped entirely (the caller guards on
/// `kind == FileKind::Raw`).
pub fn spawn_full(
    jobs: &std::sync::Arc<JobSystem>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    path: PathBuf,
) -> JobHandle {
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Visible, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        match ferrolite_decode::decode_full(&path) {
            Ok(raw) => {
                if cancel.is_cancelled() {
                    return;
                }
                let color_profile = raw.color_profile.clone();
                let image = QuadBin.to_linear_rgba_f32(&raw);
                let _ = tx.send(AppEvent::FullDecoded {
                    image_id,
                    image,
                    color_profile,
                });
            }
            Err(e) => {
                eprintln!("ferrolite: full decode failed for #{image_id}: {e}");
                let _ = tx.send(AppEvent::FullFailed { image_id });
            }
        }
        ctx.request_repaint();
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_image::{ImageBuffer, PixelFormat};

    #[test]
    fn srgb8_to_linear_inverts_gamma() {
        // mid-gray 188/255 sRGB ~= 0.5 linear.
        let buf = ImageBuffer::new(1, 1, PixelFormat::Rgb8, vec![188, 188, 188]).unwrap();
        let lin = preview_to_linear(&buf);
        assert_eq!((lin.width, lin.height), (1, 1));
        assert!((lin.pixels[0] - 0.5).abs() < 0.02, "sRGB decode ~0.5");
        assert!((lin.pixels[3] - 1.0).abs() < 1e-6, "alpha opaque");
    }
}

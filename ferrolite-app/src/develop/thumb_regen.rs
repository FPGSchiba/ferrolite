//! Headless edited-thumbnail regeneration: render an image's persisted edit
//! stack at preview resolution through the real `EditPipeline`, downscale to a
//! grid thumbnail, persist it, and publish it via `AppEvent::ThumbReady`.
//!
//! All decode / GPU-render / readback / encode / DB-write runs inside a
//! `ferrolite-jobs` Background job (CLAUDE.md responsiveness rules); only the
//! existing per-frame `ThumbReady` upload touches the UI thread.
//!
//! The Develop-leave transitions (`app.rs`) and the context-menu on-demand
//! action wire `spawn_regen_edited_thumbnail` in; it renders the persisted edit
//! stack — including lens corrections — so a grid thumbnail matches the edit.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ferrolite_catalog::{
    generate_thumbnail, read_ops, sidecar_path, Catalog, DecodedThumb, ThumbnailStore,
};
use ferrolite_color::WorkingSpace;
use ferrolite_decode::{decode_preview, ColorProfile};
use ferrolite_gpu::GpuContext;
use ferrolite_image::{FileKind, ImageBuffer, PixelFormat};
use ferrolite_jobs::{JobSystem, Priority};
use ferrolite_lens::LensfunDb;
use ferrolite_pipeline::{
    bake_products, blit_to_rgba8, deserialize, lens_uniform, vignette_amount, EditPipeline,
    OpStack, VignetteTexture, WarpGridTexture,
};

use crate::events::AppEvent;
use crate::viewer::load::preview_to_linear;

/// Gate for the auto-on-leave trigger: regenerate only when the edit stack
/// changed during this Develop session. Merely viewing never regenerates.
pub fn should_regenerate_on_leave(edits_dirty: bool) -> bool {
    edits_dirty
}

/// Resolve the stack for the on-demand path from a sidecar `frl:ops` payload.
/// Missing (`None`) or malformed (`deserialize` → `None`) → identity.
pub fn resolve_regen_stack(payload: Option<String>) -> OpStack {
    payload.and_then(|p| deserialize(&p)).unwrap_or_default()
}

/// camera→working for a preview-resolution (sRGB-primaries) source under the
/// current working space. The embedded preview is already sRGB for BOTH RAW and
/// Standard, so we use the sRGB-fallback profile (no `normalize_neutral`) — this
/// matches `export/batch.rs`'s Standard branch and the ingest thumbnail source.
pub fn srgb_fallback_camera_to_working(working_space: WorkingSpace) -> [[f32; 3]; 3] {
    let p = ColorProfile::srgb_fallback();
    ferrolite_color::camera_to_working(
        p.xyz_to_cam,
        ferrolite_color::Xy {
            x: p.white_xy[0],
            y: p.white_xy[1],
        },
        working_space,
    )
}

/// Blocking regenerate: decode the preview → apply the edit stack via the real
/// `EditPipeline` → read back RGBA8 → downscale to a grid thumbnail → persist.
/// Returns the decoded thumbnail pixels for the `ThumbReady` upload.
///
/// GPU + I/O; MUST run inside a Background job. `EditPipeline` is built and
/// dropped here (it is not `Send`). Any failure returns `Err(String)` so the
/// caller logs and keeps the existing thumbnail — never panics.
#[allow(clippy::too_many_arguments)]
pub fn regenerate_edited_thumbnail_blocking(
    writer: &Arc<Mutex<Catalog>>,
    gpu: &Arc<GpuContext>,
    lens_db: Option<&Arc<LensfunDb>>,
    image_id: i64,
    path: &Path,
    kind: FileKind,
    stack: OpStack,
    camera_to_working: [[f32; 3]; 3],
) -> Result<DecodedThumb, String> {
    // 1. Preview-resolution source (both kinds), sRGB → display-linear.
    let preview = decode_preview(path, kind).map_err(|e| format!("decode_preview: {e}"))?;
    let linear = preview_to_linear(&preview);

    // 2. Render the edit stack. Read output dims from the evaluated image — a
    //    crop/geometry op changes them.
    let mut pipeline =
        EditPipeline::new(Arc::clone(gpu), &linear, stack.clone(), camera_to_working);

    // 2b. Apply lens corrections so the thumbnail matches the edit (as crop/
    //     rotate already do). Bake off-thread here (inside the Background job)
    //     via the shared `bake_products` primitive — never on the UI thread
    //     (CLAUDE.md §1). Skipped entirely when there's no db, no enabled
    //     correction, or no matched lens (identity → byte-identical to no bake).
    if let (Some(db), Some(lc)) = (lens_db, stack.lens_correction()) {
        if lc.lens_id.is_some()
            && (lc.distortion.enabled || lc.tca.enabled || lc.vignetting.enabled)
        {
            let (warp, vignette) = bake_products(db.as_ref(), &lc);
            if let Some(w) = warp.as_ref() {
                pipeline.set_warp(WarpGridTexture::upload(gpu, w));
            }
            pipeline.set_lens_uniform(lens_uniform(Some(&lc), warp.is_some()));
            if let Some(v) = vignette.as_ref() {
                pipeline.set_vignette(VignetteTexture::upload(gpu, v));
            }
            pipeline.set_vig_amount(vignette_amount(Some(&lc)));
        }
    }

    let out = pipeline.evaluate();
    let (w, h) = (out.width, out.height);
    let rgba = blit_to_rgba8(gpu, &out); // RGBA8 sRGB, row-unpadded, len w*h*4

    // 3. Downscale + JPEG-encode via the existing thumbnail path.
    let edited = ImageBuffer::new(w, h, PixelFormat::Rgba8, rgba)
        .map_err(|e| format!("edited buffer: {e}"))?;
    let (thumb, decoded) =
        generate_thumbnail(&edited).map_err(|e| format!("generate_thumbnail: {e}"))?;

    // 4. Persist (cache-safe single-writer lock, released immediately).
    {
        let db = writer.lock().expect("writer");
        db.put_thumbnail(image_id, &thumb)
            .map_err(|e| format!("put_thumbnail: {e}"))?;
    }

    Ok(decoded)
}

/// Where the job obtains the edit stack.
pub enum RegenStackSource {
    /// From the just-closed viewer (auto-on-leave) — no sidecar read.
    InMemory(Box<OpStack>),
    /// Read the `.xmp` sidecar inside the job (on-demand catch-up); missing or
    /// malformed → identity.
    Sidecar,
}

/// Submit a Background regen job. On success it emits `AppEvent::ThumbReady`
/// (the existing grid texture-swap signal); on failure it logs and keeps the
/// existing thumbnail. Always requests a repaint so the UI drains the event.
#[allow(clippy::too_many_arguments)]
pub fn spawn_regen_edited_thumbnail(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    egui_ctx: &egui::Context,
    gpu: Arc<GpuContext>,
    lens_db: Option<Arc<LensfunDb>>,
    image_id: i64,
    path: PathBuf,
    kind: FileKind,
    camera_to_working: [[f32; 3]; 3],
    stack_source: RegenStackSource,
) {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let egui_ctx = egui_ctx.clone();
    jobs.submit(Priority::Background, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        let stack = match stack_source {
            RegenStackSource::InMemory(s) => *s,
            RegenStackSource::Sidecar => resolve_regen_stack(read_ops(&sidecar_path(&path))),
        };
        match regenerate_edited_thumbnail_blocking(
            &writer,
            &gpu,
            lens_db.as_ref(),
            image_id,
            &path,
            kind,
            stack,
            camera_to_working,
        ) {
            Ok(decoded) => {
                let _ = tx.send(AppEvent::ThumbReady {
                    image_id,
                    rgba: decoded.rgba,
                    w: decoded.w,
                    h: decoded.h,
                });
            }
            Err(e) => {
                eprintln!("ferrolite: edited-thumbnail regen failed for #{image_id}: {e}");
            }
        }
        egui_ctx.request_repaint();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{serialize, OpStack};

    #[test]
    fn regenerates_only_when_edits_dirty() {
        assert!(should_regenerate_on_leave(true));
        assert!(!should_regenerate_on_leave(false));
    }

    #[test]
    fn missing_sidecar_resolves_to_identity() {
        assert!(resolve_regen_stack(None).is_identity());
    }

    #[test]
    fn malformed_sidecar_resolves_to_identity() {
        assert!(resolve_regen_stack(Some("not json".to_string())).is_identity());
    }

    #[test]
    fn valid_sidecar_payload_round_trips() {
        let stack = OpStack::default().set_op(ferrolite_pipeline::Op::Exposure(
            ferrolite_pipeline::Exposure { ev: 1.0 },
        ));
        let payload = serialize(&stack);
        let resolved = resolve_regen_stack(Some(payload));
        assert!(!resolved.is_identity());
        assert_eq!(resolved, stack);
    }

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn srgb_fallback_matrix_is_near_identity_for_srgb_working_space() {
        let m = srgb_fallback_camera_to_working(ferrolite_color::WorkingSpace::Srgb);
        // sRGB source → sRGB working space is (numerically) the identity map.
        for r in 0..3 {
            for c in 0..3 {
                let expected = if r == c { 1.0 } else { 0.0 };
                assert!(
                    (m[r][c] - expected).abs() < 1e-3,
                    "m[{r}][{c}] = {} not ~{expected}",
                    m[r][c]
                );
            }
        }
    }
}

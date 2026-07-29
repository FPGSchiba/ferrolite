//! The export orchestrator: render (tiled) → optional resize → encode (+ICC) →
//! copy EXIF. Called from a ferrolite-jobs Background closure (spec §8.1). All
//! GPU work uses the passed shared `Arc<GpuContext>` on the worker thread; the
//! pipeline is built and dropped inside `render_tiled` (it is !Send).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferrolite_color::WorkingSpace;
use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_jobs::CancelToken;
use ferrolite_lens::LensfunDb;
use ferrolite_pipeline::{GpuPyramidSource, OpStack};

use crate::encode::encode_to_file;
use crate::error::ExportError;
use crate::metadata::copy_exif;
use crate::options::ExportOptions;
use crate::render::{render_tiled, PixelData, RenderedImage};
use crate::resize::{apply_resize, resize_dims};

pub struct ExportRequest<'a> {
    pub ctx: &'a Arc<GpuContext>,
    pub pyramid: &'a Arc<GpuPyramidSource>,
    pub stack: &'a OpStack,
    /// Row-major camera→working 3×3 for the open image + working space.
    pub camera_to_working: [[f32; 3]; 3],
    pub working_space: WorkingSpace,
    /// Shared lens database. When present AND the stack carries an enabled lens
    /// correction with a matched `lens_id`, the export bakes the correction
    /// products off-thread (inside this job) and renders them; otherwise the
    /// render is identity (byte-identical to an uncorrected export). `None` for
    /// batch/thumbnail-less callers or when no db is loaded.
    pub lens_db: Option<&'a Arc<LensfunDb>>,
    pub options: &'a ExportOptions,
    pub dest: &'a Path,
    /// Source image path for EXIF copy.
    pub source_path: &'a Path,
    /// Whole-image dehaze atmospheric light `A`, computed by the caller from the
    /// decoded source (design §5.3 — `A` is a whole-image constant, not per-tile).
    /// Use `ferrolite_pipeline::DEHAZE_ATMOS_NEUTRAL` when the caller has no
    /// dehaze (or hasn't computed `A` yet).
    pub atmospheric_light: [f32; 3],
    /// CPU preview-resolution source to build this export's OWN bounded
    /// whole-image dehaze transmission from (ST-Task 5). `render_tiled` never
    /// samples the live preview `EditPipeline`'s transmission texture directly —
    /// export runs in a background job while the user may keep editing, and that
    /// texture's contents get overwritten on the next preview evaluate (a race).
    /// `None` when the stack has no active dehaze anywhere — global op OR any
    /// visible mask layer's amount (Phase 4 Task 3, see
    /// `EditDoc::dehaze_active_anywhere`) — or no preview source has decoded
    /// yet; the tiled recovery then stays a passthrough.
    pub transmission_source: Option<&'a LinearRgbaF32>,
}

#[derive(Debug, Clone)]
pub struct ExportOutcome {
    pub dest: PathBuf,
    pub warnings: Vec<String>,
}

/// Render, resize, encode, and copy metadata for one image. `progress(done,total)`
/// reports tile progress during the render phase.
pub fn run_export(
    req: ExportRequest,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(u32, u32),
) -> Result<ExportOutcome, ExportError> {
    let opts = req.options;
    let depth = opts.effective_bit_depth();

    // 1. Tiled full-res render → quantized output-space RGB.
    let mut rendered = render_tiled(
        req.ctx,
        req.pyramid,
        req.stack,
        req.camera_to_working,
        req.working_space,
        opts.output_space,
        req.lens_db,
        depth,
        req.atmospheric_light,
        req.transmission_source,
        cancel,
        progress,
    )?;

    if cancel.is_cancelled() {
        return Err(ExportError::Cancelled);
    }

    // 2. Optional resize (on the quantized RGB buffer).
    let (tw, th) = resize_dims(opts.resize, rendered.width, rendered.height);
    if (tw, th) != (rendered.width, rendered.height) {
        let resized = match &rendered.data {
            PixelData::Eight(v) => {
                let out = apply_resize(v, rendered.width, rendered.height, tw, th, depth)?;
                PixelData::Eight(out)
            }
            PixelData::Sixteen(v) => {
                let bytes = bytemuck::cast_slice::<u16, u8>(v);
                let out = apply_resize(bytes, rendered.width, rendered.height, tw, th, depth)?;
                PixelData::Sixteen(bytemuck::cast_slice::<u8, u16>(&out).to_vec())
            }
        };
        rendered = RenderedImage {
            width: tw,
            height: th,
            data: resized,
        };
    }

    // 3. Encode (+ ICC embed). Collect non-fatal warnings.
    let mut warnings = encode_to_file(&rendered, opts, req.dest)?;

    // 4. Copy EXIF (unless stripping). Best-effort.
    if opts.copy_exif && !opts.strip_metadata {
        if let Err(msg) = copy_exif(req.source_path, req.dest) {
            warnings.push(format!("EXIF not copied: {msg}"));
        }
    }

    Ok(ExportOutcome {
        dest: req.dest.to_path_buf(),
        warnings,
    })
}

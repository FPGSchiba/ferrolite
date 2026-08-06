//! Domain events flowing from job threads back to the UI thread over an
//! app-owned channel. `apply` folds an event into `AppState`'s counters; it is
//! pure w.r.t. egui so it can be unit-tested.

use crate::state::AppState;

pub enum AppEvent {
    /// `added` stat-only placeholder rows were inserted by the instant index pass
    /// (grid shows the filenames immediately; metadata/thumbnails stream in after).
    Scanned { added: usize },
    /// `added` rows were indexed (status-bar "N indexed").
    Indexed { added: usize },
    /// A thumbnail finished: decoded RGBA8 pixels (tightly packed, len = w*h*4)
    /// ready for direct texture upload (NO UI-thread JPEG decode).
    ThumbReady {
        image_id: i64,
        rgba: Vec<u8>,
        w: u32,
        h: u32,
    },
    /// A thumbnail (or its decode) failed; the cell shows a broken placeholder.
    ThumbFailed { image_id: i64 },
    /// A lazy-load job found no thumbnail blob yet (`Ok(None)`), distinct from
    /// a hard decode failure. Sticky guard: `request_thumbnail` will not
    /// re-spawn a job for this id until `ThumbReady` (generated) or
    /// `IngestDone` (retry once) clears it — prevents a per-frame re-spawn
    /// storm for not-yet-ingested cells.
    ThumbMissing { image_id: i64 },
    /// The producer has determined how many files this ingest pass will actually
    /// process (after the needs-reingest filter). Sets the ingest-progress
    /// denominator once; the consumer advances `ingest_done` per completed row.
    IngestPlanned { total: usize },
    /// The ingest walk + row upserts completed.
    IngestDone,
    /// A viewer tier-1 embedded preview finished decoding off-thread. Carries the
    /// display-linear RGBA f32 buffer (sRGB→linear conversion already done on the
    /// job thread) for upload as a rung-1 `VirtualTexture`. Handled directly in
    /// `app.rs` (needs the GPU render state), not folded by `apply`.
    PreviewReady {
        image_id: i64,
        linear: ferrolite_image::LinearRgbaF32,
    },
    /// A viewer tier-2 full RAW decode + quad-bin finished off-thread. Carries the
    /// display-linear RGBA f32 image for upload as a sparse `VirtualTexture`.
    /// Handled directly in `app.rs` (needs the GPU render state), not folded by
    /// `apply`.
    FullDecoded {
        image_id: i64,
        image: ferrolite_image::LinearRgbaF32,
        color_profile: ferrolite_decode::ColorProfile,
    },
    /// Both full-res pyramids finished building off-thread (tier-2 open path):
    /// the sparse-VT CPU tile source and the GPU-resident edit pyramid. Installed
    /// on the UI thread (needs render state + the `Rc`-based tile pipeline).
    /// Emitted by the Background job submitted from `apply_full_decoded`, handled
    /// in `app.rs` (`apply_pyramid_ready`); not folded by `apply`.
    PyramidReady {
        image_id: i64,
        tile_source: std::sync::Arc<dyn ferrolite_vt::TileSource + Send + Sync>,
        gpu_pyramid: std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>,
    },
    /// The tier-2 full decode failed; the viewer keeps showing the preview and
    /// goes idle. Folded by `apply` (no GPU work) but matched in `app.rs`.
    FullFailed { image_id: i64 },
    /// Result of an off-thread metadata persist. `ok==false` → reload truth;
    /// `warning` → surface in the status bar.
    MetadataResult { ok: bool, warning: Option<String> },
    /// An off-thread frl:ops sidecar read finished. Carries the hydrated stack
    /// (default = unedited). Handled in `app.rs` (needs GPU state), not folded.
    #[allow(dead_code)] // constructed in ops_persist; handled in app.rs (Task 9)
    OpsLoaded {
        image_id: i64,
        stack: ferrolite_pipeline::OpStack,
    },
    /// Result of an off-thread frl:ops persist (sidecar + catalog `has_edits`).
    /// Distinct from `MetadataResult` (rating/flag/tag path) so the save-state
    /// indicator can track ops-persist inflight count and failure separately.
    OpsSaved { ok: bool, warning: Option<String> },
    /// An off-thread (async `map_async`) histogram readback finished: 1024 bins
    /// (256 × {R,G,B,luma}). Handled in `app.rs` (stores into the viewer); the
    /// `apply` fold ignores it.
    HistogramReady { image_id: i64, bins: Vec<u32> },
    /// An off-thread preview-cache write-back finished (the identity render for
    /// `image_id` was encoded + stored). Emitted for metrics/tests; the `apply`
    /// fold is a no-op (the job already requested a repaint). `image_id` is
    /// reserved for a future per-image cache indicator (same pattern as
    /// `OpsLoaded`/`ExportFinished`).
    PreviewCacheWritten {
        #[allow(dead_code)]
        image_id: i64,
    },
    /// A preview-cache READ (Task 6) resolved to a HIT: the cached JPEG for
    /// `image_id` was found and decoded off-thread to `linear` (display-linear
    /// RGBA f32, sRGB→linear already done on the job thread). Handled in `app.rs`
    /// (needs GPU state) to reveal via the Improvement-1 sRGB path, skipping the
    /// RAW pixel decode; not folded by `apply`.
    PreviewCacheHit {
        image_id: i64,
        linear: ferrolite_image::LinearRgbaF32,
    },
    /// A preview-cache READ (Task 6) resolved to a MISS (no entry, or a read/
    /// decode error). Handled in `app.rs`: the full-decode path then runs and
    /// (Task 5) caches its result. Not folded by `apply`.
    PreviewCacheMiss { image_id: i64 },
    /// A warm-neighbor SOURCE finished decoding off-thread
    /// (`develop::warm_prefetch::spawn_warm_sources`, Task 7): the demosaiced
    /// RAW / decoded Standard pixel source for a forward-biased filmstrip
    /// neighbor, its persisted op stack, and enough context (`kind`,
    /// `color_profile`) to render it exactly like a real open. `apply` queues
    /// this as a `WarmSourcePayload` onto `AppState.warm_render_queue`;
    /// `FerroliteApp::drain_one_warm_render` (Task 8) pops one per frame on
    /// the render thread and turns it into a cached display texture.
    WarmSourceReady {
        image_id: i64,
        source: std::sync::Arc<ferrolite_image::LinearRgbaF32>,
        op_stack: ferrolite_pipeline::OpStack,
        kind: ferrolite_image::FileKind,
        color_profile: ferrolite_decode::ColorProfile,
    },
    /// Tile progress for the running single-file export.
    ExportProgress {
        // The single active `ExportActivity` (there is only ever one running
        // export) is updated regardless of which image is open, so this is
        // discarded (`image_id: _`) at the one call site — same pattern as
        // `ExportFinished`/`BatchItemFinished`.
        #[allow(dead_code)]
        image_id: i64,
        done: u32,
        total: u32,
    },
    /// A batch export started a new image (carries the output file basename for
    /// the status-bar indicator's "current file"). Single export sets its name at
    /// spawn, so it does not emit this.
    ExportItemStarted { name: String },
    /// The single-file export finished (ok=false → failed/cancelled). `message`
    /// is the status-bar text (success path, warnings, or the error).
    ExportFinished {
        // Reserved for a future per-image status indicator; currently only
        // `ok`/`message` drive the (global) status bar, so this is discarded
        // (`image_id: _`) at the one call site — same pattern as `OpsLoaded`.
        #[allow(dead_code)]
        image_id: i64,
        ok: bool,
        message: String,
    },
    /// One image of a running batch export finished (ok=false → failed/cancelled).
    /// Folded by `apply` into the aggregate `ExportActivity` counters.
    BatchItemFinished {
        // Reserved for a future per-image status indicator in the queue list
        // (Task 7); the aggregate fold below only needs `ok`/`message`.
        #[allow(dead_code)]
        image_id: i64,
        ok: bool,
        message: String,
    },
    /// A display-profile detect+parse+bake job finished. `lut = Some` → the
    /// monitor-managed LUT path; `None` → sRGB fallback. `generation` guards
    /// against stale results from superseded re-detects. Handled in `app.rs`
    /// (needs GPU state); the `apply` fold ignores it.
    DisplayProfileResolved {
        lut: Option<ferrolite_color::DisplayLut>,
        name: String,
        generation: u64,
    },
    /// An off-thread lens-correction bake (`lens_bake::spawn_lens_bake`) finished:
    /// the warp grid + vignette map for the image's current `LensCorrection`
    /// (or all-`None` when unmatched). Handled in `app.rs` (needs GPU state to
    /// upload textures + rebuild the tile producer); guarded there on
    /// `image_id == current` so a bake superseded by navigation is dropped. The
    /// `apply` fold ignores it (no counters to update).
    LensBaked {
        image_id: i64,
        result: crate::develop::lens_bake::LensBakeResult,
    },
    /// An off-thread EXIF metadata read (`develop::meta_read::spawn_meta_read`)
    /// finished on Develop open. `meta = None` on a decode error — the panel
    /// then falls back to its constant defaults. Handled in `app.rs` (drives
    /// the cheap in-memory auto-match against `state.lens_db`); the `apply`
    /// fold ignores it (no counters to update).
    MetaLoaded {
        image_id: i64,
        meta: Option<ferrolite_decode::Metadata>,
    },
    /// A general-purpose user notification (toast). Raised from job threads over
    /// the event channel; folded by `apply` into `AppState.notifications`.
    Notify {
        level: crate::notifications::Level,
        message: String,
    },
    /// One Task-14 background-backfill batch finished off-thread
    /// (`library::meta_backfill::spawn_meta_backfill`): up to `BATCH_SIZE`
    /// resolved `lens`/`aperture`/`focal_length` reads for pre-v7-ingest rows.
    /// Unlike other metadata writes (`metadata::spawn_metadata_write`, which
    /// locks the writer from the job thread), this catalog write happens HERE
    /// in `apply`, on the UI thread: it ties exactly one `state.dirty` bump
    /// to exactly one delivered batch, so an active metadata range/lens
    /// filter refreshes once per batch as the backfill progresses.
    MetaBackfillReady {
        results: Vec<ferrolite_catalog::BackfillResult>,
    },
    /// A batch preset/paste apply finished. `snapshot` is `None` when the batch
    /// exceeded `BATCH_UNDO_MAX` (see `presets::apply`), in which case undo is
    /// not offered. `label` names the applied patch for the toast. `cancelled`
    /// is `true` when the run was cut short by its cancel token: the remaining,
    /// unattempted targets are folded into `result.skipped` by
    /// `apply_patch_to_targets`, which is indistinguishable from "sidecar
    /// unreadable" at the count level — `cancelled` lets the toast phrase that
    /// case as a cancellation instead of implying N corrupt files. `snapshot`
    /// is read by `apply()` below (pushed into `AppState.batch_undo`).
    BatchApplyDone {
        result: crate::presets::apply::BatchResult,
        snapshot: Option<crate::presets::apply::UndoSnapshot>,
        label: String,
        cancelled: bool,
    },
    /// Progress within a batch apply.
    BatchApplyProgress { done: usize, total: usize },
    /// A batch UNDO (`spawn_batch_undo`) finished restoring a snapshot's
    /// prior documents. Deliberately a DISTINCT variant from
    /// `BatchApplyDone`, not a reuse with `snapshot: None`: both funnel
    /// through the same `AppState.batch_undo` slot, and if a batch apply's
    /// `BatchApplyDone` (which just populated a fresh snapshot) raced an
    /// in-flight undo's completion, reusing `BatchApplyDone` would let
    /// whichever event folds last unconditionally overwrite `batch_undo` —
    /// silently clearing a freshly-promised "Press Ctrl+Z to undo." toast's
    /// snapshot if the undo happened to land after it. A separate variant
    /// means the undo path only ever clears `batch_undo` for the snapshot
    /// IT took (already `None` after `take_batch_undo`), never one a
    /// newer, unrelated batch apply just installed.
    BatchUndoDone {
        result: crate::presets::apply::BatchResult,
    },
    /// The startup preset-directory scan finished.
    PresetsLoaded {
        presets: Vec<crate::presets::Preset>,
    },
    /// An off-thread source-document read started by a library context-menu
    /// action finished (`presets::menu::spawn_doc_read`). `purpose` says which
    /// action asked for it — filling the copy-settings clipboard, or opening
    /// the "Save preset" modal over the document.
    MenuDocRead {
        doc: ferrolite_pipeline::EditDoc,
        purpose: crate::presets::menu::DocReadPurpose,
    },
}

/// Owned fields of a delivered `AppEvent::WarmSourceReady`, queued onto
/// `AppState.warm_render_queue` by `apply` and drained one-per-frame by
/// `FerroliteApp::drain_one_warm_render` (Task 8) on the render thread.
pub struct WarmSourcePayload {
    pub image_id: i64,
    pub source: std::sync::Arc<ferrolite_image::LinearRgbaF32>,
    pub op_stack: ferrolite_pipeline::OpStack,
    pub kind: ferrolite_image::FileKind,
    pub color_profile: ferrolite_decode::ColorProfile,
}

// Manual `Debug`: `AppEvent::PyramidReady` carries an `Arc<dyn TileSource + Send +
// Sync>` and an `Arc<GpuPyramidSource>`. `TileSource` is an engine-tier trait
// (`ferrolite-vt`) with no `Debug` supertrait, so it cannot be `#[derive(Debug)]`d
// as a trait object; adding `Debug` as a supertrait there would touch every
// implementor across the engine (large blast radius). Instead we hand-write
// `Debug` here and format the pyramid/source fields opaquely, keeping the change
// confined to this crate.
impl std::fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppEvent::Scanned { added } => f.debug_struct("Scanned").field("added", added).finish(),
            AppEvent::Indexed { added } => f.debug_struct("Indexed").field("added", added).finish(),
            AppEvent::ThumbReady { image_id, w, h, .. } => f
                .debug_struct("ThumbReady")
                .field("image_id", image_id)
                .field("w", w)
                .field("h", h)
                .finish_non_exhaustive(),
            AppEvent::ThumbFailed { image_id } => f
                .debug_struct("ThumbFailed")
                .field("image_id", image_id)
                .finish(),
            AppEvent::ThumbMissing { image_id } => f
                .debug_struct("ThumbMissing")
                .field("image_id", image_id)
                .finish(),
            AppEvent::IngestPlanned { total } => f
                .debug_struct("IngestPlanned")
                .field("total", total)
                .finish(),
            AppEvent::IngestDone => f.write_str("IngestDone"),
            AppEvent::PreviewReady { image_id, .. } => f
                .debug_struct("PreviewReady")
                .field("image_id", image_id)
                .finish_non_exhaustive(),
            AppEvent::FullDecoded { image_id, .. } => f
                .debug_struct("FullDecoded")
                .field("image_id", image_id)
                .finish_non_exhaustive(),
            AppEvent::PyramidReady { image_id, .. } => f
                .debug_struct("PyramidReady")
                .field("image_id", image_id)
                .field("tile_source", &"<tile_source>")
                .field("gpu_pyramid", &"<gpu_pyramid>")
                .finish(),
            AppEvent::FullFailed { image_id } => f
                .debug_struct("FullFailed")
                .field("image_id", image_id)
                .finish(),
            AppEvent::MetadataResult { ok, warning } => f
                .debug_struct("MetadataResult")
                .field("ok", ok)
                .field("warning", warning)
                .finish(),
            AppEvent::OpsLoaded { image_id, .. } => f
                .debug_struct("OpsLoaded")
                .field("image_id", image_id)
                .finish_non_exhaustive(),
            AppEvent::OpsSaved { ok, warning } => f
                .debug_struct("OpsSaved")
                .field("ok", ok)
                .field("warning", warning)
                .finish(),
            AppEvent::HistogramReady { image_id, .. } => f
                .debug_struct("HistogramReady")
                .field("image_id", image_id)
                .finish_non_exhaustive(),
            AppEvent::PreviewCacheWritten { image_id } => f
                .debug_struct("PreviewCacheWritten")
                .field("image_id", image_id)
                .finish(),
            AppEvent::PreviewCacheHit { image_id, .. } => f
                .debug_struct("PreviewCacheHit")
                .field("image_id", image_id)
                .finish_non_exhaustive(),
            AppEvent::PreviewCacheMiss { image_id } => f
                .debug_struct("PreviewCacheMiss")
                .field("image_id", image_id)
                .finish(),
            AppEvent::WarmSourceReady { image_id, .. } => f
                .debug_struct("WarmSourceReady")
                .field("image_id", image_id)
                .finish_non_exhaustive(),
            AppEvent::ExportProgress {
                image_id,
                done,
                total,
            } => f
                .debug_struct("ExportProgress")
                .field("image_id", image_id)
                .field("done", done)
                .field("total", total)
                .finish(),
            AppEvent::ExportItemStarted { name } => f
                .debug_struct("ExportItemStarted")
                .field("name", name)
                .finish(),
            AppEvent::ExportFinished {
                image_id,
                ok,
                message,
            } => f
                .debug_struct("ExportFinished")
                .field("image_id", image_id)
                .field("ok", ok)
                .field("message", message)
                .finish(),
            AppEvent::BatchItemFinished {
                image_id,
                ok,
                message,
            } => f
                .debug_struct("BatchItemFinished")
                .field("image_id", image_id)
                .field("ok", ok)
                .field("message", message)
                .finish(),
            AppEvent::DisplayProfileResolved {
                name, generation, ..
            } => f
                .debug_struct("DisplayProfileResolved")
                .field("name", name)
                .field("generation", generation)
                .finish_non_exhaustive(),
            AppEvent::LensBaked { image_id, .. } => f
                .debug_struct("LensBaked")
                .field("image_id", image_id)
                .finish_non_exhaustive(),
            AppEvent::MetaLoaded { image_id, .. } => f
                .debug_struct("MetaLoaded")
                .field("image_id", image_id)
                .finish_non_exhaustive(),
            AppEvent::Notify { level, message } => f
                .debug_struct("Notify")
                .field("level", level)
                .field("message", message)
                .finish(),
            AppEvent::MetaBackfillReady { results } => f
                .debug_struct("MetaBackfillReady")
                .field("batch_len", &results.len())
                .finish(),
            AppEvent::BatchApplyDone {
                result,
                label,
                cancelled,
                ..
            } => f
                .debug_struct("BatchApplyDone")
                .field("result", result)
                .field("label", label)
                .field("cancelled", cancelled)
                .finish_non_exhaustive(),
            AppEvent::BatchApplyProgress { done, total } => f
                .debug_struct("BatchApplyProgress")
                .field("done", done)
                .field("total", total)
                .finish(),
            AppEvent::BatchUndoDone { result } => f
                .debug_struct("BatchUndoDone")
                .field("result", result)
                .finish(),
            AppEvent::PresetsLoaded { presets } => f
                .debug_struct("PresetsLoaded")
                .field("count", &presets.len())
                .finish(),
            AppEvent::MenuDocRead { purpose, .. } => f
                .debug_struct("MenuDocRead")
                .field("purpose", purpose)
                .finish_non_exhaustive(),
        }
    }
}

impl AppState {
    /// Fold a non-texture event into counters. Returns the decoded RGBA8 pixels
    /// (+ dimensions) for a `ThumbReady` so the caller (which holds egui
    /// `Context`) can upload a texture — keeping this function egui-free. No
    /// decode happens here; the pixels arrive already decoded from a job thread.
    pub fn apply(&mut self, event: AppEvent) -> Option<(i64, Vec<u8>, u32, u32)> {
        match event {
            AppEvent::Scanned { added } => {
                self.scanned += added as u64;
                None
            }
            AppEvent::Indexed { added } => {
                self.indexed += added as u64;
                // In the inline-thumbnail model, each ingested (indexed) row IS
                // one processed file, so indexed rows drive ingest progress. This
                // is deliberately fed off the consumer's per-row `Indexed` (not
                // `ThumbReady`) so lazy-load scroll re-decodes — which emit
                // `ThumbReady` but no `Indexed` — never inflate the progress.
                self.ingest_done += added;
                None
            }
            AppEvent::ThumbReady {
                image_id,
                rgba,
                w,
                h,
            } => {
                // Purely a texture-upload signal now: clear the lazy-load in-flight
                // marker and hand the decoded pixels up for upload. Touches no
                // ingest counter (both ingest and lazy-load paths emit this).
                self.thumb_pending.remove(&image_id);
                self.thumb_handles.remove(&image_id);
                // A thumbnail actually arrived, so any prior "missing" verdict
                // for this id is stale — clear it so a later refresh/scroll
                // that finds the texture gone (e.g. evicted from the LRU cache)
                // can request it again instead of being stuck sticky-missing.
                self.thumb_missing.remove(&image_id);
                // Keep the in-memory grid row's cell-aspect source (`thumb_w`/
                // `thumb_h`, see `ImageRecord::thumb_w`) in sync with what was
                // just persisted, so an edited-thumbnail regen's new (cropped)
                // aspect shows immediately without a full library reload — and
                // an un-cropped re-edit correctly restores the original aspect.
                // Only bumps `images_rev` (rebuilding the grid's justified-rows
                // layout) when the aspect actually changed: an ordinary
                // lazy-load re-decode reports the SAME dims already cached from
                // the initial `thumbnails`-joined query, so this is a no-op on
                // the hot scroll path and only fires for a genuine crop/geometry
                // edit.
                if let Some(rec) = self.images.iter_mut().find(|r| r.id == image_id) {
                    if rec.thumb_w != Some(w) || rec.thumb_h != Some(h) {
                        rec.thumb_w = Some(w);
                        rec.thumb_h = Some(h);
                        self.images_rev = self.images_rev.wrapping_add(1);
                    }
                }
                Some((image_id, rgba, w, h))
            }
            AppEvent::ThumbFailed { image_id } => {
                self.thumb_pending.remove(&image_id);
                self.thumb_handles.remove(&image_id);
                None
            }
            AppEvent::ThumbMissing { image_id } => {
                self.thumb_pending.remove(&image_id);
                self.thumb_handles.remove(&image_id);
                self.thumb_missing.insert(image_id);
                None
            }
            AppEvent::IngestPlanned { total } => {
                self.ingest_total += total;
                None
            }
            AppEvent::IngestDone => {
                self.active_ingests = self.active_ingests.saturating_sub(1);
                // Let any cell still marked "missing" retry once now that this
                // ingest pass has finished (it may have since been generated).
                self.thumb_missing.clear();
                None
            }
            // Handled in `app.rs` (needs GPU state) before reaching `apply`.
            AppEvent::PreviewReady { .. } => None,
            AppEvent::FullDecoded { .. } => None,
            // Handled in `app.rs` (needs GPU state); nothing to fold here.
            AppEvent::PyramidReady { .. } => None,
            // Terminal-state handling happens in `app.rs`; nothing to fold here.
            AppEvent::FullFailed { .. } => None,
            AppEvent::MetadataResult { ok, warning } => {
                if !ok {
                    self.dirty = true;
                }
                if let Some(w) = warning {
                    self.notify(crate::notifications::Level::Error, w);
                }
                None
            }
            // Handled in `app.rs` (needs GPU state); nothing to fold here.
            AppEvent::OpsLoaded { .. } => None,
            AppEvent::OpsSaved { ok, warning } => {
                self.ops_save_inflight = self.ops_save_inflight.saturating_sub(1);
                self.ops_save_failed = !ok;
                if let Some(w) = warning {
                    self.notify(crate::notifications::Level::Error, w);
                }
                None
            }
            AppEvent::HistogramReady { .. } => None,
            // Metrics/tests only; the write-back job already requested a repaint.
            AppEvent::PreviewCacheWritten { .. } => None,
            // Handled in `app.rs` (needs GPU state to reveal / gate the full
            // decode); nothing to fold here.
            AppEvent::PreviewCacheHit { .. } => None,
            AppEvent::PreviewCacheMiss { .. } => None,
            // Queue the decoded source for the render-thread warm-render
            // (`FerroliteApp::drain_one_warm_render`, Task 8). Bounded so a fast
            // filmstrip scrub cannot pile up unbounded GPU work: an overflow
            // drops the OLDEST queued payload, since a fast scrub has already
            // moved past the neighbors it would have warmed.
            AppEvent::WarmSourceReady {
                image_id,
                source,
                op_stack,
                kind,
                color_profile,
            } => {
                const CAP: usize = crate::develop::cache::WARM_WINDOW_FORWARD
                    + crate::develop::cache::WARM_WINDOW_BACK;
                if self.warm_render_queue.len() >= CAP {
                    self.warm_render_queue.pop_front();
                }
                self.warm_render_queue.push_back(WarmSourcePayload {
                    image_id,
                    source,
                    op_stack,
                    kind,
                    color_profile,
                });
                None
            }
            // Handled in `app.rs` (needs GPU-independent status-bar update, but
            // routed there alongside the other viewer-scoped events); nothing to
            // fold here.
            AppEvent::ExportProgress { .. } => None,
            AppEvent::ExportFinished { .. } => None,
            AppEvent::ExportItemStarted { .. } => None, // handled in app.rs (sets current_name)
            AppEvent::BatchItemFinished {
                image_id: _,
                ok,
                message,
            } => {
                if let Some(a) = self.export_activity.as_mut() {
                    a.item_finished(ok, message);
                }
                None
            }
            // Handled in `app.rs` (needs GPU state to build/replace the display
            // LUT texture); nothing to fold here.
            AppEvent::DisplayProfileResolved { .. } => None,
            // Handled in `app.rs` (needs GPU state to upload the warp/vignette
            // textures and rebuild the tile producer); nothing to fold here.
            AppEvent::LensBaked { .. } => None,
            // Handled in `app.rs` (drives the auto-match against `state.lens_db`
            // and seeds the panel); nothing to fold here.
            AppEvent::MetaLoaded { .. } => None,
            AppEvent::Notify { level, message } => {
                self.notifications
                    .push(level, message, std::time::Instant::now());
                None
            }
            AppEvent::MetaBackfillReady { results } => {
                // Deliberately on the UI thread (see the variant's doc
                // comment): locks the same writer every other catalog
                // mutation uses, then bumps `dirty` exactly once so any
                // active lens/aperture/focal filter re-queries with the
                // freshly-backfilled rows visible. A write failure (e.g. a
                // disk error) is surfaced as a toast rather than silently
                // dropped; the rows simply stay NULL and are picked up
                // again by a later launch's backfill pass.
                let write_result = self
                    .writer
                    .lock()
                    .expect("writer")
                    .apply_metadata_backfill_batch(&results);
                if let Err(e) = write_result {
                    self.notify(
                        crate::notifications::Level::Error,
                        format!("metadata backfill write failed: {e}"),
                    );
                }
                // Refresh the cached camera/lens/ISO/date aggregates once per
                // batch so newly-recovered lenses show up in the Lens filter
                // dropdown without a restart. Bounded by distinct-value
                // count, so this is cheap even across many batches.
                self.reload_vocab();
                self.dirty = true;
                None
            }
            // Push the one-level undo snapshot (`None` when the batch exceeded
            // `BATCH_UNDO_MAX` or nothing was applied — undo is simply not
            // offered) and raise the result toast. `cancelled` is threaded
            // through to `batch_result_message` so a cancelled run reads as
            // a cancellation rather than "N images skipped" (see that
            // variant's doc comment and Task 4's review finding).
            AppEvent::BatchApplyDone {
                result,
                snapshot,
                label,
                cancelled,
            } => {
                self.batch_undo = snapshot;
                let undo_hint = self.batch_undo.is_some().then(|| {
                    self.settings
                        .keymap
                        .hint(crate::settings::keymap::Action::Undo)
                });
                let (level, mut msg) = crate::presets::apply::batch_result_message(
                    &result,
                    &label,
                    cancelled,
                    undo_hint.as_deref(),
                );
                // The image open in Develop is never a batch target (design
                // §5.1). The user asked for it to be included, so say plainly
                // that it was not — silently applying to N-1 images would look
                // like a bug. The flag is set when the batch is SPAWNED
                // (`presets::menu`) and consumed exactly once here.
                if std::mem::take(&mut self.batch_excluded_open_image) {
                    msg.push_str(crate::presets::menu::EXCLUDED_OPEN_IMAGE_NOTE);
                }
                self.notify(level, msg);
                // At least one image's `has_edits`/thumbnail-stale flag was
                // just rewritten on the catalog side (`spawn_batch_apply`/
                // `spawn_batch_undo`), so the currently-browsed grid's
                // in-memory `ImageRecord`s are now stale — mirrors
                // `MetaBackfillReady`'s `dirty = true` for the same reason.
                // No-op when nothing actually applied (e.g. an all-skipped
                // or fully-cancelled-before-the-first-item run).
                if result.applied > 0 {
                    self.dirty = true;
                }
                None
            }
            // Status-bar-only progress readout (a future indicator); no
            // counter to fold here.
            AppEvent::BatchApplyProgress { .. } => None,
            // Deliberately does NOT touch `batch_undo`: `take_batch_undo`
            // already consumed it (set it to `None`) before this job was
            // ever spawned (`FerroliteApp::apply_undo_redo`), and a newer,
            // unrelated batch apply may have installed a fresh snapshot in
            // the meantime — this must never clobber that (see
            // `AppEvent::BatchUndoDone`'s doc comment).
            AppEvent::BatchUndoDone { result } => {
                let (level, msg) = crate::presets::apply::batch_undo_message(&result);
                self.notify(level, msg);
                if result.applied > 0 {
                    self.dirty = true;
                }
                None
            }
            AppEvent::PresetsLoaded { presets } => {
                self.presets = presets;
                None
            }
            // Both landings are pure state transitions (no egui, no GPU), so
            // they fold here rather than in `app.rs`.
            AppEvent::MenuDocRead { doc, purpose } => {
                match purpose {
                    crate::presets::menu::DocReadPurpose::Copy => {
                        crate::presets::menu::set_clipboard(self, &doc);
                    }
                    crate::presets::menu::DocReadPurpose::SavePreset => {
                        crate::presets::menu::open_save_modal(self, doc);
                    }
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_event_increments_count() {
        let mut s = AppState::for_test();
        s.apply(AppEvent::Indexed { added: 3 });
        s.apply(AppEvent::Indexed { added: 2 });
        assert_eq!(s.indexed, 5);
    }

    #[test]
    fn indexed_event_advances_ingest_done() {
        // In the inline model, each indexed row is one processed file, so
        // `Indexed` drives `ingest_done`.
        let mut s = AppState::for_test();
        s.ingest_total = 5;
        s.apply(AppEvent::Indexed { added: 1 });
        s.apply(AppEvent::Indexed { added: 2 });
        assert_eq!(s.ingest_done, 3);
        assert_eq!(s.indexed, 3);
    }

    #[test]
    fn thumb_ready_returns_pixels_and_clears_pending_without_counting() {
        let mut s = AppState::for_test();
        s.thumb_pending.insert(7);
        // 1x1 RGBA pixel (4 bytes).
        let out = s.apply(AppEvent::ThumbReady {
            image_id: 7,
            rgba: vec![10, 20, 30, 255],
            w: 1,
            h: 1,
        });
        assert_eq!(out, Some((7, vec![10, 20, 30, 255], 1, 1)));
        // ThumbReady is now purely a texture-upload signal — no ingest counter.
        assert_eq!(s.ingest_done, 0);
        assert!(
            !s.thumb_pending.contains(&7),
            "ThumbReady must clear the pending marker"
        );
    }

    #[test]
    fn thumb_ready_does_not_advance_ingest_for_lazy_load() {
        // Lazy-load scroll re-decodes emit ThumbReady but no Indexed: they must
        // not inflate ingest progress.
        let mut s = AppState::for_test();
        s.thumb_pending.insert(7);
        let out = s.apply(AppEvent::ThumbReady {
            image_id: 7,
            rgba: vec![10, 20, 30, 255],
            w: 1,
            h: 1,
        });
        assert_eq!(out, Some((7, vec![10, 20, 30, 255], 1, 1)));
        assert_eq!(s.ingest_done, 0);
        assert!(
            !s.thumb_pending.contains(&7),
            "ThumbReady must clear the pending marker even for lazy-load"
        );
    }

    /// Minimal `ImageRecord` fixture for the `thumb_w`/`thumb_h` sync tests.
    fn rec(id: i64, thumb_w: Option<u32>, thumb_h: Option<u32>) -> ferrolite_catalog::ImageRecord {
        ferrolite_catalog::ImageRecord {
            id,
            folder_id: 1,
            filename: "x.nef".into(),
            width: Some(4000),
            height: Some(3000),
            orientation: ferrolite_image::Orientation::Normal,
            capture_time: None,
            iso: None,
            decode_status: ferrolite_catalog::DecodeStatus::Done,
            kind: ferrolite_catalog::FileKind::Raw,
            rating: ferrolite_image::Rating::default(),
            flag: ferrolite_image::Flag::None,
            has_edits: false,
            thumb_w,
            thumb_h,
        }
    }

    /// A crop-driven regen reports NEW (cropped) thumbnail dims: the grid row's
    /// `thumb_w`/`thumb_h` (its cell-aspect source, see `library::grid::
    /// cell_aspect`) must update in place, and `images_rev` must bump so the
    /// justified-rows layout rebuilds with the new aspect — without this, the
    /// grid would keep showing the pre-crop aspect until a full library reload.
    #[test]
    fn thumb_ready_updates_thumb_dims_and_bumps_images_rev_on_change() {
        let mut s = AppState::for_test();
        s.images = vec![rec(7, Some(400), Some(300))];
        let rev_before = s.images_rev;

        s.apply(AppEvent::ThumbReady {
            image_id: 7,
            rgba: vec![0; 120 * 200 * 4],
            w: 120,
            h: 200,
        });

        let updated = s.images.iter().find(|r| r.id == 7).unwrap();
        assert_eq!(updated.thumb_w, Some(120));
        assert_eq!(updated.thumb_h, Some(200));
        assert_ne!(
            s.images_rev, rev_before,
            "images_rev must bump so the grid layout cache rebuilds for the new aspect"
        );
    }

    /// An ordinary lazy-load scroll re-decode reports the SAME dims already
    /// cached (from the initial `thumbnails`-joined query) — this must be a
    /// no-op for `images_rev` so scrolling never triggers a full grid layout
    /// rebuild per cell.
    #[test]
    fn thumb_ready_does_not_bump_images_rev_when_dims_unchanged() {
        let mut s = AppState::for_test();
        s.images = vec![rec(7, Some(120), Some(200))];
        let rev_before = s.images_rev;

        s.apply(AppEvent::ThumbReady {
            image_id: 7,
            rgba: vec![0; 120 * 200 * 4],
            w: 120,
            h: 200,
        });

        assert_eq!(
            s.images_rev, rev_before,
            "unchanged thumb dims must not bump images_rev (no layout rebuild on ordinary lazy-load)"
        );
    }

    #[test]
    fn thumb_failed_clears_pending_without_counting() {
        let mut s = AppState::for_test();
        s.thumb_pending.insert(9);
        let out = s.apply(AppEvent::ThumbFailed { image_id: 9 });
        assert_eq!(out, None);
        assert_eq!(s.ingest_done, 0);
        assert!(
            !s.thumb_pending.contains(&9),
            "ThumbFailed must clear the pending marker"
        );
    }

    /// Anti-storm invariant (Task 1): folding `ThumbMissing` must clear
    /// `thumb_pending` and mark the id sticky-missing so `request_thumbnail`'s
    /// guard skips it on every subsequent frame instead of re-spawning a
    /// `Visible` job for a cell whose blob simply isn't there yet.
    #[test]
    fn thumb_missing_clears_pending_and_marks_sticky_missing() {
        let mut s = AppState::for_test();
        s.thumb_pending.insert(11);
        let out = s.apply(AppEvent::ThumbMissing { image_id: 11 });
        assert_eq!(out, None);
        assert!(
            !s.thumb_pending.contains(&11),
            "ThumbMissing must clear the pending marker"
        );
        assert!(
            s.thumb_missing.contains(&11),
            "ThumbMissing must mark the id sticky-missing"
        );

        // The anti-storm guard: request_thumbnail must now short-circuit for
        // this id (no job submitted) since it's neither textured nor merely
        // in-flight — it's known-missing.
        let ctx = egui::Context::default();
        let before = s.jobs.pending_count();
        s.request_thumbnail(&ctx, 11);
        assert_eq!(
            s.jobs.pending_count(),
            before,
            "request_thumbnail must not submit a job for a sticky-missing id"
        );
    }

    /// Once a thumbnail actually arrives, a stale sticky-missing marker for
    /// that id must be cleared (so a future eviction + rescroll can re-request
    /// it rather than being stuck skipped forever).
    #[test]
    fn thumb_ready_clears_sticky_missing() {
        let mut s = AppState::for_test();
        s.thumb_missing.insert(13);
        s.apply(AppEvent::ThumbReady {
            image_id: 13,
            rgba: vec![1, 2, 3, 255],
            w: 1,
            h: 1,
        });
        assert!(
            !s.thumb_missing.contains(&13),
            "ThumbReady must clear the sticky-missing marker"
        );
    }

    /// A completed ingest pass lets any still-missing cells retry once, in
    /// case generation reached them after their lazy-load job observed
    /// `Ok(None)`.
    #[test]
    fn ingest_done_clears_thumb_missing() {
        let mut s = AppState::for_test();
        s.active_ingests = 1;
        s.thumb_missing.insert(21);
        s.thumb_missing.insert(22);
        s.apply(AppEvent::IngestDone);
        assert!(
            s.thumb_missing.is_empty(),
            "IngestDone must clear thumb_missing so retries can happen"
        );
    }

    #[test]
    fn ingest_planned_sets_total() {
        let mut s = AppState::for_test();
        let out = s.apply(AppEvent::IngestPlanned { total: 42 });
        assert_eq!(out, None);
        assert_eq!(s.ingest_total, 42);
    }

    #[test]
    fn metadata_result_failure_pushes_error_toast() {
        use crate::notifications::Level;
        let mut s = AppState::for_test();
        s.apply(AppEvent::MetadataResult {
            ok: false,
            warning: Some("catalog write failed".into()),
        });
        assert!(s.dirty);
        let n = s.notifications.iter_newest_first().next().unwrap();
        assert_eq!(n.level(), Level::Error);
        assert_eq!(n.message(), "catalog write failed");
    }

    #[test]
    fn metadata_result_clean_success_pushes_nothing() {
        let mut s = AppState::for_test();
        s.apply(AppEvent::MetadataResult {
            ok: true,
            warning: None,
        });
        assert!(s.notifications.is_empty());
    }

    #[test]
    fn ops_saved_ok_decrements_inflight_and_clears_failed() {
        let mut s = AppState::for_test();
        s.ops_save_inflight = 1;
        s.ops_save_failed = true;
        s.apply(AppEvent::OpsSaved {
            ok: true,
            warning: None,
        });
        assert_eq!(s.ops_save_inflight, 0);
        assert!(!s.ops_save_failed);
        assert!(s.notifications.is_empty());
    }

    #[test]
    fn ops_saved_failure_pushes_error_toast() {
        use crate::notifications::Level;
        let mut s = AppState::for_test();
        s.ops_save_inflight = 1;
        s.apply(AppEvent::OpsSaved {
            ok: false,
            warning: Some("sidecar write failed".into()),
        });
        assert!(s.ops_save_failed);
        assert_eq!(
            s.notifications.iter_newest_first().next().unwrap().level(),
            Level::Error
        );
    }

    #[test]
    fn ops_saved_ok_saturates_at_zero_when_already_zero() {
        let mut s = AppState::for_test();
        s.ops_save_inflight = 0;

        s.apply(AppEvent::OpsSaved {
            ok: true,
            warning: None,
        });

        assert_eq!(s.ops_save_inflight, 0, "saturating_sub must not underflow");
    }

    #[test]
    fn batch_item_finished_folds_into_aggregate() {
        let mut s = AppState::for_test();
        s.export_activity = Some(crate::export::ExportActivity::new_batch(2));
        s.apply(AppEvent::BatchItemFinished {
            image_id: 1,
            ok: true,
            message: "ok".into(),
        });
        s.apply(AppEvent::BatchItemFinished {
            image_id: 2,
            ok: false,
            message: "disk full".into(),
        });
        let a = s.export_activity.as_ref().unwrap();
        assert_eq!(a.completed, 2);
        assert_eq!(a.failed, 1);
        assert!(a.is_done());
        assert_eq!(a.warnings, vec!["disk full".to_string()]);
    }

    #[test]
    fn notify_event_pushes_into_store() {
        use crate::notifications::Level;
        let mut s = AppState::for_test();
        s.apply(AppEvent::Notify {
            level: Level::Error,
            message: "SD card removed".into(),
        });
        assert_eq!(s.notifications.iter_newest_first().count(), 1);
        let n = s.notifications.iter_newest_first().next().unwrap();
        assert_eq!(n.level(), Level::Error);
        assert_eq!(n.message(), "SD card removed");
    }

    #[test]
    fn notify_helper_pushes_into_store() {
        use crate::notifications::Level;
        let mut s = AppState::for_test();
        s.notify(Level::Info, "12 photos indexed");
        assert_eq!(s.notifications.iter_newest_first().count(), 1);
    }

    /// `MetaBackfillReady` writes through the catalog writer HERE, on the UI
    /// thread inside `apply` (unlike `metadata::spawn_metadata_write`, which
    /// writes from a job thread) — see the variant's doc comment. This test
    /// exercises that write end-to-end: seed a NULL-metadata row via the same
    /// writer `AppState::for_test` sets up, fold the event, then confirm the
    /// backlog count dropped to zero and `dirty` was bumped.
    #[test]
    fn meta_backfill_ready_writes_batch_through_writer_and_marks_dirty() {
        let mut s = AppState::for_test();
        let image_id = {
            let db = s.writer.lock().unwrap();
            let folder = db.upsert_folder(std::path::Path::new("/p"), None).unwrap();
            db.upsert_image(&ferrolite_catalog::NewImage::pending(
                folder,
                "a.nef".into(),
                1,
                1,
                ferrolite_catalog::FileKind::Raw,
                0,
            ))
            .unwrap()
        };
        assert_eq!(
            s.writer
                .lock()
                .unwrap()
                .metadata_backfill_pending_count()
                .unwrap(),
            1
        );

        s.dirty = false;
        let out = s.apply(AppEvent::MetaBackfillReady {
            results: vec![ferrolite_catalog::BackfillResult {
                id: image_id,
                lens: Some("50mm f/1.8".to_string()),
                aperture: Some(1.8),
                focal_length: Some(50.0),
            }],
        });
        assert_eq!(out, None);
        assert!(s.dirty, "MetaBackfillReady must bump dirty once per batch");
        assert_eq!(
            s.writer
                .lock()
                .unwrap()
                .metadata_backfill_pending_count()
                .unwrap(),
            0,
            "the backfilled row must drop out of the NULL-metadata backlog"
        );
        assert!(s.notifications.is_empty(), "a clean write raises no toast");
    }

    /// A successful batch apply with a retained snapshot: the snapshot is
    /// pushed into `batch_undo`, the toast names the live Undo keybind, and
    /// `dirty` is bumped (the catalog's `has_edits` was rewritten under us).
    #[test]
    fn batch_apply_done_stores_snapshot_pushes_toast_and_marks_dirty() {
        use crate::notifications::Level;
        use crate::presets::apply::{BatchResult, UndoSnapshot};

        let mut s = AppState::for_test();
        s.dirty = false;
        let snapshot = UndoSnapshot {
            entries: vec![(1, std::path::PathBuf::from("/a.arw"), "{}".to_string())],
        };
        let out = s.apply(AppEvent::BatchApplyDone {
            result: BatchResult {
                applied: 5,
                failed: 0,
                skipped: 0,
            },
            snapshot: Some(snapshot),
            label: "Warm portrait".to_string(),
            cancelled: false,
        });
        assert_eq!(out, None);
        assert!(
            s.batch_undo.is_some(),
            "a retained snapshot must be pushed onto batch_undo"
        );
        assert!(s.dirty, "an applied batch must bump dirty");
        let n = s.notifications.iter_newest_first().next().unwrap();
        assert_eq!(n.level(), Level::Info);
        assert!(
            n.message().contains("Press Ctrl+Z to undo."),
            "the toast must name the live Undo keybind: {}",
            n.message()
        );
    }

    /// No snapshot retained (batch over `BATCH_UNDO_MAX`, or nothing
    /// applied): `batch_undo` clears to `None`, the toast carries no undo
    /// hint, and a fully no-op batch does not spuriously mark `dirty`.
    #[test]
    fn batch_apply_done_without_snapshot_omits_undo_hint_and_skips_dirty_when_nothing_applied() {
        use crate::presets::apply::BatchResult;

        let mut s = AppState::for_test();
        s.batch_undo = Some(crate::presets::apply::UndoSnapshot::default());
        s.dirty = false;
        s.apply(AppEvent::BatchApplyDone {
            result: BatchResult {
                applied: 0,
                failed: 0,
                skipped: 3,
            },
            snapshot: None,
            label: "Warm portrait".to_string(),
            cancelled: false,
        });
        assert!(
            s.batch_undo.is_none(),
            "a None snapshot must clear any prior batch_undo"
        );
        assert!(!s.dirty, "nothing applied must not spuriously mark dirty");
        let n = s.notifications.iter_newest_first().next().unwrap();
        assert!(
            !n.message().contains("undo"),
            "no snapshot means no undo hint: {}",
            n.message()
        );
    }

    /// The startup preset scan populates `state.presets` verbatim.
    #[test]
    fn presets_loaded_populates_state() {
        let mut doc = ferrolite_pipeline::EditDoc::default();
        doc.global.exposure = 0.5;
        let preset = crate::presets::Preset {
            version: ferrolite_pipeline::PATCH_VERSION,
            name: "Warm".to_string(),
            owns: ferrolite_pipeline::GroupSet::LIGHT,
            doc,
        };
        let mut s = AppState::for_test();
        assert!(s.presets.is_empty());
        let out = s.apply(AppEvent::PresetsLoaded {
            presets: vec![preset.clone()],
        });
        assert_eq!(out, None);
        assert_eq!(s.presets, vec![preset]);
    }

    /// The regression this variant exists to prevent: `BatchUndoDone` must
    /// NEVER touch `batch_undo`. If a newer, unrelated batch apply installed
    /// a fresh snapshot (its own `BatchApplyDone` already ran) while an
    /// older undo job was still in flight, that undo's own completion must
    /// not clobber the fresh promise the toast already made.
    #[test]
    fn batch_undo_done_never_touches_an_unrelated_pending_snapshot() {
        use crate::notifications::Level;
        use crate::presets::apply::{BatchResult, UndoSnapshot};

        let mut s = AppState::for_test();
        let fresh = UndoSnapshot {
            entries: vec![(9, std::path::PathBuf::from("/newer.arw"), "{}".to_string())],
        };
        s.batch_undo = Some(fresh.clone());
        s.dirty = false;

        let out = s.apply(AppEvent::BatchUndoDone {
            result: BatchResult {
                applied: 5,
                failed: 0,
                skipped: 0,
            },
        });

        assert_eq!(out, None);
        assert_eq!(
            s.batch_undo.as_ref().map(|snap| &snap.entries),
            Some(&fresh.entries),
            "an unrelated newer snapshot must survive BatchUndoDone untouched"
        );
        assert!(s.dirty, "a successful revert must still mark dirty");
        let n = s.notifications.iter_newest_first().next().unwrap();
        assert_eq!(n.level(), Level::Info);
        assert_eq!(n.message(), "Reverted the last batch apply on 5 images.");
    }

    /// A batch that left the open Develop image out of its targets must SAY
    /// so, appended to the ordinary result toast — applying to N-1 images
    /// without a word would read as a bug (design §5.1).
    #[test]
    fn batch_apply_done_reports_the_excluded_develop_image_and_clears_the_flag() {
        use crate::presets::apply::BatchResult;

        let mut s = AppState::for_test();
        s.batch_excluded_open_image = true;
        s.apply(AppEvent::BatchApplyDone {
            result: BatchResult {
                applied: 2,
                failed: 0,
                skipped: 0,
            },
            snapshot: None,
            label: "Warm portrait".to_string(),
            cancelled: false,
        });
        let n = s.notifications.iter_newest_first().next().unwrap();
        assert!(
            n.message()
                .ends_with(crate::presets::menu::EXCLUDED_OPEN_IMAGE_NOTE.trim_start()),
            "the result toast must name the skipped Develop image: {}",
            n.message()
        );
        assert!(
            !s.batch_excluded_open_image,
            "the flag is consumed exactly once, so the NEXT batch's toast stays honest"
        );
    }

    /// The same event with the flag unset must not gain the sentence.
    #[test]
    fn batch_apply_done_omits_the_excluded_note_when_nothing_was_excluded() {
        use crate::presets::apply::BatchResult;

        let mut s = AppState::for_test();
        s.apply(AppEvent::BatchApplyDone {
            result: BatchResult {
                applied: 2,
                failed: 0,
                skipped: 0,
            },
            snapshot: None,
            label: "Warm portrait".to_string(),
            cancelled: false,
        });
        let n = s.notifications.iter_newest_first().next().unwrap();
        assert!(
            !n.message().contains("Develop"),
            "nothing was excluded, so nothing to mention: {}",
            n.message()
        );
    }

    /// A "Copy settings" read landing fills the clipboard with the FULL
    /// document (`default_owns()`); the paste dialog narrows it later.
    #[test]
    fn menu_doc_read_for_copy_fills_the_clipboard_and_toasts() {
        use crate::presets::menu::DocReadPurpose;

        let mut s = AppState::for_test();
        let doc = ferrolite_pipeline::EditDoc::default().set_op(ferrolite_pipeline::Op::Exposure(
            ferrolite_pipeline::Exposure { ev: 1.25 },
        ));
        let out = s.apply(AppEvent::MenuDocRead {
            doc: doc.clone(),
            purpose: DocReadPurpose::Copy,
        });
        assert_eq!(out, None);
        let clip = s.clipboard_patch.as_ref().expect("clipboard must be set");
        assert_eq!(clip.doc, doc);
        assert_eq!(clip.owns, crate::presets::modal::default_owns());
        assert!(
            s.open_group_modal.is_none(),
            "copying opens no dialog — only pasting and saving do"
        );
        assert_eq!(
            s.notifications
                .iter_newest_first()
                .next()
                .unwrap()
                .message(),
            "Copied settings."
        );
    }

    /// A "Save preset" read landing opens the group modal in Save mode over the
    /// document that just arrived, without touching the clipboard.
    #[test]
    fn menu_doc_read_for_save_preset_opens_the_modal_without_touching_the_clipboard() {
        use crate::presets::menu::{DocReadPurpose, GroupModalPurpose};
        use crate::presets::modal::GroupModalMode;

        let mut s = AppState::for_test();
        let doc = ferrolite_pipeline::EditDoc::default().set_op(ferrolite_pipeline::Op::Exposure(
            ferrolite_pipeline::Exposure { ev: -0.5 },
        ));
        s.apply(AppEvent::MenuDocRead {
            doc: doc.clone(),
            purpose: DocReadPurpose::SavePreset,
        });
        let pending = s.open_group_modal.as_ref().expect("modal must be open");
        assert!(matches!(pending.modal.mode, GroupModalMode::Save { .. }));
        match &pending.purpose {
            GroupModalPurpose::SavePreset { doc: captured } => {
                assert_eq!(**captured, doc, "the modal must carry the read document");
            }
            _ => panic!("expected a SavePreset purpose"),
        }
        assert!(
            s.clipboard_patch.is_none(),
            "saving a preset must not clobber the copy/paste clipboard"
        );
    }
}

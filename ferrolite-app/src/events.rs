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
}

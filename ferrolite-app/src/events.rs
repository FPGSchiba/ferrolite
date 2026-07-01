//! Domain events flowing from job threads back to the UI thread over an
//! app-owned channel. `apply` folds an event into `AppState`'s counters; it is
//! pure w.r.t. egui so it can be unit-tested.

use crate::state::AppState;

#[derive(Debug)]
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
    /// A thumbnail job was submitted; record its JobId for reprioritization.
    ThumbRegistered {
        image_id: i64,
        job_id: ferrolite_jobs::JobId,
    },
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
                None
            }
            AppEvent::ThumbReady {
                image_id,
                rgba,
                w,
                h,
            } => {
                self.thumb_done += 1;
                self.thumb_jobs.remove(&image_id);
                self.thumb_pending.remove(&image_id);
                Some((image_id, rgba, w, h))
            }
            AppEvent::ThumbFailed { image_id } => {
                self.thumb_done += 1;
                self.thumb_jobs.remove(&image_id);
                self.thumb_pending.remove(&image_id);
                None
            }
            AppEvent::ThumbRegistered { image_id, job_id } => {
                self.thumb_total += 1;
                self.thumb_jobs.insert(image_id, job_id);
                None
            }
            AppEvent::IngestDone => {
                self.active_ingests = self.active_ingests.saturating_sub(1);
                None
            }
            // Handled in `app.rs` (needs GPU state) before reaching `apply`.
            AppEvent::PreviewReady { .. } => None,
            AppEvent::FullDecoded { .. } => None,
            // Terminal-state handling happens in `app.rs`; nothing to fold here.
            AppEvent::FullFailed { .. } => None,
            AppEvent::MetadataResult { ok, warning } => {
                if !ok {
                    self.dirty = true;
                }
                match warning {
                    Some(w) => self.warning = Some(w),
                    None if ok => self.warning = None,
                    None => {}
                }
                None
            }
            // Handled in `app.rs` (needs GPU state); nothing to fold here.
            AppEvent::OpsLoaded { .. } => None,
            AppEvent::OpsSaved { ok, warning } => {
                self.ops_save_inflight = self.ops_save_inflight.saturating_sub(1);
                self.ops_save_failed = !ok;
                match warning {
                    Some(w) => self.warning = Some(w),
                    None if ok => self.warning = None,
                    None => {}
                }
                None
            }
            AppEvent::HistogramReady { .. } => None,
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
    fn thumb_ready_returns_pixels_and_advances_done() {
        let mut s = AppState::for_test();
        s.thumb_total = 2;
        s.thumb_pending.insert(7);
        // 1x1 RGBA pixel (4 bytes).
        let out = s.apply(AppEvent::ThumbReady {
            image_id: 7,
            rgba: vec![10, 20, 30, 255],
            w: 1,
            h: 1,
        });
        assert_eq!(out, Some((7, vec![10, 20, 30, 255], 1, 1)));
        assert_eq!(s.thumb_done, 1);
        assert!(
            !s.thumb_pending.contains(&7),
            "ThumbReady must clear the pending marker"
        );
    }

    #[test]
    fn thumb_failed_advances_done_without_bytes() {
        let mut s = AppState::for_test();
        s.thumb_pending.insert(9);
        let out = s.apply(AppEvent::ThumbFailed { image_id: 9 });
        assert_eq!(out, None);
        assert_eq!(s.thumb_done, 1);
        assert!(
            !s.thumb_pending.contains(&9),
            "ThumbFailed must clear the pending marker"
        );
    }

    #[test]
    fn thumb_registered_increments_total_and_records_job() {
        let mut s = AppState::for_test();
        let job_id = ferrolite_jobs::JobId(42);
        let out = s.apply(AppEvent::ThumbRegistered {
            image_id: 5,
            job_id,
        });
        assert_eq!(out, None);
        assert_eq!(s.thumb_total, 1);
        assert_eq!(s.thumb_jobs.get(&5), Some(&job_id));
    }

    #[test]
    fn metadata_result_clears_warning_on_clean_success() {
        let mut s = AppState::for_test();
        s.warning = Some("stale warning".into());

        // ok=true, no warning → warning should be cleared.
        s.apply(AppEvent::MetadataResult {
            ok: true,
            warning: None,
        });
        assert_eq!(s.warning, None, "warning must be cleared on clean success");
    }

    #[test]
    fn metadata_result_preserves_warning_on_failure() {
        let mut s = AppState::for_test();
        s.warning = Some("prior warning".into());

        // ok=false, no warning → warning must NOT be cleared (keep the prior).
        s.apply(AppEvent::MetadataResult {
            ok: false,
            warning: None,
        });
        assert_eq!(
            s.warning,
            Some("prior warning".into()),
            "warning must be preserved when ok=false and no new warning"
        );
    }

    #[test]
    fn metadata_result_sets_warning_when_provided() {
        let mut s = AppState::for_test();
        s.warning = None;

        s.apply(AppEvent::MetadataResult {
            ok: true,
            warning: Some("sidecar write failed".into()),
        });
        assert_eq!(
            s.warning,
            Some("sidecar write failed".into()),
            "warning must be set when provided"
        );
    }

    #[test]
    fn ops_saved_ok_decrements_inflight_and_clears_failed() {
        let mut s = AppState::for_test();
        s.ops_save_inflight = 1;
        s.ops_save_failed = true;
        s.warning = Some("prior".into());

        s.apply(AppEvent::OpsSaved {
            ok: true,
            warning: None,
        });

        assert_eq!(s.ops_save_inflight, 0, "inflight decremented to 0");
        assert!(!s.ops_save_failed, "failed cleared on ok=true");
        assert_eq!(s.warning, None, "warning cleared on clean ok=true");
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
}

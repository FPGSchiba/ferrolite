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
    /// A thumbnail finished: JPEG bytes for immediate texture upload.
    ThumbReady { image_id: i64, jpeg: Vec<u8> },
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
    /// upright RGB8/RGBA8 buffer for upload as a rung-1 `VirtualTexture`. Handled
    /// directly in `app.rs` (needs the GPU render state), not folded by `apply`.
    PreviewReady {
        image_id: i64,
        image: ferrolite_image::ImageBuffer,
    },
    /// A viewer tier-2 full RAW decode + quad-bin finished off-thread. Carries the
    /// display-linear RGBA f32 image for upload as a sparse `VirtualTexture`.
    /// Handled directly in `app.rs` (needs the GPU render state), not folded by
    /// `apply`.
    FullDecoded {
        image_id: i64,
        image: ferrolite_image::LinearRgbaF32,
    },
    /// The tier-2 full decode failed; the viewer keeps showing the preview and
    /// goes idle. Folded by `apply` (no GPU work) but matched in `app.rs`.
    FullFailed { image_id: i64 },
    /// Result of an off-thread metadata persist. `ok==false` → reload truth;
    /// `warning` → surface in the status bar.
    MetadataResult { ok: bool, warning: Option<String> },
}

impl AppState {
    /// Fold a non-texture event into counters. Returns the JPEG bytes for a
    /// `ThumbReady` so the caller (which holds egui `Context`) can upload a
    /// texture — keeping this function egui-free.
    pub fn apply(&mut self, event: AppEvent) -> Option<(i64, Vec<u8>)> {
        match event {
            AppEvent::Scanned { added } => {
                self.scanned += added as u64;
                None
            }
            AppEvent::Indexed { added } => {
                self.indexed += added as u64;
                None
            }
            AppEvent::ThumbReady { image_id, jpeg } => {
                self.thumb_done += 1;
                self.thumb_jobs.remove(&image_id);
                Some((image_id, jpeg))
            }
            AppEvent::ThumbFailed { image_id } => {
                self.thumb_done += 1;
                self.thumb_jobs.remove(&image_id);
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
    fn thumb_ready_returns_bytes_and_advances_done() {
        let mut s = AppState::for_test();
        s.thumb_total = 2;
        let out = s.apply(AppEvent::ThumbReady {
            image_id: 7,
            jpeg: vec![1, 2, 3],
        });
        assert_eq!(out, Some((7, vec![1, 2, 3])));
        assert_eq!(s.thumb_done, 1);
    }

    #[test]
    fn thumb_failed_advances_done_without_bytes() {
        let mut s = AppState::for_test();
        let out = s.apply(AppEvent::ThumbFailed { image_id: 9 });
        assert_eq!(out, None);
        assert_eq!(s.thumb_done, 1);
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
}

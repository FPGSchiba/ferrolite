//! Domain events flowing from job threads back to the UI thread over an
//! app-owned channel. `apply` folds an event into `AppState`'s counters; it is
//! pure w.r.t. egui so it can be unit-tested.

use crate::state::AppState;

#[derive(Debug)]
pub enum AppEvent {
    /// `added` rows were indexed (status-bar "N indexed").
    Indexed { added: usize },
    /// A thumbnail finished: JPEG bytes for immediate texture upload.
    ThumbReady { image_id: i64, jpeg: Vec<u8> },
    /// A thumbnail (or its decode) failed; the cell shows a broken placeholder.
    ThumbFailed { image_id: i64 },
    /// The ingest walk + row upserts completed.
    IngestDone,
}

impl AppState {
    /// Fold a non-texture event into counters. Returns the JPEG bytes for a
    /// `ThumbReady` so the caller (which holds egui `Context`) can upload a
    /// texture — keeping this function egui-free.
    pub fn apply(&mut self, event: AppEvent) -> Option<(i64, Vec<u8>)> {
        match event {
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
            AppEvent::IngestDone => None,
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
        let out = s.apply(AppEvent::ThumbReady { image_id: 7, jpeg: vec![1, 2, 3] });
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
}

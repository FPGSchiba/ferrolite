//! Unified export activity: one source of truth for the status-bar indicator,
//! populated by both the single-file (`export/mod.rs`) and batch (`export/batch.rs`)
//! flows. Replaces the batch-only `BatchExportState`.

use ferrolite_jobs::JobHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Single,
    Batch,
}

/// Live state of the in-progress export (single or batch). While `!is_done()`
/// the status bar shows an indicator built from it; the Export module reads it
/// (batch only) for its queue-lock and aggregate summary.
pub struct ExportActivity {
    pub kind: ExportKind,
    /// Images to export (1 for single).
    pub total: usize,
    /// Images finished (ok or failed).
    pub completed: usize,
    pub failed: usize,
    /// Output filename of the in-flight image (already basename + truncatable).
    /// Read by a later task's status-bar indicator; not yet consumed.
    pub current_name: Option<String>,
    /// Per-image render progress for the current image. Read by a later task's
    /// status-bar indicator; not yet consumed.
    pub tile_done: u32,
    pub tile_total: u32,
    /// Cancellation targets: the single export job, or the one batch job.
    pub handles: Vec<JobHandle>,
    /// Per-image failure messages, rolled into the final summary.
    pub warnings: Vec<String>,
}

impl ExportActivity {
    pub fn new_batch(total: usize) -> Self {
        Self {
            kind: ExportKind::Batch,
            total,
            completed: 0,
            failed: 0,
            current_name: None,
            tile_done: 0,
            tile_total: 0,
            handles: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn new_single(name: Option<String>) -> Self {
        Self {
            kind: ExportKind::Single,
            total: 1,
            completed: 0,
            failed: 0,
            current_name: name,
            tile_done: 0,
            tile_total: 0,
            handles: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn is_done(&self) -> bool {
        self.completed >= self.total
    }

    pub fn cancel_all(&self) {
        for h in &self.handles {
            h.cancel();
        }
    }

    /// (completed + current-image tile fraction) / total, clamped to 0..=1.
    /// Read by the status-bar indicator.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        let tile_frac = if self.tile_total == 0 {
            0.0
        } else {
            self.tile_done as f32 / self.tile_total as f32
        };
        ((self.completed as f32 + tile_frac) / self.total as f32).clamp(0.0, 1.0)
    }

    /// A new image started: set its name and reset per-image tile progress.
    pub fn start_item(&mut self, name: Option<String>) {
        self.current_name = name;
        self.tile_done = 0;
        self.tile_total = 0;
    }

    pub fn set_tiles(&mut self, done: u32, total: u32) {
        self.tile_done = done;
        self.tile_total = total;
    }

    /// One image finished; folds into the aggregate counts.
    pub fn item_finished(&mut self, ok: bool, message: String) {
        self.completed += 1;
        if !ok {
            self.failed += 1;
            self.warnings.push(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_batch_and_single_defaults() {
        let b = ExportActivity::new_batch(8);
        assert_eq!(b.kind, ExportKind::Batch);
        assert_eq!(b.total, 8);
        assert_eq!(b.completed, 0);
        assert!(!b.is_done());
        let s = ExportActivity::new_single(Some("hero.avif".into()));
        assert_eq!(s.kind, ExportKind::Single);
        assert_eq!(s.total, 1);
        assert_eq!(s.current_name.as_deref(), Some("hero.avif"));
    }

    #[test]
    fn item_finished_folds_completed_failed_and_warnings() {
        let mut a = ExportActivity::new_batch(2);
        a.item_finished(true, "ok".into());
        a.item_finished(false, "disk full".into());
        assert_eq!(a.completed, 2);
        assert_eq!(a.failed, 1);
        assert!(a.is_done());
        assert_eq!(a.warnings, vec!["disk full".to_string()]);
    }

    #[test]
    fn fraction_blends_completed_with_current_tiles() {
        let mut a = ExportActivity::new_batch(4);
        assert_eq!(a.fraction(), 0.0);
        a.completed = 2;
        a.set_tiles(1, 2); // half of the in-flight image
                           // (2 + 0.5) / 4 = 0.625
        assert!((a.fraction() - 0.625).abs() < 1e-6);
        a.completed = 4;
        a.set_tiles(0, 0);
        assert_eq!(a.fraction(), 1.0, "clamped to 1.0");
    }

    #[test]
    fn fraction_zero_total_is_zero_not_nan() {
        let a = ExportActivity::new_batch(0);
        assert_eq!(a.fraction(), 0.0);
    }

    #[test]
    fn start_item_sets_name_and_resets_tiles() {
        let mut a = ExportActivity::new_batch(3);
        a.set_tiles(5, 10);
        a.start_item(Some("next.avif".into()));
        assert_eq!(a.current_name.as_deref(), Some("next.avif"));
        assert_eq!(a.tile_done, 0);
        assert_eq!(a.tile_total, 0);
    }
}

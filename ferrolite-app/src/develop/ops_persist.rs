//! Off-thread frl:ops sidecar persistence (mirrors `metadata.rs`): the in-memory
//! OpStack edit is immediate; this job follows and reports a MetadataResult.

use crate::events::AppEvent;
use ferrolite_catalog::Catalog;
use ferrolite_jobs::{JobHandle, JobSystem, Priority};
use ferrolite_pipeline::OpStack;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// Persist the op stack: write `frl:ops` to the sidecar (merge-preserving) and
/// update the catalog `has_edits` cache. A sidecar failure is a warning, not a
/// revert (the in-memory stack + has_edits intent are kept), per spec §9.
pub fn spawn_ops_write(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    path: PathBuf,
    stack: OpStack,
) {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Visible, move |_cancel| {
        let payload = ferrolite_pipeline::serialize(&stack);
        let mut warning = None;
        let xmp = ferrolite_catalog::sidecar_path(&path);
        if let Err(e) = ferrolite_catalog::write_ops(&xmp, &payload) {
            warning = Some(format!("sidecar write failed: {e}"));
        }
        let mut ok = true;
        {
            let db = writer.lock().expect("writer");
            if let Err(e) = db.set_has_edits(image_id, !stack.is_identity()) {
                ok = false;
                warning = Some(format!("catalog write failed: {e}"));
            }
        }
        let _ = tx.send(AppEvent::MetadataResult { ok, warning });
        ctx.request_repaint();
    });
}

/// Read `frl:ops` off-thread on viewer open; send an `OpsLoaded` (default stack
/// when absent/malformed/unknown-version, per spec §7).
pub fn spawn_ops_read(
    jobs: &Arc<JobSystem>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    path: PathBuf,
) -> JobHandle {
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Interactive, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        let xmp = ferrolite_catalog::sidecar_path(&path);
        let stack = ferrolite_catalog::read_ops(&xmp)
            .and_then(|p| ferrolite_pipeline::deserialize(&p))
            .unwrap_or_default();
        let _ = tx.send(AppEvent::OpsLoaded { image_id, stack });
        ctx.request_repaint();
    })
}

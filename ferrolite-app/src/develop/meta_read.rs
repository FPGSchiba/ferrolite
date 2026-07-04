//! Off-thread EXIF metadata read on Develop open (mirrors `ops_persist.rs`'s
//! `spawn_ops_read`): the catalog only caches `camera_make`/`camera_model`
//! (used by the library filter bar) and does not carry `focal_length`/
//! `aperture`/`lens` at all, so those fields cannot be read from the catalog.
//! Rather than grow the catalog schema for a Develop-only need, this spawns a
//! lightweight metadata-only decode (`ferrolite_decode::read_metadata` — no
//! pixel data, unlike `decode_meta_and_preview`) off the UI thread and
//! delivers the result via `AppEvent::MetaLoaded`. CLAUDE.md rule 1: even a
//! "cheap" RAW header read is real file I/O and must never run inline on the
//! UI/update thread.

use crate::events::AppEvent;
use ferrolite_image::FileKind;
use ferrolite_jobs::{JobHandle, JobSystem, Priority};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// Read `path`'s metadata off-thread and send `AppEvent::MetaLoaded`. `meta` is
/// `None` on a decode error (corrupt/unsupported file) — the caller then simply
/// has no EXIF to seed the lens panel from, degrading to the existing
/// constant-default seed rather than blocking or erroring the Develop open.
pub fn spawn_meta_read(
    jobs: &Arc<JobSystem>,
    tx: &Sender<AppEvent>,
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
        let meta = ferrolite_decode::read_metadata(&path, kind).ok();
        if cancel.is_cancelled() {
            return;
        }
        let _ = tx.send(AppEvent::MetaLoaded { image_id, meta });
        ctx.request_repaint();
    })
}

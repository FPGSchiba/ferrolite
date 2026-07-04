//! Off-thread lens bake: resolve the persisted lens id → warp grid + vignette
//! map via `ferrolite-lens`. Mirrors `ops_persist.rs`'s off-thread job shape
//! (submit → compute → send an `AppEvent` → `request_repaint`) and the
//! Spec-4.3 monitor-ICC bake (`redetect_display_profile` in `app.rs`): DB
//! lookup + bake are real, possibly non-trivial CPU work (XML-derived
//! polynomial evaluation over a 129×129 grid), so they NEVER run on the UI/
//! update thread (CLAUDE.md rule 1) — only the (O(1)-ish) auto-match string
//! lookup is cheap enough to stay inline, per the task brief.
//!
//! Cancellable like `spawn_ops_read`: the caller stores the returned
//! `JobHandle` and cancels it when superseding (a newer edit, or navigating
//! away from `image_id`) so a stale bake never overwrites a fresher one. The
//! `AppEvent::LensBaked` handler ALSO guards on `image_id == current` as a
//! second line of defense for a bake that was already past its cancellation
//! checkpoint when superseded.

use crate::events::AppEvent;
use ferrolite_jobs::{JobHandle, JobSystem, Priority};
use ferrolite_lens::{LensDb, LensfunDb};
use ferrolite_pipeline::LensCorrection;
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// Baked lens-correction products for one image's current `LensCorrection`.
/// `warp`/`vignette` are `None` when the corresponding correction is disabled
/// OR the matched lens has no calibration for it — both cases render as
/// identity (the geometry head / vignette node fall back to their built-in
/// identity defaults). `resolved_name` is `None` when `lens_id` didn't
/// resolve (e.g. a stale/unknown persisted key), for the panel's label.
#[derive(Debug)]
pub struct LensBakeResult {
    pub warp: Option<ferrolite_lens::WarpGrid>,
    pub vignette: Option<ferrolite_lens::VignetteMap>,
    pub resolved_name: Option<String>,
}

/// Spawn the off-thread bake for `image_id`'s current `LensCorrection`. A
/// `None` `lc.lens_id` (no lens matched yet) or an unresolvable id yields an
/// all-`None` result — the caller then binds the identity warp/vignette,
/// which is byte-identical to no lens correction.
///
/// Callers must NOT invoke this for an Amount-only change (per-correction
/// `distortion.amount`/`tca.amount`/`vignetting.amount`): those are uniform-
/// only updates applied directly via `TileEditPipeline::set_lens_uniform`/
/// `set_vig_amount`, never requiring a re-bake. Gate the call site on the same
/// rebuild-relevant key `ops_edit::needs_full_rebuild` uses (lens id, the
/// enabled flags, focal/aperture/crop) so an amount-only slider drag never
/// spawns a job.
pub fn spawn_lens_bake(
    jobs: &Arc<JobSystem>,
    db: &Arc<LensfunDb>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    lc: LensCorrection,
) -> JobHandle {
    let db = Arc::clone(db);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Visible, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        let result = match lc.lens_id.as_deref().and_then(|id| db.match_by_id(id)) {
            Some(m) => {
                let warp = if lc.distortion.enabled || lc.tca.enabled {
                    db.bake_geometry(&m, lc.focal_len, ferrolite_lens::GRID_N)
                } else {
                    None
                };
                if cancel.is_cancelled() {
                    return;
                }
                let vignette = if lc.vignetting.enabled {
                    db.bake_vignetting(&m, lc.focal_len, lc.aperture, ferrolite_lens::VIGNETTE_LEN)
                } else {
                    None
                };
                LensBakeResult {
                    warp,
                    vignette,
                    resolved_name: Some(m.display_name),
                }
            }
            None => LensBakeResult {
                warp: None,
                vignette: None,
                resolved_name: None,
            },
        };
        if cancel.is_cancelled() {
            return;
        }
        let _ = tx.send(AppEvent::LensBaked { image_id, result });
        ctx.request_repaint();
    })
}

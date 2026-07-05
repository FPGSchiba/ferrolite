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
        // Resolve the display name for the panel label (the shared bake helper
        // returns only the GPU products, not the name).
        let resolved_name = lc
            .lens_id
            .as_deref()
            .and_then(|id| db.match_by_id(id))
            .map(|m| m.display_name);
        if cancel.is_cancelled() {
            return;
        }
        // Delegate the resolve → crop-override → conditional bake to the shared
        // `ferrolite-pipeline` primitive so the viewer, thumbnail, and export
        // paths never drift. Off-thread here (inside the job), per CLAUDE.md §1.
        let (warp, vignette) = ferrolite_pipeline::bake_products(db.as_ref(), &lc);
        let result = LensBakeResult {
            warp,
            vignette,
            resolved_name,
        };
        if cancel.is_cancelled() {
            return;
        }
        let _ = tx.send(AppEvent::LensBaked { image_id, result });
        ctx.request_repaint();
    })
}

/// Decide whether a just-loaded (persisted) `OpStack` needs its lens-correction
/// products re-baked on open. Pure so the on-open wiring (`OpsLoaded` handler
/// in `app.rs`) can be exercised without a `JobSystem`/GPU. A re-bake is only
/// worthwhile when there is a resolvable lens key AND at least one correction
/// is actually enabled — a `LensCorrection` op with everything toggled off (or
/// no `lens_id` at all) bakes to an all-`None` `LensBakeResult` identical to
/// never baking, so skipping the job entirely avoids a pointless DB lookup on
/// every single Develop open.
pub fn needs_rebake_on_load(lc: &LensCorrection) -> bool {
    lc.lens_id.is_some() && (lc.distortion.enabled || lc.tca.enabled || lc.vignetting.enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::Correction;

    fn lc(lens_id: Option<&str>, dist: bool, tca: bool, vig: bool) -> LensCorrection {
        LensCorrection {
            lens_id: lens_id.map(String::from),
            focal_len: 24.0,
            aperture: 8.0,
            crop_factor: 1.0,
            distortion: Correction {
                enabled: dist,
                amount: 1.0,
            },
            tca: Correction {
                enabled: tca,
                amount: 1.0,
            },
            vignetting: Correction {
                enabled: vig,
                amount: 1.0,
            },
        }
    }

    #[test]
    fn no_rebake_when_no_lens_matched() {
        assert!(!needs_rebake_on_load(&lc(None, true, true, true)));
    }

    #[test]
    fn no_rebake_when_lens_matched_but_all_corrections_off() {
        assert!(!needs_rebake_on_load(&lc(
            Some("EF 24-70"),
            false,
            false,
            false
        )));
    }

    #[test]
    fn rebakes_when_lens_matched_and_distortion_enabled() {
        assert!(needs_rebake_on_load(&lc(
            Some("EF 24-70"),
            true,
            false,
            false
        )));
    }

    #[test]
    fn rebakes_when_lens_matched_and_only_vignetting_enabled() {
        assert!(needs_rebake_on_load(&lc(
            Some("EF 24-70"),
            false,
            false,
            true
        )));
    }
}

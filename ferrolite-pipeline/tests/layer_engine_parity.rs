//! Parity goldens for the fused-layer-engine `EditPipeline` chain
//! (`.superpowers/sdd/2026-07-28-unified-engine-phase3-fused-layers/`).
//!
//! **History:** this suite originally pinned the PRE-fusion (six standalone
//! point-op passes) chain, committed before any engine code changed, so
//! Tasks 2-3 could prove the new two-segment engine reproduced it. That job
//! is done (2026-07-29): it caught a real bug (the shared `adjust()` shader's
//! floor clamp was wrongly applied to the new global pseudo-layer dispatches
//! — see `local_adjust.wgsl`) and forced the post-global-color-segment
//! mask-compositing fix (`local_node.rs`'s `evaluate_color` samples masks
//! against `current`, not the node's raw `input`). The remaining old-vs-new
//! deltas (up to 0.6 on `two_masks`) were root-caused to inherent
//! floating-point/hue-domain sensitivity from removing intermediate
//! `rgba16float` round-trips — an accepted, documented precision improvement,
//! not a defect (see `docs/benchmarks/2026-07-28-phase3-fused-engine.md`'s
//! "Accepted rendering deltas vs the pre-fusion chain" section). The goldens
//! were regenerated from the FUSED engine on 2026-07-29 (author-approved).
//!
//! **Going forward**, this suite's job is pinning the FUSED engine against
//! future drift — a regression here means something changed the fused
//! engine's output, not a fusion-vs-pre-fusion parity question anymore.
//! Reproduce these committed goldens within `common::layer_engine::PARITY_TOL`.
//!
//! `UPDATE_GOLDENS=1 cargo test -p ferrolite-pipeline --test layer_engine_parity`
//! (re)writes the committed 16-bit-PNG goldens under
//! `tests/golden/layer_engine/`; a normal run compares against them. Skips
//! cleanly with no GPU adapter, same pattern as `local_node.rs`'s tests — this
//! is primarily a local/author gate since CI may lack an adapter.

mod common;

use common::layer_engine::{compare_or_write_golden16, fixture_docs, hsv_sweep_source, PARITY_TOL};
use ferrolite_gpu::GpuContext;
use ferrolite_pipeline::EditPipeline;
use std::sync::Arc;

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

#[test]
fn current_chain_matches_committed_goldens() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!(
            "no GPU adapter; skipping (expected in headless CI — this is primarily a \
             local/author gate)"
        );
        return;
    };
    let ctx = Arc::new(ctx);
    let src = hsv_sweep_source();
    let docs = fixture_docs();
    assert_eq!(
        docs.first().map(|(name, _)| *name),
        Some("identity"),
        "fixture_docs() must list 'identity' first — the no-op sanity check below depends on it"
    );

    let mut identity_pixels: Option<Vec<f32>> = None;
    let mut failures = Vec::new();

    for (name, doc) in &docs {
        // A fresh `EditPipeline` per fixture, matching every existing golden
        // test file's own pattern (each `#[test]` builds its own pipeline)
        // rather than driving one instance through 8 different stacks via
        // repeated `set_stack` calls.
        let mut pipe = EditPipeline::new(ctx.clone(), &src, doc.clone(), IDENTITY);
        let out = pipe.evaluate();
        let pixels = common::read_image_linear(&ctx, &out);

        if *name == "identity" {
            identity_pixels = Some(pixels.clone());
        } else {
            let base = identity_pixels
                .as_ref()
                .expect("identity fixture must run before any other (see the assert above)");
            let vs_identity = common::layer_engine::max_abs_diff_f32(&pixels, base);
            assert!(
                vs_identity > PARITY_TOL,
                "{name}: fixture is indistinguishable from identity (max diff {vs_identity} <= \
                 PARITY_TOL {PARITY_TOL}) — this fixture accidentally no-ops and tests nothing"
            );
        }

        let diff =
            compare_or_write_golden16(&pixels, out.width, out.height, &format!("{name}.png"));
        if diff > PARITY_TOL {
            failures.push(format!(
                "{name}: max diff {diff:.6} exceeds PARITY_TOL {PARITY_TOL}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "layer-engine parity regression(s) vs committed goldens:\n{}",
        failures.join("\n")
    );
}

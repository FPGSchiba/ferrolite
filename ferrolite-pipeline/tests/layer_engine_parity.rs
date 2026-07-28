//! Parity goldens for the CURRENT (pre-fusion) `EditPipeline` chain — the
//! safety net for the fused-layer-engine phase
//! (`.superpowers/sdd/2026-07-28-unified-engine-phase3-fused-layers/`), which
//! must land *before* any engine code changes. Later fusion tasks reproduce
//! these committed goldens within `common::layer_engine::PARITY_TOL` and beat
//! the medians recorded in
//! `docs/benchmarks/2026-07-28-phase3-fused-engine.md`.
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

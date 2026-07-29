//! Pre-fusion timing baselines for the fused-layer-engine phase
//! (`.superpowers/sdd/2026-07-28-unified-engine-phase3-fused-layers/`).
//! `#[ignore]`d — run explicitly:
//!
//!   cargo test -p ferrolite-pipeline --test engine_bench -- --ignored --nocapture
//!
//! Prints median ms/iteration for three re-evaluate-on-a-dirty-node cases so
//! later fusion tasks have concrete numbers to beat. Skips cleanly with no GPU
//! adapter (same headless-CI pattern as `local_node.rs`'s tests) — this is
//! primarily a local/author benchmark, not a CI gate.

mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_pipeline::{EditPipeline, Exposure, Op};
use std::sync::Arc;
use std::time::{Duration, Instant};

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
const ITERATIONS: usize = 20;

/// Deterministic 6000x4000 synthetic source. Generation cost is intentionally
/// OUTSIDE the timed region below (built once before the warm-up evaluate). A
/// plain gradient is enough here — unlike the parity goldens, the bench only
/// needs a stable, real-content source of the right megapixel count, not the
/// full HSV cube.
fn bench_source(w: u32, h: u32) -> LinearRgbaF32 {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[x as f32 / w as f32, y as f32 / h as f32, 0.25, 1.0]);
        }
    }
    LinearRgbaF32::new(w, h, px).expect("bench source length")
}

fn median_ms(mut samples: Vec<Duration>) -> f64 {
    samples.sort();
    let mid = samples.len() / 2;
    if samples.len().is_multiple_of(2) {
        (samples[mid - 1].as_secs_f64() + samples[mid].as_secs_f64()) / 2.0 * 1000.0
    } else {
        samples[mid].as_secs_f64() * 1000.0
    }
}

/// Independent adapter query purely to print a human-readable GPU name
/// alongside the medians (`GpuContext::headless()` doesn't expose the adapter
/// it picked, only the resulting device/queue) — requested with the same
/// `HighPerformance` preference so it should name the same adapter
/// `GpuContext::headless()` selects.
fn adapter_name() -> String {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .map(|a| a.get_info().name)
    .unwrap_or_else(|| "<unknown>".into())
}

/// Time `ITERATIONS` (`set_stack` + `evaluate`) calls, each followed by
/// `device.poll(Wait)` so the timer captures actual GPU execution, not just
/// command-buffer recording/submission. `next_stack(i)` builds iteration `i`'s
/// doc from the previous iteration's baseline (the alternating +/- delta the
/// brief specifies, so consecutive iterations don't converge to a no-op
/// steady state cached by the graph).
fn time_iterations(
    pipe: &mut EditPipeline,
    ctx: &GpuContext,
    mut next_stack: impl FnMut(usize) -> ferrolite_pipeline::OpStack,
) -> f64 {
    let mut samples = Vec::with_capacity(ITERATIONS);
    for i in 0..ITERATIONS {
        let stack = next_stack(i);
        let start = Instant::now();
        pipe.set_stack(stack);
        let _ = pipe.evaluate();
        ctx.device.poll(wgpu::Maintain::Wait);
        samples.push(start.elapsed());
    }
    median_ms(samples)
}

#[test]
#[ignore]
fn engine_bench_medians() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (expected in headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    eprintln!("GPU adapter: {}", adapter_name());

    let (w, h) = (6000u32, 4000u32);
    let src = bench_source(w, h);

    let docs = common::layer_engine::fixture_docs();
    let full_global = docs
        .iter()
        .find(|(name, _)| *name == "full_global")
        .expect("fixture_docs must include full_global")
        .1
        .clone();
    let two_masks_layers = docs
        .iter()
        .find(|(name, _)| *name == "two_masks")
        .expect("fixture_docs must include two_masks")
        .1
        .local_adjustments()
        .expect("two_masks fixture must carry LocalAdjustments layers");

    let mut pipe = EditPipeline::new(ctx.clone(), &src, full_global.clone(), IDENTITY);
    // Warm-up: the first evaluate always pays first-dispatch driver pipeline
    // compilation, which the timed iterations below must not include.
    let _ = pipe.evaluate();
    ctx.device.poll(wgpu::Maintain::Wait);

    let base_ev = full_global.exposure().map(|e| e.ev).unwrap_or(0.0);

    // (a) exposure-dirty evaluate: only the exposure node + everything
    // downstream of it re-runs.
    let median_a = time_iterations(&mut pipe, &ctx, |i| {
        let delta = if i.is_multiple_of(2) { 0.01 } else { -0.01 };
        full_global.set_op(Op::Exposure(Exposure {
            ev: base_ev + delta,
        }))
    });

    // Settle back to the steady-state `full_global` doc before the next case
    // so it isn't still mid-alternation when (b) starts.
    pipe.set_stack(full_global.clone());
    let _ = pipe.evaluate();
    ctx.device.poll(wgpu::Maintain::Wait);

    // (b) grade-dirty evaluate: only the color-grade node + everything
    // downstream of it re-runs (a different, later position in the chain than
    // exposure).
    let base_grade = full_global
        .color_grade()
        .expect("full_global fixture must carry a non-identity ColorGrade");
    let median_b = time_iterations(&mut pipe, &ctx, |i| {
        let delta = if i.is_multiple_of(2) { 0.01 } else { -0.01 };
        let mut grade = base_grade;
        grade.global.lum = delta;
        full_global.set_op(Op::ColorGrade(grade))
    });

    // (c) same as (a), but with `two_masks`' local layers added on top —
    // measures whether an upstream (exposure) dirty still reuses the
    // LocalAdjustmentsNode's mask-composite cache (keyed on mask DEFINITIONS
    // only, see `local_node.rs`) instead of paying to re-composite masks that
    // didn't change.
    let with_masks = full_global.set_op(Op::LocalAdjustments(two_masks_layers));
    pipe.set_stack(with_masks.clone());
    let _ = pipe.evaluate();
    ctx.device.poll(wgpu::Maintain::Wait);
    let median_c = time_iterations(&mut pipe, &ctx, |i| {
        let delta = if i.is_multiple_of(2) { 0.01 } else { -0.01 };
        with_masks.set_op(Op::Exposure(Exposure {
            ev: base_ev + delta,
        }))
    });

    eprintln!("=== engine bench medians over {ITERATIONS} iterations, {w}x{h} ===");
    eprintln!("(a) exposure-dirty evaluate:              {median_a:.3} ms");
    eprintln!("(b) grade-dirty evaluate:                 {median_b:.3} ms");
    eprintln!("(c) exposure-dirty + two_masks' layers:    {median_c:.3} ms");
}

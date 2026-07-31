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
use ferrolite_pipeline::{
    EditPipeline, Exposure, GpuPyramidSource, NoiseReduction, Op, OpStack, Sharpen,
};
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

    // (d) P4: NR-dirty evaluate. `noise_reduction` sits right after the
    // color-matrix — the SECOND node in the graph, upstream of the light
    // engine, dehaze, color engine, sharpen, AND geometry — so dirtying it
    // forces the entire rest of the chain to re-run too (a strict superset of
    // case (a)'s downstream cost, PLUS NR's own à trous decomposition).
    // `noise_reduction` has no dedicated `Op` variant (same category as
    // vibrance) so it is set by mutating `global` directly, matching
    // `nr_node.rs`'s test pattern, rather than `with_global` (which would
    // wipe `full_global`'s other global fields).
    let mut nr_base = full_global.clone();
    nr_base.global.noise_reduction = NoiseReduction {
        luminance: 0.6,
        color: 0.3,
        ..Default::default()
    };
    pipe.set_stack(nr_base.clone());
    let _ = pipe.evaluate();
    ctx.device.poll(wgpu::Maintain::Wait);
    let median_d = time_iterations(&mut pipe, &ctx, |i| {
        let delta = if i.is_multiple_of(2) { 0.01 } else { -0.01 };
        let mut d = nr_base.clone();
        d.global.noise_reduction.luminance = 0.6 + delta;
        d
    });

    // (e) P4: NR-dirty evaluate + sharpen's new Detail/Masking active
    // together (both P4 features exercised at once, worst-case pass count:
    // NR's decomposition + the sharpen node's extra fine-blur and gradient-
    // mask passes on top of everything (d) already re-runs).
    let mut nr_sharpen_base = nr_base.clone();
    nr_sharpen_base.global.sharpen = Sharpen {
        amount: 0.8,
        radius: 2,
        detail: 0.5,
        masking: 0.5,
    };
    pipe.set_stack(nr_sharpen_base.clone());
    let _ = pipe.evaluate();
    ctx.device.poll(wgpu::Maintain::Wait);
    let median_e = time_iterations(&mut pipe, &ctx, |i| {
        let delta = if i.is_multiple_of(2) { 0.01 } else { -0.01 };
        let mut d = nr_sharpen_base.clone();
        d.global.noise_reduction.luminance = 0.6 + delta;
        d
    });

    eprintln!("=== engine bench medians over {ITERATIONS} iterations, {w}x{h} ===");
    eprintln!("(a) exposure-dirty evaluate:              {median_a:.3} ms");
    eprintln!("(b) grade-dirty evaluate:                 {median_b:.3} ms");
    eprintln!("(c) exposure-dirty + two_masks' layers:    {median_c:.3} ms");
    eprintln!("(d) NR-dirty evaluate:                     {median_d:.3} ms");
    eprintln!("(e) NR-dirty + sharpen detail/masking:      {median_e:.3} ms");
}

/// P4 design section 3.3 / section 7.4 — the peak-GPU-memory gate that decides
/// whether NR stays on both the tile and whole-image paths, or falls back to
/// tile-path-only. `#[ignore]`d — run explicitly:
///
///   cargo test -p ferrolite-pipeline --test engine_bench nr_memory_gate -- --ignored --nocapture
///
/// Measures at the REAL resolution of the largest RAW fixture in this repo
/// (`fixtures/raw/DSC04692.ARW`, a Sony ILCE-7M2 file: 6048x4024, read via
/// `ferrolite_decode::read_metadata` — not re-decoded/demosaiced here, since
/// the memory figure only depends on pixel count, not content) rather than
/// extrapolating from a smaller synthetic size.
#[test]
#[ignore]
fn nr_memory_gate() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (expected in headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let (w, h) = (6048u32, 4024u32);
    let src = bench_source(w, h);
    let mp = (w as f64 * h as f64) / 1_000_000.0;

    // Gate 1: identity NR allocates ZERO NR textures on the whole-image path.
    let mut identity_pipe = EditPipeline::new(ctx.clone(), &src, OpStack::default(), IDENTITY);
    let _ = identity_pipe.evaluate();
    ctx.device.poll(wgpu::Maintain::Wait);
    let identity_nr_bytes = identity_pipe.nr_live_bytes();

    // Gate 2: active NR's peak allocation on the whole-image path.
    let mut doc = OpStack::default();
    doc.global.noise_reduction = NoiseReduction {
        luminance: 0.8,
        color: 0.5,
        ..Default::default()
    };
    let mut active_pipe = EditPipeline::new(ctx.clone(), &src, doc, IDENTITY);
    let _ = active_pipe.evaluate();
    ctx.device.poll(wgpu::Maintain::Wait);
    let active_nr_bytes = active_pipe.nr_live_bytes();

    // The source pyramid the develop-canvas/tile path keeps GPU-resident for
    // this SAME image concurrently in real usage — the realistic co-resident
    // total this gate is meant to bound, not NR's allocation in isolation.
    // Read via the process-wide `live_gpu_pyramid_bytes()` gauge (this test
    // runs alone under `--ignored`, so no other pyramid pollutes the reading).
    let pyramid = GpuPyramidSource::new(&ctx, &src);
    let pyramid_bytes = ferrolite_pipeline::live_gpu_pyramid_bytes();
    let total_peak_bytes = pyramid_bytes + active_nr_bytes;
    drop(pyramid);

    let gib = |b: u64| b as f64 / (1024.0 * 1024.0 * 1024.0);
    eprintln!("=== NR memory gate at {w}x{h} ({mp:.1} MP, real DSC04692.ARW resolution) ===");
    eprintln!("identity NR bytes:            {identity_nr_bytes} (must be 0)");
    eprintln!(
        "active NR bytes:              {active_nr_bytes} ({:.3} GiB)",
        gib(active_nr_bytes)
    );
    eprintln!(
        "resident source pyramid bytes: {pyramid_bytes} ({:.3} GiB)",
        gib(pyramid_bytes)
    );
    eprintln!(
        "total peak (pyramid + active NR): {total_peak_bytes} ({:.3} GiB)",
        gib(total_peak_bytes)
    );
    assert_eq!(
        identity_nr_bytes, 0,
        "identity NR must allocate no textures on the whole-image path"
    );
}

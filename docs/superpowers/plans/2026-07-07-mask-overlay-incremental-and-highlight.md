# Mask overlay: incremental composite + component highlight — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Develop mask overlay smooth on large masks (100–500+ components) by re-evaluating only the component that changed each frame, and let hovering a Components-modal row highlight that component's coverage in white on the canvas.

**Architecture:** Two building blocks in `ferrolite-mask` — (1) batch the composite fold into one encoder + one submit (ping-pong, no per-step submit/alloc storm); (2) a per-component coverage cache (`ComponentCache`) so `MaskCompositor::composite_cached` re-evaluates only components whose params changed. `ferrolite-pipeline`'s `MaskOverlayCompositor` uses the cache and gains a parameterized tint color (red overlay, white highlight). `ferrolite-app` wires hover-highlight (bold modal row + white canvas coverage).

**Tech Stack:** Rust, wgpu (`ferrolite-gpu`), egui/egui-wgpu 0.29, `ferrolite-mask`, `ferrolite-pipeline`, `ferrolite-app`.

## Global Constraints

- **Never block the UI/update thread** (CLAUDE.md §1): no readback / `device.poll(Wait)` on the overlay path. Composite + tint stay async submits.
- **Build GPU pipelines once, reuse** (CLAUDE.md §2): all pipelines built in their `::new`; never per frame.
- **Correctness invariant:** the incremental (cached) composite MUST produce a byte-identical coverage to the from-scratch composite for the same `MaskDefinition`. This is the load-bearing test.
- **Behavior preservation:** the red overlay looks/aligns exactly as today (premultiplied red, strength 0.5, `Rgba8Unorm` target + `Rgba8UnormSrgb` view for egui). Highlight is white, drawn over the red, and shows even when the red-overlay toggle is off; it does NOT force the red overlay on.
- **Non-goals:** do NOT modify `LocalAdjustmentsNode` / the preview pipeline (measured cheap, already cached). Do NOT add intermediate-accumulator fold caching (deferred). No sidecar/persistence change (highlight is transient UI state).
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → HOLD for the author's visual test.
- Branch: `fix/brush-mask-perf`. The `FERROLITE_BRUSH_PROFILE` instrumentation currently on the branch is used for measure-after in Task 5, then removed.

---

## Task 1: Batch the composite fold (ferrolite-mask)

Make `CompositePass::composite` record the whole fold chain (+ optional invert) into ONE command encoder with a two-buffer ping-pong and a SINGLE `queue.submit`, instead of one alloc + encoder + submit per fold step. Behavior-preserving; golden-verified.

**Files:**
- Modify: `ferrolite-mask/src/composite.rs`
- Test: existing `#[cfg(test)]` goldens in the mask crate already cover fold modes (`composite`); this task must keep them green. Add one new golden if noted below.

**Interfaces:**
- Consumes: `MaskBuffer` (`::alloc(ctx,w,h)`, `.texture`, `.width`, `.height`), the existing `fold`/`invert` WGSL + bind group layouts (`fold_bgl`, `fold_pipeline`, `invert_bgl`, `invert_pipeline`).
- Produces: unchanged public signature `CompositePass::composite(&self, inputs: &[(MaskBuffer, CompositeMode)], invert: bool) -> MaskBuffer` — same output, one submit.

- [ ] **Step 1: Add a failing golden that pins multi-input fold correctness under batching**

In `ferrolite-mask/src/composite.rs` `#[cfg(test)]`, add (if an equivalent doesn't already exist) a test that folds ≥3 constant buffers with mixed modes and checks the result against the CPU `composite_scalar` reference, so the batched rewrite is held to the same output:

```rust
#[test]
fn batched_composite_matches_scalar_reference_three_inputs() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let pass = CompositePass::new(ctx.clone());
    // 2x2 constant buffers: 0.8 (seed, Add), 0.5 (Subtract), 0.3 (Intersect)
    let a = const_buf(&ctx, 2, 2, 0.8);
    let b = const_buf(&ctx, 2, 2, 0.5);
    let c = const_buf(&ctx, 2, 2, 0.3);
    let out = pass.composite(
        &[
            (a, crate::model::CompositeMode::Add),
            (b, crate::model::CompositeMode::Subtract),
            (c, crate::model::CompositeMode::Intersect),
        ],
        false,
    );
    // scalar reference: intersect(subtract(0.8, 0.5), 0.3)
    let want = crate::model::composite_scalar(
        &[
            (0.8, crate::model::CompositeMode::Add),
            (0.5, crate::model::CompositeMode::Subtract),
            (0.3, crate::model::CompositeMode::Intersect),
        ],
        false,
    );
    let got = crate::compositor::read_mask_r32f(&ctx, &out);
    assert!(got.iter().all(|&v| (v - want).abs() < 1e-4), "got {:?} want {want}", &got[..1]);
}
```

Add a `const_buf` test helper if not present (mirror the `constant_buffer` helper in `compositor.rs` tests):

```rust
#[cfg(test)]
fn const_buf(ctx: &Arc<GpuContext>, w: u32, h: u32, v: f32) -> MaskBuffer {
    let buf = MaskBuffer::alloc(ctx, w, h);
    let data = vec![v; (w * h) as usize];
    ctx.queue.write_texture(
        wgpu::ImageCopyTexture { texture: &buf.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        bytemuck::cast_slice(&data),
        wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    buf
}
```

Verify `composite_scalar` exists and its signature (it's re-exported from `model`); adjust the reference call to match its actual signature if different.

- [ ] **Step 2: Run it (should pass on the OLD code — this pins behavior before the refactor)**

Run: `cargo test -p ferrolite-mask batched_composite_matches_scalar_reference -- --nocapture`
Expected: PASS on the current (unbatched) `composite` (the test documents the target behavior; it guards the refactor). If it fails, the reference is wrong — fix the test's expected value, not the code.

- [ ] **Step 3: Rewrite `composite` to one encoder + ping-pong + one submit**

Replace `CompositePass::composite` (and refactor `fold_into`/`invert` to record into a caller-provided encoder rather than each submitting). Add private helpers `record_fold(&self, enc, acc_view, b_view, out, mode)` and `record_invert(&self, enc, src_view, out)` that only *record* a compute pass (no submit); keep `dispatch` for any remaining single-shot callers or inline the recording. New `composite`:

```rust
pub fn composite(&self, inputs: &[(MaskBuffer, CompositeMode)], invert: bool) -> MaskBuffer {
    assert!(!inputs.is_empty(), "composite requires >= 1 input buffer");
    let (w, h) = (inputs[0].0.width, inputs[0].0.height);

    // Single input, no invert: nothing to compute — hand back the seed.
    if inputs.len() == 1 && !invert {
        return inputs[0].0.clone();
    }

    // Two scratch buffers; ping-pong so read-tex != write-tex each step (and the
    // cached input buffers are never written). Only 2 allocs regardless of N.
    let scratch = [MaskBuffer::alloc(&self.ctx, w, h), MaskBuffer::alloc(&self.ctx, w, h)];
    let mut enc = self
        .ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("mask-composite") });

    // Fold: step k reads the previous accumulator, writes the other scratch.
    // acc for step 1 is the seed (inputs[0]); afterwards it alternates scratch.
    let mut acc_tex = &inputs[0].0.texture;
    let mut cur = 0usize;
    for (buf, mode) in &inputs[1..] {
        self.record_fold(&mut enc, acc_tex, &buf.texture, &scratch[cur], *mode);
        acc_tex = &scratch[cur].texture;
        cur ^= 1;
    }
    // `cur` now points at the free scratch; the last write went to scratch[cur ^ 1].
    let mut result_idx = cur ^ 1;

    if invert {
        // Read the last accumulator, write the free scratch.
        let src_tex = if inputs.len() == 1 { &inputs[0].0.texture } else { &scratch[result_idx].texture };
        self.record_invert(&mut enc, src_tex, &scratch[cur]);
        result_idx = cur;
    }

    self.ctx.queue.submit([enc.finish()]);
    scratch[result_idx].clone()
}
```

Where `record_fold` mirrors the current `fold_into` body but takes `enc: &mut wgpu::CommandEncoder`, builds the bind group + mode uniform, and records a `begin_compute_pass` + dispatch WITHOUT `queue.submit`. `record_invert` likewise mirrors `invert`. (wgpu inserts automatic memory barriers between separate compute passes in one encoder, so each ping-pong step sees the previous step's writes.) Keep the WGSL, bind group layouts, and pipelines unchanged.

> Note the `invert && inputs.len()==1` case: acc is the seed, invert it into a scratch. The code above handles it (the `inputs.len()==1 && !invert` early-return covers the no-invert single-input case; single-input WITH invert falls through, does zero fold steps, then the invert branch reads `inputs[0]`).

- [ ] **Step 4: Run the mask crate tests (existing fold/invert goldens + the new one)**

Run: `cargo test -p ferrolite-mask -- --nocapture`
Expected: all pass (fold add/subtract/intersect, invert, the new 3-input golden). These prove the batched output equals the previous behavior.

- [ ] **Step 5: Format, lint, commit**

Run: `cargo fmt && cargo clippy -p ferrolite-mask --all-targets -- -D warnings`

```bash
git add ferrolite-mask/src/composite.rs
git commit -m "perf(mask): batch composite fold into one encoder + submit (ping-pong)"
```

---

## Task 2: Per-component coverage cache (ferrolite-mask)

Add a `ComponentCache` and `MaskCompositor::composite_cached` that re-evaluate only components whose params changed since the last call, reusing cached coverage buffers for the rest, then fold (via Task 1's batched `composite`).

**Files:**
- Modify: `ferrolite-mask/src/compositor.rs` (add cache + `composite_cached`; refactor the empty-def handling into a helper reused by both `composite` and `composite_cached`)
- Modify: `ferrolite-mask/src/lib.rs` (re-export `ComponentCache`)
- Test: inline `#[cfg(test)]` in `compositor.rs`

**Interfaces:**
- Consumes: the private `MaskCompositor::eval(comp, input_view, w, h, rasters) -> MaskBuffer`, `MaskCompositor::composite`, `CompositePass::composite` (batched, Task 1), `MaskComponent`, `CompositeMode`, `MaskBuffer`.
- Produces (used by Task 3):
  - `pub struct ComponentCache { /* private */ }` with `pub fn new() -> Self` (+ `Default`), and `pub fn coverage(&self, index: usize) -> Option<&MaskBuffer>` (for the highlight to fetch one component's cached buffer).
  - `MaskCompositor::composite_cached(&self, def: &MaskDefinition, input: &wgpu::TextureView, input_id: u64, w: u32, h: u32, rasters: &RasterStore, cache: &mut ComponentCache) -> MaskBuffer`.

- [ ] **Step 1: Write the failing correctness test (cached == from-scratch, incl. after a mutation)**

In `compositor.rs` `#[cfg(test)]`:

```rust
#[test]
fn composite_cached_matches_full_composite_and_after_mutation() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let comp = MaskCompositor::new(ctx.clone());
    let input = MaskBuffer::alloc_zeroed(&ctx, 16, 16);
    let iv = input.texture.create_view(&wgpu::TextureViewDescriptor::default());

    // A 3-component def: radial (Add), linear (Add), radial (Subtract).
    let mk = |cx: f32| MaskDefinition {
        components: vec![
            (MaskComponent::RadialGradient { center: crate::vec::Vec2::new(cx, 0.5), radius: crate::vec::Vec2::new(0.3, 0.3), rotation: 0.0, feather: 0.3, invert: false }, CompositeMode::Add),
            (MaskComponent::LinearGradient { start: crate::vec::Vec2::new(0.0, 0.5), end: crate::vec::Vec2::new(1.0, 0.5) }, CompositeMode::Add),
            (MaskComponent::RadialGradient { center: crate::vec::Vec2::new(0.7, 0.5), radius: crate::vec::Vec2::new(0.2, 0.2), rotation: 0.0, feather: 0.3, invert: false }, CompositeMode::Subtract),
        ],
        invert: false,
    };
    let def = mk(0.3);
    let mut cache = ComponentCache::new();
    let cached = comp.composite_cached(&def, &iv, 1, 16, 16, &RasterStore::default(), &mut cache);
    let full = comp.composite(&def, &iv, 16, 16, &RasterStore::default());
    assert_eq!(read_mask_r32f(&ctx, &cached), read_mask_r32f(&ctx, &full), "cached == full (initial)");

    // Mutate ONLY the first component (move the radial center); cached must still
    // equal a fresh full composite of the mutated def (proves selective re-eval).
    let def2 = mk(0.6);
    let cached2 = comp.composite_cached(&def2, &iv, 1, 16, 16, &RasterStore::default(), &mut cache);
    let full2 = comp.composite(&def2, &iv, 16, 16, &RasterStore::default());
    assert_eq!(read_mask_r32f(&ctx, &cached2), read_mask_r32f(&ctx, &full2), "cached == full (after mutation)");
}
```

- [ ] **Step 2: Run it, verify it fails to compile**

Run: `cargo test -p ferrolite-mask composite_cached_matches_full -- --nocapture`
Expected: FAIL — `ComponentCache` / `composite_cached` not found.

- [ ] **Step 3: Implement `component_hash`, `ComponentCache`, `composite_cached`**

In `compositor.rs`:

```rust
use std::hash::{Hash, Hasher};

/// Cheap, allocation-free structural hash of a component's params (f32 by bits —
/// f32 isn't Hash). Used to detect which components changed between frames so the
/// cache re-evaluates only those. NOT serde (that was the O(n) UI-thread cost).
fn component_hash(c: &MaskComponent) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    fn f(h: &mut impl Hasher, x: f32) { x.to_bits().hash(h); }
    match c {
        MaskComponent::LinearGradient { start, end } => {
            0u8.hash(&mut h); f(&mut h, start.x); f(&mut h, start.y); f(&mut h, end.x); f(&mut h, end.y);
        }
        MaskComponent::RadialGradient { center, radius, rotation, feather, invert } => {
            1u8.hash(&mut h);
            f(&mut h, center.x); f(&mut h, center.y); f(&mut h, radius.x); f(&mut h, radius.y);
            f(&mut h, *rotation); f(&mut h, *feather); invert.hash(&mut h);
        }
        MaskComponent::LumaRange { lo, hi, softness } => {
            2u8.hash(&mut h); f(&mut h, *lo); f(&mut h, *hi); f(&mut h, *softness);
        }
        MaskComponent::ColorRange { samples, tolerance, softness } => {
            3u8.hash(&mut h);
            for s in samples { f(&mut h, s.r); f(&mut h, s.g); f(&mut h, s.b); }
            f(&mut h, *tolerance); f(&mut h, *softness);
        }
        MaskComponent::Brush { strokes } => {
            4u8.hash(&mut h);
            for st in strokes {
                st.erase.hash(&mut h);
                for n in &st.nodes { f(&mut h, n.pos.x); f(&mut h, n.pos.y); f(&mut h, n.radius); f(&mut h, n.hardness); f(&mut h, n.flow); }
            }
        }
        MaskComponent::Imported { handle, .. } => { 5u8.hash(&mut h); handle.0.hash(&mut h); }
    }
    h.finish()
}

/// Per-component coverage cache for incremental overlay compositing. Reused across
/// frames; only components whose `component_hash` changed are re-evaluated. Slot i
/// corresponds to component i. `input_id` guards range shapes (which sample the
/// input image): a new input clears the cache.
#[derive(Default)]
pub struct ComponentCache {
    input_id: u64,
    dims: (u32, u32),
    slots: Vec<(u64, MaskBuffer)>, // (component_hash, coverage)
}

impl ComponentCache {
    pub fn new() -> Self { Self::default() }
    /// The cached coverage of component `index`, if evaluated this generation.
    pub fn coverage(&self, index: usize) -> Option<&MaskBuffer> {
        self.slots.get(index).map(|(_, b)| b)
    }
    fn reset_if_stale(&mut self, input_id: u64, dims: (u32, u32)) {
        if self.input_id != input_id || self.dims != dims {
            self.slots.clear();
            self.input_id = input_id;
            self.dims = dims;
        }
    }
}

impl MaskCompositor {
    /// Empty-def coverage (extracted from `composite` for reuse): full (ones) or
    /// zeroed if inverted.
    fn empty_coverage(&self, invert: bool, w: u32, h: u32) -> MaskBuffer {
        if invert { MaskBuffer::alloc_zeroed(&self.ctx, w, h) } else { self.ones(w, h) }
    }

    /// Incremental composite: evaluate only components whose params changed since
    /// the last call (per `cache`), reuse the rest, then fold. Byte-identical to
    /// `composite` for the same `def`. `input_id` identifies the input image
    /// (range shapes sample it) — pass a value that changes when the input does.
    pub fn composite_cached(
        &self,
        def: &MaskDefinition,
        input: &wgpu::TextureView,
        input_id: u64,
        w: u32,
        h: u32,
        rasters: &RasterStore,
        cache: &mut ComponentCache,
    ) -> MaskBuffer {
        if def.components.is_empty() {
            cache.slots.clear();
            return self.empty_coverage(def.invert, w, h);
        }
        cache.reset_if_stale(input_id, (w, h));
        cache.slots.truncate(def.components.len());
        for (i, (comp, _mode)) in def.components.iter().enumerate() {
            let hash = component_hash(comp);
            match cache.slots.get(i) {
                Some((h0, _)) if *h0 == hash => { /* reuse */ }
                _ => {
                    let cov = self.eval(comp, input, w, h, rasters);
                    if i < cache.slots.len() { cache.slots[i] = (hash, cov); }
                    else { cache.slots.push((hash, cov)); }
                }
            }
        }
        let inputs: Vec<(MaskBuffer, CompositeMode)> = def
            .components
            .iter()
            .enumerate()
            .map(|(i, (_, m))| (cache.slots[i].1.clone(), *m))
            .collect();
        self.composite.composite(&inputs, def.invert)
    }
}
```

Refactor the existing `composite` to call `self.empty_coverage(def.invert, w, h)` in its empty branch (DRY). Make `eval` callable from `composite_cached` (it already is — same impl block). `component_hash` stays private.

- [ ] **Step 4: Re-export `ComponentCache`**

In `ferrolite-mask/src/lib.rs`, add to the `compositor` re-export:

```rust
pub use compositor::{read_mask_r32f, ComponentCache, MaskCompositor};
```

- [ ] **Step 5: Run the test, verify it passes**

Run: `cargo test -p ferrolite-mask composite_cached_matches_full -- --nocapture`
Expected: PASS. Also run `cargo test -p ferrolite-mask` (all green).

- [ ] **Step 6: Format, lint, commit**

Run: `cargo fmt && cargo clippy -p ferrolite-mask --all-targets -- -D warnings`

```bash
git add ferrolite-mask/src/compositor.rs ferrolite-mask/src/lib.rs
git commit -m "perf(mask): per-component coverage cache (composite_cached)"
```

---

## Task 3: Overlay compositor uses the cache; parameterized tint color + highlight (ferrolite-pipeline)

`MaskOverlayCompositor` composites via `composite_cached` (owning a `ComponentCache`), gains a tint COLOR param (red overlay / white highlight), and a `highlight_texture` that white-tints a single cached component's coverage.

**Files:**
- Modify: `ferrolite-pipeline/src/mask_overlay.rs`
- Modify: `ferrolite-pipeline/src/shaders/mask_overlay_tint.wgsl`
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `ferrolite_mask::{ComponentCache, MaskCompositor}` (`composite_cached`, `ComponentCache::coverage`), the existing tint pipeline + `OverlayTexture`.
- Produces (used by Task 4):
  - `MaskOverlayCompositor::overlay_texture(&mut self, def, input, input_id: u64, strength: f32) -> OverlayTexture` — now `&mut self` (owns the cache), red tint.
  - `MaskOverlayCompositor::highlight_texture(&self, component: usize, strength: f32) -> Option<OverlayTexture>` — white-tint of that component's cached coverage; `None` if the index isn't cached.
  - `overlay_tint(coverage: f32, strength: f32, color: [f32; 3]) -> [f32; 4]` (color-parameterized; red = `[1,0,0]`, white = `[1,1,1]`).

- [ ] **Step 1: Failing test — `overlay_tint` takes a color**

Replace the existing `overlay_tint_is_premultiplied_red_and_clamped` test's calls and add color coverage:

```rust
#[test]
fn overlay_tint_is_premultiplied_and_color_parameterized() {
    // red
    assert_eq!(overlay_tint(0.0, 0.5, [1.0, 0.0, 0.0]), [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(overlay_tint(1.0, 0.5, [1.0, 0.0, 0.0]), [0.5, 0.0, 0.0, 0.5]);
    // white: all channels premultiplied by alpha
    assert_eq!(overlay_tint(1.0, 0.7, [1.0, 1.0, 1.0]), [0.7, 0.7, 0.7, 0.7]);
    // clamp
    assert_eq!(overlay_tint(1.5, 2.0, [1.0, 1.0, 1.0]), [1.0, 1.0, 1.0, 1.0]);
}
```

- [ ] **Step 2: Run it, verify it fails (arity mismatch)**

Run: `cargo test -p ferrolite-pipeline overlay_tint_is_premultiplied -- --nocapture`
Expected: FAIL to compile (`overlay_tint` takes 2 args).

- [ ] **Step 3: Parameterize `overlay_tint` + the WGSL uniform**

`overlay_tint`:

```rust
pub fn overlay_tint(coverage: f32, strength: f32, color: [f32; 3]) -> [f32; 4] {
    let a = coverage.clamp(0.0, 1.0) * strength.clamp(0.0, 1.0);
    [color[0] * a, color[1] * a, color[2] * a, a]
}
```

In `mask_overlay_tint.wgsl`, extend `TintParams` and the fragment output:

```wgsl
struct TintParams { color: vec3<f32>, strength: f32 };   // 16 bytes
@group(0) @binding(1) var<uniform> params: TintParams;

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = vec2<i32>(i32(frag.x), i32(frag.y));
    let c = textureLoad(coverage, px, 0).r;
    let a = clamp(c, 0.0, 1.0) * params.strength;
    return vec4<f32>(params.color * a, a); // premultiplied
}
```

Update the Rust uniform struct to match (`{ color: [f32;3], strength: f32 }`, 16 bytes, `#[repr(C)]` + `bytemuck`), and change the params buffer write to pack `color` then `strength`. The red overlay passes `color = [1.0, 0.0, 0.0]`.

- [ ] **Step 4: Cache-backed `overlay_texture` + `highlight_texture`**

Add a `ComponentCache` field to `MaskOverlayCompositor` (init `ComponentCache::new()` in `new`). Factor the "tint a coverage buffer with a color into a fresh `OverlayTexture`" render-pass work into a private `fn tint(&self, coverage: &MaskBuffer, color: [f32;3], strength: f32) -> OverlayTexture` (this is the existing render-pass body, now taking a coverage buffer + color). Then:

```rust
pub fn overlay_texture(
    &mut self,
    def: &ferrolite_mask::MaskDefinition,
    input: &PipelineImage,
    input_id: u64,
    strength: f32,
) -> OverlayTexture {
    let (w, h) = (input.width, input.height);
    let iv = input.texture.create_view(&wgpu::TextureViewDescriptor::default());
    let coverage = self.compositor.composite_cached(
        def, &iv, input_id, w, h, &RasterStore::default(), &mut self.cache,
    );
    self.tint(&coverage, [1.0, 0.0, 0.0], strength)
}

/// White-tint of a single component's cached coverage (from the last
/// `overlay_texture` call). `None` if `component` wasn't cached (e.g. index out
/// of range, or an empty def).
pub fn highlight_texture(&self, component: usize, strength: f32) -> Option<OverlayTexture> {
    let cov = self.cache.coverage(component)?;
    Some(self.tint(cov, [1.0, 1.0, 1.0], strength))
}
```

> `input_id`: the app passes a value that changes when the overlay input changes (e.g. a generation counter bumped when `mask_overlay_input` is rebuilt, or the input texture's `Arc` pointer address). Task 4 supplies it.

- [ ] **Step 5: Golden — highlight white-tint of a known component**

Add a GPU golden (auto-skip headless): build an overlay for a 2-component def (e.g. two radials) via `overlay_texture`, then `highlight_texture(0, 0.7)`; read the linear bytes and assert `r==g==b==a` (premultiplied white) and alpha ramps with that component's coverage. (Mirror the Task-1 GPU-overlay golden's readback via `ctx.read_rgba8`.)

- [ ] **Step 6: Run tests, format, lint, commit**

Run: `cargo test -p ferrolite-pipeline -- --nocapture` (all green), then `cargo fmt && cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings`.

```bash
git add ferrolite-pipeline/src/mask_overlay.rs ferrolite-pipeline/src/shaders/mask_overlay_tint.wgsl
git commit -m "feat(pipeline): cache-backed overlay composite + white component highlight"
```

---

## Task 4: Wire hover-highlight in the app (ferrolite-app)

Add `highlight_component` UI state; bold the hovered Components-modal row and set the index; build/update a second app-global native texture (white highlight) and draw it over the red overlay; supply `input_id` to `overlay_texture`.

**Files:**
- Modify: `ferrolite-app/src/develop/mask_ui.rs` (add `highlight_component: Option<usize>`)
- Modify: `ferrolite-app/src/develop/mask_components_modal.rs` (row hover → bold + set index; clear when none hovered)
- Modify: `ferrolite-app/src/state.rs` (add `mask_overlay_highlight_native: Option<egui::TextureId>` + `mask_overlay_highlight_gpu: Option<ferrolite_pipeline::OverlayTexture>`)
- Modify: `ferrolite-app/src/app.rs` (`rebuild_mask_overlay_if_needed`: pass `input_id`, `&mut` compositor, build/free the highlight texture)
- Modify: `ferrolite-app/src/develop/mask_overlay.rs` (`show`: draw the highlight texture over the red)
- Modify: `ferrolite-app/src/develop/tools/mask.rs` (pass the highlight `TextureId`)

**Interfaces:**
- Consumes: `MaskOverlayCompositor::overlay_texture(&mut self, .., input_id, ..)`, `::highlight_texture(component, strength)`, `OverlayTexture::srgb_view()`.
- Produces: `MaskUiState.highlight_component`; a second native overlay texture drawn over the red.

- [ ] **Step 1: Add `highlight_component` to `MaskUiState`**

In `mask_ui.rs`, add `pub highlight_component: Option<usize>` to `MaskUiState` and initialize it `None` in its constructor/`Default`.

- [ ] **Step 2: Modal — bold hovered row + set `highlight_component`**

In `mask_components_modal.rs`, before the component loop set a local `let mut hovered: Option<usize> = None;`. In each row, wrap the label so its text bolds when the row is hovered, and record the hover. Since the row is built inside `ui.horizontal(|ui| …)`, capture the horizontal's response and test hover:

```rust
let row = ui.horizontal(|ui| {
    let hovered_now = mask.highlight_component == Some(i);
    let label = egui::RichText::new(format!("{}. {}  [{:?}]", i + 1, component_label(comp), mode));
    ui.label(if hovered_now { label.strong() } else { label });
    // …existing right-to-left Remove/Edit buttons…
});
if row.response.hovered() { hovered = Some(i); }
```

After the loop (still inside the ScrollArea or just after it), commit the hover to state:

```rust
mask.highlight_component = hovered;
```

> The bold uses the PREVIOUS frame's `highlight_component` (set at end of the prior frame) — a one-frame lag on the bold is imperceptible and avoids a second pass. If you prefer zero-lag bold, compute `hovered` first with `ui.rect_contains_pointer(row.response.rect)` isn't available before building; the one-frame approach is simplest and fine. Clear happens naturally (`hovered = None` when no row is under the pointer).
- When the modal is closed / nothing selected (the early-returns at the top of `show`), also set `mask.highlight_component = None`.

- [ ] **Step 3: Add the highlight native-texture fields to `AppState`**

In `state.rs`, next to `mask_overlay_native`/`mask_overlay_gpu`, add:

```rust
    pub mask_overlay_highlight_native: Option<egui::TextureId>,
    pub mask_overlay_highlight_gpu: Option<ferrolite_pipeline::OverlayTexture>,
```

Init both `None` in the two `AppState` constructor sites (the `new` `Ok(Self { .. })` and the test-helper literal — grep for `mask_overlay_native:` to find both).

- [ ] **Step 4: `rebuild_mask_overlay_if_needed` — input_id, &mut compositor, highlight build**

In `app.rs`:
- The compositor call is now `&mut`: `v.mask_overlay.as_ref()` → `v.mask_overlay.as_mut()`. Supply `input_id`: use the overlay input texture's pointer as a stable id, e.g. compute before the borrow split `let input_id = std::sync::Arc::as_ptr(&input.texture) as u64;` (the `PipelineImage.texture` is `Arc<wgpu::Texture>`). Pass it into `overlay_texture(&def, input, input_id, OVERLAY_STRENGTH)`.
- Fold `highlight_component` into the overlay rebuild key so the highlight texture rebuilds when the hovered component changes: `v.mask.highlight_component.hash(&mut h);`.
- After building/updating the red native texture (existing code), build the highlight: if `v.mask.highlight_component` is `Some(idx)`, call `oc.highlight_texture(idx, HIGHLIGHT_STRENGTH)`; register/update `self.state.mask_overlay_highlight_native` the same register-once/update-in-place way as the red one, and store the `OverlayTexture` in `self.state.mask_overlay_highlight_gpu`. If `None`, leave the id registered but mark "no highlight this frame" — simplest: track whether to draw via `mask.highlight_component.is_some()` at draw time (the stale texture simply isn't drawn). Add `pub const HIGHLIGHT_STRENGTH: f32 = 0.7;` next to `OVERLAY_STRENGTH` in `mask_overlay_color.rs`.

> Borrow discipline: same as the existing code — compute the `OverlayTexture`s inside the `v` scope, drop `v`, then touch `self.state.*` + `rs.renderer.write()`. `overlay_texture` needs `&mut` on the compositor (which lives on `v`), and `highlight_texture` needs `&self` — call both while `v` is borrowed, return the two `OverlayTexture`s out of the scope.

- [ ] **Step 5: `show` draws the highlight over the red**

In `mask_overlay.rs`, add a parameter `highlight_tex: Option<egui::TextureId>` and, after the red fill draw (and only when `mask.highlight_component.is_some()`), draw it with the same `ui.painter().image(id, image_rect, Rect(0..1), WHITE)`. The highlight draws regardless of `mask.overlay_on` (so hovering answers "which one" even with the red overlay off) — guard the red fill on `overlay_on && !adjusting` as today, but draw the highlight whenever `highlight_component.is_some()` and a highlight texture exists.

- [ ] **Step 6: `MaskTool::canvas` passes the highlight id**

In `develop/tools/mask.rs`, extract `state.mask_overlay_highlight_native` alongside `state.mask_overlay_native` and pass it as the new `highlight_tex` arg to `mask_overlay::show`. (If a second `mask_overlay::show(` call site exists in `app.rs`, update it too.)

- [ ] **Step 7: Build, test, lint, commit**

Run: `cargo build -p ferrolite-app --bin ferrolite-app`, then `cargo test -p ferrolite-app -p ferrolite-pipeline`, then `cargo fmt && cargo clippy -p ferrolite-app --all-targets -- -D warnings`.

```bash
git add ferrolite-app/src/develop/mask_ui.rs ferrolite-app/src/develop/mask_components_modal.rs ferrolite-app/src/state.rs ferrolite-app/src/app.rs ferrolite-app/src/develop/mask_overlay.rs ferrolite-app/src/develop/tools/mask.rs ferrolite-app/src/develop/mask_overlay_color.rs
git commit -m "feat(develop): hover a component to highlight its coverage (white) + bold its row"
```

---

## Task 5: Measure-after, remove instrumentation, verify, hand off

**Files:**
- Modify: `ferrolite-app/src/app.rs` (remove the two round-2 `FERROLITE_BRUSH_PROFILE` probe blocks in `rebuild_mask_overlay_if_needed` and `set_preview_and_full`)
- Modify: `ferrolite-app/src/diag.rs` (remove `brush_profile_enabled`)

- [ ] **Step 1: (Author-assisted) measure-after — CONTROLLER handles this; implementer skips**

The controller runs the measure-after with the author (re-profile the 190-component mask, confirm `overlay_texture` is now flat/small while painting AND dragging a component slider). The implementer for this task does Steps 2–5 only.

- [ ] **Step 2: Remove the round-2 probe blocks**

In `app.rs`, delete the `// TEMP brush-perf probe (round 2)` block around `oc.overlay_texture(...)` in `rebuild_mask_overlay_if_needed` (keep the plain `overlay_texture` call) and the `// TEMP brush-perf probe (round 2)` block around `ep.evaluate()` in `set_preview_and_full` (keep the plain `let img = ep.evaluate();`).

- [ ] **Step 3: Remove `brush_profile_enabled` from diag**

In `diag.rs`, delete the `brush_profile_enabled()` fn + doc comment (added for this round). Confirm no remaining references: `grep -rn "brush_profile_enabled\|FERROLITE_BRUSH_PROFILE\|brush-perf" ferrolite-app/src` returns nothing.

- [ ] **Step 4: Full workspace gate**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all green. Fix any unused-import fallout from the deletions.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "chore(develop): remove round-2 brush-perf instrumentation"
```

---

## Self-Review

**Spec coverage:**
- §2 per-component coverage cache (re-eval only changed) → Task 2 (`composite_cached` + `ComponentCache`). ✓
- §2.1 one encoder/submit + no zeroed-upload storm → Task 1 (batched fold; the `alloc_zeroed` per-buffer upload is avoided because cached buffers are reused and the fold no longer allocs per step — note the shape/brush `eval` still uses `alloc_zeroed` for the ONE dirty component, which is fine). ✓
- §2.2 cache invalidation (count change, input change via `input_id`, hash) → Task 2 (`reset_if_stale`, `truncate`, per-slot hash). ✓
- §3 hover-highlight white + bold row, any component, independent of red toggle → Tasks 3 (`highlight_texture`, white) + 4 (state, modal bold, draw). ✓
- §5 error handling (bounds-checked highlight index, empty def) → Task 2 (`coverage` returns Option, empty-def clears slots) + Task 3 (`highlight_texture` returns None). ✓
- §6 correctness golden (cached == full) → Task 2 Step 1; white-tint golden → Task 3 Step 5; measure-after + instrumentation removal → Task 5. ✓
- §7 non-goals (no LocalAdjustmentsNode change; no accumulator caching) → respected. ✓

**Placeholder scan:** none — code given for every implementation step. GPU boilerplate that mirrors existing code (bind group layouts in `record_fold`/`tint`) is explicitly directed to mirror the current `fold_into`/tint-pass bodies.

**Type consistency:** `ComponentCache` (new/coverage) defined in Task 2, consumed in Task 3. `composite_cached(def, input, input_id, w, h, rasters, &mut cache)` signature consistent T2↔T3. `overlay_texture(&mut self, def, input, input_id, strength)` and `highlight_texture(component, strength)` consistent T3↔T4. `overlay_tint(coverage, strength, color)` consistent T3 (test + impl). `highlight_component: Option<usize>` consistent T4 across mask_ui/modal/app/show. `input_id: u64` via `Arc::as_ptr` consistent T3↔T4.

**Note (batched fold + cached buffers):** Task 1's ping-pong writes only scratch buffers, never the input buffers — this is REQUIRED because Task 2 passes cached component buffers as fold inputs that must not be mutated. Called out in Task 1 Step 3.

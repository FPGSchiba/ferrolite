# P1 Masking — Plan 5: AI-mask hand-off seam Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `MaskComponent::Imported { handle, provenance }` composite like any other mask component (Add/Subtract/Intersect/invert) via an engine-tier raster-resolution seam that carries **no producer**, and prove with tests that the variant + its provenance are forward-compatible so A2 is purely additive.

**Architecture:** Plans 1–4 already defined and serialized the `Imported` variant, `MaskProvenance`, and `RasterHandle` (in `ferrolite-mask/src/model.rs`), and the `frl:ops` sidecar already round-trips them (`ferrolite-pipeline/tests/local_persistence.rs` exercises `Imported`). The single remaining engine gap is `MaskCompositor::eval`, whose `Imported` arm returns a zeroed buffer with the comment *"Plan 5 wires it."* This plan introduces a **`RasterStore`** — a runtime, non-serialized registry mapping `RasterHandle → MaskBuffer` — threaded into `MaskCompositor::composite`. When a handle resolves, the imported raster folds through the existing add/subtract/intersect/invert compositing exactly like a shape buffer ("refine/combine is free"); with no producer the store is empty and `Imported` stays inert (identity/zero), the P1 default. The raster is a re-derivable cache (contract 2) — **only the prompt persists**, never pixels. No `ort`, no weights, no model files anywhere.

**Tech Stack:** Rust, `wgpu` (compute), `serde` / `serde_json`, `ferrolite-gpu` (`GpuContext`, headless test adapter), `bytemuck`.

## Global Constraints

- **Engine tier, weight-free (map D6/D7):** `ferrolite-mask` carries **no copyleft deps and no model weights**. This plan adds **no** `ort`, no model files, no producer, and no `ferrolite-ai` dependency. `RasterStore` is a plain `HashMap<RasterHandle, MaskBuffer>` — no AI contamination.
- **Contract 2 — parametric is source of truth, rasters are caches:** no rasterized mask ever enters the sidecar. The `Imported` seam persists only `MaskProvenance` (`model_id` / `model_version` / `prompt`); `RasterStore` is **runtime-only and never serialized**.
- **Engine-opaque provenance:** `MaskProvenance` is stored **verbatim** and never interpreted by `ferrolite-mask`. `prompt` is an opaque string (clicks / box / semantic class in A2).
- **Additive, no schema break:** adding/using this variant must not change the encoding of existing `MaskComponent` variants nor the `frl:ops` schema. Legacy shapes-only payloads must still deserialize; future extra provenance fields must be tolerated (serde default = ignore-unknown; do **not** add `deny_unknown_fields`).
- **Contract 4 — executor unchanged:** `Graph<PipelineImage>` / `Graph<MaskBuffer>` executors are not modified; compositing stays a set of generic nodes/passes.
- **CLAUDE.md GPU rule:** build pipelines once and reuse; resolving an `Imported` raster clones an `Arc<wgpu::Texture>` handle (cheap) — it must not allocate or rebuild pipelines per component.
- **Rust style:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean; no `unwrap()` in non-test code; immutable-by-default (builder-style `RasterStore` construction).
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` all green, then STOP and hold for Jann's hands-on visual test before finishing the branch.

---

## File Structure

- **Create** `ferrolite-mask/src/raster_store.rs` — the `RasterStore` registry (`RasterHandle → MaskBuffer`); runtime cache, **not** serde. ~60 lines.
- **Modify** `ferrolite-mask/src/lib.rs` — add `mod raster_store;` and `pub use raster_store::RasterStore;`.
- **Modify** `ferrolite-mask/src/compositor.rs` — `eval` and `composite` take `&RasterStore`; the `Imported` arm resolves the handle (matching dims → clone the buffer; absent / dim-mismatch → inert zeroed). Adapt the existing 3 in-file test call sites; add a GPU-gated test proving `Imported` composites like any other component.
- **Modify** `ferrolite-pipeline/src/local_node.rs:298` — pass `&RasterStore::default()` (documented: A2 threads a populated store here).
- **Modify** `ferrolite-pipeline/src/mask_overlay.rs:41` — pass `&RasterStore::default()`.
- **Create** `ferrolite-mask/tests/ai_seam_forward_compat.rs` — engine-tier forward-compat/additivity tests (legacy-decode, unknown-field tolerance, verbatim prompt, prompt-not-raster).
- **Modify** `ferrolite-pipeline/tests/local_persistence.rs` — add sidecar-tier additivity tests (Imported provenance unknown-field tolerance; shapes-only legacy `frl:ops` still loads).

**No changes** to: `model.rs` (variant already defined + serialized), `composite.rs` / `pass.rs` / shape passes (fold math is variant-agnostic), any executor, any UI crate, `Cargo.toml` dependency sets.

---

## Task 1: `RasterStore` — the raster-resolution seam (no producer)

**Files:**
- Create: `ferrolite-mask/src/raster_store.rs`
- Modify: `ferrolite-mask/src/lib.rs` (add `mod raster_store;` + re-export)
- Test: unit tests in `ferrolite-mask/src/raster_store.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::model::RasterHandle`, `crate::buffer::MaskBuffer`, `ferrolite_gpu::GpuContext` (tests only).
- Produces:
  - `pub struct RasterStore` — wraps `std::collections::HashMap<RasterHandle, MaskBuffer>`; derives `Default` and `Clone`. **No `Serialize`/`Deserialize`** (runtime cache, contract 2).
  - `pub fn RasterStore::with_raster(self, handle: RasterHandle, buffer: MaskBuffer) -> Self` — immutable builder insert.
  - `pub fn RasterStore::insert(&mut self, handle: RasterHandle, buffer: MaskBuffer)`.
  - `pub fn RasterStore::get(&self, handle: RasterHandle) -> Option<&MaskBuffer>`.
  - `pub fn RasterStore::is_empty(&self) -> bool`.
  - `RasterHandle` must be usable as a `HashMap` key — it already derives `Clone, Copy, PartialEq, Eq` (see `model.rs`); this task adds `Hash` to that derive.

- [ ] **Step 1: Add `Hash` to `RasterHandle`'s derive**

In `ferrolite-mask/src/model.rs`, change the `RasterHandle` derive line so it can be a map key:

```rust
/// Opaque handle to an externally-produced raster mask (the AI seam). Inert in P1.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct RasterHandle(pub u64);
```

- [ ] **Step 2: Write the failing unit tests**

Create `ferrolite-mask/src/raster_store.rs`:

```rust
//! `RasterStore` — a runtime registry mapping `RasterHandle → MaskBuffer` for the
//! AI/imported-mask seam (design §8). It is the resolution point the `Imported`
//! component reads during compositing. It is a **re-derivable cache** (contract 2):
//! never serialized — only the parametric `MaskProvenance` (the prompt) persists in
//! the sidecar; A2's `ferrolite-ai::segment` job rebuilds the raster from that prompt
//! and populates this store. In P1 there is no producer, so the store is empty and
//! every `Imported` component composites as identity/zero (inert).
//!
//! Engine tier, weight-free: this is a plain map of GPU handles — no `ort`, no model
//! weights, no `ferrolite-ai` dependency (map D6).

use std::collections::HashMap;

use crate::buffer::MaskBuffer;
use crate::model::RasterHandle;

/// Runtime registry of externally-produced raster masks, keyed by `RasterHandle`.
/// Not serialized (the raster is a cache; the prompt is the source of truth).
#[derive(Clone, Default)]
pub struct RasterStore {
    rasters: HashMap<RasterHandle, MaskBuffer>,
}

impl RasterStore {
    /// Immutable-builder insert: returns a new store with `buffer` bound to `handle`.
    pub fn with_raster(mut self, handle: RasterHandle, buffer: MaskBuffer) -> Self {
        self.rasters.insert(handle, buffer);
        self
    }

    /// Bind (or replace) the raster for `handle`.
    pub fn insert(&mut self, handle: RasterHandle, buffer: MaskBuffer) {
        self.rasters.insert(handle, buffer);
    }

    /// Resolve `handle` to its raster buffer, if present.
    pub fn get(&self, handle: RasterHandle) -> Option<&MaskBuffer> {
        self.rasters.get(&handle)
    }

    /// True when no raster is registered (the P1 no-producer default).
    pub fn is_empty(&self) -> bool {
        self.rasters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_gpu::GpuContext;
    use std::sync::Arc;

    #[test]
    fn default_store_is_empty_and_resolves_nothing() {
        let store = RasterStore::default();
        assert!(store.is_empty());
        assert!(store.get(RasterHandle(0)).is_none());
        assert!(store.get(RasterHandle(42)).is_none());
    }

    #[test]
    fn with_raster_binds_and_resolves_by_handle() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let buf = MaskBuffer::alloc_zeroed(&ctx, 4, 4);
        let store = RasterStore::default().with_raster(RasterHandle(7), buf);
        assert!(!store.is_empty());
        let got = store.get(RasterHandle(7)).expect("handle 7 resolves");
        assert_eq!((got.width, got.height), (4, 4));
        assert!(store.get(RasterHandle(8)).is_none(), "unbound handle is None");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-mask raster_store`
Expected: FAIL to compile — `ferrolite-mask/src/raster_store.rs` is not yet declared as a module in `lib.rs` (unresolved), so `cargo test` errors on the unknown module / missing `RasterStore` export.

- [ ] **Step 4: Declare and re-export the module**

In `ferrolite-mask/src/lib.rs`, add the module declaration (keep alphabetical order alongside the existing `mod` lines) and re-export:

```rust
mod brush;
mod buffer;
mod composite;
mod compositor;
mod model;
mod pass;
mod raster_store;
mod shapes;
mod stroke;
mod vec;
```

```rust
pub use raster_store::RasterStore;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-mask raster_store`
Expected: PASS — `default_store_is_empty_and_resolves_nothing` passes everywhere; `with_raster_binds_and_resolves_by_handle` passes on a GPU host and prints the skip line on headless CI.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-mask/src/raster_store.rs ferrolite-mask/src/lib.rs ferrolite-mask/src/model.rs
git commit -m "feat(mask): RasterStore — runtime raster-resolution seam for the AI/imported mask (no producer)"
```

---

## Task 2: Wire `Imported` through the compositing path

**Files:**
- Modify: `ferrolite-mask/src/compositor.rs` (`eval`, `composite`, the 3 in-file test call sites; add a new GPU-gated test)
- Modify: `ferrolite-pipeline/src/local_node.rs:298`
- Modify: `ferrolite-pipeline/src/mask_overlay.rs:41`
- Test: `#[cfg(test)] mod tests` in `ferrolite-mask/src/compositor.rs`

**Interfaces:**
- Consumes: `crate::RasterStore` (Task 1), `crate::buffer::MaskBuffer`.
- Produces (changed signatures — the single compositing path):
  - `pub fn MaskCompositor::composite(&self, def: &MaskDefinition, input: &wgpu::TextureView, w: u32, h: u32, rasters: &RasterStore) -> MaskBuffer`
  - `fn MaskCompositor::eval(&self, comp: &MaskComponent, input: &wgpu::TextureView, w: u32, h: u32, rasters: &RasterStore) -> MaskBuffer` (private)
  - Resolution rule for `Imported { handle, .. }`: `rasters.get(handle)` → if present **and** `(width,height) == (w,h)`, return `buffer.clone()` (cheap `Arc` handle — it then folds through add/subtract/intersect/invert like any shape buffer); otherwise `MaskBuffer::alloc_zeroed(&self.ctx, w, h)` (inert — the no-producer default and the dim-mismatch fallback).

- [ ] **Step 1: Write the failing test proving `Imported` composites like any other**

Add to the `#[cfg(test)] mod tests` in `ferrolite-mask/src/compositor.rs`. This injects a known constant raster and checks it folds identically to a shape buffer. Add a small local helper to build a constant-valued `MaskBuffer` (mirrors the `ones` write-texture pattern already in this file):

```rust
    fn constant_buffer(ctx: &Arc<GpuContext>, w: u32, h: u32, value: f32) -> MaskBuffer {
        let buf = MaskBuffer::alloc(ctx, w, h);
        let data = vec![value; (buf.width * buf.height) as usize];
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &buf.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&data),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(buf.width * 4),
                rows_per_image: Some(buf.height),
            },
            wgpu::Extent3d {
                width: buf.width,
                height: buf.height,
                depth_or_array_layers: 1,
            },
        );
        buf
    }

    fn imported(handle: u64) -> MaskComponent {
        use crate::model::{MaskProvenance, RasterHandle};
        MaskComponent::Imported {
            handle: RasterHandle(handle),
            provenance: MaskProvenance {
                model_id: "sam2.1".into(),
                model_version: "1".into(),
                prompt: "click:0.5,0.5".into(),
            },
        }
    }

    #[test]
    fn imported_composites_like_any_other_component() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let comp = MaskCompositor::new(ctx.clone());
        let input = MaskBuffer::alloc_zeroed(&ctx, 4, 4);
        let iv = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // A store holding a constant-0.6 raster for handle 7 (stands in for an AI mask).
        let store = RasterStore::default().with_raster(RasterHandle(7), constant_buffer(&ctx, 4, 4, 0.6));

        // 1) Single Imported → the raster values themselves.
        let single = comp.composite(
            &MaskDefinition { components: vec![(imported(7), CompositeMode::Add)], invert: false },
            &iv, 4, 4, &store,
        );
        assert!(read_mask_r32f(&ctx, &single).iter().all(|&v| (v - 0.6).abs() < 1e-4),
            "single imported == raster");

        // 2) Imported inverted → 1 - 0.6 = 0.4 (composites like any other, invert applies).
        let inv = comp.composite(
            &MaskDefinition { components: vec![(imported(7), CompositeMode::Add)], invert: true },
            &iv, 4, 4, &store,
        );
        assert!(read_mask_r32f(&ctx, &inv).iter().all(|&v| (v - 0.4).abs() < 1e-4),
            "inverted imported == 0.4");

        // 3) Full luma seed (lo=0,hi=1 → 1.0) SUBTRACT imported → 1*(1-0.6) = 0.4 ("refine for free").
        let seed = MaskComponent::LumaRange { lo: 0.0, hi: 1.0, softness: 0.0 };
        let sub = comp.composite(
            &MaskDefinition {
                components: vec![(seed.clone(), CompositeMode::Add), (imported(7), CompositeMode::Subtract)],
                invert: false,
            },
            &iv, 4, 4, &store,
        );
        assert!(read_mask_r32f(&ctx, &sub).iter().all(|&v| (v - 0.4).abs() < 1e-4),
            "brush/range SUBTRACT imported folds like any component");

        // 4) Imported INTERSECT a 0.3 constant raster (handle 9) → min(0.6, 0.3) = 0.3.
        let store2 = store.with_raster(RasterHandle(9), constant_buffer(&ctx, 4, 4, 0.3));
        let isect = comp.composite(
            &MaskDefinition {
                components: vec![(imported(7), CompositeMode::Add), (imported(9), CompositeMode::Intersect)],
                invert: false,
            },
            &iv, 4, 4, &store2,
        );
        assert!(read_mask_r32f(&ctx, &isect).iter().all(|&v| (v - 0.3).abs() < 1e-4),
            "imported INTERSECT imported == min");
    }

    #[test]
    fn imported_with_no_producer_is_inert() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let comp = MaskCompositor::new(ctx.clone());
        let input = MaskBuffer::alloc_zeroed(&ctx, 4, 4);
        let iv = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        // Empty store (the P1 default: no producer) → Imported contributes zero.
        let out = comp.composite(
            &MaskDefinition { components: vec![(imported(7), CompositeMode::Add)], invert: false },
            &iv, 4, 4, &RasterStore::default(),
        );
        assert!(read_mask_r32f(&ctx, &out).iter().all(|&v| v.abs() < 1e-4),
            "no producer => imported inert");
    }
```

Also update the existing test imports at the top of the `tests` module so `RasterStore` and `RasterHandle` are in scope:

```rust
    use super::*;
    use crate::model::{CompositeMode, MaskComponent, RasterHandle};
    use crate::RasterStore;
```

(Delete the now-superseded `imported_component_contributes_zero` test — it is replaced by `imported_with_no_producer_is_inert`, which asserts the same inert-by-default behavior through the new signature.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-mask compositor`
Expected: FAIL to compile — `MaskCompositor::composite` does not yet accept a `rasters: &RasterStore` argument.

- [ ] **Step 3: Thread `&RasterStore` through `eval` + `composite` and resolve `Imported`**

In `ferrolite-mask/src/compositor.rs`, add the import and change the two methods. Add near the existing `use` lines:

```rust
use crate::RasterStore;
```

Change `eval`'s signature and the `Imported` arm:

```rust
    fn eval(
        &self,
        comp: &MaskComponent,
        input: &wgpu::TextureView,
        w: u32,
        h: u32,
        rasters: &RasterStore,
    ) -> MaskBuffer {
        match comp {
            // ... LinearGradient / RadialGradient / LumaRange / ColorRange / Brush arms UNCHANGED ...

            // The AI/imported seam. Resolve the handle from the runtime RasterStore;
            // a resolved raster of matching dims folds like any other component. With
            // no producer (P1) the store is empty → inert (zeroed). A dim-mismatch is
            // also treated as absent (A2 supplies rasters at the compositing resolution).
            MaskComponent::Imported { handle, .. } => match rasters.get(*handle) {
                Some(buf) if (buf.width, buf.height) == (w, h) => buf.clone(),
                _ => MaskBuffer::alloc_zeroed(&self.ctx, w, h),
            },
        }
    }
```

Change `composite`'s signature and its `eval` call:

```rust
    pub fn composite(
        &self,
        def: &MaskDefinition,
        input: &wgpu::TextureView,
        w: u32,
        h: u32,
        rasters: &RasterStore,
    ) -> MaskBuffer {
        if def.components.is_empty() {
            return if def.invert {
                MaskBuffer::alloc_zeroed(&self.ctx, w, h)
            } else {
                self.ones(w, h)
            };
        }
        let inputs: Vec<(MaskBuffer, CompositeMode)> = def
            .components
            .iter()
            .map(|(c, m)| (self.eval(c, input, w, h, rasters), *m))
            .collect();
        self.composite.composite(&inputs, def.invert)
    }
```

Update the existing GPU-gated test `empty_definition_is_ones_or_zero_by_invert` in this file to pass `&RasterStore::default()` at both `comp.composite(...)` call sites (append `, &RasterStore::default()` before the closing paren of each call).

- [ ] **Step 4: Update the two production callers**

In `ferrolite-pipeline/src/local_node.rs`, add `RasterStore` to the `ferrolite_mask` import on line 11 and pass an empty store at line 298:

```rust
use ferrolite_mask::{MaskBuffer, MaskCompositor, RasterStore};
```

```rust
                .map(|l| self.compositor.composite(&l.mask, &input_view, mw, mh, &RasterStore::default()))
```

Add a short comment above that `.map(...)` line:

```rust
            // P1 has no mask producer, so imported components resolve to nothing here.
            // A2 threads a populated RasterStore (rebuilt from provenance prompts) in.
```

In `ferrolite-pipeline/src/mask_overlay.rs`, add `RasterStore` to the `ferrolite_mask` import on line 11 and pass an empty store at line 41:

```rust
use ferrolite_mask::{read_mask_r32f, MaskCompositor, MaskDefinition, RasterStore};
```

```rust
        let buf = self.compositor.composite(def, &iv, w, h, &RasterStore::default());
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-mask -p ferrolite-pipeline`
Expected: PASS — new compositor tests pass on a GPU host (skip line on headless); pipeline crate compiles and its existing tests stay green.

- [ ] **Step 6: Verify clippy is clean for the touched crates**

Run: `cargo clippy -p ferrolite-mask -p ferrolite-pipeline --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-mask/src/compositor.rs ferrolite-pipeline/src/local_node.rs ferrolite-pipeline/src/mask_overlay.rs
git commit -m "feat(mask): composite Imported via RasterStore (add/subtract/intersect/invert); inert with no producer"
```

---

## Task 3: Forward-compatibility / additivity tests (enum + sidecar schema)

**Files:**
- Create: `ferrolite-mask/tests/ai_seam_forward_compat.rs`
- Modify: `ferrolite-pipeline/tests/local_persistence.rs`

**Interfaces:**
- Consumes: `ferrolite_mask::{MaskComponent, MaskDefinition, MaskProvenance, RasterHandle, CompositeMode, Vec2}`; `ferrolite_pipeline::{deserialize, serialize, OpStack}` and the existing `local_persistence.rs` helpers.
- Produces: proof (not code) that the `Imported` variant + `MaskProvenance` are additive — legacy shapes-only data still decodes, future provenance fields are tolerated, provenance is stored verbatim, and only the prompt (never a raster) persists.

- [ ] **Step 1: Write the engine-tier forward-compat tests**

Create `ferrolite-mask/tests/ai_seam_forward_compat.rs`. These are pure (de)serialization tests — no GPU, run on every OS in CI:

```rust
//! Forward-compatibility / additivity proof for the AI-mask seam (design §8, §11).
//! Adding `MaskComponent::Imported { handle, provenance }` must NOT break existing
//! variants or the schema, and A2 must be able to extend it additively. Contract 2:
//! only the prompt (provenance) persists — never a raster.

use ferrolite_mask::{
    CompositeMode, MaskComponent, MaskDefinition, MaskProvenance, RasterHandle, Vec2,
};

/// A definition authored BEFORE the `Imported` variant existed (shapes only) must
/// still deserialize on the current build — proving the variant addition is additive
/// and did not change how existing variants encode (externally-tagged by variant name).
#[test]
fn legacy_shapes_only_definition_still_deserializes() {
    let legacy = r#"{
        "components": [
            [{"LinearGradient": {"start": {"x": 0.0, "y": 0.0}, "end": {"x": 0.0, "y": 1.0}}}, "Add"],
            [{"LumaRange": {"lo": 0.2, "hi": 0.7, "softness": 0.1}}, "Subtract"]
        ],
        "invert": false
    }"#;
    let def: MaskDefinition = serde_json::from_str(legacy).expect("legacy shapes-only decodes");
    assert_eq!(def.components.len(), 2);
    assert_eq!(
        def.components[0].0,
        MaskComponent::LinearGradient {
            start: Vec2::new(0.0, 0.0),
            end: Vec2::new(0.0, 1.0),
        }
    );
    assert_eq!(def.components[1].1, CompositeMode::Subtract);
    assert!(!def.invert);
}

/// A future build may add fields to `MaskProvenance`. Serde ignores unknown fields by
/// default (we must never add `deny_unknown_fields`), so an extended payload still
/// loads on today's build with the known fields intact — A2 can grow provenance.
#[test]
fn imported_provenance_tolerates_unknown_future_fields() {
    let future = r#"{
        "Imported": {
            "handle": 42,
            "provenance": {
                "model_id": "segnext",
                "model_version": "2.0",
                "prompt": "box:0.1,0.2,0.8,0.9",
                "confidence": 0.97,
                "future_field": {"nested": true}
            }
        }
    }"#;
    let comp: MaskComponent = serde_json::from_str(future).expect("unknown fields tolerated");
    match comp {
        MaskComponent::Imported { handle, provenance } => {
            assert_eq!(handle, RasterHandle(42));
            assert_eq!(provenance.model_id, "segnext");
            assert_eq!(provenance.model_version, "2.0");
            assert_eq!(provenance.prompt, "box:0.1,0.2,0.8,0.9");
        }
        other => panic!("expected Imported, got {other:?}"),
    }
}

/// The engine stores the prompt verbatim and never interprets it. Any opaque prompt
/// encoding (clicks / box / semantic class) round-trips byte-identically.
#[test]
fn provenance_prompt_is_stored_verbatim() {
    for prompt in [
        "click:0.5,0.5;0.25,0.75",
        "box:0.1,0.2,0.8,0.9",
        "semantic:sky",
        "", // empty prompt is still valid opaque data
    ] {
        let def = MaskDefinition {
            components: vec![(
                MaskComponent::Imported {
                    handle: RasterHandle(1),
                    provenance: MaskProvenance {
                        model_id: "sam2.1".into(),
                        model_version: "1.0".into(),
                        prompt: prompt.into(),
                    },
                },
                CompositeMode::Add,
            )],
            invert: false,
        };
        let back: MaskDefinition =
            serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
        assert_eq!(def, back, "prompt {prompt:?} round-trips verbatim");
    }
}

/// Contract 2: the serialized `Imported` component carries the PROMPT (provenance),
/// not a raster. Only `handle` (a u64 id) + `provenance` are present — no pixel data.
#[test]
fn serialized_imported_carries_prompt_not_raster() {
    let def = MaskDefinition {
        components: vec![(
            MaskComponent::Imported {
                handle: RasterHandle(7),
                provenance: MaskProvenance {
                    model_id: "sam2.1".into(),
                    model_version: "1.0".into(),
                    prompt: "click:0.5,0.5".into(),
                },
            },
            CompositeMode::Add,
        )],
        invert: false,
    };
    let json = serde_json::to_string(&def).unwrap();
    assert!(json.contains("\"prompt\":\"click:0.5,0.5\""), "prompt persists");
    assert!(json.contains("\"handle\":7"), "handle is a plain id");
    // No raster/pixel payload exists in the model to serialize.
    assert!(!json.contains("raster"), "no raster field");
    assert!(!json.contains("pixels"), "no pixel data");
    assert!(!json.contains("texture"), "no texture data");
}
```

- [ ] **Step 2: Run the engine-tier tests to verify they pass**

Run: `cargo test -p ferrolite-mask --test ai_seam_forward_compat`
Expected: PASS — all four tests green (pure serde, no GPU).

- [ ] **Step 3: Add the sidecar-tier additivity tests**

Append to `ferrolite-pipeline/tests/local_persistence.rs`. These prove the `frl:ops` schema is additive for the seam — a legacy shapes-only payload loads, and a future payload with extra provenance fields loads with the prompt intact:

```rust
#[test]
fn legacy_shapes_only_local_adjustments_still_loads() {
    // A LocalAdjustments payload authored before the Imported variant existed.
    let json = r#"{"version":1,"ops":[{"LocalAdjustments":{"layers":[
        {"name":"sky","visible":true,
         "mask":{"components":[[{"LinearGradient":{"start":{"x":0.0,"y":0.0},"end":{"x":0.0,"y":1.0}}},"Add"]],"invert":false},
         "adjustments":{"exposure":-0.4}}]}}]}"#;
    let s = deserialize(json).expect("legacy frl:ops decodes");
    let la = s.local_adjustments().expect("has local adjustments");
    assert_eq!(la.layers.len(), 1);
    assert_eq!(la.layers[0].mask.components.len(), 1);
    assert_eq!(la.layers[0].adjustments.exposure, -0.4);
}

#[test]
fn imported_provenance_unknown_field_tolerated_in_frl_ops() {
    // A future frl:ops with an extra provenance field must load on today's build,
    // proving A2 can extend MaskProvenance without a sidecar schema break.
    let json = r#"{"version":1,"ops":[{"LocalAdjustments":{"layers":[
        {"name":"subject","visible":true,
         "mask":{"components":[
            [{"Imported":{"handle":7,"provenance":{"model_id":"sam2.1","model_version":"1","prompt":"click:0.5,0.5","future_score":0.9}}},"Add"]
         ],"invert":false},
         "adjustments":{"exposure":0.2}}]}}]}"#;
    let s = deserialize(json).expect("future frl:ops with extra provenance field decodes");
    let la = s.local_adjustments().unwrap();
    match &la.layers[0].mask.components[0].0 {
        MaskComponent::Imported { handle, provenance } => {
            assert_eq!(*handle, RasterHandle(7));
            assert_eq!(provenance.prompt, "click:0.5,0.5");
        }
        other => panic!("expected Imported, got {other:?}"),
    }
}
```

Ensure `MaskComponent` is imported in the test file's `use ferrolite_mask::{...}` line (it already imports `CompositeMode, MaskComponent, MaskDefinition, MaskProvenance, RasterHandle, Vec2 as MVec2` — confirm `MaskComponent` and `RasterHandle` are present; they are).

- [ ] **Step 4: Run the sidecar-tier tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --test local_persistence`
Expected: PASS — the two new tests plus the existing `local_persistence` suite (already exercising `Imported` through XMP) stay green.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-mask/tests/ai_seam_forward_compat.rs ferrolite-pipeline/tests/local_persistence.rs
git commit -m "test(mask,pipeline): forward-compat/additivity proof for the AI-mask seam (A2 is additive; prompt persists, not raster)"
```

---

## Task 4: Workspace gate

**Files:** none (verification only).

- [ ] **Step 1: Format check**

Run: `cargo fmt --check`
Expected: no diff. (If it reports formatting, run `cargo fmt` and re-commit the touched files.)

- [ ] **Step 2: Clippy across the whole workspace**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings, no errors.

- [ ] **Step 3: Full test suite**

Run: `cargo test --workspace`
Expected: all green. GPU-gated tests skip cleanly on headless CI (print the "no GPU adapter" line); on a GPU host the new `imported_composites_like_any_other_component` test passes.

- [ ] **Step 4: STOP — hand over the visual test plan and hold**

The gate is green. Per CLAUDE.md "Finishing a branch", **do not merge/PR/finish**. Hand Jann the visual test plan below and wait for his hands-on results.

---

## Visual test plan (for the author)

**Nothing new is visually reachable in the running app from this plan — and here is why.** This plan is engine-tier + test-only: it adds a `RasterStore` registry and wires the `MaskComponent::Imported` compositing arm, but **there is no producer** (no `ort`, no weights, no UI to create an imported mask), and both production callers (`local_node.rs`, `mask_overlay.rs`) pass an **empty** store — so an `Imported` component still composites to nothing in the running app, exactly as before. No panel, control, gesture, or rendered pixel changes for a user. The real hands-on test for AI masks lands in **A2** (the `ferrolite-ai::segment` producer that populates the store from a prompt).

**Optional sanity glance (not required):** the existing Develop → Masking tool must still behave exactly as it did after Plan 4 — create a mask, add brush/linear/radial/luma/color-range components with add/subtract/intersect, toggle the overlay, adjust Light/Color, undo/redo, and confirm masks persist across reopen. This plan should not have altered any of that (the compositing signature change is internal); if any masking regression appears, that is the failure signature to report.

---

## Self-Review

**1. Spec coverage (design §8, §12 plan 5):**
- "Define + serialize `MaskComponent::Imported { handle, provenance }`; `MaskProvenance` engine-opaque, stored never interpreted" → already landed in Plans 1–4 (`model.rs`); re-verified/locked by Task 3 (`provenance_prompt_is_stored_verbatim`, unknown-field tolerance).
- "Wire it through the compositing path so an Imported component composites like any other (Add/Subtract/Intersect, invert)" → Task 2 (`imported_composites_like_any_other_component` covers single/invert/subtract/intersect).
- "so an AI mask refines/combines with brush/range masks for free" → Task 2 case 3 (LumaRange SUBTRACT Imported) + case 4 (Imported INTERSECT Imported).
- "NO producer, NO ort, NO weights; ferrolite-mask stays engine-tier/weight-free" → Global Constraints; `RasterStore` is a plain map; no `Cargo.toml` change; Task 2 empty-store callers; `imported_with_no_producer_is_inert`.
- "prove adding this variant does not break the MaskComponent enum or the frl:ops sidecar schema; A2 purely additive" → Task 3 (`legacy_shapes_only_definition_still_deserializes`, `imported_provenance_tolerates_unknown_future_fields`, `legacy_shapes_only_local_adjustments_still_loads`, `imported_provenance_unknown_field_tolerated_in_frl_ops`).
- "provenance (a prompt) is what persists (raster is a re-derivable cache, contract 2), not a raster" → Task 3 (`serialized_imported_carries_prompt_not_raster`) + `RasterStore` non-serialized (Task 1 doc + no serde derive).
- §11 TDD list: serde round-trip of Imported+provenance (Task 3 + pre-existing model test) ✓; sidecar version-tolerance (Task 3 sidecar tests + pre-existing) ✓; composite-path behavior (Task 2) ✓; forward-compat/additivity (Task 3) ✓.
- §13 honored: executor unchanged (no `Graph` edit); engine crate weight-free; AI hand-off define+serialize now / producer in A2.

**2. Placeholder scan:** none — every code step shows complete code; every run step names an exact command + expected result.

**3. Type consistency:** `RasterStore` (Task 1) is consumed by exact signature in Task 2 (`composite(&self, def, input, w, h, &RasterStore)`); `RasterHandle` gains `Hash` in Task 1 before use as a map key; `get(RasterHandle) -> Option<&MaskBuffer>` matches the `Some(buf) if dims ...` resolution in Task 2; `with_raster`/`insert`/`is_empty` names are used consistently in Task 1 tests and Task 2 tests. `MaskBuffer` fields (`width`, `height`, `texture`) match `buffer.rs`. `deserialize`/`serialize`/`OpStack`/`local_adjustments()` match `local_persistence.rs`.

# Spec 2 — Plan 1: Pipeline Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the `ferrolite-pipeline` crate — the `OpStack` document model, its serialization, and a retained GPU edit DAG (on the existing `ferrolite-gpu::Graph`) with the three point-op WGSL **compute** passes (exposure, white balance, contrast) — proving non-destructive editing end-to-end at preview resolution, verified by golden-image diffs.

**Architecture:** `OpStack` is pure, serializable document data. `EditPipeline` builds a `Graph<PipelineImage>` where a `SourceNode` (uploaded image) feeds a fixed chain of `PointOpNode`s (one WGSL compute pass each). Editing one op updates that node's shared param cell + `mark_dirty`s it, so the existing dirty-flag executor re-runs only that op + downstream (per-op invalidation). The executor is **used unchanged** with a concrete photo output type — no executor edits (cross-cutting contract §4).

**Tech Stack:** Rust 2021, `wgpu` 22 (compute pipelines + storage textures), `bytemuck`, `half` (f16 upload), `serde`/`serde_json` (op-stack codec), `ferrolite-gpu::{GpuContext, Graph, Node, NodeId}`, `ferrolite-image::LinearRgbaF32`. Golden tests use a headless `GpuContext` + `image` (PNG), auto-skipping when no GPU adapter exists.

## Global Constraints

Copied verbatim from the spec (`docs/superpowers/specs/2026-06-30-spec2-editing-design.md`) and repo conventions — every task implicitly includes these:

- **Branch:** all work on `feat/editing-pipeline` (already created off `main`).
- **License:** crate is `GPL-3.0-only` (photo tier — may pull LGPL/GPL; `ferrolite-pipeline` is *not* an engine-transferable crate, so dep restrictions do not apply to it). The engine-tier crates (`ferrolite-gpu`/`ferrolite-vt`/`ferrolite-image`) are **not modified** by this plan.
- **Executor is photo/wgpu-agnostic — do NOT modify `ferrolite-gpu/src/executor.rs`.** Use `Graph<O>` with `O = PipelineImage` only (contract §4).
- **Pinned versions (do not bump):** `wgpu = "22"`, `bytemuck` (workspace, `derive`), `half` (workspace, `bytemuck`), `rusqlite` pinned 0.32 (untouched here). Rust floor `rust-version = "1.88"`.
- **Color space:** all passes operate in **display-linear RGB** (the space `QuadBin`/preview produce); the sRGB OETF is applied only at display/blit, never baked into an edit pass.
- **CLAUDE.md responsiveness rules:** no blocking/heavy CPU or I/O on the UI thread; GPU pipelines/shaders built **once and reused** (never per-edit/per-frame); bound any work that could exceed a frame budget.
- **Golden GPU tests MUST auto-skip when `GpuContext::headless()` returns `None`** (log + `return`), so `cargo test --workspace` stays green on GPU-less CI. Goldens are authored/verified on the dev GPU (RTX 3060/3070 class) and committed.
- **Commit style:** conventional commits (`feat:`/`test:`/`chore:`), no attribution trailer (disabled globally; matches repo history).
- **Gate (end of plan):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green, then **hold for the author's visual test** before finishing.

---

## File Structure

**New crate `ferrolite-pipeline/`:**
- `Cargo.toml` — crate manifest (workspace deps).
- `src/lib.rs` — module declarations + public re-exports.
- `src/op.rs` — `Op`, `OpKind`, param structs (`Exposure`, `WhiteBalance`, `Contrast`), `OpStack`. Pure, no GPU.
- `src/serialize.rs` — `serialize`/`deserialize` (serde_json + version check). Pure.
- `src/uniforms.rs` — pure param→shader-uniform math + the `#[repr(C)]` Pod uniform structs. No GPU.
- `src/image.rs` — `PipelineImage` (Arc-wrapped `wgpu::Texture` + dims).
- `src/nodes.rs` — `upload_source`, `SourceNode`, generic `PointOpNode<U>` (the compute-node machinery).
- `src/pipeline.rs` — `EditPipeline` (builds/evaluates the DAG, `set_stack`) + `blit_to_rgba8`.
- `src/shaders/exposure.wgsl`, `white_balance.wgsl`, `contrast.wgsl`, `blit.wgsl`.
- `tests/common/mod.rs` — golden helpers (`max_abs_diff`, `gradient`, `assert_golden`).
- `tests/golden.rs` — GPU golden + cache-reuse integration tests (skip headless).
- `tests/fixtures/*.png` — committed golden references (authored on dev GPU).

**Modified (root):**
- `Cargo.toml` — add `ferrolite-pipeline` to `members` + `workspace.dependencies` (and `serde`/`serde_json`).

---

### Task 1: `OpStack` document model (+ crate skeleton)

**Files:**
- Create: `ferrolite-pipeline/Cargo.toml`
- Create: `ferrolite-pipeline/src/lib.rs`
- Create: `ferrolite-pipeline/src/op.rs`
- Modify: `Cargo.toml` (root — `members` + `workspace.dependencies`)

**Interfaces:**
- Produces: `Op` (enum: `Exposure(Exposure)`, `WhiteBalance(WhiteBalance)`, `Contrast(Contrast)`), `OpKind` (`#[repr(u8)]` enum), param structs `Exposure{ev:f32}` / `WhiteBalance{temp:f32,tint:f32}` / `Contrast{amount:f32}`, `OpStack{version:u32,ops:Vec<Op>}`, `const STACK_VERSION: u32 = 1`. Methods: `OpStack::default()`, `is_identity()`, `set_op(Op)->OpStack` (immutable), `reset(OpKind)->OpStack`, `exposure()->Option<Exposure>`, `white_balance()->Option<WhiteBalance>`, `contrast()->Option<Contrast>`; `Op::kind()->OpKind`. All `Clone`, `PartialEq`, `Debug`, `Serialize`, `Deserialize`.

- [ ] **Step 1: Add the crate to the workspace**

In root `Cargo.toml`, add `"ferrolite-pipeline"` to `members`, and under `[workspace.dependencies]` add:

```toml
ferrolite-pipeline = { path = "ferrolite-pipeline" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

- [ ] **Step 2: Create the crate manifest**

`ferrolite-pipeline/Cargo.toml`:

```toml
[package]
name = "ferrolite-pipeline"
version = "0.0.1"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
ferrolite-image = { workspace = true }
ferrolite-gpu = { workspace = true }
wgpu = { workspace = true }
bytemuck = { workspace = true }
half = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }

[dev-dependencies]
pollster = { workspace = true }
image = { workspace = true, features = ["png"] }
```

- [ ] **Step 3: Create `src/lib.rs` with the module + re-exports**

```rust
//! ferrolite-pipeline — the photo edit DAG. An ordered `OpStack` document model
//! and a retained GPU pipeline built on `ferrolite-gpu`'s generic executor; WGSL
//! compute passes implement the edits. Photo tier (GPL-OK).

mod image;
mod nodes;
mod op;
mod pipeline;
mod serialize;
mod uniforms;

pub use image::PipelineImage;
pub use op::{Contrast, Exposure, Op, OpKind, OpStack, WhiteBalance, STACK_VERSION};
pub use pipeline::{blit_to_rgba8, upload_source, EditPipeline};
pub use serialize::{deserialize, serialize};
pub use uniforms::{ContrastUniform, ExposureUniform, WbUniform};
```

(Modules `nodes`, `pipeline`, `image`, `serialize`, `uniforms` are created in later tasks; until then this won't compile — Task 1's gate is the model test, so create empty stub files `image.rs`, `nodes.rs`, `pipeline.rs`, `serialize.rs`, `uniforms.rs` containing only `// stub` for now, and comment out the re-exports for items not yet defined. The simplest path: in Task 1, declare only `mod op;` and `pub use op::*;`-equivalent re-exports, and add the other `mod`/`pub use` lines in the task that creates them.)

Concretely, for Task 1 use this minimal `lib.rs`:

```rust
//! ferrolite-pipeline — the photo edit DAG (see crate docs in the final lib.rs).
mod op;

pub use op::{Contrast, Exposure, Op, OpKind, OpStack, WhiteBalance, STACK_VERSION};
```

- [ ] **Step 4: Write the failing model test**

Append to `ferrolite-pipeline/src/op.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_identity_and_empty() {
        let s = OpStack::default();
        assert_eq!(s.version, STACK_VERSION);
        assert!(s.is_identity());
        assert!(s.ops.is_empty());
    }

    #[test]
    fn set_op_is_immutable_and_adds() {
        let base = OpStack::default();
        let next = base.set_op(Op::Exposure(Exposure { ev: 0.5 }));
        assert!(base.is_identity(), "original stack unchanged (immutable)");
        assert_eq!(next.exposure(), Some(Exposure { ev: 0.5 }));
        assert_eq!(next.ops.len(), 1);
    }

    #[test]
    fn set_op_same_kind_replaces() {
        let s = OpStack::default()
            .set_op(Op::Exposure(Exposure { ev: 0.5 }))
            .set_op(Op::Exposure(Exposure { ev: -1.0 }));
        assert_eq!(s.ops.len(), 1, "same kind replaced, not appended");
        assert_eq!(s.exposure(), Some(Exposure { ev: -1.0 }));
    }

    #[test]
    fn ops_stay_in_canonical_order() {
        let s = OpStack::default()
            .set_op(Op::Contrast(Contrast { amount: 0.2 }))
            .set_op(Op::Exposure(Exposure { ev: 0.1 }))
            .set_op(Op::WhiteBalance(WhiteBalance { temp: 0.0, tint: 0.0 }));
        let kinds: Vec<OpKind> = s.ops.iter().map(|o| o.kind()).collect();
        assert_eq!(
            kinds,
            vec![OpKind::Exposure, OpKind::WhiteBalance, OpKind::Contrast]
        );
    }

    #[test]
    fn reset_removes_one_kind() {
        let s = OpStack::default()
            .set_op(Op::Exposure(Exposure { ev: 0.5 }))
            .set_op(Op::Contrast(Contrast { amount: 0.2 }))
            .reset(OpKind::Exposure);
        assert_eq!(s.exposure(), None);
        assert_eq!(s.contrast(), Some(Contrast { amount: 0.2 }));
    }
}
```

- [ ] **Step 5: Run the test to verify it fails**

Run: `cargo test -p ferrolite-pipeline`
Expected: FAIL — `op.rs` has no `Op`/`OpStack` definitions yet (compile error).

- [ ] **Step 6: Implement the model**

Prepend to `ferrolite-pipeline/src/op.rs` (above the test module):

```rust
//! The edit document model: an ordered `OpStack` of point/parametric ops. Pure
//! data — no GPU. This is the unit of undo/redo (later plan) and the payload
//! persisted to the `.xmp` sidecar (Plan 4). Apply order is the fixed canonical
//! op order (the `OpKind` discriminant order); the `Vec` is kept sorted by it.

use serde::{Deserialize, Serialize};

/// Current on-stack schema version. Bumped if `Op`'s shape changes incompatibly.
pub const STACK_VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Exposure {
    /// Exposure adjustment in stops (EV). 0 = identity.
    pub ev: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct WhiteBalance {
    /// Normalized temperature in [-1, 1] (warm positive). 0 = identity.
    pub temp: f32,
    /// Normalized tint in [-1, 1] (magenta positive). 0 = identity.
    pub tint: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Contrast {
    /// Bipolar contrast amount in [-1, 1]. 0 = identity.
    pub amount: f32,
}

/// One adjustment in the stack. Plan 2 adds ToneCurve/Hsl/Sharpen/Geometry.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum Op {
    Exposure(Exposure),
    WhiteBalance(WhiteBalance),
    Contrast(Contrast),
}

/// Canonical op identity + apply order (the discriminant order is the order ops
/// are applied in the pipeline chain).
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    Exposure = 0,
    WhiteBalance = 1,
    Contrast = 2,
}

impl Op {
    pub fn kind(&self) -> OpKind {
        match self {
            Op::Exposure(_) => OpKind::Exposure,
            Op::WhiteBalance(_) => OpKind::WhiteBalance,
            Op::Contrast(_) => OpKind::Contrast,
        }
    }
}

/// An ordered, immutable stack of edits. `set_op`/`reset` return new stacks.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct OpStack {
    pub version: u32,
    pub ops: Vec<Op>,
}

impl Default for OpStack {
    fn default() -> Self {
        Self {
            version: STACK_VERSION,
            ops: Vec::new(),
        }
    }
}

impl OpStack {
    /// No ops = unedited (renders identically to the source).
    pub fn is_identity(&self) -> bool {
        self.ops.is_empty()
    }

    /// Return a new stack with `op` set: replaces any existing op of the same
    /// kind, keeps the `Vec` sorted in canonical (`OpKind`) order.
    pub fn set_op(&self, op: Op) -> OpStack {
        let k = op.kind();
        let mut ops: Vec<Op> = self.ops.iter().copied().filter(|o| o.kind() != k).collect();
        ops.push(op);
        ops.sort_by_key(|o| o.kind() as u8);
        OpStack {
            version: self.version,
            ops,
        }
    }

    /// Return a new stack with any op of `kind` removed (per-op reset).
    pub fn reset(&self, kind: OpKind) -> OpStack {
        OpStack {
            version: self.version,
            ops: self.ops.iter().copied().filter(|o| o.kind() != kind).collect(),
        }
    }

    pub fn exposure(&self) -> Option<Exposure> {
        self.ops.iter().find_map(|o| match o {
            Op::Exposure(e) => Some(*e),
            _ => None,
        })
    }

    pub fn white_balance(&self) -> Option<WhiteBalance> {
        self.ops.iter().find_map(|o| match o {
            Op::WhiteBalance(w) => Some(*w),
            _ => None,
        })
    }

    pub fn contrast(&self) -> Option<Contrast> {
        self.ops.iter().find_map(|o| match o {
            Op::Contrast(c) => Some(*c),
            _ => None,
        })
    }
}
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS (5 model tests). Also run `cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings` — expected clean.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml ferrolite-pipeline/
git commit -m "feat(pipeline): OpStack document model + crate skeleton"
```

---

### Task 2: Op-stack serialization

**Files:**
- Create: `ferrolite-pipeline/src/serialize.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (add `mod serialize;` + re-export)

**Interfaces:**
- Consumes: `OpStack`, `STACK_VERSION` (Task 1).
- Produces: `pub fn serialize(stack: &OpStack) -> String`; `pub fn deserialize(s: &str) -> Option<OpStack>` (returns `None` on parse failure OR unrecognized `version`).

- [ ] **Step 1: Wire the module**

In `src/lib.rs` add `mod serialize;` and `pub use serialize::{deserialize, serialize};`.

- [ ] **Step 2: Write the failing test**

`ferrolite-pipeline/src/serialize.rs`:

```rust
//! Op-stack <-> string codec. JSON payload (embedded in the `frl:ops` XMP
//! attribute in Plan 4). Version-checked: an unknown version deserializes to
//! `None` so the caller can fall back to `OpStack::default()` (unedited).

use crate::op::OpStack;
use crate::op::STACK_VERSION;

pub fn serialize(stack: &OpStack) -> String {
    serde_json::to_string(stack).expect("OpStack is always serializable")
}

pub fn deserialize(s: &str) -> Option<OpStack> {
    let stack: OpStack = serde_json::from_str(s).ok()?;
    if stack.version != STACK_VERSION {
        return None;
    }
    Some(stack)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::{Contrast, Exposure, Op, WhiteBalance};

    #[test]
    fn round_trips_a_full_stack() {
        let s = OpStack::default()
            .set_op(Op::Exposure(Exposure { ev: 0.75 }))
            .set_op(Op::WhiteBalance(WhiteBalance { temp: 0.2, tint: -0.1 }))
            .set_op(Op::Contrast(Contrast { amount: 0.3 }));
        let text = serialize(&s);
        assert_eq!(deserialize(&text), Some(s));
    }

    #[test]
    fn round_trips_the_empty_stack() {
        let s = OpStack::default();
        assert_eq!(deserialize(&serialize(&s)), Some(s));
    }

    #[test]
    fn unknown_version_is_none() {
        // A well-formed stack but with a future version.
        let json = r#"{"version":999,"ops":[]}"#;
        assert_eq!(deserialize(json), None);
    }

    #[test]
    fn garbage_is_none() {
        assert_eq!(deserialize("not json {{"), None);
    }
}
```

- [ ] **Step 3: Run to verify it fails, then passes**

Run: `cargo test -p ferrolite-pipeline serialize`
Expected: the test module compiles and PASSES (the implementation is included above — this task's "RED" is purely that the module didn't exist before wiring it; confirm green).

- [ ] **Step 4: Commit**

```bash
git add ferrolite-pipeline/src/serialize.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): versioned OpStack serialization (json)"
```

---

### Task 3: Pure param→uniform conversions

**Files:**
- Create: `ferrolite-pipeline/src/uniforms.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (`mod uniforms;` + re-export)

**Interfaces:**
- Consumes: `Exposure`, `WhiteBalance`, `Contrast` (Task 1).
- Produces: pure fns `exposure_gain(ev:f32)->f32`, `wb_multipliers(temp:f32,tint:f32)->[f32;3]`, `contrast_gain_pivot(amount:f32)->(f32,f32)`, `const CONTRAST_PIVOT: f32`; Pod uniform structs `ExposureUniform{gain:f32,pad:[f32;3]}`, `WbUniform{mul:[f32;3],pad:f32}`, `ContrastUniform{gain:f32,pivot:f32,pad:[f32;2]}`; constructors `exposure_uniform(Option<Exposure>)->ExposureUniform`, `wb_uniform(Option<WhiteBalance>)->WbUniform`, `contrast_uniform(Option<Contrast>)->ContrastUniform`. (Structs are re-exported `pub` so their padding fields are reachable — avoids dead_code under `-D warnings`.)

- [ ] **Step 1: Wire the module**

In `src/lib.rs` add `mod uniforms;` and `pub use uniforms::{ContrastUniform, ExposureUniform, WbUniform};`.

- [ ] **Step 2: Write the failing test**

`ferrolite-pipeline/src/uniforms.rs`:

```rust
//! Pure CPU math turning UI op params into GPU shader uniforms, plus the
//! `#[repr(C)]` Pod uniform structs (layouts MIRROR the WGSL `struct P` in each
//! shader). Display-linear space; the sRGB OETF lives only in the display/blit
//! shader. No GPU here — fully unit-tested.

use crate::op::{Contrast, Exposure, WhiteBalance};

/// Mid-grey pivot (display-linear) about which contrast scales. Placeholder
/// constant; Spec 3 may refine once the working space is fixed.
pub const CONTRAST_PIVOT: f32 = 0.18;

/// EV (stops) -> linear gain. `2^ev`. ev=0 -> 1.0 (identity).
pub fn exposure_gain(ev: f32) -> f32 {
    2.0f32.powf(ev)
}

/// Normalized temp/tint in [-1,1] -> per-channel linear multipliers `[r,g,b]`.
/// Pragmatic placeholder (image science is secondary): warm temp boosts R /
/// cuts B; magenta tint cuts G. Clamped non-negative.
pub fn wb_multipliers(temp: f32, tint: f32) -> [f32; 3] {
    let r = (1.0 + 0.5 * temp).max(0.0);
    let b = (1.0 - 0.5 * temp).max(0.0);
    let g = (1.0 - 0.5 * tint).max(0.0);
    [r, g, b]
}

/// Bipolar amount -> (gain, pivot). amount=0 -> gain 1.0 (identity).
pub fn contrast_gain_pivot(amount: f32) -> (f32, f32) {
    (1.0 + amount, CONTRAST_PIVOT)
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ExposureUniform {
    pub gain: f32,
    pub pad: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WbUniform {
    pub mul: [f32; 3],
    pub pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ContrastUniform {
    pub gain: f32,
    pub pivot: f32,
    pub pad: [f32; 2],
}

pub fn exposure_uniform(op: Option<Exposure>) -> ExposureUniform {
    let ev = op.map(|e| e.ev).unwrap_or(0.0);
    ExposureUniform {
        gain: exposure_gain(ev),
        pad: [0.0; 3],
    }
}

pub fn wb_uniform(op: Option<WhiteBalance>) -> WbUniform {
    let (t, ti) = op.map(|w| (w.temp, w.tint)).unwrap_or((0.0, 0.0));
    WbUniform {
        mul: wb_multipliers(t, ti),
        pad: 0.0,
    }
}

pub fn contrast_uniform(op: Option<Contrast>) -> ContrastUniform {
    let a = op.map(|c| c.amount).unwrap_or(0.0);
    let (gain, pivot) = contrast_gain_pivot(a);
    ContrastUniform {
        gain,
        pivot,
        pad: [0.0; 2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_gain_is_two_to_the_ev() {
        assert!((exposure_gain(0.0) - 1.0).abs() < 1e-6);
        assert!((exposure_gain(1.0) - 2.0).abs() < 1e-6);
        assert!((exposure_gain(-1.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn wb_identity_at_zero() {
        assert_eq!(wb_multipliers(0.0, 0.0), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn wb_warm_temp_boosts_red_cuts_blue() {
        assert_eq!(wb_multipliers(1.0, 0.0), [1.5, 1.0, 0.5]);
    }

    #[test]
    fn wb_magenta_tint_cuts_green() {
        assert_eq!(wb_multipliers(0.0, 1.0), [1.0, 0.5, 1.0]);
    }

    #[test]
    fn contrast_identity_and_gain() {
        assert_eq!(contrast_gain_pivot(0.0), (1.0, CONTRAST_PIVOT));
        assert_eq!(contrast_gain_pivot(1.0), (2.0, CONTRAST_PIVOT));
    }

    #[test]
    fn uniform_constructors_use_identity_when_absent() {
        assert_eq!(exposure_uniform(None).gain, 1.0);
        assert_eq!(wb_uniform(None).mul, [1.0, 1.0, 1.0]);
        assert_eq!(contrast_uniform(None).gain, 1.0);
    }
}
```

- [ ] **Step 3: Run to verify pass**

Run: `cargo test -p ferrolite-pipeline uniforms`
Expected: PASS (6 tests). `cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings` clean (padding fields are pub + re-exported → no dead_code).

- [ ] **Step 4: Commit**

```bash
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): pure param->uniform conversions + Pod uniforms"
```

---

### Task 4: `PipelineImage`, `upload_source`, and `blit_to_rgba8` (display/readback)

**Files:**
- Create: `ferrolite-pipeline/src/image.rs`
- Create: `ferrolite-pipeline/src/nodes.rs` (only `upload_source` in this task)
- Create: `ferrolite-pipeline/src/pipeline.rs` (only `blit_to_rgba8` in this task)
- Create: `ferrolite-pipeline/src/shaders/blit.wgsl`
- Create: `ferrolite-pipeline/tests/common/mod.rs`
- Create: `ferrolite-pipeline/tests/golden.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (`mod image; mod nodes; mod pipeline;` + re-exports)

**Interfaces:**
- Consumes: `LinearRgbaF32`, `GpuContext`.
- Produces: `pub struct PipelineImage { pub texture: Arc<wgpu::Texture>, pub width: u32, pub height: u32 }` (`Clone`); `pub fn upload_source(ctx:&GpuContext, img:&LinearRgbaF32) -> PipelineImage` (Rgba16Float, `TEXTURE_BINDING|COPY_DST`); `pub fn blit_to_rgba8(ctx:&GpuContext, img:&PipelineImage) -> Vec<u8>` (samples nearest, applies sRGB OETF, returns `width*height*4` bytes). Test helpers in `tests/common/mod.rs`: `gradient(w,h)->LinearRgbaF32`, `max_abs_diff(&[u8],&[u8])->u8`, `assert_golden(pixels:&[u8], w:u32, h:u32, name:&str)`.

- [ ] **Step 1: Wire modules + re-exports in `lib.rs`**

Make `src/lib.rs` the final version from Task 1 Step 3 (all modules + all re-exports). It will only compile once the items exist; this task creates `image`, `nodes::upload_source`, `pipeline::blit_to_rgba8`. `EditPipeline` (in the re-export) does not exist yet — temporarily re-export only what exists:

```rust
pub use image::PipelineImage;
pub use op::{Contrast, Exposure, Op, OpKind, OpStack, WhiteBalance, STACK_VERSION};
pub use pipeline::{blit_to_rgba8, upload_source};
pub use serialize::{deserialize, serialize};
pub use uniforms::{ContrastUniform, ExposureUniform, WbUniform};
```

(Add `EditPipeline` to the `pipeline` re-export in Task 5. `upload_source` lives in `nodes` but is re-exported via `pipeline` — instead re-export it from `nodes`: `pub use nodes::upload_source;`. Use that.)

Final Task-4 `lib.rs` re-export block:

```rust
pub use image::PipelineImage;
pub use nodes::upload_source;
pub use op::{Contrast, Exposure, Op, OpKind, OpStack, WhiteBalance, STACK_VERSION};
pub use pipeline::blit_to_rgba8;
pub use serialize::{deserialize, serialize};
pub use uniforms::{ContrastUniform, ExposureUniform, WbUniform};
```

- [ ] **Step 2: Create `src/image.rs`**

```rust
//! `PipelineImage` — a GPU-resident `Rgba16Float` image (display-linear). Cheap
//! to clone (Arc handle); it is the node output type `O` of the edit DAG.

use std::sync::Arc;

/// The internal pipeline texture format (display-linear, f16).
pub const PIPELINE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[derive(Clone)]
pub struct PipelineImage {
    pub texture: Arc<wgpu::Texture>,
    pub width: u32,
    pub height: u32,
}
```

- [ ] **Step 3: Create `src/nodes.rs` with `upload_source`**

```rust
//! GPU edit nodes. This task adds only `upload_source` (the graph root upload);
//! `SourceNode` and `PointOpNode` arrive in later tasks.

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use half::f16;
use std::sync::Arc;
use wgpu::util::DeviceExt;

use crate::image::{PipelineImage, PIPELINE_FORMAT};

/// Upload a display-linear `f32` image as an `Rgba16Float` GPU texture (the
/// pipeline source). Mirrors the VT's single-texture upload (f32 -> f16).
pub fn upload_source(ctx: &GpuContext, img: &LinearRgbaF32) -> PipelineImage {
    let texels: Vec<f16> = img.pixels.iter().map(|&v| f16::from_f32(v)).collect();
    let texture = ctx.device.create_texture_with_data(
        &ctx.queue,
        &wgpu::TextureDescriptor {
            label: Some("pipeline-source"),
            size: wgpu::Extent3d {
                width: img.width,
                height: img.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PIPELINE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(&texels),
    );
    PipelineImage {
        texture: Arc::new(texture),
        width: img.width,
        height: img.height,
    }
}
```

- [ ] **Step 4: Create `src/shaders/blit.wgsl`**

```wgsl
// Full-screen blit of a display-linear Rgba16Float texture to an sRGB-encoded
// Rgba8 target. Nearest sampling (1:1 readback for golden tests).

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var out: VsOut;
    let xy = p[vid];
    out.pos = vec4(xy, 0.0, 1.0);
    out.uv = (xy * 0.5 + vec2(0.5, 0.5)) * vec2(1.0, -1.0) + vec2(0.0, 1.0);
    return out;
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let lin = textureSampleLevel(src, samp, in.uv, 0.0).rgb;
    return vec4(linear_to_srgb(lin), 1.0);
}
```

- [ ] **Step 5: Create `src/pipeline.rs` with `blit_to_rgba8`**

```rust
//! `EditPipeline` (later task) + the `blit_to_rgba8` display/readback helper.

use ferrolite_gpu::GpuContext;

use crate::image::PipelineImage;

/// Render a display-linear `PipelineImage` to an sRGB `Rgba8Unorm` buffer at 1:1
/// (its own dims), returning `width*height*4` row-unpadded bytes. Used by golden
/// tests and (later) any CPU-side preview/export readback. Builds its pipeline
/// per call — acceptable for the test/readback path (not the per-frame path).
pub fn blit_to_rgba8(ctx: &GpuContext, img: &PipelineImage) -> Vec<u8> {
    let device = &ctx.device;
    let (w, h) = (img.width, img.height);

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pipeline-blit"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/blit.wgsl").into()),
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("pipeline-blit-samp"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("pipeline-blit-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-blit-pl"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("pipeline-blit-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    let src_view = img.texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("pipeline-blit-bind"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&src_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let target = ctx.render_target(w, h, wgpu::TextureFormat::Rgba8Unorm);
    let tview = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pipeline-blit-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &tview,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
    ctx.queue.submit([enc.finish()]);
    ctx.read_rgba8(&target, w, h)
}
```

- [ ] **Step 6: Create `tests/common/mod.rs`**

```rust
//! Shared golden-test helpers (mirrors ferrolite-vt/tests/common). Golden PNGs
//! are authored on the dev GPU (set UPDATE_GOLDEN=1 or delete the fixture) and
//! committed; in headless CI the GPU tests skip before reaching these.

use ferrolite_image::LinearRgbaF32;

/// A deterministic RGB gradient used as the edit source.
pub fn gradient(w: u32, h: u32) -> LinearRgbaF32 {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[
                x as f32 / w as f32,
                y as f32 / h as f32,
                0.25,
                1.0,
            ]);
        }
    }
    LinearRgbaF32::new(w, h, px).expect("gradient length")
}

pub fn max_abs_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

const TOL: u8 = 4; // absorbs driver float differences

/// Compare `pixels` against `tests/fixtures/<name>`. Authors the golden if the
/// file is absent or UPDATE_GOLDEN is set (then returns, passing).
pub fn assert_golden(pixels: &[u8], w: u32, h: u32, name: &str) {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    if std::env::var("UPDATE_GOLDEN").is_ok() || !std::path::Path::new(&path).exists() {
        std::fs::create_dir_all(format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"))).unwrap();
        image::save_buffer(&path, pixels, w, h, image::ColorType::Rgba8).unwrap();
        eprintln!("wrote golden {path}");
        return;
    }
    let golden = image::open(&path).unwrap().to_rgba8();
    assert_eq!(golden.dimensions(), (w, h), "golden dims mismatch: {name}");
    assert!(
        max_abs_diff(pixels, golden.as_raw()) <= TOL,
        "{name}: rendered output drifted from golden beyond tolerance"
    );
}
```

- [ ] **Step 7: Write the failing golden test**

`ferrolite-pipeline/tests/golden.rs`:

```rust
mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_pipeline::{blit_to_rgba8, upload_source};

const W: u32 = 64;
const H: u32 = 48;

#[test]
fn source_upload_blit_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping golden (expected in headless CI)");
        return;
    };
    let src = common::gradient(W, H);
    let img = upload_source(&ctx, &src);
    let pixels = blit_to_rgba8(&ctx, &img);
    common::assert_golden(&pixels, W, H, "source.png");
}
```

- [ ] **Step 8: Run to verify it fails, then passes**

Run: `cargo test -p ferrolite-pipeline --test golden`
Expected (no GPU): compiles, test prints "no GPU adapter; skipping" and PASSES.
Expected (dev GPU, first run): authors `tests/fixtures/source.png`, prints "wrote golden", PASSES. Re-running compares against it.

- [ ] **Step 9: Commit**

```bash
git add ferrolite-pipeline/ 
git commit -m "feat(pipeline): PipelineImage, source upload, sRGB blit + golden harness"
```

---

### Task 5: `PointOpNode` + exposure pass + `EditPipeline` (source→exposure)

**Files:**
- Modify: `ferrolite-pipeline/src/nodes.rs` (add `SourceNode`, generic `PointOpNode<U>`, `point_op_bgl`, `point_op_pipeline`)
- Modify: `ferrolite-pipeline/src/pipeline.rs` (add `EditPipeline`)
- Create: `ferrolite-pipeline/src/shaders/exposure.wgsl`
- Modify: `ferrolite-pipeline/src/lib.rs` (re-export `EditPipeline`)
- Modify: `ferrolite-pipeline/tests/golden.rs` (add exposure tests)

**Interfaces:**
- Consumes: `Graph`, `Node`, `NodeId` (from `ferrolite_gpu`), `PipelineImage`, `upload_source`, `ExposureUniform`, `exposure_uniform`, `OpStack`.
- Produces: `pub struct EditPipeline` with `pub fn new(ctx: Arc<GpuContext>, source: &LinearRgbaF32, stack: OpStack) -> Self`, `pub fn set_stack(&mut self, stack: OpStack)`, `pub fn evaluate(&mut self) -> PipelineImage`, `pub fn eval_count(&self) -> usize`, `pub fn render_to_image(&mut self) -> Vec<u8>`. Internal: `SourceNode`, `PointOpNode<U: bytemuck::Pod>` implementing `Node<PipelineImage>`.

- [ ] **Step 1: Create `src/shaders/exposure.wgsl`**

```wgsl
// Exposure: multiply linear RGB by a gain (2^EV). Point op.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P { gain: f32, pad0: f32, pad1: f32, pad2: f32 };
@group(0) @binding(2) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    textureStore(dst, xy, vec4<f32>(c.rgb * p.gain, c.a));
}
```

- [ ] **Step 2: Add the node machinery to `src/nodes.rs`**

Append:

```rust
use ferrolite_gpu::Node;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::image::PIPELINE_FORMAT;

/// Graph root: returns the pre-uploaded source image (ignores inputs).
pub(crate) struct SourceNode {
    image: PipelineImage,
}

impl SourceNode {
    pub(crate) fn new(ctx: &GpuContext, src: &LinearRgbaF32) -> Self {
        Self {
            image: upload_source(ctx, src),
        }
    }
}

impl Node<PipelineImage> for SourceNode {
    fn evaluate(&self, _inputs: &[&PipelineImage]) -> PipelineImage {
        self.image.clone()
    }
}

/// Bind-group layout shared by every point-op compute pass:
/// 0 = input texture, 1 = output storage texture, 2 = params uniform.
pub(crate) fn point_op_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("point-op-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: PIPELINE_FORMAT,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

fn point_op_pipeline(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    wgsl: &str,
    label: &str,
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        module: &module,
        entry_point: "main",
        compilation_options: Default::default(),
        cache: None,
    })
}

/// A single point-op compute pass. Owns its (once-built) pipeline + a reusable
/// output texture; reads its current params from a shared `Cell` each evaluate.
pub(crate) struct PointOpNode<U: bytemuck::Pod> {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    params: Rc<Cell<U>>,
    out: RefCell<Option<PipelineImage>>,
}

impl<U: bytemuck::Pod> PointOpNode<U> {
    pub(crate) fn new(ctx: Arc<GpuContext>, wgsl: &str, label: &str, params: Rc<Cell<U>>) -> Self {
        let bgl = point_op_bgl(&ctx.device);
        let pipeline = point_op_pipeline(&ctx.device, &bgl, wgsl, label);
        let uniform_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: std::mem::size_of::<U>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            ctx,
            pipeline,
            bgl,
            uniform_buf,
            params,
            out: RefCell::new(None),
        }
    }

    /// Allocate (or reuse) the output texture matching `(w,h)`.
    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((w, h)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("point-op-out"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PIPELINE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            });
            *out = Some(PipelineImage {
                texture: Arc::new(tex),
                width: w,
                height: h,
            });
        }
        out.as_ref().unwrap().clone()
    }
}

impl<U: bytemuck::Pod> Node<PipelineImage> for PointOpNode<U> {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let src = inputs[0];
        let dst = self.ensure_out(src.width, src.height);

        // Current params -> uniform buffer.
        self.ctx
            .queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&self.params.get()));

        let src_view = src.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("point-op-bind"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&dst_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.uniform_buf.as_entire_binding(),
                },
            ],
        });

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("point-op-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(src.width.div_ceil(8), src.height.div_ceil(8), 1);
        }
        self.ctx.queue.submit([enc.finish()]);
        dst
    }
}
```

- [ ] **Step 3: Add `EditPipeline` to `src/pipeline.rs`**

Prepend the imports and the struct (keep `blit_to_rgba8` below it):

```rust
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use ferrolite_gpu::{Graph, NodeId};
use ferrolite_image::LinearRgbaF32;

use crate::nodes::{PointOpNode, SourceNode};
use crate::op::OpStack;
use crate::uniforms::{exposure_uniform, ExposureUniform};

/// The retained photo edit pipeline: a `Graph<PipelineImage>` of a source node
/// feeding the fixed canonical op chain. Editing updates a shared param cell and
/// marks that op's node dirty, so only it + downstream re-evaluate.
pub struct EditPipeline {
    ctx: Arc<GpuContext>,
    graph: Graph<PipelineImage>,
    output_id: NodeId,
    exposure_id: NodeId,
    exposure: Rc<Cell<ExposureUniform>>,
    stack: OpStack,
}

impl EditPipeline {
    pub fn new(ctx: Arc<GpuContext>, source: &LinearRgbaF32, stack: OpStack) -> Self {
        let mut graph = Graph::new();
        let source_id = graph.add_node(Box::new(SourceNode::new(&ctx, source)), vec![]);

        let exposure = Rc::new(Cell::new(exposure_uniform(stack.exposure())));
        let exposure_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/exposure.wgsl"),
            "exposure",
            exposure.clone(),
        );
        let exposure_id = graph.add_node(Box::new(exposure_node), vec![source_id]);

        Self {
            ctx,
            graph,
            output_id: exposure_id,
            exposure_id,
            exposure,
            stack,
        }
    }

    /// Apply a new op stack, dirtying only the nodes whose params changed.
    pub fn set_stack(&mut self, stack: OpStack) {
        let e = exposure_uniform(stack.exposure());
        if e != self.exposure.get() {
            self.exposure.set(e);
            self.graph.mark_dirty(self.exposure_id);
        }
        self.stack = stack;
    }

    /// Evaluate the pipeline output (re-running only dirty nodes).
    pub fn evaluate(&mut self) -> PipelineImage {
        self.graph.evaluate(self.output_id).clone()
    }

    /// Total node evaluations so far (for per-op invalidation tests).
    pub fn eval_count(&self) -> usize {
        self.graph.eval_count()
    }

    /// Evaluate and read back to an sRGB Rgba8 buffer (golden tests).
    pub fn render_to_image(&mut self) -> Vec<u8> {
        let out = self.evaluate();
        blit_to_rgba8(&self.ctx, &out)
    }
}
```

(Note: `GpuContext` and `PipelineImage` are already imported at the top of `pipeline.rs` from Task 4; ensure both `use ferrolite_gpu::GpuContext;` and `use crate::image::PipelineImage;` are present.)

- [ ] **Step 4: Re-export `EditPipeline` in `lib.rs`**

Change the `pipeline` re-export line to `pub use pipeline::{blit_to_rgba8, EditPipeline};`.

- [ ] **Step 5: Write the failing exposure tests**

Append to `tests/golden.rs`:

```rust
use ferrolite_pipeline::{EditPipeline, Exposure, Op, OpStack};
use std::sync::Arc;

#[test]
fn exposure_plus_one_ev_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::Exposure(Exposure { ev: 1.0 }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "exposure_plus1.png");
}

#[test]
fn identity_stack_matches_source_render() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let src = common::gradient(W, H);
    // Source rendered directly through the blit.
    let source_render = blit_to_rgba8(&ctx, &upload_source(&ctx, &src));
    // Empty stack through the full pipeline must match within tolerance.
    let mut pipe = EditPipeline::new(ctx.clone(), &src, OpStack::default());
    let edited = pipe.render_to_image();
    let diff = common::max_abs_diff(&source_render, &edited);
    assert!(diff <= 4, "identity stack diverged from source (diff {diff})");
}
```

- [ ] **Step 6: Run to verify (fails to compile until Steps 1–4 land, then passes)**

Run: `cargo test -p ferrolite-pipeline --test golden`
Expected (no GPU): compiles, all golden tests skip + PASS.
Expected (dev GPU): authors `exposure_plus1.png` on first run; `identity_stack_matches_source_render` PASSES (GPU-vs-GPU, tolerance 4).

- [ ] **Step 7: Commit**

```bash
git add ferrolite-pipeline/
git commit -m "feat(pipeline): EditPipeline + exposure compute pass"
```

---

### Task 6: White-balance pass

**Files:**
- Create: `ferrolite-pipeline/src/shaders/white_balance.wgsl`
- Modify: `ferrolite-pipeline/src/pipeline.rs` (add WB node to the chain + `set_stack`)
- Modify: `ferrolite-pipeline/tests/golden.rs` (WB golden)

**Interfaces:**
- Consumes: `WbUniform`, `wb_uniform` (Task 3); `PointOpNode` (Task 5).
- Produces: WB node inserted between exposure and the output; `EditPipeline` gains `wb_id` + `wb: Rc<Cell<WbUniform>>`; `set_stack` handles WB.

- [ ] **Step 1: Create `src/shaders/white_balance.wgsl`**

```wgsl
// White balance: per-channel linear multipliers. Point op.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P { mul: vec3<f32>, pad: f32 };
@group(0) @binding(2) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    textureStore(dst, xy, vec4<f32>(c.rgb * p.mul, c.a));
}
```

- [ ] **Step 2: Extend `EditPipeline` with the WB node**

In `src/pipeline.rs`: add to imports `use crate::uniforms::{wb_uniform, WbUniform};` (alongside the exposure ones). Add fields `wb_id: NodeId` and `wb: Rc<Cell<WbUniform>>` to the struct. In `new`, after building the exposure node:

```rust
        let wb = Rc::new(Cell::new(wb_uniform(stack.white_balance())));
        let wb_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/white_balance.wgsl"),
            "white-balance",
            wb.clone(),
        );
        let wb_id = graph.add_node(Box::new(wb_node), vec![exposure_id]);
```

Set `output_id: wb_id` and add `wb_id, wb` to the struct literal. In `set_stack`, after the exposure block:

```rust
        let w = wb_uniform(stack.white_balance());
        if w != self.wb.get() {
            self.wb.set(w);
            self.graph.mark_dirty(self.wb_id);
        }
```

- [ ] **Step 3: Write the failing WB golden**

Append to `tests/golden.rs`:

```rust
use ferrolite_pipeline::WhiteBalance;

#[test]
fn white_balance_warm_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::WhiteBalance(WhiteBalance { temp: 0.5, tint: -0.2 }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "wb_warm.png");
}
```

- [ ] **Step 4: Run + verify**

Run: `cargo test -p ferrolite-pipeline --test golden`
Expected: no-GPU skip+pass; dev GPU authors `wb_warm.png`. `identity_stack_matches_source_render` still passes (WB identity = [1,1,1]).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-pipeline/
git commit -m "feat(pipeline): white-balance compute pass"
```

---

### Task 7: Contrast pass

**Files:**
- Create: `ferrolite-pipeline/src/shaders/contrast.wgsl`
- Modify: `ferrolite-pipeline/src/pipeline.rs` (add contrast node as the new output)
- Modify: `ferrolite-pipeline/tests/golden.rs` (contrast golden)

**Interfaces:**
- Consumes: `ContrastUniform`, `contrast_uniform` (Task 3).
- Produces: contrast node appended after WB; `output_id` becomes `contrast_id`; `EditPipeline` gains `contrast_id` + `contrast: Rc<Cell<ContrastUniform>>`; `set_stack` handles contrast.

- [ ] **Step 1: Create `src/shaders/contrast.wgsl`**

```wgsl
// Contrast: scale linear RGB about a fixed mid-grey pivot. Point op.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P { gain: f32, pivot: f32, pad0: f32, pad1: f32 };
@group(0) @binding(2) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    let rgb = (c.rgb - vec3(p.pivot)) * p.gain + vec3(p.pivot);
    textureStore(dst, xy, vec4<f32>(rgb, c.a));
}
```

- [ ] **Step 2: Extend `EditPipeline` with the contrast node**

In `src/pipeline.rs`: add `use crate::uniforms::{contrast_uniform, ContrastUniform};`. Add struct fields `contrast_id: NodeId`, `contrast: Rc<Cell<ContrastUniform>>`. In `new`, after the WB node:

```rust
        let contrast = Rc::new(Cell::new(contrast_uniform(stack.contrast())));
        let contrast_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/contrast.wgsl"),
            "contrast",
            contrast.clone(),
        );
        let contrast_id = graph.add_node(Box::new(contrast_node), vec![wb_id]);
```

Set `output_id: contrast_id`; add `contrast_id, contrast` to the struct literal. In `set_stack`, after the WB block:

```rust
        let c = contrast_uniform(stack.contrast());
        if c != self.contrast.get() {
            self.contrast.set(c);
            self.graph.mark_dirty(self.contrast_id);
        }
```

- [ ] **Step 3: Write the failing contrast golden**

Append to `tests/golden.rs`:

```rust
use ferrolite_pipeline::Contrast;

#[test]
fn contrast_boost_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::Contrast(Contrast { amount: 0.5 }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "contrast_boost.png");
}
```

- [ ] **Step 4: Run + verify**

Run: `cargo test -p ferrolite-pipeline --test golden`
Expected: no-GPU skip+pass; dev GPU authors `contrast_boost.png`; identity test still green.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-pipeline/
git commit -m "feat(pipeline): contrast compute pass"
```

---

### Task 8: Full-stack composition + per-op invalidation

**Files:**
- Modify: `ferrolite-pipeline/tests/golden.rs` (composed-stack golden + cache-reuse test)

**Interfaces:**
- Consumes: the complete `EditPipeline` (Tasks 5–7), `eval_count`.
- Produces: no new production code — this task verifies composition + per-op invalidation, the contract that justified building on the retained DAG.

- [ ] **Step 1: Write the failing composition + invalidation tests**

Append to `tests/golden.rs`:

```rust
#[test]
fn full_stack_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default()
        .set_op(Op::Exposure(Exposure { ev: 0.5 }))
        .set_op(Op::WhiteBalance(WhiteBalance { temp: 0.3, tint: 0.0 }))
        .set_op(Op::Contrast(Contrast { amount: 0.4 }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "full_stack.png");
}

#[test]
fn editing_one_op_reevaluates_minimally() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let base = OpStack::default()
        .set_op(Op::Exposure(Exposure { ev: 0.2 }))
        .set_op(Op::WhiteBalance(WhiteBalance { temp: 0.1, tint: 0.0 }))
        .set_op(Op::Contrast(Contrast { amount: 0.1 }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), base.clone());

    let _ = pipe.evaluate();
    let after_first = pipe.eval_count();
    assert_eq!(after_first, 4, "source + 3 ops each evaluated once");

    // Change only contrast -> only contrast re-runs.
    pipe.set_stack(base.set_op(Op::Contrast(Contrast { amount: 0.9 })));
    let _ = pipe.evaluate();
    assert_eq!(
        pipe.eval_count(),
        after_first + 1,
        "only the contrast node re-evaluated"
    );

    // Change exposure -> exposure + WB + contrast re-run (downstream), source cached.
    let prev = pipe.eval_count();
    pipe.set_stack(
        OpStack::default()
            .set_op(Op::Exposure(Exposure { ev: 1.5 }))
            .set_op(Op::WhiteBalance(WhiteBalance { temp: 0.1, tint: 0.0 }))
            .set_op(Op::Contrast(Contrast { amount: 0.9 })),
    );
    let _ = pipe.evaluate();
    assert_eq!(pipe.eval_count(), prev + 3, "exposure + downstream re-evaluated");
}
```

- [ ] **Step 2: Run + verify**

Run: `cargo test -p ferrolite-pipeline --test golden`
Expected (no GPU): skip+pass. Expected (dev GPU): authors `full_stack.png`; `editing_one_op_reevaluates_minimally` PASSES, proving per-op invalidation on the real pipeline.

- [ ] **Step 3: Commit**

```bash
git add ferrolite-pipeline/
git commit -m "test(pipeline): full-stack golden + per-op invalidation"
```

---

## Definition of done & gate

After Task 8, run the workspace gate from the repo root:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three must be green (on the dev GPU the new golden fixtures are authored on first run, then re-run to confirm they compare clean; commit the authored `tests/fixtures/*.png`). Then **STOP and hold for the author's (Jann's) visual test** per CLAUDE.md — the author inspects the authored golden PNGs (exposure/WB/contrast/full-stack look correct) and confirms the app still builds/opens images with no regression. The app-side preview wiring (routing the viewer's preview through `EditPipeline`) is intentionally deferred to **Plan 4**, where the adjustment-panel UI makes editing observable and the wiring is testable end-to-end; building it now would add a no-op render path (empty stack = identity) with nothing to drive it.

---

## Self-Review

**1. Spec coverage (spec §11.1 — Plan 1 scope):**
- `ferrolite-pipeline` crate ✓ (Task 1). `OpStack` model ✓ (Task 1). Serialization ✓ (Task 2). Edit DAG on `Graph<PipelineImage>` + `SourceNode` ✓ (Task 5). Point-op WGSL compute passes exposure/WB/contrast ✓ (Tasks 5–7). Golden tests ✓ (Tasks 4–8). Per-op invalidation (spec §4.2 / DAG granularity) ✓ (Task 8). Display-linear color space (spec §4.3) ✓ (blit OETF only). Executor unchanged (contract §4) ✓ (no edits to `executor.rs`). Pipelines built once (CLAUDE.md) ✓ (`PointOpNode::new` builds the pipeline; `evaluate` reuses it + the output texture).
- Deferred-with-rationale: "preview-tier render into the existing rung-1 display" app wiring → moved to Plan 4 (noted in Definition of done). This is a sequencing refinement, not a scope cut — the render path is exercised by `render_to_image`/goldens here and wired to the live viewer when the UI exists.

**2. Placeholder scan:** No TBD/TODO; every step has complete code or exact commands. The `lib.rs` re-export evolves across Tasks 1/4/5 — each task states the exact re-export block to use, so no task references an undefined item.

**3. Type consistency:** `EditPipeline::{new,set_stack,evaluate,eval_count,render_to_image}` signatures are identical across Tasks 5–8. `PointOpNode::new(ctx, wgsl, label, params)` matches its three call sites (Tasks 5/6/7). Uniform structs (`ExposureUniform`/`WbUniform`/`ContrastUniform`) and their constructors (`*_uniform(Option<…>)`) match the WGSL `struct P` layouts (16 bytes each). `PIPELINE_FORMAT = Rgba16Float` is used consistently for source, intermediates, and storage-texture BGL. `assert_golden`/`max_abs_diff`/`gradient` signatures match all call sites.

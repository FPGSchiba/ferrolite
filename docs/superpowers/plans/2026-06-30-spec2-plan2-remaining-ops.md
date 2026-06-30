# Spec 2 Plan 2 — Remaining Edit Ops (tone curve, HSL, sharpen, geometry) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the four remaining Spec 2 edit ops — tone curve (256-entry monotone LUT), HSL (8-band), sharpening (unsharp mask, the neighborhood op), and geometry (crop/rotate as a sampling transform) — to the existing preview-res `ferrolite-pipeline` edit DAG, each with a pure param→uniform unit and a golden-image GPU diff.

**Architecture:** Extend the Plan 1 `OpStack` model with four new `Op` variants in canonical apply order (after `Contrast`), add their pure param→uniform conversions to `uniforms.rs`, add their WGSL compute passes + DAG nodes, and chain them onto `EditPipeline` so the fixed source→Exposure→WB→Contrast→ToneCurve→HSL→Sharpen→Geometry graph renders the full op set. The generic `ferrolite-gpu::Graph<O>` executor is **not modified** (cross-cutting contract §4/§5): new ops are supplied as `Node<PipelineImage>` impls. Still preview-res only — the full-res tiled/halo path is Plan 3, the panel UI is Plan 4.

**Tech Stack:** Rust (edition per workspace), `wgpu 22.1` compute passes, WGSL, `bytemuck` Pod uniforms, `half::f16` uploads, `serde`/`serde_json` for the document model. Tests: `#[test]` pure units (run everywhere) + headless-skipping golden GPU diffs (`GpuContext::headless()` → `None` ⇒ skip).

## Global Constraints

- **Engine/photo tier boundary stays intact.** All new code lands in `ferrolite-pipeline` (photo tier, GPL-OK). Do **not** touch `ferrolite-gpu` (the generic executor) or `ferrolite-vt` — no `Graph<O>` changes; new ops are `Node<PipelineImage>` impls only (cross-cutting contract §4/§5; spec §3, §5).
- **Pipelines built once, reused.** Every node builds its `wgpu::ComputePipeline`/`BindGroupLayout` exactly once in its constructor and reuses it across `evaluate` calls. Never rebuild a pipeline per evaluate/edit/open (CLAUDE.md GPU rule; spec §4.2).
- **Never block the UI thread.** Plan 2 is preview-res only — a handful of ~6 MP compute passes on the render thread, inside frame budget. The pure CPU param→uniform math (`curve_lut`, `hsl_uniform`, `sharpen_uniform`, `geometry_uniform`) is trivially cheap. No new file/DB/CPU-heavy work is introduced here (CLAUDE.md §1; spec §6).
- **Color space is display-linear** — the same space `QuadBin` outputs and the blit shader sRGB-encodes. LUT/curve domains and contrast/HSL math are documented display-linear placeholders; Spec 3 refines without reworking pass structure (spec §4.3).
- **Canonical op order = `OpKind` discriminant order.** The new ops append after `Contrast`: `ToneCurve = 3`, `Hsl = 4`, `Sharpen = 5`, `Geometry = 6`. `OpStack::set_op` keeps `ops` sorted by this. An absent op = identity (spec §4.1).
- **Goldens auto-skip headless.** Every GPU golden test begins `let Some(ctx) = GpuContext::headless() else { eprintln!(...); return; };` so `cargo test --workspace` stays green in headless CI. Golden PNG fixtures are authored on the dev GPU (RTX 3060/3070 class) on first GPU run (auto-written when the fixture file is absent, per `tests/common::assert_golden`) and committed; the author validates them in the hands-on visual test (spec §10; CLAUDE.md "Finishing a branch").
- **Gate (necessary, not sufficient):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → then **STOP and hold for the author's (Jann's) visual test** before finishing the branch.
- **Tolerances:** golden compare uses `tests/common`'s `TOL = 4` (u8, post-sRGB). Identity passes must be true no-ops within tolerance — design each new pass so an absent/zero param renders the source unchanged (verified by the existing `identity_stack_matches_source_render` test, which keeps the empty stack ≤ 4).

---

## File Structure

**Modified:**
- `ferrolite-pipeline/src/op.rs` — add `ToneCurve`/`Hsl`/`Sharpen`/`Geometry` param structs + `HslBand`/`CropRect`/`Aspect`, extend `Op`/`OpKind`/`kind()`, add accessors. `Op` loses `Copy` (ToneCurve holds a `Vec`), so `set_op`/`reset` switch `.copied()` → `.cloned()`.
- `ferrolite-pipeline/src/uniforms.rs` — add `curve_lut`, `HslUniform`+`hsl_uniform`, `SharpenUniform`+`sharpen_uniform`+`sharpen_halo`, `GeometryUniform`+`geometry_uniform`.
- `ferrolite-pipeline/src/nodes.rs` — add `CurveNode` (extra LUT storage-buffer binding) and `GeometryNode` (sampler + variable output dims). HSL and Sharpen reuse the existing generic `PointOpNode<U>`.
- `ferrolite-pipeline/src/pipeline.rs` — extend `EditPipeline` to chain the four new nodes; extend `set_stack`; add `node_count()`.
- `ferrolite-pipeline/src/lib.rs` — export the new public types.
- `ferrolite-pipeline/tests/golden.rs` — add per-op goldens; update `editing_one_op_reevaluates_minimally` as the chain grows.

**Created:**
- `ferrolite-pipeline/src/shaders/tone_curve.wgsl`
- `ferrolite-pipeline/src/shaders/hsl.wgsl`
- `ferrolite-pipeline/src/shaders/sharpen.wgsl`
- `ferrolite-pipeline/src/shaders/geometry.wgsl`
- `ferrolite-pipeline/tests/fixtures/{tone_curve,hsl,sharpen,geometry}.png` (auto-authored on the dev GPU)

**Design note (recorded):** Geometry is the **last** op in the canonical chain (discriminant 6), applied to the edited result on the preview tier. Spec §8.4's "rotate at the head of the per-tile pass" is a Plan 3 full-res-tiling concern; on the preview tier, applying crop/rotate last is equivalent for per-pixel color ops (they commute with geometric resampling except at resampled edges) and keeps the Plan 1 "canonical order = discriminant order" invariant intact.

---

## Task 1: Extend the `OpStack` document model + serialization

**Files:**
- Modify: `ferrolite-pipeline/src/op.rs`
- Modify: `ferrolite-pipeline/src/serialize.rs` (tests only)

**Interfaces:**
- Consumes: nothing new (pure data).
- Produces:
  - `pub struct ToneCurve { pub points: Vec<(f32, f32)> }` (Clone, not Copy)
  - `pub struct HslBand { pub hue: f32, pub sat: f32, pub lum: f32 }` (Copy)
  - `pub struct Hsl { pub bands: [HslBand; 8] }` (Copy)
  - `pub struct Sharpen { pub amount: f32, pub radius: u32 }` (Copy)
  - `pub enum Aspect { Original, Free, Square, ThreeTwo, FourThree, SixteenNine }` (Copy)
  - `pub struct CropRect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }` (Copy)
  - `pub struct Geometry { pub crop: CropRect, pub angle_deg: f32, pub aspect: Aspect }` (Copy)
  - `Op` variants `ToneCurve(ToneCurve)`, `Hsl(Hsl)`, `Sharpen(Sharpen)`, `Geometry(Geometry)`; `OpKind::{ToneCurve=3, Hsl=4, Sharpen=5, Geometry=6}`
  - `OpStack` accessors `tone_curve() -> Option<ToneCurve>`, `hsl() -> Option<Hsl>`, `sharpen() -> Option<Sharpen>`, `geometry() -> Option<Geometry>`
- `STACK_VERSION` stays `1` — adding enum variants is backward-compatible for reading existing sidecars (they contain only old variants, which still exist).

- [ ] **Step 1: Write the failing model tests**

Append to the `#[cfg(test)] mod tests` block in `ferrolite-pipeline/src/op.rs`:

```rust
    #[test]
    fn new_ops_round_through_set_and_accessors() {
        let s = OpStack::default()
            .set_op(Op::ToneCurve(ToneCurve {
                points: vec![(0.0, 0.0), (1.0, 1.0)],
            }))
            .set_op(Op::Hsl(Hsl {
                bands: [HslBand {
                    hue: 0.1,
                    sat: 0.0,
                    lum: 0.0,
                }; 8],
            }))
            .set_op(Op::Sharpen(Sharpen {
                amount: 0.5,
                radius: 2,
            }))
            .set_op(Op::Geometry(Geometry {
                crop: CropRect {
                    x: 0.1,
                    y: 0.1,
                    w: 0.8,
                    h: 0.8,
                },
                angle_deg: 5.0,
                aspect: Aspect::Free,
            }));
        assert_eq!(s.tone_curve().unwrap().points.len(), 2);
        assert_eq!(s.hsl().unwrap().bands[0].hue, 0.1);
        assert_eq!(s.sharpen(), Some(Sharpen { amount: 0.5, radius: 2 }));
        assert_eq!(s.geometry().unwrap().angle_deg, 5.0);
    }

    #[test]
    fn full_seven_op_stack_is_in_canonical_order() {
        let s = OpStack::default()
            .set_op(Op::Geometry(Geometry {
                crop: CropRect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 },
                angle_deg: 0.0,
                aspect: Aspect::Original,
            }))
            .set_op(Op::Sharpen(Sharpen { amount: 0.3, radius: 1 }))
            .set_op(Op::Hsl(Hsl { bands: [HslBand { hue: 0.0, sat: 0.0, lum: 0.0 }; 8] }))
            .set_op(Op::ToneCurve(ToneCurve { points: vec![] }))
            .set_op(Op::Contrast(Contrast { amount: 0.1 }))
            .set_op(Op::WhiteBalance(WhiteBalance { temp: 0.0, tint: 0.0 }))
            .set_op(Op::Exposure(Exposure { ev: 0.1 }));
        let kinds: Vec<OpKind> = s.ops.iter().map(|o| o.kind()).collect();
        assert_eq!(
            kinds,
            vec![
                OpKind::Exposure,
                OpKind::WhiteBalance,
                OpKind::Contrast,
                OpKind::ToneCurve,
                OpKind::Hsl,
                OpKind::Sharpen,
                OpKind::Geometry,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-pipeline --lib op::`
Expected: FAIL — `cannot find type ToneCurve` / `no variant ToneCurve` / `no method tone_curve`.

- [ ] **Step 3: Add the new param structs**

In `ferrolite-pipeline/src/op.rs`, after the `Contrast` struct (before `enum Op`), add:

```rust
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ToneCurve {
    /// Control points in [0,1]×[0,1] (x ascending). Identity = `[(0,0),(1,1)]`
    /// or empty. Baked to a 256-entry monotone LUT by `uniforms::curve_lut`.
    pub points: Vec<(f32, f32)>,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct HslBand {
    /// Hue shift, normalized [-1, 1]. 0 = identity.
    pub hue: f32,
    /// Saturation delta, normalized [-1, 1]. 0 = identity.
    pub sat: f32,
    /// Lightness delta, normalized [-1, 1]. 0 = identity.
    pub lum: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Hsl {
    /// Per-band deltas; bands = red, orange, yellow, green, aqua, blue,
    /// purple, magenta (the canonical 8-band order). All-zero = identity.
    pub bands: [HslBand; 8],
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Sharpen {
    /// Unsharp-mask amount (>= 0). 0 = identity.
    pub amount: f32,
    /// Box-blur radius in pixels (drives the halo size in Plan 3). 0 = identity.
    pub radius: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Aspect {
    Original,
    Free,
    Square,
    ThreeTwo,
    FourThree,
    SixteenNine,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct CropRect {
    /// Normalized crop in source space: (x, y) top-left, (w, h) extent, all [0,1].
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl CropRect {
    /// The whole image (no crop).
    pub fn full() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Geometry {
    pub crop: CropRect,
    /// Rotation in degrees about the crop center. 0 = identity.
    pub angle_deg: f32,
    pub aspect: Aspect,
}
```

- [ ] **Step 4: Extend `Op`, `OpKind`, and `kind()`**

Replace the `enum Op` block and the `enum OpKind` block and the `impl Op` block in `ferrolite-pipeline/src/op.rs` with:

```rust
/// One adjustment in the stack. `Op` is `Clone` (not `Copy`) because `ToneCurve`
/// carries a `Vec` of control points.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Op {
    Exposure(Exposure),
    WhiteBalance(WhiteBalance),
    Contrast(Contrast),
    ToneCurve(ToneCurve),
    Hsl(Hsl),
    Sharpen(Sharpen),
    Geometry(Geometry),
}

/// Canonical op identity + apply order (the discriminant order is the order ops
/// are applied in the pipeline chain).
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    Exposure = 0,
    WhiteBalance = 1,
    Contrast = 2,
    ToneCurve = 3,
    Hsl = 4,
    Sharpen = 5,
    Geometry = 6,
}

impl Op {
    pub fn kind(&self) -> OpKind {
        match self {
            Op::Exposure(_) => OpKind::Exposure,
            Op::WhiteBalance(_) => OpKind::WhiteBalance,
            Op::Contrast(_) => OpKind::Contrast,
            Op::ToneCurve(_) => OpKind::ToneCurve,
            Op::Hsl(_) => OpKind::Hsl,
            Op::Sharpen(_) => OpKind::Sharpen,
            Op::Geometry(_) => OpKind::Geometry,
        }
    }
}
```

- [ ] **Step 5: Make `set_op`/`reset` clone instead of copy, and add accessors**

In `ferrolite-pipeline/src/op.rs`, change the two `.copied()` calls in `set_op` and `reset` to `.cloned()`:

```rust
    pub fn set_op(&self, op: Op) -> OpStack {
        let k = op.kind();
        let mut ops: Vec<Op> = self.ops.iter().cloned().filter(|o| o.kind() != k).collect();
        ops.push(op);
        ops.sort_by_key(|o| o.kind() as u8);
        OpStack {
            version: self.version,
            ops,
        }
    }

    pub fn reset(&self, kind: OpKind) -> OpStack {
        OpStack {
            version: self.version,
            ops: self
                .ops
                .iter()
                .cloned()
                .filter(|o| o.kind() != kind)
                .collect(),
        }
    }
```

Then add the four accessors after the existing `contrast()` accessor (inside `impl OpStack`):

```rust
    pub fn tone_curve(&self) -> Option<ToneCurve> {
        self.ops.iter().find_map(|o| match o {
            Op::ToneCurve(t) => Some(t.clone()),
            _ => None,
        })
    }

    pub fn hsl(&self) -> Option<Hsl> {
        self.ops.iter().find_map(|o| match o {
            Op::Hsl(h) => Some(*h),
            _ => None,
        })
    }

    pub fn sharpen(&self) -> Option<Sharpen> {
        self.ops.iter().find_map(|o| match o {
            Op::Sharpen(s) => Some(*s),
            _ => None,
        })
    }

    pub fn geometry(&self) -> Option<Geometry> {
        self.ops.iter().find_map(|o| match o {
            Op::Geometry(g) => Some(*g),
            _ => None,
        })
    }
```

- [ ] **Step 6: Run the model tests to verify they pass**

Run: `cargo test -p ferrolite-pipeline --lib op::`
Expected: PASS (all `op::tests::*`, including the two new ones).

- [ ] **Step 7: Add a serialization round-trip test for the full op set**

Append to the `#[cfg(test)] mod tests` block in `ferrolite-pipeline/src/serialize.rs`. First extend its `use` line to import the new types:

```rust
    use crate::op::{
        Aspect, Contrast, CropRect, Exposure, Geometry, Hsl, HslBand, Op, Sharpen, ToneCurve,
        WhiteBalance,
    };
```

Then add:

```rust
    #[test]
    fn round_trips_all_seven_ops() {
        let s = OpStack::default()
            .set_op(Op::Exposure(Exposure { ev: 0.5 }))
            .set_op(Op::WhiteBalance(WhiteBalance { temp: 0.2, tint: -0.1 }))
            .set_op(Op::Contrast(Contrast { amount: 0.3 }))
            .set_op(Op::ToneCurve(ToneCurve {
                points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
            }))
            .set_op(Op::Hsl(Hsl {
                bands: [HslBand { hue: 0.1, sat: -0.2, lum: 0.05 }; 8],
            }))
            .set_op(Op::Sharpen(Sharpen { amount: 0.6, radius: 3 }))
            .set_op(Op::Geometry(Geometry {
                crop: CropRect { x: 0.05, y: 0.05, w: 0.9, h: 0.9 },
                angle_deg: 2.5,
                aspect: Aspect::SixteenNine,
            }));
        let text = serialize(&s);
        assert_eq!(deserialize(&text), Some(s));
    }
```

- [ ] **Step 8: Run the serialization test to verify it passes**

Run: `cargo test -p ferrolite-pipeline --lib serialize::`
Expected: PASS (including `round_trips_all_seven_ops`).

- [ ] **Step 9: Commit**

```bash
git add ferrolite-pipeline/src/op.rs ferrolite-pipeline/src/serialize.rs
git commit -m "feat(pipeline): model tone-curve/HSL/sharpen/geometry ops"
```

---

## Task 2: Tone curve — `curve_lut` unit + `CurveNode` + WGSL pass + golden

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs`
- Modify: `ferrolite-pipeline/src/nodes.rs`
- Modify: `ferrolite-pipeline/src/pipeline.rs`
- Modify: `ferrolite-pipeline/src/lib.rs`
- Create: `ferrolite-pipeline/src/shaders/tone_curve.wgsl`
- Modify: `ferrolite-pipeline/tests/golden.rs`

**Interfaces:**
- Consumes: `OpStack::tone_curve() -> Option<ToneCurve>` (Task 1); `PipelineImage`, `GpuContext`, `Graph`, `NodeId`; `crate::image::PIPELINE_FORMAT`.
- Produces:
  - `pub fn curve_lut(points: &[(f32, f32)]) -> [f32; 256]` (pure; identity ramp for empty/`[(0,0),(1,1)]`; linearly interpolated; monotone non-decreasing).
  - `pub(crate) struct CurveNode` implementing `Node<PipelineImage>`, constructed `CurveNode::new(ctx: Arc<GpuContext>, lut: Rc<Cell<[f32; 256]>>) -> CurveNode`.
  - `EditPipeline` gains a tone-curve node whose output becomes the new pipeline output; `node_count(&self) -> usize`.

- [ ] **Step 1: Write the failing `curve_lut` tests**

Append to the `#[cfg(test)] mod tests` block in `ferrolite-pipeline/src/uniforms.rs`:

```rust
    #[test]
    fn curve_lut_identity_is_a_linear_ramp() {
        let lut = curve_lut(&[(0.0, 0.0), (1.0, 1.0)]);
        assert!((lut[0] - 0.0).abs() < 1e-6);
        assert!((lut[255] - 1.0).abs() < 1e-6);
        assert!((lut[128] - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn curve_lut_empty_points_is_identity() {
        let lut = curve_lut(&[]);
        assert!((lut[64] - 64.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn curve_lut_pulls_midtones_down() {
        // A point below the diagonal at x=0.5 darkens the midtones.
        let lut = curve_lut(&[(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)]);
        assert!(lut[128] < 128.0 / 255.0, "midpoint pulled below diagonal");
        assert!((lut[0] - 0.0).abs() < 1e-6);
        assert!((lut[255] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn curve_lut_is_monotone_non_decreasing() {
        // A non-monotone control set must still produce a non-decreasing LUT.
        let lut = curve_lut(&[(0.0, 0.0), (0.5, 0.8), (1.0, 0.2)]);
        for i in 1..256 {
            assert!(lut[i] >= lut[i - 1], "lut dipped at {i}");
        }
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests::curve`
Expected: FAIL — `cannot find function curve_lut`.

- [ ] **Step 3: Implement `curve_lut`**

Add to `ferrolite-pipeline/src/uniforms.rs` (after the existing free functions, before the uniform structs):

```rust
/// Bake tone-curve control points into a 256-entry display-linear LUT.
/// Points are clamped to [0,1], sorted by x, linearly interpolated, and held
/// flat outside the control range; the result is forced monotone
/// non-decreasing. Empty input is the identity ramp.
pub fn curve_lut(points: &[(f32, f32)]) -> [f32; 256] {
    let mut pts: Vec<(f32, f32)> = points
        .iter()
        .map(|&(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
        .collect();
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if pts.is_empty() {
        pts = vec![(0.0, 0.0), (1.0, 1.0)];
    }

    let mut lut = [0.0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let x = i as f32 / 255.0;
        *slot = curve_interp(&pts, x);
    }
    for i in 1..256 {
        if lut[i] < lut[i - 1] {
            lut[i] = lut[i - 1];
        }
    }
    lut
}

/// Piecewise-linear sample of sorted control points; flat (clamped) outside.
fn curve_interp(pts: &[(f32, f32)], x: f32) -> f32 {
    if x <= pts[0].0 {
        return pts[0].1;
    }
    let last = pts[pts.len() - 1];
    if x >= last.0 {
        return last.1;
    }
    for w in pts.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        if x >= x0 && x <= x1 {
            let t = if (x1 - x0).abs() < 1e-9 {
                0.0
            } else {
                (x - x0) / (x1 - x0)
            };
            return y0 + t * (y1 - y0);
        }
    }
    last.1
}
```

- [ ] **Step 4: Run to verify the unit passes**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests::curve`
Expected: PASS (all four `curve_lut_*`).

- [ ] **Step 5: Write the tone-curve WGSL pass**

Create `ferrolite-pipeline/src/shaders/tone_curve.wgsl`:

```wgsl
// Tone curve: per-channel 256-entry display-linear LUT with linear
// interpolation between entries (so an identity ramp is exactly identity).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<storage, read> lut: array<f32, 256>;

fn apply_lut(v: f32) -> f32 {
    let x = clamp(v, 0.0, 1.0) * 255.0;
    let i0 = u32(floor(x));
    let i1 = min(i0 + 1u, 255u);
    let f = x - floor(x);
    return mix(lut[i0], lut[i1], f);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    let rgb = vec3<f32>(apply_lut(c.r), apply_lut(c.g), apply_lut(c.b));
    textureStore(dst, xy, vec4<f32>(rgb, c.a));
}
```

- [ ] **Step 6: Implement `CurveNode`**

Add to `ferrolite-pipeline/src/nodes.rs` (the `use` block already imports `Cell`, `RefCell`, `Rc`, `Arc`, `GpuContext`, `Node`, `PipelineImage`, `PIPELINE_FORMAT`):

```rust
/// Bind-group layout for the tone-curve pass: 0 = input texture,
/// 1 = output storage texture, 2 = 256-entry LUT (read-only storage buffer).
fn curve_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("curve-bgl"),
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
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

/// Tone-curve compute pass. Owns its (once-built) pipeline + a 256-entry LUT
/// storage buffer; re-reads its LUT from a shared `Cell` each evaluate.
pub(crate) struct CurveNode {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    lut_buf: wgpu::Buffer,
    lut: Rc<Cell<[f32; 256]>>,
    out: RefCell<Option<PipelineImage>>,
}

impl CurveNode {
    pub(crate) fn new(ctx: Arc<GpuContext>, lut: Rc<Cell<[f32; 256]>>) -> Self {
        let bgl = curve_bgl(&ctx.device);
        let module = ctx.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tone-curve"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/tone_curve.wgsl").into()),
        });
        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("tone-curve"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            });
        let pipeline = ctx
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("tone-curve"),
                layout: Some(&layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            });
        let lut_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tone-curve-lut"),
            size: (std::mem::size_of::<f32>() * 256) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            ctx,
            pipeline,
            bgl,
            lut_buf,
            lut,
            out: RefCell::new(None),
        }
    }

    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((w, h)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("curve-out"),
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

impl Node<PipelineImage> for CurveNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let src = inputs[0];
        let dst = self.ensure_out(src.width, src.height);

        let lut = self.lut.get();
        // `[f32; 256]: Pod` via bytemuck's const-generic array impl.
        self.ctx
            .queue
            .write_buffer(&self.lut_buf, 0, bytemuck::bytes_of(&lut));

        let src_view = src
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("curve-bind"),
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
                    resource: self.lut_buf.as_entire_binding(),
                },
            ],
        });

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("curve-pass"),
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

- [ ] **Step 7: Chain `CurveNode` into `EditPipeline` and add a local `node_count`**

`ferrolite-gpu` is frozen (Global Constraints), so the node count is tracked locally in `EditPipeline`, never added to `Graph`. In `ferrolite-pipeline/src/pipeline.rs`:

1. Extend the `use crate::nodes` line:
```rust
use crate::nodes::{CurveNode, PointOpNode, SourceNode};
```
2. Extend the `use crate::uniforms` import to also bring in `curve_lut`:
```rust
use crate::uniforms::{
    contrast_uniform, curve_lut, exposure_uniform, wb_uniform, ContrastUniform, ExposureUniform,
    WbUniform,
};
```
3. Add fields to the `struct EditPipeline` definition (after the `contrast` field):
```rust
    tone_curve_id: NodeId,
    tone_curve: Rc<Cell<[f32; 256]>>,
    node_count: usize,
```
4. In `EditPipeline::new`, after the `contrast_id` node is created, insert the tone-curve node and re-point the output:
```rust
        let tone_curve = Rc::new(Cell::new(curve_lut(
            &stack.tone_curve().map(|t| t.points).unwrap_or_default(),
        )));
        let tone_curve_node = CurveNode::new(ctx.clone(), tone_curve.clone());
        let tone_curve_id = graph.add_node(Box::new(tone_curve_node), vec![contrast_id]);
```
5. Update the struct literal returned from `new`: set `output_id: tone_curve_id,` (was `contrast_id`), add `tone_curve_id,` and `tone_curve,`, and add `node_count: 5,` (source + exposure + wb + contrast + tone_curve = 5; later tasks bump this to 6, 7, 8).
6. In `set_stack`, after the contrast block, add:
```rust
        let lut = curve_lut(&stack.tone_curve().map(|t| t.points).unwrap_or_default());
        if lut != self.tone_curve.get() {
            self.tone_curve.set(lut);
            self.graph.mark_dirty(self.tone_curve_id);
        }
```
7. Add a `node_count` accessor in `impl EditPipeline` (next to `eval_count`):
```rust
    /// Total nodes in the graph (source + one per op). Used by invalidation tests.
    pub fn node_count(&self) -> usize {
        self.node_count
    }
```

`curve_lut` stays crate-internal (like the other param→uniform helpers, per the `lib.rs` comment); no `lib.rs` change is needed for the node itself. The public `ToneCurve` op type is exported in Step 10 below.

- [ ] **Step 8: Add the tone-curve golden test**

First export `ToneCurve` so the test compiles: in `ferrolite-pipeline/src/lib.rs`, add `ToneCurve` to the `pub use op::{...}` list (Task 6 reconciles the full list). Then, in `ferrolite-pipeline/tests/golden.rs`, extend the `use ferrolite_pipeline::{...}` import to add `ToneCurve`, and append:

```rust
#[test]
fn tone_curve_darken_midtones_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::ToneCurve(ToneCurve {
        points: vec![(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)],
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "tone_curve.png");
}
```

- [ ] **Step 9: Run the full pipeline test suite**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS. On a GPU host, `tone_curve.png` is auto-authored on first run (and `identity_stack_matches_source_render` + `full_stack_matches_golden` still pass — the identity LUT is an exact ramp). On a headless host, GPU tests skip. The existing `editing_one_op_reevaluates_minimally` test still asserts the **old** counts (4 nodes, contrast as terminal) and will now FAIL on a GPU host — replace it in the next step.

- [ ] **Step 10: Replace `editing_one_op_reevaluates_minimally` with a chain-agnostic version**

The old test hard-codes 4 nodes and assumes contrast is terminal. Replace it **once** with a version that depends only on (a) `node_count()` and (b) the always-present root op (exposure) — so it needs **no further edits** as Tasks 3–5 deepen the chain. It exercises the executor's three guarantees: first-eval runs every node once, a no-op re-evaluate re-runs nothing (caching), and dirtying the root re-runs the root + all downstream while the source stays cached. (Per-op downstream isolation is already covered by `ferrolite-gpu`'s executor unit tests.) Replace the whole `editing_one_op_reevaluates_minimally` function in `tests/golden.rs` with:

```rust
#[test]
fn editing_one_op_reevaluates_minimally() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let base = OpStack::default().set_op(Op::Exposure(Exposure { ev: 0.2 }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), base.clone());

    // First evaluate runs every node exactly once (source + one per op).
    let _ = pipe.evaluate();
    assert_eq!(pipe.eval_count(), pipe.node_count());

    // Re-evaluating with no change re-runs nothing (all cached).
    let after_first = pipe.eval_count();
    pipe.set_stack(base.clone());
    let _ = pipe.evaluate();
    assert_eq!(after_first, pipe.eval_count(), "no node re-ran when nothing changed");

    // Dirtying the root op (exposure) re-runs it + every downstream op; the
    // source node stays cached -> exactly node_count - 1 re-evaluations.
    let prev = pipe.eval_count();
    pipe.set_stack(OpStack::default().set_op(Op::Exposure(Exposure { ev: 1.5 })));
    let _ = pipe.evaluate();
    assert_eq!(
        pipe.eval_count(),
        prev + (pipe.node_count() - 1),
        "exposure + every downstream op re-evaluated (source stays cached)"
    );
}
```

This form is final: Tasks 3, 4, and 5 only bump `node_count` inside `EditPipeline::new`, which this test reads dynamically — it never needs editing again.

- [ ] **Step 11: Run the suite again to confirm green**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS on both GPU and headless hosts.

- [ ] **Step 12: Commit**

```bash
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/nodes.rs \
  ferrolite-pipeline/src/pipeline.rs ferrolite-pipeline/src/lib.rs \
  ferrolite-pipeline/src/shaders/tone_curve.wgsl ferrolite-pipeline/tests/golden.rs
git commit -m "feat(pipeline): tone-curve LUT compute pass"
```

---

## Task 3: HSL — `hsl_uniform` unit + WGSL pass (reuses `PointOpNode`) + golden

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs`
- Modify: `ferrolite-pipeline/src/pipeline.rs`
- Modify: `ferrolite-pipeline/src/lib.rs`
- Create: `ferrolite-pipeline/src/shaders/hsl.wgsl`
- Modify: `ferrolite-pipeline/tests/golden.rs`

**Interfaces:**
- Consumes: `OpStack::hsl() -> Option<Hsl>`; `HslBand`; the generic `PointOpNode<U>` from Task-1-era `nodes.rs`.
- Produces:
  - `#[repr(C)] pub struct HslUniform { pub bands: [[f32; 4]; 8] }` (Pod; mirrors WGSL `array<vec4<f32>, 8>`; layout 128 bytes).
  - `pub fn hsl_uniform(op: Option<Hsl>) -> HslUniform` (all-zero = identity).
  - `EditPipeline` gains an HSL node after tone curve; output re-pointed.

- [ ] **Step 1: Write the failing `hsl_uniform` tests**

Append to `#[cfg(test)] mod tests` in `ferrolite-pipeline/src/uniforms.rs` (extend its `use super::*;` is already present; add `use crate::op::{Hsl, HslBand};` inside the test module if not pulled by `super`):

```rust
    #[test]
    fn hsl_uniform_identity_is_all_zero() {
        let u = hsl_uniform(None);
        assert_eq!(u.bands, [[0.0; 4]; 8]);
    }

    #[test]
    fn hsl_uniform_packs_bands_in_order() {
        use crate::op::{Hsl, HslBand};
        let mut bands = [HslBand { hue: 0.0, sat: 0.0, lum: 0.0 }; 8];
        bands[3] = HslBand { hue: 0.2, sat: -0.3, lum: 0.1 };
        let u = hsl_uniform(Some(Hsl { bands }));
        assert_eq!(u.bands[3], [0.2, -0.3, 0.1, 0.0]);
        assert_eq!(u.bands[0], [0.0, 0.0, 0.0, 0.0]);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests::hsl`
Expected: FAIL — `cannot find function hsl_uniform` / `HslUniform`.

- [ ] **Step 3: Implement `HslUniform` + `hsl_uniform`**

In `ferrolite-pipeline/src/uniforms.rs`, extend the top `use` to include `Hsl`:

```rust
use crate::op::{Contrast, Exposure, Hsl, WhiteBalance};
```

Add the struct (with the other `#[repr(C)]` uniforms) and the constructor (with the other `*_uniform` fns):

```rust
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct HslUniform {
    /// 8 bands × (hue, sat, lum, pad). Mirrors WGSL `array<vec4<f32>, 8>`.
    pub bands: [[f32; 4]; 8],
}

pub fn hsl_uniform(op: Option<Hsl>) -> HslUniform {
    let mut bands = [[0.0f32; 4]; 8];
    if let Some(h) = op {
        for (i, b) in h.bands.iter().enumerate() {
            bands[i] = [b.hue, b.sat, b.lum, 0.0];
        }
    }
    HslUniform { bands }
}
```

- [ ] **Step 4: Run to verify the unit passes**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests::hsl`
Expected: PASS.

- [ ] **Step 5: Write the HSL WGSL pass**

Create `ferrolite-pipeline/src/shaders/hsl.wgsl`:

```wgsl
// HSL: 8-band hue/sat/lum adjustment. Point op (reuses the point-op bind layout:
// 0 = src texture, 1 = dst storage texture, 2 = uniform). Display-linear input is
// clamped to [0,1] for the HSL round-trip (a documented Spec-3 placeholder).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P { bands: array<vec4<f32>, 8> };
@group(0) @binding(2) var<uniform> p: P;

const MAX_HUE_SHIFT: f32 = 30.0; // degrees per unit band.hue

fn band_center(i: u32) -> f32 {
    // red, orange, yellow, green, aqua, blue, purple, magenta
    var centers = array<f32, 8>(0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0);
    return centers[i];
}

fn rgb2hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    let l = (mx + mn) * 0.5;
    var h = 0.0;
    var s = 0.0;
    let d = mx - mn;
    if (d > 1e-6) {
        s = d / (1.0 - abs(2.0 * l - 1.0));
        if (mx == c.r) {
            h = ((c.g - c.b) / d) % 6.0;
        } else if (mx == c.g) {
            h = (c.b - c.r) / d + 2.0;
        } else {
            h = (c.r - c.g) / d + 4.0;
        }
        h = h * 60.0;
        if (h < 0.0) { h = h + 360.0; }
    }
    return vec3<f32>(h, s, l);
}

fn hue2rgb(pp: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) { t = t + 1.0; }
    if (t > 1.0) { t = t - 1.0; }
    if (t < 1.0 / 6.0) { return pp + (q - pp) * 6.0 * t; }
    if (t < 1.0 / 2.0) { return q; }
    if (t < 2.0 / 3.0) { return pp + (q - pp) * (2.0 / 3.0 - t) * 6.0; }
    return pp;
}

fn hsl2rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x / 360.0;
    let s = hsl.y;
    let l = hsl.z;
    if (s <= 1e-6) { return vec3<f32>(l, l, l); }
    var q = l + s - l * s;
    if (l < 0.5) { q = l * (1.0 + s); }
    let pp = 2.0 * l - q;
    return vec3<f32>(
        hue2rgb(pp, q, h + 1.0 / 3.0),
        hue2rgb(pp, q, h),
        hue2rgb(pp, q, h - 1.0 / 3.0),
    );
}

fn band_weight(hue: f32, center: f32) -> f32 {
    var d = abs(hue - center);
    if (d > 180.0) { d = 360.0 - d; }
    return max(0.0, 1.0 - d / 60.0);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    let hsl = rgb2hsl(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));

    var hue_acc = 0.0;
    var sat_acc = 0.0;
    var lum_acc = 0.0;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let w = band_weight(hsl.x, band_center(i));
        hue_acc = hue_acc + w * p.bands[i].x;
        sat_acc = sat_acc + w * p.bands[i].y;
        lum_acc = lum_acc + w * p.bands[i].z;
    }

    var out_hsl = hsl;
    out_hsl.x = hsl.x + hue_acc * MAX_HUE_SHIFT;
    if (out_hsl.x < 0.0) { out_hsl.x = out_hsl.x + 360.0; }
    if (out_hsl.x >= 360.0) { out_hsl.x = out_hsl.x - 360.0; }
    out_hsl.y = clamp(hsl.y * (1.0 + sat_acc), 0.0, 1.0);
    out_hsl.z = clamp(hsl.z * (1.0 + lum_acc), 0.0, 1.0);

    let rgb = hsl2rgb(out_hsl);
    textureStore(dst, xy, vec4<f32>(max(rgb, vec3<f32>(0.0)), c.a));
}
```

- [ ] **Step 6: Chain the HSL node into `EditPipeline`**

In `ferrolite-pipeline/src/pipeline.rs`:

1. Extend the `use crate::uniforms` import to add `hsl_uniform` and `HslUniform`:
```rust
use crate::uniforms::{
    contrast_uniform, curve_lut, exposure_uniform, hsl_uniform, wb_uniform, ContrastUniform,
    ExposureUniform, HslUniform, WbUniform,
};
```
2. Add fields to `struct EditPipeline` (after `tone_curve`):
```rust
    hsl_id: NodeId,
    hsl: Rc<Cell<HslUniform>>,
```
3. In `new`, after the tone-curve node, add:
```rust
        let hsl = Rc::new(Cell::new(hsl_uniform(stack.hsl())));
        let hsl_node = PointOpNode::new(ctx.clone(), include_str!("shaders/hsl.wgsl"), "hsl", hsl.clone());
        let hsl_id = graph.add_node(Box::new(hsl_node), vec![tone_curve_id]);
```
4. In the struct literal: change `output_id: tone_curve_id` → `output_id: hsl_id`, add `hsl_id,` and `hsl,`, and bump `node_count: 5` → `node_count: 6`.
5. In `set_stack`, after the tone-curve block:
```rust
        let h = hsl_uniform(stack.hsl());
        if h != self.hsl.get() {
            self.hsl.set(h);
            self.graph.mark_dirty(self.hsl_id);
        }
```

- [ ] **Step 7: Export `Hsl`/`HslBand` and add the HSL golden test**

In `ferrolite-pipeline/src/lib.rs`, add `Hsl, HslBand` to the `pub use op::{...}` list (Task 6 reconciles the full list). Then, in `ferrolite-pipeline/tests/golden.rs`, add `Hsl, HslBand` to the `use ferrolite_pipeline::{...}` import and append:

```rust
#[test]
fn hsl_shift_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // Boost saturation + nudge hue across all bands.
    let stack = OpStack::default().set_op(Op::Hsl(Hsl {
        bands: [HslBand { hue: 0.2, sat: 0.4, lum: 0.0 }; 8],
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "hsl.png");
}
```

- [ ] **Step 8: Run the suite**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS on GPU and headless hosts. `hsl.png` is auto-authored on a GPU host; `identity_stack_matches_source_render` stays ≤ 4 (all-zero bands → HSL round-trip is within tolerance); `editing_one_op_reevaluates_minimally` stays green unchanged — it reads `node_count()` (now 6) dynamically and dirties only the always-present root op, so it needs no edit.

- [ ] **Step 9: Commit**

```bash
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/pipeline.rs \
  ferrolite-pipeline/src/lib.rs ferrolite-pipeline/src/shaders/hsl.wgsl \
  ferrolite-pipeline/tests/golden.rs
git commit -m "feat(pipeline): 8-band HSL compute pass"
```

---

## Task 4: Sharpening — `sharpen_uniform`/`sharpen_halo` units + WGSL unsharp pass (reuses `PointOpNode`) + golden

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs`
- Modify: `ferrolite-pipeline/src/pipeline.rs`
- Modify: `ferrolite-pipeline/src/lib.rs`
- Create: `ferrolite-pipeline/src/shaders/sharpen.wgsl`
- Modify: `ferrolite-pipeline/tests/golden.rs`

**Interfaces:**
- Consumes: `OpStack::sharpen() -> Option<Sharpen>`.
- Produces:
  - `#[repr(C)] pub struct SharpenUniform { pub amount: f32, pub radius: i32, pub pad: [f32; 2] }` (Pod; 16 bytes; mirrors WGSL `struct P { amount: f32, radius: i32, pad0: f32, pad1: f32 }`).
  - `pub fn sharpen_uniform(op: Option<Sharpen>) -> SharpenUniform` (amount 0 = identity).
  - `pub fn sharpen_halo(op: Option<Sharpen>) -> u32` (the halo width Plan 3's tile producer needs; = `radius`, or 0 when absent/amount 0).
  - `EditPipeline` gains a sharpen node after HSL.

- [ ] **Step 1: Write the failing `sharpen_uniform`/`sharpen_halo` tests**

Append to `#[cfg(test)] mod tests` in `ferrolite-pipeline/src/uniforms.rs`:

```rust
    #[test]
    fn sharpen_uniform_identity_when_absent() {
        let u = sharpen_uniform(None);
        assert_eq!(u.amount, 0.0);
        assert_eq!(u.radius, 0);
    }

    #[test]
    fn sharpen_uniform_carries_amount_and_radius() {
        use crate::op::Sharpen;
        let u = sharpen_uniform(Some(Sharpen { amount: 0.75, radius: 3 }));
        assert_eq!(u.amount, 0.75);
        assert_eq!(u.radius, 3);
    }

    #[test]
    fn sharpen_halo_is_radius_or_zero() {
        use crate::op::Sharpen;
        assert_eq!(sharpen_halo(None), 0);
        // amount 0 contributes no halo even with radius set.
        assert_eq!(sharpen_halo(Some(Sharpen { amount: 0.0, radius: 4 })), 0);
        assert_eq!(sharpen_halo(Some(Sharpen { amount: 0.5, radius: 4 })), 4);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests::sharpen`
Expected: FAIL — `cannot find function sharpen_uniform`.

- [ ] **Step 3: Implement `SharpenUniform`, `sharpen_uniform`, `sharpen_halo`**

In `ferrolite-pipeline/src/uniforms.rs`, extend the top `use` to add `Sharpen`:

```rust
use crate::op::{Contrast, Exposure, Hsl, Sharpen, WhiteBalance};
```

Add the struct and functions:

```rust
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SharpenUniform {
    pub amount: f32,
    pub radius: i32,
    pub pad: [f32; 2],
}

pub fn sharpen_uniform(op: Option<Sharpen>) -> SharpenUniform {
    let (amount, radius) = op.map(|s| (s.amount, s.radius)).unwrap_or((0.0, 0));
    SharpenUniform {
        amount,
        radius: radius as i32,
        pad: [0.0; 2],
    }
}

/// Halo (pixels) a tiled full-res sharpen pass must over-fetch. Zero when the
/// op is absent or a no-op (amount 0). Consumed by Plan 3's tile producer.
pub fn sharpen_halo(op: Option<Sharpen>) -> u32 {
    match op {
        Some(s) if s.amount != 0.0 => s.radius,
        _ => 0,
    }
}
```

- [ ] **Step 4: Run to verify the units pass**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests::sharpen`
Expected: PASS.

- [ ] **Step 5: Write the sharpen WGSL pass**

Create `ferrolite-pipeline/src/shaders/sharpen.wgsl`:

```wgsl
// Unsharp mask: out = src + amount * (src - boxblur(src, radius)). The
// neighborhood op. Reuses the point-op bind layout (0 = src, 1 = dst, 2 = uniform).
// At preview-res a single-pass box blur is enough; Plan 3 adds the tiled halo.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P { amount: f32, radius: i32, pad0: f32, pad1: f32 };
@group(0) @binding(2) var<uniform> p: P;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(src));
    if (i32(gid.x) >= dims.x || i32(gid.y) >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);

    if (p.amount == 0.0 || p.radius <= 0) {
        textureStore(dst, xy, c);
        return;
    }

    var sum = vec3<f32>(0.0);
    var n = 0.0;
    for (var dy = -p.radius; dy <= p.radius; dy = dy + 1) {
        for (var dx = -p.radius; dx <= p.radius; dx = dx + 1) {
            let q = clamp(xy + vec2<i32>(dx, dy), vec2<i32>(0, 0), dims - vec2<i32>(1, 1));
            sum = sum + textureLoad(src, q, 0).rgb;
            n = n + 1.0;
        }
    }
    let blur = sum / n;
    let sharp = c.rgb + p.amount * (c.rgb - blur);
    textureStore(dst, xy, vec4<f32>(max(sharp, vec3<f32>(0.0)), c.a));
}
```

- [ ] **Step 6: Chain the sharpen node into `EditPipeline`**

In `ferrolite-pipeline/src/pipeline.rs`:

1. Extend the `use crate::uniforms` import to add `sharpen_uniform` and `SharpenUniform`.
2. Add fields to `struct EditPipeline` (after `hsl`):
```rust
    sharpen_id: NodeId,
    sharpen: Rc<Cell<SharpenUniform>>,
```
3. In `new`, after the HSL node:
```rust
        let sharpen = Rc::new(Cell::new(sharpen_uniform(stack.sharpen())));
        let sharpen_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/sharpen.wgsl"),
            "sharpen",
            sharpen.clone(),
        );
        let sharpen_id = graph.add_node(Box::new(sharpen_node), vec![hsl_id]);
```
4. Struct literal: `output_id: sharpen_id`, add `sharpen_id,` + `sharpen,`, bump `node_count: 6` → `node_count: 7`.
5. In `set_stack`, after the HSL block:
```rust
        let sh = sharpen_uniform(stack.sharpen());
        if sh != self.sharpen.get() {
            self.sharpen.set(sh);
            self.graph.mark_dirty(self.sharpen_id);
        }
```

- [ ] **Step 7: Export `Sharpen` and add the sharpen golden test**

In `ferrolite-pipeline/src/lib.rs`, add `Sharpen` to the `pub use op::{...}` list (Task 6 reconciles the full list). Then, in `ferrolite-pipeline/tests/golden.rs`, add `Sharpen` to the `use ferrolite_pipeline::{...}` import and append:

```rust
#[test]
fn sharpen_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::Sharpen(Sharpen { amount: 0.8, radius: 2 }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
    let pixels = pipe.render_to_image();
    common::assert_golden(&pixels, W, H, "sharpen.png");
}
```

- [ ] **Step 8: Run the suite**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS on GPU and headless hosts. `sharpen.png` is auto-authored on a GPU host; `identity_stack_matches_source_render` stays ≤ 4 (amount 0 → exact passthrough); `editing_one_op_reevaluates_minimally` stays green unchanged (reads `node_count()`, now 7).

- [ ] **Step 9: Commit**

```bash
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/pipeline.rs \
  ferrolite-pipeline/src/lib.rs ferrolite-pipeline/src/shaders/sharpen.wgsl \
  ferrolite-pipeline/tests/golden.rs
git commit -m "feat(pipeline): unsharp-mask sharpening compute pass"
```

---

## Task 5: Geometry — `geometry_uniform` unit + `GeometryNode` (sampler, variable out dims) + WGSL + golden

**Files:**
- Modify: `ferrolite-pipeline/src/uniforms.rs`
- Modify: `ferrolite-pipeline/src/nodes.rs`
- Modify: `ferrolite-pipeline/src/pipeline.rs`
- Modify: `ferrolite-pipeline/src/lib.rs`
- Create: `ferrolite-pipeline/src/shaders/geometry.wgsl`
- Modify: `ferrolite-pipeline/tests/golden.rs`

**Interfaces:**
- Consumes: `OpStack::geometry() -> Option<Geometry>`; `Geometry`, `CropRect`, `Aspect`.
- Produces:
  - `#[repr(C)] pub struct GeometryUniform { pub m: [f32; 4], pub off: [f32; 2], pub src_dims: [f32; 2], pub out_dims: [f32; 2], pub pad: [f32; 2] }` (Pod; 48 bytes; mirrors WGSL `struct P { m: vec4<f32>, off: vec2<f32>, src_dims: vec2<f32>, out_dims: vec2<f32>, pad: vec2<f32> }`).
  - `pub fn geometry_uniform(op: Option<Geometry>, src_w: u32, src_h: u32) -> (GeometryUniform, u32, u32)` — returns the uniform plus output (width, height). Identity (absent / full crop, angle 0) ⇒ `m = [1,0,0,1]`, `off = [0,0]`, out dims = src dims.
  - `pub(crate) struct GeometryNode` implementing `Node<PipelineImage>`, constructed `GeometryNode::new(ctx: Arc<GpuContext>, params: Rc<Cell<GeometryUniform>>) -> GeometryNode`; output texture dims come from `params`'s `out_dims`.
  - `EditPipeline` gains the geometry node as the final output; `EditPipeline` stores `src_w`/`src_h` so `set_stack` can recompute the geometry uniform.

- [ ] **Step 1: Write the failing `geometry_uniform` tests**

Append to `#[cfg(test)] mod tests` in `ferrolite-pipeline/src/uniforms.rs`:

```rust
    #[test]
    fn geometry_uniform_identity_when_absent() {
        let (u, w, h) = geometry_uniform(None, 64, 48);
        assert_eq!((w, h), (64, 48));
        assert_eq!(u.m, [1.0, 0.0, 0.0, 1.0]);
        assert!(u.off[0].abs() < 1e-4 && u.off[1].abs() < 1e-4);
        assert_eq!(u.src_dims, [64.0, 48.0]);
        assert_eq!(u.out_dims, [64.0, 48.0]);
    }

    #[test]
    fn geometry_uniform_crop_halves_output_dims() {
        use crate::op::{Aspect, CropRect, Geometry};
        let (_, w, h) = geometry_uniform(
            Some(Geometry {
                crop: CropRect { x: 0.25, y: 0.25, w: 0.5, h: 0.5 },
                angle_deg: 0.0,
                aspect: Aspect::Free,
            }),
            64,
            48,
        );
        assert_eq!((w, h), (32, 24));
    }

    #[test]
    fn geometry_uniform_rotation_sets_rotation_matrix() {
        use crate::op::{Aspect, CropRect, Geometry};
        let (u, _, _) = geometry_uniform(
            Some(Geometry {
                crop: CropRect::full(),
                angle_deg: 90.0,
                aspect: Aspect::Original,
            }),
            64,
            48,
        );
        // 90°: cos=0, sin=1 -> m = [0,-1,1,0] (row-major).
        assert!(u.m[0].abs() < 1e-5);
        assert!((u.m[1] - -1.0).abs() < 1e-5);
        assert!((u.m[2] - 1.0).abs() < 1e-5);
        assert!(u.m[3].abs() < 1e-5);
    }
```

(`CropRect::full()` was added in Task 1.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests::geometry`
Expected: FAIL — `cannot find function geometry_uniform`.

- [ ] **Step 3: Implement `GeometryUniform` + `geometry_uniform`**

In `ferrolite-pipeline/src/uniforms.rs`, extend the top `use` to add `Geometry`:

```rust
use crate::op::{Contrast, Exposure, Geometry, Hsl, Sharpen, WhiteBalance};
```

Add:

```rust
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GeometryUniform {
    /// Row-major 2×2 mapping output-pixel → source-pixel: [m00, m01, m10, m11].
    pub m: [f32; 4],
    /// Source-pixel translation: src = m·out + off.
    pub off: [f32; 2],
    pub src_dims: [f32; 2],
    pub out_dims: [f32; 2],
    pub pad: [f32; 2],
}

/// Crop + rotate as a sampling transform. Returns the uniform plus the output
/// (width, height) in pixels. Maps each output pixel center to a source pixel:
/// `src = R(angle)·(out − out_center) + crop_center`, sampled bilinearly.
pub fn geometry_uniform(op: Option<Geometry>, src_w: u32, src_h: u32) -> (GeometryUniform, u32, u32) {
    let sw = src_w as f32;
    let sh = src_h as f32;
    let geo = op.unwrap_or(Geometry {
        crop: CropRect::full(),
        angle_deg: 0.0,
        aspect: Aspect::Original,
    });

    let cx = geo.crop.x.clamp(0.0, 1.0);
    let cy = geo.crop.y.clamp(0.0, 1.0);
    let cw = geo.crop.w.clamp(1e-4, (1.0 - cx).max(1e-4));
    let ch = geo.crop.h.clamp(1e-4, (1.0 - cy).max(1e-4));

    let crop_w_px = cw * sw;
    let crop_h_px = ch * sh;
    let out_w = (crop_w_px.round() as u32).max(1);
    let out_h = (crop_h_px.round() as u32).max(1);

    let theta = geo.angle_deg.to_radians();
    let (s, c) = theta.sin_cos();
    let m = [c, -s, s, c];

    let out_center = [out_w as f32 * 0.5, out_h as f32 * 0.5];
    let crop_center = [cx * sw + crop_w_px * 0.5, cy * sh + crop_h_px * 0.5];
    let off = [
        crop_center[0] - (m[0] * out_center[0] + m[1] * out_center[1]),
        crop_center[1] - (m[2] * out_center[0] + m[3] * out_center[1]),
    ];

    (
        GeometryUniform {
            m,
            off,
            src_dims: [sw, sh],
            out_dims: [out_w as f32, out_h as f32],
            pad: [0.0; 2],
        },
        out_w,
        out_h,
    )
}
```

Add `CropRect`, `Aspect`, `Geometry` to the top `use` (already adding `Geometry`; also add `Aspect, CropRect`):

```rust
use crate::op::{Aspect, Contrast, CropRect, Exposure, Geometry, Hsl, Sharpen, WhiteBalance};
```

- [ ] **Step 4: Run to verify the units pass**

Run: `cargo test -p ferrolite-pipeline --lib uniforms::tests::geometry`
Expected: PASS.

- [ ] **Step 5: Write the geometry WGSL pass**

Create `ferrolite-pipeline/src/shaders/geometry.wgsl`:

```wgsl
// Geometry: crop + rotate as a bilinear sampling transform. Output dims differ
// from input dims, so this is NOT a point op — it has its own bind layout
// (0 = src texture, 1 = dst storage, 2 = uniform, 3 = sampler). Uses
// textureSampleLevel (compute has no implicit derivatives).
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct P {
    m: vec4<f32>,         // row-major 2x2: m00,m01,m10,m11
    off: vec2<f32>,
    src_dims: vec2<f32>,
    out_dims: vec2<f32>,
    pad: vec2<f32>,
};
@group(0) @binding(2) var<uniform> p: P;
@group(0) @binding(3) var samp: sampler;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ow = u32(p.out_dims.x);
    let oh = u32(p.out_dims.y);
    if (gid.x >= ow || gid.y >= oh) { return; }
    let po = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5);
    let sx = p.m.x * po.x + p.m.y * po.y + p.off.x;
    let sy = p.m.z * po.x + p.m.w * po.y + p.off.y;
    let uv = vec2<f32>(sx, sy) / p.src_dims;
    let c = textureSampleLevel(src, samp, uv, 0.0);
    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), c);
}
```

- [ ] **Step 6: Implement `GeometryNode`**

Add to `ferrolite-pipeline/src/nodes.rs`:

```rust
/// Bind-group layout for the geometry pass: 0 = input texture (filterable),
/// 1 = output storage texture, 2 = transform uniform, 3 = filtering sampler.
fn geometry_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("geometry-bgl"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// Geometry compute pass (crop + rotate). Output texture dims come from the
/// uniform's `out_dims`, so it reallocates when the crop changes.
pub(crate) struct GeometryNode {
    ctx: Arc<GpuContext>,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    uniform_buf: wgpu::Buffer,
    sampler: wgpu::Sampler,
    params: Rc<Cell<crate::uniforms::GeometryUniform>>,
    out: RefCell<Option<PipelineImage>>,
}

impl GeometryNode {
    pub(crate) fn new(
        ctx: Arc<GpuContext>,
        params: Rc<Cell<crate::uniforms::GeometryUniform>>,
    ) -> Self {
        let bgl = geometry_bgl(&ctx.device);
        let module = ctx.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("geometry"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/geometry.wgsl").into()),
        });
        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("geometry"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            });
        let pipeline = ctx
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("geometry"),
                layout: Some(&layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            });
        let uniform_buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("geometry-uniform"),
            size: std::mem::size_of::<crate::uniforms::GeometryUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = ctx.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("geometry-samp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            ctx,
            pipeline,
            bgl,
            uniform_buf,
            sampler,
            params,
            out: RefCell::new(None),
        }
    }

    fn ensure_out(&self, w: u32, h: u32) -> PipelineImage {
        let mut out = self.out.borrow_mut();
        if out.as_ref().map(|o| (o.width, o.height)) != Some((w, h)) {
            let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("geometry-out"),
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

impl Node<PipelineImage> for GeometryNode {
    fn evaluate(&self, inputs: &[&PipelineImage]) -> PipelineImage {
        let src = inputs[0];
        let u = self.params.get();
        let out_w = (u.out_dims[0] as u32).max(1);
        let out_h = (u.out_dims[1] as u32).max(1);
        let dst = self.ensure_out(out_w, out_h);

        self.ctx
            .queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));

        let src_view = src
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("geometry-bind"),
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
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("geometry-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(out_w.div_ceil(8), out_h.div_ceil(8), 1);
        }
        self.ctx.queue.submit([enc.finish()]);
        dst
    }
}
```

- [ ] **Step 7: Chain the geometry node into `EditPipeline` as the final output**

In `ferrolite-pipeline/src/pipeline.rs`:

1. Extend `use crate::nodes` to add `GeometryNode`:
```rust
use crate::nodes::{CurveNode, GeometryNode, PointOpNode, SourceNode};
```
2. Extend `use crate::uniforms` to add `geometry_uniform` and `GeometryUniform`.
3. Add fields to `struct EditPipeline` (after `sharpen`):
```rust
    geometry_id: NodeId,
    geometry: Rc<Cell<GeometryUniform>>,
    src_w: u32,
    src_h: u32,
```
4. In `new`, capture source dims at the top (right after `let mut graph = Graph::new();`):
```rust
        let (src_w, src_h) = (source.width, source.height);
```
   Then after the sharpen node:
```rust
        let (geo_uniform, _, _) = geometry_uniform(stack.geometry(), src_w, src_h);
        let geometry = Rc::new(Cell::new(geo_uniform));
        let geometry_node = GeometryNode::new(ctx.clone(), geometry.clone());
        let geometry_id = graph.add_node(Box::new(geometry_node), vec![sharpen_id]);
```
5. Struct literal: `output_id: geometry_id`, add `geometry_id,`, `geometry,`, `src_w,`, `src_h,`, bump `node_count: 7` → `node_count: 8`.
6. In `set_stack`, after the sharpen block:
```rust
        let (geo_uniform, _, _) = geometry_uniform(stack.geometry(), self.src_w, self.src_h);
        if geo_uniform != self.geometry.get() {
            self.geometry.set(geo_uniform);
            self.graph.mark_dirty(self.geometry_id);
        }
```

- [ ] **Step 8: Export the geometry op types and add the geometry golden test**

In `ferrolite-pipeline/src/lib.rs`, add `Aspect, CropRect, Geometry` to the `pub use op::{...}` list (Task 6 reconciles the full list). Then, in `ferrolite-pipeline/tests/golden.rs`, add `Aspect, CropRect, Geometry` to the `use ferrolite_pipeline::{...}` import and append a golden that asserts the **cropped output dims**:

```rust
#[test]
fn geometry_crop_rotate_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default().set_op(Op::Geometry(Geometry {
        crop: CropRect { x: 0.1, y: 0.1, w: 0.8, h: 0.8 },
        angle_deg: 10.0,
        aspect: Aspect::Free,
    }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
    let pixels = pipe.render_to_image();
    // out dims = round(0.8 * 64) x round(0.8 * 48) = 51 x 38.
    common::assert_golden(&pixels, 51, 38, "geometry_crop_rotate.png");
}
```

- [ ] **Step 9: Run the suite**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS on GPU and headless hosts. `geometry_crop_rotate.png` is auto-authored on a GPU host at 51×38; `identity_stack_matches_source_render` stays ≤ 4 (full crop + angle 0 → bilinear at exact texel centers = identity); `editing_one_op_reevaluates_minimally` stays green unchanged — its `base` has only an exposure op, geometry is identity (full crop, 0°) so readback stays `W × H`, and it reads `node_count()` (now 8) dynamically.

- [ ] **Step 10: Commit**

```bash
git add ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/nodes.rs \
  ferrolite-pipeline/src/pipeline.rs ferrolite-pipeline/src/lib.rs \
  ferrolite-pipeline/src/shaders/geometry.wgsl ferrolite-pipeline/tests/golden.rs
git commit -m "feat(pipeline): crop/rotate geometry sampling pass"
```

---

## Task 6: Public API surface + full-stack golden + workspace gate

**Files:**
- Modify: `ferrolite-pipeline/src/lib.rs`
- Modify: `ferrolite-pipeline/tests/golden.rs`

**Interfaces:**
- Consumes: everything from Tasks 1–5.
- Produces: the finalized `ferrolite-pipeline` public surface and a full-stack golden exercising all seven ops.

> **Note:** Tasks 2–5 each added their op type to the `lib.rs` `pub use op::{...}` list at the point the golden test needed it. This task **reconciles** that list to exactly the intended public surface (in case it drifted) and adds the uniform-layout exports, then locks a 7-op full-stack golden.

- [ ] **Step 1: Set the final `lib.rs` exports**

Replace the `pub use` block in `ferrolite-pipeline/src/lib.rs` with:

```rust
pub use image::PipelineImage;
pub use nodes::upload_source;
pub use op::{
    Aspect, Contrast, CropRect, Exposure, Geometry, Hsl, HslBand, Op, OpKind, OpStack, Sharpen,
    ToneCurve, WhiteBalance, STACK_VERSION,
};
pub use pipeline::{blit_to_rgba8, EditPipeline};
pub use serialize::{deserialize, serialize};
// The uniform structs are exported as the documented GPU memory layout the
// edit passes consume; the param→uniform helper fns + math are crate-internal.
pub use uniforms::{
    ContrastUniform, ExposureUniform, GeometryUniform, HslUniform, SharpenUniform, WbUniform,
};
```

- [ ] **Step 2: Add a full-seven-op golden test**

Append to `ferrolite-pipeline/tests/golden.rs`:

```rust
#[test]
fn full_seven_op_stack_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let stack = OpStack::default()
        .set_op(Op::Exposure(Exposure { ev: 0.3 }))
        .set_op(Op::WhiteBalance(WhiteBalance { temp: 0.2, tint: 0.0 }))
        .set_op(Op::Contrast(Contrast { amount: 0.3 }))
        .set_op(Op::ToneCurve(ToneCurve {
            points: vec![(0.0, 0.0), (0.5, 0.4), (1.0, 1.0)],
        }))
        .set_op(Op::Hsl(Hsl {
            bands: [HslBand { hue: 0.0, sat: 0.2, lum: 0.0 }; 8],
        }))
        .set_op(Op::Sharpen(Sharpen { amount: 0.5, radius: 1 }))
        .set_op(Op::Geometry(Geometry {
            crop: CropRect { x: 0.05, y: 0.05, w: 0.9, h: 0.9 },
            angle_deg: 3.0,
            aspect: Aspect::Free,
        }));
    let mut pipe = EditPipeline::new(Arc::new(ctx), &common::gradient(W, H), stack);
    let pixels = pipe.render_to_image();
    // out dims = round(0.9*64) x round(0.9*48) = 58 x 43.
    common::assert_golden(&pixels, 58, 43, "full_seven_op_stack.png");
}
```

- [ ] **Step 3: Run the whole pipeline crate**

Run: `cargo test -p ferrolite-pipeline`
Expected: PASS (GPU host auto-authors `full_seven_op_stack.png`; headless skips GPU tests). All pure-unit tests pass on every host.

- [ ] **Step 4: Run the workspace gate**

Run each and confirm green:
```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all three succeed with no warnings. If `clippy` flags the new code, fix minimally (e.g. needless clones, `as` casts) without changing behavior.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-pipeline/src/lib.rs ferrolite-pipeline/tests/golden.rs
git commit -m "test(pipeline): full seven-op golden; finalize public API"
```

- [ ] **Step 6: STOP — hold for the author's visual test**

The automated gate being green is **necessary but not sufficient** (CLAUDE.md "Finishing a branch"). Do **not** merge/PR/finish. Report status to the author and hold:
- Summarize: four new ops added (tone curve, HSL, sharpen, geometry), all pure units + goldens green, workspace gate green.
- Note that golden fixtures (`tone_curve.png`, `hsl.png`, `sharpen.png`, `geometry_crop_rotate.png`, `full_seven_op_stack.png`) were authored on the dev GPU and should be eyeballed.
- Ask the author to run the app and visually confirm the edits look correct, then report back any issues to address before the branch is considered finished.

---

## Notes on TDD discipline for this plan

- **Real RED/GREEN lives in the pure units.** `curve_lut`, `hsl_uniform`, `sharpen_uniform`/`sharpen_halo`, and `geometry_uniform` are pure functions with deterministic tests — write the failing test, see it fail, implement, see it pass. These run on every host (the 80%+ coverage target, spec §10).
- **Golden GPU tests are regression locks, not RED drivers.** On a headless host they skip (green); on a GPU host they auto-author the fixture on first run (green) then lock it. They do not produce a meaningful RED. Their correctness is confirmed by the author's hands-on visual test, per CLAUDE.md. If a shader is wrong, the author catches it visually; delete the fixture and re-author after the fix.
- **Identity must stay a true no-op.** Each pass is designed so an absent/zero param renders the source unchanged within `TOL = 4`: the tone-curve LUT is interpolated (identity ramp is exact), sharpen early-returns at amount 0, geometry samples exact texel centers at full-crop/0°, and HSL's zero-delta round-trip is within tolerance. The existing `identity_stack_matches_source_render` guards this across the growing chain.
- **Executor stays frozen.** No `ferrolite-gpu` edits. `EditPipeline::node_count()` is tracked in the pipeline (a local counter), not added to `Graph` (Global Constraints; contract §4).

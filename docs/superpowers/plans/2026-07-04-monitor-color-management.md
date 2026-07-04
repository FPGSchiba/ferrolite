# Monitor Color Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the assumed-sRGB display tail with the real monitor profile, rendered via a generic 3D LUT baked from the monitor's ICC profile (auto-detected on Windows + macOS, manual picker everywhere), persisted, and falling back to sRGB when unavailable.

**Architecture:** `ferrolite-color` (photo tier) parses a monitor ICC via `moxcms` and bakes a `working→monitor` 3D LUT off-thread. `ferrolite-vt`'s on-screen display shader (`display.wgsl`) gains a dual tail: the existing analytic `working→sRGB` matrix path (unchanged, exact) OR a generic 3D-LUT texture path, selected by a `use_lut` uniform flag — no photo concepts enter the engine tier. `ferrolite-app` detects the profile of the monitor the window sits on, runs detect→parse→bake on `ferrolite-jobs`, delivers the baked LUT over the app event channel, and exposes an `Auto | sRGB | Custom(.icc)` control in Settings → General.

**Tech Stack:** Rust, `wgpu` 22, `eframe`/`egui` 0.29, `moxcms` 0.8 (ICC parse + profile→profile transform), `windows` crate (Win32 ICM), `objc2`/`core-graphics`/`core-foundation` (macOS ColorSync), `bytemuck`, `half`.

## Global Constraints

- **Licensing tiers (map §3.1):** `ferrolite-vt` / `ferrolite-gpu` / `ferrolite-image` stay **engine-transferable** — **no photo concepts, no copyleft deps**. The LUT reaches the engine tier only as **primitives** (`size: u32`, `rgba16f: &[u16]`, `shaper_gamma: f32`) — `ferrolite-vt` MUST NOT depend on `ferrolite-color`. All ICC/colorimetry lives in `ferrolite-color` / `ferrolite-app` (photo tier).
- **No C toolchain** is introduced → no Cargo feature gate needed. `moxcms` is pure Rust; `windows`/`objc2`/`core-graphics` are permissive and cfg-gated to their target OS.
- **Responsiveness (CLAUDE.md §1):** detect (OS call, UI thread, microseconds) is cheap; **file read + ICC parse + LUT bake run on `ferrolite-jobs`**, never the UI thread. Results arrive over the app event channel, then `ctx.request_repaint()`.
- **GPU build-once (CLAUDE.md §2):** the 3D-LUT texture + sampler are allocated **once** at a fixed size and reused; `set_display_lut` only `write_texture`s into the existing texture (stable view → per-image bind groups stay valid). Never rebuild pipelines per profile/image/frame.
- **Regression invariant (Spec 3 §4.3):** the `use_lut == 0` path must stay **byte-identical** to today's `linear_to_srgb(disp.m * lin)`. The existing sRGB golden must keep passing unchanged.
- **Never panics:** any failure (unsupported OS, no/undetectable/unparseable profile, missing custom file, bake error) → the sRGB analytic path (`use_lut = 0`), logged.
- **Shared constants:** `DISPLAY_LUT_SIZE = 33` and `DISPLAY_LUT_SHAPER_GAMMA = 2.2` appear in both `ferrolite-color` (bake) and `ferrolite-vt` (texture alloc / shader). They MUST match; each is documented as mirroring the other.
- **Gate then hold:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → then STOP and hold for the author's hands-on visual test on real Windows monitors before finishing the branch.
- **Branch:** `feat/monitor-color-management` (already created off `main`).
- **macOS caveat:** the macOS detection path cannot be visually verified on the Windows dev machine; the `Custom`-file picker is the cross-OS safety net.

---

## Plan phase 1 — `ferrolite-color`: monitor profile parse + 3D-LUT bake

No GPU, no app. Pure/testable on every OS.

### Task 1: Shaper constants + `shaper_decode`/`shaper_encode`

**Files:**
- Create: `ferrolite-color/src/display_lut.rs`
- Modify: `ferrolite-color/src/lib.rs` (add `mod display_lut;` + re-exports)

**Interfaces:**
- Produces: `pub const DISPLAY_LUT_SIZE: u32 = 33;`, `pub const DISPLAY_LUT_SHAPER_GAMMA: f32 = 2.2;`, `pub fn shaper_decode(x: f32) -> f32`, `pub fn shaper_encode(x: f32) -> f32`.

- [ ] **Step 1: Write the failing test**

In `ferrolite-color/src/display_lut.rs`:
```rust
//! Monitor-profile 3D-LUT bake: `working→monitor` baked from a parsed ICC via
//! moxcms, indexed through a gamma shaper. GPU-agnostic (data only).

/// Cube edge length of the display LUT (nodes per axis). Mirrored by
/// `ferrolite-vt`'s LUT texture allocation — the two MUST match.
pub const DISPLAY_LUT_SIZE: u32 = 33;

/// Gamma the LUT index grid is encoded with, concentrating nodes in the
/// shadows. Mirrored by `display.wgsl`'s `shaper_encode` — the two MUST match.
pub const DISPLAY_LUT_SHAPER_GAMMA: f32 = 2.2;

/// LUT index (`[0,1]`) → working-linear input fed to the transform.
pub fn shaper_decode(x: f32) -> f32 {
    x.clamp(0.0, 1.0).powf(DISPLAY_LUT_SHAPER_GAMMA)
}

/// Working-linear value → LUT sample coordinate (`[0,1]`). Inverse of `shaper_decode`.
pub fn shaper_encode(x: f32) -> f32 {
    x.clamp(0.0, 1.0).powf(1.0 / DISPLAY_LUT_SHAPER_GAMMA)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shaper_round_trips() {
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            assert!((shaper_encode(shaper_decode(x)) - x).abs() < 1e-5, "x={x}");
        }
    }

    #[test]
    fn shaper_endpoints_are_fixed() {
        assert!((shaper_decode(0.0)).abs() < 1e-6);
        assert!((shaper_decode(1.0) - 1.0).abs() < 1e-6);
    }
}
```

- [ ] **Step 2: Wire the module**

In `ferrolite-color/src/lib.rs`, add after `mod camera;`:
```rust
mod display_lut;
```
and add to the `pub use` block:
```rust
pub use display_lut::{
    bake_display_lut, shaper_decode, shaper_encode, DisplayLut, DisplayProfile,
    DISPLAY_LUT_SHAPER_GAMMA, DISPLAY_LUT_SIZE,
};
```
(The `bake_display_lut`, `DisplayLut`, `DisplayProfile` names resolve in Tasks 2–3; add them now so the module wiring is done once.)

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p ferrolite-color display_lut`
Expected: FAIL to compile — `DisplayLut`, `DisplayProfile`, `bake_display_lut` not yet defined (referenced in the `pub use`). Comment the three missing names out of the `pub use` temporarily, re-run, and confirm `shaper_round_trips` + `shaper_endpoints_are_fixed` PASS. Then restore the full `pub use`.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-color/src/display_lut.rs ferrolite-color/src/lib.rs
git commit -m "feat(color): display-LUT shaper constants + round-trip"
```

---

### Task 2: `DisplayProfile::parse` (moxcms ICC parse + name)

**Files:**
- Modify: `ferrolite-color/src/display_lut.rs`
- Test: same file (`#[cfg(test)]`)
- Add fixtures: `fixtures/icc/` (see Step 4)

**Interfaces:**
- Consumes: `moxcms::{ColorProfile, ProfileText}`, `crate::error::ColorError`.
- Produces: `pub struct DisplayProfile { pub(crate) profile: moxcms::ColorProfile, pub name: String }`, `impl DisplayProfile { pub fn parse(bytes: &[u8]) -> Result<DisplayProfile, ColorError> }`.

- [ ] **Step 1: Write the failing test**

Add to `display_lut.rs`:
```rust
use crate::error::ColorError;

/// A parsed monitor ICC profile (matrix/TRC or cLUT/A2B, uniformly) + a
/// human-readable name for the UI.
pub struct DisplayProfile {
    pub(crate) profile: moxcms::ColorProfile,
    pub name: String,
}

impl DisplayProfile {
    /// Parse monitor ICC bytes. `Err` on malformed input (caller falls back to sRGB).
    pub fn parse(bytes: &[u8]) -> Result<DisplayProfile, ColorError> {
        let profile = moxcms::ColorProfile::new_from_slice(bytes)
            .map_err(|e| ColorError::Icc(e.to_string()))?;
        let name = profile_name(&profile).unwrap_or_else(|| "Monitor profile".to_string());
        Ok(DisplayProfile { profile, name })
    }
}

fn profile_name(p: &moxcms::ColorProfile) -> Option<String> {
    use moxcms::ProfileText;
    let s = match p.description.as_ref()? {
        ProfileText::PlainString(s) => s.clone(),
        ProfileText::Localizable(v) => v.first().map(|l| l.value.clone())?,
        ProfileText::Description(d) => {
            if !d.unicode_string.is_empty() {
                d.unicode_string.clone()
            } else if !d.ascii_string.is_empty() {
                d.ascii_string.clone()
            } else {
                d.mac_string.clone()
            }
        }
    };
    let s = s.trim().trim_end_matches('\0').trim().to_string();
    (!s.is_empty()).then_some(s)
}
```
Add tests:
```rust
#[test]
fn parse_accepts_emitted_srgb_profile() {
    // Reuse the crate's own ICC emitter as a known-valid profile.
    let bytes = crate::emit_icc(crate::WorkingSpace::Srgb).expect("emit");
    let dp = DisplayProfile::parse(&bytes).expect("parse");
    assert!(!dp.name.is_empty());
}

#[test]
fn parse_rejects_garbage() {
    assert!(DisplayProfile::parse(&[0u8; 8]).is_err());
}
```

- [ ] **Step 2: Run test to verify it fails, then passes**

Run: `cargo test -p ferrolite-color display_lut::tests::parse`
Expected: PASS (uses the crate's own `emit_icc` — no external fixture needed for this task).

- [ ] **Step 3: Commit**

```bash
git add ferrolite-color/src/display_lut.rs
git commit -m "feat(color): DisplayProfile::parse via moxcms + name extraction"
```

- [ ] **Step 4: Add real monitor ICC fixtures (used by Task 3)**

Create `fixtures/icc/` and place two small real-world display profiles there for Task 3's known-value tests:
- `srgb.icc` — copy from the crate's emitter at test time (no binary needs committing): Task 3 tests generate it in-process via `emit_icc`.
- `widegamut.icc` — emit a Display P3 profile in-process via `emit_icc(WorkingSpace::DisplayP3)` as the stand-in "wide-gamut monitor". No external binary fixture is committed; profiles are produced in-process. (Skip creating files; this step is a no-op marker confirming Task 3 uses `emit_icc`-generated profiles as monitor stand-ins.)

---

### Task 3: `bake_display_lut` (working→monitor 3D LUT)

**Files:**
- Modify: `ferrolite-color/src/display_lut.rs`
- Modify: `ferrolite-color/Cargo.toml` (add `half` dep if not present — check first)

**Interfaces:**
- Consumes: `crate::WorkingSpace`, `DisplayProfile`, `moxcms::{Layout, TransformOptions, TransformExecutor, curve_from_gamma}`, `crate::{emit_icc-style base profiles}`, `crate::error::ColorError`, `half::f16`.
- Produces: `pub struct DisplayLut { pub size: u32, pub rgba16f: Vec<u16> }`, `pub fn bake_display_lut(working: WorkingSpace, monitor: &DisplayProfile, size: u32) -> Result<DisplayLut, ColorError>`.

- [ ] **Step 1: Confirm/add `half` dep**

Check `ferrolite-color/Cargo.toml` for `half`. If absent, add under `[dependencies]`:
```toml
half = { workspace = true }
```
(`half` is already a workspace dependency — see root `Cargo.toml`.)

- [ ] **Step 2: Write the failing test**

Add to `display_lut.rs`:
```rust
/// A baked `working→monitor` 3D LUT. `rgba16f` is `size³` RGBA half-float
/// texels, R fastest then G then B (matches wgpu `write_texture` row/layer order).
pub struct DisplayLut {
    pub size: u32,
    pub rgba16f: Vec<u16>,
}

/// Build a moxcms source profile representing `working` with a LINEAR TRC, so
/// working-linear RGB can be fed straight through a profile→profile transform.
fn linear_working_profile(working: WorkingSpace) -> moxcms::ColorProfile {
    let mut p = match working {
        WorkingSpace::Srgb => moxcms::ColorProfile::new_srgb(),
        WorkingSpace::AdobeRgb => moxcms::ColorProfile::new_adobe_rgb(),
        WorkingSpace::DisplayP3 => moxcms::ColorProfile::new_display_p3(),
        WorkingSpace::Rec2020 => moxcms::ColorProfile::new_bt2020(),
        WorkingSpace::ProPhoto => moxcms::ColorProfile::new_pro_photo_rgb(),
    };
    let lin = moxcms::curve_from_gamma(1.0);
    p.red_trc = Some(lin.clone());
    p.green_trc = Some(lin.clone());
    p.blue_trc = Some(lin);
    p.cicp = None; // don't let CICP transfer override the linear TRC
    p
}

/// Bake the `working→monitor` transform into a `size³` RGBA16F 3D LUT, indexed
/// through the gamma shaper (`shaper_decode`).
pub fn bake_display_lut(
    working: WorkingSpace,
    monitor: &DisplayProfile,
    size: u32,
) -> Result<DisplayLut, ColorError> {
    use moxcms::{Layout, TransformOptions};
    let src = linear_working_profile(working);
    let opts = TransformOptions {
        allow_use_cicp_transfer: false,
        prefer_fixed_point: false,
        ..TransformOptions::default()
    };
    let xf = src
        .create_transform_f32(Layout::Rgb, &monitor.profile, Layout::Rgb, opts)
        .map_err(|e| ColorError::Icc(e.to_string()))?;

    let n = size as usize;
    let denom = (n - 1) as f32;
    // Build the input grid: working-linear values from shaper-decoded indices.
    let mut input = Vec::with_capacity(n * n * n * 3);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                input.push(shaper_decode(r as f32 / denom));
                input.push(shaper_decode(g as f32 / denom));
                input.push(shaper_decode(b as f32 / denom));
            }
        }
    }
    let mut out = vec![0.0f32; input.len()];
    xf.transform(&input, &mut out)
        .map_err(|e| ColorError::Icc(e.to_string()))?;

    // Pack to RGBA16F, clamped to [0,1], alpha = 1.
    let mut rgba16f = Vec::with_capacity(n * n * n * 4);
    for px in out.chunks_exact(3) {
        rgba16f.push(half::f16::from_f32(px[0].clamp(0.0, 1.0)).to_bits());
        rgba16f.push(half::f16::from_f32(px[1].clamp(0.0, 1.0)).to_bits());
        rgba16f.push(half::f16::from_f32(px[2].clamp(0.0, 1.0)).to_bits());
        rgba16f.push(half::f16::from_f32(1.0).to_bits());
    }
    Ok(DisplayLut { size, rgba16f })
}
```
Add tests:
```rust
#[test]
fn bakes_lut_of_expected_shape() {
    let mon = DisplayProfile::parse(&crate::emit_icc(WorkingSpace::Srgb).unwrap()).unwrap();
    let lut = bake_display_lut(WorkingSpace::Srgb, &mon, DISPLAY_LUT_SIZE).unwrap();
    assert_eq!(lut.size, DISPLAY_LUT_SIZE);
    let n = DISPLAY_LUT_SIZE as usize;
    assert_eq!(lut.rgba16f.len(), n * n * n * 4);
    assert!(lut.rgba16f.iter().all(|&h| half::f16::from_bits(h).is_finite()));
}

#[test]
fn srgb_working_to_srgb_monitor_reproduces_srgb_oetf() {
    // sRGB working through an sRGB monitor profile ≈ the sRGB OETF within
    // trilinear tolerance: the LUT-encoded corners bracket a known value.
    let mon = DisplayProfile::parse(&crate::emit_icc(WorkingSpace::Srgb).unwrap()).unwrap();
    let lut = bake_display_lut(WorkingSpace::Srgb, &mon, DISPLAY_LUT_SIZE).unwrap();
    let n = DISPLAY_LUT_SIZE as usize;
    // Node at index (n-1,n-1,n-1) is working-linear (1,1,1) → sRGB ~1.0.
    let last = (n * n * n - 1) * 4;
    let white = half::f16::from_bits(lut.rgba16f[last]).to_f32();
    assert!((white - 1.0).abs() < 0.02, "white corner {white}");
    // Node at index (0,0,0) is (0,0,0) → 0.
    let black = half::f16::from_bits(lut.rgba16f[0]).to_f32();
    assert!(black.abs() < 0.02, "black corner {black}");
}

#[test]
fn lut_channels_are_monotonic_along_red_axis() {
    let mon = DisplayProfile::parse(&crate::emit_icc(WorkingSpace::Srgb).unwrap()).unwrap();
    let lut = bake_display_lut(WorkingSpace::Rec2020, &mon, DISPLAY_LUT_SIZE).unwrap();
    let n = DISPLAY_LUT_SIZE as usize;
    // Walk r at g=b=0; the R output must be non-decreasing.
    let mut prev = -1.0f32;
    for r in 0..n {
        let idx = r * 4; // g=b=0 → linear index = r
        let v = half::f16::from_bits(lut.rgba16f[idx]).to_f32();
        assert!(v + 1e-3 >= prev, "non-monotonic at r={r}: {v} < {prev}");
        prev = v;
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p ferrolite-color display_lut`
Expected: PASS. If `create_transform_f32`/`TransformOptions` field names differ in the pinned moxcms 0.8.1, adjust to the exact API (verified present: `create_transform_f32(src_layout, dst_pr, dst_layout, options)`, `TransformOptions { allow_use_cicp_transfer, prefer_fixed_point, .. }`, `TransformExecutor::transform(&self, src, dst)`).

- [ ] **Step 4: Commit**

```bash
git add ferrolite-color/src/display_lut.rs ferrolite-color/Cargo.toml
git commit -m "feat(color): bake working->monitor 3D LUT via moxcms"
```

---

## Plan phase 2 — engine-tier dual-path display tail (`ferrolite-vt`)

Engine tier stays generic (primitives only). The on-screen `display.wgsl` only.

### Task 4: Extend `DisplayColorUniform` + shader dual path

**Files:**
- Modify: `ferrolite-vt/src/pipelines.rs:19-32` (`DisplayColorUniform`, `pack_display_matrix`)
- Modify: `ferrolite-vt/src/shaders/display.wgsl`

**Interfaces:**
- Produces (Rust): `DisplayColorUniform { m: [[f32;4];3], use_lut: u32, shaper_gamma: f32, _pad: [f32;2] }` (still `Pod`/`Zeroable`).
- Produces (WGSL): `DisplayColor { m: mat3x3<f32>, use_lut: u32, shaper_gamma: f32, _pad: vec2<f32> }` at binding 8; `lut3d: texture_3d<f32>` @9; `lut_samp: sampler` @10.

- [ ] **Step 1: Extend the Rust uniform struct**

In `ferrolite-vt/src/pipelines.rs`, replace the `DisplayColorUniform` definition:
```rust
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayColorUniform {
    m: [[f32; 4]; 3],
    use_lut: u32,
    shaper_gamma: f32,
    _pad: [f32; 2],
}
```
`pack_display_matrix` is unchanged.

- [ ] **Step 2: Update the shader**

In `ferrolite-vt/src/shaders/display.wgsl`, replace the `DisplayColor` struct + binding block (lines 12-13) with:
```wgsl
struct DisplayColor { m: mat3x3<f32>, use_lut: u32, shaper_gamma: f32, _pad: vec2<f32> };
@group(0) @binding(8) var<uniform> disp: DisplayColor;
@group(0) @binding(9) var lut3d: texture_3d<f32>;
@group(0) @binding(10) var lut_samp: sampler;

fn shaper_encode(c: vec3<f32>) -> vec3<f32> {
    return pow(clamp(c, vec3(0.0), vec3(1.0)), vec3(1.0 / disp.shaper_gamma));
}

fn tail(lin: vec3<f32>) -> vec3<f32> {
    if (disp.use_lut == 0u) {
        return linear_to_srgb(disp.m * lin);
    }
    return textureSampleLevel(lut3d, lut_samp, shaper_encode(lin), 0.0).rgb;
}
```
Then in `fs_main`, `fs_tiled`, `fs_sparse`, replace each final `return vec4(linear_to_srgb(disp.m * lin), 1.0);` with `return vec4(tail(lin), 1.0);`.

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build -p ferrolite-vt`
Expected: compile error at `DisplayPipelines::new` and `set_display_matrix` — the struct initializers now miss `use_lut`/`shaper_gamma`/`_pad`. Fixed in Task 5. (This task's deliverable is the struct + shader; it commits together with Task 5.)

- [ ] **Step 4: (No standalone commit — proceed to Task 5, commit together.)**

---

### Task 5: LUT texture + sampler in `DisplayPipelines`; `set_display_lut`

**Files:**
- Modify: `ferrolite-vt/src/pipelines.rs` (struct fields, `new`, bind-group layouts, methods)

**Interfaces:**
- Consumes: `DisplayColorUniform` (Task 4).
- Produces:
  - `pub const LUT_SIZE: u32 = 33;` (mirrors `ferrolite_color::DISPLAY_LUT_SIZE`).
  - `pub fn display_lut_view(&self) -> &Arc<wgpu::TextureView>`
  - `pub fn display_lut_sampler(&self) -> &Arc<wgpu::Sampler>`
  - `pub fn set_display_lut(&self, queue: &wgpu::Queue, size: u32, rgba16f: &[u16], shaper_gamma: f32)`
  - `set_display_matrix` now also writes `use_lut = 0`.

- [ ] **Step 1: Add the LUT const, texture, sampler, and struct fields**

At the top of `pipelines.rs` (after the uniform):
```rust
/// Cube edge length of the display LUT texture. Mirrors
/// `ferrolite_color::DISPLAY_LUT_SIZE` — the two MUST match.
pub const LUT_SIZE: u32 = 33;
```
Add fields to `DisplayPipelines`:
```rust
    lut_texture: Arc<wgpu::Texture>,
    lut_view: Arc<wgpu::TextureView>,
    lut_sampler: Arc<wgpu::Sampler>,
```

- [ ] **Step 2: Create the LUT texture + sampler in `new` and add bindings 9/10 to all four layouts**

In `DisplayPipelines::new`, after the `display_matrix` buffer is created, add:
```rust
        let lut_texture = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vt-display-lut"),
            size: wgpu::Extent3d {
                width: LUT_SIZE,
                height: LUT_SIZE,
                depth_or_array_layers: LUT_SIZE,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        }));
        let lut_view = Arc::new(lut_texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let lut_sampler = Arc::new(device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vt-display-lut-samp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        }));
```
Add these two entries to **each** of the four bind-group layouts (the `single_bgl` entries array, the `tiled_bgl()` closure's array, and the `sparse_bgl` array — right after the `binding: 8` entry in each):
```rust
                wgpu::BindGroupLayoutEntry {
                    binding: 9,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 10,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
```
Initialize `DisplayColorUniform` (in the `display_matrix` `create_buffer_init`) with the new fields:
```rust
                contents: bytemuck::bytes_of(&DisplayColorUniform {
                    m: pack_display_matrix([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
                    use_lut: 0,
                    shaper_gamma: 2.2,
                    _pad: [0.0, 0.0],
                }),
```
Add the three new fields to the `Self { .. }` return.

- [ ] **Step 3: Add getters + `set_display_lut`, update `set_display_matrix`**

```rust
    /// The 3D-LUT texture view (bound @9 by every variant). Cloned into per-image VT resources.
    pub fn display_lut_view(&self) -> &Arc<wgpu::TextureView> {
        &self.lut_view
    }

    /// The LUT sampler (bound @10 by every variant).
    pub fn display_lut_sampler(&self) -> &Arc<wgpu::Sampler> {
        &self.lut_sampler
    }

    /// Upload a monitor LUT and switch the tail to the LUT path (`use_lut = 1`).
    /// `size` MUST equal `LUT_SIZE`; `rgba16f` is `size³` RGBA half-float texels.
    /// Call only when the profile / working space changes — never per frame/image.
    pub fn set_display_lut(&self, queue: &wgpu::Queue, size: u32, rgba16f: &[u16], shaper_gamma: f32) {
        debug_assert_eq!(size, LUT_SIZE, "display LUT size must match LUT_SIZE");
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.lut_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(rgba16f),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(size * 4 * 2), // 4 channels × 2 bytes (f16)
                rows_per_image: Some(size),
            },
            wgpu::Extent3d { width: size, height: size, depth_or_array_layers: size },
        );
        queue.write_buffer(
            &self.display_matrix,
            0,
            bytemuck::bytes_of(&DisplayColorUniform {
                m: pack_display_matrix([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
                use_lut: 1,
                shaper_gamma,
                _pad: [0.0, 0.0],
            }),
        );
    }
```
Update `set_display_matrix` to write the full struct with `use_lut: 0`:
```rust
    pub fn set_display_matrix(&self, queue: &wgpu::Queue, m: [[f32; 3]; 3]) {
        queue.write_buffer(
            &self.display_matrix,
            0,
            bytemuck::bytes_of(&DisplayColorUniform {
                m: pack_display_matrix(m),
                use_lut: 0,
                shaper_gamma: 2.2,
                _pad: [0.0, 0.0],
            }),
        );
    }
```

- [ ] **Step 4: Build**

Run: `cargo build -p ferrolite-vt`
Expected: compile errors in `view.rs` (bind groups miss bindings 9/10). Fixed in Task 6.

- [ ] **Step 5: (Commit together with Tasks 4 + 6.)**

---

### Task 6: Bind bindings 9/10 in every `view.rs` bind group

**Files:**
- Modify: `ferrolite-vt/src/view.rs` (every `binding: 8` bind-group site: ~lines 334, 404, 645, 987, 1063, 1356, 1417 — grep to confirm all)
- The per-variant VT resource structs already clone `display_matrix`; add `lut_view` + `lut_sampler` clones alongside.

**Interfaces:**
- Consumes: `DisplayPipelines::display_lut_view`, `display_lut_sampler` (Task 5).

- [ ] **Step 1: Find every bind-group + resource site**

Run: `grep -n "binding: 8\|display_matrix\|display_lut" ferrolite-vt/src/view.rs`
For each per-variant resource struct that stores `display_matrix: Arc<wgpu::Buffer>`, add:
```rust
    lut_view: Arc<wgpu::TextureView>,
    lut_sampler: Arc<wgpu::Sampler>,
```
and populate them where `display_matrix = pipelines.display_matrix_buffer().clone();` is set:
```rust
        let lut_view = pipelines.display_lut_view().clone();
        let lut_sampler = pipelines.display_lut_sampler().clone();
```

- [ ] **Step 2: Add the two entries at each bind-group site**

At every `wgpu::BindGroupEntry { binding: 8, resource: <...>.display_matrix.as_entire_binding() }`, add immediately after:
```rust
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: wgpu::BindingResource::TextureView(&<same>.lut_view),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::Sampler(&<same>.lut_sampler),
                },
```
(Replace `<same>` with the same receiver used for `display_matrix` at that site — `single`, `tiled`, `s`, etc.)

- [ ] **Step 3: Build + run the existing golden (regression)**

Run: `cargo build -p ferrolite-vt && cargo test -p ferrolite-vt`
Expected: PASS. The existing display golden still matches because `use_lut` defaults `0` → the shader path is byte-identical. (Goldens auto-skip if no GPU; run locally on the dev GPU to actually exercise them.)

- [ ] **Step 4: Commit Tasks 4 + 5 + 6 together**

```bash
git add ferrolite-vt/src
git commit -m "feat(vt): dual-path display tail with generic 3D-LUT (use_lut flag)"
```

---

### Task 7: GPU golden for the LUT path

**Files:**
- Modify: `ferrolite-vt/tests/golden.rs`

**Interfaces:**
- Consumes: `DisplayPipelines::set_display_lut`, `LUT_SIZE`.

- [ ] **Step 1: Write the golden test**

Add a test that: builds an identity-ish LUT on the CPU (each node `(r,g,b)` → its own shaper-encoded coordinate, i.e. output == `shaper_encode(shaper_decode(idx))` == idx, giving a LUT that maps working-linear→its shaper-encoded value), calls `set_display_lut`, renders a known single-texture image through the `Single` variant, reads back, and asserts each output pixel ≈ `shaper_encode(clamp(lin,0,1))` within tolerance (proves the LUT path samples correctly). Follow the existing golden harness in `golden.rs` for GPU setup + `#[test]` gating on `GpuContext::headless()`.
```rust
// Skeleton — adapt to the existing harness in this file:
#[test]
fn lut_path_samples_identity_shaper_lut() {
    let Some(ctx) = ferrolite_gpu::GpuContext::headless() else { return; };
    let n = ferrolite_vt::pipelines::LUT_SIZE as usize; // expose module if needed
    let denom = (n - 1) as f32;
    let mut rgba16f = Vec::with_capacity(n * n * n * 4);
    for b in 0..n { for g in 0..n { for r in 0..n {
        // Output = the sample coordinate itself (idx), so tail(lin)=shaper_encode(lin)=idx.
        rgba16f.push(half::f16::from_f32(r as f32 / denom).to_bits());
        rgba16f.push(half::f16::from_f32(g as f32 / denom).to_bits());
        rgba16f.push(half::f16::from_f32(b as f32 / denom).to_bits());
        rgba16f.push(half::f16::from_f32(1.0).to_bits());
    }}}
    // ... build pipelines, set_display_lut(&queue, LUT_SIZE, &rgba16f, 2.2),
    //     render a known image, read back, compare each px to shaper_encode(lin) ± tol.
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p ferrolite-vt lut_path`
Expected: PASS on the dev GPU; auto-skip (early return) when headless.

- [ ] **Step 3: Commit**

```bash
git add ferrolite-vt/tests/golden.rs
git commit -m "test(vt): golden for the 3D-LUT display path"
```

---

## Plan phase 3 — app: detection, off-thread bake, multi-monitor

### Task 8: `AppEvent::DisplayProfileResolved` + state fields

**Files:**
- Modify: `ferrolite-app/src/events.rs` (enum variant + `apply` arm)
- Modify: `ferrolite-app/src/state.rs` (new fields)

**Interfaces:**
- Produces: `AppEvent::DisplayProfileResolved { lut: Option<ferrolite_color::DisplayLut>, name: String, generation: u64 }`; `AppState { display_profile_name: String, display_detect_gen: u64, last_monitor_key: u64 }`.

- [ ] **Step 1: Add state fields**

In `ferrolite-app/src/state.rs`, add to `AppState`:
```rust
    /// Resolved display-profile name for the Settings label ("sRGB (default)" when off).
    pub display_profile_name: String,
    /// Monotonic generation; each re-detect bumps it. Stale job results are dropped.
    pub display_detect_gen: u64,
    /// The monitor key the window was last seen on (0 = unknown / unsupported OS).
    pub last_monitor_key: u64,
```
Initialize in `AppState::new`/`for_test` (defaults: `"sRGB (default)".to_string()`, `0`, `0`).

- [ ] **Step 2: Add the event variant + apply arm**

In `events.rs`, add to `AppEvent`:
```rust
    /// A display-profile detect+parse+bake job finished. `lut = Some` → the
    /// monitor-managed LUT path; `None` → sRGB fallback. `generation` guards
    /// against stale results from superseded re-detects. Handled in `app.rs`
    /// (needs GPU state); the `apply` fold ignores it.
    DisplayProfileResolved {
        lut: Option<ferrolite_color::DisplayLut>,
        name: String,
        generation: u64,
    },
```
Add to the `apply` match (handled in app.rs, nothing to fold):
```rust
            AppEvent::DisplayProfileResolved { .. } => None,
```
Add `ferrolite-color` to `ferrolite-app/Cargo.toml` deps if not already present (it is — `working_to_display` is used).

- [ ] **Step 3: Build**

Run: `cargo build -p ferrolite-app`
Expected: PASS (the app.rs match on AppEvent may warn about the new arm being unhandled in the direct-match in app.rs — wired in Task 11).

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/events.rs ferrolite-app/src/state.rs
git commit -m "feat(app): DisplayProfileResolved event + display-profile state"
```

---

### Task 9: `monitor_profile` detection module (Windows + macOS + stub)

**Files:**
- Create: `ferrolite-app/src/monitor_profile.rs`
- Modify: `ferrolite-app/src/lib.rs` or `main.rs` (add `mod monitor_profile;`)
- Modify: `ferrolite-app/Cargo.toml` (cfg-gated deps)

**Interfaces:**
- Produces:
  - `pub enum ProfileSource { Path(std::path::PathBuf), Bytes(Vec<u8>) }`
  - `pub fn detect(raw: raw_window_handle::RawWindowHandle) -> (Option<ProfileSource>, u64)` — returns the ICC source for the window's monitor + a stable `MonitorKey` (0 when unsupported/unknown).
  - `pub fn source_to_bytes(src: ProfileSource) -> std::io::Result<Vec<u8>>` — reads a `Path`, passes `Bytes` through (called on the job thread).

- [ ] **Step 1: Add cfg-gated deps**

In `ferrolite-app/Cargo.toml`:
```toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_Graphics_Gdi",
    "Win32_UI_ColorSystem",
] }

[target.'cfg(target_os = "macos")'.dependencies]
core-graphics = "0.24"
core-foundation = "0.10"
objc2 = "0.5"
objc2-app-kit = { version = "0.2", features = ["NSScreen", "NSView", "NSWindow"] }
objc2-foundation = { version = "0.2", features = ["NSString", "NSDictionary", "NSValue"] }
```
(`raw-window-handle` is already available transitively via eframe 0.29; add `raw-window-handle = "0.6"` to `[dependencies]` to name the types directly.)

- [ ] **Step 2: Write the module (parse/mode logic testable; FFI cfg-gated)**

Create `ferrolite-app/src/monitor_profile.rs`:
```rust
//! Per-monitor ICC profile detection for the window's current monitor.
//! FFI is cfg-gated (Windows GDI ICM, macOS ColorSync); other OSes return None.
//! The heavy work (file read, parse, bake) happens on a job thread, not here.

use std::path::PathBuf;

/// Where a detected/selected profile's bytes come from. `Path` is read on the
/// job thread (keeps file I/O off the UI thread).
pub enum ProfileSource {
    Path(PathBuf),
    Bytes(Vec<u8>),
}

/// Read a source to ICC bytes (job thread).
pub fn source_to_bytes(src: ProfileSource) -> std::io::Result<Vec<u8>> {
    match src {
        ProfileSource::Path(p) => std::fs::read(p),
        ProfileSource::Bytes(b) => Ok(b),
    }
}

#[cfg(windows)]
pub fn detect(raw: raw_window_handle::RawWindowHandle) -> (Option<ProfileSource>, u64) {
    use raw_window_handle::RawWindowHandle;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        CreateDCW, DeleteDC, GetMonitorInfoW, MonitorFromWindow, MONITORINFOEXW,
        MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::ColorSystem::GetICMProfileW;

    let RawWindowHandle::Win32(h) = raw else { return (None, 0); };
    let hwnd = HWND(h.hwnd.get() as *mut _);
    // SAFETY: hwnd is a live top-level window handle from eframe for this frame.
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let key = hmon.0 as u64;
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut mi.monitorInfo as *mut _ as *mut _).as_bool() == false {
            return (None, key);
        }
        let dc = CreateDCW(PWSTR(mi.szDevice.as_ptr() as *mut _), PWSTR::null(), PWSTR::null(), None);
        if dc.is_invalid() {
            return (None, key);
        }
        let mut len: u32 = 260;
        let mut buf = vec![0u16; len as usize];
        let ok = GetICMProfileW(dc, &mut len, PWSTR(buf.as_mut_ptr())).as_bool();
        let _ = DeleteDC(dc);
        if !ok {
            return (None, key);
        }
        buf.truncate(len.saturating_sub(1) as usize);
        let path = String::from_utf16_lossy(&buf);
        if path.is_empty() {
            return (None, key);
        }
        (Some(ProfileSource::Path(PathBuf::from(path))), key)
    }
}

#[cfg(target_os = "macos")]
pub fn detect(raw: raw_window_handle::RawWindowHandle) -> (Option<ProfileSource>, u64) {
    // Resolve the NSView → its window → screen → CGDirectDisplayID, then copy
    // the display color space's ICC data. Unverifiable on the Windows dev
    // machine; the manual Custom picker is the safety net.
    use raw_window_handle::RawWindowHandle;
    let RawWindowHandle::AppKit(_h) = raw else { return (None, 0); };
    // Implementation: use objc2-app-kit to get NSScreen for the view's window,
    // read `deviceDescription["NSScreenNumber"]` → CGDirectDisplayID (u64 key),
    // then core_graphics::display::CGDisplay::new(id).copy_color_space() and
    // CGColorSpace::icc_data() → Vec<u8>. Return (Some(Bytes(..)), id as u64).
    // If any step fails, return (None, key). See task note for the concrete calls.
    (None, 0) // Replaced by the objc2/core-graphics impl during execution.
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn detect(_raw: raw_window_handle::RawWindowHandle) -> (Option<ProfileSource>, u64) {
    (None, 0)
}
```
Register the module: add `mod monitor_profile;` in the app crate root (`main.rs`/`lib.rs`).

**NOTE for the executing agent:** the macOS `detect` body above is the only place a `todo!()`-style stub is acceptable *in the plan* because it is unverifiable here; you MUST replace the `(None, 0)` with the real objc2 + core-graphics implementation described in the comment before committing Task 9 (or, if the reviewer decides macOS is out of scope during execution, leave the stub and record that decision — the Windows path and the manual picker still satisfy the spec's cross-OS requirement).

- [ ] **Step 3: Build on Windows**

Run: `cargo build -p ferrolite-app`
Expected: PASS on Windows. Verify the exact `windows` 0.58 symbol paths (`GetICMProfileW` lives under `Win32::UI::ColorSystem`; `MONITORINFOEXW` under `Win32::Graphics::Gdi`) and adjust feature flags until it compiles.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/monitor_profile.rs ferrolite-app/src/main.rs ferrolite-app/Cargo.toml
git commit -m "feat(app): monitor_profile detection (Windows ICM, macOS ColorSync, stub)"
```

---

### Task 10: `PersistedDisplayProfile` DTO + resolve logic

**Files:**
- Modify: `ferrolite-app/src/settings/dto.rs`
- Modify: `ferrolite-app/src/settings/mod.rs` (`Settings` field + default)
- Test: `ferrolite-app/src/settings/dto.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `pub enum PersistedDisplayProfile { Auto, Srgb, Custom(PathBuf) }` (serde, default `Auto`); `pub fn resolve(mode: &PersistedDisplayProfile, detected: Option<ProfileSource>) -> Option<ProfileSource>`.

- [ ] **Step 1: Write the failing test**

In `ferrolite-app/src/settings/dto.rs`:
```rust
use crate::monitor_profile::ProfileSource;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum PersistedDisplayProfile {
    #[default]
    Auto,
    Srgb,
    Custom(std::path::PathBuf),
}

/// Resolve the effective profile source. `Srgb` → None (analytic sRGB path);
/// `Custom` → the file; `Auto` → whatever detection found (may be None).
pub fn resolve(
    mode: &PersistedDisplayProfile,
    detected: Option<ProfileSource>,
) -> Option<ProfileSource> {
    match mode {
        PersistedDisplayProfile::Srgb => None,
        PersistedDisplayProfile::Custom(p) => Some(ProfileSource::Path(p.clone())),
        PersistedDisplayProfile::Auto => detected,
    }
}
```
Tests:
```rust
#[test]
fn resolve_srgb_is_none() {
    assert!(matches!(
        resolve(&PersistedDisplayProfile::Srgb, Some(ProfileSource::Bytes(vec![1]))),
        None
    ));
}

#[test]
fn resolve_custom_uses_path_even_when_detected_present() {
    let p = std::path::PathBuf::from("x.icc");
    let r = resolve(&PersistedDisplayProfile::Custom(p.clone()), Some(ProfileSource::Bytes(vec![9])));
    assert!(matches!(r, Some(ProfileSource::Path(pp)) if pp == p));
}

#[test]
fn resolve_auto_passes_detected_through() {
    assert!(matches!(resolve(&PersistedDisplayProfile::Auto, None), None));
    assert!(matches!(
        resolve(&PersistedDisplayProfile::Auto, Some(ProfileSource::Bytes(vec![7]))),
        Some(ProfileSource::Bytes(_))
    ));
}

#[test]
fn display_profile_roundtrips_through_json() {
    for m in [
        PersistedDisplayProfile::Auto,
        PersistedDisplayProfile::Srgb,
        PersistedDisplayProfile::Custom("/tmp/p.icc".into()),
    ] {
        let js = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<PersistedDisplayProfile>(&js).unwrap(), m);
    }
}
```

- [ ] **Step 2: Add to `Settings`**

In `ferrolite-app/src/settings/mod.rs`, add the field + default:
```rust
    pub display_profile: dto::PersistedDisplayProfile,
```
```rust
            display_profile: dto::PersistedDisplayProfile::default(),
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ferrolite-app settings::dto`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/settings/dto.rs ferrolite-app/src/settings/mod.rs
git commit -m "feat(app): PersistedDisplayProfile DTO + resolve + persistence field"
```

---

### Task 11: Off-thread detect→bake orchestration + event handling

**Files:**
- Modify: `ferrolite-app/src/app.rs` (new method `redetect_display_profile`; handle `DisplayProfileResolved`; per-frame monitor-change check; call on startup, working-space change, and monitor change)

**Interfaces:**
- Consumes: `monitor_profile::{detect, source_to_bytes, ProfileSource}`, `settings::dto::resolve`, `ferrolite_color::{DisplayProfile, bake_display_lut, DISPLAY_LUT_SIZE, DISPLAY_LUT_SHAPER_GAMMA}`, `DisplayPipelines::{set_display_lut, set_display_matrix}`, `ferrolite_jobs::Priority`.

- [ ] **Step 1: Add `redetect_display_profile`**

In `app.rs` (near `apply_working_space`):
```rust
    /// Detect the window's monitor profile (UI-thread OS call), then parse+bake
    /// off-thread. Bumps the generation so stale results are dropped.
    fn redetect_display_profile(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        self.state.display_detect_gen += 1;
        let generation = self.state.display_detect_gen;
        let mode = self.state.settings.display_profile.clone();
        let working = self.state.working_space;
        let tx = self.state.tx.clone();

        // UI-thread OS call only (cheap): get the ICC source + monitor key.
        let (detected, key) = match frame.window_handle() {
            Ok(h) => crate::monitor_profile::detect(h.as_raw()),
            Err(_) => (None, 0),
        };
        self.state.last_monitor_key = key;
        let source = crate::settings::dto::resolve(&mode, detected);

        self.state.jobs.submit(ferrolite_jobs::Priority::Background, move |_cancel| {
            let (lut, name) = match source {
                None => (None, "sRGB (default)".to_string()),
                Some(src) => match crate::monitor_profile::source_to_bytes(src)
                    .ok()
                    .and_then(|b| ferrolite_color::DisplayProfile::parse(&b).ok())
                {
                    Some(profile) => match ferrolite_color::bake_display_lut(
                        working,
                        &profile,
                        ferrolite_color::DISPLAY_LUT_SIZE,
                    ) {
                        Ok(lut) => {
                            let name = profile.name.clone();
                            (Some(lut), name)
                        }
                        Err(e) => {
                            log::warn!("display LUT bake failed: {e}");
                            (None, "Not detected — using sRGB".to_string())
                        }
                    },
                    None => (None, "Not detected — using sRGB".to_string()),
                },
            };
            let _ = tx.send(crate::events::AppEvent::DisplayProfileResolved { lut, name, generation });
        });
        ctx.request_repaint();
    }
```
(Confirm the exact `JobSystem::submit` signature and the field name for the job system on `AppState` — the thumbnail path uses `self.state.jobs`; match it. Confirm `Priority::Background` exists in `ferrolite_jobs::Priority`.)

- [ ] **Step 2: Handle the event where other GPU-needing events are matched**

Find the `app.rs` event loop that matches `AppEvent::FullDecoded`/`PreviewReady` (search `AppEvent::PreviewReady`), and add:
```rust
            AppEvent::DisplayProfileResolved { lut, name, generation } => {
                if generation != self.state.display_detect_gen {
                    // superseded by a newer re-detect
                } else {
                    self.state.display_profile_name = name;
                    if let Some(rs) = frame.wgpu_render_state() {
                        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
                        let renderer = rs.renderer.read();
                        if let Some(vp) = renderer.callback_resources.get::<viewer::ViewerPipelines>() {
                            match lut {
                                Some(l) => vp.pipelines.set_display_lut(
                                    &gpu.queue,
                                    l.size,
                                    &l.rgba16f,
                                    ferrolite_color::DISPLAY_LUT_SHAPER_GAMMA,
                                ),
                                None => vp.pipelines.set_display_matrix(
                                    &gpu.queue,
                                    ferrolite_color::working_to_display(self.state.working_space),
                                ),
                            }
                        }
                    }
                    ctx.request_repaint();
                }
            }
```

- [ ] **Step 3: Trigger on startup, working-space change, and monitor change**

- Startup: after the viewer pipelines are pre-warmed (find where the app first has a valid render state — e.g. the first `update` frame guarded by a `bool` flag), call `self.redetect_display_profile(ctx, frame)` once.
- Working-space change: in `apply_working_space`, after the existing `set_display_matrix` block, add: `self.redetect_display_profile(ctx, frame);` (re-bakes the LUT for the new working space when a profile is active; when in sRGB mode it's a cheap no-op resolving to the matrix).
- Per-frame monitor change: near the top of `update`, once per frame, compute the current key and compare:
```rust
        if let Ok(h) = frame.window_handle() {
            let (_src, key) = crate::monitor_profile::detect(h.as_raw());
            if key != self.state.last_monitor_key {
                self.redetect_display_profile(ctx, frame);
            }
        }
```
(This calls `detect` twice on a change — acceptable; `detect` is a cheap OS lookup. If profiling shows cost, split a lightweight `current_monitor_key(raw) -> u64` out of `detect`.)

- [ ] **Step 4: Build + full test**

Run: `cargo build -p ferrolite-app && cargo test -p ferrolite-app`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/app.rs
git commit -m "feat(app): off-thread display-profile detect+bake, event apply, re-detect triggers"
```

---

## Plan phase 4 — Settings UI + persistence

### Task 12: Settings → General "Display" group

**Files:**
- Modify: `ferrolite-app/src/settings/ui.rs` (`draw_general_tab`)
- Modify: `ferrolite-app/src/app.rs` (react to the change: persist + re-detect)

**Interfaces:**
- Consumes: `settings::dto::PersistedDisplayProfile`, `state.display_profile_name`.

- [ ] **Step 1: Add the Display group to `draw_general_tab`**

Find `fn draw_general_tab(ui, settings) -> bool` in `settings/ui.rs`. Add a Display section (returns `true` if changed):
```rust
    ui.add_space(8.0);
    ui.heading("Display");
    ui.label(format!("Active profile: {}", /* passed-in name; see Step 2 */ display_name));
    let mut mode = settings.display_profile.clone();
    let mut changed = false;
    egui::ComboBox::from_label("Monitor color")
        .selected_text(match &mode {
            dto::PersistedDisplayProfile::Auto => "Auto (detect)".to_string(),
            dto::PersistedDisplayProfile::Srgb => "sRGB".to_string(),
            dto::PersistedDisplayProfile::Custom(_) => "Custom file…".to_string(),
        })
        .show_ui(ui, |ui| {
            changed |= ui.selectable_value(&mut mode, dto::PersistedDisplayProfile::Auto, "Auto (detect)").changed();
            changed |= ui.selectable_value(&mut mode, dto::PersistedDisplayProfile::Srgb, "sRGB").changed();
            if ui.selectable_label(matches!(mode, dto::PersistedDisplayProfile::Custom(_)), "Custom file…").clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("ICC profile", &["icc", "icm"]).pick_file() {
                    mode = dto::PersistedDisplayProfile::Custom(path);
                    changed = true;
                }
            }
        });
    if ui.button("Redetect").clicked() {
        changed = true; // force a re-resolve even if the mode is unchanged
    }
    if changed {
        settings.display_profile = mode;
    }
```
Because `draw_general_tab` only has `settings`, thread the display name in: change its signature to `draw_general_tab(ui, settings, display_name: &str)` and pass `&self.state.display_profile_name` (or a temp) from the `show` call chain. Update `settings::ui::show`'s signature to accept and forward `display_name` from `app.rs`.

- [ ] **Step 2: React to the change in `app.rs`**

Where `settings::ui::show(...)` is called (search `show_settings`), it returns `changed: bool`. When `changed`, the app already persists settings; add a re-detect so the new mode takes effect immediately:
```rust
            if settings_changed {
                self.mark_settings_dirty();
                self.redetect_display_profile(ctx, frame);
            }
```
Pass `&self.state.display_profile_name` into the `show` call.

- [ ] **Step 3: Build**

Run: `cargo build -p ferrolite-app`
Expected: PASS. (No unit test for egui rendering — this is covered by the author's visual test.)

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/settings/ui.rs ferrolite-app/src/app.rs
git commit -m "feat(app): Settings > Display group (Auto/sRGB/Custom + Redetect)"
```

---

### Task 13: Startup resolve from persisted setting

**Files:**
- Modify: `ferrolite-app/src/app.rs` (ensure the startup re-detect honors the persisted mode — already does, since `redetect_display_profile` reads `settings.display_profile`)

- [ ] **Step 1: Verify startup path**

Confirm the one-time startup call added in Task 11 Step 3 runs after settings are loaded (settings load happens before the first frame). If startup detection runs before the viewer pipelines exist, the `DisplayProfileResolved` handler already guards on `wgpu_render_state()`/`ViewerPipelines` being present; on the first successful frame the re-detect installs the LUT. Add the startup trigger inside the same guard that pre-warms `ViewerPipelines` so ordering is guaranteed.

- [ ] **Step 2: Manual smoke (documented, run by the author)**

Document in the commit: launch the app, open Settings → Display, confirm "Active profile" shows the real monitor profile name on Windows; switch to sRGB and back to Auto; pick a Custom `.icc`; drag the window to a second monitor and confirm the label updates.

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/app.rs
git commit -m "feat(app): resolve display profile from persisted setting at startup"
```

---

## Final gate (run before handing off for the author's visual test)

- [ ] `cargo fmt --all`
- [ ] `cargo fmt --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] All green → **STOP**. Hold for the author's hands-on visual test on real Windows monitors (open images on an sRGB and a wide-gamut monitor; drag the window between them; toggle Auto/sRGB/Custom in Settings). Do not merge/finish until the author confirms.

---

## Self-review notes (author of the plan)

- **Spec coverage:** §4 color math → Tasks 1–3. §5 engine tail → Tasks 4–7. §6 app detect/bake/re-detect/settings/persistence → Tasks 8–13. §7 histogram+blit unchanged → no task (deliberately untouched; the golden in Task 6 proves the sRGB path is byte-identical). §8 error handling → Task 11 fallbacks + `parse`/`bake` `Result`s. §9 testing → Tasks 1–3, 7, 10. §10 contracts → Global Constraints + job in Task 11. §11 four plans → the four plan phases.
- **Placeholder scan:** the only intentional stub is the macOS `detect` body (Task 9), explicitly flagged as unverifiable-here with the concrete API to fill in and a reviewer decision point. All other steps carry real code.
- **Type consistency:** `DisplayLut { size, rgba16f }`, `DisplayProfile { profile, name }`, `bake_display_lut(working, &DisplayProfile, size) -> Result<DisplayLut, ColorError>`, `set_display_lut(queue, size, rgba16f, shaper_gamma)`, `AppEvent::DisplayProfileResolved { lut, name, generation }`, `PersistedDisplayProfile { Auto, Srgb, Custom(PathBuf) }`, `resolve(mode, detected) -> Option<ProfileSource>` are used consistently across tasks. `DISPLAY_LUT_SIZE` (color) mirrors `LUT_SIZE` (vt).
- **Known verification points for the executor:** exact `windows` 0.58 symbol paths/feature flags (Task 9); `eframe` 0.29 `window_handle()`/`HasWindowHandle` accessor (Task 11); `ferrolite_jobs::Priority::Background` + `JobSystem::submit` signature (Task 11); moxcms `TransformOptions` field names (Task 3).

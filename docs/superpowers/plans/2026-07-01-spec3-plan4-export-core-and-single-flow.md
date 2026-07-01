# Spec 3 Plan 4 — `ferrolite-export` encode core + single-file Photo → Export

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up a new photo-tier `ferrolite-export` crate that renders the full-res edited image **tiled** through the Spec 2 GPU tile producer (no whole-image RGBA16F), converts working→output via `ferrolite-color`, optionally resizes, and encodes JPEG/PNG/TIFF/WebP (8-bit default; 16-bit TIFF/PNG) with EXIF copy + embedded ICC — then wire a single-file **Photo → Export** flow (interactive menu → format+options popup → destination popup → one cancellable `ferrolite-jobs` Background job).

**Architecture:** `ferrolite-export` reuses `ferrolite_pipeline::TileEditPipeline` + the viewer's already-GPU-resident `Arc<GpuPyramidSource>`. The export runs inside a `ferrolite-jobs` **Background** worker closure that captures the app's shared `Arc<GpuContext>` (wgpu is internally synchronized for cross-thread submit/poll) and the viewer's pyramid `Arc` — so it **never blocks the UI/update thread** and needs **no re-decode and no source re-upload**. It renders one `TILE_SIZE²` tile at a time via `TileEditPipeline::produce_tile`, reads that tile back to the CPU (`Rgba16Float` → f32), applies the working→output 3×3 + output OETF, quantizes per-tile into the final RGB buffer (never a whole-image f32/RGBA16F CPU buffer), optionally resizes with `fast_image_resize`, encodes with the pure-Rust `image` crate (ICC via `ImageEncoder::set_icc_profile`), and copies EXIF with `little_exif`. Progress + completion flow back over the existing app event channel.

**Tech Stack:** Rust, `wgpu` (reuse Spec 2 tile producer; per-tile `copy_texture_to_buffer` readback), `ferrolite-color` (working→output matrix + `emit_icc` + a new per-space `output_oetf`), `ferrolite-pipeline` (`TileEditPipeline`, `GpuPyramidSource`, `OpStack`, a new `edited_output_dims`), `ferrolite-jobs` (`Priority::Background` + `CancelToken`), `image = 0.25` (jpeg/png/tiff/webp), `fast_image_resize = 6.0`, `little_exif = 0.6`, `bytemuck`, `half`, `rfd`, `egui`/`eframe`.

## Global Constraints

- **Licensing tiers (spec §3):** `ferrolite-export` and `ferrolite-color` are **photo tier** (GPL-OK). `ferrolite-export` may depend on `ferrolite-pipeline`, `ferrolite-color`, `ferrolite-gpu`, `ferrolite-image`, `ferrolite-jobs`, `image`, `fast_image_resize`, `little_exif`. It **must not** be depended on by any engine-tier crate (`ferrolite-gpu`/`ferrolite-vt`/`ferrolite-image`). Do **not** add `ferrolite-export` or `ferrolite-color` as a dependency of an engine crate.
- **No whole-image RGBA16F, anywhere (CLAUDE.md §1, §2; spec §8.1):** render **tile-by-tile** with `TileEditPipeline::produce_tile` and read back each `TILE_SIZE²` tile individually. The only full-image CPU buffer allowed is the **final quantized RGB output** (3 bytes/px at 8-bit, 6 bytes/px at 16-bit) — never a full-image f32 or RGBA16F buffer.
- **Off the UI/update thread (CLAUDE.md §1):** every export runs inside a `ferrolite-jobs::Priority::Background` closure. The closure captures the shared `Arc<GpuContext>` (Send+Sync) and the viewer's `Arc<GpuPyramidSource>` (Send+Sync); it builds its `TileEditPipeline` **inside** the closure (the pipeline is `!Send` because it holds `Rc`/`Cell`, so it must be created and dropped on the worker thread, never moved across threads). GPU submissions from the worker are safe (wgpu synchronizes the device/queue internally). After completion the job sends an app event and calls `ctx.request_repaint()`.
- **Export GPU threading decision (resolved 2026-07-01):** **shared render device on the Background worker, reusing the viewer's resident pyramid** — zero re-decode, zero re-upload, no extra whole-image memory. Do not create a second wgpu device and do not re-decode.
- **WebP decision (resolved 2026-07-01):** WebP is **lossless** (image 0.25 native VP8L encoder; no C toolchain per spec §2). The **quality** setting applies to **JPEG only**; PNG/TIFF are lossless; WebP is lossless. Do not add `libwebp`/`webp`/`ravif`/`jpegxl` (Spec 4).
- **Bit depth (spec §8.2):** 8-bit is the default. **16-bit is valid only for TIFF and PNG.** For JPEG and WebP, silently clamp any 16-bit request to 8-bit (the mapping unit enforces this).
- **Output OETF via `ferrolite-color` (spec §8.1):** the output space's OETF is added to `ferrolite-color` as `output_oetf(space, linear)` (sRGB/Display-P3 = sRGB piecewise; Adobe RGB = gamma 2.19921875; ProPhoto = 1.8 + linear toe; Rec.2020 = BT.2020 piecewise). Never hardcode an OETF in `ferrolite-export`.
- **Output is RGB (no alpha):** edited photos are opaque; drop the alpha channel on quantize. Output color types are `Rgb8` / `Rgb16` only.
- **Error handling never panics (spec §10):** encode/write failure → `Err(ExportError::...)` surfaced as a status-bar warning; ICC-emit or `set_icc_profile` failure → proceed **without** an embedded profile + a warning (the file is still valid, just untagged); EXIF-copy failure → proceed + warning; job panics are already caught at the `ferrolite-jobs` worker boundary. `CancelToken` is checked once per tile.
- **Pipelines built once per export job (CLAUDE.md GPU rule):** `TileEditPipeline::new` builds its compute pipelines once inside the closure and reuses them across all tiles of that export. Do not build pipelines per tile. (Per-open/per-frame pipeline reuse is unchanged — export is a separate, non-interactive job.)
- **Gate (necessary, not sufficient):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → **then STOP and hold for the author's (Jann's) hands-on visual test** (open an image, Photo → Export, pick options + destination, confirm the written file opens and looks right) before finishing the branch (CLAUDE.md "Finishing a branch" rule).
- **GPU goldens auto-skip headless:** every GPU test starts with `let Some(ctx) = ferrolite_gpu::GpuContext::headless() else { eprintln!("no GPU adapter; skipping (headless CI)"); return; };`. `cargo test --workspace` stays green headless (spec §11).
- **Branch:** continue on `feat/color-and-export`. Conventional-commit messages, no attribution footer (disabled globally).

---

## File Structure

**`ferrolite-color`** (photo tier — extend):
- Modify `ferrolite-color/src/oetf.rs` — add `output_oetf(space, linear)` + unit tests.
- Modify `ferrolite-color/src/lib.rs` — re-export `output_oetf`.

**`ferrolite-pipeline`** (photo tier — extend):
- Modify `ferrolite-pipeline/src/lib.rs` — add + export `edited_output_dims(&OpStack, u32, u32) -> (u32, u32)`.

**`ferrolite-export`** (photo tier — **new crate**):
- `ferrolite-export/Cargo.toml`
- `ferrolite-export/src/lib.rs` — module wiring + public re-exports.
- `ferrolite-export/src/error.rs` — `ExportError`.
- `ferrolite-export/src/options.rs` — `ExportFormat`, `BitDepth`, `ResizeSpec`, `ExportOptions` (+ defaults §8.2) + the format→effective-bit-depth/quality mapping unit.
- `ferrolite-export/src/resize.rs` — pure `resize_dims` math + `apply_resize` (`fast_image_resize`).
- `ferrolite-export/src/convert.rs` — pure `convert_pixel` (matrix + OETF, clamp) + `to_u8`/`to_u16` quantizers.
- `ferrolite-export/src/render.rs` — `RenderedImage`/`PixelData` + `render_tiled` (GPU tiled render + per-tile readback + per-tile convert/assemble).
- `ferrolite-export/src/encode.rs` — `encode_to_file` (4 formats + ICC embed).
- `ferrolite-export/src/metadata.rs` — `copy_exif` (`little_exif`).
- `ferrolite-export/src/job.rs` — `ExportRequest`, `ExportOutcome`, `run_export` orchestrator.
- `ferrolite-export/tests/render_golden.rs` — tiled-vs-whole-image GPU golden + cancellation.
- `ferrolite-export/tests/encode_roundtrip.rs` — per-format encode→decode round-trip + ICC-present.

**`ferrolite-app`** (photo tier — wire the flow):
- Modify `Cargo.toml` (root workspace) — add `ferrolite-export` to `members` + `[workspace.dependencies]` + the two new deps `little_exif`, and `image` already present.
- Modify `ferrolite-app/Cargo.toml` — add `ferrolite-export` dep; widen `image` features to `["jpeg","png","tiff","webp"]`.
- Modify `ferrolite-app/src/events.rs` — add `ExportProgress` + `ExportFinished`.
- Modify `ferrolite-app/src/state.rs` — add `export_dialog: Option<crate::export::ExportDialogState>`.
- Create `ferrolite-app/src/export/mod.rs` — `ExportDialogState`, `draw_dialog`, `spawn_export`.
- Modify `ferrolite-app/src/lib.rs` (or `main.rs` module list) — add `pub mod export;`.
- Modify `ferrolite-app/src/chrome/mod.rs` — interactive `Photo` menu returning a `MenuAction`.
- Modify `ferrolite-app/src/app.rs` — handle the menu action, draw the dialog, spawn the job, handle the events.

---

## Task 1: `ferrolite-color::output_oetf` — per-space output encoding

Add the output-space OETF so `ferrolite-export` never hardcodes a transfer function (spec §8.1).

**Files:**
- Modify: `ferrolite-color/src/oetf.rs`
- Modify: `ferrolite-color/src/lib.rs`

**Interfaces:**
- Consumes: existing `srgb_oetf(f32) -> f32`, `WorkingSpace`.
- Produces: `pub fn output_oetf(space: WorkingSpace, linear: f32) -> f32` — maps a linear (0..1) channel to the space's encoded value; input is clamped to `[0, 1]`.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` in `ferrolite-color/src/oetf.rs` (add one if absent, `use super::*; use crate::WorkingSpace;`):

```rust
    #[test]
    fn output_oetf_srgb_and_p3_match_srgb_oetf() {
        for &x in &[0.0_f32, 0.001, 0.05, 0.5, 1.0] {
            assert_eq!(output_oetf(WorkingSpace::Srgb, x), srgb_oetf(x));
            assert_eq!(output_oetf(WorkingSpace::DisplayP3, x), srgb_oetf(x));
        }
    }

    #[test]
    fn output_oetf_endpoints_are_zero_and_one() {
        for &ws in &WorkingSpace::ALL {
            assert!((output_oetf(ws, 0.0)).abs() < 1e-6, "{ws:?} @0");
            assert!((output_oetf(ws, 1.0) - 1.0).abs() < 1e-4, "{ws:?} @1");
        }
    }

    #[test]
    fn output_oetf_clamps_out_of_range() {
        assert_eq!(output_oetf(WorkingSpace::Srgb, -0.5), 0.0);
        assert!((output_oetf(WorkingSpace::Srgb, 2.0) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn output_oetf_adobe_rgb_is_pure_gamma() {
        // Adobe RGB (1998): gamma 2.19921875; encoded = linear^(1/2.19921875).
        let want = 0.5_f32.powf(1.0 / 2.19921875);
        assert!((output_oetf(WorkingSpace::AdobeRgb, 0.5) - want).abs() < 1e-4);
    }

    #[test]
    fn output_oetf_is_monotonic() {
        for &ws in &WorkingSpace::ALL {
            let mut prev = -1.0_f32;
            for i in 0..=20 {
                let v = output_oetf(ws, i as f32 / 20.0);
                assert!(v >= prev - 1e-6, "{ws:?} not monotonic at {i}");
                prev = v;
            }
        }
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-color output_oetf`
Expected: FAIL — `cannot find function output_oetf`.

- [ ] **Step 3: Implement `output_oetf`**

In `ferrolite-color/src/oetf.rs`, add (keep `srgb_oetf`/`srgb_eotf` unchanged) — note the `use crate::WorkingSpace;` at the top of the module if not already imported:

```rust
use crate::WorkingSpace;

/// Encode a linear working-space channel into the given output space's encoded
/// (display-referred) value. Input is clamped to `[0, 1]`. Used at export encode
/// time (spec §8.1); the display path uses `srgb_oetf` in-shader instead.
pub fn output_oetf(space: WorkingSpace, linear: f32) -> f32 {
    let l = linear.clamp(0.0, 1.0);
    match space {
        // sRGB and Display P3 share the sRGB piecewise transfer.
        WorkingSpace::Srgb | WorkingSpace::DisplayP3 => srgb_oetf(l),
        // Adobe RGB (1998): pure gamma 2.19921875 (= 563/256).
        WorkingSpace::AdobeRgb => l.powf(1.0 / 2.19921875),
        // ProPhoto RGB (ROMM): gamma 1.8 with a short linear toe (Et = 1/512).
        WorkingSpace::ProPhoto => {
            const ET: f32 = 1.0 / 512.0;
            if l < ET {
                16.0 * l
            } else {
                l.powf(1.0 / 1.8)
            }
        }
        // BT.2020 / Rec.2020 piecewise OETF.
        WorkingSpace::Rec2020 => {
            const ALPHA: f32 = 1.099_296_8; // 1.09929682680944
            const BETA: f32 = 0.018_053_97; // 0.018053968510807
            if l < BETA {
                4.5 * l
            } else {
                ALPHA * l.powf(0.45) - (ALPHA - 1.0)
            }
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ferrolite-color output_oetf`
Expected: PASS (5 tests).

- [ ] **Step 5: Export it**

In `ferrolite-color/src/lib.rs`, extend the oetf re-export:

```rust
pub use oetf::{output_oetf, srgb_eotf, srgb_oetf};
```

Run: `cargo build -p ferrolite-color`
Expected: compiles.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-color/src/oetf.rs ferrolite-color/src/lib.rs
git commit -m "feat(color): per-space output_oetf for export encoding"
```

---

## Task 2: `ferrolite-pipeline::edited_output_dims` — output size after geometry

The tiled export renders in **output space**; the output image size = the geometry-applied extent. Expose it (wraps the existing pure `geometry_uniform`).

**Files:**
- Modify: `ferrolite-pipeline/src/lib.rs`

**Interfaces:**
- Consumes: `crate::uniforms::geometry_uniform(op: Option<Geometry>, src_w: u32, src_h: u32) -> (GeometryUniform, u32, u32)` (returns `(uniform, out_w, out_h)`); `OpStack::geometry() -> Option<Geometry>`.
- Produces: `pub fn edited_output_dims(stack: &OpStack, src_w: u32, src_h: u32) -> (u32, u32)`.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` in `ferrolite-pipeline/src/lib.rs` (create one with `use super::*;` if absent):

```rust
    #[test]
    fn edited_output_dims_identity_equals_source() {
        let stack = OpStack::default();
        assert_eq!(edited_output_dims(&stack, 6000, 4000), (6000, 4000));
    }
```

> If `ferrolite-pipeline/src/lib.rs` has no test module, add `use crate::{edited_output_dims, OpStack};` inside a fresh `#[cfg(test)] mod lib_tests { ... }` block instead of `use super::*;`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-pipeline edited_output_dims`
Expected: FAIL — `cannot find function edited_output_dims`.

- [ ] **Step 3: Implement + export**

In `ferrolite-pipeline/src/lib.rs`, near the other `pub use`/free functions, add:

```rust
/// Output image dimensions after the stack's geometry (crop/rotate) is applied to
/// a `src_w × src_h` source. For an identity/absent geometry op this is the source
/// size. The tiled full-res export renders `ceil(out_w/TILE_SIZE) × ceil(out_h/
/// TILE_SIZE)` tiles in this output space.
pub fn edited_output_dims(stack: &OpStack, src_w: u32, src_h: u32) -> (u32, u32) {
    let (_, out_w, out_h) = crate::uniforms::geometry_uniform(stack.geometry(), src_w, src_h);
    (out_w, out_h)
}
```

> Ensure `OpStack` is in scope in `lib.rs` (it already is, since it is re-exported). If `uniforms` is a private module, `crate::uniforms::geometry_uniform` is still reachable from `lib.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ferrolite-pipeline edited_output_dims`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): edited_output_dims (geometry-applied output size)"
```

---

## Task 3: `ferrolite-export` crate scaffold + options + error

Create the crate with its dependency set, the export options model (defaults §8.2), and the format→effective-settings mapping unit.

**Files:**
- Modify: root `Cargo.toml`
- Create: `ferrolite-export/Cargo.toml`
- Create: `ferrolite-export/src/lib.rs`
- Create: `ferrolite-export/src/error.rs`
- Create: `ferrolite-export/src/options.rs`

**Interfaces:**
- Produces:
  - `pub enum ExportFormat { Jpeg, Png, Tiff, WebP }` with `pub fn extension(self) -> &'static str`, `pub fn label(self) -> &'static str`, `pub const ALL: [ExportFormat; 4]`, `pub fn supports_16bit(self) -> bool`, `pub fn supports_quality(self) -> bool`.
  - `pub enum BitDepth { Eight, Sixteen }`.
  - `pub enum ResizeSpec { None, LongEdge(u32), Exact { w: u32, h: u32 }, Percent(f32) }`.
  - `pub struct ExportOptions { format, output_space, bit_depth, quality: u8, resize, copy_exif: bool, embed_icc: bool, strip_metadata: bool }` with `Default` (= §8.2), plus `pub fn effective_bit_depth(&self) -> BitDepth` (clamps to `Eight` unless the format supports 16-bit).
  - `pub enum ExportError { NoGpu, Cancelled, Render(String), Encode(String), Io(String) }` (thiserror).

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`, add `"ferrolite-export"` to `members`, add the path dep to `[workspace.dependencies]`, and add `little_exif`:

```toml
members = ["ferrolite-app", "ferrolite-image", "ferrolite-decode", "ferrolite-catalog", "ferrolite-jobs", "ferrolite-gpu", "ferrolite-vt", "ferrolite-pipeline", "ferrolite-color", "ferrolite-export"]
```

```toml
ferrolite-export = { path = "ferrolite-export" }
little_exif = "0.6"
```

(Leave the existing `image`, `fast_image_resize`, `half`, `bytemuck`, `thiserror` workspace deps as-is.)

- [ ] **Step 2: Create `ferrolite-export/Cargo.toml`**

```toml
[package]
name = "ferrolite-export"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
ferrolite-color = { workspace = true }
ferrolite-pipeline = { workspace = true }
ferrolite-gpu = { workspace = true }
ferrolite-image = { workspace = true }
ferrolite-jobs = { workspace = true }
image = { workspace = true, features = ["jpeg", "png", "tiff", "webp"] }
fast_image_resize = { workspace = true }
little_exif = { workspace = true }
half = { workspace = true }
bytemuck = { workspace = true }
thiserror = { workspace = true }
wgpu = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 3: Create `ferrolite-export/src/error.rs`**

```rust
//! Export error type. Never panics — every failure is a variant surfaced to the
//! UI as a status-bar warning (spec §10).

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("no GPU adapter available for export")]
    NoGpu,
    #[error("export cancelled")]
    Cancelled,
    #[error("render failed: {0}")]
    Render(String),
    #[error("encode failed: {0}")]
    Encode(String),
    #[error("write failed: {0}")]
    Io(String),
}
```

- [ ] **Step 4: Write the failing options tests**

Create `ferrolite-export/src/options.rs` with the test module first (implementation follows in Step 5):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec_8_2() {
        let o = ExportOptions::default();
        assert_eq!(o.format, ExportFormat::Jpeg);
        assert_eq!(o.output_space, ferrolite_color::WorkingSpace::Srgb);
        assert_eq!(o.bit_depth, BitDepth::Eight);
        assert_eq!(o.quality, 90);
        assert_eq!(o.resize, ResizeSpec::None);
        assert!(o.copy_exif);
        assert!(o.embed_icc);
        assert!(!o.strip_metadata);
    }

    #[test]
    fn sixteen_bit_only_for_tiff_and_png() {
        for f in ExportFormat::ALL {
            let o = ExportOptions {
                format: f,
                bit_depth: BitDepth::Sixteen,
                ..Default::default()
            };
            let expected = if f.supports_16bit() {
                BitDepth::Sixteen
            } else {
                BitDepth::Eight
            };
            assert_eq!(o.effective_bit_depth(), expected, "{f:?}");
        }
        assert!(ExportFormat::Tiff.supports_16bit());
        assert!(ExportFormat::Png.supports_16bit());
        assert!(!ExportFormat::Jpeg.supports_16bit());
        assert!(!ExportFormat::WebP.supports_16bit());
    }

    #[test]
    fn only_jpeg_uses_quality() {
        assert!(ExportFormat::Jpeg.supports_quality());
        assert!(!ExportFormat::Png.supports_quality());
        assert!(!ExportFormat::Tiff.supports_quality());
        assert!(!ExportFormat::WebP.supports_quality());
    }

    #[test]
    fn extensions_are_stable() {
        assert_eq!(ExportFormat::Jpeg.extension(), "jpg");
        assert_eq!(ExportFormat::Png.extension(), "png");
        assert_eq!(ExportFormat::Tiff.extension(), "tif");
        assert_eq!(ExportFormat::WebP.extension(), "webp");
    }
}
```

- [ ] **Step 5: Implement `options.rs` (above the test module)**

```rust
//! Export options (spec §8.2). Shared by the single flow (Plan 4) and the batch
//! Export module (Plan 5). `Default` encodes the spec defaults.

use ferrolite_color::WorkingSpace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Jpeg,
    Png,
    Tiff,
    WebP,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 4] = [
        ExportFormat::Jpeg,
        ExportFormat::Png,
        ExportFormat::Tiff,
        ExportFormat::WebP,
    ];

    /// Lower-case file extension (no dot).
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Jpeg => "jpg",
            ExportFormat::Png => "png",
            ExportFormat::Tiff => "tif",
            ExportFormat::WebP => "webp",
        }
    }

    /// Human label for the format combo.
    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Jpeg => "JPEG",
            ExportFormat::Png => "PNG",
            ExportFormat::Tiff => "TIFF",
            ExportFormat::WebP => "WebP (lossless)",
        }
    }

    /// 16-bit output is supported only for TIFF and PNG (spec §8.2).
    pub fn supports_16bit(self) -> bool {
        matches!(self, ExportFormat::Tiff | ExportFormat::Png)
    }

    /// Only JPEG honors the quality setting (WebP is lossless; PNG/TIFF lossless).
    pub fn supports_quality(self) -> bool {
        matches!(self, ExportFormat::Jpeg)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    Eight,
    Sixteen,
}

/// Optional output resize (spec §8.1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResizeSpec {
    None,
    /// Scale so the longer edge equals this many pixels (aspect preserved).
    LongEdge(u32),
    /// Exact width×height (aspect may change).
    Exact { w: u32, h: u32 },
    /// Scale both axes by this fraction (1.0 = unchanged).
    Percent(f32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub output_space: WorkingSpace,
    pub bit_depth: BitDepth,
    /// JPEG (and WebP if it were lossy) quality 1..=100. Ignored otherwise.
    pub quality: u8,
    pub resize: ResizeSpec,
    pub copy_exif: bool,
    pub embed_icc: bool,
    pub strip_metadata: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ExportFormat::Jpeg,
            output_space: WorkingSpace::Srgb, // web-safe default (§8.2)
            bit_depth: BitDepth::Eight,
            quality: 90,
            resize: ResizeSpec::None,
            copy_exif: true,
            embed_icc: true,
            strip_metadata: false,
        }
    }
}

impl ExportOptions {
    /// The bit depth actually used: `Sixteen` only when the format supports it,
    /// else `Eight` (spec §8.2).
    pub fn effective_bit_depth(&self) -> BitDepth {
        match self.bit_depth {
            BitDepth::Sixteen if self.format.supports_16bit() => BitDepth::Sixteen,
            _ => BitDepth::Eight,
        }
    }
}
```

- [ ] **Step 6: Create `ferrolite-export/src/lib.rs`**

```rust
//! ferrolite-export — the photo-tier encode core. Renders the full-res edited
//! image TILED via the Spec 2 GPU tile producer (no whole-image RGBA16F),
//! converts working→output via ferrolite-color, optionally resizes, and encodes
//! JPEG/PNG/TIFF/WebP with EXIF copy + embedded ICC. Runs on ferrolite-jobs at
//! Background priority (spec §8).

mod convert;
mod encode;
mod error;
mod metadata;
mod options;
mod render;
mod resize;

pub mod job;

pub use error::ExportError;
pub use job::{run_export, ExportOutcome, ExportRequest};
pub use options::{BitDepth, ExportFormat, ExportOptions, ResizeSpec};
pub use render::{render_tiled, PixelData, RenderedImage};
```

> `convert`/`encode`/`metadata`/`resize` stay private modules; their unit tests live inside them (unit tests can reach private items). `job`, `error`, `options`, `render` are public.

- [ ] **Step 7: Build + test the scaffold**

Run: `cargo test -p ferrolite-export options`
Expected: the module tree compiles only after Tasks 4–9 add the private modules. To keep this task self-contained, create **empty stubs** now so `lib.rs` compiles:

```bash
printf '// filled in Task 5\n' > ferrolite-export/src/convert.rs
printf '// filled in Task 7\n' > ferrolite-export/src/encode.rs
printf '// filled in Task 8\n' > ferrolite-export/src/metadata.rs
printf '// filled in Task 4\n' > ferrolite-export/src/resize.rs
```
and a minimal `render.rs` + `job.rs` so `pub use` resolves:
```bash
cat > ferrolite-export/src/render.rs <<'RS'
//! Filled in Task 6.
#[derive(Debug, Clone)]
pub enum PixelData {
    Eight(Vec<u8>),
    Sixteen(Vec<u16>),
}
#[derive(Debug, Clone)]
pub struct RenderedImage {
    pub width: u32,
    pub height: u32,
    pub data: PixelData,
}
RS
cat > ferrolite-export/src/job.rs <<'RS'
//! Filled in Task 9.
RS
```
Then add a temporary `pub fn render_tiled` stub is **not** needed yet — remove `render_tiled` and `run_export`/`ExportOutcome`/`ExportRequest` from the `pub use` in `lib.rs` **for this task only**, restoring them in Tasks 6/9. Simpler: comment those two `pub use` lines with `// TODO: Task 6/9` now.

Run: `cargo test -p ferrolite-export options`
Expected: PASS (4 options tests).

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml ferrolite-export/
git commit -m "feat(export): scaffold ferrolite-export crate + options model"
```

---

## Task 4: Resize — pure dims math + `fast_image_resize` apply

**Files:**
- Modify: `ferrolite-export/src/resize.rs`

**Interfaces:**
- Consumes: `ResizeSpec`, `BitDepth`, `fast_image_resize`.
- Produces:
  - `pub(crate) fn resize_dims(spec: ResizeSpec, w: u32, h: u32) -> (u32, u32)` — target size (never zero; identity for `None`).
  - `pub(crate) fn apply_resize(rgb: &[u8], w: u32, h: u32, tw: u32, th: u32, depth: BitDepth) -> Result<Vec<u8>, ExportError>` — resize an interleaved RGB buffer (`U8x3` or `U16x3`, bytes); returns the resized bytes. No-op fast path when `(w,h) == (tw,th)`.

- [ ] **Step 1: Write failing tests**

Replace `ferrolite-export/src/resize.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::ResizeSpec;

    #[test]
    fn none_is_identity() {
        assert_eq!(resize_dims(ResizeSpec::None, 6000, 4000), (6000, 4000));
    }

    #[test]
    fn long_edge_preserves_aspect() {
        // Landscape 6000x4000, long edge 1200 -> 1200x800.
        assert_eq!(resize_dims(ResizeSpec::LongEdge(1200), 6000, 4000), (1200, 800));
        // Portrait 4000x6000, long edge 1200 -> 800x1200.
        assert_eq!(resize_dims(ResizeSpec::LongEdge(1200), 4000, 6000), (800, 1200));
    }

    #[test]
    fn exact_is_verbatim() {
        assert_eq!(resize_dims(ResizeSpec::Exact { w: 1024, h: 768 }, 6000, 4000), (1024, 768));
    }

    #[test]
    fn percent_scales_both_axes() {
        assert_eq!(resize_dims(ResizeSpec::Percent(0.5), 6000, 4000), (3000, 2000));
        assert_eq!(resize_dims(ResizeSpec::Percent(0.25), 800, 600), (200, 150));
    }

    #[test]
    fn dims_never_zero() {
        assert_eq!(resize_dims(ResizeSpec::Percent(0.0001), 100, 100), (1, 1));
        assert_eq!(resize_dims(ResizeSpec::LongEdge(0), 100, 50), (1, 1));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-export resize`
Expected: FAIL to compile — `cannot find function resize_dims`.

- [ ] **Step 3: Implement `resize_dims` + `apply_resize` (above the test module)**

```rust
//! Optional output resize. Dims math is pure/tested; the pixel resample uses
//! `fast_image_resize` (same crate the thumbnailer uses) over the quantized RGB
//! buffer. Quality is secondary (spec §1), so resampling the encoded RGB rather
//! than linear light is acceptable.

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use crate::error::ExportError;
use crate::options::{BitDepth, ResizeSpec};

/// Target dimensions for a resize spec applied to a `w × h` image. Never returns
/// a zero axis (clamps to 1).
pub(crate) fn resize_dims(spec: ResizeSpec, w: u32, h: u32) -> (u32, u32) {
    let (tw, th) = match spec {
        ResizeSpec::None => (w, h),
        ResizeSpec::Exact { w: ew, h: eh } => (ew, eh),
        ResizeSpec::LongEdge(px) => {
            let long = w.max(h) as f64;
            if long == 0.0 {
                (w, h)
            } else {
                let s = px as f64 / long;
                ((w as f64 * s).round() as u32, (h as f64 * s).round() as u32)
            }
        }
        ResizeSpec::Percent(p) => {
            ((w as f64 * p as f64).round() as u32, (h as f64 * p as f64).round() as u32)
        }
    };
    (tw.max(1), th.max(1))
}

/// Resize an interleaved RGB byte buffer to `tw × th`. `depth` selects the pixel
/// type (`U8x3` / `U16x3`). No-op (clone) when the size is unchanged.
pub(crate) fn apply_resize(
    rgb: &[u8],
    w: u32,
    h: u32,
    tw: u32,
    th: u32,
    depth: BitDepth,
) -> Result<Vec<u8>, ExportError> {
    if (w, h) == (tw, th) {
        return Ok(rgb.to_vec());
    }
    let pt = match depth {
        BitDepth::Eight => PixelType::U8x3,
        BitDepth::Sixteen => PixelType::U16x3,
    };
    let src = Image::from_vec_u8(w, h, rgb.to_vec(), pt)
        .map_err(|e| ExportError::Encode(format!("resize src: {e}")))?;
    let mut dst = Image::new(tw, th, pt);
    let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    Resizer::new()
        .resize(&src, &mut dst, &opts)
        .map_err(|e| ExportError::Encode(format!("resize: {e}")))?;
    Ok(dst.buffer().to_vec())
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ferrolite-export resize`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-export/src/resize.rs
git commit -m "feat(export): resize dims math + fast_image_resize apply"
```

---

## Task 5: Convert — working→output matrix + OETF + quantize (pure)

**Files:**
- Modify: `ferrolite-export/src/convert.rs`

**Interfaces:**
- Consumes: `ferrolite_color::{Mat3, mul_vec3, output_oetf, WorkingSpace}`.
- Produces:
  - `pub(crate) fn convert_pixel(rgb_lin: [f32; 3], m: &ferrolite_color::Mat3, out: WorkingSpace) -> [f32; 3]` — matrix-multiply working-linear RGB by `m` (working→output), clamp to `[0,1]`, apply `output_oetf`.
  - `pub(crate) fn to_u8(encoded: [f32; 3]) -> [u8; 3]`.
  - `pub(crate) fn to_u16(encoded: [f32; 3]) -> [u16; 3]`.

- [ ] **Step 1: Write failing tests**

Replace `ferrolite-export/src/convert.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_color::{identity, srgb_oetf, WorkingSpace};

    #[test]
    fn identity_matrix_srgb_is_just_oetf() {
        let m = identity(); // working==output==sRGB -> working_to_output is identity
        let out = convert_pixel([0.5, 0.25, 0.0], &m, WorkingSpace::Srgb);
        assert!((out[0] - srgb_oetf(0.5)).abs() < 1e-5);
        assert!((out[1] - srgb_oetf(0.25)).abs() < 1e-5);
        assert!((out[2] - srgb_oetf(0.0)).abs() < 1e-5);
    }

    #[test]
    fn clamps_out_of_gamut_before_oetf() {
        let m = identity();
        // A negative and a >1 channel clamp to [0,1] endpoints.
        let out = convert_pixel([-0.2, 2.0, 0.5], &m, WorkingSpace::Srgb);
        assert_eq!(out[0], 0.0);
        assert!((out[1] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn quantizers_round_and_clamp() {
        assert_eq!(to_u8([0.0, 1.0, 0.5]), [0, 255, 128]);
        assert_eq!(to_u8([-1.0, 2.0, 0.5]), [0, 255, 128]);
        assert_eq!(to_u16([0.0, 1.0, 0.5]), [0, 65535, 32768]);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-export convert`
Expected: FAIL to compile.

- [ ] **Step 3: Implement (above the test module)**

```rust
//! Pure per-pixel output conversion: working-linear RGB → output-space encoded
//! RGB. The 3×3 (working→output) and the output OETF both come from
//! ferrolite-color (spec §8.1). No GPU, fully unit-tested.

use ferrolite_color::{mul_vec3, output_oetf, Mat3, WorkingSpace};

/// Apply the working→output 3×3, clamp to `[0,1]`, then the output OETF.
pub(crate) fn convert_pixel(rgb_lin: [f32; 3], m: &Mat3, out: WorkingSpace) -> [f32; 3] {
    let lin = mul_vec3(m, &rgb_lin);
    [
        output_oetf(out, lin[0]),
        output_oetf(out, lin[1]),
        output_oetf(out, lin[2]),
    ]
}

/// Quantize an encoded (0..1) RGB triple to 8-bit, rounding + clamping.
pub(crate) fn to_u8(encoded: [f32; 3]) -> [u8; 3] {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [q(encoded[0]), q(encoded[1]), q(encoded[2])]
}

/// Quantize an encoded (0..1) RGB triple to 16-bit, rounding + clamping.
pub(crate) fn to_u16(encoded: [f32; 3]) -> [u16; 3] {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    [q(encoded[0]), q(encoded[1]), q(encoded[2])]
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p ferrolite-export convert`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-export/src/convert.rs
git commit -m "feat(export): pure working→output convert + quantize"
```

---

## Task 6: `render_tiled` — GPU tiled render + per-tile readback + assemble

The core. Build a `TileEditPipeline` from the resident pyramid, render each output tile, read it back, convert per pixel, and place it into the final quantized RGB buffer. Never allocates a whole-image f32/RGBA16F CPU buffer.

**Files:**
- Modify: `ferrolite-export/src/render.rs`
- Create: `ferrolite-export/tests/render_golden.rs`

**Interfaces:**
- Consumes: `ferrolite_pipeline::{TileEditPipeline, GpuPyramidSource, OpStack, edited_output_dims}`, `ferrolite_gpu::GpuContext`, `ferrolite_image::{TileCoord, TILE_SIZE, tile_pixel_origin}`, `ferrolite_color::{working_to_output, WorkingSpace}`, `ferrolite_jobs::CancelToken`, `crate::convert`, `crate::options::BitDepth`, `half::f16`.
- Produces:
  - `pub enum PixelData { Eight(Vec<u8>), Sixteen(Vec<u16>) }` — interleaved **RGB** (3 channels).
  - `pub struct RenderedImage { pub width: u32, pub height: u32, pub data: PixelData }`.
  - `pub fn render_tiled(ctx: &std::sync::Arc<GpuContext>, pyramid: &std::sync::Arc<GpuPyramidSource>, stack: &OpStack, camera_to_working: [[f32; 3]; 3], working_space: WorkingSpace, output_space: WorkingSpace, depth: BitDepth, cancel: &CancelToken, progress: &mut dyn FnMut(u32, u32)) -> Result<RenderedImage, ExportError>`.

- [ ] **Step 1: Replace the `render.rs` stub with the real types + a private tile readback**

```rust
//! Full-res tiled export render. Reuses the Spec 2 GPU tile producer
//! (`TileEditPipeline::produce_tile`) to render the edited image one
//! `TILE_SIZE²` tile at a time, reads each tile back to the CPU, converts
//! working→output + OETF, and quantizes into the final RGB buffer. No
//! whole-image RGBA16F/f32 CPU buffer is ever allocated (CLAUDE.md §1/§2;
//! spec §8.1).

use std::sync::Arc;

use ferrolite_color::{working_to_output, WorkingSpace};
use ferrolite_gpu::GpuContext;
use ferrolite_image::{tile_pixel_origin, TileCoord, TILE_SIZE};
use ferrolite_jobs::CancelToken;
use ferrolite_pipeline::{edited_output_dims, GpuPyramidSource, OpStack, TileEditPipeline};
use half::f16;

use crate::convert::{convert_pixel, to_u16, to_u8};
use crate::error::ExportError;
use crate::options::BitDepth;

#[derive(Debug, Clone)]
pub enum PixelData {
    Eight(Vec<u8>),
    Sixteen(Vec<u16>),
}

#[derive(Debug, Clone)]
pub struct RenderedImage {
    pub width: u32,
    pub height: u32,
    pub data: PixelData,
}

/// Read a `TILE_SIZE²` `Rgba16Float` tile texture (COPY_SRC) back to the CPU as
/// f32 RGBA (row-unpadded, len = TILE_SIZE*TILE_SIZE*4). Blocks on the device.
fn read_tile_rgba16f(ctx: &GpuContext, tex: &wgpu::Texture) -> Vec<f32> {
    let dim = TILE_SIZE;
    let channels = 4u32;
    let bpp = 2u32; // f16
    let bpr_unpadded = dim * channels * bpp;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let bpr_padded = bpr_unpadded.div_ceil(align) * align;

    let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("export-tile-readback"),
        size: (bpr_padded * dim) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buf,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(bpr_padded),
                rows_per_image: Some(dim),
            },
        },
        wgpu::Extent3d {
            width: dim,
            height: dim,
            depth_or_array_layers: 1,
        },
    );
    ctx.queue.submit([enc.finish()]);

    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    ctx.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();

    let mut out = Vec::with_capacity((dim * dim * channels) as usize);
    for row in 0..dim {
        let start = (row * bpr_padded) as usize;
        let end = start + (bpr_unpadded) as usize;
        let row_u16: &[u16] = bytemuck::cast_slice(&data[start..end]);
        for &h in row_u16 {
            out.push(f16::from_bits(h).to_f32());
        }
    }
    drop(data);
    buf.unmap();
    out
}
```

- [ ] **Step 2: Implement `render_tiled` (append to `render.rs`)**

```rust
/// Render the full-res edited image to a quantized RGB buffer, tile by tile.
/// `camera_to_working` is the row-major 3×3 for the open image + working space
/// (from the app's `camera_to_working()`); `working_space`→`output_space` drives
/// the output conversion. Checks `cancel` once per tile and reports `(done,total)`.
pub fn render_tiled(
    ctx: &Arc<GpuContext>,
    pyramid: &Arc<GpuPyramidSource>,
    stack: &OpStack,
    camera_to_working: [[f32; 3]; 3],
    working_space: WorkingSpace,
    output_space: WorkingSpace,
    depth: BitDepth,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(u32, u32),
) -> Result<RenderedImage, ExportError> {
    let (src_w, src_h) = pyramid.level_size(0);
    let (out_w, out_h) = edited_output_dims(stack, src_w, src_h);
    if out_w == 0 || out_h == 0 {
        return Err(ExportError::Render("zero output dimensions".into()));
    }

    // Build the per-tile edit pipeline ONCE for this export (CLAUDE.md GPU rule).
    let mut pipeline =
        TileEditPipeline::new(ctx.clone(), pyramid.clone(), stack.clone(), camera_to_working);

    let m = working_to_output(working_space, output_space); // ferrolite_color::Mat3

    let tiles_x = out_w.div_ceil(TILE_SIZE);
    let tiles_y = out_h.div_ceil(TILE_SIZE);
    let total = tiles_x * tiles_y;

    // Final quantized RGB buffer (3 or 6 bytes/px). This is the only full-image
    // CPU allocation — no whole-image f32/RGBA16F.
    let px_count = (out_w * out_h) as usize;
    let mut buf8: Vec<u8> = Vec::new();
    let mut buf16: Vec<u16> = Vec::new();
    match depth {
        BitDepth::Eight => buf8 = vec![0u8; px_count * 3],
        BitDepth::Sixteen => buf16 = vec![0u16; px_count * 3],
    }

    let mut done = 0u32;
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            if cancel.is_cancelled() {
                return Err(ExportError::Cancelled);
            }
            let coord = TileCoord { lod: 0, x: tx, y: ty };
            let tile_tex = pipeline.produce_tile(coord);
            let rgba = read_tile_rgba16f(ctx, &tile_tex); // len TILE_SIZE²*4 f32

            let (ox, oy) = tile_pixel_origin(coord); // interior top-left in output
            for row in 0..TILE_SIZE {
                let py = oy + row;
                if py >= out_h {
                    break;
                }
                for col in 0..TILE_SIZE {
                    let px = ox + col;
                    if px >= out_w {
                        break;
                    }
                    let ti = ((row * TILE_SIZE + col) * 4) as usize;
                    let rgb_lin = [rgba[ti], rgba[ti + 1], rgba[ti + 2]];
                    let enc = convert_pixel(rgb_lin, &m, output_space);
                    let di = ((py * out_w + px) * 3) as usize;
                    match depth {
                        BitDepth::Eight => {
                            let q = to_u8(enc);
                            buf8[di] = q[0];
                            buf8[di + 1] = q[1];
                            buf8[di + 2] = q[2];
                        }
                        BitDepth::Sixteen => {
                            let q = to_u16(enc);
                            buf16[di] = q[0];
                            buf16[di + 1] = q[1];
                            buf16[di + 2] = q[2];
                        }
                    }
                }
            }
            done += 1;
            progress(done, total);
        }
    }

    let data = match depth {
        BitDepth::Eight => PixelData::Eight(buf8),
        BitDepth::Sixteen => PixelData::Sixteen(buf16),
    };
    Ok(RenderedImage {
        width: out_w,
        height: out_h,
        data,
    })
}
```

- [ ] **Step 3: Restore the `pub use render::{render_tiled, ...}` in `lib.rs`**

Uncomment the `pub use render::{render_tiled, PixelData, RenderedImage};` line (the `run_export`/`ExportRequest`/`ExportOutcome` re-export stays commented until Task 9).

Run: `cargo build -p ferrolite-export`
Expected: compiles.

- [ ] **Step 4: Write the tiled-vs-whole-image GPU golden + cancellation test**

Create `ferrolite-export/tests/render_golden.rs`:

```rust
//! GPU goldens for the tiled export render. Auto-skip headless. Proves the
//! tile-by-tile render + convert matches a whole-image reference (tile-seam
//! correctness reusing the Spec 2 halo), and that cancellation stops the render.

use std::sync::Arc;

use ferrolite_color::WorkingSpace;
use ferrolite_export::{render_tiled, BitDepth, PixelData};
use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_jobs::CancelToken;
use ferrolite_pipeline::{EditPipeline, GpuPyramidSource, OpStack};

const TOL: i32 = 6; // absorbs f16 + tile-edge resample (Spec 2 SEAM_TOL rationale)

fn probe(w: u32, h: u32) -> LinearRgbaF32 {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[
                (x as f32 / w as f32),
                (y as f32 / h as f32),
                0.35,
                1.0,
            ]);
        }
    }
    LinearRgbaF32::new(w, h, px).unwrap()
}

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

#[test]
fn tiled_render_matches_whole_image_reference() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // Non-tile-aligned dims so edge tiles are exercised.
    let (w, h) = (600u32, 500u32);
    let img = probe(w, h);
    let ctx = Arc::new(ctx);

    // Whole-image reference: preview EditPipeline (uploads whole image; fine in a
    // small test) with sRGB working/output so the tail == the export convert path.
    let mut ep = EditPipeline::new(ctx.clone(), &img, OpStack::default(), IDENTITY);
    let reference = ep.render_to_image(); // sRGB Rgba8, w×h, row-unpadded

    // Tiled export render, sRGB working -> sRGB output, 8-bit RGB.
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &img));
    let cancel = CancelToken::new();
    let mut seen = (0u32, 0u32);
    let out = render_tiled(
        &ctx,
        &pyramid,
        &OpStack::default(),
        IDENTITY,
        WorkingSpace::Srgb,
        WorkingSpace::Srgb,
        BitDepth::Eight,
        &cancel,
        &mut |d, t| seen = (d, t),
    )
    .expect("render");

    assert_eq!((out.width, out.height), (w, h));
    assert_eq!(seen.0, seen.1, "progress reached 100%");
    let PixelData::Eight(rgb) = out.data else {
        panic!("expected 8-bit")
    };
    // Compare RGB (export) vs RGBA (reference) channel-by-channel within tolerance.
    let mut max_diff = 0i32;
    for i in 0..(w * h) as usize {
        for c in 0..3 {
            let a = rgb[i * 3 + c] as i32;
            let b = reference[i * 4 + c] as i32;
            max_diff = max_diff.max((a - b).abs());
        }
    }
    assert!(max_diff <= TOL, "tiled vs whole-image max channel diff {max_diff} > {TOL}");
}

#[test]
fn cancellation_stops_render() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let ctx = Arc::new(ctx);
    let img = probe(600, 500);
    let pyramid = Arc::new(GpuPyramidSource::new(&ctx, &img));
    let cancel = CancelToken::new();
    cancel.cancel(); // pre-cancelled -> first tile check returns Cancelled
    let r = render_tiled(
        &ctx,
        &pyramid,
        &OpStack::default(),
        IDENTITY,
        WorkingSpace::Srgb,
        WorkingSpace::Srgb,
        BitDepth::Eight,
        &cancel,
        &mut |_, _| {},
    );
    assert!(matches!(r, Err(ferrolite_export::ExportError::Cancelled)));
}
```

> `EditPipeline::render_to_image` and `EditPipeline::new(.., IDENTITY)` are the Plan 1/2 signatures (see `ferrolite-pipeline/tests/color_golden.rs`). If `render_to_image` is named differently in the current tree, grep `fn render_to_image` in `ferrolite-pipeline/src/pipeline.rs` and match it.

- [ ] **Step 5: Add `ferrolite-pipeline` + `ferrolite-image` as export dev-deps for the golden**

In `ferrolite-export/Cargo.toml` add:

```toml
[dev-dependencies]
ferrolite-pipeline = { workspace = true }
ferrolite-image = { workspace = true }
ferrolite-gpu = { workspace = true }
ferrolite-color = { workspace = true }
ferrolite-jobs = { workspace = true }
```

(They are already normal deps; dev-deps make them usable from `tests/`. If cargo warns about duplication, the `[dev-dependencies]` block can be omitted since normal deps are visible to integration tests — verify with the build.)

- [ ] **Step 6: Run the golden (dev GPU) + full unit suite**

Run: `cargo test -p ferrolite-export`
Expected: on the dev GPU both golden tests PASS; on headless they print "skipping" and pass. Unit tests (options/resize/convert) PASS.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-export/src/render.rs ferrolite-export/src/lib.rs \
  ferrolite-export/Cargo.toml ferrolite-export/tests/render_golden.rs
git commit -m "feat(export): tiled full-res render + per-tile readback + convert"
```

---

## Task 7: Encode — JPEG/PNG/TIFF/WebP + embedded ICC

**Files:**
- Modify: `ferrolite-export/src/encode.rs`
- Create: `ferrolite-export/tests/encode_roundtrip.rs`

**Interfaces:**
- Consumes: `RenderedImage`/`PixelData`, `ExportFormat`, `ExportOptions`, `ferrolite_color::emit_icc`, the `image` crate encoders + `ImageEncoder`/`ExtendedColorType`.
- Produces:
  - `pub(crate) fn encode_to_file(img: &RenderedImage, opts: &ExportOptions, dest: &std::path::Path) -> Result<Vec<String>, ExportError>` — writes the file; returns non-fatal **warnings** (e.g. "ICC not embedded"). ICC is emitted from `opts.output_space` when `opts.embed_icc`.

- [ ] **Step 1: Implement `encode.rs`**

```rust
//! Encode a `RenderedImage` to a file in the chosen format, embedding the output
//! ICC profile where the format supports it (via `ImageEncoder::set_icc_profile`).
//! Best-effort ICC + never-panic per spec §10: a failed ICC step downgrades to an
//! untagged (but valid) file plus a warning.

use std::io::BufWriter;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::tiff::TiffEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder};

use crate::error::ExportError;
use crate::options::{BitDepth, ExportFormat, ExportOptions};
use crate::render::{PixelData, RenderedImage};

pub(crate) fn encode_to_file(
    img: &RenderedImage,
    opts: &ExportOptions,
    dest: &Path,
) -> Result<Vec<String>, ExportError> {
    let mut warnings = Vec::new();

    // Emit the output ICC once (best-effort).
    let icc: Option<Vec<u8>> = if opts.embed_icc {
        match ferrolite_color::emit_icc(opts.output_space) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                warnings.push(format!("ICC profile not embedded (emit failed: {e})"));
                None
            }
        }
    } else {
        None
    };

    let (w, h) = (img.width, img.height);
    let (bytes, color): (&[u8], ExtendedColorType) = match &img.data {
        PixelData::Eight(v) => (v.as_slice(), ExtendedColorType::Rgb8),
        PixelData::Sixteen(v) => (bytemuck::cast_slice(v), ExtendedColorType::Rgb16),
    };

    let file = std::fs::File::create(dest).map_err(|e| ExportError::Io(e.to_string()))?;
    let mut out = BufWriter::new(file);

    // Each encoder: create, best-effort set ICC, then write_image (consumes self).
    macro_rules! set_icc {
        ($enc:expr) => {{
            if let Some(ref profile) = icc {
                if let Err(e) = $enc.set_icc_profile(profile.clone()) {
                    warnings.push(format!("ICC not embedded for this format: {e}"));
                }
            }
        }};
    }

    match opts.format {
        ExportFormat::Jpeg => {
            let mut enc = JpegEncoder::new_with_quality(&mut out, opts.quality);
            set_icc!(enc);
            enc.write_image(bytes, w, h, color)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::Png => {
            let mut enc = PngEncoder::new(&mut out);
            set_icc!(enc);
            enc.write_image(bytes, w, h, color)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::Tiff => {
            // TiffEncoder needs Seek; BufWriter<File> is Seek.
            let mut enc = TiffEncoder::new(&mut out).map_err(|e| ExportError::Encode(e.to_string()))?;
            set_icc!(enc);
            enc.write_image(bytes, w, h, color)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::WebP => {
            // Lossless only (spec §2). Force 8-bit RGB.
            let mut enc = WebPEncoder::new_lossless(&mut out);
            set_icc!(enc);
            enc.write_image(bytes, w, h, color)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
    }

    // Silence an unused-variant warning if 16-bit is somehow paired with an 8-bit
    // format (effective_bit_depth prevents this upstream).
    let _ = BitDepth::Eight;
    Ok(warnings)
}
```

> `TiffEncoder::new` returns a `Result`; the other three constructors do not. If the current `image` version differs, grep the vendored source at `~/.cargo/registry/src/*/image-0.25.*/src/codecs/` and match the exact constructor return types.

- [ ] **Step 2: Write the per-format round-trip + ICC test**

Create `ferrolite-export/tests/encode_roundtrip.rs` (pure CPU — runs everywhere):

```rust
//! Encode → decode round-trips per format, and an ICC-present check for a format
//! that embeds it (PNG). CPU-only; no GPU.

use ferrolite_export::{render_tiled, BitDepth, ExportFormat, ExportOptions, PixelData};

// Small helper: build a RenderedImage directly (bypass GPU) via a public shim.
// render.rs types are public, so construct one here for encode tests.
fn solid_rgb8(w: u32, h: u32, rgb: [u8; 3]) -> ferrolite_export::RenderedImage {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for _ in 0..(w * h) {
        v.extend_from_slice(&rgb);
    }
    ferrolite_export::RenderedImage {
        width: w,
        height: h,
        data: PixelData::Eight(v),
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ferrolite-export-test-{name}"))
}

#[test]
fn roundtrip_each_format_within_tolerance() {
    let img = solid_rgb8(32, 24, [200, 100, 40]);
    for (fmt, ext) in [
        (ExportFormat::Jpeg, "jpg"),
        (ExportFormat::Png, "png"),
        (ExportFormat::Tiff, "tif"),
        (ExportFormat::WebP, "webp"),
    ] {
        let dest = tmp(&format!("rt.{ext}"));
        let opts = ExportOptions {
            format: fmt,
            embed_icc: false, // isolate pixel round-trip from ICC
            ..Default::default()
        };
        // encode via the crate's public API path: reuse encode by exporting it.
        ferrolite_export::encode_for_test(&img, &opts, &dest).expect("encode");
        let decoded = image::open(&dest).expect("decode").to_rgb8();
        assert_eq!(decoded.dimensions(), (32, 24), "{fmt:?} dims");
        // JPEG is lossy; allow a wide tolerance. Others lossless.
        let tol = if matches!(fmt, ExportFormat::Jpeg) { 12 } else { 0 };
        let p = decoded.get_pixel(4, 4).0;
        for c in 0..3 {
            assert!(
                (p[c] as i32 - [200, 100, 40][c] as i32).abs() <= tol,
                "{fmt:?} ch {c}: {} vs {}",
                p[c],
                [200, 100, 40][c]
            );
        }
        let _ = std::fs::remove_file(&dest);
    }
}

#[test]
fn png_embeds_icc_profile() {
    let img = solid_rgb8(16, 16, [128, 128, 128]);
    let dest = tmp("icc.png");
    let opts = ExportOptions {
        format: ExportFormat::Png,
        output_space: ferrolite_color::WorkingSpace::Srgb,
        embed_icc: true,
        ..Default::default()
    };
    ferrolite_export::encode_for_test(&img, &opts, &dest).expect("encode");

    // Reopen with the PNG decoder and read the ICC chunk back.
    use image::ImageDecoder;
    let file = std::fs::File::open(&dest).unwrap();
    let mut dec = image::codecs::png::PngDecoder::new(std::io::BufReader::new(file)).unwrap();
    let icc = dec.icc_profile().unwrap();
    assert!(icc.is_some_and(|p| !p.is_empty()), "PNG should carry an ICC profile");
    let _ = std::fs::remove_file(&dest);
}

// keep the render import referenced so the crate compiles even if unused here
#[allow(unused_imports)]
use render_tiled as _render_tiled;
```

- [ ] **Step 3: Expose a test-only encode shim**

Because `encode_to_file` is `pub(crate)`, add a small public wrapper for integration tests in `ferrolite-export/src/lib.rs`:

```rust
/// Test-only re-export of the internal encoder so integration tests can encode a
/// `RenderedImage` without going through the GPU render path.
#[doc(hidden)]
pub fn encode_for_test(
    img: &RenderedImage,
    opts: &ExportOptions,
    dest: &std::path::Path,
) -> Result<Vec<String>, ExportError> {
    crate::encode::encode_to_file(img, opts, dest)
}
```

(Add `use crate::render::RenderedImage;` at the top of `lib.rs` if needed, or reference the re-exported path. Keep `mod encode;` declared.)

- [ ] **Step 4: Run the round-trip suite**

Run: `cargo test -p ferrolite-export --test encode_roundtrip`
Expected: PASS (2 tests) on every OS (no GPU needed).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-export/src/encode.rs ferrolite-export/src/lib.rs \
  ferrolite-export/tests/encode_roundtrip.rs
git commit -m "feat(export): JPEG/PNG/TIFF/WebP encode + embedded ICC"
```

---

## Task 8: Metadata — copy source EXIF with `little_exif`

**Files:**
- Modify: `ferrolite-export/src/metadata.rs`

**Interfaces:**
- Produces: `pub(crate) fn copy_exif(source: &std::path::Path, dest: &std::path::Path) -> Result<(), String>` — reads EXIF from `source`, writes it into the already-encoded `dest`. Returns `Err(String)` (non-fatal; the caller records it as a warning).

- [ ] **Step 1: Implement `metadata.rs`**

```rust
//! Copy source EXIF into the exported file (spec §8.1). Best-effort: any failure
//! is returned as a message the orchestrator records as a warning (never fatal,
//! never panics; spec §10). Path-based read/write lets little_exif infer the
//! container format from the extension.

use std::path::Path;

use little_exif::metadata::Metadata;

/// Read EXIF from `source` and write it into `dest` (which must already exist as a
/// valid encoded image). Returns `Err` with a human message on any failure.
pub(crate) fn copy_exif(source: &Path, dest: &Path) -> Result<(), String> {
    let meta = Metadata::new_from_path(source)
        .map_err(|e| format!("read source EXIF: {e}"))?;
    meta.write_to_file(dest)
        .map_err(|e| format!("write EXIF to output: {e}"))?;
    Ok(())
}
```

- [ ] **Step 2: Write a round-trip test (CPU)**

Append a `#[cfg(test)] mod tests` to `metadata.rs`. Build a tiny JPEG source that carries one EXIF tag, then export a plain JPEG and copy EXIF into it, and confirm the tag survives:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use little_exif::exif_tag::ExifTag;
    use little_exif::filetype::FileExtension;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ferrolite-exif-test-{name}"))
    }

    #[test]
    fn copies_a_tag_from_source_to_dest() {
        // Source: a minimal JPEG with an EXIF ImageDescription tag.
        let src = tmp("src.jpg");
        let dst = tmp("dst.jpg");
        // Write two solid JPEGs via the image crate (both valid JPEG containers).
        let buf = image::RgbImage::from_pixel(8, 8, image::Rgb([10, 20, 30]));
        buf.save(&src).unwrap();
        buf.save(&dst).unwrap();

        // Tag the source.
        let mut m = Metadata::new();
        m.set_tag(ExifTag::ImageDescription("ferrolite-test".to_string()));
        m.write_to_file(&src).unwrap();
        let _ = FileExtension::JPEG; // ensure the enum is linked

        // Copy EXIF source -> dest and read it back.
        copy_exif(&src, &dst).expect("copy");
        let back = Metadata::new_from_path(&dst).expect("read back");
        let found = back.get_tag(&ExifTag::ImageDescription(String::new())).next().is_some();
        assert!(found, "ImageDescription should have been copied");

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
    }
}
```

> The exact `little_exif` API (`Metadata::new`, `set_tag`, `get_tag`, `ExifTag::ImageDescription`) is version-sensitive. Before implementing, confirm signatures in the vendored source (`~/.cargo/registry/src/*/little_exif-0.6.*/src/`) or `https://docs.rs/little_exif/0.6.23`. If `get_tag`/`set_tag` differ, adapt the test to the real accessor names — the production `copy_exif` only uses `new_from_path` + `write_to_file`, which are stable.

- [ ] **Step 2b: Run to verify**

Run: `cargo test -p ferrolite-export metadata`
Expected: PASS. If the tag-accessor API mismatches and the test cannot be made to compile quickly, downgrade the test to assert only that `copy_exif` returns `Ok(())` on two valid JPEGs (still exercises read+write), and note the limitation in a comment.

- [ ] **Step 3: Commit**

```bash
git add ferrolite-export/src/metadata.rs
git commit -m "feat(export): copy source EXIF into output via little_exif"
```

---

## Task 9: `run_export` orchestrator

Tie render → resize → encode → EXIF into one call the job closure invokes.

**Files:**
- Modify: `ferrolite-export/src/job.rs`
- Modify: `ferrolite-export/src/lib.rs` (restore the `pub use job::{...}`)

**Interfaces:**
- Consumes: `render_tiled`, `crate::resize::{resize_dims, apply_resize}`, `crate::encode::encode_to_file`, `crate::metadata::copy_exif`, `ExportOptions`, `GpuContext`, `GpuPyramidSource`, `OpStack`, `CancelToken`, `ferrolite_color::WorkingSpace`.
- Produces:
  - `pub struct ExportRequest<'a> { ctx, pyramid, stack, camera_to_working, working_space, options, dest, source_path }` (borrows).
  - `pub struct ExportOutcome { pub dest: std::path::PathBuf, pub warnings: Vec<String> }`.
  - `pub fn run_export(req: ExportRequest, cancel: &CancelToken, progress: &mut dyn FnMut(u32, u32)) -> Result<ExportOutcome, ExportError>`.

- [ ] **Step 1: Implement `job.rs`**

```rust
//! The export orchestrator: render (tiled) → optional resize → encode (+ICC) →
//! copy EXIF. Called from a ferrolite-jobs Background closure (spec §8.1). All
//! GPU work uses the passed shared `Arc<GpuContext>` on the worker thread; the
//! pipeline is built and dropped inside `render_tiled` (it is !Send).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferrolite_color::WorkingSpace;
use ferrolite_gpu::GpuContext;
use ferrolite_jobs::CancelToken;
use ferrolite_pipeline::{GpuPyramidSource, OpStack};

use crate::encode::encode_to_file;
use crate::error::ExportError;
use crate::metadata::copy_exif;
use crate::options::ExportOptions;
use crate::render::{render_tiled, PixelData, RenderedImage};
use crate::resize::{apply_resize, resize_dims};

pub struct ExportRequest<'a> {
    pub ctx: &'a Arc<GpuContext>,
    pub pyramid: &'a Arc<GpuPyramidSource>,
    pub stack: &'a OpStack,
    /// Row-major camera→working 3×3 for the open image + working space.
    pub camera_to_working: [[f32; 3]; 3],
    pub working_space: WorkingSpace,
    pub options: &'a ExportOptions,
    pub dest: &'a Path,
    /// Source image path for EXIF copy.
    pub source_path: &'a Path,
}

#[derive(Debug, Clone)]
pub struct ExportOutcome {
    pub dest: PathBuf,
    pub warnings: Vec<String>,
}

/// Render, resize, encode, and copy metadata for one image. `progress(done,total)`
/// reports tile progress during the render phase.
pub fn run_export(
    req: ExportRequest,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(u32, u32),
) -> Result<ExportOutcome, ExportError> {
    let opts = req.options;
    let depth = opts.effective_bit_depth();

    // 1. Tiled full-res render → quantized output-space RGB.
    let mut rendered = render_tiled(
        req.ctx,
        req.pyramid,
        req.stack,
        req.camera_to_working,
        req.working_space,
        opts.output_space,
        depth,
        cancel,
        progress,
    )?;

    if cancel.is_cancelled() {
        return Err(ExportError::Cancelled);
    }

    // 2. Optional resize (on the quantized RGB buffer).
    let (tw, th) = resize_dims(opts.resize, rendered.width, rendered.height);
    if (tw, th) != (rendered.width, rendered.height) {
        let resized = match &rendered.data {
            PixelData::Eight(v) => {
                let out = apply_resize(v, rendered.width, rendered.height, tw, th, depth)?;
                PixelData::Eight(out)
            }
            PixelData::Sixteen(v) => {
                let bytes = bytemuck::cast_slice::<u16, u8>(v);
                let out = apply_resize(bytes, rendered.width, rendered.height, tw, th, depth)?;
                PixelData::Sixteen(bytemuck::cast_slice::<u8, u16>(&out).to_vec())
            }
        };
        rendered = RenderedImage {
            width: tw,
            height: th,
            data: resized,
        };
    }

    // 3. Encode (+ ICC embed). Collect non-fatal warnings.
    let mut warnings = encode_to_file(&rendered, opts, req.dest)?;

    // 4. Copy EXIF (unless stripping). Best-effort.
    if opts.copy_exif && !opts.strip_metadata {
        if let Err(msg) = copy_exif(req.source_path, req.dest) {
            warnings.push(format!("EXIF not copied: {msg}"));
        }
    }

    Ok(ExportOutcome {
        dest: req.dest.to_path_buf(),
        warnings,
    })
}
```

- [ ] **Step 2: Restore the `pub use` in `lib.rs`**

Uncomment: `pub use job::{run_export, ExportOutcome, ExportRequest};`

Run: `cargo build -p ferrolite-export && cargo clippy -p ferrolite-export --all-targets -- -D warnings`
Expected: compiles clean.

- [ ] **Step 3: Run the whole export crate suite**

Run: `cargo test -p ferrolite-export`
Expected: unit + integration tests PASS (GPU golden runs on dev GPU, skips headless).

- [ ] **Step 4: Commit**

```bash
git add ferrolite-export/src/job.rs ferrolite-export/src/lib.rs
git commit -m "feat(export): run_export orchestrator (render→resize→encode→exif)"
```

---

## Task 10: App events + state for the export flow

**Files:**
- Modify: `ferrolite-app/Cargo.toml`
- Modify: `ferrolite-app/src/events.rs`
- Modify: `ferrolite-app/src/state.rs`
- Modify: `ferrolite-app/src/lib.rs` (module list)

**Interfaces:**
- Produces: `AppEvent::ExportProgress { image_id: i64, done: u32, total: u32 }`, `AppEvent::ExportFinished { image_id: i64, ok: bool, message: String }`; `AppState.export_dialog: Option<crate::export::ExportDialogState>`.

- [ ] **Step 1: Add the app dep + image features**

In `ferrolite-app/Cargo.toml`:
- Add `ferrolite-export = { workspace = true }`.
- Change the `image` dependency features from `["jpeg"]` to `["jpeg", "png", "tiff", "webp"]`.

Run: `cargo build -p ferrolite-app` (will fail later until `export` module exists — that's fine; this step just edits Cargo.toml).

- [ ] **Step 2: Add the events**

In `ferrolite-app/src/events.rs`, add to `enum AppEvent`:

```rust
    /// Tile progress for the running single-file export.
    ExportProgress {
        image_id: i64,
        done: u32,
        total: u32,
    },
    /// The single-file export finished (ok=false → failed/cancelled). `message`
    /// is the status-bar text (success path, warnings, or the error).
    ExportFinished {
        image_id: i64,
        ok: bool,
        message: String,
    },
```

- [ ] **Step 3: Add the dialog state slot**

In `ferrolite-app/src/state.rs`, add to `struct AppState` (after `viewer`):

```rust
    /// The single-file export dialog, `Some` while the format+options popup is
    /// open (spec §8.3).
    pub export_dialog: Option<crate::export::ExportDialogState>,
```

Initialize it to `None` in `AppState`'s constructor.

- [ ] **Step 4: Declare the export module**

In `ferrolite-app/src/lib.rs` (or wherever `mod chrome;` etc. are declared), add `pub mod export;`.

- [ ] **Step 5: Build check (expected to fail on missing module)**

Run: `cargo build -p ferrolite-app`
Expected: FAILS with "file not found for module `export`" — resolved in Task 12. Proceed.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/Cargo.toml ferrolite-app/src/events.rs ferrolite-app/src/state.rs ferrolite-app/src/lib.rs
git commit -m "feat(app): export events + dialog-state slot + deps"
```

---

## Task 11: Interactive `Photo` menu → Export action

Replace the painted `Photo` label with a real dropdown returning a `MenuAction`. Keep the other labels painted (unchanged grammar) or as inert menu buttons.

**Files:**
- Modify: `ferrolite-app/src/chrome/mod.rs`
- Modify: `ferrolite-app/src/app.rs` (call site returns/handles the action)

**Interfaces:**
- Produces: `pub enum MenuAction { ExportImage }`; `title_bar(...) -> Option<MenuAction>`.

- [ ] **Step 1: Add `MenuAction` and change `title_bar` to return it**

In `ferrolite-app/src/chrome/mod.rs`, add near the top:

```rust
/// A menu action selected from the title-bar menus, handled by the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    ExportImage,
}
```

Change the signature:

```rust
pub fn title_bar(
    ctx: &Context,
    ui: &mut egui::Ui,
    module: &mut Module,
    version: &str,
    export_enabled: bool,
) -> Option<MenuAction> {
```

- [ ] **Step 2: Replace the painted menu-label loop with an interactive menu row**

Remove the `for m in ["File", "Edit", "Photo", "View", "Help"] { ... painter.text ... }` block. Keep the icon + wordmark painting. After computing `x` past the wordmark, add an interactive child UI holding the menus and capture the action:

```rust
    // Interactive menu row (on top of the drag region, like the tabs). Only
    // "Photo" is functional in this plan; the others are inert placeholders.
    let mut action: Option<MenuAction> = None;
    let menu_rect = Rect::from_min_max(pos2(x, bar.top()), pos2(bar.center().x - 60.0, bar.bottom()));
    ui.allocate_new_ui(
        UiBuilder::new()
            .max_rect(menu_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            // Frameless, dim menu buttons to match the old painted look.
            ui.visuals_mut().widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
            ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::NONE;
            for label in ["File", "Edit"] {
                let _ = ui.menu_button(label, |ui| {
                    ui.add_enabled(false, egui::Button::new("(no actions)"));
                });
            }
            ui.menu_button("Photo", |ui| {
                if ui
                    .add_enabled(export_enabled, egui::Button::new("Export…"))
                    .clicked()
                {
                    action = Some(MenuAction::ExportImage);
                    ui.close_menu();
                }
            });
            for label in ["View", "Help"] {
                let _ = ui.menu_button(label, |ui| {
                    ui.add_enabled(false, egui::Button::new("(no actions)"));
                });
            }
        },
    );
```

At the end of `title_bar`, `return action;` (change the fn to return it; the control/tab UIs stay as-is).

> The `x` after the wordmark is computed as `logo.right() + 14.0` today; reuse that. The `menu_rect` right bound (`bar.center().x - 60.0`) just keeps the menus left of the centered tabs — tune during the visual test.

- [ ] **Step 3: Update the call site in `app.rs`**

At `ferrolite-app/src/app.rs:1117`, capture the return and handle it. `export_enabled` = a viewer is open with a resident pyramid:

```rust
                let export_enabled = self
                    .state
                    .viewer
                    .as_ref()
                    .is_some_and(|v| v.pyramid.is_some());
                let menu_action =
                    crate::chrome::title_bar(ctx, ui, &mut self.module, "v0.0.1", export_enabled);
                if menu_action == Some(crate::chrome::MenuAction::ExportImage) {
                    self.open_export_dialog();
                }
```

Add the handler to `impl FerroliteApp`:

```rust
    /// Open the single-file export dialog for the current viewer image.
    fn open_export_dialog(&mut self) {
        if self.state.viewer.is_some() {
            self.state.export_dialog = Some(crate::export::ExportDialogState::default());
        }
    }
```

- [ ] **Step 4: Build (still needs `export` module — Task 12)**

Run: `cargo build -p ferrolite-app`
Expected: still fails only on the missing `export` module; the chrome + call-site edits compile once Task 12 lands. (If you want a green checkpoint here, temporarily stub `open_export_dialog` to a no-op and skip `ExportDialogState` until Task 12, then wire it. Recommended: land Task 12 next and build once.)

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/chrome/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(app): interactive Photo menu with Export action"
```

---

## Task 12: Export dialog UI + spawn the Background job

**Files:**
- Create: `ferrolite-app/src/export/mod.rs`

**Interfaces:**
- Consumes: `ferrolite_export::{ExportOptions, ExportFormat, BitDepth, ResizeSpec, run_export, ExportRequest}`, `ferrolite_color::WorkingSpace`, `rfd`, the app `AppState`/`JobSystem`/event channel, `FerroliteApp::camera_to_working`.
- Produces:
  - `pub struct ExportDialogState { pub options: ExportOptions }` (`Default`).
  - `pub enum DialogOutcome { Confirm, Cancel }`.
  - `pub fn draw_dialog(ctx: &egui::Context, dialog: &mut ExportDialogState) -> Option<DialogOutcome>`.
  - `pub fn spawn_export(state: &crate::state::AppState, egui_ctx: &egui::Context, gpu: std::sync::Arc<ferrolite_gpu::GpuContext>, pyramid: std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>, camera_to_working: [[f32; 3]; 3], working_space: ferrolite_color::WorkingSpace, options: ferrolite_export::ExportOptions, source_path: std::path::PathBuf, dest: std::path::PathBuf, image_id: i64)`.

- [ ] **Step 1: Create `ferrolite-app/src/export/mod.rs` — dialog state + UI**

```rust
//! Single-file Photo → Export flow (spec §8.3): a format+options popup, then an
//! rfd destination picker, then one ferrolite-jobs Background export job.

use std::path::PathBuf;
use std::sync::Arc;

use ferrolite_color::WorkingSpace;
use ferrolite_export::{run_export, BitDepth, ExportFormat, ExportOptions, ExportRequest, ResizeSpec};
use ferrolite_gpu::GpuContext;
use ferrolite_jobs::Priority;
use ferrolite_pipeline::GpuPyramidSource;

use crate::events::AppEvent;
use crate::state::AppState;

#[derive(Default)]
pub struct ExportDialogState {
    pub options: ExportOptions,
}

pub enum DialogOutcome {
    Confirm,
    Cancel,
}

/// Draw the export options popup. Returns `Some(Confirm)` when the user hits
/// "Choose destination…", `Some(Cancel)` on cancel/close, else `None`.
pub fn draw_dialog(ctx: &egui::Context, dialog: &mut ExportDialogState) -> Option<DialogOutcome> {
    let mut outcome = None;
    let mut open = true;
    egui::Window::new("Export")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            let o = &mut dialog.options;

            egui::ComboBox::from_label("Format")
                .selected_text(o.format.label())
                .show_ui(ui, |ui| {
                    for f in ExportFormat::ALL {
                        ui.selectable_value(&mut o.format, f, f.label());
                    }
                });

            egui::ComboBox::from_label("Output color space")
                .selected_text(format!("{:?}", o.output_space))
                .show_ui(ui, |ui| {
                    for ws in WorkingSpace::ALL {
                        ui.selectable_value(&mut o.output_space, ws, format!("{ws:?}"));
                    }
                });

            // Bit depth — 16-bit only for TIFF/PNG.
            ui.horizontal(|ui| {
                ui.label("Bit depth");
                ui.selectable_value(&mut o.bit_depth, BitDepth::Eight, "8-bit");
                ui.add_enabled_ui(o.format.supports_16bit(), |ui| {
                    ui.selectable_value(&mut o.bit_depth, BitDepth::Sixteen, "16-bit");
                });
            });
            if !o.format.supports_16bit() {
                o.bit_depth = BitDepth::Eight;
            }

            // Quality — JPEG only.
            ui.add_enabled_ui(o.format.supports_quality(), |ui| {
                ui.add(egui::Slider::new(&mut o.quality, 1..=100).text("Quality"));
            });

            // Resize.
            let mut mode = match o.resize {
                ResizeSpec::None => 0,
                ResizeSpec::LongEdge(_) => 1,
                ResizeSpec::Exact { .. } => 2,
                ResizeSpec::Percent(_) => 3,
            };
            egui::ComboBox::from_label("Resize")
                .selected_text(["None", "Long edge", "Exact", "Percent"][mode])
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut mode, 0, "None");
                    ui.selectable_value(&mut mode, 1, "Long edge");
                    ui.selectable_value(&mut mode, 2, "Exact");
                    ui.selectable_value(&mut mode, 3, "Percent");
                });
            o.resize = match mode {
                1 => {
                    let mut px = if let ResizeSpec::LongEdge(p) = o.resize { p } else { 2048 };
                    ui.add(egui::DragValue::new(&mut px).range(1..=100_000).prefix("px "));
                    ResizeSpec::LongEdge(px)
                }
                2 => {
                    let (mut w, mut h) = if let ResizeSpec::Exact { w, h } = o.resize {
                        (w, h)
                    } else {
                        (1920, 1080)
                    };
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut w).range(1..=100_000).prefix("W "));
                        ui.add(egui::DragValue::new(&mut h).range(1..=100_000).prefix("H "));
                    });
                    ResizeSpec::Exact { w, h }
                }
                3 => {
                    let mut pct = if let ResizeSpec::Percent(p) = o.resize { p * 100.0 } else { 50.0 };
                    ui.add(egui::Slider::new(&mut pct, 1.0..=100.0).suffix("%"));
                    ResizeSpec::Percent(pct / 100.0)
                }
                _ => ResizeSpec::None,
            };

            ui.separator();
            ui.checkbox(&mut o.copy_exif, "Copy EXIF metadata");
            ui.checkbox(&mut o.embed_icc, "Embed ICC profile");
            ui.checkbox(&mut o.strip_metadata, "Strip metadata");

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Choose destination…").clicked() {
                    outcome = Some(DialogOutcome::Confirm);
                }
                if ui.button("Cancel").clicked() {
                    outcome = Some(DialogOutcome::Cancel);
                }
            });
        });
    if !open && outcome.is_none() {
        outcome = Some(DialogOutcome::Cancel);
    }
    outcome
}
```

- [ ] **Step 2: Add `spawn_export` (append to `export/mod.rs`)**

```rust
/// Submit ONE Background export job for the currently open image. Captures the
/// shared GpuContext + resident pyramid; builds the TileEditPipeline inside the
/// closure (worker thread). Progress + completion flow back over the app channel.
#[allow(clippy::too_many_arguments)]
pub fn spawn_export(
    state: &AppState,
    egui_ctx: &egui::Context,
    gpu: Arc<GpuContext>,
    pyramid: Arc<GpuPyramidSource>,
    stack: ferrolite_pipeline::OpStack,
    camera_to_working: [[f32; 3]; 3],
    working_space: WorkingSpace,
    options: ExportOptions,
    source_path: PathBuf,
    dest: PathBuf,
    image_id: i64,
) {
    let tx = state.tx.clone();
    let egui_ctx = egui_ctx.clone();
    state.jobs.submit(Priority::Background, move |cancel| {
        let mut last_repaint = 0u32;
        let mut progress = |done: u32, total: u32| {
            let _ = tx.send(AppEvent::ExportProgress { image_id, done, total });
            // Repaint occasionally so the status bar advances without flooding.
            if done == total || done.saturating_sub(last_repaint) >= 8 {
                last_repaint = done;
                egui_ctx.request_repaint();
            }
        };
        let req = ExportRequest {
            ctx: &gpu,
            pyramid: &pyramid,
            stack: &stack,
            camera_to_working,
            working_space,
            options: &options,
            dest: &dest,
            source_path: &source_path,
        };
        let (ok, message) = match run_export(req, cancel, &mut progress) {
            Ok(outcome) => {
                let base = format!("Exported to {}", outcome.dest.display());
                let msg = if outcome.warnings.is_empty() {
                    base
                } else {
                    format!("{base} ({})", outcome.warnings.join("; "))
                };
                (true, msg)
            }
            Err(ferrolite_export::ExportError::Cancelled) => (false, "Export cancelled".to_string()),
            Err(e) => (false, format!("Export failed: {e}")),
        };
        let _ = tx.send(AppEvent::ExportFinished { image_id, ok, message });
        egui_ctx.request_repaint();
    });
}
```

- [ ] **Step 3: Build the app**

Run: `cargo build -p ferrolite-app`
Expected: compiles (the `export` module now exists; chrome + state + events from Tasks 10–11 resolve).

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/export/mod.rs
git commit -m "feat(app): export options dialog + Background job spawn"
```

---

## Task 13: Wire the dialog → rfd destination → job in `app.rs`

**Files:**
- Modify: `ferrolite-app/src/app.rs`

**Interfaces:**
- Consumes: `crate::export::{draw_dialog, spawn_export, DialogOutcome}`, `FerroliteApp::camera_to_working`, `frame.wgpu_render_state()`.

- [ ] **Step 1: Draw the dialog each frame and act on its outcome**

In `FerroliteApp::update` (where the central content is drawn, after the status panel or near the other per-frame UI), add:

```rust
        // Single-file export dialog (spec §8.3).
        if self.state.export_dialog.is_some() {
            let outcome = {
                let dialog = self.state.export_dialog.as_mut().unwrap();
                crate::export::draw_dialog(ctx, dialog)
            };
            match outcome {
                Some(crate::export::DialogOutcome::Cancel) => {
                    self.state.export_dialog = None;
                }
                Some(crate::export::DialogOutcome::Confirm) => {
                    self.confirm_export(ctx, frame);
                }
                None => {}
            }
        }
```

- [ ] **Step 2: Implement `confirm_export` (rfd → spawn)**

Add to `impl FerroliteApp`:

```rust
    /// The user confirmed the export dialog: pick a destination and spawn the job.
    fn confirm_export(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(dialog) = self.state.export_dialog.take() else { return };
        let options = dialog.options;

        let Some(v) = self.state.viewer.as_ref() else { return };
        let Some(pyramid) = v.pyramid.clone() else {
            self.state.warning = Some("Image still loading; cannot export yet.".to_string());
            return;
        };
        let source_path = v.path.clone();
        let image_id = v.image_id;

        // Default filename: source basename + new extension.
        let stem = source_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "export".to_string());
        let ext = options.format.extension();
        let default_name = format!("{stem}.{ext}");

        let Some(dest) = rfd::FileDialog::new()
            .set_file_name(default_name)
            .add_filter(options.format.label(), &[ext])
            .save_file()
        else {
            return; // user cancelled the save dialog
        };

        // Build the shared GpuContext from eframe's render state.
        let Some(rs) = frame.wgpu_render_state() else {
            self.state.warning = Some("No GPU render state; cannot export.".to_string());
            return;
        };
        let gpu = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));

        let camera_to_working = self.camera_to_working();
        let working_space = self.state.working_space;
        let stack = v.op_stack.clone();

        crate::export::spawn_export(
            &self.state,
            ctx,
            gpu,
            pyramid,
            stack,
            camera_to_working,
            working_space,
            options,
            source_path,
            dest,
            image_id,
        );
        self.state.warning = Some("Exporting…".to_string());
    }
```

> `self.camera_to_working()` borrows `self` immutably; take the viewer fields (`pyramid`, `path`, `op_stack`, `image_id`) into locals **before** calling it to avoid a borrow conflict — the code above clones them out of `v` first, then drops the `v` borrow implicitly before `self.camera_to_working()`. If the borrow checker complains, compute `let camera_to_working = self.camera_to_working();` at the very top of the function (before the `let Some(v)` borrow).

- [ ] **Step 3: Build**

Run: `cargo build -p ferrolite-app`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/app.rs
git commit -m "feat(app): wire export dialog → rfd destination → background job"
```

---

## Task 14: Handle export events + status bar + final gate

**Files:**
- Modify: `ferrolite-app/src/app.rs` (event loop)

**Interfaces:**
- Consumes: `AppEvent::ExportProgress`, `AppEvent::ExportFinished`; `AppState.warning`.

- [ ] **Step 1: Handle the events in the `try_recv` loop**

In the event-drain loop in `app.rs` (around line 998, alongside the other `match &event` arms), add:

```rust
                crate::events::AppEvent::ExportProgress { image_id, done, total } => {
                    if self.state.viewer.as_ref().is_some_and(|v| v.image_id == *image_id) {
                        self.state.warning = Some(format!("Exporting… {done}/{total}"));
                    }
                    ctx.request_repaint();
                    continue;
                }
                crate::events::AppEvent::ExportFinished { image_id: _, ok, message } => {
                    // Surface success + warnings, or the failure, in the status bar.
                    let _ = ok;
                    self.state.warning = Some(message.clone());
                    ctx.request_repaint();
                    continue;
                }
```

> `state.warning` currently renders in red (status_bar.rs). For Plan 4 that is acceptable for both success and failure (single, low-frequency message). A dedicated success/neutral color is a nice-to-have for the visual test, not required by the spec. If desired, add an `AppState.status_info: Option<String>` and render it in a neutral color; otherwise reuse `warning`.

- [ ] **Step 2: Full workspace gate**

Run each and confirm green:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected:
- `fmt` clean (run `cargo fmt` first if not).
- `clippy` zero warnings.
- `test` green: all pure units pass on every OS; GPU goldens (`ferrolite-export/tests/render_golden.rs`, plus the Plan 1–3 goldens) run on the dev GPU and **skip** on headless.

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/app.rs
git commit -m "feat(app): handle export progress/finished events + status"
```

- [ ] **Step 4: STOP — hold for the author's visual test**

Do **not** merge/PR/finish the branch yet (CLAUDE.md "Finishing a branch"). Present the finish options, then **hold** for Jann to run the app and confirm:
- Open an image in Develop (wait for full decode so the pyramid exists).
- Photo → Export → the options popup appears; format/space/bit-depth/quality/resize/metadata toggles behave (16-bit disabled for JPEG/WebP; quality enabled only for JPEG).
- Choose destination → a Background job runs (status bar shows "Exporting… n/total" then "Exported to …").
- Open the written file in an external viewer: correct pixels, correct size (incl. resize), EXIF present (if copied), ICC embedded (JPEG/PNG/TIFF).
- Address any issues found before completing the branch.

---

## Self-Review

**Spec coverage (§8.1–8.3, §10, §12.4):**
- Full-res tiled render via the Spec 2 GPU tile producer, no whole-image RGBA16F → Task 6 (`render_tiled` + per-tile readback; only the final quantized RGB buffer is whole-image).
- working→output conversion via `ferrolite-color` → Task 5 (`convert_pixel` uses `working_to_output` matrix + new `output_oetf`) + Task 1.
- Optional resize (`fast_image_resize`: none/long-edge/exact/percent) → Task 4 + Task 9.
- Encode JPEG/PNG/TIFF/WebP (8-bit default, 16-bit TIFF/PNG) → Task 7 + `effective_bit_depth` (Task 3).
- EXIF copy (`little_exif`) + embedded ICC → Task 7 (ICC via `set_icc_profile`) + Task 8 (EXIF).
- Runs as cancellable `ferrolite-jobs` Background job → Task 12 (`spawn_export` at `Priority::Background`, `CancelToken` checked per tile in Task 6).
- Single Photo → Export flow: menu → options popup → destination popup → one job → Tasks 11, 12, 13.
- Defaults per §8.2 → Task 3 `ExportOptions::default`.
- Error handling (§10): never-panic, ICC best-effort, encode/write/EXIF failures surfaced → Tasks 3/7/8/9/14.
- Honors CLAUDE.md off-thread + bounded GPU → Global Constraints + Task 6/12.

**Resolved tensions (documented in Global Constraints):** (a) GPU-on-worker vs render-thread → shared device on Background worker, reuse resident pyramid (user-approved); (b) WebP quality vs no-C-toolchain → lossless WebP, quality = JPEG only (user-approved).

**Placeholder scan:** every code step has concrete code; commands have expected output. Two API-version caveats are flagged inline with a verification pointer (the `image` encoder constructor return types in Task 7; the `little_exif` tag accessors in Task 8) rather than left as TODOs.

**Type consistency:** `RenderedImage`/`PixelData` (Task 6) used verbatim in Tasks 7/9; `ExportOptions`/`ExportFormat`/`BitDepth`/`ResizeSpec` (Task 3) used verbatim in Tasks 4/5/7/9/12; `ExportRequest`/`run_export`/`ExportOutcome` (Task 9) used verbatim in Task 12; `MenuAction` (Task 11) used in the `app.rs` call site; `AppEvent::ExportProgress`/`ExportFinished` (Task 10) produced in Task 12 and consumed in Task 14; `edited_output_dims` (Task 2) consumed in Task 6; `output_oetf` (Task 1) consumed in Task 5. `camera_to_working()` and `working_space` are the Plan 2 app members.

**Out of scope (Plan 5 / later specs):** the batch Export module, `export_queue` table, filename token template, add-to-queue actions, and the design-system "three modules" doc update are Plan 5. AVIF/JPEG-XL and monitor-profile display CM are Spec 4.

# Viewer & VT Ladder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the single-image viewer and the sparse virtual texture that backs it, completing Spec 1's two-tier load path (instant embedded preview → full RAW decode → display-linear RGB → sparse-VT full-res view with smooth GPU zoom/pan).

**Architecture:** Two new engine-transferable crates — `ferrolite-gpu` (wgpu `GpuContext` + a generic photo-agnostic retained-DAG executor) and `ferrolite-vt` (source-agnostic sparse virtual texture built as four de-risking rungs: single-texture → static tiled+mip → residency+LRU → page-table + GPU feedback). Photo-domain CFA→RGB conversion lives in `ferrolite-decode` behind a `DemosaicToRgb16f` trait (a fast half-res `QuadBin` impl now). The `ferrolite-app` viewer wires the two tiers via the existing `ferrolite-jobs` system with navigation-driven cancellation.

**Tech Stack:** Rust 2021, wgpu 22.1, egui/eframe 0.29.1, `bytemuck`, `half`, `pollster`, `fast_image_resize` 6.0, `rawler` 0.7.2, `ferrolite-jobs`.

## Global Constraints

- **Rust edition 2021; `rust-version = "1.88"`** (workspace `[workspace.package]`).
- **Engine-tier crates (`ferrolite-image`, `ferrolite-gpu`, `ferrolite-vt`) carry ONLY permissive deps** (`wgpu`, `bytemuck`, `half`, `pollster`, `fast_image_resize`, `rayon`) — never `rawler`/`rusqlite`/`image`/photo-domain crates. They must stay relicensable. Photo concepts (Bayer, WB, EXIF) never appear in these crates.
- **`ferrolite-vt` is source-agnostic** — it consumes a `TileSource` trait and never knows what produced the pixels (cross-cutting contract §5).
- **`ferrolite-gpu`'s executor is photo-agnostic** — testable with toy arithmetic nodes, no image/tile/wgpu concepts in the executor logic (contract §4).
- **All slow work submits to `ferrolite-jobs`** with priority + cancellation token (contract §1). Navigation cancels superseded decode/tile jobs.
- **GPU tests MUST skip when no adapter exists.** CI runs `cargo test --all` on ubuntu/macos/windows with no GPU/software rasterizer. Any test needing a device calls `GpuContext::headless()`; if it returns `None`, the test logs and returns early (passes). `cargo test --workspace` must stay green in CI.
- **Tile format:** 256×256 tiles, `wgpu::TextureFormat::Rgba16Float`. CPU pyramid kept as `f32` RGBA (`LinearRgbaF32`); converted to `f16` (via `half::f16`) only at upload.
- **Display-linear convention:** demosaic outputs linear `[0,1]` RGBA; the sRGB OETF (gamma) is applied in the display shader, never baked into pixels (so Spec 3 can swap the color path).
- **Workspace gate (must pass before finishing the branch):** `cargo fmt --all -- --check` && `cargo clippy --workspace --all-targets -- -D warnings` && `cargo test --workspace`.
- **Commit style:** conventional commits (`feat:`/`test:`/`docs:`/`refactor:`/`fix:`/`chore:`), no attribution footer (disabled globally).
- **Branch:** `feat/viewer-and-vt-ladder` (already created).

---

## File Structure

**`ferrolite-image`** (modify):
- `src/lib.rs` — re-export new vocab.
- `src/tile.rs` (create) — `TILE_SIZE`, `TileCoord`, pyramid/tile math.
- `src/linear.rs` (create) — `LinearRgbaF32`.

**`ferrolite-gpu`** (create crate):
- `Cargo.toml`, `src/lib.rs`
- `src/context.rs` — `GpuContext` (`from_render_state`, `headless`, offscreen render target).
- `src/executor.rs` — generic retained-DAG executor (`Node`, `Graph`, `NodeId`).

**`ferrolite-vt`** (create crate):
- `Cargo.toml`, `src/lib.rs`
- `src/source.rs` — `TileSource` trait + `PyramidTileSource`.
- `src/residency.rs` — pure CPU residency core (`needed_tiles`, `ResidencySet`).
- `src/transform.rs` — `ViewTransform` (zoom/pan/fit math; pure).
- `src/pool.rs` — physical tile pool atlas (rungs 3–4).
- `src/page_table.rs` — indirection texture + feedback buffer (rung 4).
- `src/view.rs` — `VirtualTexture` orchestrator: `request_view`, paint.
- `src/shaders/display.wgsl` — display/sampling shader (grows per rung).
- `tests/golden.rs` — golden-image diffs (adapter-gated).
- `tests/fixtures/*.png` — committed reference images.

**`ferrolite-decode`** (modify):
- `src/raw.rs:1-46` — extend `RawDecoded` / `decode_full` to surface CFA + black/white + WB.
- `src/demosaic.rs` (create) — `DemosaicParams`, `DemosaicToRgb16f`, `QuadBin`.
- `src/lib.rs` — re-export.

**`ferrolite-app`** (modify):
- `src/viewer/mod.rs` (create) — `ViewerState`, open/close, paint dispatch.
- `src/viewer/load.rs` (create) — two-tier load orchestration + job wiring.
- `src/state.rs` — add `viewer: Option<ViewerState>`.
- `src/events.rs` — add viewer load events (`PreviewReady`, `FullDecoded`, `FullFailed`).
- `src/app.rs:157-166` — route central panel to viewer when open; key handling.
- `src/library/grid.rs` — double-click / Enter opens the viewer.

---

## Phase 1 — `ferrolite-image` vocabulary

### Task 1: Tile coordinates & pyramid math

**Files:**
- Create: `ferrolite-image/src/tile.rs`
- Modify: `ferrolite-image/src/lib.rs`

**Interfaces:**
- Produces: `pub const TILE_SIZE: u32 = 256;`
  `pub struct TileCoord { pub lod: u32, pub x: u32, pub y: u32 }`
  `pub fn pyramid_level_count(width: u32, height: u32) -> u32`
  `pub fn level_size(width: u32, height: u32, lod: u32) -> (u32, u32)`
  `pub fn tiles_per_level(width: u32, height: u32, lod: u32) -> (u32, u32)`
  `pub fn tile_pixel_origin(coord: TileCoord) -> (u32, u32)`

- [ ] **Step 1: Write the failing test**

Append to `ferrolite-image/src/tile.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_count_reaches_single_tile_top() {
        // 1024x512 -> L0 1024x512, L1 512x256, L2 256x128 (fits one tile both dims) => 3 levels
        assert_eq!(pyramid_level_count(1024, 512), 3);
        // exactly one tile already => 1 level
        assert_eq!(pyramid_level_count(256, 256), 1);
        // smaller than a tile => still 1 level
        assert_eq!(pyramid_level_count(100, 10), 1);
    }

    #[test]
    fn level_size_halves_each_lod_min_one() {
        assert_eq!(level_size(1024, 512, 0), (1024, 512));
        assert_eq!(level_size(1024, 512, 1), (512, 256));
        assert_eq!(level_size(1024, 512, 2), (256, 128));
        // never collapses below 1
        assert_eq!(level_size(1, 1, 5), (1, 1));
    }

    #[test]
    fn tiles_per_level_is_ceil_div_tile_size() {
        assert_eq!(tiles_per_level(512, 256, 0), (2, 1));
        assert_eq!(tiles_per_level(513, 256, 0), (3, 1)); // ceil
        assert_eq!(tiles_per_level(1024, 512, 1), (2, 1)); // 512x256 at L1
    }

    #[test]
    fn tile_origin_multiplies_by_tile_size() {
        assert_eq!(tile_pixel_origin(TileCoord { lod: 0, x: 0, y: 0 }), (0, 0));
        assert_eq!(tile_pixel_origin(TileCoord { lod: 3, x: 2, y: 1 }), (512, 256));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-image tile::`
Expected: FAIL — `cannot find function/type` (module/items not defined).

- [ ] **Step 3: Write minimal implementation**

Prepend to `ferrolite-image/src/tile.rs`:
```rust
//! Tile coordinate vocabulary and LOD-pyramid math. Pure, GPU-free, photo-free
//! so it stays in the engine-transferable tier and is testable without a device.

/// Edge length of a square virtual tile, in pixels.
pub const TILE_SIZE: u32 = 256;

/// Address of one virtual tile: mip level + tile column/row within that level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub lod: u32,
    pub x: u32,
    pub y: u32,
}

/// Pixel size of `lod`, halving each level (floor), clamped to a 1px minimum.
pub fn level_size(width: u32, height: u32, lod: u32) -> (u32, u32) {
    let w = (width >> lod).max(1);
    let h = (height >> lod).max(1);
    (w, h)
}

/// Number of LOD levels: keep halving until both dims fit within one tile.
pub fn pyramid_level_count(width: u32, height: u32) -> u32 {
    let mut lod = 0u32;
    loop {
        let (w, h) = level_size(width, height, lod);
        if w <= TILE_SIZE && h <= TILE_SIZE {
            return lod + 1;
        }
        lod += 1;
    }
}

/// Tile grid dimensions of `lod` (ceil-division by `TILE_SIZE`).
pub fn tiles_per_level(width: u32, height: u32, lod: u32) -> (u32, u32) {
    let (w, h) = level_size(width, height, lod);
    (w.div_ceil(TILE_SIZE), h.div_ceil(TILE_SIZE))
}

/// Top-left pixel of `coord` within its own LOD level.
pub fn tile_pixel_origin(coord: TileCoord) -> (u32, u32) {
    (coord.x * TILE_SIZE, coord.y * TILE_SIZE)
}
```

Add to `ferrolite-image/src/lib.rs` (after existing `mod` lines and `pub use`):
```rust
mod tile;
pub use tile::{level_size, pyramid_level_count, tile_pixel_origin, tiles_per_level, TileCoord, TILE_SIZE};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-image tile::`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-image/src/tile.rs ferrolite-image/src/lib.rs
git commit -m "feat(image): tile coordinate + LOD pyramid vocabulary"
```

---

### Task 2: `LinearRgbaF32` display-linear image buffer

**Files:**
- Create: `ferrolite-image/src/linear.rs`
- Modify: `ferrolite-image/src/lib.rs`

**Interfaces:**
- Produces: `pub struct LinearRgbaF32 { pub width: u32, pub height: u32, pub pixels: Vec<f32> }`
  `LinearRgbaF32::new(width, height, pixels) -> Result<Self, ImageBufferError>` (reuses existing `ImageBufferError`)
  `LinearRgbaF32::expected_len(width, height) -> usize` (== `w*h*4`)
  `LinearRgbaF32::black(width, height) -> Self`

- [ ] **Step 1: Write the failing test**

Append to `ferrolite-image/src/linear.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_len_is_four_channels() {
        assert_eq!(LinearRgbaF32::expected_len(4, 2), 32);
    }

    #[test]
    fn new_accepts_correct_length() {
        let img = LinearRgbaF32::new(2, 1, vec![0.0; 8]).unwrap();
        assert_eq!(img.width, 2);
        assert_eq!(img.pixels.len(), 8);
    }

    #[test]
    fn new_rejects_wrong_length() {
        let err = LinearRgbaF32::new(2, 1, vec![0.0; 7]).unwrap_err();
        assert_eq!(err.expected, 8);
        assert_eq!(err.actual, 7);
    }

    #[test]
    fn black_is_opaque_zero_rgb() {
        let img = LinearRgbaF32::black(1, 1);
        assert_eq!(img.pixels, vec![0.0, 0.0, 0.0, 1.0]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-image linear::`
Expected: FAIL — type not defined.

- [ ] **Step 3: Write minimal implementation**

Prepend to `ferrolite-image/src/linear.rs`:
```rust
//! Display-linear RGBA f32 image — the CPU-side product of demosaic and the
//! input to the VT LOD pyramid. f32 on the CPU; converted to f16 at GPU upload.

use crate::ImageBufferError;

#[derive(Debug, Clone, PartialEq)]
pub struct LinearRgbaF32 {
    pub width: u32,
    pub height: u32,
    /// Interleaved RGBA, length `width*height*4`, linear `[0,1]` (not gamma-encoded).
    pub pixels: Vec<f32>,
}

impl LinearRgbaF32 {
    pub fn expected_len(width: u32, height: u32) -> usize {
        width as usize * height as usize * 4
    }

    pub fn new(width: u32, height: u32, pixels: Vec<f32>) -> Result<Self, ImageBufferError> {
        let expected = Self::expected_len(width, height);
        if pixels.len() != expected {
            return Err(ImageBufferError { expected, actual: pixels.len() });
        }
        Ok(Self { width, height, pixels })
    }

    /// Opaque black image (RGB 0, A 1).
    pub fn black(width: u32, height: u32) -> Self {
        let mut pixels = vec![0.0f32; Self::expected_len(width, height)];
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 1.0;
        }
        Self { width, height, pixels }
    }
}
```

Add to `ferrolite-image/src/lib.rs`:
```rust
mod linear;
pub use linear::LinearRgbaF32;
```

`ImageBufferError` is already `pub` from `pixel.rs` — confirm it is re-exported in `lib.rs` (it is: `pub use pixel::{ImageBuffer, ImageBufferError, PixelFormat};`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-image linear::`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-image/src/linear.rs ferrolite-image/src/lib.rs
git commit -m "feat(image): LinearRgbaF32 display-linear buffer"
```

---

## Phase 2 — `ferrolite-gpu`

### Task 3: Crate skeleton + `GpuContext`

**Files:**
- Create: `ferrolite-gpu/Cargo.toml`, `ferrolite-gpu/src/lib.rs`, `ferrolite-gpu/src/context.rs`
- Modify: `Cargo.toml` (workspace members + workspace deps)

**Interfaces:**
- Produces:
  `pub struct GpuContext { pub device: std::sync::Arc<wgpu::Device>, pub queue: std::sync::Arc<wgpu::Queue> }`
  `GpuContext::from_render_state(rs: &egui_wgpu::RenderState) -> Self`
  `GpuContext::headless() -> Option<GpuContext>` (None when no adapter)
  `GpuContext::render_target(&self, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::Texture`
  `GpuContext::read_rgba8(&self, tex: &wgpu::Texture, w: u32, h: u32) -> Vec<u8>` (offscreen readback for golden tests)

- [ ] **Step 1: Add the crate to the workspace**

Edit root `Cargo.toml` `members`:
```toml
members = ["ferrolite-app", "ferrolite-image", "ferrolite-decode", "ferrolite-catalog", "ferrolite-jobs", "ferrolite-gpu", "ferrolite-vt"]
```
Add to `[workspace.dependencies]`:
```toml
ferrolite-gpu = { path = "ferrolite-gpu" }
ferrolite-vt = { path = "ferrolite-vt" }
wgpu = "22"
bytemuck = { version = "1", features = ["derive"] }
half = { version = "2", features = ["bytemuck"] }
pollster = "0.4"
```

Create `ferrolite-gpu/Cargo.toml`:
```toml
[package]
name = "ferrolite-gpu"
version = "0.0.1"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
ferrolite-image = { workspace = true }
wgpu = { workspace = true }
bytemuck = { workspace = true }
half = { workspace = true }
pollster = { workspace = true }
egui-wgpu = "0.29"
```

- [ ] **Step 2: Write the failing test**

Create `ferrolite-gpu/src/context.rs` with the test first:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_context_can_make_and_read_a_render_target() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let tex = ctx.render_target(4, 4, wgpu::TextureFormat::Rgba8Unorm);
        // Clear it to opaque red via a render pass, then read it back.
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        ctx.queue.submit([enc.finish()]);
        let pixels = ctx.read_rgba8(&tex, 4, 4);
        assert_eq!(pixels.len(), 4 * 4 * 4);
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]); // first texel red
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p ferrolite-gpu context::`
Expected: FAIL — `GpuContext` not defined (or compile error). (If a dev machine has no adapter the test would *skip*, but it must first compile — so failure here is the compile error.)

- [ ] **Step 4: Write minimal implementation**

Prepend to `ferrolite-gpu/src/context.rs`:
```rust
//! wgpu device handle wrapper. In the app it borrows eframe's device; for tests
//! it spins up a headless adapter (returning None when none is available so
//! GPU tests skip cleanly in headless CI). Engine-transferable.

use std::sync::Arc;

pub struct GpuContext {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
}

impl GpuContext {
    /// Borrow eframe's already-created device/queue (the app path).
    pub fn from_render_state(rs: &egui_wgpu::RenderState) -> Self {
        Self { device: rs.device.clone(), queue: rs.queue.clone() }
    }

    /// Create a standalone headless context for tests. Returns `None` if no
    /// adapter is available (e.g. CI runners without a GPU) so callers skip.
    pub fn headless() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("ferrolite-headless"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        ))
        .ok()?;
        Some(Self { device: Arc::new(device), queue: Arc::new(queue) })
    }

    /// A `RENDER_ATTACHMENT | COPY_SRC` texture for offscreen rendering.
    pub fn render_target(&self, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ferrolite-render-target"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Copy an `Rgba8` texture to the CPU (row-unpadded). For golden tests.
    pub fn read_rgba8(&self, tex: &wgpu::Texture, w: u32, h: u32) -> Vec<u8> {
        let bpr_unpadded = w * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let bpr_padded = bpr_unpadded.div_ceil(align) * align;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (bpr_padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
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
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((bpr_unpadded * h) as usize);
        for row in 0..h {
            let start = (row * bpr_padded) as usize;
            out.extend_from_slice(&data[start..start + bpr_unpadded as usize]);
        }
        drop(data);
        buf.unmap();
        out
    }
}
```

Create `ferrolite-gpu/src/lib.rs`:
```rust
//! ferrolite-gpu — wgpu context + a generic, photo-agnostic retained-DAG
//! executor. Engine-transferable (permissive deps only).

mod context;
mod executor;

pub use context::GpuContext;
pub use executor::{Graph, Node, NodeId};
```

(Note: `lib.rs` references `executor` — Task 4 creates it. To keep this task compiling, temporarily create an empty `ferrolite-gpu/src/executor.rs` containing only `// placeholder, implemented in Task 4` and the three `pub` items as minimal stubs is NOT allowed. Instead, in this task set `lib.rs` to declare only `mod context; pub use context::GpuContext;`, and Task 4 adds the executor lines.)

Revised `ferrolite-gpu/src/lib.rs` for THIS task:
```rust
//! ferrolite-gpu — wgpu context + (Task 4) a generic retained-DAG executor.
//! Engine-transferable (permissive deps only).

mod context;
pub use context::GpuContext;
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p ferrolite-gpu context::`
Expected: PASS (on a machine with a GPU: renders+reads red; on headless CI: prints skip message and passes).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml ferrolite-gpu/
git commit -m "feat(gpu): GpuContext (eframe + headless) with offscreen readback"
```

---

### Task 4: Generic retained-DAG executor

**Files:**
- Create: `ferrolite-gpu/src/executor.rs`
- Modify: `ferrolite-gpu/src/lib.rs`

**Interfaces:**
- Produces:
  `pub struct NodeId(pub usize);`
  `pub trait Node<O> { fn evaluate(&self, inputs: &[&O]) -> O; }`
  `pub struct Graph<O> { ... }` with:
  `Graph::new() -> Self`
  `Graph::add_node(&mut self, node: Box<dyn Node<O>>, inputs: Vec<NodeId>) -> NodeId`
  `Graph::mark_dirty(&mut self, id: NodeId)`
  `Graph::evaluate(&mut self, id: NodeId) -> &O` (topological, caches clean outputs)
  `Graph::eval_count(&self) -> usize` (test instrumentation: total node evaluations performed)

- [ ] **Step 1: Write the failing test**

Create `ferrolite-gpu/src/executor.rs` with tests first:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct Const(i64);
    impl Node<i64> for Const {
        fn evaluate(&self, _inputs: &[&i64]) -> i64 { self.0 }
    }
    struct Add;
    impl Node<i64> for Add {
        fn evaluate(&self, inputs: &[&i64]) -> i64 { inputs.iter().copied().sum() }
    }

    #[test]
    fn evaluates_in_topological_order() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(Const(2)), vec![]);
        let b = g.add_node(Box::new(Const(3)), vec![]);
        let sum = g.add_node(Box::new(Add), vec![a, b]);
        assert_eq!(*g.evaluate(sum), 5);
    }

    #[test]
    fn caches_clean_nodes_no_reevaluation() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(Const(2)), vec![]);
        let b = g.add_node(Box::new(Const(3)), vec![]);
        let sum = g.add_node(Box::new(Add), vec![a, b]);
        assert_eq!(*g.evaluate(sum), 5);
        let after_first = g.eval_count();
        assert_eq!(*g.evaluate(sum), 5); // all clean -> cache hit
        assert_eq!(g.eval_count(), after_first, "no node re-evaluated when all clean");
    }

    #[test]
    fn dirty_propagates_to_dependents() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(Const(2)), vec![]);
        let b = g.add_node(Box::new(Const(3)), vec![]);
        let sum = g.add_node(Box::new(Add), vec![a, b]);
        assert_eq!(*g.evaluate(sum), 5);
        g.mark_dirty(a); // a and its dependent `sum` must re-evaluate; b must not
        let before = g.eval_count();
        assert_eq!(*g.evaluate(sum), 5);
        assert_eq!(g.eval_count(), before + 2, "only a and sum re-evaluate");
    }

    #[test]
    fn diamond_evaluates_shared_input_once() {
        // a -> b, a -> c, (b,c) -> d : evaluating d evaluates a exactly once.
        let mut g = Graph::new();
        let a = g.add_node(Box::new(Const(1)), vec![]);
        let b = g.add_node(Box::new(Add), vec![a]);
        let c = g.add_node(Box::new(Add), vec![a]);
        let d = g.add_node(Box::new(Add), vec![b, c]);
        assert_eq!(*g.evaluate(d), 2);
        assert_eq!(g.eval_count(), 4, "a,b,c,d each once");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-gpu executor::`
Expected: FAIL — `Graph`/`Node`/`NodeId` not defined.

- [ ] **Step 3: Write minimal implementation**

Prepend to `ferrolite-gpu/src/executor.rs`:
```rust
//! A generic, photo-agnostic retained-DAG executor: nodes produce outputs `O`,
//! edges declare inputs, dirty flags drive minimal recompute with cached
//! outputs. Knows nothing about images, tiles, or wgpu (cross-cutting
//! contract §4); Spec 2's photo edit nodes implement `Node` and slot in.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub usize);

/// A unit of computation producing an output of type `O` from its inputs' outputs.
pub trait Node<O> {
    fn evaluate(&self, inputs: &[&O]) -> O;
}

struct Entry<O> {
    node: Box<dyn Node<O>>,
    inputs: Vec<NodeId>,
    cache: Option<O>,
    dirty: bool,
}

pub struct Graph<O> {
    nodes: Vec<Entry<O>>,
    eval_count: usize,
}

impl<O: Clone> Graph<O> {
    pub fn new() -> Self {
        Self { nodes: Vec::new(), eval_count: 0 }
    }

    pub fn add_node(&mut self, node: Box<dyn Node<O>>, inputs: Vec<NodeId>) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(Entry { node, inputs, cache: None, dirty: true });
        id
    }

    /// Mark `id` dirty and transitively mark every node that depends on it.
    pub fn mark_dirty(&mut self, id: NodeId) {
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            if !self.nodes[cur.0].dirty {
                self.nodes[cur.0].dirty = true;
            }
            // Dependents: any node listing `cur` as an input.
            for i in 0..self.nodes.len() {
                if self.nodes[i].inputs.contains(&cur) && !self.nodes[i].dirty {
                    stack.push(NodeId(i));
                }
            }
        }
        // The seed itself must be dirty even if already false-skipped above.
        self.nodes[id.0].dirty = true;
    }

    /// Evaluate `id`, recursively evaluating dirty inputs; clean nodes return
    /// their cached output. Returns a reference into the cache.
    pub fn evaluate(&mut self, id: NodeId) -> &O {
        self.eval_recursive(id);
        self.nodes[id.0].cache.as_ref().expect("evaluated node has a cache")
    }

    pub fn eval_count(&self) -> usize {
        self.eval_count
    }

    fn eval_recursive(&mut self, id: NodeId) {
        if !self.nodes[id.0].dirty && self.nodes[id.0].cache.is_some() {
            return;
        }
        let input_ids = self.nodes[id.0].inputs.clone();
        for &inp in &input_ids {
            self.eval_recursive(inp);
        }
        let inputs: Vec<&O> = input_ids
            .iter()
            .map(|i| self.nodes[i.0].cache.as_ref().expect("input cached"))
            .collect();
        let out = self.nodes[id.0].node.evaluate(&inputs);
        self.eval_count += 1;
        let entry = &mut self.nodes[id.0];
        entry.cache = Some(out);
        entry.dirty = false;
    }
}

impl<O: Clone> Default for Graph<O> {
    fn default() -> Self {
        Self::new()
    }
}
```

Update `ferrolite-gpu/src/lib.rs`:
```rust
//! ferrolite-gpu — wgpu context + a generic, photo-agnostic retained-DAG
//! executor. Engine-transferable (permissive deps only).

mod context;
mod executor;

pub use context::GpuContext;
pub use executor::{Graph, Node, NodeId};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-gpu executor::`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-gpu/src/executor.rs ferrolite-gpu/src/lib.rs
git commit -m "feat(gpu): generic retained-DAG executor with dirty-flag caching"
```

---

## Phase 3 — `ferrolite-decode` demosaic seam

### Task 5: Surface CFA/black/white/WB from full decode

**Files:**
- Modify: `ferrolite-decode/src/raw.rs`

**Interfaces:**
- Produces (extends existing `RawDecoded`):
  add fields `pub cfa_pattern: [u8; 4]` (0=R,1=G,2=B per the 2×2 CFA, row-major from top-left),
  `pub black_levels: [f32; 4]`, `pub white_level: f32`, `pub wb_coeffs: [f32; 4]` (R,G1,B,G2; NaN→1.0).
- Consumes: existing `decode_full(path) -> Result<RawDecoded, DecodeError>`.

**Note on rawler 0.7.2:** `RawImage` exposes `cfa` (with a pattern), `blacklevel`, `whitelevel`, and `wb_coeffs: [f32;4]`. Field/method names must be confirmed against the pinned crate by reading `~/.cargo` source or `cargo doc -p rawler --open`; the test pins the *contract* (lengths/defaults), and the implementer maps rawler's actual accessors to it. If an accessor differs, adapt the mapping — do not change the public `RawDecoded` shape below.

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` in `ferrolite-decode/src/raw.rs` (create the module if absent):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn decode_full_surfaces_cfa_and_levels() {
        // Use a committed fixture RAW if present; otherwise skip (kept green
        // where fixtures are absent).
        let fixture = Path::new("../fixtures/sample.dng");
        if !fixture.exists() {
            eprintln!("no RAW fixture; skipping decode_full metadata assertions");
            return;
        }
        let d = decode_full(fixture).expect("decode");
        assert_eq!(d.cfa_pattern.len(), 4);
        assert!(d.white_level > 0.0, "white level must be positive");
        assert!(d.wb_coeffs.iter().all(|c| c.is_finite() && *c > 0.0));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-decode raw::tests::decode_full_surfaces`
Expected: FAIL — `RawDecoded` has no field `cfa_pattern` (compile error).

- [ ] **Step 3: Write minimal implementation**

In `ferrolite-decode/src/raw.rs`, extend the struct and populate it. Replace the struct and `Ok(RawDecoded { ... })` tail:
```rust
#[derive(Debug, Clone)]
pub struct RawDecoded {
    pub width: u32,
    pub height: u32,
    pub cpp: usize,
    pub pixels: Vec<u16>,
    /// 2×2 CFA color indices (0=R,1=G,2=B), row-major from the top-left sensor pixel.
    pub cfa_pattern: [u8; 4],
    /// Per-CFA-position black levels (sensor units), order matches `cfa_pattern`.
    pub black_levels: [f32; 4],
    /// Saturation/white level (sensor units).
    pub white_level: f32,
    /// Camera white-balance multipliers [R, G1, B, G2]; non-finite → 1.0.
    pub wb_coeffs: [f32; 4],
}
```
After obtaining `img` from `decoder.raw_image(...)`, derive the new fields (map from rawler's accessors — confirm exact names against rawler 0.7.2):
```rust
    // --- CFA + levels (rawler 0.7.2 accessors; confirm names via cargo doc) ---
    let cfa_pattern = cfa_to_indices(&img.cfa); // helper below maps RGGB/BGGR/etc.
    let black_levels = normalize4(img.blacklevel.as_bayer_array()); // -> [f32;4]
    let white_level = img.whitelevel.0.first().copied().unwrap_or(65535) as f32;
    let wb = img.wb_coeffs;
    let wb_coeffs = [
        finite_or_one(wb[0]),
        finite_or_one(wb[1]),
        finite_or_one(wb[2]),
        finite_or_one(if wb[3].is_finite() { wb[3] } else { wb[1] }),
    ];
```
Add helpers at module bottom (adjust the rawler-facing parts to the real API):
```rust
fn finite_or_one(v: f32) -> f32 {
    if v.is_finite() && v > 0.0 { v } else { 1.0 }
}

/// Map rawler's CFA description to four 0=R/1=G/2=B indices (top-left row-major).
/// Implement against rawler 0.7.2's `CFA` API; default to RGGB if unknown.
fn cfa_to_indices(cfa: &rawler::cfa::CFA) -> [u8; 4] {
    let color = |x, y| match cfa.color_at(y, x) {
        0 => 0u8, // red
        1 => 1u8, // green
        2 => 2u8, // blue
        _ => 1u8,
    };
    [color(0, 0), color(1, 0), color(0, 1), color(1, 1)]
}

fn normalize4(levels: [u16; 4]) -> [f32; 4] {
    [levels[0] as f32, levels[1] as f32, levels[2] as f32, levels[3] as f32]
}
```
(If `blacklevel`/`whitelevel`/`wb_coeffs`/`cfa.color_at` names differ in 0.7.2, adapt — the public `RawDecoded` fields and their meaning are the fixed contract.)

- [ ] **Step 4: Run test to verify it passes / compiles**

Run: `cargo test -p ferrolite-decode raw::`
Expected: PASS (assertions run if a fixture exists; otherwise the metadata test skips, and existing decode tests still pass). The crate must compile.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-decode/src/raw.rs
git commit -m "feat(decode): surface CFA pattern, black/white levels, WB coeffs from full decode"
```

---

### Task 6: `DemosaicToRgb16f` trait + `QuadBin`

**Files:**
- Create: `ferrolite-decode/src/demosaic.rs`
- Modify: `ferrolite-decode/src/lib.rs`

**Interfaces:**
- Consumes: `RawDecoded` (Task 5), `ferrolite_image::LinearRgbaF32`.
- Produces:
  `pub struct DemosaicParams { pub black_levels: [f32;4], pub white_level: f32, pub wb_coeffs: [f32;4], pub cfa_pattern: [u8;4] }`
  `DemosaicParams::from_raw(&RawDecoded) -> Self`
  `pub trait DemosaicToRgb16f { fn to_linear_rgba_f32(&self, raw: &RawDecoded) -> LinearRgbaF32; }`
  `pub struct QuadBin;` implementing the trait.

- [ ] **Step 1: Write the failing test**

Create `ferrolite-decode/src/demosaic.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_image::LinearRgbaF32;

    /// Build a 2x2 RGGB RawDecoded with known samples and verify the single
    /// binned output pixel: R, avg(G1,G2), B, after black-level + WB + normalize.
    fn raw_2x2(r: u16, g1: u16, g2: u16, b: u16) -> crate::raw::RawDecoded {
        crate::raw::RawDecoded {
            width: 2,
            height: 2,
            cpp: 1,
            pixels: vec![r, g1, g2, b], // row0: R,G1 ; row1: G2,B
            cfa_pattern: [0, 1, 1, 2], // RGGB
            black_levels: [0.0; 4],
            white_level: 100.0,
            wb_coeffs: [1.0, 1.0, 1.0, 1.0],
        }
    }

    #[test]
    fn quadbin_halves_dimensions() {
        let raw = raw_2x2(100, 50, 50, 0);
        let out: LinearRgbaF32 = QuadBin.to_linear_rgba_f32(&raw);
        assert_eq!((out.width, out.height), (1, 1));
        assert_eq!(out.pixels.len(), 4);
    }

    #[test]
    fn quadbin_bins_channels_and_normalizes() {
        // white_level 100 -> R=100/100=1.0, G=avg(50,50)/100=0.5, B=0, A=1
        let raw = raw_2x2(100, 50, 50, 0);
        let out = QuadBin.to_linear_rgba_f32(&raw);
        assert!((out.pixels[0] - 1.0).abs() < 1e-6);
        assert!((out.pixels[1] - 0.5).abs() < 1e-6);
        assert!((out.pixels[2] - 0.0).abs() < 1e-6);
        assert!((out.pixels[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn quadbin_applies_black_level_and_wb() {
        // black 10 on all; wb R=2.0. R=(100-10)*2/(100-10)=2.0 -> clamps to 1.0.
        let mut raw = raw_2x2(100, 50, 50, 10);
        raw.black_levels = [10.0; 4];
        raw.wb_coeffs = [2.0, 1.0, 1.0, 1.0];
        let out = QuadBin.to_linear_rgba_f32(&raw);
        assert!((out.pixels[0] - 1.0).abs() < 1e-6, "R saturates to 1.0 after WB");
        // G=(50-10)/(100-10)=0.444...
        assert!((out.pixels[1] - (40.0 / 90.0)).abs() < 1e-5);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-decode demosaic::`
Expected: FAIL — `QuadBin`/`DemosaicToRgb16f` not defined.

- [ ] **Step 3: Write minimal implementation**

Prepend to `ferrolite-decode/src/demosaic.rs`:
```rust
//! CFA → display-linear RGBA conversion. Photo-domain (needs WB / black level),
//! so it lives here, not in the engine tier. `QuadBin` is the fast half-res
//! default; a full-res `Bilinear` impl is a future drop-in behind the trait.

use crate::raw::RawDecoded;
use ferrolite_image::LinearRgbaF32;

#[derive(Debug, Clone)]
pub struct DemosaicParams {
    pub black_levels: [f32; 4],
    pub white_level: f32,
    pub wb_coeffs: [f32; 4],
    pub cfa_pattern: [u8; 4],
}

impl DemosaicParams {
    pub fn from_raw(raw: &RawDecoded) -> Self {
        Self {
            black_levels: raw.black_levels,
            white_level: raw.white_level,
            wb_coeffs: raw.wb_coeffs,
            cfa_pattern: raw.cfa_pattern,
        }
    }
}

/// Convert raw CFA samples to a display-linear RGBA f32 image.
pub trait DemosaicToRgb16f {
    fn to_linear_rgba_f32(&self, raw: &RawDecoded) -> LinearRgbaF32;
}

/// Half-resolution 2×2 quad binning: each RGGB quad → one RGB pixel. Zero
/// demosaic artifacts; output is display-linear (gamma applied at the shader).
pub struct QuadBin;

impl DemosaicToRgb16f for QuadBin {
    fn to_linear_rgba_f32(&self, raw: &RawDecoded) -> LinearRgbaF32 {
        let out_w = (raw.width / 2).max(1);
        let out_h = (raw.height / 2).max(1);
        let p = DemosaicParams::from_raw(raw);
        // Locate R, the two greens, and B within the 2×2 pattern.
        let idx_of = |target: u8| p.cfa_pattern.iter().position(|&c| c == target);
        let r_pos = idx_of(0).unwrap_or(0);
        let b_pos = idx_of(2).unwrap_or(3);
        let greens: Vec<usize> = (0..4).filter(|&i| p.cfa_pattern[i] == 1).collect();
        let (g0, g1) = (greens.first().copied().unwrap_or(1), greens.get(1).copied().unwrap_or(2));

        let span = (p.white_level - p.black_levels[0]).max(1.0);
        let sample = |x: u32, y: u32, quad_idx: usize| -> f32 {
            let (qx, qy) = (quad_idx % 2, quad_idx / 2);
            let px = (x * 2 + qx as u32).min(raw.width - 1);
            let py = (y * 2 + qy as u32).min(raw.height - 1);
            let raw_v = raw.pixels[(py * raw.width + px) as usize] as f32;
            let bl = p.black_levels[quad_idx];
            ((raw_v - bl) / span).max(0.0)
        };

        let wb = p.wb_coeffs;
        let mut pixels = Vec::with_capacity(LinearRgbaF32::expected_len(out_w, out_h));
        for y in 0..out_h {
            for x in 0..out_w {
                let r = (sample(x, y, r_pos) * wb[0]).clamp(0.0, 1.0);
                let g = (((sample(x, y, g0) + sample(x, y, g1)) * 0.5) * wb[1]).clamp(0.0, 1.0);
                let b = (sample(x, y, b_pos) * wb[2]).clamp(0.0, 1.0);
                pixels.extend_from_slice(&[r, g, b, 1.0]);
            }
        }
        LinearRgbaF32::new(out_w, out_h, pixels).expect("quadbin length matches dims")
    }
}
```

Add to `ferrolite-decode/src/lib.rs`:
```rust
mod demosaic;
pub use demosaic::{DemosaicParams, DemosaicToRgb16f, QuadBin};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-decode demosaic::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-decode/src/demosaic.rs ferrolite-decode/src/lib.rs
git commit -m "feat(decode): DemosaicToRgb16f trait + QuadBin half-res demosaic"
```

---

## Phase 4 — `ferrolite-vt`

### Task 7: Crate skeleton + `TileSource` + `PyramidTileSource`

**Files:**
- Create: `ferrolite-vt/Cargo.toml`, `ferrolite-vt/src/lib.rs`, `ferrolite-vt/src/source.rs`

**Interfaces:**
- Consumes: `ferrolite_image::{LinearRgbaF32, TileCoord, TILE_SIZE, level_size, pyramid_level_count, tile_pixel_origin}`.
- Produces:
  `pub trait TileSource { fn level_count(&self)->u32; fn level_size(&self, lod:u32)->(u32,u32); fn tile(&self, coord:TileCoord)->LinearRgbaF32; }`
  `pub struct PyramidTileSource { ... }` with `PyramidTileSource::new(full: LinearRgbaF32) -> Self`.

- [ ] **Step 1: Create the crate manifest**

Create `ferrolite-vt/Cargo.toml`:
```toml
[package]
name = "ferrolite-vt"
version = "0.0.1"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
ferrolite-image = { workspace = true }
ferrolite-gpu = { workspace = true }
ferrolite-jobs = { workspace = true }
wgpu = { workspace = true }
bytemuck = { workspace = true }
half = { workspace = true }
fast_image_resize = { workspace = true }

[dev-dependencies]
pollster = { workspace = true }
image = { workspace = true, features = ["png"] }
```

- [ ] **Step 2: Write the failing test**

Create `ferrolite-vt/src/source.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_image::{LinearRgbaF32, TileCoord, TILE_SIZE};

    fn solid(w: u32, h: u32, rgb: [f32; 3]) -> LinearRgbaF32 {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
        }
        LinearRgbaF32::new(w, h, px).unwrap()
    }

    #[test]
    fn level_count_matches_pyramid_math() {
        let src = PyramidTileSource::new(solid(1024, 512, [0.5, 0.5, 0.5]));
        assert_eq!(src.level_count(), ferrolite_image::pyramid_level_count(1024, 512));
        assert_eq!(src.level_size(1), (512, 256));
    }

    #[test]
    fn tile_is_tile_sized_and_edge_clamped() {
        let src = PyramidTileSource::new(solid(300, 300, [1.0, 0.0, 0.0]));
        let t = src.tile(TileCoord { lod: 0, x: 0, y: 0 });
        assert_eq!((t.width, t.height), (TILE_SIZE, TILE_SIZE));
        // Interior pixel is red; out-of-image area is edge-clamped (also red here).
        assert_eq!(&t.pixels[0..4], &[1.0, 0.0, 0.0, 1.0]);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p ferrolite-vt source::`
Expected: FAIL — types not defined.

- [ ] **Step 4: Write minimal implementation**

Prepend to `ferrolite-vt/src/source.rs`:
```rust
//! Source-agnostic tile supply (cross-cutting contract §5). The VT consumes a
//! `TileSource`; it never knows what produced the pixels. `PyramidTileSource`
//! builds an in-memory LOD pyramid (box-downsample) from one full image.

use ferrolite_image::{
    level_size as img_level_size, pyramid_level_count, tile_pixel_origin, LinearRgbaF32,
    TileCoord, TILE_SIZE,
};

pub trait TileSource {
    fn level_count(&self) -> u32;
    fn level_size(&self, lod: u32) -> (u32, u32);
    /// A `TILE_SIZE`² tile, edge-clamped where the tile overhangs the level.
    fn tile(&self, coord: TileCoord) -> LinearRgbaF32;
}

pub struct PyramidTileSource {
    levels: Vec<LinearRgbaF32>, // index = lod
}

impl PyramidTileSource {
    pub fn new(full: LinearRgbaF32) -> Self {
        let count = pyramid_level_count(full.width, full.height);
        let mut levels = Vec::with_capacity(count as usize);
        levels.push(full);
        for lod in 1..count {
            let (w, h) = img_level_size(levels[0].width, levels[0].height, lod);
            levels.push(box_downsample(&levels[(lod - 1) as usize], w, h));
        }
        Self { levels }
    }
}

impl TileSource for PyramidTileSource {
    fn level_count(&self) -> u32 {
        self.levels.len() as u32
    }
    fn level_size(&self, lod: u32) -> (u32, u32) {
        let l = &self.levels[lod as usize];
        (l.width, l.height)
    }
    fn tile(&self, coord: TileCoord) -> LinearRgbaF32 {
        let level = &self.levels[coord.lod as usize];
        let (ox, oy) = tile_pixel_origin(coord);
        let mut px = Vec::with_capacity((TILE_SIZE * TILE_SIZE * 4) as usize);
        for ty in 0..TILE_SIZE {
            for tx in 0..TILE_SIZE {
                let sx = (ox + tx).min(level.width - 1);
                let sy = (oy + ty).min(level.height - 1);
                let i = ((sy * level.width + sx) * 4) as usize;
                px.extend_from_slice(&level.pixels[i..i + 4]);
            }
        }
        LinearRgbaF32::new(TILE_SIZE, TILE_SIZE, px).expect("tile length")
    }
}

/// Simple 2×2-average downsample to `(dst_w, dst_h)`. (Box filter is adequate for
/// the display pyramid; `fast_image_resize` can replace this for quality later.)
fn box_downsample(src: &LinearRgbaF32, dst_w: u32, dst_h: u32) -> LinearRgbaF32 {
    let mut px = vec![0.0f32; LinearRgbaF32::expected_len(dst_w, dst_h)];
    for dy in 0..dst_h {
        for dx in 0..dst_w {
            let sx0 = (dx * src.width / dst_w).min(src.width - 1);
            let sy0 = (dy * src.height / dst_h).min(src.height - 1);
            let sx1 = (sx0 + 1).min(src.width - 1);
            let sy1 = (sy0 + 1).min(src.height - 1);
            let mut acc = [0.0f32; 4];
            for &(x, y) in &[(sx0, sy0), (sx1, sy0), (sx0, sy1), (sx1, sy1)] {
                let i = ((y * src.width + x) * 4) as usize;
                for c in 0..4 {
                    acc[c] += src.pixels[i + c];
                }
            }
            let di = ((dy * dst_w + dx) * 4) as usize;
            for c in 0..4 {
                px[di + c] = acc[c] * 0.25;
            }
        }
    }
    LinearRgbaF32::new(dst_w, dst_h, px).expect("downsample length")
}
```

Create `ferrolite-vt/src/lib.rs`:
```rust
//! ferrolite-vt — source-agnostic sparse virtual texture. Engine-transferable.

mod source;
pub use source::{PyramidTileSource, TileSource};
```

`fast_image_resize` is declared as a dep for later quality work; the box filter keeps this task self-contained. (Clippy will not warn on an unused dep; if it does in your config, defer adding `fast_image_resize` to the manifest until the rung that uses it.)

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p ferrolite-vt source::`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml ferrolite-vt/Cargo.toml ferrolite-vt/src/lib.rs ferrolite-vt/src/source.rs
git commit -m "feat(vt): TileSource trait + in-memory PyramidTileSource"
```

---

### Task 8: Pure residency core (`needed_tiles` + `ResidencySet`)

**Files:**
- Create: `ferrolite-vt/src/transform.rs`, `ferrolite-vt/src/residency.rs`
- Modify: `ferrolite-vt/src/lib.rs`

**Interfaces:**
- Produces:
  `pub struct ViewTransform { pub zoom: f32, pub pan: (f32, f32) }` (pan in image pixels at zoom 1)
  `ViewTransform::fit(image: (u32,u32), viewport: (f32,f32)) -> Self`
  `ViewTransform::lod_for(&self, image: (u32,u32), max_lod: u32) -> u32`
  `pub fn needed_tiles(image: (u32,u32), view: &ViewTransform, viewport: (f32,f32), level_count: u32) -> Vec<TileCoord>`
  `pub struct ResidencySet { ... }` with `new(capacity)`, `touch(TileCoord)`, `insert(TileCoord)->Option<TileCoord>`, `diff(&[TileCoord]) -> (Vec<TileCoord>, Vec<TileCoord>)`, `contains`.

- [ ] **Step 1: Write the failing tests**

Create `ferrolite-vt/src/transform.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_centers_and_scales_to_viewport() {
        // 2000x1000 image into 1000x1000 viewport -> zoom 0.5 (width-bound).
        let t = ViewTransform::fit((2000, 1000), (1000.0, 1000.0));
        assert!((t.zoom - 0.5).abs() < 1e-6);
    }

    #[test]
    fn lod_increases_as_zoom_decreases() {
        // Zoomed way out -> coarse LOD; at 1:1 -> LOD 0.
        let mut t = ViewTransform { zoom: 1.0, pan: (0.0, 0.0) };
        assert_eq!(t.lod_for((4096, 4096), 6), 0);
        t.zoom = 0.25; // 1 screen px = 4 image px -> LOD ~2
        assert_eq!(t.lod_for((4096, 4096), 6), 2);
    }
}
```

Create `ferrolite-vt/src/residency.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_image::TileCoord;

    fn tc(lod: u32, x: u32, y: u32) -> TileCoord { TileCoord { lod, x, y } }

    #[test]
    fn insert_evicts_least_recently_used_over_capacity() {
        let mut r = ResidencySet::new(2);
        assert_eq!(r.insert(tc(0, 0, 0)), None);
        assert_eq!(r.insert(tc(0, 1, 0)), None);
        r.touch(tc(0, 0, 0)); // 0,0,0 now MRU
        assert_eq!(r.insert(tc(0, 2, 0)), Some(tc(0, 1, 0))); // evict LRU
        assert!(r.contains(tc(0, 0, 0)));
        assert!(!r.contains(tc(0, 1, 0)));
    }

    #[test]
    fn diff_reports_missing_and_unneeded() {
        let mut r = ResidencySet::new(8);
        r.insert(tc(0, 0, 0));
        r.insert(tc(0, 9, 9)); // resident but not needed
        let needed = vec![tc(0, 0, 0), tc(0, 1, 0)];
        let (to_load, to_evict) = r.diff(&needed);
        assert_eq!(to_load, vec![tc(0, 1, 0)]);
        assert_eq!(to_evict, vec![tc(0, 9, 9)]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ferrolite-vt transform:: ; cargo test -p ferrolite-vt residency::`
Expected: FAIL — types not defined.

- [ ] **Step 3: Write minimal implementation**

Prepend to `ferrolite-vt/src/transform.rs`:
```rust
//! Pure zoom/pan/fit + LOD selection math. No egui, no GPU — unit-testable.

/// View transform: `zoom` (screen px per image px) and `pan` (image-space px
/// offset of the viewport center).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewTransform {
    pub zoom: f32,
    pub pan: (f32, f32),
}

impl ViewTransform {
    /// Scale so the whole image fits the viewport; centered (pan 0,0).
    pub fn fit(image: (u32, u32), viewport: (f32, f32)) -> Self {
        let zx = viewport.0 / image.0 as f32;
        let zy = viewport.1 / image.1 as f32;
        Self { zoom: zx.min(zy).max(f32::MIN_POSITIVE), pan: (0.0, 0.0) }
    }

    /// LOD whose texels are ~1 screen pixel: `lod = floor(log2(1/zoom))`, clamped.
    pub fn lod_for(&self, _image: (u32, u32), max_lod: u32) -> u32 {
        if self.zoom >= 1.0 {
            return 0;
        }
        let l = (1.0 / self.zoom).log2().floor();
        (l.max(0.0) as u32).min(max_lod.saturating_sub(1))
    }
}
```

Prepend to `ferrolite-vt/src/residency.rs`:
```rust
//! Pure CPU residency bookkeeping: which tiles a view needs, and an LRU set with
//! a tile-count budget. No GPU — the streaming brain, fully testable headless.

use ferrolite_image::{tiles_per_level, TileCoord, TILE_SIZE};

use crate::transform::ViewTransform;

/// Virtual tiles the current view needs at its chosen LOD (visible rect only).
pub fn needed_tiles(
    image: (u32, u32),
    view: &ViewTransform,
    viewport: (f32, f32),
    level_count: u32,
) -> Vec<TileCoord> {
    let lod = view.lod_for(image, level_count);
    let (cols, rows) = tiles_per_level(image.0, image.1, lod);
    // Visible image-space rect (centered pan). Half-viewport in image px = (vp/2)/zoom.
    let half_w = (viewport.0 * 0.5) / view.zoom;
    let half_h = (viewport.1 * 0.5) / view.zoom;
    let cx = image.0 as f32 * 0.5 + view.pan.0;
    let cy = image.1 as f32 * 0.5 + view.pan.1;
    let lod_scale = (1u32 << lod) as f32; // image px per lod px
    let tile_px = TILE_SIZE as f32 * lod_scale; // image px covered by one tile at this lod
    let x0 = (((cx - half_w).max(0.0)) / tile_px).floor() as u32;
    let x1 = (((cx + half_w).max(0.0)) / tile_px).floor() as u32;
    let y0 = (((cy - half_h).max(0.0)) / tile_px).floor() as u32;
    let y1 = (((cy + half_h).max(0.0)) / tile_px).floor() as u32;
    let mut out = Vec::new();
    for y in y0..=y1.min(rows.saturating_sub(1)) {
        for x in x0..=x1.min(cols.saturating_sub(1)) {
            out.push(TileCoord { lod, x, y });
        }
    }
    out
}

/// LRU set of resident tiles under a fixed tile-count budget.
pub struct ResidencySet {
    capacity: usize,
    order: Vec<TileCoord>, // front = LRU
}

impl ResidencySet {
    pub fn new(capacity: usize) -> Self {
        Self { capacity: capacity.max(1), order: Vec::new() }
    }
    pub fn contains(&self, t: TileCoord) -> bool {
        self.order.contains(&t)
    }
    pub fn touch(&mut self, t: TileCoord) {
        if let Some(p) = self.order.iter().position(|&x| x == t) {
            self.order.remove(p);
        }
        self.order.push(t);
    }
    /// Insert `t` as MRU; return an evicted tile if over capacity.
    pub fn insert(&mut self, t: TileCoord) -> Option<TileCoord> {
        self.touch(t);
        if self.order.len() > self.capacity {
            Some(self.order.remove(0))
        } else {
            None
        }
    }
    /// Given the needed set, return (to_load = needed∖resident, to_evict =
    /// resident∖needed). Does not mutate; caller drives load/evict via jobs.
    pub fn diff(&self, needed: &[TileCoord]) -> (Vec<TileCoord>, Vec<TileCoord>) {
        let to_load = needed.iter().copied().filter(|t| !self.contains(*t)).collect();
        let to_evict = self
            .order
            .iter()
            .copied()
            .filter(|t| !needed.contains(t))
            .collect();
        (to_load, to_evict)
    }
}
```

Update `ferrolite-vt/src/lib.rs`:
```rust
mod residency;
mod source;
mod transform;

pub use residency::{needed_tiles, ResidencySet};
pub use source::{PyramidTileSource, TileSource};
pub use transform::ViewTransform;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ferrolite-vt transform:: residency::`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-vt/src/transform.rs ferrolite-vt/src/residency.rs ferrolite-vt/src/lib.rs
git commit -m "feat(vt): pure residency core — needed_tiles + LRU ResidencySet + ViewTransform"
```

---

### Task 9: Rung 1 — single-texture viewer + display shader (golden)

**Files:**
- Create: `ferrolite-vt/src/view.rs`, `ferrolite-vt/src/shaders/display.wgsl`, `ferrolite-vt/tests/golden.rs`, `ferrolite-vt/tests/common/mod.rs`
- Modify: `ferrolite-vt/src/lib.rs`

**Interfaces:**
- Consumes: `GpuContext`, `LinearRgbaF32`, `ViewTransform`.
- Produces:
  `pub struct VirtualTexture { ... }`
  `VirtualTexture::single_texture(ctx: &GpuContext, image: &LinearRgbaF32, target_format: wgpu::TextureFormat) -> Self`
  `VirtualTexture::render(&self, ctx: &GpuContext, pass: &mut wgpu::RenderPass<'_>, view: &ViewTransform, viewport: (f32,f32))`
  Helper for tests: `VirtualTexture::render_to_image(ctx, image, view, viewport, out_w, out_h) -> Vec<u8>` (offscreen Rgba8).

**Shader contract (`display.wgsl`):** a full-screen triangle vertex stage; a fragment stage sampling the image texture with a `Transform` uniform `{ zoom: f32, pan: vec2<f32>, viewport: vec2<f32>, image: vec2<f32> }`; converts sampled linear RGB → sRGB via the OETF on output; out-of-image UVs render the canvas background (`vec4(0.05,0.05,0.05,1.0)`).

- [ ] **Step 1: Write the failing golden test**

Create `ferrolite-vt/tests/common/mod.rs`:
```rust
use ferrolite_image::LinearRgbaF32;

/// A 4×4 image: left half red, right half green (linear).
pub fn split_image() -> LinearRgbaF32 {
    let (w, h) = (4u32, 4u32);
    let mut px = Vec::new();
    for _y in 0..h {
        for x in 0..w {
            if x < w / 2 {
                px.extend_from_slice(&[1.0, 0.0, 0.0, 1.0]);
            } else {
                px.extend_from_slice(&[0.0, 1.0, 0.0, 1.0]);
            }
        }
    }
    LinearRgbaF32::new(w, h, px).unwrap()
}

/// Max per-channel absolute difference between two equal-length RGBA8 buffers.
pub fn max_abs_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}
```

Create `ferrolite-vt/tests/golden.rs`:
```rust
mod common;
use ferrolite_gpu::GpuContext;
use ferrolite_vt::{ViewTransform, VirtualTexture};

const TOL: u8 = 4; // absorbs driver float differences

#[test]
fn rung1_fit_view_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping golden (expected in headless CI)");
        return;
    };
    let img = common::split_image();
    let (w, h) = (64u32, 64u32);
    let view = ViewTransform::fit((img.width, img.height), (w as f32, h as f32));
    let pixels = VirtualTexture::render_to_image(&ctx, &img, &view, (w as f32, h as f32), w, h);

    let golden_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rung1_fit.png");
    if std::env::var("UPDATE_GOLDEN").is_ok() || !std::path::Path::new(golden_path).exists() {
        image::save_buffer(golden_path, &pixels, w, h, image::ColorType::Rgba8).unwrap();
        eprintln!("wrote golden {golden_path}");
        return;
    }
    let golden = image::open(golden_path).unwrap().to_rgba8();
    assert_eq!(golden.dimensions(), (w, h));
    assert!(
        common::max_abs_diff(&pixels, golden.as_raw()) <= TOL,
        "rendered output drifted from golden beyond tolerance"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-vt --test golden`
Expected: FAIL — `VirtualTexture` not defined (compile error). (On headless CI it would skip — but it must compile first.)

- [ ] **Step 3: Write the shader**

Create `ferrolite-vt/src/shaders/display.wgsl`:
```wgsl
struct Transform {
    zoom: f32,
    _pad0: f32,
    pan: vec2<f32>,
    viewport: vec2<f32>,
    image: vec2<f32>,
};
@group(0) @binding(0) var img_tex: texture_2d<f32>;
@group(0) @binding(1) var img_samp: sampler;
@group(0) @binding(2) var<uniform> xf: Transform;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) screen_uv: vec2<f32>, // 0..1 across the viewport
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Full-screen triangle.
    var p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var out: VsOut;
    let xy = p[vid];
    out.pos = vec4(xy, 0.0, 1.0);
    out.screen_uv = (xy * 0.5 + vec2(0.5, 0.5)) * vec2(1.0, -1.0) + vec2(0.0, 1.0);
    return out;
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Screen pixel -> image pixel: center the image, apply pan, divide by zoom.
    let screen_px = in.screen_uv * xf.viewport;
    let center = xf.image * 0.5 + xf.pan;
    let img_px = center + (screen_px - xf.viewport * 0.5) / xf.zoom;
    let uv = img_px / xf.image;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return vec4(0.05, 0.05, 0.05, 1.0);
    }
    let lin = textureSampleLevel(img_tex, img_samp, uv, 0.0).rgb;
    return vec4(linear_to_srgb(lin), 1.0);
}
```

- [ ] **Step 4: Write the minimal implementation**

Create `ferrolite-vt/src/view.rs`:
```rust
//! VirtualTexture rung 1: the whole image as one `Rgba16Float` texture, sampled
//! by the display shader with a zoom/pan transform. Also the fallback path.

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use half::f16;
use wgpu::util::DeviceExt;

use crate::ViewTransform;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransformUniform {
    zoom: f32,
    _pad0: f32,
    pan: [f32; 2],
    viewport: [f32; 2],
    image: [f32; 2],
}

pub struct VirtualTexture {
    texture: wgpu::Texture,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    image_dims: (u32, u32),
}

impl VirtualTexture {
    pub fn single_texture(
        ctx: &GpuContext,
        image: &LinearRgbaF32,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let device = &ctx.device;
        // f32 -> f16 RGBA.
        let texels: Vec<f16> = image.pixels.iter().map(|&v| f16::from_f32(v)).collect();
        let texture = device.create_texture_with_data(
            &ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("vt-single"),
                size: wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&texels),
        );

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vt-bgl"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vt-display"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/display.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vt-pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vt-pipeline"),
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
                targets: &[Some(target_format.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vt-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            texture,
            bind_group_layout: bgl,
            sampler,
            pipeline,
            image_dims: (image.width, image.height),
        }
    }

    pub fn render(
        &self,
        ctx: &GpuContext,
        pass: &mut wgpu::RenderPass<'_>,
        view: &ViewTransform,
        viewport: (f32, f32),
    ) {
        let uniform = TransformUniform {
            zoom: view.zoom,
            _pad0: 0.0,
            pan: [view.pan.0, view.pan.1],
            viewport: [viewport.0, viewport.1],
            image: [self.image_dims.0 as f32, self.image_dims.1 as f32],
        };
        let ubuf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vt-xf"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let tview = self.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-bind"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&tview) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: ubuf.as_entire_binding() },
            ],
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Offscreen render to an `Rgba8Unorm` image (golden tests).
    pub fn render_to_image(
        ctx: &GpuContext,
        image: &LinearRgbaF32,
        view: &ViewTransform,
        viewport: (f32, f32),
        out_w: u32,
        out_h: u32,
    ) -> Vec<u8> {
        let vt = Self::single_texture(ctx, image, wgpu::TextureFormat::Rgba8Unorm);
        let target = ctx.render_target(out_w, out_h, wgpu::TextureFormat::Rgba8Unorm);
        let tview = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vt-offscreen"),
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
            vt.render(ctx, &mut pass, view, viewport);
        }
        ctx.queue.submit([enc.finish()]);
        ctx.read_rgba8(&target, out_w, out_h)
    }
}
```

Add `wgpu`'s `util` requires the `util` feature? In wgpu 22 `wgpu::util` is available by default. Confirm `create_texture_with_data` / `create_buffer_init` compile; they live in `wgpu::util::DeviceExt`.

Update `ferrolite-vt/src/lib.rs`:
```rust
mod residency;
mod source;
mod transform;
mod view;

pub use residency::{needed_tiles, ResidencySet};
pub use source::{PyramidTileSource, TileSource};
pub use transform::ViewTransform;
pub use view::VirtualTexture;
```

- [ ] **Step 5: Generate the golden on a GPU machine, then run the test**

On a machine with a GPU:
Run: `UPDATE_GOLDEN=1 cargo test -p ferrolite-vt --test golden rung1_fit_view_matches_golden`
Then: `cargo test -p ferrolite-vt --test golden`
Expected: PASS (writes then matches `tests/fixtures/rung1_fit.png`). On headless CI: skips.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-vt/src/view.rs ferrolite-vt/src/shaders/ ferrolite-vt/src/lib.rs ferrolite-vt/tests/
git commit -m "feat(vt): rung 1 single-texture viewer + display shader + golden test"
```

---

### Task 10: Rung 2 — static tiled mip pyramid + per-fragment LOD (golden)

**Files:**
- Modify: `ferrolite-vt/src/view.rs`, `ferrolite-vt/src/shaders/display.wgsl`, `ferrolite-vt/tests/golden.rs`

**Interfaces:**
- Consumes: `TileSource`, `PyramidTileSource`.
- Produces:
  `VirtualTexture::tiled_resident(ctx, source: &dyn TileSource, target_format) -> Self` — uploads ALL tiles of ALL levels into a 2-D-array texture (one array layer per (lod,tile) slot) plus a CPU-built slot map; shader selects LOD from screen-space derivatives and samples the right slot.

**Design note:** rung 2 keeps everything resident, so the slot map is a complete `(lod,x,y)->layer` table uploaded as a uniform/storage buffer. The shader computes desired LOD via `textureNumLevels`-style manual derivative: `lod = clamp(log2(max(len(dpdx(img_px)), len(dpdy(img_px)))))`. Reuse the existing `Transform` uniform; add a storage buffer `slots: array<u32>` indexed by a CPU-provided `level_offsets`.

- [ ] **Step 1: Add the failing golden case**

Append to `ferrolite-vt/tests/golden.rs`:
```rust
#[test]
fn rung2_tiled_matches_single_texture() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // A larger gradient so multiple tiles exist.
    let (iw, ih) = (300u32, 200u32);
    let mut px = Vec::new();
    for y in 0..ih {
        for x in 0..iw {
            px.extend_from_slice(&[x as f32 / iw as f32, y as f32 / ih as f32, 0.25, 1.0]);
        }
    }
    let img = ferrolite_image::LinearRgbaF32::new(iw, ih, px).unwrap();
    let (w, h) = (128u32, 128u32);
    let view = ViewTransform::fit((iw, ih), (w as f32, h as f32));

    let single = VirtualTexture::render_to_image(&ctx, &img, &view, (w as f32, h as f32), w, h);
    let src = ferrolite_vt::PyramidTileSource::new(img);
    let tiled = VirtualTexture::render_tiled_to_image(&ctx, &src, &view, (w as f32, h as f32), w, h);

    // At fit zoom the tiled path samples a coarse LOD; allow a generous tolerance
    // vs the single-texture reference (different filtering), but they must broadly agree.
    assert!(common::max_abs_diff(&single, &tiled) <= 24, "tiled diverges from single-texture reference");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p ferrolite-vt --test golden rung2`
Expected: FAIL — `render_tiled_to_image` / `tiled_resident` not defined.

- [ ] **Step 3: Extend the shader**

Add a tiled fragment entry point `fs_tiled` to `display.wgsl` that: computes `img_px` as in `fs_main`; derives LOD from `dpdx/dpdy` of `img_px`; looks up the slot layer for `(lod, tile_x, tile_y)` via a `slots` storage buffer and `level_offsets`; samples a `texture_2d_array<f32>` at the slot layer with the in-tile UV; sRGB-encodes. Bind group adds: `@binding(3) var tiles: texture_2d_array<f32>;` `@binding(4) var<storage, read> slots: array<u32>;` `@binding(5) var<uniform> meta: TileMeta { level_count, level_offsets: array<vec4<u32>, 8> (packed) }`.

```wgsl
// (appended to display.wgsl)
@group(0) @binding(3) var tiles: texture_2d_array<f32>;
@group(0) @binding(4) var<storage, read> slots: array<u32>;

struct TileMeta {
    level_count: u32,
    _pad: vec3<u32>,
    // tiles-per-row and flat slot offset for up to 8 levels (x = cols, y = offset)
    levels: array<vec4<u32>, 8>,
};
@group(0) @binding(5) var<uniform> meta: TileMeta;

fn pick_lod(img_px: vec2<f32>) -> u32 {
    let dx = length(dpdx(img_px));
    let dy = length(dpdy(img_px));
    let d = max(max(dx, dy), 1.0);
    return min(u32(max(log2(d), 0.0)), meta.level_count - 1u);
}

@fragment
fn fs_tiled(in: VsOut) -> @location(0) vec4<f32> {
    let screen_px = in.screen_uv * xf.viewport;
    let center = xf.image * 0.5 + xf.pan;
    let img_px = center + (screen_px - xf.viewport * 0.5) / xf.zoom;
    if (img_px.x < 0.0 || img_px.x >= xf.image.x || img_px.y < 0.0 || img_px.y >= xf.image.y) {
        return vec4(0.05, 0.05, 0.05, 1.0);
    }
    let lod = pick_lod(img_px);
    let lod_px = img_px / f32(1u << lod);
    let tx = u32(lod_px.x) / 256u;
    let ty = u32(lod_px.y) / 256u;
    let cols = meta.levels[lod].x;
    let offset = meta.levels[lod].y;
    let slot = slots[offset + ty * cols + tx];
    let in_tile = (lod_px - vec2(f32(tx * 256u), f32(ty * 256u))) / 256.0;
    let lin = textureSampleLevel(tiles, img_samp, in_tile, slot, 0.0).rgb;
    return vec4(linear_to_srgb(lin), 1.0);
}
```

- [ ] **Step 4: Implement `tiled_resident` + `render_tiled_to_image`**

In `ferrolite-vt/src/view.rs`, add a second constructor that allocates a `texture_2d_array` of `Rgba16Float` with `layers = total tile count across levels`, uploads each `source.tile(coord)` (f32→f16) into its layer, builds the `slots` storage buffer and `TileMeta` (cols + flat offset per level), and a pipeline using `fs_tiled`. Add `render_tiled` (records the draw) and `render_tiled_to_image` (offscreen, mirrors `render_to_image`). Keep rung-1 fields; store an enum or `Option` for the tiled resources. Show the full code:

```rust
// (additions to view.rs — fields, constructor, render, offscreen helper)
// 1) Add to VirtualTexture: optional tiled resources.
//    tiled: Option<TiledResources>, where TiledResources holds the array
//    texture, slots buffer, meta buffer, bind group layout, pipeline.
// 2) tiled_resident(ctx, source, target_format): build the above.
// 3) render_tiled(ctx, pass, view, viewport): bind + draw 0..3.
// 4) render_tiled_to_image(ctx, source, view, viewport, w, h): offscreen.
```

> Implementer: write the concrete `TiledResources` struct and the four methods, following the exact bind-group layout in the shader (bindings 0–2 as in rung 1, plus 3=array texture, 4=slots storage, 5=meta uniform). The `slots` buffer is `Vec<u32>` of `total_tiles` entries where entry `level_offset[lod] + y*cols + x = layer_index`. The golden test in Step 1 is the gate; iterate until `max_abs_diff <= 24`.

- [ ] **Step 5: Generate golden / run on GPU machine**

Run: `cargo test -p ferrolite-vt --test golden rung2`
Expected: PASS on GPU; skips on headless CI.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-vt/src/view.rs ferrolite-vt/src/shaders/display.wgsl ferrolite-vt/tests/golden.rs
git commit -m "feat(vt): rung 2 static tiled mip pyramid + per-fragment LOD selection"
```

**← G2 milestone validated here (smooth tiled zoom/pan).**

---

### Task 11: Rung 3 — physical tile pool + residency + LRU eviction + job loads

**Files:**
- Create: `ferrolite-vt/src/pool.rs`
- Modify: `ferrolite-vt/src/view.rs`, `ferrolite-vt/src/shaders/display.wgsl`, `ferrolite-vt/src/lib.rs`

**Interfaces:**
- Consumes: `ResidencySet`, `needed_tiles`, `ferrolite_jobs::{JobSystem, Priority, CancelToken}`, `TileSource`.
- Produces:
  `pub struct TilePool { ... }` — a fixed array texture of N physical slots (`Rgba16Float`, `TILE_SIZE`²); `TilePool::new(ctx, slot_count)`, `upload(ctx, slot, &LinearRgbaF32)`, `slot_count()`.
  `VirtualTexture::streaming(ctx, source: Arc<dyn TileSource + Send + Sync>, jobs: Arc<JobSystem>, budget_tiles, target_format) -> Self`
  `VirtualTexture::request_view(&mut self, ctx, view, viewport)` — computes needed, diffs residency, enqueues missing-tile loads as `Visible` jobs, evicts LRU, updates the `(lod,tile)->slot` indirection used by the shader's coarse-LOD fallback.

**Design note:** rung 3 still uses a flat slot→layer mapping but now slots are a *budget-limited pool* and the slot map is updated each frame. Missing tiles render from the **coarsest resident LOD** (walk up until a resident tile is found) — the shader's `fs_tiled` is extended to loop up levels when the looked-up slot is the sentinel `NOT_RESIDENT = 0xFFFFFFFF`.

- [ ] **Step 1: Write the failing test (pure pool + request logic)**

Create `ferrolite-vt/src/pool.rs` with a CPU-only allocation test (no GPU needed for the slot-allocation logic):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_image::TileCoord;

    #[test]
    fn slot_allocator_reuses_evicted_slots() {
        let mut a = SlotAllocator::new(2);
        let s0 = a.alloc(TileCoord { lod: 0, x: 0, y: 0 }).unwrap();
        let s1 = a.alloc(TileCoord { lod: 0, x: 1, y: 0 }).unwrap();
        assert_ne!(s0, s1);
        assert!(a.alloc(TileCoord { lod: 0, x: 2, y: 0 }).is_none(), "pool full");
        a.free(TileCoord { lod: 0, x: 0, y: 0 });
        let s2 = a.alloc(TileCoord { lod: 0, x: 2, y: 0 }).unwrap();
        assert_eq!(s2, s0, "freed slot reused");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p ferrolite-vt pool::`
Expected: FAIL — `SlotAllocator` not defined.

- [ ] **Step 3: Implement `SlotAllocator` + `TilePool`**

Prepend to `ferrolite-vt/src/pool.rs`:
```rust
//! Physical tile pool: a budget-limited array texture of `TILE_SIZE`² slots,
//! plus a CPU allocator mapping resident tiles to slot indices.

use std::collections::HashMap;

use ferrolite_gpu::GpuContext;
use ferrolite_image::{LinearRgbaF32, TileCoord, TILE_SIZE};
use half::f16;

pub const NOT_RESIDENT: u32 = 0xFFFF_FFFF;

/// Maps tiles to physical slot indices; recycles freed slots.
pub struct SlotAllocator {
    capacity: u32,
    free: Vec<u32>,
    map: HashMap<TileCoord, u32>,
}

impl SlotAllocator {
    pub fn new(capacity: u32) -> Self {
        Self { capacity, free: (0..capacity).rev().collect(), map: HashMap::new() }
    }
    pub fn slot_of(&self, t: TileCoord) -> Option<u32> {
        self.map.get(&t).copied()
    }
    pub fn alloc(&mut self, t: TileCoord) -> Option<u32> {
        if let Some(&s) = self.map.get(&t) {
            return Some(s);
        }
        let s = self.free.pop()?;
        self.map.insert(t, s);
        Some(s)
    }
    pub fn free(&mut self, t: TileCoord) {
        if let Some(s) = self.map.remove(&t) {
            self.free.push(s);
        }
    }
    pub fn capacity(&self) -> u32 {
        self.capacity
    }
}

/// GPU side: an array texture of `capacity` `Rgba16Float` `TILE_SIZE`² layers.
pub struct TilePool {
    texture: wgpu::Texture,
    capacity: u32,
}

impl TilePool {
    pub fn new(ctx: &GpuContext, capacity: u32) -> Self {
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vt-tile-pool"),
            size: wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: capacity,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        Self { texture, capacity }
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Upload one tile's pixels into physical `slot` (array layer).
    pub fn upload(&self, ctx: &GpuContext, slot: u32, tile: &LinearRgbaF32) {
        let texels: Vec<f16> = tile.pixels.iter().map(|&v| f16::from_f32(v)).collect();
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: slot },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&texels),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(TILE_SIZE * 4 * 2), // RGBA * f16
                rows_per_image: Some(TILE_SIZE),
            },
            wgpu::Extent3d { width: TILE_SIZE, height: TILE_SIZE, depth_or_array_layers: 1 },
        );
    }
}
```

- [ ] **Step 4: Wire streaming into `VirtualTexture`**

In `view.rs` add `streaming(...)` + `request_view(...)`. `request_view` computes `needed_tiles`, calls `ResidencySet::diff`, for each `to_evict` frees its slot, for each `to_load` (not already requested) submits a `Visible` job that calls `source.tile(coord)` and sends the result back over a channel; the app/owner drains the channel and calls `pool.upload` + `allocator.alloc` + `residency.touch` on the main thread (GPU access is single-threaded). Update the per-frame `slots` buffer with `NOT_RESIDENT` sentinels for missing tiles. Extend `fs_tiled` with the coarse-LOD fallback loop:

```wgsl
// replace the slot lookup in fs_tiled with a fallback walk:
var lvl = lod;
var slot = NOT_RESIDENT;
loop {
    let cols = meta.levels[lvl].x;
    let offset = meta.levels[lvl].y;
    let lp = img_px / f32(1u << lvl);
    let tx = u32(lp.x) / 256u;
    let ty = u32(lp.y) / 256u;
    let cand = slots[offset + ty * cols + tx];
    if (cand != NOT_RESIDENT) { slot = cand; break; }
    if (lvl + 1u >= meta.level_count) { break; }
    lvl = lvl + 1u;
}
if (slot == NOT_RESIDENT) { return vec4(0.05, 0.05, 0.05, 1.0); }
```
(Define `const NOT_RESIDENT: u32 = 0xFFFFFFFFu;` in WGSL. The in-tile UV must use `lvl`, not `lod`.)

> The job-load channel + main-thread drain mirrors the app's existing thumbnail-job pattern (`ferrolite-app/src/events.rs` + `app.rs` `try_recv` loop). Cancellation: `request_view` holds the `JobHandle`s of in-flight loads keyed by `TileCoord`; tiles no longer needed are cancelled.

- [ ] **Step 5: Add a CPU integration test for request/evict accounting**

Append to `ferrolite-vt/src/residency.rs` tests (or a new `streaming` test module) a test that drives `needed_tiles` + `ResidencySet` + `SlotAllocator` together over two viewports and asserts the second view evicts the now-unneeded tiles and loads the newly visible ones (no GPU):
```rust
#[test]
fn panning_evicts_offscreen_and_loads_newly_visible() {
    use crate::pool::SlotAllocator;
    use crate::transform::ViewTransform;
    let image = (2048u32, 2048u32);
    let vp = (256.0f32, 256.0f32);
    let mut res = ResidencySet::new(64);
    let mut alloc = SlotAllocator::new(64);
    // View A: top-left.
    let a = ViewTransform { zoom: 1.0, pan: (-800.0, -800.0) };
    for t in needed_tiles(image, &a, vp, 4) {
        res.insert(t);
        alloc.alloc(t);
    }
    // View B: bottom-right (disjoint).
    let b = ViewTransform { zoom: 1.0, pan: (800.0, 800.0) };
    let needed_b = needed_tiles(image, &b, vp, 4);
    let (to_load, to_evict) = res.diff(&needed_b);
    assert!(!to_load.is_empty(), "new tiles needed");
    assert!(!to_evict.is_empty(), "old tiles evicted");
    for t in &to_evict { alloc.free(*t); }
    for t in &to_load { assert!(alloc.alloc(*t).is_some(), "freed slots make room"); }
}
```

- [ ] **Step 6: Run tests + golden on GPU machine**

Run: `cargo test -p ferrolite-vt pool:: residency::`
Expected: PASS. Add/confirm a streaming golden if practical; otherwise rely on the rung-2 golden + CPU accounting.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-vt/src/pool.rs ferrolite-vt/src/view.rs ferrolite-vt/src/shaders/display.wgsl ferrolite-vt/src/lib.rs ferrolite-vt/src/residency.rs
git commit -m "feat(vt): rung 3 physical tile pool + LRU residency + job-driven tile loads + coarse-LOD fallback"
```

---

### Task 12: Rung 4 — page-table indirection + GPU feedback pass

**Files:**
- Create: `ferrolite-vt/src/page_table.rs`
- Modify: `ferrolite-vt/src/view.rs`, `ferrolite-vt/src/shaders/display.wgsl`, `ferrolite-vt/src/lib.rs`

**Interfaces:**
- Produces:
  `pub struct PageTable { ... }` — an `Rg32Uint` indirection texture (one texel per virtual tile per level, laid out by `level_offsets`) holding `(slot, flags)`; `PageTable::new(ctx, total_tiles)`, `update(ctx, &SlotAllocator, level_layout)`.
  `pub struct FeedbackBuffer { ... }` — a GPU storage buffer the display shader marks with needed `(lod,tile)` ids; `FeedbackBuffer::new(ctx, total_tiles)`, `clear(ctx)`, `read_back(ctx) -> Vec<TileCoord>` (async map; one-frame latent).
  `VirtualTexture::request_view_feedback(&mut self, ctx)` — reads last frame's feedback, diffs residency, loads/evicts, updates page table.

**Design note:** the display shader now (a) reads the page table to find a tile's slot (with the same coarse fallback), and (b) writes the desired `(lod,tile)` into the feedback buffer (`atomicOr` a presence bit per tile id). The CPU reads it back the next frame. This replaces the CPU-side `needed_tiles` rect estimate with GPU-truth visibility. Keep `needed_tiles` as the rung-3 path / fallback.

- [ ] **Step 1: Write the failing test (page-table encoding, CPU)**

Create `ferrolite-vt/src/page_table.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_index_round_trips_level_tile() {
        // level_offsets: L0 at 0 (4x4=16 tiles), L1 at 16 (2x2=4), L2 at 20 (1).
        let layout = LevelLayout::new(&[(4, 4), (2, 2), (1, 1)]);
        assert_eq!(layout.flat_index(0, 0, 0), 0);
        assert_eq!(layout.flat_index(0, 3, 3), 15);
        assert_eq!(layout.flat_index(1, 0, 0), 16);
        assert_eq!(layout.flat_index(2, 0, 0), 20);
        assert_eq!(layout.total(), 21);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p ferrolite-vt page_table::`
Expected: FAIL — `LevelLayout` not defined.

- [ ] **Step 3: Implement `LevelLayout`, `PageTable`, `FeedbackBuffer`**

Prepend to `ferrolite-vt/src/page_table.rs`:
```rust
//! Rung 4: page-table indirection + a GPU feedback buffer. The display shader
//! resolves virtual→physical via the page table and marks needed tiles into the
//! feedback buffer; the CPU reads it back (one frame latent) to drive streaming.

use ferrolite_gpu::GpuContext;
use ferrolite_image::TileCoord;

/// Flat-indexing of all tiles across all levels (cols/rows per level + offsets).
pub struct LevelLayout {
    dims: Vec<(u32, u32)>, // (cols, rows) per lod
    offsets: Vec<u32>,
    total: u32,
}

impl LevelLayout {
    pub fn new(dims: &[(u32, u32)]) -> Self {
        let mut offsets = Vec::with_capacity(dims.len());
        let mut acc = 0u32;
        for &(c, r) in dims {
            offsets.push(acc);
            acc += c * r;
        }
        Self { dims: dims.to_vec(), offsets, total: acc }
    }
    pub fn flat_index(&self, lod: u32, x: u32, y: u32) -> u32 {
        let (cols, _rows) = self.dims[lod as usize];
        self.offsets[lod as usize] + y * cols + x
    }
    pub fn total(&self) -> u32 {
        self.total
    }
    pub fn offsets(&self) -> &[u32] {
        &self.offsets
    }
    pub fn from_flat(&self, flat: u32) -> TileCoord {
        // Inverse mapping for feedback read-back.
        let lod = self
            .offsets
            .iter()
            .rposition(|&o| flat >= o)
            .unwrap_or(0);
        let local = flat - self.offsets[lod];
        let (cols, _) = self.dims[lod];
        TileCoord { lod: lod as u32, x: local % cols, y: local / cols }
    }
}
```
Then add `PageTable` (an `Rg32Uint` texture sized to cover `total` texels — store as a 1×total or a square; simplest: a width-`total`, height-1 texture) with `new`/`update(slots: &[u32])` writing `(slot, flags)` per texel via `queue.write_texture`; and `FeedbackBuffer` (a `STORAGE | COPY_SRC` buffer of `total` u32s) with `new`, `clear` (write zeros), and `read_back` (copy to a `MAP_READ` buffer, `map_async`, `device.poll(Wait)`, collect set bits → `Vec<TileCoord>` via `LevelLayout::from_flat`). Mirror the readback mechanics of `GpuContext::read_rgba8`.

- [ ] **Step 4: Extend the shader with page-table read + feedback write**

In `display.wgsl` add an `fs_sparse` entry: resolve slot from a `page_table` texture (`textureLoad`) with the coarse fallback; `atomicOr` (or plain store of `1u`) the visited tile's flat index into a `feedback: array<atomic<u32>>` storage binding; sample the pool at the resolved slot. Add bindings `@binding(6) var page_table: texture_2d<u32>;` and `@binding(7) var<storage, read_write> feedback: array<atomic<u32>>;`. Provide full WGSL for `fs_sparse` following the rung-3 fallback structure, replacing the `slots[...]` read with `textureLoad(page_table, vec2<i32>(i32(flat), 0), 0).r`.

- [ ] **Step 5: Implement `request_view_feedback` + an end-to-end GPU test**

`request_view_feedback(ctx)`: `feedback.read_back(ctx)` → needed set; `residency.diff`; free/evict; submit `Visible` load jobs; on completion upload to pool + `allocator.alloc`; `page_table.update`; `feedback.clear`. Add a golden/behavioral GPU test (adapter-gated) that renders two frames and asserts that after frame 1's feedback is processed, the tiles covering the viewport are resident (`allocator.slot_of(t).is_some()` for the center tile). Keep it skip-on-no-adapter.

```rust
// ferrolite-vt/tests/golden.rs (append)
#[test]
fn rung4_feedback_makes_center_tile_resident() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // Build a streaming+feedback VirtualTexture over a multi-tile gradient,
    // render one frame, process feedback, and assert the center tile resolved.
    // (Construct via the rung-4 constructor; exact assertion through a test-only
    // accessor `VirtualTexture::is_resident(TileCoord) -> bool`.)
    // Implementer fills this in against the rung-4 API.
}
```
Add a `#[cfg(any(test, feature = "test-introspection"))]` accessor `VirtualTexture::is_resident(&self, t: TileCoord) -> bool` for the assertion.

- [ ] **Step 6: Run tests on GPU machine**

Run: `cargo test -p ferrolite-vt`
Expected: PASS on GPU (feedback round-trip resolves tiles); skips GPU cases on headless CI; CPU `page_table::` tests pass everywhere.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-vt/src/page_table.rs ferrolite-vt/src/view.rs ferrolite-vt/src/shaders/display.wgsl ferrolite-vt/src/lib.rs ferrolite-vt/tests/golden.rs
git commit -m "feat(vt): rung 4 page-table indirection + GPU feedback pass (full sparse VT)"
```

---

## Phase 5 — `ferrolite-app` viewer & two-tier load

### Task 13: `ViewerState` + open/close routing + pan/zoom input math

**Files:**
- Create: `ferrolite-app/src/viewer/mod.rs`
- Modify: `ferrolite-app/src/state.rs`, `ferrolite-app/src/lib.rs` (add `mod viewer;`), `ferrolite-app/src/library/grid.rs`, `ferrolite-app/src/app.rs`, `ferrolite-app/Cargo.toml`

**Interfaces:**
- Consumes: `ferrolite_vt::ViewTransform`, `ferrolite_image` types.
- Produces:
  `pub struct ViewerState { pub image_id: i64, pub path: std::path::PathBuf, pub view: ViewTransform, /* textures/VT added in Tasks 14–15 */ }`
  `ViewerState::open(image_id, path) -> Self`
  `pub fn apply_zoom(view: ViewTransform, scroll: f32, cursor: (f32,f32), viewport: (f32,f32)) -> ViewTransform` (zoom about cursor)
  `pub fn apply_pan(view: ViewTransform, drag_delta: (f32,f32)) -> ViewTransform`

- [ ] **Step 1: Add deps + module wiring**

`ferrolite-app/Cargo.toml` `[dependencies]` add:
```toml
ferrolite-gpu = { workspace = true }
ferrolite-vt = { workspace = true }
half = { workspace = true }
```
`ferrolite-app/src/lib.rs` (and `main.rs` module list): add `mod viewer;`.

- [ ] **Step 2: Write the failing test**

Create `ferrolite-app/src/viewer/mod.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_vt::ViewTransform;

    #[test]
    fn zoom_keeps_cursor_point_stationary() {
        let v = ViewTransform { zoom: 1.0, pan: (0.0, 0.0) };
        let viewport = (100.0, 100.0);
        let cursor = (75.0, 50.0); // right of center
        let z = apply_zoom(v, 1.0, cursor, viewport); // scroll up = zoom in
        assert!(z.zoom > 1.0, "scroll up zooms in");
        // Panning should shift toward the cursor side (pan.x increases).
        assert!(z.pan.0 > 0.0, "zoom about off-center cursor pans toward it");
    }

    #[test]
    fn pan_translates_by_delta_over_zoom() {
        let v = ViewTransform { zoom: 2.0, pan: (0.0, 0.0) };
        let p = apply_pan(v, (20.0, -10.0)); // drag right/up in screen px
        assert!((p.pan.0 + 10.0).abs() < 1e-6, "screen delta / zoom, inverted for pan");
        assert!((p.pan.1 - 5.0).abs() < 1e-6);
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p ferrolite-app viewer::`
Expected: FAIL — `apply_zoom`/`apply_pan`/`ViewerState` not defined.

- [ ] **Step 4: Write minimal implementation**

Prepend to `ferrolite-app/src/viewer/mod.rs`:
```rust
//! Single-image viewer state + pure pan/zoom input math. The two-tier load and
//! GPU wiring are layered on in later tasks.

use std::path::PathBuf;

use ferrolite_vt::ViewTransform;

pub struct ViewerState {
    pub image_id: i64,
    pub path: PathBuf,
    pub view: ViewTransform,
}

impl ViewerState {
    pub fn open(image_id: i64, path: PathBuf) -> Self {
        // Fit is computed once the viewport size is known (first paint);
        // start at zoom 1 centered as a placeholder.
        Self { image_id, path, view: ViewTransform { zoom: 1.0, pan: (0.0, 0.0) } }
    }
}

/// Zoom about the cursor: keep the image point under the cursor fixed.
pub fn apply_zoom(
    view: ViewTransform,
    scroll: f32,
    cursor: (f32, f32),
    viewport: (f32, f32),
) -> ViewTransform {
    let factor = (1.0 + scroll * 0.1).max(0.05);
    let new_zoom = (view.zoom * factor).clamp(0.01, 64.0);
    // Image-space point under the cursor before zoom:
    let center = (view.pan.0 + viewport.0 * 0.5, view.pan.1 + viewport.1 * 0.5);
    let img_pt = (
        center.0 + (cursor.0 - viewport.0 * 0.5) / view.zoom,
        center.1 + (cursor.1 - viewport.1 * 0.5) / view.zoom,
    );
    // New pan so img_pt stays under cursor at new_zoom.
    let new_center = (
        img_pt.0 - (cursor.0 - viewport.0 * 0.5) / new_zoom,
        img_pt.1 - (cursor.1 - viewport.1 * 0.5) / new_zoom,
    );
    ViewTransform {
        zoom: new_zoom,
        pan: (new_center.0 - viewport.0 * 0.5, new_center.1 - viewport.1 * 0.5),
    }
}

/// Pan by a screen-space drag delta (image-space translation is delta/zoom,
/// inverted so dragging right moves the image right).
pub fn apply_pan(view: ViewTransform, drag_delta: (f32, f32)) -> ViewTransform {
    ViewTransform {
        zoom: view.zoom,
        pan: (
            view.pan.0 - drag_delta.0 / view.zoom,
            view.pan.1 - drag_delta.1 / view.zoom,
        ),
    }
}
```
Add `pub viewer: Option<viewer::ViewerState>` to `AppState` (in `state.rs`), initialized `None` in both `new()` and `for_test()`.

> Note: the `apply_zoom` `pan.0 > 0` assertion encodes that `pan` is the image-space offset of the viewport's top-left from the image's center-origin; keep the convention consistent with `ViewTransform` usage in `ferrolite-vt` (pan = image-px offset of viewport center). If the sign convention in `ViewTransform::fit`/`needed_tiles` differs, align all three and adjust the test's expected sign — the *invariant under test* is "the cursor point stays put", not the raw sign.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p ferrolite-app viewer::`
Expected: PASS (2 tests).

- [ ] **Step 6: Open/close routing (no new test; manual + compile)**

In `library/grid.rs`, when a cell is double-clicked, set `state.viewer = Some(ViewerState::open(record.id, record.path()))`. In `app.rs` central panel: `if let Some(_) = self.state.viewer { viewer::paint(...) } else { grid::show(...) }`. Add Esc handling: `if ctx.input(|i| i.key_pressed(egui::Key::Escape)) { self.state.viewer = None; }`. (`viewer::paint` is a stub here that clears the canvas; real paint lands in Task 14.)

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/Cargo.toml ferrolite-app/src/viewer/ ferrolite-app/src/state.rs ferrolite-app/src/lib.rs ferrolite-app/src/main.rs ferrolite-app/src/library/grid.rs ferrolite-app/src/app.rs
git commit -m "feat(app): viewer state + open/close routing + pure pan/zoom math"
```

---

### Task 14: Tier-1 preview load + display in the viewer

**Files:**
- Modify: `ferrolite-app/src/viewer/mod.rs`, `ferrolite-app/src/viewer/load.rs` (create), `ferrolite-app/src/events.rs`, `ferrolite-app/src/app.rs`

**Interfaces:**
- Consumes: `ferrolite_decode::decode_preview`, `ferrolite_jobs`, `GpuContext`, `ferrolite_vt::VirtualTexture`, `ferrolite_image::{ImageBuffer, LinearRgbaF32}`.
- Produces:
  `AppEvent::PreviewReady { image_id: i64, image: ferrolite_image::ImageBuffer }`
  `viewer::load::spawn_preview(state, image_id, path, kind)` — submits an `Interactive` decode-preview job, sends `PreviewReady`.
  `ViewerState::set_preview(ctx, &GpuContext, &ImageBuffer)` — builds a rung-1 `VirtualTexture` from the preview (convert RGB8/RGBA8 → `LinearRgbaF32`, applying sRGB→linear) and fits the view.

- [ ] **Step 1: Write the failing test (RGB8 → LinearRgbaF32 conversion is pure)**

In `ferrolite-app/src/viewer/load.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_image::{ImageBuffer, PixelFormat};

    #[test]
    fn srgb8_to_linear_inverts_gamma() {
        // mid-gray 188/255 sRGB ~= 0.5 linear.
        let buf = ImageBuffer::new(1, 1, PixelFormat::Rgb8, vec![188, 188, 188]).unwrap();
        let lin = preview_to_linear(&buf);
        assert_eq!((lin.width, lin.height), (1, 1));
        assert!((lin.pixels[0] - 0.5).abs() < 0.02, "sRGB decode ~0.5");
        assert!((lin.pixels[3] - 1.0).abs() < 1e-6, "alpha opaque");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p ferrolite-app load::`
Expected: FAIL — `preview_to_linear` not defined.

- [ ] **Step 3: Implement conversion + load spawn**

Create `ferrolite-app/src/viewer/load.rs`:
```rust
//! Two-tier viewer load: tier-1 embedded preview (this task) and tier-2 full
//! decode → demosaic → VT (Task 15). All decode work runs on the job system.

use ferrolite_image::{ImageBuffer, LinearRgbaF32, PixelFormat};

/// sRGB-encoded 8-bit preview → display-linear RGBA f32.
pub fn preview_to_linear(buf: &ImageBuffer) -> LinearRgbaF32 {
    let ch = buf.format.channels();
    let n = (buf.width * buf.height) as usize;
    let mut px = Vec::with_capacity(n * 4);
    let srgb_to_lin = |u: u8| -> f32 {
        let c = u as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    for i in 0..n {
        let base = i * ch;
        let r = srgb_to_lin(buf.pixels[base]);
        let g = srgb_to_lin(buf.pixels[base + 1]);
        let b = srgb_to_lin(buf.pixels[base + 2]);
        let a = if matches!(buf.format, PixelFormat::Rgba8) {
            buf.pixels[base + 3] as f32 / 255.0
        } else {
            1.0
        };
        px.extend_from_slice(&[r, g, b, a]);
    }
    LinearRgbaF32::new(buf.width, buf.height, px).expect("preview length")
}
```
Add `spawn_preview` submitting an `Interactive` job that calls `ferrolite_decode::decode_preview(path, kind)` and sends `AppEvent::PreviewReady`. Add the `PreviewReady` variant to `events.rs` and handle it in `app.rs`'s `try_recv` loop: build/replace the viewer's rung-1 `VirtualTexture` via `ViewerState::set_preview` and compute `ViewTransform::fit`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p ferrolite-app load::`
Expected: PASS.

- [ ] **Step 5: Wire viewer paint (manual verification)**

`ViewerState` gains `vt: Option<ferrolite_vt::VirtualTexture>` and `viewport: (f32,f32)`. `viewer::paint` allocates a canvas rect, reads scroll/drag for `apply_zoom`/`apply_pan`, and records the VT render via an `egui_wgpu::Callback` (mirror `canvas/callback.rs`, but the callback borrows the viewer's `VirtualTexture` from `callback_resources` or via a per-frame prepare). On `open`, call `spawn_preview`.

> The egui↔wgpu integration must follow the existing `canvas/callback.rs` pattern: the `VirtualTexture` lives in `callback_resources`; `paint` enqueues a `CallbackTrait` whose `paint` calls `vt.render(...)`. Because the VT needs per-frame uniforms (view/viewport), pass them through the callback struct.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/viewer/ ferrolite-app/src/events.rs ferrolite-app/src/app.rs
git commit -m "feat(app): tier-1 preview decode + display in viewer (first pixel)"
```

---

### Task 15: Tier-2 full decode → QuadBin → VT + crossfade + navigation cancel

**Files:**
- Modify: `ferrolite-app/src/viewer/mod.rs`, `ferrolite-app/src/viewer/load.rs`, `ferrolite-app/src/events.rs`, `ferrolite-app/src/app.rs`

**Interfaces:**
- Consumes: `ferrolite_decode::{decode_full, QuadBin, DemosaicToRgb16f}`, `ferrolite_vt::{PyramidTileSource, VirtualTexture}`, `ferrolite_jobs`.
- Produces:
  `AppEvent::FullDecoded { image_id: i64, image: ferrolite_image::LinearRgbaF32 }`
  `AppEvent::FullFailed { image_id: i64 }`
  `viewer::load::spawn_full(state, image_id, path)` — `Interactive` job: `decode_full` → `QuadBin.to_linear_rgba_f32` → send `FullDecoded`.
  `ViewerState::set_full(ctx, &GpuContext, jobs, &LinearRgbaF32)` — builds `PyramidTileSource` + streaming `VirtualTexture`; starts crossfade.
  `ViewerState::tick_crossfade(dt) -> f32` (0→1 over ~150ms).
  `ViewerState::cancel_loads()` — cancels in-flight decode + tile jobs.

- [ ] **Step 1: Write the failing test (crossfade ramp is pure)**

In `viewer/mod.rs` tests:
```rust
#[test]
fn crossfade_ramps_zero_to_one_then_clamps() {
    let mut v = ViewerState::open(1, std::path::PathBuf::from("x"));
    v.begin_crossfade();
    assert_eq!(v.tick_crossfade(0.0), 0.0);
    let mid = v.tick_crossfade(0.075); // half of 150ms
    assert!(mid > 0.4 && mid < 0.6, "about halfway");
    let done = v.tick_crossfade(1.0); // way past
    assert_eq!(done, 1.0, "clamps at 1.0");
}
```
(Add `crossfade_elapsed: f32` + `crossfading: bool` fields, `begin_crossfade`, `tick_crossfade`.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p ferrolite-app viewer::tests::crossfade`
Expected: FAIL — methods not defined.

- [ ] **Step 3: Implement crossfade + full-load wiring**

Add the fields/methods to `ViewerState`. `tick_crossfade`:
```rust
const CROSSFADE_SECS: f32 = 0.15;

impl ViewerState {
    pub fn begin_crossfade(&mut self) {
        self.crossfading = true;
        self.crossfade_elapsed = 0.0;
    }
    pub fn tick_crossfade(&mut self, dt: f32) -> f32 {
        if !self.crossfading {
            return if self.full_ready { 1.0 } else { 0.0 };
        }
        self.crossfade_elapsed += dt;
        (self.crossfade_elapsed / CROSSFADE_SECS).clamp(0.0, 1.0)
    }
}
```
Add `spawn_full` (job submits `decode_full` + `QuadBin`, sends `FullDecoded`/`FullFailed`). In `app.rs`, handle `FullDecoded`: build the streaming `VirtualTexture` from a `PyramidTileSource`, call `begin_crossfade`. The display shader blends preview→full by the crossfade factor (extend the callback to hold both textures + factor, OR render full over preview with alpha = factor). `FullFailed`: keep the preview, log.

**Navigation cancel:** when opening image B or closing the viewer, call the old `ViewerState::cancel_loads()` (cancel decode `JobHandle`s + VT tile-load handles via their `CancelToken`s) before replacing `state.viewer`. Store the decode `JobHandle`s on `ViewerState`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p ferrolite-app viewer::`
Expected: PASS.

- [ ] **Step 5: Drive crossfade + VT requests each frame (manual verification)**

In `app.rs` viewer branch: call `request_view`/`request_view_feedback` on the VT each frame with the current `view`+`viewport`, drain tile-load results, advance crossfade with `ctx.input(|i| i.stable_dt)`, and `ctx.request_repaint()` while crossfading or while tiles are loading.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/viewer/ ferrolite-app/src/events.rs ferrolite-app/src/app.rs
git commit -m "feat(app): tier-2 full decode → quad-bin → sparse VT, preview→full crossfade, navigation cancel"
```

---

### Task 16: Workspace gate + finish

**Files:** none (verification + cleanup only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Then: `cargo fmt --all -- --check`
Expected: clean (no diff).

- [ ] **Step 2: Clippy (deny warnings)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings. Fix any (prefer real fixes over `#[allow]`; an `#[allow]` needs a one-line justification comment).

- [ ] **Step 3: Full test suite**

Run: `cargo test --workspace`
Expected: PASS on this machine. GPU golden tests run if an adapter exists; otherwise they print a skip line and pass. Confirm no test is silently ignored that should run.

- [ ] **Step 4: Build the binary**

Run: `cargo build --workspace --all-targets`
Expected: success.

- [ ] **Step 5: Manual smoke (if a GPU + RAW fixtures are available)**

Run the app, ingest a folder of 24MP RAWs, double-click an image: confirm (a) preview appears fast, (b) it sharpens to full-res, (c) scroll-zoom and drag-pan are smooth, (d) Esc returns to the grid, (e) opening another image cancels the previous load (status "GPU: busy" settles).

- [ ] **Step 6: Commit any gate fixups**

```bash
git add -A
git commit -m "chore: workspace gate green (fmt + clippy + tests) for viewer & VT ladder"
```

- [ ] **Step 7: Finish the branch**

Use the superpowers:finishing-a-development-branch skill to choose merge/PR. Summarize: two new crates (`ferrolite-gpu`, `ferrolite-vt`), full 4-rung sparse VT, two-tier viewer load, quad-bin demosaic seam, retained-DAG executor.

---

## Self-Review

**Spec coverage:**
- §3 crate layout/deps → Tasks 3, 7 (manifests + workspace members). ✓
- §4 image vocab (`TileCoord`, `LinearRgbaF32`, pyramid math) → Tasks 1–2. ✓
- §5.1 `GpuContext` → Task 3; §5.2 retained-DAG executor → Task 4. ✓
- §6.1 `TileSource`/`PyramidTileSource` → Task 7; §6.2 residency core → Task 8; §6.3 rungs 1–4 → Tasks 9–12; §6.4 `request_view`/paint → Tasks 11–12. ✓
- §7 demosaic (`DemosaicToRgb16f`/`QuadBin`, CFA/WB surfacing) → Tasks 5–6. ✓
- §8.1 viewer surface/open-close → Task 13; §8.2 two-tier load → Tasks 14–15; §8.3 navigation cancel → Task 15; §8.4 input → Task 13; §8.5 status bar → already wired (jobs activity). ✓
- §9 error handling → Tasks 6 (CFA fallback), 14 (`FullFailed` keeps preview), 11 (eviction/backpressure); device-loss recreation is handled by eframe's surface management + rebuilding the VT on `set_full`. ✓
- §10 testing (CPU + adapter-gated golden) → every task; CI skip pattern in Tasks 3, 9–12. ✓
- §11 build order/rung gates → task order matches; G2 flagged at Task 10. ✓

**Placeholder scan:** GPU rung Tasks 10–12 intentionally delegate the *body* of large wgpu constructors/shaders to the implementer with exact bind-group layouts, shader entry contracts, and golden/CPU tests as gates — these are specified by interface + test, not left as "TODO". Pure-logic tasks (1, 2, 4, 6, 8, 13, 14, 15) carry complete code. No "TBD"/"add error handling"/"similar to Task N" left in.

**Type consistency:** `ViewTransform { zoom, pan }`, `TileCoord { lod, x, y }`, `LinearRgbaF32 { width, height, pixels }`, `RawDecoded` extended fields, `DemosaicToRgb16f::to_linear_rgba_f32`, `needed_tiles`/`ResidencySet::diff`, `VirtualTexture::{single_texture, tiled_resident, streaming, render, render_to_image, request_view, request_view_feedback}` are used consistently across tasks. `NOT_RESIDENT` sentinel shared (Rust `pool.rs` + WGSL). Pan sign convention flagged once (Task 13 note) to keep `fit`/`needed_tiles`/`apply_*` aligned.

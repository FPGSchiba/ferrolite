# Spec 3 Plan 2 — Managed color pipeline: DAG-head ColorMatrixNode + swappable display tail + working-space selector + resizable side panels

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Insert a GPU `camera→working` color-matrix node at the head of both edit DAGs, replace the hardcoded `linear_to_srgb` in the display and blit shaders with a swappable `working→display` 3×3 matrix + sRGB OETF, add the Develop working-space selector, and make all side panels resizable — proving the `sRGB ≡ old linear_to_srgb` regression invariant with a GPU golden.

**Architecture:** The camera color transform becomes a GPU `PointOpNode<ColorMatrixUniform>` inserted between the source/geometry head and `Exposure` in `EditPipeline` (preview tier) and `TileEditPipeline` (full-res tier). The display tail (`ferrolite-vt` `display.wgsl` × 3 entry points, and `ferrolite-pipeline` `blit.wgsl`) gains a `mat3x3` uniform applied before the sRGB OETF; the matrix is pushed only when the working space changes (never per-frame, never per-image), reusing the once-built, pre-warmed `DisplayPipelines`. The generic `Graph<PipelineImage>` executor is **not** touched, and `ferrolite-vt`/`ferrolite-gpu`/`ferrolite-image` stay photo-agnostic (they receive a plain `[[f32;3];3]`, never a `ferrolite-color` type).

**Tech Stack:** Rust, `wgpu` (WGSL compute + render), `bytemuck` Pod uniforms, `egui`/`eframe`, `ferrolite-color` (Plan 1: `Mat3 = [[f32;3];3]` row-major, `camera_to_working`, `working_to_display`, `srgb_oetf`, `WorkingSpace` default `Rec2020`), `ferrolite-decode::ColorProfile` (Plan 1).

## Global Constraints

- **Licensing tiers (spec §3):** `ferrolite-vt`, `ferrolite-gpu`, `ferrolite-image` are engine-transferable — they MUST NOT depend on `ferrolite-color` or carry any photo concept. The display tail is a generic `mat3x3` + OETF; it receives the matrix as a plain `[[f32; 3]; 3]` (row-major). Only `ferrolite-pipeline` and `ferrolite-app` (photo tier) may use `ferrolite-color`.
- **Executor unchanged (spec §5, contract §4):** do not modify `ferrolite-gpu::Graph` / `Node`. Reuse `PointOpNode<U>`.
- **GPU pipelines built once (CLAUDE.md):** the display/blit pipelines are already built once and pre-warmed in `DisplayPipelines::new` at startup — do NOT rebuild them per image/open/edit. The `working→display` matrix is pushed via `queue.write_buffer` only when the working space changes.
- **Matrix type:** `ferrolite_color::Mat3 = [[f32; 3]; 3]`, **row-major**. `ColorProfile.xyz_to_cam: [[f32; 3]; 3]`, `ColorProfile.white_xy: [f32; 2]`, `ColorProfile::srgb_fallback()`, `ColorProfile.is_fallback: bool`.
- **WGSL `mat3x3<f32>` layout:** column-major, each column padded to 16 bytes ⇒ a `[[f32; 4]; 3]` Rust uniform. Packing a row-major `[[f32;3];3]` `m` into WGSL columns: `cols[c] = [m[0][c], m[1][c], m[2][c], 0.0]` so that WGSL `M * v == row-major m · v`.
- **Default working space = `WorkingSpace::Rec2020`** (spec §4.1) — so `working→display` is **not** identity by default; the app must push the real matrix on open and on change.
- **Per-component reset (CLAUDE.md):** the working-space selector is a global **preference**, not an editable op in the `OpStack`; it is NOT subject to per-control reset and is NOT touched by "Reset all". No reset affordance is added for it.
- **Regression invariant (spec §4.3/§10):** with `WorkingSpace::Srgb`, `working_to_display` is the identity 3×3 and the tail reduces exactly to today's `linear_to_srgb`. Proven by (a) existing `ferrolite-vt` display goldens staying green with the identity default, and (b) a new `ferrolite-pipeline` golden comparing the identity-matrix blit against `srgb_oetf`.
- **Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → then STOP and hold for Jann's hands-on visual test before finishing the branch (CLAUDE.md).
- **GPU goldens auto-skip headless:** every GPU test starts with `let Some(ctx) = GpuContext::headless() else { return; };` (spec §10 convention). `UPDATE_GOLDEN=1` regenerates fixtures; `TOL` absorbs driver float diffs.
- **Branch:** continue on `feat/color-and-export`. Conventional-commit messages, no attribution footer (disabled globally).

---

## File Structure

**`ferrolite-pipeline`** (photo tier):
- Create `ferrolite-pipeline/src/shaders/color_matrix.wgsl` — camera→working 3×3 multiply compute pass.
- Modify `ferrolite-pipeline/src/uniforms.rs` — add `ColorMatrixUniform` + `color_matrix_uniform` + `pack_mat3` + tests.
- Modify `ferrolite-pipeline/src/pipeline.rs` — `EditPipeline`: insert node, new ctor param, `set_color_matrix`.
- Modify `ferrolite-pipeline/src/tile_edit.rs` — `TileEditPipeline`: mirror the node insertion + `set_color_matrix`.
- Modify `ferrolite-pipeline/src/shaders/blit.wgsl` + `ferrolite-pipeline/src/pipeline.rs` (`blit_to_rgba8`) — matrix uniform; add `blit_to_rgba8_with_matrix`.
- Create `ferrolite-pipeline/tests/color_golden.rs` — ColorMatrixNode golden + sRGB≡old blit golden.
- Modify `ferrolite-pipeline/Cargo.toml` — add `ferrolite-color` as **dev-dependency** (golden reference only).

**`ferrolite-vt`** (engine tier — NO `ferrolite-color`):
- Modify `ferrolite-vt/src/shaders/display.wgsl` — matrix uniform @binding(8), applied in `fs_main`/`fs_tiled`/`fs_sparse` before OETF.
- Modify `ferrolite-vt/src/pipelines.rs` — add binding 8 to all 4 BGLs; own+expose the shared display-matrix buffer; `set_display_matrix`; `pack_display_matrix` (generic, tested).
- Modify `ferrolite-vt/src/view.rs` — every bind-group builder binds @8; each `*Resources` holds the buffer `Arc`.

**`ferrolite-app`** (photo tier):
- Modify `ferrolite-app/src/events.rs` — `FullDecoded` carries `color_profile`.
- Modify `ferrolite-app/src/viewer/load.rs` — send the profile.
- Modify `ferrolite-app/src/viewer/mod.rs` — `ViewerState.color_profile`.
- Modify `ferrolite-app/src/viewer/edit_producer.rs` — `set_color_matrix` pass-through.
- Modify `ferrolite-app/src/state.rs` — `AppState.working_space`.
- Modify `ferrolite-app/src/app.rs` — thread the profile + working space into every pipeline build; push tail matrix; `apply_working_space` handler.
- Modify `ferrolite-app/src/develop/adjustment_panel.rs` — working-space `ComboBox`.
- Modify `ferrolite-app/src/app.rs` (Develop right panel) — make `develop_adjust` resizable.
- Modify `ferrolite-app/Cargo.toml` — add `ferrolite-color` + `ferrolite-decode` deps if absent.

---

## Task 1: `ColorMatrixNode` in the preview `EditPipeline`

Insert a `camera→working` GPU pass at the DAG head of `EditPipeline`, driven by a `[[f32;3];3]` matrix supplied by the caller. Reuse the existing generic `PointOpNode<U>` (bindings 0=src tex, 1=dst storage, 2=uniform) — no new node type.

**Files:**
- Create: `ferrolite-pipeline/src/shaders/color_matrix.wgsl`
- Modify: `ferrolite-pipeline/src/uniforms.rs` (add uniform + packer + tests)
- Modify: `ferrolite-pipeline/src/pipeline.rs:47-131` (ctor), `:22-44` (struct), add `set_color_matrix`
- Test: `ferrolite-pipeline/src/uniforms.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `pub struct ColorMatrixUniform { pub m: [[f32; 4]; 3] }` (`#[repr(C)]`, `bytemuck::Pod`+`Zeroable`, `PartialEq`); `pub fn color_matrix_uniform(m: [[f32; 3]; 3]) -> ColorMatrixUniform`; `pub fn pack_mat3(m: [[f32; 3]; 3]) -> [[f32; 4]; 3]`.
- Produces: `EditPipeline::new(ctx, source, stack, camera_to_working: [[f32; 3]; 3])` (new 4th param); `EditPipeline::set_color_matrix(&mut self, m: [[f32; 3]; 3])`.
- Consumes (Task 5): the caller passes `ferrolite_color::camera_to_working(...)` output (a `Mat3 = [[f32;3];3]`).

- [ ] **Step 1: Write the failing packer test**

In `ferrolite-pipeline/src/uniforms.rs`, add to `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn pack_mat3_identity_columns() {
        // Row-major identity packs to WGSL column-major identity (last lane = 0 pad).
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        assert_eq!(
            pack_mat3(id),
            [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]
        );
    }

    #[test]
    fn pack_mat3_transposes_into_columns() {
        // Row-major m[row][col]; WGSL column c = (m[0][c], m[1][c], m[2][c], 0).
        let m = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]];
        assert_eq!(
            pack_mat3(m),
            [[1.0, 4.0, 7.0, 0.0], [2.0, 5.0, 8.0, 0.0], [3.0, 6.0, 9.0, 0.0]]
        );
    }

    #[test]
    fn color_matrix_uniform_wraps_packed_mat() {
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        assert_eq!(color_matrix_uniform(id).m, pack_mat3(id));
    }
```

- [ ] **Step 2: Run it to verify failure**

Run: `cargo test -p ferrolite-pipeline pack_mat3 -- --nocapture`
Expected: FAIL — `cannot find function pack_mat3`.

- [ ] **Step 3: Implement the uniform + packer**

In `ferrolite-pipeline/src/uniforms.rs`, after the `ExposureUniform` block (around line 92), add:

```rust
/// WGSL `mat3x3<f32>` uniform for a 3×3 color transform. Column-major with each
/// column padded to 16 bytes (`[[f32; 4]; 3]`), matching WGSL layout rules.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ColorMatrixUniform {
    pub m: [[f32; 4]; 3],
}

/// Pack a **row-major** 3×3 (`m[row][col]`) into WGSL column-major padded columns
/// so that in-shader `M * v` equals the row-major `m · v`.
pub fn pack_mat3(m: [[f32; 3]; 3]) -> [[f32; 4]; 3] {
    [
        [m[0][0], m[1][0], m[2][0], 0.0],
        [m[0][1], m[1][1], m[2][1], 0.0],
        [m[0][2], m[1][2], m[2][2], 0.0],
    ]
}

/// Build the color-matrix uniform from a row-major camera→working (or any) 3×3.
pub fn color_matrix_uniform(m: [[f32; 3]; 3]) -> ColorMatrixUniform {
    ColorMatrixUniform { m: pack_mat3(m) }
}
```

- [ ] **Step 4: Run the packer tests to green**

Run: `cargo test -p ferrolite-pipeline pack_mat3 color_matrix_uniform_wraps -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 5: Write the color-matrix WGSL shader**

Create `ferrolite-pipeline/src/shaders/color_matrix.wgsl`:

```wgsl
// Camera->working color transform: multiply linear RGB by a 3x3 matrix. Point op.
// Bindings match PointOpNode: 0 = src texture, 1 = dst storage, 2 = matrix uniform.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
struct M { m: mat3x3<f32> };
@group(0) @binding(2) var<uniform> cm: M;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if (gid.x >= dims.x || gid.y >= dims.y) { return; }
    let xy = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, xy, 0);
    textureStore(dst, xy, vec4<f32>(cm.m * c.rgb, c.a));
}
```

> Note: a `struct { m: mat3x3<f32> }` uniform occupies 48 bytes, matching `ColorMatrixUniform` (`[[f32;4];3]`). WGSL infers std140-style column padding; the Rust side already pads each column to 16 bytes.

- [ ] **Step 6: Wire the node into `EditPipeline`**

In `ferrolite-pipeline/src/pipeline.rs`:

Add imports (extend the existing `use crate::uniforms::{...}` at line 13):
```rust
use crate::uniforms::{
    color_matrix_uniform, contrast_uniform, curve_lut, exposure_uniform, geometry_uniform,
    hsl_uniform, sharpen_uniform, wb_uniform, ColorMatrixUniform, ContrastUniform, ExposureUniform,
    GeometryUniform, HslUniform, SharpenUniform, WbUniform,
};
```

Add two fields to `struct EditPipeline` (after `output_id: NodeId,` at line 26):
```rust
    color_matrix_id: NodeId,
    color_matrix: Rc<Cell<ColorMatrixUniform>>,
```

Change the ctor signature (line 47) and insert the node between source and exposure. Replace lines 47-59:
```rust
    pub fn new(
        ctx: Arc<GpuContext>,
        source: &LinearRgbaF32,
        stack: OpStack,
        camera_to_working: [[f32; 3]; 3],
    ) -> Self {
        let mut graph = Graph::new();
        let (src_w, src_h) = (source.width, source.height);
        let source_id = graph.add_node(Box::new(SourceNode::new(&ctx, source)), vec![]);

        let color_matrix = Rc::new(Cell::new(color_matrix_uniform(camera_to_working)));
        let color_matrix_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/color_matrix.wgsl"),
            "color-matrix",
            color_matrix.clone(),
        );
        let color_matrix_id = graph.add_node(Box::new(color_matrix_node), vec![source_id]);

        let exposure = Rc::new(Cell::new(exposure_uniform(stack.exposure())));
        let exposure_node = PointOpNode::new(
            ctx.clone(),
            include_str!("shaders/exposure.wgsl"),
            "exposure",
            exposure.clone(),
        );
        let exposure_id = graph.add_node(Box::new(exposure_node), vec![color_matrix_id]);
```

In the returned `Self { ... }` literal (line 108), add the two fields (after `output_id: geometry_id,`):
```rust
            color_matrix_id,
            color_matrix,
```

Bump `node_count: 8` → `node_count: 9` (line 128).

Add the setter after `set_stack` (after line 171):
```rust
    /// Update the camera→working matrix (working-space change) and dirty the head
    /// so the chain re-runs. `m` is a row-major 3×3.
    pub fn set_color_matrix(&mut self, m: [[f32; 3]; 3]) {
        let u = color_matrix_uniform(m);
        if u != self.color_matrix.get() {
            self.color_matrix.set(u);
            self.graph.mark_dirty(self.color_matrix_id);
        }
    }
```

- [ ] **Step 7: Fix the existing `EditPipeline::new` call sites to compile**

The app calls `EditPipeline::new` at `ferrolite-app/src/app.rs:259`. Task 5 rewrites it properly; for now keep the workspace compiling by passing identity at that one call site (temporary — Task 5 replaces it):
```rust
                v.preview_edit = Some(ferrolite_pipeline::EditPipeline::new(
                    ctx_arc,
                    &src,
                    shown.clone(),
                    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                ));
```
Also update any `EditPipeline::new(` usage in `ferrolite-pipeline` tests (grep `EditPipeline::new`) to add the identity 4th arg.

Run: `cargo build -p ferrolite-pipeline -p ferrolite-app`
Expected: compiles.

- [ ] **Step 8: Write the ColorMatrixNode GPU golden test**

Create `ferrolite-pipeline/tests/color_golden.rs`:

```rust
//! GPU goldens for the Spec 3 color pipeline: the camera→working ColorMatrixNode
//! and the sRGB≡old blit regression. Auto-skip when no GPU adapter is present.

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_pipeline::{EditPipeline, OpStack};

const TOL: u8 = 4;

/// A 2×2 image with distinct linear RGB per texel (values chosen to stay in-gamut
/// after a channel-swap matrix and below the sRGB linear knee for at least one).
fn probe_image() -> LinearRgbaF32 {
    // RGBA f32, row-major, 2×2.
    let px = vec![
        0.20, 0.40, 0.60, 1.0, //
        0.50, 0.10, 0.30, 1.0, //
        0.05, 0.25, 0.45, 1.0, //
        0.60, 0.55, 0.15, 1.0, //
    ];
    LinearRgbaF32::new(2, 2, px).unwrap()
}

fn srgb_oetf(l: f32) -> f32 {
    if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

#[test]
fn color_matrix_node_applies_matrix_before_srgb() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let img = probe_image();
    // A channel-swap + scale matrix (row-major): out.r = 0.5*b, out.g = r, out.b = g.
    let m = [[0.0, 0.0, 0.5], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mut ep = EditPipeline::new(
        std::sync::Arc::new(ctx),
        &img,
        OpStack::default(), // identity ops: isolate the color matrix
        m,
    );
    let out = ep.render_to_image(); // sRGB Rgba8, 2×2, row-unpadded

    for i in 0..4usize {
        let (r, g, b) = (
            img.pixels[i * 4],
            img.pixels[i * 4 + 1],
            img.pixels[i * 4 + 2],
        );
        let lin = [0.5 * b, r, g]; // expected linear after the matrix
        for c in 0..3 {
            let want = (srgb_oetf(lin[c]).clamp(0.0, 1.0) * 255.0).round() as i32;
            let got = out[i * 4 + c] as i32;
            assert!(
                (want - got).abs() <= TOL as i32,
                "texel {i} ch {c}: want {want} got {got}"
            );
        }
    }
}
```

- [ ] **Step 9: Run the golden (local GPU)**

Run: `cargo test -p ferrolite-pipeline --test color_golden color_matrix_node -- --nocapture`
Expected: PASS on the dev GPU; on headless CI it prints "skipping" and passes.

- [ ] **Step 10: Commit**

```bash
git add ferrolite-pipeline/src/shaders/color_matrix.wgsl ferrolite-pipeline/src/uniforms.rs ferrolite-pipeline/src/pipeline.rs ferrolite-pipeline/tests/color_golden.rs ferrolite-app/src/app.rs
git commit -m "feat(pipeline): camera→working ColorMatrixNode at the EditPipeline DAG head"
```

---

## Task 2: Mirror `ColorMatrixNode` into the full-res `TileEditPipeline`

The full-res tier (`TileEditPipeline`) has `GeometryHeadNode` as its root; the color chain runs downstream. Insert the same `ColorMatrixNode` between the head and `Exposure` so 1:1 inspection matches the preview.

**Files:**
- Modify: `ferrolite-pipeline/src/tile_edit.rs:30-44` (struct), `:47-137` (ctor), `:154+` (`set_stack` region — add `set_color_matrix`)

**Interfaces:**
- Produces: `TileEditPipeline::new(ctx, source, stack, camera_to_working: [[f32; 3]; 3])`; `TileEditPipeline::set_color_matrix(&mut self, m: [[f32; 3]; 3])`.

- [ ] **Step 1: Add fields + ctor param + node**

In `ferrolite-pipeline/src/tile_edit.rs`, extend the `use crate::uniforms::{...}` (line ~24) to include `color_matrix_uniform, ColorMatrixUniform`. Add to `struct TileEditPipeline` (after `head_id: NodeId,`):
```rust
    color_matrix_id: NodeId,
    color_matrix: Rc<Cell<ColorMatrixUniform>>,
```

Change the ctor signature (line 47):
```rust
    pub fn new(
        ctx: Arc<GpuContext>,
        source: Arc<GpuPyramidSource>,
        stack: OpStack,
        camera_to_working: [[f32; 3]; 3],
    ) -> Self {
```

Insert the node between the head and exposure. After `let head_id = graph.add_node(Box::new(head), vec![]);` (line 61), add:
```rust
        let color_matrix = Rc::new(Cell::new(color_matrix_uniform(camera_to_working)));
        let color_matrix_id = graph.add_node(
            Box::new(PointOpNode::new(
                ctx.clone(),
                include_str!("shaders/color_matrix.wgsl"),
                "color-matrix",
                color_matrix.clone(),
            )),
            vec![head_id],
        );
```

Change the exposure node's input from `vec![head_id]` to `vec![color_matrix_id]` (line 71).

Add the two fields to the returned `Self { ... }` (after `head_id,`):
```rust
            color_matrix_id,
            color_matrix,
```

- [ ] **Step 2: Add `set_color_matrix`**

After `set_stack` in `tile_edit.rs`, add:
```rust
    /// Update the camera→working matrix (working-space change) and dirty the head.
    pub fn set_color_matrix(&mut self, m: [[f32; 3]; 3]) {
        let u = color_matrix_uniform(m);
        if u != self.color_matrix.get() {
            self.color_matrix.set(u);
            self.graph.mark_dirty(self.color_matrix_id);
        }
    }
```

- [ ] **Step 3: Fix `TileEditPipeline::new` call sites to compile**

The app calls it at `ferrolite-app/src/app.rs:192` and `:298`. Temporarily pass identity at both (Task 5 replaces):
```rust
                        let tep = ferrolite_pipeline::TileEditPipeline::new(
                            ctx_arc,
                            pyramid,
                            v.op_stack.clone(),
                            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                        );
```
Update any `TileEditPipeline::new(` in `ferrolite-pipeline` tests likewise (grep it).

Run: `cargo build -p ferrolite-pipeline -p ferrolite-app`
Expected: compiles.

- [ ] **Step 4: Run the existing tile-seam golden to confirm no regression**

Run: `cargo test -p ferrolite-pipeline -- --nocapture`
Expected: existing goldens PASS on dev GPU (the identity color matrix leaves them unchanged); skip on headless.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-pipeline/src/tile_edit.rs ferrolite-app/src/app.rs
git commit -m "feat(pipeline): ColorMatrixNode at the TileEditPipeline head (full-res tier)"
```

---

## Task 3: Swappable `working→display` tail in `ferrolite-vt` (display.wgsl)

Add a `mat3x3` uniform (binding 8) to all three display entry points, applied before the sRGB OETF. `DisplayPipelines` owns one shared uniform buffer (initialized to identity), exposes `set_display_matrix` (pushed only on working-space change), and every bind group binds it. **No `ferrolite-color` dependency** — the matrix arrives as `[[f32; 3]; 3]`.

**Files:**
- Modify: `ferrolite-vt/src/shaders/display.wgsl`
- Modify: `ferrolite-vt/src/pipelines.rs` (4 BGLs + struct + `new` + accessors + `pack_display_matrix` + test)
- Modify: `ferrolite-vt/src/view.rs` (all 4 `*Resources` + all bind-group builders)

**Interfaces:**
- Produces: `pub fn pack_display_matrix(m: [[f32; 3]; 3]) -> [[f32; 4]; 3]` (generic; same transpose-pad rule as Task 1); `DisplayPipelines::display_matrix_buffer(&self) -> &Arc<wgpu::Buffer>`; `DisplayPipelines::set_display_matrix(&self, queue: &wgpu::Queue, m: [[f32; 3]; 3])`.

- [ ] **Step 1: Write the failing packer test**

In `ferrolite-vt/src/pipelines.rs`, add a `#[cfg(test)] mod tests` (or extend one) :
```rust
#[cfg(test)]
mod tests {
    use super::pack_display_matrix;

    #[test]
    fn pack_identity_columns() {
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        assert_eq!(
            pack_display_matrix(id),
            [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]
        );
    }

    #[test]
    fn pack_transposes_rows_into_columns() {
        let m = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]];
        assert_eq!(
            pack_display_matrix(m),
            [[1.0, 4.0, 7.0, 0.0], [2.0, 5.0, 8.0, 0.0], [3.0, 6.0, 9.0, 0.0]]
        );
    }
}
```

- [ ] **Step 2: Run it to verify failure**

Run: `cargo test -p ferrolite-vt pack_ -- --nocapture`
Expected: FAIL — `cannot find function pack_display_matrix`.

- [ ] **Step 3: Add the uniform struct + packer to `pipelines.rs`**

At the top of `ferrolite-vt/src/pipelines.rs` (after imports), add:
```rust
/// WGSL `mat3x3<f32>` uniform for the working→display tail transform. Column-major,
/// each column padded to 16 bytes. Generic (no photo concepts): the app supplies a
/// plain row-major 3×3.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayColorUniform {
    m: [[f32; 4]; 3],
}

/// Pack a row-major 3×3 into WGSL column-major padded columns (`M * v == m · v`).
pub fn pack_display_matrix(m: [[f32; 3]; 3]) -> [[f32; 4]; 3] {
    [
        [m[0][0], m[1][0], m[2][0], 0.0],
        [m[0][1], m[1][1], m[2][1], 0.0],
        [m[0][2], m[1][2], m[2][2], 0.0],
    ]
}
```

- [ ] **Step 4: Run the packer tests to green**

Run: `cargo test -p ferrolite-vt pack_ -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 5: Edit the display shader**

In `ferrolite-vt/src/shaders/display.wgsl`, after the `xf` uniform declaration (line 12), add:
```wgsl
struct DisplayColor { m: mat3x3<f32> };
@group(0) @binding(8) var<uniform> disp: DisplayColor;
```

Replace the three `linear_to_srgb(lin)` return sites so the matrix is applied first. In `fs_main` (line ~45), `fs_tiled` (line ~109), and `fs_sparse` (line ~178), change:
```wgsl
    return vec4(linear_to_srgb(lin), 1.0);
```
to:
```wgsl
    return vec4(linear_to_srgb(disp.m * lin), 1.0);
```

> Keep `linear_to_srgb` exactly as-is (it is the OETF). With an identity `disp.m`, `disp.m * lin == lin`, so behavior is unchanged — the regression invariant.

- [ ] **Step 6: Add binding 8 to all four BGLs in `pipelines.rs`**

In `DisplayPipelines::new`, add this entry (a fragment-visible uniform) to the `entries: &[...]` of **each** bind-group layout — `single_bgl` (after binding 2, ~line 117), the `tiled_bgl` closure (after binding 5), and `sparse_bgl` (after binding 7):
```rust
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
```

- [ ] **Step 7: Own + expose the shared display-matrix buffer**

Add a field to `struct DisplayPipelines` (after `sampler`):
```rust
    display_matrix: Arc<wgpu::Buffer>,
```

In `DisplayPipelines::new`, before the `Self { ... }` return, create the buffer initialized to identity:
```rust
        let display_matrix = Arc::new(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vt-display-matrix"),
            contents: bytemuck::bytes_of(&DisplayColorUniform {
                m: pack_display_matrix([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        }));
```
Add `display_matrix,` to the returned `Self`. Ensure `use wgpu::util::DeviceExt;` is present at the top of `pipelines.rs` (add it if missing).

Add accessor + setter methods to `impl DisplayPipelines` (near `sampler()`):
```rust
    /// The shared working→display matrix uniform buffer (bound at @8 by every
    /// variant). Cloned into per-image VT resources.
    pub fn display_matrix_buffer(&self) -> &Arc<wgpu::Buffer> {
        &self.display_matrix
    }

    /// Push a new working→display matrix (row-major 3×3). Call ONLY when the working
    /// space changes — never per frame, never per image. Cheap `write_buffer`.
    pub fn set_display_matrix(&self, queue: &wgpu::Queue, m: [[f32; 3]; 3]) {
        queue.write_buffer(
            &self.display_matrix,
            0,
            bytemuck::bytes_of(&DisplayColorUniform { m: pack_display_matrix(m) }),
        );
    }
```

- [ ] **Step 8: Hold the buffer Arc in each `*Resources` and bind it at @8**

In `ferrolite-vt/src/view.rs`, add `display_matrix: Arc<wgpu::Buffer>,` to each of `SingleResources`, `TiledResources`, `StreamingResources`, `SparseResources`.

At each construction site (`single_texture`, `tiled_resident`, `streaming`, `sparse`), grab it alongside the sampler:
```rust
        let display_matrix = pipelines.display_matrix_buffer().clone();
```
and add `display_matrix,` to the corresponding `*Resources { ... }` literal.

Then add `binding: 8` to **every** bind-group builder — there are seven: `prepare_single` (line ~267), `render` (line ~333), `render_tiled` (line ~560), `prepare_streaming` (line ~896), `render_streaming` (line ~968), `render_sparse` (line ~1251), `prepare_sparse` (line ~1308). In each `entries: &[...]`, append:
```rust
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: <res>.display_matrix.as_entire_binding(),
                },
```
where `<res>` is the local binding used by that builder (`single`, `tiled`, or `s`). For `prepare_*`/`render_*` on the single path the resource holder is `single`; for tiled it is `tiled`; for streaming/sparse it is `s`.

- [ ] **Step 9: Build + run the existing display goldens (regression proof)**

Run: `cargo test -p ferrolite-vt -- --nocapture`
Expected: `cargo build` clean; existing `rung1_fit` / tiled / sparse goldens PASS unchanged on the dev GPU (identity default ⇒ tail ≡ old `linear_to_srgb`). This IS the "sRGB ≡ old" regression proof for the display path. Skips on headless.

- [ ] **Step 10: Add an explicit display-tail non-identity golden**

Append to `ferrolite-vt/tests/golden.rs`:
```rust
#[test]
fn display_tail_applies_matrix() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    // Channel-swap matrix (row-major): display.r = g, .g = b, .b = r.
    pipelines.set_display_matrix(&ctx.queue, [[0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]]);
    let img = common::split_image();
    let (w, h) = (64u32, 64u32);
    let view = ViewTransform::fit((img.width, img.height), (w as f32, h as f32));
    let pixels =
        VirtualTexture::render_to_image(&ctx, &img, &view, (w as f32, h as f32), w, h, &pipelines);

    let golden_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/display_tail_swap.png");
    if std::env::var("UPDATE_GOLDEN").is_ok() || !std::path::Path::new(golden_path).exists() {
        image::save_buffer(golden_path, &pixels, w, h, image::ColorType::Rgba8).unwrap();
        eprintln!("wrote golden {golden_path}");
        return;
    }
    let golden = image::open(golden_path).unwrap().to_rgba8();
    assert!(common::max_abs_diff(&pixels, golden.as_raw()) <= TOL);
}
```
Generate the fixture once on the dev GPU: `UPDATE_GOLDEN=1 cargo test -p ferrolite-vt display_tail_applies_matrix`, then re-run without the env var to confirm it passes. Commit the PNG.

- [ ] **Step 11: Commit**

```bash
git add ferrolite-vt/src/shaders/display.wgsl ferrolite-vt/src/pipelines.rs ferrolite-vt/src/view.rs ferrolite-vt/tests/golden.rs ferrolite-vt/tests/fixtures/display_tail_swap.png
git commit -m "feat(vt): swappable working→display 3×3 tail uniform in display shaders"
```

---

## Task 4: `working→display` tail in the pipeline blit + `sRGB ≡ old` regression golden

`blit_to_rgba8` (used by golden/readback paths and, later, export) must also apply the matrix before the OETF. Keep `blit_to_rgba8(ctx, img)` as identity for existing callers; add `blit_to_rgba8_with_matrix`. Add the definitive `sRGB ≡ old linear_to_srgb` golden.

**Files:**
- Modify: `ferrolite-pipeline/src/shaders/blit.wgsl`
- Modify: `ferrolite-pipeline/src/pipeline.rs:199-303` (`blit_to_rgba8`)
- Modify: `ferrolite-pipeline/Cargo.toml` (add `ferrolite-color` dev-dependency)
- Test: `ferrolite-pipeline/tests/color_golden.rs`

**Interfaces:**
- Produces: `pub fn blit_to_rgba8_with_matrix(ctx: &GpuContext, img: &PipelineImage, working_to_display: [[f32; 3]; 3]) -> Vec<u8>`; `blit_to_rgba8(ctx, img)` delegates with identity.

- [ ] **Step 1: Edit `blit.wgsl` to add the matrix uniform**

In `ferrolite-pipeline/src/shaders/blit.wgsl`, after the sampler binding (line 5), add:
```wgsl
struct DisplayColor { m: mat3x3<f32> };
@group(0) @binding(2) var<uniform> disp: DisplayColor;
```
Change `fs_main`'s return (line 31) to:
```wgsl
    return vec4(linear_to_srgb(disp.m * lin), 1.0);
```

- [ ] **Step 2: Refactor `blit_to_rgba8` to bind the matrix**

In `ferrolite-pipeline/src/pipeline.rs`, replace `blit_to_rgba8` (lines 199-303) so it delegates, and add the matrix binding. Rename the body to `blit_to_rgba8_with_matrix` and add a thin wrapper:

```rust
/// Identity-matrix blit (working≡display, i.e. sRGB working space). Existing
/// golden/readback callers use this; it reduces to the old sRGB OETF path exactly.
pub fn blit_to_rgba8(ctx: &GpuContext, img: &PipelineImage) -> Vec<u8> {
    blit_to_rgba8_with_matrix(
        ctx,
        img,
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    )
}

/// Render a display-linear `PipelineImage` to an sRGB `Rgba8Unorm` buffer at 1:1,
/// applying `working_to_display` (row-major 3×3) before the sRGB OETF. Builds its
/// pipeline per call — for the test/readback path, not per-frame.
pub fn blit_to_rgba8_with_matrix(
    ctx: &GpuContext,
    img: &PipelineImage,
    working_to_display: [[f32; 3]; 3],
) -> Vec<u8> {
    let device = &ctx.device;
    let (w, h) = (img.width, img.height);
    // ... (unchanged shader/sampler creation) ...
```

Keep the existing shader/sampler/pipeline creation, but:

(a) add binding 2 to the blit BGL `entries` (after the sampler entry at line 231):
```rust
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
```

(b) before the bind-group creation (line 264), create the matrix buffer (reuse `crate::uniforms::pack_mat3` + a local Pod struct, or inline). Add near the top of `pipeline.rs`:
```rust
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlitMatrix {
    m: [[f32; 4]; 3],
}
```
then:
```rust
    let matrix_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("pipeline-blit-matrix"),
        contents: bytemuck::bytes_of(&BlitMatrix { m: crate::uniforms::pack_mat3(working_to_display) }),
        usage: wgpu::BufferUsages::UNIFORM,
    });
```
Add `use wgpu::util::DeviceExt;` at the top of `pipeline.rs` if not already imported.

(c) add the binding-2 entry to the bind group `entries` (after the sampler entry at line 276):
```rust
            wgpu::BindGroupEntry {
                binding: 2,
                resource: matrix_buf.as_entire_binding(),
            },
```

- [ ] **Step 3: Build**

Run: `cargo build -p ferrolite-pipeline`
Expected: compiles (existing `render_to_image` callers still use `blit_to_rgba8`).

- [ ] **Step 4: Add the `ferrolite-color` dev-dependency**

In `ferrolite-pipeline/Cargo.toml` `[dev-dependencies]`, add:
```toml
ferrolite-color = { workspace = true }
```

- [ ] **Step 5: Write the `sRGB ≡ old linear_to_srgb` regression golden**

Append to `ferrolite-pipeline/tests/color_golden.rs`:
```rust
/// Regression invariant (spec §4.3): the identity-matrix tail == the old
/// hardcoded `linear_to_srgb`. Proven by comparing the identity blit against
/// `ferrolite_color::srgb_oetf` over a known image.
#[test]
fn blit_srgb_identity_equals_old_linear_to_srgb() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let img = probe_image();
    // Upload as a PipelineImage via a no-op identity EditPipeline evaluate.
    let mut ep = EditPipeline::new(
        std::sync::Arc::new(ctx),
        &img,
        OpStack::default(),
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    );
    let out = ep.render_to_image(); // uses blit_to_rgba8 (identity)

    for i in 0..4usize {
        for c in 0..3 {
            let lin = img.pixels[i * 4 + c];
            let want = (ferrolite_color::srgb_oetf(lin).clamp(0.0, 1.0) * 255.0).round() as i32;
            let got = out[i * 4 + c] as i32;
            assert!(
                (want - got).abs() <= TOL as i32,
                "texel {i} ch {c}: identity tail drifted from sRGB OETF (want {want}, got {got})"
            );
        }
    }
}
```

- [ ] **Step 6: Run the regression golden**

Run: `cargo test -p ferrolite-pipeline --test color_golden blit_srgb_identity -- --nocapture`
Expected: PASS on dev GPU; skip on headless.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-pipeline/src/shaders/blit.wgsl ferrolite-pipeline/src/pipeline.rs ferrolite-pipeline/Cargo.toml ferrolite-pipeline/tests/color_golden.rs
git commit -m "feat(pipeline): working→display matrix in blit + sRGB≡old regression golden"
```

---

## Task 5: App wiring — thread `ColorProfile` + working space into every pipeline build

Carry the camera `ColorProfile` from decode into `ViewerState`, add the global `working_space`, compose `camera_to_working` and `working_to_display` via `ferrolite-color`, and push them into `EditPipeline`/`TileEditPipeline`/`DisplayPipelines` at open (default Rec2020). This replaces the temporary identity args from Tasks 1–2. (Visual-test-only per spec §11; unit-tested composition is covered by `ferrolite-color`'s own Plan 1 tests.)

**Files:**
- Modify: `ferrolite-app/Cargo.toml` (deps), `ferrolite-app/src/events.rs`, `ferrolite-app/src/viewer/load.rs`, `ferrolite-app/src/viewer/mod.rs`, `ferrolite-app/src/viewer/edit_producer.rs`, `ferrolite-app/src/state.rs`, `ferrolite-app/src/app.rs`

**Interfaces:**
- Consumes: `EditPipeline::new(.., camera_to_working)`, `EditPipeline::set_color_matrix`, `TileEditPipeline::new(.., camera_to_working)`, `TileEditPipeline::set_color_matrix`, `DisplayPipelines::set_display_matrix`.
- Produces: `AppState.working_space: ferrolite_color::WorkingSpace`; `ViewerState.color_profile: ferrolite_decode::ColorProfile`; `FerroliteApp::camera_to_working(&self) -> [[f32; 3]; 3]` helper; `FerroliteApp::apply_working_space(&mut self, ctx, frame, ws)`.

- [ ] **Step 1: Add deps**

In `ferrolite-app/Cargo.toml` `[dependencies]`, ensure both are present (add if missing):
```toml
ferrolite-color = { workspace = true }
ferrolite-decode = { workspace = true }
```
Run `cargo build -p ferrolite-app` to confirm they resolve.

- [ ] **Step 2: `FullDecoded` carries the profile**

In `ferrolite-app/src/events.rs`, change the `FullDecoded` variant (line 42):
```rust
    FullDecoded {
        image_id: i64,
        image: ferrolite_image::LinearRgbaF32,
        color_profile: ferrolite_decode::ColorProfile,
    },
```

In `ferrolite-app/src/viewer/load.rs:104`, send it:
```rust
                let _ = tx.send(AppEvent::FullDecoded {
                    image_id,
                    image,
                    color_profile: raw.color_profile,
                });
```
(`raw` is the `RawDecoded` from `decode_full`; `raw.color_profile` is the Plan 1 field. Reorder so `raw.color_profile` is read before `raw` is moved into `QuadBin.to_linear_rgba_f32(&raw)` — clone it: `let color_profile = raw.color_profile.clone();` then send `color_profile`.)

- [ ] **Step 3: Store the profile on `ViewerState`**

In `ferrolite-app/src/viewer/mod.rs`, add to `struct ViewerState` (after `pyramid`):
```rust
    /// Camera color profile from the tier-2 full decode; feeds the ColorMatrixNode
    /// via `ferrolite_color::camera_to_working`. sRGB fallback until full decode.
    pub color_profile: ferrolite_decode::ColorProfile,
```
In `ViewerState::open` (line ~99), initialize it: `color_profile: ferrolite_decode::ColorProfile::srgb_fallback(),`.

- [ ] **Step 4: Add the global working space**

In `ferrolite-app/src/state.rs`, add to `struct AppState`:
```rust
    /// Editing working space (spec §4.1, default Rec.2020). Global preference; the
    /// ColorMatrixNode + display tail are recomposed on change.
    pub working_space: ferrolite_color::WorkingSpace,
```
Initialize it in `AppState`'s constructor to `ferrolite_color::WorkingSpace::default()` (= `Rec2020`).

- [ ] **Step 5: Add the composition helper**

In `ferrolite-app/src/app.rs`, add to `impl FerroliteApp` (near `set_preview_and_full`):
```rust
    /// Compose the camera→working 3×3 for the open viewer + current working space.
    fn camera_to_working(&self) -> [[f32; 3]; 3] {
        let ws = self.state.working_space;
        match self.state.viewer.as_ref() {
            Some(v) => {
                let p = &v.color_profile;
                ferrolite_color::camera_to_working(
                    p.xyz_to_cam,
                    ferrolite_color::Xy { x: p.white_xy[0], y: p.white_xy[1] },
                    ws,
                )
            }
            None => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }
```

- [ ] **Step 6: Feed the profile at decode + push the tail matrix on open**

In `apply_full_decoded`, change the signature (line 120) to accept the profile and store it:
```rust
    fn apply_full_decoded(
        &mut self,
        frame: &eframe::Frame,
        image_id: i64,
        image: &ferrolite_image::LinearRgbaF32,
        color_profile: &ferrolite_decode::ColorProfile,
    ) {
```
Right after the staleness guards set the profile on the viewer:
```rust
        if let Some(v) = self.state.viewer.as_mut() {
            if v.image_id == image_id {
                v.color_profile = color_profile.clone();
            }
        }
```
Update the call site (line 626):
```rust
                crate::events::AppEvent::FullDecoded { image_id, image, color_profile } => {
                    self.apply_full_decoded(frame, *image_id, image, color_profile);
                }
```
Where the sparse VT + pipelines are fetched (the `let full = { let renderer = rs.renderer.read(); ... }` block, ~line 146), after building `full`, push the working→display matrix once (default Rec2020) using the pre-warmed pipelines:
```rust
            vp.pipelines
                .set_display_matrix(&gpu.queue, ferrolite_color::working_to_display(self.state.working_space));
```
(Add this inside the read-lock block where `vp` is in scope, before it is dropped.)

Replace the temporary identity in the `TileEditPipeline::new` call (line 192) with `self.camera_to_working()`. NOTE the borrow: compute `let cam = self.camera_to_working();` before the `self.state.viewer.as_mut()` borrow region and pass `cam`.

- [ ] **Step 7: Feed the matrix into `set_preview_and_full`'s builds**

In `set_preview_and_full`, compute `let cam = self.camera_to_working();` at the top (before the `v` borrow). Replace the `EditPipeline::new` identity arg (line 259) with `cam`, and the `TileEditPipeline::new` identity arg (line 298) with `cam`.

- [ ] **Step 8: `set_color_matrix` pass-through on the producer**

In `ferrolite-app/src/viewer/edit_producer.rs`, add a method mirroring the existing `set_stack` pass-through:
```rust
    pub fn set_color_matrix(&mut self, m: [[f32; 3]; 3]) {
        self.pipeline.set_color_matrix(m);
    }
```
(Field name matches the existing `set_stack` impl — grep `fn set_stack` in this file and mirror it.)

- [ ] **Step 9: The `apply_working_space` handler**

Add to `impl FerroliteApp` in `app.rs`:
```rust
    /// Change the editing working space: recompose camera→working + working→display,
    /// push the tail matrix to the display pipelines (once), update both edit tiers,
    /// and invalidate full-res tiles so they re-render. Never rebuilds pipelines.
    fn apply_working_space(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        ws: ferrolite_color::WorkingSpace,
    ) {
        if ws == self.state.working_space {
            return;
        }
        self.state.working_space = ws;
        let Some(rs) = frame.wgpu_render_state() else { return; };
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);

        // Push the working→display tail (shared uniform; not per-frame).
        {
            let renderer = rs.renderer.read();
            if let Some(vp) = renderer.callback_resources.get::<viewer::ViewerPipelines>() {
                vp.pipelines
                    .set_display_matrix(&gpu.queue, ferrolite_color::working_to_display(ws));
            }
        }

        let cam = self.camera_to_working();
        let Some(v) = self.state.viewer.as_mut() else { ctx.request_repaint(); return; };

        // Preview tier: update the matrix, re-evaluate, swap the displayed texture.
        if let Some(ep) = v.preview_edit.as_mut() {
            ep.set_color_matrix(cam);
            let img = ep.evaluate();
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == v.image_id {
                    g.preview
                        .update_single_from_texture(img.texture.clone(), (img.width, img.height));
                }
            }
        }

        // Full-res tier: update the producer's matrix + invalidate cached tiles.
        if let Some(producer) = v.edit_producer.as_mut() {
            producer.set_color_matrix(cam);
        }
        v.opstack_version = v.opstack_version.wrapping_add(1);
        let version = v.opstack_version;
        let image_id = v.image_id;
        {
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == image_id {
                    if let Some(full) = g.full.as_mut() {
                        full.set_opstack_version(&g.ctx, version);
                    }
                }
            }
        }
        v.idle = false;
        ctx.request_repaint();
    }
```

- [ ] **Step 10: Build + gate**

Run: `cargo build -p ferrolite-app` then `cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: compiles clean. (Wiring is exercised by Jann's visual test; no unit test.)

- [ ] **Step 11: Commit**

```bash
git add ferrolite-app/Cargo.toml ferrolite-app/src/events.rs ferrolite-app/src/viewer/load.rs ferrolite-app/src/viewer/mod.rs ferrolite-app/src/viewer/edit_producer.rs ferrolite-app/src/state.rs ferrolite-app/src/app.rs
git commit -m "feat(app): thread camera ColorProfile + working space into color pipeline"
```

---

## Task 6: Develop working-space selector

Add a `ComboBox` at the top of the Develop adjustment panel (above "Basic") that selects the working space and routes the change into `apply_working_space`. It is a preference, not an op — no per-control reset.

**Files:**
- Modify: `ferrolite-app/src/develop/adjustment_panel.rs` (add selector + outcome), `ferrolite-app/src/app.rs` (route the outcome)

**Interfaces:**
- Produces: the panel returns a signal for a working-space change. Extend `EditOutcome` or add a sibling return; simplest is a dedicated field on the panel's return. Use: change `show` to also surface an optional `WorkingSpace` selection.

- [ ] **Step 1: Surface a working-space change from the panel**

In `ferrolite-app/src/develop/adjustment_panel.rs`, add a struct for the panel result and a param for the current space. Add near the top:
```rust
use ferrolite_color::WorkingSpace;

/// What the adjustment panel produced this frame: an op edit and/or a working-space change.
pub struct PanelOutcome {
    pub edit: Option<EditOutcome>,
    pub working_space: Option<WorkingSpace>,
}
```
Change the `show` signature to accept the current working space and return `PanelOutcome`:
```rust
pub fn show(ui: &mut egui::Ui, state: &mut AppState, working_space: WorkingSpace) -> PanelOutcome {
```
At the end of `show`, wrap the existing `out` into `PanelOutcome { edit: out, working_space: ws_change }` (define `let mut ws_change: Option<WorkingSpace> = None;` near the top).

Insert the selector right after the save-state indicator block (after line 48), before "Basic":
```rust
    // ── Working space (spec §4.1) ── global preference; not an editable op, so no
    // per-control reset. Recomposes the ColorMatrixNode + display tail on change.
    {
        let mut ws = working_space;
        egui::ComboBox::from_label("Working space")
            .selected_text(format!("{ws:?}"))
            .show_ui(ui, |ui| {
                for w in WorkingSpace::ALL {
                    ui.selectable_value(&mut ws, w, format!("{w:?}"));
                }
            });
        if ws != working_space {
            ws_change = Some(ws);
        }
        ui.add_space(4.0);
    }
```

- [ ] **Step 2: Route the outcome in `app.rs`**

Find the `adjustment_panel::show(...)` call site in `app.rs` (grep `adjustment_panel::show`). Update it to pass the working space and handle both outputs:
```rust
        let outcome = crate::develop::adjustment_panel::show(ui, &mut self.state, self.state.working_space);
        if let Some(ws) = outcome.working_space {
            self.apply_working_space(ctx, frame, ws);
        }
        if let Some(e) = outcome.edit {
            self.apply_edit(ctx, frame, e.kind, e.stack, e.commit);
        }
```
(Adapt to the exact variable names/`ctx`/`frame` in scope at that call — the existing code already calls `apply_edit` with the old `EditOutcome`; replace that usage.)

- [ ] **Step 3: Build + clippy**

Run: `cargo build -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/develop/adjustment_panel.rs ferrolite-app/src/app.rs
git commit -m "feat(app): Develop working-space selector wired to the color pipeline"
```

---

## Task 7: Cross-cutting resizable side panels (spec §9)

Audit every `SidePanel` and make each resizable with a design-system default width + clamps; egui persists the width automatically for a stable panel id. There are exactly two `SidePanel`s: Library left `"left"` (already resizable) and Develop right `"develop_adjust"` (fixed at `exact_width(296.0)`).

**Files:**
- Modify: `ferrolite-app/src/app.rs:1044-1056` (the `develop_adjust` panel)

- [ ] **Step 1: Make `develop_adjust` resizable**

In `ferrolite-app/src/app.rs`, change the Develop right panel builder (line ~1044) from:
```rust
        egui::SidePanel::right("develop_adjust")
            .exact_width(296.0)
```
to:
```rust
        egui::SidePanel::right("develop_adjust")
            .resizable(true)
            .default_width(296.0)
            .width_range(250.0..=400.0)
```

- [ ] **Step 2: Confirm the Library left panel already conforms**

Verify `ferrolite-app/src/app.rs:800` reads `.resizable(true).default_width(236.0).width_range(180.0..=460.0)` (spec §9 says it is already resizable — no change needed). If any other `SidePanel` exists (grep `SidePanel::`), give it `.resizable(true)` + `.default_width(..)` + `.width_range(..)` with the design-system default and sensible clamps. (Top/bottom `TopBottomPanel`s are out of scope.)

- [ ] **Step 3: Build + clippy + full gate**

Run:
```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: fmt clean, clippy clean, all tests green (GPU goldens run on the dev GPU / skip headless).

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/app.rs
git commit -m "feat(app): make Develop adjustment panel resizable (resizable side-panels sweep)"
```

- [ ] **Step 5: Manual persistence check (visual test prep)**

Launch the app, drag the Develop right panel wider, close and reopen — confirm the width persists (egui memory). This is part of Jann's hands-on test.

---

## Finish

- [ ] **Full gate green:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace`.
- [ ] **Regenerate any GPU goldens on the dev GPU** (`UPDATE_GOLDEN=1` where a new fixture was added) and re-run without the env var to confirm stability; commit the fixture PNGs.
- [ ] **STOP and hold for Jann's hands-on visual test** (CLAUDE.md "Finishing a branch"): open a RAW → confirm colors look right at the default Rec.2020 working space; switch working spaces in the Develop selector and confirm the on-screen image and 1:1 view update; confirm sRGB working space looks identical to pre-change; drag/resize both side panels and confirm persistence. Address any issues found before merging. Do not finish the branch until Jann approves.

---

## Self-Review notes

- **Spec §5.1 (ColorMatrixNode at DAG head):** Tasks 1–2 (preview + full-res tiers). Op order preserved: `Source/GeoHead → ColorMatrix → Exposure → …`. `OpKind`/`OpStack` untouched → sidecar schema unchanged (it is not a user op).
- **Spec §5.2 (display tail, swappable, built once, pushed on WS change):** Task 3 — shared uniform in the pre-warmed `DisplayPipelines`, `set_display_matrix` via `write_buffer`, no per-frame/per-image rebuild.
- **Spec §4.3/§10 (sRGB ≡ old regression golden):** Task 3 Step 9 (existing vt goldens stay green under identity) + Task 4 Step 5 (explicit blit vs `srgb_oetf`).
- **Spec §9 (resizable side panels):** Task 7.
- **Spec §5 contract (executor photo-agnostic; engine crates copyleft-free):** `ferrolite-gpu::Graph` untouched; `ferrolite-vt` takes `[[f32;3];3]`, never a `ferrolite-color` type; `ferrolite-color` used only in pipeline (dev-dep) + app.
- **CLAUDE.md:** pipelines built once/pre-warmed (unchanged); off-thread decode carries the profile via the existing `FullDecoded` event; per-component reset N/A for the WS preference (documented).
- **Default Rec.2020 gotcha:** tail matrix is non-identity by default; the app pushes it on open (Task 5 Step 6) and on change (Step 9) — not left at the identity pre-warm default.

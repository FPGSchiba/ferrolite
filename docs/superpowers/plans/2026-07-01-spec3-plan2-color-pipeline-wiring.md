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

## Task 8: Close the unedited-display color gap (whole-branch review Issue 1)

The whole-branch review found that the display tail now applies `working→display` (non-identity by default at Rec.2020), but the **unedited** display paths never pass through a `camera→working` transform, so camera-native / sRGB-linear texels are rendered as if they were working-space linear. Deeper investigation showed the source color space **differs by tier**, so a single matrix is wrong:

- **Preview tier** — `preview_source` (set in `apply_preview_ready`) is the **sRGB embedded thumbnail** (`preview_to_linear` → sRGB-linear); for a Standard (non-RAW) image this is the *permanent* display. Correct transform: **sRGB→working**. (Task 5 mistakenly wired `camera→working` here — a latent bug this task also fixes.)
- **Full-res tier** — the sparse VT source is **camera-native** (`QuadBin` demosaic). Correct transform: **camera→working** (already correct via Tasks 1–2/5 *when the producer is attached*).

`camera_to_working(ColorProfile::srgb_fallback(), ws)` is exactly **sRGB→working** (the fallback's `xyz_to_cam` is XYZ→sRGB), so both matrices come from the existing `ferrolite_color::camera_to_working` — no new color math.

**Fix strategy:** every texture handed to the display tail must be working-space linear. Achieve it by (a) always driving the preview single-texture through the preview `EditPipeline` with the **sRGB→working** matrix (built eagerly on preview-ready), and (b) always driving the full-res tier through the `TileEditPipeline` producer with the **camera→working** matrix (drop the `is_identity` gate; always `set_producing(true)`). The raw CPU-upload tile path (camera-native) is never shown through the tail again.

**Files:** `ferrolite-app/src/app.rs` only (helpers + `apply_preview_ready` + `set_preview_and_full` + `apply_full_decoded` + `apply_working_space`).

**Interfaces produced:** `FerroliteApp::source_to_working(&self, profile: &ferrolite_decode::ColorProfile) -> [[f32;3];3]`; `FerroliteApp::camera_to_working(&self) -> [[f32;3];3]` (real profile, full-res); `FerroliteApp::preview_to_working(&self) -> [[f32;3];3]` (sRGB fallback, preview tier).

- [ ] **Step 1: Refactor the matrix helpers** (`app.rs` ~116–133)

Replace the current `camera_to_working` with three helpers:

```rust
    /// Compose a source→working 3×3 for `profile` under the current working space.
    fn source_to_working(&self, profile: &ferrolite_decode::ColorProfile) -> [[f32; 3]; 3] {
        ferrolite_color::camera_to_working(
            profile.xyz_to_cam,
            ferrolite_color::Xy {
                x: profile.white_xy[0],
                y: profile.white_xy[1],
            },
            self.state.working_space,
        )
    }

    /// camera→working for the open viewer's RAW profile (full-res tier).
    fn camera_to_working(&self) -> [[f32; 3]; 3] {
        match self.state.viewer.as_ref() {
            Some(v) => self.source_to_working(&v.color_profile),
            None => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    /// sRGB→working for the preview tier: the embedded preview and Standard images
    /// are sRGB-primaries, so they convert via the sRGB fallback profile.
    fn preview_to_working(&self) -> [[f32; 3]; 3] {
        self.source_to_working(&ferrolite_decode::ColorProfile::srgb_fallback())
    }
```

- [ ] **Step 2: Route the preview single-texture through sRGB→working** (`app.rs` `apply_preview_ready`, ~55–114)

Build the preview `EditPipeline` eagerly (identity op-stack, sRGB→working matrix) and display *its* output, so the single texture is working-space. Compute the two matrices BEFORE the `&mut self.state.viewer` borrow. Concretely:

- At the very top of `apply_preview_ready` (before `let Some(v) = self.state.viewer.as_mut()`), add:
  ```rust
        let pw = self.preview_to_working();
        let w2d = ferrolite_color::working_to_display(self.state.working_space);
  ```
- After `let linear = viewer::load::preview_to_linear(image);` and `v.preview_source = Some(...)`, build the pipeline and use its output. Replace the `let vt = { ... single_texture ... };` block with:
  ```rust
        let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let mut ep = ferrolite_pipeline::EditPipeline::new(
            ctx_arc,
            &linear,
            ferrolite_pipeline::OpStack::default(),
            pw,
        );
        let edited = ep.evaluate();
        let mut vt = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            // Push the working→display tail now: a Standard image never reaches
            // apply_full_decoded, so this is the only place its tail gets set.
            vp.pipelines.set_display_matrix(&gpu.queue, w2d);
            ferrolite_vt::VirtualTexture::single_texture(&gpu, &linear, &vp.pipelines)
        };
        vt.update_single_from_texture(edited.texture.clone(), (edited.width, edited.height));
        v.preview_edit = Some(ep);
  ```
- Leave the `fit`/`image_dims`/`loaded`/`idle` logic and the `ViewerGpu { preview: vt, .. }` insertion exactly as they are (the `insert` already moves `vt`).

- [ ] **Step 3: Preview tier uses sRGB→working; full-res always produces** (`app.rs` `set_preview_and_full`, ~250–352)

- After `let cam = self.camera_to_working();` (kept for the full-res tier), add `let pw = self.preview_to_working();` (both computed before the `&mut` borrow).
- In the preview-tier block, change the lazy `EditPipeline::new(..., cam)` (the `if v.preview_edit.is_none()` branch, ~291–296) to pass `pw` instead of `cam`. (preview_edit is normally built in Step 2, so this is a safety net; it must still use the preview matrix.)
- In the full-res block, change `full.set_producing(!identity);` (~345) to `full.set_producing(true);` and delete the now-unused `let identity = shown.is_identity();` line (~339). Update the stale comment above the block (~316–322): the sparse VT is now **always** producer-driven; the "before" (identity `shown`) is rendered by the producer with an identity op-stack + camera→working, i.e. the correct unedited image in working space — never the raw camera-native CPU path.

- [ ] **Step 4: Always attach the full-res producer on open** (`app.rs` `apply_full_decoded`, ~206–242)

Remove the `if !v.op_stack.is_identity()` gate so the producer is built for every RAW open (identity stack included). Replace the `if !v.op_stack.is_identity() { ... }` block with an unconditional build:

```rust
                    // Always attach the full-res producer so the sparse VT tiles
                    // pass through camera→working (the raw camera-native CPU path
                    // must never reach the working→display tail). Identity stack =
                    // unedited-but-color-managed.
                    let ctx_arc =
                        std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
                    let tep = ferrolite_pipeline::TileEditPipeline::new(
                        ctx_arc,
                        pyramid,
                        v.op_stack.clone(),
                        cam,
                    );
                    v.edit_producer = Some(viewer::EditTileProducer::new(tep));
                    let version = v.opstack_version.max(1);
                    let mut renderer = rs.renderer.write();
                    if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                        if g.image_id == image_id {
                            if let Some(full) = g.full.as_mut() {
                                full.set_producing(true);
                                full.set_opstack_version(&g.ctx, version);
                            }
                        }
                    }
```

(`pyramid` is already the `Arc<GpuPyramidSource>` built just above; it is moved into `TileEditPipeline::new`, so keep the existing `v.pyramid = Some(Arc::clone(&pyramid));` line before this block.)

- [ ] **Step 5: `apply_working_space` uses the right matrix per tier** (`app.rs` ~410–431)

- Before the `&mut` borrow, compute both: keep `let cam = self.camera_to_working();` and add `let pw = self.preview_to_working();`.
- In the preview-tier update, change `ep.set_color_matrix(cam);` to `ep.set_color_matrix(pw);`.
- Leave the full-res `producer.set_color_matrix(cam);` as-is (camera→working is correct there).

- [ ] **Step 6: Build + verify**

Run:
- `cargo build -p ferrolite-app`
- `cargo fmt --all` then `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings` (confirm no unused-variable warning from the removed `identity` binding)
- `cargo test --workspace`

This is display-path wiring — correctness is confirmed by Jann's visual test (unedited RAW colors at Rec.2020; sRGB working space matches the old look; Standard/JPEG colors correct; crossfade has no color jump beyond the inherent camera-JPEG-vs-raw difference). The automated bar is a clean gate.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/app.rs
git commit -m "fix(app): route unedited preview (sRGB→working) and full-res (camera→working) through the color pipeline"
```

---

## Task 9: Shader-module cache on `GpuContext` (fix the Task-8 open-latency regression)

The whole-branch re-review found that after Task 8 the app compiles ~8 uncached compute shaders synchronously on the UI thread on every image open (`apply_preview_ready` builds the preview `EditPipeline`) and every full-decode settle (`apply_full_decoded` builds the `TileEditPipeline` unconditionally — required, since the unedited full-res producer applies `camera→working`). Each edit node compiles its WGSL fresh per pipeline instance (no reuse) — a pre-existing Spec 2 pattern Task 8 moved onto the open/navigation path. This violates CLAUDE.md rule #1 (no multi-ms UI-thread work; the rule exists because "multi-second UI freezes on image open" already happened here).

**Fix (Jann-approved):** a process-global `ShaderModule` cache on `ferrolite-gpu::GpuContext` (engine tier — a generic memoization, no photo concepts) so each unique WGSL source is compiled **once per device** and reused across all `EditPipeline`/`TileEditPipeline`/node instances (CLAUDE.md "build shaders once and reuse"). Plus a startup pre-warm so even the first open reuses.

**Key design facts (verified):**
- `GpuContext::from_render_state` builds a *new* `GpuContext` per call but `.clone()`s the shared `rs.device` Arc, so `Arc::as_ptr(&device)` is **stable** across every open. Keying the cache by `(device-ptr, source)` therefore shares entries across opens.
- Headless tests each create a distinct device (distinct Arc). To keep the ptr key from being reused after a test device drops, the cache **pins** the device `Arc` in its entry (the one app device is already process-lived; test devices pin for the process — modest, acceptable).
- All edit shaders are `include_str!` `&'static str`; a `&'static str` HashMap key compares by content, so the same WGSL `include_str!`'d from both `pipeline.rs` and `tile_edit.rs` dedups to one module.
- `wgpu::{Device, Queue, ShaderModule}` are `Send + Sync`, so a `static Mutex<HashMap<..>>` holding `Arc`s is sound.

**Files:**
- Modify: `ferrolite-gpu/src/context.rs` (add cache + `GpuContext::shader_module`, + a cache test)
- Modify: `ferrolite-pipeline/src/nodes.rs` (route every node's module creation through `ctx.shader_module`)
- Create/modify: `ferrolite-pipeline/src/lib.rs` (add `pub fn prewarm_shaders(ctx: &GpuContext)`)
- Modify: `ferrolite-app/src/app.rs` (call `prewarm_shaders` at the startup pre-warm)

**Interfaces produced:** `GpuContext::shader_module(&self, label: &str, wgsl: &'static str) -> Arc<wgpu::ShaderModule>`; `ferrolite_pipeline::prewarm_shaders(ctx: &ferrolite_gpu::GpuContext)`.

- [ ] **Step 1: Write the failing cache test**

In `ferrolite-gpu/src/context.rs` `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn shader_module_is_compiled_once_and_reused() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        const SRC: &str = "@compute @workgroup_size(1,1,1) fn main() {}";
        let a = ctx.shader_module("cache-test", SRC);
        let b = ctx.shader_module("cache-test", SRC);
        assert!(
            std::sync::Arc::ptr_eq(&a, &b),
            "same (device, source) must return the cached module"
        );
    }
```

- [ ] **Step 2: Run it to verify failure**

Run: `cargo test -p ferrolite-gpu shader_module_is_compiled_once -- --nocapture`
Expected: FAIL — no method `shader_module`.

- [ ] **Step 3: Implement the cache + method**

In `ferrolite-gpu/src/context.rs`, extend the imports and add the cache above `impl GpuContext`:

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// One device's compiled shader modules, keyed by WGSL source (content-compared
/// via `&'static str`). `_device` pins the `Arc` so the device-pointer cache key
/// cannot be reused by a later device allocated at the same address.
struct DeviceShaders {
    _device: Arc<wgpu::Device>,
    modules: HashMap<&'static str, Arc<wgpu::ShaderModule>>,
}

/// Process-global shader cache: `device-ptr -> that device's compiled modules`.
fn shader_cache() -> &'static Mutex<HashMap<usize, DeviceShaders>> {
    static CACHE: OnceLock<Mutex<HashMap<usize, DeviceShaders>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}
```

(Replace the existing `use std::sync::Arc;` line with the combined `use` above.)

Add the method inside `impl GpuContext`:

```rust
    /// Compile `wgsl` into a `ShaderModule` at most once per (device, source) and
    /// reuse it across every pipeline instance. Compiling WGSL (naga front-end) is
    /// the expensive part of pipeline creation; caching it keeps per-image
    /// EditPipeline/TileEditPipeline builds off the "recompile on every open" path
    /// (CLAUDE.md: build shaders once and reuse). Safe to call on each node build.
    pub fn shader_module(&self, label: &str, wgsl: &'static str) -> Arc<wgpu::ShaderModule> {
        let dev_key = Arc::as_ptr(&self.device) as usize;
        let mut guard = shader_cache().lock().expect("shader cache mutex");
        let entry = guard.entry(dev_key).or_insert_with(|| DeviceShaders {
            _device: Arc::clone(&self.device),
            modules: HashMap::new(),
        });
        if let Some(m) = entry.modules.get(wgsl) {
            return Arc::clone(m);
        }
        // Compiled under the lock: node construction is single-threaded (render/UI
        // thread) and not a per-frame hot path, so contention is negligible.
        let module = Arc::new(
            self.device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(label),
                    source: wgpu::ShaderSource::Wgsl(wgsl.into()),
                }),
        );
        entry.modules.insert(wgsl, Arc::clone(&module));
        module
    }
```

- [ ] **Step 4: Run the cache test to green**

Run: `cargo test -p ferrolite-gpu shader_module_is_compiled_once -- --nocapture`
Expected: PASS on the dev GPU; skips headless.

- [ ] **Step 5: Route every edit node's module creation through the cache**

In `ferrolite-pipeline/src/nodes.rs`:

- Change `point_op_pipeline` to take a prebuilt module instead of compiling. Replace its signature + body:
  ```rust
  fn point_op_pipeline(
      device: &wgpu::Device,
      bgl: &wgpu::BindGroupLayout,
      module: &wgpu::ShaderModule,
      label: &str,
  ) -> wgpu::ComputePipeline {
      let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
          label: Some(label),
          bind_group_layouts: &[bgl],
          push_constant_ranges: &[],
      });
      device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
          label: Some(label),
          layout: Some(&layout),
          module,
          entry_point: "main",
          compilation_options: Default::default(),
          cache: None,
      })
  }
  ```
- In `PointOpNode::new`, change the `wgsl: &str` param to `wgsl: &'static str`, and build the module via the cache:
  ```rust
      pub(crate) fn new(
          ctx: Arc<GpuContext>,
          wgsl: &'static str,
          label: &str,
          params: Rc<Cell<U>>,
      ) -> Self {
          let bgl = point_op_bgl(&ctx.device);
          let module = ctx.shader_module(label, wgsl);
          let pipeline = point_op_pipeline(&ctx.device, &bgl, &module, label);
          // ... unchanged (uniform_buf, Self { .. }) ...
  ```
- In `CurveNode::new`, replace the inline `create_shader_module` with:
  ```rust
          let module = ctx.shader_module("tone-curve", include_str!("shaders/tone_curve.wgsl"));
  ```
  and change the pipeline's `module: &module,` (drop the old `module: &module` built inline — reuse the cached one). Keep the rest (layout, create_compute_pipeline) the same, referencing `&module`.
- In `GeometryNode::new` and `GeometryHeadNode::new`, replace their inline `create_shader_module(... include_str!("shaders/geometry.wgsl") ...)` with:
  ```rust
          let module = ctx.shader_module("geometry", include_str!("shaders/geometry.wgsl"));
  ```
  (both use the same geometry.wgsl → they share one cached module) and reference `&module` in `create_compute_pipeline`.

All `PointOpNode::new` call sites already pass `include_str!(...)` (`&'static str`), so the tightened param type compiles unchanged.

- [ ] **Step 6: Add the startup pre-warm helper**

In `ferrolite-pipeline/src/lib.rs`, add (adjust the `use`/path so `include_str!` resolves from `src/`):

```rust
/// Pre-compile every edit-pass shader on `ctx` so the first image open reuses
/// cached modules instead of compiling on the UI thread. Call once at startup,
/// alongside the display-pipeline pre-warm.
pub fn prewarm_shaders(ctx: &ferrolite_gpu::GpuContext) {
    for (label, src) in [
        ("color-matrix", include_str!("shaders/color_matrix.wgsl")),
        ("exposure", include_str!("shaders/exposure.wgsl")),
        ("white-balance", include_str!("shaders/white_balance.wgsl")),
        ("contrast", include_str!("shaders/contrast.wgsl")),
        ("tone-curve", include_str!("shaders/tone_curve.wgsl")),
        ("hsl", include_str!("shaders/hsl.wgsl")),
        ("sharpen", include_str!("shaders/sharpen.wgsl")),
        ("geometry", include_str!("shaders/geometry.wgsl")),
    ] {
        let _ = ctx.shader_module(label, src);
    }
}
```

> These `include_str!` paths resolve from `ferrolite-pipeline/src/` (lib.rs's dir) to `src/shaders/…`, the same files the nodes include — so the content-keyed cache entries pre-warmed here are exactly the ones the node constructors look up.

- [ ] **Step 7: Call the pre-warm at startup**

In `ferrolite-app/src/app.rs` `FerroliteApp::new` (the `if let Some(rs) = cc.wgpu_render_state()` pre-warm block, where `DisplayPipelines::new` is called on the local `gpu`), add after the pipelines are built:

```rust
        ferrolite_pipeline::prewarm_shaders(&gpu);
```

(Use the same `gpu = GpuContext::from_render_state(rs)` already built there for the display pre-warm; it clones the same `rs.device` Arc used on every later open, so the cache key matches.)

- [ ] **Step 8: Verify (full gate) + confirm reuse**

Run:
- `cargo build -p ferrolite-app`
- `cargo fmt --all` then `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` — all green, including the new `shader_module_is_compiled_once_and_reused` and all existing GPU goldens (behavior is unchanged: same shaders, now reused).

- [ ] **Step 9: Commit**

```bash
git add ferrolite-gpu/src/context.rs ferrolite-pipeline/src/nodes.rs ferrolite-pipeline/src/lib.rs ferrolite-app/src/app.rs
git commit -m "perf(gpu): cache compiled shader modules per device; pre-warm edit shaders at startup"
```

---

## Task 10: Preview-open latency — off-thread conversion + single color pass

**Evidence (release build, measured):** for a large Standard image (3072×2048) `apply_preview_ready` took **713 ms on the UI thread**, breakdown: `preview_to_linear` 161 (sRGB→f32, per-pixel, on UI thread — pre-existing) · `linear.clone` 16 · eager `EditPipeline::new` 117 · `evaluate` 213 (9 compute passes, 8 identity no-ops) · throwaway `single_texture` upload 200 (immediately overwritten). ~530 ms of that is redundant work Task 8 introduced.

**Root cause (systematic-debugging, confirmed):** Task 8's preview path (a) uploads the image **twice** (once in `EditPipeline::new`'s source, once in the discarded `single_texture`), (b) builds+evaluates a **full 9-node** pipeline just to apply one sRGB→working matrix, and (c) runs the per-pixel sRGB→linear conversion **on the UI thread**.

**Fix (author-approved "Full"):**
1. **Off-thread conversion** — `PreviewReady` carries a `LinearRgbaF32` converted in the decode job (not an `ImageBuffer`); `preview_to_linear` moves off the UI thread.
2. **Single color pass** — the initial preview is produced by ONE sRGB→working color-matrix pass (`ferrolite_pipeline::color_convert`), displayed directly (no throwaway upload); the full 9-node `EditPipeline` (`preview_edit`) stays **lazy**, built on first edit (as before Task 8).
3. WS-change-before-edit re-runs the single pass.

Keeps working-space correctness and runtime WS switching. Realistic result: worst case ~713 ms → ~100–150 ms UI-thread (the residual is the single f32→f16 GPU upload). Re-measure to confirm.

**Files:** `ferrolite-vt/src/view.rs` (new `single_from_texture`), `ferrolite-pipeline` (new `color_convert` + re-export), `ferrolite-app/src/events.rs`, `viewer/load.rs`, `app.rs`.

**Interfaces produced:**
- `ferrolite_vt::VirtualTexture::single_from_texture(ctx: &GpuContext, texture: std::sync::Arc<wgpu::Texture>, dims: (u32,u32), pipelines: &DisplayPipelines) -> Self` — a rung-1 VT wrapping an existing `Rgba16Float` `TEXTURE_BINDING` texture (no upload).
- `ferrolite_pipeline::color_convert(ctx: std::sync::Arc<GpuContext>, src: &LinearRgbaF32, matrix: [[f32;3];3]) -> PipelineImage` — upload `src` + ONE `color_matrix.wgsl` pass; returns the working-space texture.

- [ ] **Step 1: `VirtualTexture::single_from_texture` (ferrolite-vt)**

In `ferrolite-vt/src/view.rs`, add a constructor mirroring `single_texture` but taking an existing texture instead of uploading a `LinearRgbaF32`. Refactor the shared tail of `single_texture` (everything after the texture exists: grab bgl/pipeline/sampler/uniform_buf, build `SingleResources`, wrap in `Self`) into this. Concretely add:

```rust
    /// Rung-1 VT wrapping an already-GPU-resident `Rgba16Float` texture
    /// (`TEXTURE_BINDING`), e.g. a pipeline color-convert output. No upload.
    pub fn single_from_texture(
        ctx: &GpuContext,
        texture: std::sync::Arc<wgpu::Texture>,
        dims: (u32, u32),
        pipelines: &DisplayPipelines,
    ) -> Self {
        let device = &ctx.device;
        let bgl = pipelines.layout(DisplayVariant::Single).clone();
        let pipeline = pipelines.pipeline(DisplayVariant::Single).clone();
        let sampler = pipelines.sampler().clone();
        let display_matrix = pipelines.display_matrix_buffer().clone();
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-xf"),
            size: std::mem::size_of::<TransformUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            single: Some(SingleResources {
                texture,
                texture_view,
                bind_group_layout: bgl,
                sampler,
                pipeline,
                image_dims: dims,
                uniform_buf,
                display_matrix,
                bind_group: None,
            }),
            tiled: None,
            streaming: None,
            sparse: None,
        }
    }
```

(If `SingleResources` has fields beyond these, match `single_texture`'s literal exactly — read it first.) A quick unit test can assert `single_dims()` equals the passed dims, mirroring the existing `update_single_swaps_dims` test.

- [ ] **Step 2: `color_convert` (ferrolite-pipeline)**

In `ferrolite-pipeline/src/nodes.rs` (near `upload_source`), add — reusing the crate-internal `upload_source` + `PointOpNode<ColorMatrixUniform>` + the cached `color_matrix.wgsl`:

```rust
/// One-shot camera/sRGB→working color pass: upload `src`, run a single
/// `color_matrix.wgsl` pass, return the working-space texture. Cheaper than a
/// full `EditPipeline` for the preview's initial color conversion (one upload,
/// one pass). Uses the shared shader cache (built once) via `PointOpNode`.
pub fn color_convert(
    ctx: std::sync::Arc<GpuContext>,
    src: &LinearRgbaF32,
    matrix: [[f32; 3]; 3],
) -> PipelineImage {
    let source = upload_source(&ctx, src);
    let params = std::rc::Rc::new(std::cell::Cell::new(crate::uniforms::color_matrix_uniform(
        matrix,
    )));
    let node = PointOpNode::new(
        ctx,
        include_str!("shaders/color_matrix.wgsl"),
        "preview-color-convert",
        params,
    );
    node.evaluate(&[&source])
}
```

Re-export from `ferrolite-pipeline/src/lib.rs`: `pub use nodes::color_convert;` (and ensure `upload_source`/`PointOpNode`/`color_matrix_uniform` are reachable from `nodes` — they are, in-crate). `PointOpNode::evaluate` is the `Node` trait method; import `ferrolite_gpu::Node` in `nodes.rs` if not already in scope (it is).

- [ ] **Step 3: Move sRGB→linear off-thread (`PreviewReady` carries `LinearRgbaF32`)**

- `ferrolite-app/src/events.rs`: change `PreviewReady { image_id: i64, image: ferrolite_image::ImageBuffer }` → `PreviewReady { image_id: i64, linear: ferrolite_image::LinearRgbaF32 }`. The `apply()` fold arm `AppEvent::PreviewReady { .. } => None` is unaffected (`..`).
- `ferrolite-app/src/viewer/load.rs` `spawn_preview`: after `decode_preview(...) => Ok(image)`, convert in the job: `let linear = preview_to_linear(&image);` then `tx.send(AppEvent::PreviewReady { image_id, linear })`. (`preview_to_linear` now runs on the job thread.)
- `ferrolite-app/src/app.rs` match (~line 752): `AppEvent::PreviewReady { image_id, linear } => self.apply_preview_ready(frame, *image_id, linear)`.

- [ ] **Step 4: Rewrite `apply_preview_ready` (one pass, lazy preview_edit)**

Change the signature to `image: &ferrolite_image::LinearRgbaF32` → rename the param `linear`. Replace the body from the `preview_to_linear` line through `v.preview_edit = Some(ep);` (currently app.rs ~77–108) with:

```rust
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
        let dims = (linear.width, linear.height);
        // Retain the sRGB-linear source so the full preview EditPipeline can be
        // built lazily on the first edit (built once, reused via set_stack).
        let src = std::sync::Arc::new(linear.clone());
        v.preview_source = Some(src.clone());
        // Initial preview: ONE sRGB→working color pass (not a full 9-node
        // pipeline). Display its working-space output directly.
        let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let converted = ferrolite_pipeline::color_convert(ctx_arc, &src, pw);
        let vt = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            // A Standard image never reaches apply_full_decoded, so set the tail here.
            vp.pipelines.set_display_matrix(&gpu.queue, w2d);
            ferrolite_vt::VirtualTexture::single_from_texture(
                &gpu,
                converted.texture.clone(),
                (converted.width, converted.height),
                &vp.pipelines,
            )
        };
```

Then keep the existing `viewport`/`fit`/`image_dims`/`loaded`/`idle` block and the `ViewerGpu { preview: vt, .. }` insertion unchanged (they still reference `dims`/`vt`). `preview_edit` is NOT set here — it stays `None` (lazy).

> `set_preview_and_full` already builds `preview_edit` lazily (`if v.preview_edit.is_none()`) with the `pw` matrix on the first edit — leave that as-is (confirm it passes `pw`, not `cam`).

- [ ] **Step 5: WS-change before any edit re-runs the single pass**

In `apply_working_space` (app.rs), the preview-tier update currently only runs `if let Some(ep) = v.preview_edit`. Add an `else` for the lazy case: recompute the single pass and swap the displayed texture. Compute `let pw = self.preview_to_working();` before the `&mut` borrow (alongside the existing `cam`). Then:

```rust
        if let Some(ep) = v.preview_edit.as_mut() {
            ep.set_color_matrix(pw);
            let img = ep.evaluate();
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == v.image_id {
                    g.preview
                        .update_single_from_texture(img.texture.clone(), (img.width, img.height));
                }
            }
        } else if let Some(src) = v.preview_source.clone() {
            // No edit yet: re-run the one-shot color pass with the new matrix.
            let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
            let converted = ferrolite_pipeline::color_convert(ctx_arc, &src, pw);
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == v.image_id {
                    g.preview.update_single_from_texture(
                        converted.texture.clone(),
                        (converted.width, converted.height),
                    );
                }
            }
        }
```

(The existing preview-tier block used `cam`; it must now use `pw` — the preview source is sRGB. Full-res `producer.set_color_matrix(cam)` stays `cam`.)

- [ ] **Step 6: Verify + re-measure**

- `cargo build -p ferrolite-app`; `cargo fmt --all` + `--check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace` (all green — the color goldens are unchanged; add the `single_from_texture` dims test).
- Add a TEMPORARY `eprintln!("[perf] preview_ready TOTAL {:?}", t.elapsed())` at the end of `apply_preview_ready` (a `let t = std::time::Instant::now();` at the top) so the author can re-run `cargo run -p ferrolite-app --release`, open the same large image, and confirm the drop. Remove this timer in a follow-up cleanup commit once confirmed.
- **Color correctness must be re-verified** (this re-touches the Task 8 path): unedited Standard image colors match the pre-Task-10 look; RAW unedited still correct; WS switch (before and after an edit) recolors; first edit still works.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-vt/src/view.rs ferrolite-pipeline/src/nodes.rs ferrolite-pipeline/src/lib.rs ferrolite-app/src/events.rs ferrolite-app/src/viewer/load.rs ferrolite-app/src/app.rs
git commit -m "perf(app): off-thread preview conversion + single color pass on open (was ~700ms → ~150ms)"
```

---

## Self-Review notes

- **Spec §5.1 (ColorMatrixNode at DAG head):** Tasks 1–2 (preview + full-res tiers). Op order preserved: `Source/GeoHead → ColorMatrix → Exposure → …`. `OpKind`/`OpStack` untouched → sidecar schema unchanged (it is not a user op).
- **Spec §5.2 (display tail, swappable, built once, pushed on WS change):** Task 3 — shared uniform in the pre-warmed `DisplayPipelines`, `set_display_matrix` via `write_buffer`, no per-frame/per-image rebuild.
- **Spec §4.3/§10 (sRGB ≡ old regression golden):** Task 3 Step 9 (existing vt goldens stay green under identity) + Task 4 Step 5 (explicit blit vs `srgb_oetf`).
- **Spec §9 (resizable side panels):** Task 7.
- **Spec §5 contract (executor photo-agnostic; engine crates copyleft-free):** `ferrolite-gpu::Graph` untouched; `ferrolite-vt` takes `[[f32;3];3]`, never a `ferrolite-color` type; `ferrolite-color` used only in pipeline (dev-dep) + app.
- **CLAUDE.md:** pipelines built once/pre-warmed (unchanged); off-thread decode carries the profile via the existing `FullDecoded` event; per-component reset N/A for the WS preference (documented).
- **Default Rec.2020 gotcha:** tail matrix is non-identity by default; the app pushes it on open (Task 5 Step 6) and on change (Step 9) — not left at the identity pre-warm default.

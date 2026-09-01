# Library Grid Uniform Cells Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Library grid's justified-rows layout with a uniform cell grid whose images are letterboxed and whose filenames elide, so cell sizes are consistent, cell height cannot collapse, and no filename is ever clipped.

**Architecture:** Four sequential tasks, each leaving the running app correct. Task 1 adds the pure uniform-grid geometry alongside the existing solver (nothing consumes it yet). Task 2 stops filenames from driving cell width — labels elide, images letterbox — which on its own removes the row collapse, the right-edge overflow, and the O(all-items) text measurement, while still using the justified layout. Task 3 swaps the layout model to the uniform grid and deletes the solver. Task 4 adopts the documented Size-slider range.

**Tech Stack:** Rust, egui 0.29.1 / eframe, wgpu. Crate: `ferrolite-app` only.

**Spec:** `docs/superpowers/specs/2026-08-31-library-grid-uniform-cells-design.md` (approved 2026-08-31; decisions D1–D7 in its §3)

## Global Constraints

- **Crate scope:** `ferrolite-app` only. No other crate is touched. Run the **scoped gate** — `cargo fmt -p ferrolite-app -- --check`, `cargo clippy -p ferrolite-app --all-targets -- -D warnings`, `cargo test -p ferrolite-app` — NOT the repo gate. The coordinator runs the repo gate once at the end.
- **Virtualization is load-bearing (CLAUDE.md §1).** The grid MUST realize and decode only the cells on screen and MUST NOT do O(all-items) work per frame. This rule exists because it was violated before and caused multi-second freezes. Every task preserves it.
- **No text measurement to size cells.** Eliding to a known cell width is required; measuring filenames to fit is forbidden — it is the O(all-items) work this plan removes.
- **Icons:** every icon comes from `ferrolite-app/src/icons.rs`. Do not add raw emoji/symbol characters (they render as tofu) and do not hand-draw new icons with `Painter`. This plan adds no new icons.
- **Colour tokens** live in `ferrolite-app/src/theme.rs` and have a guard test. Use existing tokens (`theme::BG_PANEL`, `theme::TEXT_PRIMARY`, `theme::TEXT_DIM`, `theme::ACCENT_BRIGHT`); do not invent new ones.
- **Per-control reset / keybind discoverability:** this plan adds no adjustable control and no keybind, so neither rule triggers. Do not add one.
- **`egui::Image::paint_at` ignores the `fit` field.** `Image::fit_to_exact_size` is a silent no-op on the `paint_at` path (`fit` is only read by `Image::ui`/`calc_size`). Always compute the fitted rect explicitly with `grid_layout::fit_size` before painting, or thumbnails are stretched.
- **`grid_layout::fit_size` already exists** (added in commit `1680fc8` for the export queue) and is also consumed by `ferrolite-app/src/export_module/queue_list.rs:234`. Do not change its signature or behaviour; both grids share it.
- **Commit after each task.** Conventional-commit format (`fix:`/`feat:`/`refactor:`/`test:`). Do NOT add a `Co-Authored-By` trailer — attribution is disabled for this repo.

---

### Task 1: Pure uniform-grid geometry

Adds the closed-form uniform-grid arithmetic to the pure, egui-free layout module. Nothing consumes it yet — `grid.rs` still uses the justified solver — so this task cannot change app behaviour. `ferrolite-app` has a lib target (`src/lib.rs` declares `pub mod library;`), so these `pub` items are public API and will NOT trip `dead_code` before their consumer lands in Task 3.

**Files:**
- Modify: `ferrolite-app/src/library/grid_layout.rs` (append to the module body, before `#[cfg(test)] mod tests`; add tests inside that module)

**Interfaces:**
- Consumes: `fit_size(cell_w: f32, cell_h: f32, aspect: f32) -> (f32, f32)` — already in this file, unchanged.
- Produces, all in `crate::library::grid_layout`:
  - `pub const CELL_ASPECT: f32` (= 1.5, spec D7)
  - `pub struct UniformGridLayout { pub cols: usize, pub cell_w: f32, pub cell_h: f32, pub gap: f32, pub row_stride: f32, pub item_count: usize, pub total_height: f32, pub x_offset: f32 }`
  - `pub fn uniform_layout(item_count: usize, avail_w: f32, cell_w: f32, gap: f32, label_pad: f32, label_h: f32) -> UniformGridLayout`
  - `impl UniformGridLayout`: `pub fn row_count(&self) -> usize`, `pub fn cell_offset(&self, index: usize) -> (f32, f32)`, `pub fn visible_rows(&self, scroll_top: f32, viewport_h: f32) -> Range<usize>`, `pub fn indices_for_rows(&self, rows: Range<usize>) -> Range<usize>`
  - `pub struct UniformLayoutSig { pub images_rev: u64, pub item_count: usize, pub avail_w: u32, pub cell_w: u32 }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub struct CachedUniformLayout { pub sig: UniformLayoutSig, pub layout: UniformGridLayout }` (derives `Debug, Clone`)

**Deliberate deviation from spec 4.4:** the spec sketches `cell_rect(&self, index) -> Rect`, but `egui::Rect` would make this module depend on egui and stop it being the pure, unit-testable layer its own doc comment promises. `cell_offset` returns a plain `(f32, f32)` and `grid.rs` builds the `Rect`. Same information, purity preserved.

- [ ] **Step 1: Write the failing tests**

Append inside the existing `#[cfg(test)] mod tests { ... }` block in `ferrolite-app/src/library/grid_layout.rs`, after the `fit_size` tests:

```rust
    // --- uniform grid geometry ------------------------------------------

    /// Defaults close to the real grid: 8px gap, 3px label pad, 30px label band.
    fn uni(item_count: usize, avail_w: f32, cell_w: f32) -> UniformGridLayout {
        uniform_layout(item_count, avail_w, cell_w, 8.0, 3.0, 30.0)
    }

    #[test]
    fn uniform_cols_derive_from_available_width() {
        // cell 100 + gap 8 => stride 108. avail 340: (340+8)/108 = 3.22 -> 3 cols
        // (3 cells + 2 gaps = 316 <= 340; a 4th would need 424).
        assert_eq!(uni(20, 340.0, 100.0).cols, 3);
        // Exactly four strides minus the trailing gap.
        assert_eq!(uni(20, 424.0, 100.0).cols, 4);
        // One pixel short of four.
        assert_eq!(uni(20, 423.0, 100.0).cols, 3);
    }

    #[test]
    fn uniform_cols_never_zero_even_when_the_panel_is_narrower_than_a_cell() {
        // A panel narrower than one cell must still lay out one column, or the
        // grid would divide by zero / show nothing.
        assert_eq!(uni(5, 10.0, 200.0).cols, 1);
        assert_eq!(uni(5, 0.0, 200.0).cols, 1);
    }

    #[test]
    fn uniform_cell_height_follows_the_cell_aspect() {
        let l = uni(1, 1000.0, 300.0);
        assert!(
            (l.cell_h - 300.0 / CELL_ASPECT).abs() < 0.01,
            "cell_h must be cell_w / CELL_ASPECT, got {}",
            l.cell_h
        );
    }

    /// Ask 1: every cell is the same box. The size is a single pair on the
    /// layout, so identity is structural - what is worth asserting is that the
    /// positions are all DISTINCT (no two cells stacked on each other), i.e. the
    /// grid really is a tiling of one repeated box.
    #[test]
    fn uniform_every_cell_has_identical_dimensions_at_distinct_positions() {
        let l = uni(37, 900.0, 150.0);
        assert!(l.cell_w > 0.0 && l.cell_h > 0.0);
        let mut seen: Vec<(i32, i32)> = Vec::new();
        for i in 0..l.item_count {
            let (x, y) = l.cell_offset(i);
            let key = ((x * 100.0) as i32, (y * 100.0) as i32);
            assert!(
                !seen.contains(&key),
                "cell {i} shares a position with an earlier cell"
            );
            seen.push(key);
        }
        assert_eq!(seen.len(), 37);
    }

    /// Ask 2: height is a pure function of the slider-derived cell width and can
    /// never collapse. Independent of any image's aspect — there is no solver.
    #[test]
    fn uniform_cell_height_scales_with_cell_width_and_never_collapses() {
        let mut prev = 0.0_f32;
        // The V2 Size range adopted in spec D4.
        for pct in [0.0_f32, 25.0, 46.0, 75.0, 100.0] {
            let cell_w = 118.0 + pct * 1.7;
            let l = uni(50, 900.0, cell_w);
            assert!(
                l.cell_h > prev,
                "cell_h must grow with the slider: {} !> {prev}",
                l.cell_h
            );
            assert!(
                l.cell_h >= 118.0 / CELL_ASPECT - 0.01,
                "cell_h {} fell below the minimum slider position's height",
                l.cell_h
            );
            prev = l.cell_h;
        }
    }

    /// The direct regression for the overflow that clipped filenames: cells tile
    /// without overlapping and never cross the available width.
    #[test]
    fn uniform_cells_tile_without_overlap_and_stay_within_width() {
        for avail_w in [200.0_f32, 340.0, 501.0, 900.0, 1600.0] {
            for cell_w in [118.0_f32, 150.0, 288.0] {
                let l = uni(23, avail_w, cell_w);
                let mut boxes: Vec<(f32, f32, f32, f32)> = Vec::new();
                for i in 0..l.item_count {
                    let (x, y) = l.cell_offset(i);
                    assert!(
                        x >= -0.01 && x + l.cell_w <= avail_w.max(cell_w) + 0.01,
                        "avail {avail_w} cell {cell_w}: item {i} at x={x} w={} \
                         escapes the width",
                        l.cell_w
                    );
                    boxes.push((x, y, x + l.cell_w, y + l.cell_h));
                }
                for a in 0..boxes.len() {
                    for b in (a + 1)..boxes.len() {
                        let (ax0, ay0, ax1, ay1) = boxes[a];
                        let (bx0, by0, bx1, by1) = boxes[b];
                        let overlap =
                            ax0 < bx1 - 0.01 && bx0 < ax1 - 0.01 && ay0 < by1 - 0.01 && by0 < ay1 - 0.01;
                        assert!(!overlap, "cells {a} and {b} overlap");
                    }
                }
            }
        }
    }

    #[test]
    fn uniform_total_height_covers_every_row() {
        let l = uni(10, 340.0, 100.0); // 3 cols -> 4 rows
        assert_eq!(l.cols, 3);
        assert_eq!(l.row_count(), 4);
        let expected = 4.0 * l.row_stride;
        assert!(
            (l.total_height - expected).abs() < 0.01,
            "total_height {} != {expected}",
            l.total_height
        );
        // The last cell's bottom must fit inside the reported content height.
        let (_x, y) = l.cell_offset(9);
        assert!(y + l.cell_h <= l.total_height + 0.01);
    }

    #[test]
    fn uniform_empty_layout_has_no_rows_and_no_height() {
        let l = uni(0, 900.0, 150.0);
        assert_eq!(l.row_count(), 0);
        assert_eq!(l.total_height, 0.0);
        assert_eq!(l.indices_for_rows(l.visible_rows(0.0, 600.0)), 0..0);
    }

    #[test]
    fn uniform_visible_rows_windows_around_scroll() {
        let l = uni(300, 900.0, 150.0);
        let r = l.visible_rows(0.0, 300.0);
        assert_eq!(r.start, 0, "clamped at the top");
        assert!(r.end >= 2 && r.end <= l.row_count());
        // Scrolled well down: the window must move with the scroll.
        let mid = l.visible_rows(l.total_height * 0.5, 300.0);
        assert!(mid.start > 0, "a mid-scroll window must not start at row 0");
        assert!(mid.end <= l.row_count());
    }

    /// Virtualization guard: the realized index range is bounded by the viewport,
    /// NOT by the item count. This is the property CLAUDE.md §1 requires.
    #[test]
    fn uniform_indices_for_rows_is_bounded_by_the_viewport_not_the_item_count() {
        let l = uni(100_000, 900.0, 150.0);
        let rows = l.visible_rows(0.0, 600.0);
        let idx = l.indices_for_rows(rows.clone());
        let realized = idx.end - idx.start;
        let max_expected = l.cols * (rows.end - rows.start);
        assert_eq!(
            realized, max_expected,
            "realized {realized} should be cols x visible rows"
        );
        assert!(
            realized < 200,
            "a 600px viewport realized {realized} of 100000 cells — \
             virtualization is broken"
        );
    }

    #[test]
    fn uniform_indices_for_rows_clamps_to_the_item_count() {
        // 10 items, 3 cols -> 4 rows; the last row is partial.
        let l = uni(10, 340.0, 100.0);
        let idx = l.indices_for_rows(0..l.row_count());
        assert_eq!(idx, 0..10, "must never index past the item list");
        // An out-of-range row range must clamp, not panic or overrun.
        assert_eq!(l.indices_for_rows(3..99), 9..10);
        assert_eq!(l.indices_for_rows(99..200), 10..10);
    }

    #[test]
    fn uniform_leftover_width_becomes_a_centering_offset() {
        // Spec D6: gaps stay exactly `gap`; leftover width centers the block.
        let cell_w = 100.0_f32;
        let gap = 8.0_f32;
        let avail_w = 500.0_f32;
        let l = uniform_layout(20, avail_w, cell_w, gap, 3.0, 30.0);
        // 4 cols: 4*100 + 3*8 = 424. Leftover 76 -> 38 each side.
        assert_eq!(l.cols, 4);
        assert!(
            (l.x_offset - 38.0).abs() < 0.01,
            "x_offset {} should center the 424px block in 500px",
            l.x_offset
        );
        // Consecutive cells in a row are exactly `gap` apart.
        let (x0, _) = l.cell_offset(0);
        let (x1, _) = l.cell_offset(1);
        assert!((x1 - (x0 + cell_w + gap)).abs() < 0.01, "gap must stay exact");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-app --lib grid_layout 2>&1 | tail -30`

Expected: FAIL — compile errors, `cannot find function 'uniform_layout' in this scope`, `cannot find type 'UniformGridLayout'`, `cannot find value 'CELL_ASPECT'`.

- [ ] **Step 3: Write the implementation**

Insert into `ferrolite-app/src/library/grid_layout.rs` immediately **before** the `#[cfg(test)] mod tests {` line:

```rust
/// The Library grid's fixed cell aspect ratio (width / height): 3:2, per
/// `docs/design/V2/README.md:42` and spec decision D7. A uniform grid derives
/// cell height from cell width through this constant, which is why a cell's
/// height cannot depend on any image's shape — the property that makes a
/// collapsed row impossible.
pub const CELL_ASPECT: f32 = 1.5;

/// A uniform (fixed-cell) grid: every cell is the same `cell_w x cell_h` box,
/// laid out in `cols` columns. Closed-form - there is no per-item vector and no
/// solver, so building it is O(1) regardless of item count (the justified
/// layout it replaces was O(items) plus O(items) text measurement).
///
/// `x_offset` is the leftover width split evenly as outer padding (spec D6), so
/// the block stays centered while inter-cell gaps stay exactly `gap`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UniformGridLayout {
    pub cols: usize,
    pub cell_w: f32,
    pub cell_h: f32,
    /// Horizontal gap between cells. Stored rather than derived: `x_offset`
    /// already absorbed the leftover width, so the gap is NOT recoverable from
    /// the other fields.
    pub gap: f32,
    /// Vertical distance between the tops of consecutive rows: the cell box plus
    /// the label band, its pad, and the inter-row gap.
    pub row_stride: f32,
    pub item_count: usize,
    /// Total content height, for the scroll area's extent.
    pub total_height: f32,
    /// Horizontal inset that centers the column block in the available width.
    pub x_offset: f32,
}

impl Default for UniformGridLayout {
    fn default() -> Self {
        Self {
            cols: 1,
            cell_w: 1.0,
            cell_h: 1.0,
            gap: 0.0,
            row_stride: 1.0,
            item_count: 0,
            total_height: 0.0,
            x_offset: 0.0,
        }
    }
}

/// Build the uniform layout for `item_count` cells of width `cell_w` in
/// `avail_w` of horizontal space. `label_pad`/`label_h` reserve the meta band
/// under every cell. Never yields zero columns (a panel narrower than one cell
/// still lays out a single column, which then simply overflows rather than
/// showing nothing).
pub fn uniform_layout(
    item_count: usize,
    avail_w: f32,
    cell_w: f32,
    gap: f32,
    label_pad: f32,
    label_h: f32,
) -> UniformGridLayout {
    let cell_w = cell_w.max(1.0);
    let cell_h = (cell_w / CELL_ASPECT).max(1.0);
    let gap = gap.max(0.0);
    let avail_w = avail_w.max(0.0);
    let stride_x = cell_w + gap;
    // `n` cells occupy `n*cell_w + (n-1)*gap` == `n*stride_x - gap`, so the
    // largest fitting `n` is `floor((avail_w + gap) / stride_x)`.
    let cols = (((avail_w + gap) / stride_x).floor() as usize).max(1);
    let row_stride = cell_h + label_pad + label_h + gap;
    let rows = item_count.div_ceil(cols);
    let total_height = rows as f32 * row_stride;
    let block_w = cols as f32 * cell_w + cols.saturating_sub(1) as f32 * gap;
    let x_offset = ((avail_w - block_w) * 0.5).max(0.0);
    UniformGridLayout {
        cols,
        cell_w,
        cell_h,
        gap,
        row_stride,
        item_count,
        total_height,
        x_offset,
    }
}

impl UniformGridLayout {
    /// Number of rows the current item count occupies.
    pub fn row_count(&self) -> usize {
        self.item_count.div_ceil(self.cols.max(1))
    }

    /// Top-left offset of cell `index`, relative to the content origin (i.e.
    /// before the caller adds its own margin). Includes `x_offset`.
    pub fn cell_offset(&self, index: usize) -> (f32, f32) {
        let cols = self.cols.max(1);
        let col = index % cols;
        let row = index / cols;
        (
            self.x_offset + col as f32 * (self.cell_w + self.gap),
            row as f32 * self.row_stride,
        )
    }

    /// Inclusive-exclusive range of row indices intersecting the viewport,
    /// padded by one row above/below so cells do not pop in at the edges.
    /// Mirrors the justified layout's contract exactly.
    pub fn visible_rows(&self, scroll_top: f32, viewport_h: f32) -> Range<usize> {
        let rows = self.row_count();
        if rows == 0 || self.row_stride <= 0.0 {
            return 0..0;
        }
        let first = (scroll_top.max(0.0) / self.row_stride).floor() as usize;
        let last =
            ((scroll_top.max(0.0) + viewport_h.max(0.0)) / self.row_stride).floor() as usize;
        let start = first.saturating_sub(1).min(rows);
        let end = (last + 2).min(rows);
        start..end.max(start)
    }

    /// The item indices covered by `rows`, clamped to the item count. This is
    /// what the render pass iterates, so its width is bounded by the viewport -
    /// never by the item count (CLAUDE.md section 1).
    pub fn indices_for_rows(&self, rows: Range<usize>) -> Range<usize> {
        let cols = self.cols.max(1);
        let start = rows.start.saturating_mul(cols).min(self.item_count);
        let end = rows.end.saturating_mul(cols).min(self.item_count);
        start..end.max(start)
    }
}

/// Cache key: the uniform layout is rebuilt only when the image set, available
/// width, or cell width changes. Kept even though `uniform_layout` is O(1), so
/// the invalidation contract matches the justified layout it replaces (and so
/// `item_count` still guards against a mutation that forgets `images_rev`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniformLayoutSig {
    pub images_rev: u64,
    pub item_count: usize,
    pub avail_w: u32,
    pub cell_w: u32,
}

/// A computed uniform layout tagged with the inputs it was built from.
#[derive(Debug, Clone)]
pub struct CachedUniformLayout {
    pub sig: UniformLayoutSig,
    pub layout: UniformGridLayout,
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-app --lib grid_layout 2>&1 | tail -30`

Expected: PASS — all `uniform_*` tests plus the pre-existing `fit_size_*` and justified-layout tests (the justified solver is untouched in this task).

- [ ] **Step 5: Run the scoped gate**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app
```

Expected: all clean. If clippy flags `uniform_layout`'s six parameters, do NOT add `#[allow]` — it is under the 7-argument threshold.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/library/grid_layout.rs
git commit -m "feat(app): pure uniform-grid geometry for the Library grid

Closed-form cell/row arithmetic plus viewport windowing, alongside the existing
justified solver (nothing consumes it yet). O(1) to build, with a virtualization
guard test asserting a 600px viewport realizes under 200 of 100000 cells.

Spec: docs/superpowers/specs/2026-08-31-library-grid-uniform-cells-design.md"
```

---

### Task 2: Stop filenames driving cell width — elide labels, letterbox images

The behavioural fix. Today `label_width` measures every filename and feeds it in as a per-cell minimum width; when a row's floors exceed the panel, `solve_row_height` finds no valid height and returns its `0.4 * target_h` clamp, so the row collapses **and** overflows the right edge (which is what clips the names). Passing zero floors removes the collapse; eliding the label to the cell width removes the clipping; letterboxing keeps the image undistorted now that the cell is no longer widened to the text.

Still the justified layout after this task — row heights vary within the solver's real range. Task 3 makes them uniform. The app must be fully correct at this boundary.

**Files:**
- Modify: `ferrolite-app/src/library/grid.rs`
  - delete `MAX_LABEL_W` (line 26) and `label_width` (lines 136–154)
  - `show`: replace the `min_widths` computation (line ~46) with zeros
  - `paint_cell`: letterbox `img_rect` (line 339), anchor the selection border to it (line ~527)
  - replace `paint_meta` (lines ~205–225)
  - the render pass's `paint_meta` call site (line ~120)
- Test: `ferrolite-app/src/library/grid.rs` (its existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `grid_layout::fit_size(cell_w, cell_h, aspect) -> (f32, f32)`; `cell_aspect(rec: &ImageRecord) -> f32` (already in this file, `pub(crate)`, unchanged).
- Produces: `paint_meta(ui: &mut egui::Ui, rec: &ImageRecord, rect: egui::Rect)` — note `&mut egui::Ui` (was `&egui::Ui`), because eliding uses `ui.put`. `paint_cell`'s signature is unchanged, but its `rect` parameter now means the **cell box** and the image is letterboxed inside it.

- [ ] **Step 1: Write the failing tests**

Append inside `ferrolite-app/src/library/grid.rs`'s existing `#[cfg(test)] mod tests { ... }` block:

```rust
    /// Regression for the collapsed-row / clipped-filename defect: the grid must
    /// pass NO per-cell minimum widths into the layout. A filename-derived floor
    /// made `solve_row_height` unsolvable, so it returned its `0.4 * target_h`
    /// clamp — rows 40px tall that overflowed the panel by up to 211px, which is
    /// what cut the names off. Labels elide to the cell instead (`paint_meta`).
    ///
    /// Asserts on the real `grid_layout::layout` with the aspects of a mixed set:
    /// with zero floors every row must fill the width and none may collapse to
    /// the solver's lower clamp.
    #[test]
    fn zero_label_floors_stop_rows_collapsing_and_overflowing() {
        // Upright aspects spanning portrait -> panorama, as the RAW fixtures do.
        let aspects: [f32; 15] = [
            1.506, 0.661, 1.512, 1.512, 1.506, 1.527, 1.510, 1.462, 1.495, 0.665,
            0.666, 0.714, 0.666, 0.665, 1.345,
        ];
        let zeros = vec![0.0_f32; aspects.len()];
        let avail_w = 900.0_f32;
        let target_h = 150.0_f32;
        let l = crate::library::grid_layout::layout(
            &aspects, &zeros, avail_w, target_h, GAP, LABEL_H,
        );
        assert!(!l.rows.is_empty());
        for (ri, row) in l.rows.iter().enumerate() {
            let right = row.items.last().map(|it| it.x + it.width).unwrap_or(0.0);
            assert!(
                right <= avail_w + 1.0,
                "row {ri} right edge {right} overflows avail {avail_w} — this is \
                 what clipped the filenames"
            );
            assert!(
                row.img_height > target_h * 0.45,
                "row {ri} height {} collapsed to the solver's lower clamp",
                row.img_height
            );
        }
    }

    /// The image must be letterboxed inside its cell, never stretched to it.
    /// `paint_cell` cannot be unit-tested (it needs an egui `Ui`), so this pins
    /// the composition it performs: `fit_size` of `cell_aspect`.
    #[test]
    fn cell_image_is_letterboxed_to_its_own_aspect() {
        use crate::library::grid_layout::fit_size;
        // Portrait thumbnail (2:3) in a landscape 3:2 cell.
        let r = rec(Some(4000), Some(6000), Orientation::Normal, Some(200), Some(300));
        let a = cell_aspect(&r);
        let (w, h) = fit_size(150.0, 100.0, a);
        assert!(w < 150.0, "a portrait must not fill a landscape cell's width");
        assert!((h - 100.0).abs() < 0.01, "it should fill the height");
        assert!(
            (w / h - a).abs() < 1e-3,
            "the fitted rect must keep the image's aspect, else it is stretched"
        );
    }

    /// Guard against the O(all-items) text measurement returning. The deleted
    /// `label_width` called egui's no-wrap text layout on EVERY image on each
    /// layout rebuild (any panel resize or Size-slider change) — the class of
    /// work CLAUDE.md's virtualization rule forbids. Eliding to a known cell
    /// width replaces it.
    ///
    /// Each needle is ASSEMBLED AT RUNTIME from two fragments so it cannot match
    /// this test's own source (`include_str!` pulls in the test module too, so a
    /// plain literal would make the test red even on correct code). Same
    /// convention as `settings::dto`'s `disclosure_snapshot_covers_every_open_field`,
    /// which crafts its needle so it cannot self-match. `\r` is stripped so a
    /// CRLF checkout does not change the result.
    #[test]
    fn the_grid_never_measures_filenames_to_size_cells() {
        let src = include_str!("grid.rs").replace('\r', "");
        let measure_call = ["layout_no", "_wrap"].concat();
        assert!(
            !src.contains(&measure_call),
            "grid.rs measures text again — sizing cells from filename widths \
             reintroduces the O(all-items) work and the row collapse"
        );
        let label_cap = ["MAX_LABEL", "_W"].concat();
        assert!(
            !src.contains(&label_cap),
            "the label-width floor's cap is back; a filename-derived minimum \
             cell width is what made the row solver unsolvable"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-app --lib library::grid:: 2>&1 | tail -30`

Expected: FAIL — `the_grid_never_measures_filenames_to_size_cells` fails on both assertions (`layout_no_wrap` and `MAX_LABEL_W` are still present). The other two new tests may already pass; that is fine, they lock in behaviour this task must not break.

- [ ] **Step 3: Delete the label-width floor**

In `ferrolite-app/src/library/grid.rs`:

(a) Delete the `MAX_LABEL_W` const and its doc comment (currently lines 24–26):

```rust
/// Upper bound on how wide a filename label may push a cell, so one very long
/// name can't blow out a whole row.
const MAX_LABEL_W: f32 = 240.0;
```

(b) Delete the whole `label_width` function and its doc comment (currently lines 132–154), i.e. from `/// Measured pixel width of a cell's meta label` through the closing `}` of:

```rust
    (name.max(date) + 6.0).min(MAX_LABEL_W)
}
```

(c) In `show`, replace the `min_widths` block:

```rust
        // Per-cell minimum width = its label width, so portrait filenames aren't
        // clipped (the cell widens to the text and the image is centered in it).
        let min_widths: Vec<f32> = state.images.iter().map(|r| label_width(ui, r)).collect();
```

with:

```rust
        // NO per-cell minimum widths. A filename-derived floor made the row-height
        // solver unsolvable whenever a row's floors exceeded the panel: it
        // returned its `0.4 * target_h` clamp, so the row collapsed to a strip AND
        // overflowed the right edge, which is what clipped the names. Labels elide
        // to the cell width instead (`paint_meta`), which also removes the
        // O(all-items) `layout_no_wrap` measurement this used to do on every
        // rebuild (CLAUDE.md §1). Kept as a zeroed argument rather than dropped
        // from `layout`'s signature so this task does not also churn the pure
        // module; Task 3 removes the parameter with the solver.
        let min_widths: Vec<f32> = vec![0.0; state.images.len()];
```

- [ ] **Step 4: Letterbox the image inside the cell**

In `paint_cell`, replace:

```rust
    let has_tex = state.textures.contains(rec.id);
    let painter = ui.painter_at(rect);

    // Thumbnail fills the full cell in both states; the gradient border is
    // drawn on top at the end of the function.
    let img_rect = rect;
```

with:

```rust
    let has_tex = state.textures.contains(rec.id);
    let painter = ui.painter_at(rect);

    // The image is letterboxed inside the cell box, not stretched to it.
    // `egui::Image::paint_at` maps the whole texture onto whatever rect it is
    // given with no aspect handling, and `Image::fit_to_exact_size` does NOT
    // change that (`fit` is only read by `Image::ui`/`calc_size`, so on this
    // path it is a silent no-op) — so the fit has to be computed here. The
    // aspect comes from the RECORD via the shared `cell_aspect`, not from the
    // texture, so the cell does not reflow when the thumbnail finishes loading.
    // Every overlay below (rating, flag, queue badge, tag dots, selection ring)
    // anchors to `img_rect`, so they hug the thumbnail rather than floating over
    // a letterbox bar (spec D5).
    let (img_w, img_h) =
        crate::library::grid_layout::fit_size(rect.width(), rect.height(), cell_aspect(rec));
    let img_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(img_w, img_h));
```

Then in the same function, change the selection border to anchor on the image rect. Replace:

```rust
    if selected {
        let path = rect.shrink(2.0);
```

with:

```rust
    if selected {
        let path = img_rect.shrink(2.0);
```

Leave the `ui.interact(rect, ...)` hit area on the full **cell** box — a uniform, gap-free click/drag target, so a portrait's narrow image does not shrink its target. Add a note above it:

```rust
    // Hit area is the full CELL box, not the letterboxed image: a uniform,
    // gap-free target means a portrait is no harder to click than a panorama.
    let resp = ui.interact(
```

- [ ] **Step 5: Elide the filename**

Replace the whole `paint_meta` function (doc comment included) with:

```rust
/// Draw the per-cell meta label under the thumbnail: filename on top, capture
/// date below, both centered and both ELIDED to the cell width.
///
/// Eliding (not measuring) is the rule: `Label::truncate()` lays out one galley
/// for one visible cell inside the already-virtualized render pass, whereas the
/// `label_width` floor this replaces measured every filename in the catalog on
/// each layout rebuild. The full name is always on hover, so nothing is lost —
/// attached unconditionally rather than only when truncated, matching the export
/// queue's cell (`export_module::queue_list`); egui exposes no cheap
/// "was truncated" flag on the response.
///
/// `selectable(false)` keeps the label inert so it never steals the drag or
/// click that the cell's own `interact` owns.
fn paint_meta(ui: &mut egui::Ui, rec: &ImageRecord, rect: egui::Rect) {
    let name_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 14.0));
    ui.put(
        name_rect,
        egui::Label::new(
            egui::RichText::new(&rec.filename)
                .size(11.0)
                .color(theme::TEXT_PRIMARY),
        )
        .truncate()
        .selectable(false),
    )
    .on_hover_text(&rec.filename);

    if let Some(date) = format_capture_date(rec.capture_time.as_deref()) {
        let date_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.top() + 14.0),
            egui::vec2(rect.width(), (rect.height() - 14.0).max(1.0)),
        );
        ui.put(
            date_rect,
            egui::Label::new(
                egui::RichText::new(date)
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            )
            .truncate()
            .selectable(false),
        );
    }
}
```

The call site in `show` currently reads `paint_meta(ui, &rec, label_rect);` and already has `ui: &mut egui::Ui` in scope, so it compiles unchanged. If the borrow checker objects because `state` is borrowed across it, move the `paint_meta` call so it does not overlap a live `state` borrow — `rec` is already a clone (`let rec = state.images[item.index].clone();`), so this should not arise.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-app --lib library::grid 2>&1 | tail -30`

Expected: PASS, including `the_grid_never_measures_filenames_to_size_cells`.

- [ ] **Step 7: Run the scoped gate**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app
```

Expected: all clean. If clippy reports `label_width` or `MAX_LABEL_W` as unused, they were not fully deleted.

- [ ] **Step 8: Commit**

```bash
git add ferrolite-app/src/library/grid.rs
git commit -m "fix(app): Library grid elides filenames instead of widening cells to them

label_width fed each filename's measured width in as a per-cell minimum. When a
row's floors exceeded the panel no row height could satisfy the justification
constraint, so solve_row_height returned its 0.4*target_h clamp: 40px rows that
also overflowed the right edge, which is what clipped the names.

Labels now elide to the cell width with the full name on hover, the image is
letterboxed inside its cell (paint_at ignores Image::fit, so the fit is computed
via grid_layout::fit_size), and the selection ring anchors to the image rect.
Removes the O(all-items) layout_no_wrap measurement done on every layout
rebuild, with a source-grep guard against its return.

Spec: docs/superpowers/specs/2026-08-31-library-grid-uniform-cells-design.md"
```

---

### Task 3: Swap the layout model to the uniform grid

Replaces the justified solver with `UniformGridLayout` and deletes the solver. Task 2 already moved the label and letterbox logic, so this task changes only how cell rects are produced and iterated — the virtualization shape, the visible-id bookkeeping, and `paint_cell` stay as they are.

**Files:**
- Modify: `ferrolite-app/src/library/grid.rs` (imports, `show`'s layout build + render pass)
- Modify: `ferrolite-app/src/library/grid_layout.rs` (delete `layout`, `solve_row_height`, `row_width`, `Row`, `RowItem`, `GridLayout`, `LayoutSig`, `CachedGridLayout`, and their tests)
- Modify: `ferrolite-app/src/state.rs:295` and its two initializers (lines ~447, ~1128)

**Interfaces:**
- Consumes: `uniform_layout`, `UniformGridLayout`, `UniformLayoutSig`, `CachedUniformLayout`, `CELL_ASPECT` (Task 1); `paint_meta`, `paint_cell`, `cell_aspect` (Task 2, unchanged).
- Produces: `AppState::grid_layout: Option<crate::library::grid_layout::CachedUniformLayout>`. `grid::show`'s signature is unchanged: `pub fn show(ui: &mut egui::Ui, state: &mut AppState, cell: f32) -> Option<i64>`, where `cell` is now the cell **width** (it was the target row height).

- [ ] **Step 1: Write the failing test**

Append inside `ferrolite-app/src/library/grid.rs`'s test module:

```rust
    /// The justified solver must be GONE, not merely bypassed. Task 2's zeroed
    /// `min_widths` argument removed the symptom; leaving `solve_row_height` in
    /// the tree leaves the 0.4x clamp one call site away from returning.
    ///
    /// Needles are assembled at runtime so they cannot match this test's own
    /// source. They grep a different file (`grid_layout.rs`) today, so a plain
    /// literal would work — but assembling them means the test keeps meaning the
    /// same thing if it is ever moved into that file. Same convention as
    /// `settings::dto`'s `disclosure_snapshot_covers_every_open_field`.
    ///
    /// There is deliberately NO positive "grid.rs calls uniform_layout"
    /// assertion: greping this file for that name would match this test's own
    /// text and pass vacuously, and the compiler is the stronger check anyway —
    /// once the solver is deleted, `grid.rs` cannot build a layout without it.
    #[test]
    fn the_justified_row_solver_is_gone() {
        let layout_src = include_str!("grid_layout.rs").replace('\r', "");
        let gone = [
            ["solve_row", "_height"].concat(),
            ["fn row", "_width"].concat(),
            ["struct Row", "Item"].concat(),
        ];
        for needle in &gone {
            assert!(
                !layout_src.contains(needle),
                "grid_layout.rs still defines `{needle}` — the uniform grid \
                 replaced the justified solver, so it must not linger"
            );
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p ferrolite-app --lib the_justified_row_solver_is_gone 2>&1 | tail -20`

Expected: FAIL — `grid_layout.rs still defines \`solve_row_height\``.

- [ ] **Step 3: Rewrite `show`'s layout build and render pass**

In `ferrolite-app/src/library/grid.rs`, change the import (line 7):

```rust
use crate::library::grid_layout::{layout, CachedGridLayout, LayoutSig};
```

to:

```rust
use crate::library::grid_layout::{uniform_layout, CachedUniformLayout, UniformLayoutSig};
```

Replace the body of `show` from its first line through the end of the `scroll.show_viewport(...)` closure — i.e. everything from `let avail_w = ...` down to and including the closing `});` of `show_viewport` — with:

```rust
    let avail_w = (ui.available_width() - 2.0 * MARGIN).max(1.0);
    // `cell` is the cell WIDTH (spec D4's slider mapping); height follows from
    // `CELL_ASPECT` inside `uniform_layout`.
    let cell_w = cell.max(1.0);

    // Rebuild only when the image set, width, or cell size changed. Taken out of
    // `state` for the render pass so `paint_cell` can borrow `state` mutably
    // without aliasing; restored at the end.
    let sig = UniformLayoutSig {
        images_rev: state.images_rev,
        item_count: state.images.len(),
        avail_w: avail_w.round() as u32,
        cell_w: cell_w.round() as u32,
    };
    let mut cache = state.grid_layout.take();
    if cache.as_ref().map(|c| c.sig) != Some(sig) {
        cache = Some(CachedUniformLayout {
            sig,
            layout: uniform_layout(
                state.images.len(),
                avail_w,
                cell_w,
                GAP,
                LABEL_PAD,
                LABEL_H,
            ),
        });
    }
    let cache = cache.expect("layout built above");

    // Built once per frame (not per cell) so membership checks stay O(1) instead
    // of O(queue length) per cell.
    let queued: HashSet<i64> = state.export_queue.iter().copied().collect();
    // Computed once per frame, not per cell — cells with no texture render a
    // "generating" affordance instead of a flat placeholder while an ingest is
    // active (see `cell_state::CellState::Generating`).
    let is_ingesting = state.active_ingests > 0;

    let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
    let mut opened: Option<i64> = None;
    scroll.show_viewport(ui, |ui, viewport| {
        ui.set_height(cache.layout.total_height + 2.0 * MARGIN);
        // Content is offset down/right by MARGIN, so map the viewport into the
        // layout's own (0-based) coordinate space before picking visible rows.
        let scroll_top = (viewport.min.y - MARGIN).max(0.0);
        let vh = viewport.height() + MARGIN;
        let rows = cache.layout.visible_rows(scroll_top, vh);
        // Bounded by the viewport, never by the item count (CLAUDE.md §1) — see
        // `uniform_indices_for_rows_is_bounded_by_the_viewport_not_the_item_count`.
        let indices = cache.layout.indices_for_rows(rows);

        // Compute the visible id set (used to fetch tag associations for the
        // window). Ingest thumbnails are now generated inline within the ingest
        // job — there are no separate per-image thumbnail jobs to reprioritize by
        // visibility, so the old promote/demote pass is gone.
        let mut now_visible: HashSet<i64> = HashSet::new();
        for i in indices.clone() {
            now_visible.insert(state.images[i].id);
        }
        // Fetch tag associations for the visible window (only missing ids queried).
        state.ensure_tags_for(&now_visible);
        // Fetch collection membership for the same visible window, so the
        // "Add/Remove to collection" submenus can decide addable/removable
        // without a synchronous DB call.
        state.ensure_collections_for(&now_visible);
        // Cancel any lazy-load thumbnail fetches for cells scrolled out of view
        // this frame, so a big scroll doesn't leave a stale backlog blocking the
        // now-visible cells (Round 4 fix).
        state.retain_visible_thumbnail_jobs(&now_visible);

        let origin = ui.min_rect().left_top() + egui::vec2(MARGIN, MARGIN);
        for i in indices {
            let rec = state.images[i].clone();
            let (cx, cy) = cache.layout.cell_offset(i);
            let cell_rect = egui::Rect::from_min_size(
                origin + egui::vec2(cx, cy),
                egui::vec2(cache.layout.cell_w, cache.layout.cell_h),
            );
            if let Some(id) = paint_cell(
                ui,
                state,
                &rec,
                cell_rect,
                queued.contains(&rec.id),
                is_ingesting,
            ) {
                opened = Some(id);
            }
            let label_rect = egui::Rect::from_min_size(
                egui::pos2(cell_rect.left(), cell_rect.bottom() + LABEL_PAD),
                egui::vec2(cell_rect.width(), LABEL_H - LABEL_PAD),
            );
            paint_meta(ui, &rec, label_rect);
        }
    });
```

Everything after that closure (`state.grid_layout = Some(cache);`, the drag-chip block, `opened`) stays as it is.

- [ ] **Step 4: Update the `AppState` field type**

In `ferrolite-app/src/state.rs` line ~295, change:

```rust
    pub grid_layout: Option<crate::library::grid_layout::CachedGridLayout>,
```

to:

```rust
    pub grid_layout: Option<crate::library::grid_layout::CachedUniformLayout>,
```

The two initializers (lines ~447 and ~1128) are `grid_layout: None` and need no change.

- [ ] **Step 5: Delete the justified solver**

In `ferrolite-app/src/library/grid_layout.rs` delete, along with their doc comments:

- `pub struct RowItem`
- `pub struct Row`
- `pub struct GridLayout`
- `fn row_width`
- `pub fn layout`
- `fn solve_row_height`
- `impl GridLayout { pub fn visible_rows(...) }`
- `pub struct LayoutSig`
- `pub struct CachedGridLayout`

and from its test module delete the tests that exercise them: `lay` (helper), `empty_input_yields_no_rows`, `full_rows_fill_available_width`, `item_widths_follow_aspect_ratio`, `trailing_row_keeps_target_height_not_ballooned`, `min_width_widens_cell_and_centers_image`, `total_height_accounts_for_labels_and_gaps`, `visible_rows_windows_around_scroll`, `visible_rows_empty_when_no_rows`.

Keep `ASPECT_MIN`/`ASPECT_MAX` and `fit_size` (still used by `export_module::queue_list`), all `fit_size_*` tests, and everything Task 1 added. `ASPECT_MIN`/`ASPECT_MAX` lose their `row_width` caller and are then used only by `fit_size` — that is fine, leave them where they are.

Update the module's own doc comment (lines 1–4) from the justified-rows description to:

```rust
//! Pure Library-grid geometry: a uniform (fixed-cell) grid whose closed-form
//! arithmetic places every cell in the same `cell_w x cell_h` box, plus the
//! letterbox fit (`fit_size`) that keeps a thumbnail's own aspect inside such a
//! box. No egui — unit-testable.
//!
//! Replaced a justified-rows solver (Flickr/Google-Photos style) that varied row
//! height to fill the width exactly. That model could not coexist with a
//! text-derived minimum cell width: when a row's filename floors exceeded the
//! panel no height satisfied the constraint, so the solver bottomed out on its
//! lower clamp and the row both collapsed and overflowed. See
//! `docs/superpowers/specs/2026-08-31-library-grid-uniform-cells-design.md`.
```

Also delete the now-unused `use std::ops::Range;` **only if** nothing left in the file needs it — `visible_rows`/`indices_for_rows` from Task 1 both return `Range<usize>`, so it MUST stay.

- [ ] **Step 6: Remove the Task-2 guard's stale expectation**

The `zero_label_floors_stop_rows_collapsing_and_overflowing` test in `grid.rs` calls `grid_layout::layout`, which no longer exists. Delete that test — its intent is now enforced structurally by `the_justified_row_solver_is_gone` plus Task 1's `uniform_cells_tile_without_overlap_and_stay_within_width`. Keep `cell_image_is_letterboxed_to_its_own_aspect` and `the_grid_never_measures_filenames_to_size_cells`.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-app --lib library:: 2>&1 | tail -30`

Expected: PASS, including `the_justified_row_solver_is_gone`.

- [ ] **Step 8: Run the scoped gate**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app
```

Expected: all clean. Any "unused import" or "never used" warning means a deletion in Step 5 was incomplete.

- [ ] **Step 9: Commit**

```bash
git add ferrolite-app/src/library/grid.rs ferrolite-app/src/library/grid_layout.rs ferrolite-app/src/state.rs
git commit -m "refactor(app): Library grid uses uniform cells, deleting the row solver

Every cell is now the same cell_w x cell_h box (height from CELL_ASPECT), so
cell size is consistent by construction and a row's height can no longer depend
on any image's aspect. Deletes layout/solve_row_height/row_width/Row/RowItem and
their tests; the render pass iterates indices_for_rows, keeping realization
bounded by the viewport rather than the item count.

Spec: docs/superpowers/specs/2026-08-31-library-grid-uniform-cells-design.md"
```

---

### Task 4: Adopt the documented Size-slider range (spec D4)

`grid::show`'s `cell` argument is currently `self.thumb_size + 60.0`, i.e. 60–160 px for a 0–100 slider. `docs/design/V2/README.md:42` specifies `118 + sizePct * 1.7` (118–288 px, default ~196 at the slider's default of 46). The 60 px floor is a large part of why labels dominated cells.

**Files:**
- Modify: `ferrolite-app/src/library/grid.rs` (add `cell_width_for_size`)
- Modify: `ferrolite-app/src/app.rs:2612` (call site)
- Test: `ferrolite-app/src/library/grid.rs`

**Interfaces:**
- Produces: `pub fn cell_width_for_size(size_pct: f32) -> f32` in `crate::library::grid`.

- [ ] **Step 1: Write the failing test**

Append inside `ferrolite-app/src/library/grid.rs`'s test module:

```rust
    /// Spec D4: the Size slider maps to cell WIDTH by the documented V2 formula
    /// `118 + sizePct * 1.7` (docs/design/V2/README.md:42), replacing the old
    /// `thumb_size + 60`. Pinned because the old 60px floor is what let a
    /// filename dominate a cell.
    #[test]
    fn cell_width_follows_the_documented_v2_size_range() {
        assert!((cell_width_for_size(0.0) - 118.0).abs() < 0.01, "slider min");
        assert!((cell_width_for_size(100.0) - 288.0).abs() < 0.01, "slider max");
        // The persisted default (settings::dto grid_size = 46.0).
        assert!((cell_width_for_size(46.0) - 196.2).abs() < 0.01, "default");
    }

    #[test]
    fn cell_width_is_monotonic_and_clamps_out_of_range_input() {
        let mut prev = 0.0_f32;
        for pct in [0.0_f32, 10.0, 46.0, 90.0, 100.0] {
            let w = cell_width_for_size(pct);
            assert!(w > prev, "must grow with the slider");
            prev = w;
        }
        // A persisted setting outside 0..=100 must not produce a degenerate cell.
        assert!((cell_width_for_size(-50.0) - 118.0).abs() < 0.01);
        assert!((cell_width_for_size(999.0) - 288.0).abs() < 0.01);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p ferrolite-app --lib cell_width 2>&1 | tail -20`

Expected: FAIL — `cannot find function 'cell_width_for_size' in this scope`.

- [ ] **Step 3: Write the implementation**

In `ferrolite-app/src/library/grid.rs`, add next to the other constants near the top of the file:

```rust
/// Size-slider → cell-width mapping from `docs/design/V2/README.md:42`
/// (`118 + sizePct * 1.7`), spec decision D4. The slider is a 0..=100 percentage
/// (`settings.grid_size`, default 46), so this yields 118..288 px, default ~196.
///
/// Replaced `thumb_size + 60` (60..160). The old 60 px floor made a cell narrower
/// than a typical filename at small sizes, which is how the label-width floor
/// came to dominate the layout in the first place.
const CELL_W_BASE: f32 = 118.0;
const CELL_W_PER_PCT: f32 = 1.7;

/// Cell width for a `0..=100` Size-slider percentage. Clamps out-of-range input
/// so a hand-edited or future-versioned settings file cannot produce a
/// degenerate cell.
pub fn cell_width_for_size(size_pct: f32) -> f32 {
    CELL_W_BASE + size_pct.clamp(0.0, 100.0) * CELL_W_PER_PCT
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p ferrolite-app --lib cell_width 2>&1 | tail -20`

Expected: PASS (2 tests).

- [ ] **Step 5: Wire it into the call site**

In `ferrolite-app/src/app.rs` line ~2612, replace:

```rust
                        crate::library::grid::show(ui, &mut self.state, self.thumb_size + 60.0);
```

with:

```rust
                        crate::library::grid::show(
                            ui,
                            &mut self.state,
                            crate::library::grid::cell_width_for_size(self.thumb_size),
                        );
```

Note the surrounding expression may consume `show`'s return value (the opened image id) — preserve whatever the existing line does with it. If the original line is a statement whose value is discarded, keep it a statement; if it is assigned or matched, keep that.

- [ ] **Step 6: Run the scoped gate**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app
```

Expected: all clean.

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/library/grid.rs ferrolite-app/src/app.rs
git commit -m "feat(app): Library grid Size slider adopts the documented V2 range

cell_width_for_size maps the 0..=100 Size percentage to 118..288px per
docs/design/V2/README.md:42 (118 + sizePct*1.7), default ~196 at the persisted
default of 46 — replacing thumb_size + 60 (60..160). The old 60px floor made a
cell narrower than a typical filename at small sizes.

Spec: docs/superpowers/specs/2026-08-31-library-grid-uniform-cells-design.md"
```

---

## After all tasks: coordinator responsibilities

These are NOT subagent tasks. The coordinator does them.

1. **Repo gate** on the latest stable, once, at the end:
   ```bash
   rustup update stable
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo build --all-targets
   cargo test --workspace
   ```
   Run each without piping into `tail`/`grep`, or use `set -o pipefail` — a pipe makes the reported exit code the pipe's, not cargo's.

2. **Hand the author the visual test plan** from the spec's §6 (ten numbered items) and **hold** for hands-on results before finishing the branch. Green automated checks are necessary but not sufficient for egui UI (CLAUDE.md).

3. **Known pre-existing flake to watch, not caused by this work:** `ferrolite-pipeline::nr_node::tests::second_evaluate_on_same_input_matches_the_first` failed once under full-workspace GPU load on 2026-08-31 and could not be reproduced in 11 subsequent runs, including on a clean tree. If it appears, it is not from this plan.

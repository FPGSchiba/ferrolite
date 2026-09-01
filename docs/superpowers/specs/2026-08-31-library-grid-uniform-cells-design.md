# Library grid — uniform cells, letterboxed thumbnails, elided filenames

**Date:** 2026-08-31
**Status:** approved 2026-08-31 (author: "the spec looks good")
**Area:** `ferrolite-app/src/library/` (`grid.rs`, `grid_layout.rs`)
**Related:** `docs/design/V2/README.md:42` (the canonical Library grid description)

---

## 1. The reported problem

The Library grid uses a justified-rows layout. With mixed aspect ratios the row
heights vary wildly: in a screenshot of the 19 RAW fixtures at Size 100, some
rows are roughly twice the height of others and one collapses to a thin strip.
Long filenames are clipped mid-word — `ultrawide-sony-ilce7m3-iso125-16mm.ARW`
is cut off at the right edge of its cell.

The author asked for three things:

1. Consistent image sizes across the grid.
2. A minimum height that scales with the Size slider, so a wide/panoramic image
   cannot collapse to a sliver.
3. Filenames must not be cut off.

## 2. Root cause

All three symptoms are one defect: **the per-cell filename-width floor fights
the row-height solver, and when it wins the solver silently gives up.**

The chain, in the current code:

1. `grid.rs::label_width` measures each filename and returns it (capped at
   `MAX_LABEL_W = 240`) as that cell's **minimum width**, so a name is never
   clipped.
2. `grid_layout.rs::row_width` computes a cell as
   `(aspect * h).max(min_width[k])`. At small cell sizes the label floor
   dominates: a portrait thumbnail at `h = 100` is ~27 px wide inside a ~210 px
   cell.
3. The greedy packer grows a row until its width **at `target_h`** reaches
   `avail_w`, so it stops after N items — but the row's *floor* width
   (`sum(min_widths) + gaps`) already exceeds `avail_w`.
4. `solve_row_height` then binary-searches a height that makes the row span
   `avail_w`. But `row_width` has a floor **independent of `h`**: no height
   satisfies the constraint, `lo` never advances, and the function returns its
   arbitrary lower clamp, `0.4 * target_h`.
5. That row renders 40 px-tall images in 210 px-wide cells, **overflowing the
   panel** — which is what clips the rightmost filenames.

### Evidence

Running the real `grid_layout::layout` with the 19 fixtures' upright aspects,
their filename widths as `min_widths`, `avail_w = 900`, `target_h = 100`:

```
avail_w=900 target_h=100  rows=4
  row 0: h=  40.0  items=5  right_edge= 1111.4 (overflow +211.4)  label-floored=5/5
  row 1: h=  40.0  items=5  right_edge= 1037.2 (overflow +137.2)  label-floored=5/5
  row 2: h=  40.0  items=5  right_edge=  989.5 (overflow  +89.5)  label-floored=5/5
  row 3: h= 100.0  items=4  right_edge=  728.3 (overflow -171.7)  label-floored=1/4
      idx 16 cell_w= 180.4 img_w= 180.4 label_min=  59.0 name=sample.rw2
      idx 17 cell_w= 150.3 img_w= 150.3 label_min=  69.6 name=DSC04692.ARW
```

Every long-filename row lands on **exactly** `0.4 x target_h` and overflows the
right edge. The only healthy row is the one whose filenames are short
(`sample.rw2`, `DSC04692.ARW`) — which is why the effect looks arbitrary in a
screenshot. 100 vs 40 is the "roughly twice the height" the author saw.

### Why this is a model problem, not just a clamp to widen

Justified rows derive cell width from `aspect x row_height`. A **text-derived
minimum width** breaks the monotonic relationship the solver depends on, and no
choice of clamp band restores it: the floor is simply unreachable when
`sum(min_widths) + gaps > avail_w`. Either labels stop driving cell width, or
the model stops deriving width from height.

There is also a standing cost: `min_widths` measures **every** filename in the
catalog on each layout rebuild (any panel resize or Size-slider change) — O(all
items) text layout, the class of work CLAUDE.md's virtualization rule exists to
prevent. It is cached behind `LayoutSig`, so it is not per-frame, but it is
per-resize.

### The implementation already diverges from the design

`docs/design/V2/README.md:42` describes the Library grid as:

> responsive `auto-fill` grid, cell min-width driven by the Size slider
> (`118 + sizePct*1.7` px). Each cell: **3:2 thumbnail image**, 1px border
> (accent + 2px outline glow when selected), a star-rating glyph overlay
> bottom-left and a color-label dot bottom-right on the thumbnail itself,
> filename below (`IBM Plex Mono`, 10px) + capture date/time below that (9px).

That is a **uniform grid with a fixed cell box**, not justified rows. The
justified-rows layout is undocumented drift.

## 3. Decisions (author-confirmed 2026-08-31)

| # | Decision | Rationale |
|---|---|---|
| D1 | **Uniform grid.** Fixed cell box sized by the Size slider; images fitted inside. | Satisfies all three asks directly and returns to the documented V2 design. Deletes the justification solver and the per-rebuild label measurement. |
| D2 | **Fit / letterbox.** The whole image is visible, centered, background showing on the short axis. | Honest about aspect: a panorama reads as a panorama. Matches Lightroom's grid. |
| D3 | **One-line filename, elided, with a hover tooltip carrying the full name.** | Constant label height for every cell, no text measurement, nothing silently lost. |
| D4 | **Adopt the V2 Size range**: `cell_w = 118 + size_pct * 1.7` (118..288 px, default ~196), replacing today's `thumb_size + 60` (60..160). | The documented design, and today's 60 px floor is a large part of why labels dominated cells. Taken from the spec's Q1 recommendation on approval; it is one constant if the old feel is preferred. |
| D5 | **Selection border/glow on the IMAGE rect**, not the cell box. | The V2 doc reads "3:2 thumbnail image, 1px border … 2px outline glow when selected". Selection should mean "this photo", not "this slot". (Q2) |
| D6 | **Leftover width becomes extra outer padding**, gaps stay exactly `GAP`, block stays centered. | A constant gap is more legible than a width-dependent one. (Q3) |
| D7 | **Cell aspect fixed at 3:2** (`CELL_ASPECT = 1.5`). | Per the V2 doc; suits a mostly-landscape catalog. (Q4) |

Rejected: justified rows + height clamp (only partly delivers "consistent
sizes", keeps the solver and its edge cases); fixed-height rows (consistent
height but not size, ragged right edge, panoramas still produce very wide
cells).

## 4. Design

### 4.1 Geometry

A uniform grid needs no solver — the arithmetic is closed-form:

```
cell_w        = the Size-slider cell size
cell_h        = cell_w / CELL_ASPECT          (CELL_ASPECT = 3/2 per V2 doc)
stride_x      = cell_w + GAP
cols          = max(1, floor((avail_w + GAP) / stride_x))
row_stride    = cell_h + LABEL_PAD + LABEL_H + GAP
rows          = ceil(item_count / cols)
total_height  = rows * row_stride
```

`cols` is derived from the panel width, so the grid stays responsive; leftover
width is distributed as extra outer padding (or as a slightly larger gap — see
open question Q3) so the block stays centered rather than left-hugging.

This makes asks 1 and 2 structural rather than enforced:

- **Consistent image sizes**: every cell is the same box, by construction.
- **Minimum height that scales with the slider**: `cell_h = cell_w /
  CELL_ASPECT` *is* the height, and it is a pure function of the slider. There
  is no solver that can undershoot it, so no clamp is needed and a panoramic
  image cannot collapse — it letterboxes inside a full-height cell.

### 4.2 Thumbnail placement (D2)

**Already implemented** as `grid_layout::fit_size(cell_w, cell_h, aspect)`,
added while fixing the export-queue distortion (§4.6) — this spec's grid reuses
it rather than repeating the math:

```
img_w    = min(cell_w, a * cell_h)
img_h    = min(cell_h, cell_w / a)
img_rect = centered within the cell box
```

`a` is `cell_aspect(rec)`, unchanged — it already prefers the persisted
thumbnail's upright dims, so a cropped image shows its cropped aspect.

The selection border/glow is drawn on the **image rect**, not the cell box, so
selection reads as "this photo" rather than "this slot". (Open question Q2.)

> **Gotcha this must respect.** `egui::Image::paint_at` maps the whole texture
> onto the rect it is given with no aspect handling, and
> `Image::fit_to_exact_size` does **not** change that — `fit` is only read by
> `Image::ui`/`calc_size`, so on the `paint_at` path it is a silent no-op. All
> three of the app's thumbnail painters call `fit_to_exact_size(...).paint_at(...)`;
> the Library grid and filmstrip get away with it only because their rect is
> already aspect-sized. **A uniform grid removes that accident**, so the fitted
> rect must be computed explicitly (`fit_size`) before painting, or the uniform
> grid will stretch every non-3:2 thumbnail exactly as the export queue did.

### 4.6 Sibling already fixed: the export-queue grid

`export_module/queue_list.rs` had the same class of bug in its most direct form:
a hardcoded 132x88 (3:2) cell handed straight to `paint_at`, stretching every
non-3:2 queued image (a 2:3 portrait was squashed 2.25x horizontally). Fixed
separately by letterboxing with `fit_size` inside the unchanged cell box — its
uniform footprint is load-bearing for `horizontal_wrapped` row alignment, so the
aspect had to be absorbed by the painted rect rather than the allocation.

That fix is the same shape as D1+D2 here: **fixed cell box, letterboxed image.**
The Library grid should end up visually consistent with it.

### 4.3 Filename label (D3)

`paint_meta` keeps its two lines (filename 11px, capture date 10px) and its
fixed `LABEL_H`, but the filename is **elided to the cell width** rather than
the cell being widened to the filename:

- Lay out the name with `egui`'s truncation (a `TextFormat`/`LayoutJob` with
  `wrap.max_width = cell_w - 2*TEXT_INSET` and `wrap.max_rows = 1`,
  `break_anywhere`), which appends an ellipsis. This is a **single galley for
  one visible cell**, inside the already-virtualized render pass — not a
  measurement of every item.
- When the galley reports it was truncated, attach a hover tooltip with the
  full filename on the cell's existing interact response.
- `label_width`, `MAX_LABEL_W`, and the whole `min_widths` input disappear.

### 4.4 Code shape

- `grid_layout.rs`: replace `layout()` / `solve_row_height()` / `row_width()`
  with a `UniformGridLayout { cols, cell_w, cell_h, row_stride, item_count }`
  plus:
  - `fn cell_rect(&self, index: usize) -> Rect` (relative to content origin),
  - `fn visible_rows(&self, scroll_top, viewport_h) -> Range<usize>` — kept, now
    trivial division instead of `partition_point`,
  - `fn indices_for_rows(&self, rows: Range<usize>) -> Range<usize>`.
  `RowItem`/`Row` go away; nothing needs a per-item `Vec` any more, which also
  drops the layout's allocation from O(items) to O(1).
- `LayoutSig` keeps `images_rev` + `item_count` + `avail_w` + `target_h`. With
  an O(1) layout the cache is arguably unnecessary, but keeping it preserves the
  existing invalidation contract and costs nothing.
- `grid.rs::show`: same `ScrollArea::show_viewport` shape, same
  `now_visible` / `ensure_tags_for` / `ensure_collections_for` /
  `retain_visible_thumbnail_jobs` calls — driven off `indices_for_rows` instead
  of iterating `row.items`. `paint_cell` is unchanged apart from receiving the
  fitted `img_rect`.

### 4.5 Virtualization (load-bearing)

Unchanged in kind and strictly cheaper:

- Layout is O(1) (no per-item loop at all), vs. O(items) plus O(items) text
  layout today.
- The render pass still realizes only `visible_rows` (plus one row of padding
  above/below), so thumbnail decode/upload stays bounded by the viewport.
- No text is measured for off-screen cells; the only galleys built are the
  visible cells' own elided labels.

## 5. Testing

Pure-arithmetic unit tests in `grid_layout.rs` (no egui, no GPU):

1. `cols_derive_from_available_width` — width/gap/cell combinations yield the
   expected column count; never 0.
2. `cell_rects_tile_without_overlap_and_stay_within_width` — for a range of
   widths and counts, no two cell rects intersect and none exceeds `avail_w`.
   **This is the direct regression for the overflow that clipped the labels.**
3. `every_cell_has_identical_dimensions` — ask 1, asserted structurally.
4. `cell_height_scales_with_the_size_slider_and_never_collapses` — ask 2: for
   the full slider range, `cell_h` is monotonic in the slider and never below
   the minimum implied by the smallest slider value. Includes an extreme-aspect
   item (20:1 panorama and 1:20 portrait) to prove aspect cannot affect it.
5. `letterbox_fit_is_contained_and_centered` — the fitted `img_rect` is inside
   the cell, preserves the source aspect within tolerance, and is centered on
   both axes; covers panorama, portrait, square, and exact-3:2 cases.
6. `visible_rows_windows_around_scroll` / `..._empty_when_no_rows` — ported.
7. `indices_for_rows_covers_only_visible_cells` — pins virtualization: the
   realized index range for a viewport is bounded by
   `cols * (visible_rows + padding)`, independent of `item_count` (assert with
   100k items that the realized count stays small).

Elision itself is egui behaviour and is verified by eye (see §6); what is
testable and worth pinning is that **no code path measures a filename to size a
cell** — a source-grep guard test in the spirit of the existing
`every_action_is_in_a_settings_group`: assert `library/grid.rs` contains no
`layout_no_wrap` call and no `min_width` layout input, so the O(n) measurement
cannot silently return.

## 6. Visual test plan (for the author, after the gate is green)

All in **Library**, with `fixtures/raw/` ingested.

1. **Consistency.** Set Size to ~50. Every thumbnail cell must be the same box,
   in a tidy rectangular grid. *Failure:* any row shorter/taller than its
   neighbours.
2. **No collapse at any size.** Drag Size slowly from minimum to maximum. Cell
   height must grow monotonically and no row may ever flatten to a strip.
   *Failure:* a row snapping to a fraction of its neighbours at some slider
   position (the old `0.4x` clamp).
3. **Panorama letterboxing.** `sample.rw2` (4060x2250, the widest fixture) must
   sit centered in a full-height cell with background above/below — not fill the
   cell, not stretch. *Failure:* cropped edges or a distorted image.
4. **Portrait letterboxing.** `keystone-sony-ilce7m4-iso320-25mm.ARW` (Rotate270
   → portrait) must be centered with background left/right, upright.
   *Failure:* sideways, or stretched to the cell.
5. **Long filename.** `ultrawide-sony-ilce7m3-iso125-16mm.ARW` and
   `highres61mp-sony-ilce7rm5-iso100-50mm.ARW` must show a single elided line
   with an ellipsis, fully inside their cell. Hover → tooltip with the full
   name. *Failure:* text crossing the cell edge, overlapping a neighbour, or cut
   without an ellipsis.
6. **Short filename.** `sample.rw2` / `DSC04692.ARW` must show the whole name
   with **no** ellipsis and no tooltip.
7. **Right edge.** At several panel widths (drag the window, and collapse/expand
   the left panel) nothing may cross the right margin, and the block should stay
   evenly padded rather than hugging one side.
8. **Responsiveness.** Scroll the full fixture set fast at maximum Size. No
   stutter, no multi-second freeze; thumbnails fill in as rows arrive. *Failure:*
   a hitch on resize or on the first scroll (would mean per-item work returned).
9. **Selection.** Click a portrait and a panorama. The accent outline must hug
   the **image**, not the cell box.
10. **Cropped image.** Crop one fixture in Develop, return to Library. Its cell
    must letterbox at the **cropped** aspect once the thumbnail regenerates.

## 7. Non-goals

- The Size-slider range itself (see Q1) and the `IBM Plex Mono` label font from
  the V2 doc — the current label uses the proportional face. Both are cosmetic
  deltas from the doc, separable from this fix.
- Filmstrip layout. It has its own `MIN_ASPECT`/`MAX_ASPECT` clamp and is not
  affected.
- Thumbnail generation, staleness/regen, drag-and-drop, and context menus — all
  untouched.

## 8. Resolved questions

The four questions this spec opened were resolved on approval by taking each
stated recommendation — recorded as **D4–D7** in §3 rather than left open, so the
plan has no undetermined inputs. D4 is the only one with a feel-level
consequence (the Size slider's range); it is a single constant if the author
wants today's 60..160 back after seeing it.

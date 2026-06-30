# ferrolite — Develop module switch & filmstrip navigation (design)

> **Status:** Design — approved by user; pending writing-plans.
> **Date:** 2026-06-30
> **Branch:** `feat/viewer-and-vt-ladder` (UI follow-up after Plan 4, before finishing the branch).
> **Parent:** `2026-06-29-viewer-and-vt-ladder-design.md` (the viewer/VT this builds the UX on).

---

## 1. Problem

After Plan 4, opening an image works but the UX has two gaps:
1. Opening an image does **not** switch to the Develop module, and the viewer overrides the
   central panel regardless of the active module tab — so you can't open another image from the
   catalogue without first pressing Esc.
2. There is no in-viewer way to move between images.

## 2. Goal

- Opening an image switches to **Develop**; the Library/Develop segmented tabs meaningfully drive
  the central panel; the catalogue grid remains reachable so another image can always be opened.
- In Develop, the top bar becomes a **filmstrip** of the current image set, navigable by clicking a
  thumbnail or by the **arrow keys**, for fast image-to-image movement.

## 3. Design

### 3.1 Module-driven central panel (the core fix)
Today: `if viewer.is_some() { viewer } else if library { grid } else { stub }` — the viewer overrides
everything and the module tab is ignored while viewing. Change to **module-driven**:
- **Library** → grid (always).
- **Develop** → viewer if `viewer.is_some()`, else the existing wgpu canvas stub.

Opening an image (grid double-click / Enter) sets `module = Module::Develop` and opens the viewer.
Viewer state is **preserved across tab switches**: clicking **Library** shows the grid again (open
another image → returns to Develop); clicking **Develop** resumes the current image. **Esc** closes
the viewer (cancelling its decode + tile jobs, as today) **and** returns to the **Library** tab so the
user lands on the grid.

### 3.2 Left panel
The 236px Catalog/Folders `SidePanel` is shown only when `module.is_library()`; it is **hidden in
Develop**, giving the viewer the full canvas width.

### 3.3 Top bar: filters (Library) vs filmstrip (Develop)
The top `TopBottomPanel` height is module-dependent: **40px in Library**, **~72px in Develop**.
- Library → the existing `library::toolbar::show` (search/sort/filter stubs + subfolders + size slider).
- Develop → `library::filmstrip::show`: a horizontally-scrolling row of the current folder's images
  (`state.images`, in the grid's order) rendered as **~96px-wide 3:2 thumbnails**, reusing the
  existing thumbnail texture cache and the grid's lazy-load path (`texture_cache.contains` →
  `reads.get_thumbnail` → `upload_thumbnail`). The currently-open image (`viewer.image_id`) is drawn
  with an **accent-colored outline**; the strip **auto-scrolls** (`scroll_to_rect`) to keep it visible.
  Clicking a thumbnail switches the viewer to that image (staying in Develop).

### 3.4 Navigation
- A pure helper `viewer::nav::neighbor_index(current: usize, len: usize, dir: Step) -> Option<usize>`,
  where `Step::Prev` = `current - 1` and `Step::Next` = `current + 1`, returning `None` at the ends
  (**non-cyclic**). `Left` arrow → `Prev`, `Right` arrow → `Next` (standard mapping).
- In Develop with a viewer open and `!ctx.wants_keyboard_input()`: Left/Right arrows find the open
  image's index in `state.images`, compute the neighbor, and if `Some(idx)` open `state.images[idx]`.
- Filmstrip clicks and arrow keys both route through **one shared open path** on `FerroliteApp`
  (e.g. `open_record(frame, &ImageRecord)`) that: cancels the old viewer's decode jobs
  (`ViewerState::cancel_loads`) + sparse tile jobs (`cancel_viewer_tiles`), sets `module = Develop`,
  and calls `AppState::open_image_in_viewer(&rec)` — identical to today's open, without leaving
  Develop. The two-tier load (preview → full → sparse VT → crossfade) runs unchanged for the new
  image.

### 3.5 Files
- `ferrolite-app/src/app.rs` — module-driven central panel; per-module top-bar height +
  filmstrip-vs-filters; left panel gated on Library; set `module = Develop` on every open;
  Left/Right arrow handling; Esc → close viewer + return to Library; the shared `open_record` helper.
- `ferrolite-app/src/library/filmstrip.rs` (new) — the filmstrip widget; returns the clicked image id
  (if any). Reuses `texture_cache` + `reads.get_thumbnail`/`upload_thumbnail`.
- `ferrolite-app/src/viewer/nav.rs` (new) — `Step` enum + pure `neighbor_index` + unit tests.
- `ferrolite-app/src/library/mod.rs` — declare `pub mod filmstrip;`.
- `ferrolite-app/src/viewer/mod.rs` — declare `pub mod nav;`.

## 4. Testing
- `neighbor_index`: pure unit tests — next/prev in the middle; clamp (`None`) at first (Prev) and
  last (Next); empty list → `None`; single-image list → `None` both directions.
- Module routing, left-panel gating, and the filmstrip are egui glue: covered by `cargo build` +
  `clippy` + the existing app tests staying green, plus the user's manual GUI smoke.

## 5. Out of scope (YAGNI)
Real Develop edit panel / adjustment sliders (Spec 2); sort/filter wiring; filmstrip drag-reorder;
multi-select. The filmstrip shows existing catalog thumbnails only.

## 6. Decisions recorded (2026-06-30)
| Question | Decision |
|---|---|
| Arrow-key mapping | **Standard**: Left = previous, Right = next; non-cyclic (nothing at the ends). |
| Filmstrip appearance | Develop top bar grows to ~72px; ~96px 3:2 thumbnails; current outlined in accent; auto-scroll. |
| Left panel in Develop | **Hidden** (full-width canvas); browse folders via the Library tab. |
| Open-another-image flow | Library tab restores the grid (viewer preserved under Develop); Esc returns to Library. |

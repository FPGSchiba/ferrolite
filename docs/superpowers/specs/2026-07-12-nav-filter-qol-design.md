# Navigation, Filtering & QoL Improvements — Design

**Date:** 2026-07-12
**Branch:** `feat/nav-filter-qol`
**Status:** Approved design, pending implementation plan

Five independent quality-of-life improvements to navigation, filtering, and
image inspection. Each is small and mostly self-contained; they share no state
except where noted. This document is the source of truth for the implementation
plan that follows.

---

## 1. Remove from collection

### Goal
An image that belongs to a collection can be removed from it. Also fixes a
current annoyance: the "Add to collection" menu lists collections the image is
*already* a member of.

### Current state
- `catalog.add_image_to_collection(coll_id, image_id)` and
  `catalog.remove_image_from_collection(coll_id, image_id)` both already exist
  (`ferrolite-catalog/src/catalog.rs:520,529`).
- `image_context_menu::show` (`ferrolite-app/src/library/image_context_menu.rs`)
  has an "Add to collection ▸" submenu listing **all** collections
  unconditionally, with no membership awareness and no removal path.
- **There is no per-image collection-membership data in the app.** `AppState`
  caches per-image tags in `visible_tags: HashMap<i64, Vec<TagId>>` but has no
  equivalent for collections, and the catalog has no "collections for image"
  query.

### Design

**Membership data (new, mirrors `visible_tags`).**
- Catalog: add `collections_for_images(ids: &[i64]) -> HashMap<i64, Vec<i64>>`
  (image_id → collection_ids) in `ferrolite-catalog/src/queries.rs`, exposed via
  `read_pool` so it runs on the read pool, not the UI thread. A single
  `SELECT image_id, collection_id FROM collection_images WHERE image_id IN (...)`.
- `AppState`: add `visible_collections: HashMap<i64, Vec<i64>>`. Populate it
  **off-thread** on the same trigger that fills `visible_tags` (when grid/loupe
  images become visible), delivered back over the app event channel — never a
  synchronous DB read while building the menu (CLAUDE responsiveness rule 1).
  Menu-open reads only the in-memory cache; a cache miss simply shows no
  membership (submenu empty / all collections addable) until the async fill
  lands.

**"Add to collection ▸" — filter out existing members.**
For each candidate collection, skip it if the target image (or, in
multi-selection scope, *all* selected images) already belong to it. Concretely:
a collection is offered for "Add" only if at least one target image is **not**
yet a member. This removes the current behaviour of listing already-member
collections.

**"Remove from collection ▸" — new submenu.**
Lists only collections the target image(s) belong to (union across the
selection). Selecting one removes the target(s) from that collection.

**Fast path when viewing a collection.**
When the current view is `ViewSource::Collection(id)`, add a top-level
**"Remove from this collection"** item (no submenu) for the open collection.

**New `AppState` methods** (mirror the existing `add_*` pair):
- `remove_image_from_collection_now(image_id, coll_id)`
- `remove_selection_from_collection(coll_id)`
- (add path already exists: `add_image_to_collection_now`,
  `add_selection_to_collection`)

Each optimistically updates `visible_collections`, then persists off-thread via
the catalog. After a removal, if the current `ViewSource` is that collection,
refresh the grid so the removed image disappears from view.

### Testing
- Catalog: `collections_for_images` returns correct membership for a populated
  fixture (unit test with in-memory DB, following existing catalog tests).
- Pure helper: given target ids + a membership map, compute the "addable"
  collections (members excluded) and the "removable" collections (members only).
  Unit-tested without egui.

---

## 2. Image info — overlay + full tab

### Goal
Surface EXIF facts and live viewer state. Two surfaces sharing one data builder:
a compact toggleable **overlay** (like the histogram) with the key facts
photographers set, and a read-only **Info tab** in the Develop tab bar showing
all facts.

### Current state
- `ViewerState.meta: Option<ferrolite_decode::Metadata>` already carries
  `make, model, width, height, orientation, iso, aperture, shutter,
  focal_length, capture_time, lens` (`ferrolite-decode/src/metadata.rs`), read
  off-thread on open.
- ISO is currently shown only in the bottom **status bar**
  (`ferrolite-app/src/status_bar.rs:118`).
- The histogram overlay is gated by a persisted `settings.show_histogram: bool`
  with a toggle in the chrome control cluster and a Settings checkbox — the
  pattern the info overlay mirrors.
- Viewer zoom lives in `ViewTransform { zoom, pan }`; `ViewTransform::fit(dims,
  viewport)` gives the fit transform.

### Design

**`ImageFacts` — pure data builder (new module, e.g. `develop/info.rs`).**
A struct + constructor `ImageFacts::from(meta: &Metadata, view: &ViewTransform,
dims, viewport)` producing formatted, display-ready strings. No egui. Fields:
- Camera: `make model`
- Lens
- Focal length + **35 mm-equivalent** (see note)
- Aperture (`f/N`), Shutter (`1/N s` / `N"`), ISO
- Capture time
- Dimensions (`W × H`)
- **Live zoom %** — computed from `view.zoom` relative to the fit zoom, so
  "Fit" ≈ 100%-of-fit is legible; recomputed each frame it is shown.

**35 mm-equivalent focal length.**
Canonical source is the standard EXIF tag `FocalLengthIn35mmFilm` (0xA405). Add
`focal_length_35mm: Option<u32>` to `ferrolite_decode::Metadata` and read it in
the metadata reader. If absent, show the raw focal length with **no** equivalent
(no crop-factor database — avoids a fragile per-camera lookup).

**Overlay surface.**
- Persisted `settings.show_info_overlay: bool` (mirrors `show_histogram`:
  default value, `settings/mod.rs` + `settings/persist.rs`, a toggle in the same
  chrome control cluster as the histogram toggle, and a Settings checkbox).
- Compact HUD over the image showing the *key facts photographers set*: focal
  length (+ equiv), aperture, shutter, ISO, and live zoom %.
- **ISO moves here**: remove ISO from the status-bar line
  (`status_bar.rs:118`) now that it lives in the overlay. (Filename + dimensions
  remain in the status bar.)

**Info tab surface.**
- A read-only tab in the Develop tab bar showing **all** `ImageFacts` fields.
- Implemented as a `PanelTab` whose `show` renders labels/values and always
  returns `None` (no `EditOutcome` — it is not editable, so per-control reset
  does not apply).
- **Mutual exclusion:** when the Info tab becomes active, set
  `show_info_overlay = false`. The overlay may be re-toggled afterward; the two
  never render simultaneously.

### Testing
- `ImageFacts` formatting: given a `Metadata` + a `ViewTransform`, assert each
  formatted string (focal length with/without 35 mm equiv, missing-field
  handling, zoom % at fit and at 1:1). Pure unit tests.
- Metadata reader: `FocalLengthIn35mmFilm` parsed into
  `focal_length_35mm` when present, `None` when absent (decode crate test).

---

## 3. Persistent tool/tab state across image switches (session)

### Goal
Navigating between images keeps the active tool and tab instead of resetting to
Adjust/Light every time.

### Current state
`ToolState { active, active_tab, base_tab }` (`develop/tool_state.rs`) lives on
`ViewerState` and is set to `ToolState::default()` in every `ViewerState`
constructor (`viewer/mod.rs:341`) — so each image open resets it.

### Design
Lift the field to **`AppState.tool_state: ToolState`** (in-memory only — **not**
persisted, so it resets on app restart, per decision). `ViewerState` no longer
owns `tool_state`; Develop reads and writes `AppState.tool_state` instead. After
an image load, call `tool_state.ensure_valid_tab(reg)` so a tab that does not
exist for the newly-loaded image (e.g. a tool-specific tab) falls back to a
valid one instead of showing an empty bar.

Because `ToolState` is `Copy`, the existing read-out-mutate-write-back access
pattern is preserved; only the owner moves from `ViewerState` to `AppState`.

### Testing
- `ToolState` carry-forward: selecting a tool/tab, then simulating an image
  switch (new viewer) leaves `AppState.tool_state` unchanged.
- `ensure_valid_tab` after switch: an `active_tab` invalid for the new registry
  falls back to the first valid tab. (Extends existing `tool_state` tests.)

---

## 4. Fit + 1:1 zoom hotkeys

### Goal
Keyboard zoom-to-fit and zoom-to-100%.

### Current state
Fit/1:1 math already exists — the double-click handler toggles between
`ViewTransform::fit(dims, viewport)` and a centered `zoom: 1.0`
(`viewer/mod.rs:568-579`). The `Action` enum has no zoom actions.

### Design
Add two rebindable actions to the keymap (`settings/keymap.rs`):
- `ZoomFit` — proposed default **F**
- `ZoomActual` — proposed default **1** (1:1 / 100 %)

Wire each per the load-bearing CLAUDE keybind rules:
1. Add to the `Action` enum, `Action::ALL`, and `label()`.
2. Bind defaults in `Keymap::defaults()`.
3. Add to a Settings keyboard-tab `GROUPS` entry (enforced by
   `every_action_is_in_a_settings_group`).
4. Add to the Help panel shortcut list (keybind-discoverability rule).
5. Any on-screen zoom affordance that triggers these shows the bound key in its
   tooltip via `Keymap::hint(action)`, formatted `"<Label> (<Key>)"`.

Handlers reuse the existing fit and centered-1:1 transforms; dispatch alongside
the other viewer key actions. Firing either wakes the drive loop (zoom changes
the visible LOD/tiles), matching the existing zoom paths.

### Testing
- `every_action_is_in_a_settings_group` (existing test) stays green with the two
  new actions.
- Defaults coverage test (all `Action::ALL` bound) stays green.
- Pure transform reuse is already covered by existing viewer math tests; no new
  math is introduced.

---

## 5. "Not seen" filter (no flag set)

### Goal
Filter the library/develop views to images with **neither** Pick nor Reject —
i.e. `Flag::None`.

### Current state
- `Flag::None` maps to integer `0` (`ferrolite-image/src/meta.rs`).
- `LibraryQuery` already compiles `flag IN (...)` from `flags: Vec<Flag>`
  (`ferrolite-catalog/src/query.rs:157`), and `FilterState.flags` feeds it
  (`library/filter.rs`).
- `filter_widgets::flag_filters` renders only Pick and Reject toggles.

### Design
Add a third toggle to `filter_widgets::flag_filters` for **`Flag::None`**
("Not seen" / unflagged), using a neutral outline-flag glyph. It pushes/removes
`Flag::None` in `FilterState.flags` exactly like the other two, so it flows
through the existing `FilterState → LibraryQuery.flags → flag IN (...)` path
unchanged and works in **both** the Library toolbar and the Develop filter strip
automatically (both call `flag_filters`).

### Testing
- Query mapping: a `FilterState` with `flags = [Flag::None]` compiles to
  `flag IN (?)` with param `0`. (Extends `library/filter.rs` /
  `query.rs` tests.)
- Toggle add/remove behaviour on `Flag::None` matches the existing toggles.

---

## Cross-cutting requirements (CLAUDE.md, load-bearing)

- **Responsiveness / threading (rule 1):** the new collection-membership fetch
  and any metadata read go through `ferrolite-jobs` / the read pool and are
  delivered over the app event channel — never a synchronous DB read on the UI
  thread (e.g. while building the context menu).
- **Icons (load-bearing):** every new glyph — the neutral "not seen" flag, the
  info-overlay toggle, and any zoom-fit affordance — is added as a **semantic
  alias in `ferrolite-app/src/icons.rs`** sourced from the Phosphor catalog and
  rendered via the icon font. No raw emoji, no hand-drawn `Painter` shapes.
- **Keybind tooltips + discoverability (load-bearing):** `ZoomFit` / `ZoomActual`
  appear in the Settings keyboard tab (a `GROUPS` entry) **and** the Help panel,
  and any bound on-screen control shows its key via `Keymap::hint`.
- **Per-control reset (load-bearing):** N/A — no new *adjustable* editing
  controls. The Info tab is read-only; filter toggles clear on re-click.
- **Finishing (load-bearing):** after the workspace gate (`cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`) is green, hand Jann a numbered visual test plan
  (overlay toggle + ISO relocation, Info tab mutual exclusion, tool/tab
  persistence across navigation, F / 1 zoom, not-seen filter, add/remove
  collection with member-filtering) and hold for hands-on feedback before
  finishing the branch.

## Out of scope (YAGNI)
- Persisting tool/tab state across app restarts (session-only chosen).
- A crop-factor database for 35 mm-equiv when EXIF lacks `FocalLengthIn35mmFilm`.
- Bulk collection management UI beyond the context menu.
- Rebindable gestures for the info-overlay toggle (mirrors histogram; no keybind
  required unless the histogram has one — match whatever the histogram does).

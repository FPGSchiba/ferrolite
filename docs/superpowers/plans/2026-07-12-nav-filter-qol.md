# Navigation, Filtering & QoL Improvements — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship five QoL features — remove-from-collection (with member-filtered add), an image-info overlay + tab, session-persistent Develop tool/tab state, fit + 1:1 zoom hotkeys, and a "not seen" flag filter.

**Architecture:** Each feature is independent and follows an existing pattern in the `ferrolite-app` egui frontend (plus small additions to `ferrolite-catalog` and `ferrolite-decode`). Pure logic (query mapping, fact formatting, membership set math) is extracted into egui-free functions with unit tests; UI surfaces reuse established affordances (histogram-style overlay toggle, `flag_filters` toggles, the Develop tab registry, the keymap dispatch).

**Tech Stack:** Rust 2021, egui/eframe, rusqlite (bundled SQLite), `ferrolite-jobs` (off-thread work), serde (settings).

**Source spec:** `docs/superpowers/specs/2026-07-12-nav-filter-qol-design.md`

## Global Constraints

- **Never block the UI thread.** DB reads/metadata reads go through `ferrolite-jobs` / the catalog read pool and are delivered over the app event channel; the context menu reads only in-memory caches (CLAUDE.md responsiveness rule 1).
- **Icons only from the `icons` module** (`ferrolite-app/src/icons.rs`), sourced from the Phosphor catalog; no raw emoji, no hand-drawn `Painter` shapes (CLAUDE.md UI icons rule).
- **Keybind discoverability (load-bearing):** every new rebindable `Action` MUST appear in a Settings keyboard-tab `GROUPS` entry (enforced by test `every_action_is_in_a_settings_group`) AND the Help panel shortcut list; bound controls show their key via `Keymap::hint`.
- **Per-control reset:** N/A here — no new adjustable editing controls.
- **Formatting/lints:** `cargo fmt` before every commit; code must pass `cargo clippy --workspace --all-targets -- -D warnings`. Max line width 100.
- **Commit format:** conventional commits (`feat:`, `fix:`, `test:`, `refactor:`, `docs:`). No attribution footer.
- **Workspace gate (run before declaring done):** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.

**Task order note:** Tasks are independent except Feature 1 (Task 4 → 5 → 6) and Feature 2 (Task 7 → 8 → 9 → 10). Recommended order: 1, 2, 3 (small wins), then 4–6, then 7–10.

---

## Feature 5 — "Not seen" flag filter

### Task 1: Add a `Flag::None` ("Not seen") toggle to the flag filter

**Files:**
- Modify: `ferrolite-app/src/library/filter_widgets.rs` (`flag_filters`)
- Test: same file's `#[cfg(test)] mod tests`
- Verify (no change): `ferrolite-catalog/src/query.rs` already compiles `flag IN (...)` with `Flag::None → 0`.

**Interfaces:**
- Consumes: `ferrolite_image::Flag`, `crate::library::icons::flag(...)`.
- Produces: `flag_filters(ui, flags: &mut Vec<Flag>) -> bool` unchanged signature; now also toggles `Flag::None`. New pure `toggle_flag(flags: &mut Vec<Flag>, f: Flag)`.

- [ ] **Step 1: Write the failing test.** Add to the `tests` module in `filter_widgets.rs`:

```rust
#[test]
fn toggle_flag_adds_then_removes() {
    let mut flags = vec![Flag::Pick];
    toggle_flag(&mut flags, Flag::None);
    assert!(flags.contains(&Flag::None));
    toggle_flag(&mut flags, Flag::None);
    assert!(!flags.contains(&Flag::None));
    assert_eq!(flags, vec![Flag::Pick]);
}
```

- [ ] **Step 2: Run it, verify it fails.** Run: `cargo test -p ferrolite-app toggle_flag_adds_then_removes` — Expected: FAIL (`cannot find function toggle_flag`).

- [ ] **Step 3: Extract the toggle helper and add the `Flag::None` toggle.** At the top of `filter_widgets.rs` add:

```rust
/// Add `f` to `flags` if absent, else remove it. Shared by every flag toggle.
pub fn toggle_flag(flags: &mut Vec<Flag>, f: Flag) {
    if let Some(p) = flags.iter().position(|x| *x == f) {
        flags.remove(p);
    } else {
        flags.push(f);
    }
}
```

Then rewrite `flag_filters` to iterate three entries and route clicks through `toggle_flag`:

```rust
pub fn flag_filters(ui: &mut egui::Ui, flags: &mut Vec<Flag>) -> bool {
    let mut changed = false;
    for (f, color) in [
        (Flag::None, crate::theme::TEXT_FAINT),
        (Flag::Pick, crate::theme::SEMANTIC_GREEN),
        (Flag::Reject, crate::theme::SEMANTIC_RED),
    ] {
        let active = flags.contains(&f);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
        if active {
            ui.painter().rect_filled(rect, 2.0, crate::theme::ACCENT_BG_SEL);
        }
        icons::flag(
            ui.painter(),
            rect.center() + egui::vec2(0.0, 4.0),
            11.0,
            active,
            color,
            false,
            f == Flag::Reject,
        );
        let tooltip = match f {
            Flag::None => "Not seen (no flag)",
            Flag::Pick => "Pick",
            Flag::Reject => "Reject",
        };
        if resp.on_hover_text(tooltip).clicked() {
            toggle_flag(flags, f);
            changed = true;
        }
    }
    changed
}
```

Note: `Flag::None` is drawn with the neutral `TEXT_FAINT` colour to distinguish it from Pick/Reject. If a distinct outline-flag glyph reads better, add a semantic alias in `icons.rs` (Phosphor `flag` outline) and call it — do NOT hand-draw.

- [ ] **Step 4: Run tests, verify pass.** Run: `cargo test -p ferrolite-app filter_widgets` — Expected: PASS.

- [ ] **Step 5: Add a query-mapping regression test** in `ferrolite-catalog/src/query.rs` `tests`:

```rust
#[test]
fn not_seen_flag_compiles_to_flag_in_zero() {
    let q = LibraryQuery { flags: vec![Flag::None], ..base() };
    let (sql, params) = q.compile();
    assert!(sql.contains("flag IN (?)"), "sql: {sql}");
    assert_eq!(params, vec![Value::Integer(0)]);
}
```

Run: `cargo test -p ferrolite-catalog not_seen_flag_compiles_to_flag_in_zero` — Expected: PASS (locks in existing behaviour; no production change).

- [ ] **Step 6: Commit.**

```bash
cargo fmt
git add ferrolite-app/src/library/filter_widgets.rs ferrolite-catalog/src/query.rs
git commit -m "feat(filter): add 'not seen' (unflagged) flag filter toggle"
```

---

## Feature 4 — Fit + 1:1 zoom hotkeys

### Task 2: Add `ZoomFit` / `ZoomActual` actions and dispatch them

**Files:**
- Modify: `ferrolite-app/src/settings/keymap.rs` (`Action` enum, `ALL`, `label`, `defaults`)
- Modify: `ferrolite-app/src/settings/ui/keyboard.rs` (`GROUPS`)
- Modify: `ferrolite-app/src/help.rs` (shortcut list)
- Modify: `ferrolite-app/src/app.rs` (viewer key-action dispatch — search where `Action::PrevImage` / `Action::ToggleSplitCompare` are handled)
- Verify: `ferrolite-app/src/viewer/mod.rs:568-579` — reuse `ferrolite_vt::ViewTransform::fit(dims, viewport)` and the centered `zoom: 1.0` construction the double-click toggle already builds.

**Interfaces:**
- Produces: `Action::ZoomFit`, `Action::ZoomActual`; both dispatched in the viewer key handler to set the viewer's `view`.
- Defaults: `ZoomFit → F`, `ZoomActual → Z`. (`1` is unavailable — `Num1` is `Rating1`; plain `Z` is free, Undo is `Ctrl+Z`.)

- [ ] **Step 1: Write the failing test** in `keymap.rs` `tests`:

```rust
#[test]
fn zoom_actions_have_default_binds() {
    let km = Keymap::defaults();
    assert_eq!(km.chord(Action::ZoomFit).key, Key::F);
    assert_eq!(km.chord(Action::ZoomActual).key, Key::Z);
}
```

(If `Chord.key` is private, read the `Chord` struct in this file and assert via its public accessor / `.label()` returning `"F"` and `"Z"` instead.)

- [ ] **Step 2: Run it, verify it fails.** Run: `cargo test -p ferrolite-app zoom_actions_have_default_binds` — Expected: FAIL (`no variant ZoomFit`).

- [ ] **Step 3: Add the two variants.** In `keymap.rs`:
  - Add `ZoomFit,` and `ZoomActual,` to `enum Action`.
  - Add both to `Action::ALL` and bump the length `[Action; 25]` → `[Action; 27]`.
  - Add to `label()`: `Action::ZoomFit => "Zoom to fit",` and `Action::ZoomActual => "Zoom 1:1 (100%)",`.
  - In `defaults()` insert before the forward-compat fill loop:

```rust
        m.insert(ZoomFit, plain(Key::F));
        m.insert(ZoomActual, plain(Key::Z));
```

- [ ] **Step 4: Run test, verify pass.** Run: `cargo test -p ferrolite-app zoom_actions_have_default_binds` — Expected: PASS.

- [ ] **Step 5: Add to the Settings keyboard `GROUPS`.** In `settings/ui/keyboard.rs`, find the `GROUPS` constant (`(group_label, &[Action])` entries). Add `Action::ZoomFit` and `Action::ZoomActual` to the group that already holds `PrevImage`/`ToggleSplitCompare` (viewer/navigation). Run: `cargo test -p ferrolite-app every_action_is_in_a_settings_group` — Expected: PASS.

- [ ] **Step 6: Add to the Help panel shortcut list.** In `help.rs`, find where shortcut rows are built from `keymap.hint(Action::...)`. Add two rows for `Action::ZoomFit` and `Action::ZoomActual`, matching the surrounding rows' construction exactly.

- [ ] **Step 7: Dispatch the actions in the viewer key handler.** In `app.rs`, locate the match/block handling viewer actions (search `Action::ToggleSplitCompare`). Add handlers that reuse the existing fit/1:1 math. Read `viewer/mod.rs:568-579` first and copy how it builds both transforms (variable/field names for image dims, canvas size, and the 1:1 pan). Pattern to adapt:

```rust
                if keymap.pressed(Action::ZoomFit, i) {
                    if let Some(v) = self.state.viewer.as_mut() {
                        let dims = v.image_dims();          // image (w, h)
                        let viewport = v.last_canvas_size;  // last painted canvas size
                        v.view = ferrolite_vt::ViewTransform::fit(dims, viewport);
                        ctx.request_repaint();              // wake drive loop (LOD/tiles)
                    }
                }
                if keymap.pressed(Action::ZoomActual, i) {
                    if let Some(v) = self.state.viewer.as_mut() {
                        // Build 1:1 exactly as the double-click branch does at viewer/mod.rs:571-579.
                        v.view = ferrolite_vt::ViewTransform { zoom: 1.0, pan: v.view.pan };
                        ctx.request_repaint();
                    }
                }
```

Do NOT invent accessor names — use the real ones from `viewer/mod.rs`. If the double-click 1:1 branch recenters pan, replicate that here rather than keeping `v.view.pan`.

- [ ] **Step 8: Build + test.** Run: `cargo build -p ferrolite-app`, `cargo test -p ferrolite-app`, `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: builds, passes, no warnings.

- [ ] **Step 9: Commit.**

```bash
cargo fmt
git add ferrolite-app/src/settings/keymap.rs ferrolite-app/src/settings/ui/keyboard.rs ferrolite-app/src/help.rs ferrolite-app/src/app.rs
git commit -m "feat(viewer): add Zoom-to-fit (F) and Zoom 1:1 (Z) hotkeys"
```

---

## Feature 3 — Session-persistent tool/tab state

### Task 3: Lift `ToolState` from `ViewerState` to `AppState`

**Files:**
- Modify: `ferrolite-app/src/state.rs` (add `pub tool_state: ToolState` field + init in both constructors)
- Modify: `ferrolite-app/src/viewer/mod.rs` (remove the `tool_state` field ~line 208 + its `default()` init ~line 341)
- Modify: all readers/writers of `viewer.tool_state` → `state.tool_state`
- Test: `ferrolite-app/src/develop/tool_state.rs` `tests`

**Interfaces:**
- Consumes: `crate::develop::tool_state::ToolState` (already `Copy`, `Default`).
- Produces: `AppState.tool_state: ToolState` — the single owner; `ViewerState` no longer has the field.

- [ ] **Step 1: Find every use site.** Run: `grep -rn "tool_state" ferrolite-app/src` — record each read/write (expect `develop/mod.rs`, `develop/tool_panel.rs`, `develop/tool_palette.rs`, `app.rs`; adapt to actual results).

- [ ] **Step 2: Write the failing test** in `tool_state.rs` `tests` (reuse the module's existing registry/`DummyTab` helpers):

```rust
#[test]
fn selecting_a_tab_survives_ensure_valid_when_still_present() {
    let reg = test_registry();                 // reuse this file's existing helper
    let mut ts = ToolState::default();
    ts.select_tab(TabId("color"), &reg);
    assert_eq!(ts.active_tab, TabId("color"));
    ts.ensure_valid_tab(&reg);                 // simulates re-validation on image switch
    assert_eq!(ts.active_tab, TabId("color"), "valid tab kept across switch");
}
```

If the file has no `test_registry()` helper, build the registry the same way the existing tests in this module do.

- [ ] **Step 3: Run it, verify it fails or compiles red.** Run: `cargo test -p ferrolite-app selecting_a_tab_survives_ensure_valid_when_still_present` — Expected: FAIL until helpers align; then make it pass with the pure `ToolState` API.

- [ ] **Step 4: Move the field.** In `state.rs`: add `pub tool_state: crate::develop::tool_state::ToolState,` to `AppState` and `tool_state: Default::default(),` to BOTH constructors (real one ~line 292; test one ~line 859). In `viewer/mod.rs`: delete the `pub tool_state: ...` field (~208) and its initializer (~341).

- [ ] **Step 5: Repoint all use sites** from `…viewer.tool_state` to `state.tool_state` / `self.state.tool_state`. After an image load completes, keep the tab valid for the new image — find the post-load hook (in `develop/mod.rs` or the open path) and insert:

```rust
        state.tool_state.ensure_valid_tab(&registry);
```

(Use the registry variable already in scope there.)

- [ ] **Step 6: Build + test.** Run: `cargo build -p ferrolite-app` (fix any missed use sites), then `cargo test -p ferrolite-app tool_state` — Expected: PASS.

- [ ] **Step 7: Commit.**

```bash
cargo fmt
git add ferrolite-app/src/state.rs ferrolite-app/src/viewer/mod.rs ferrolite-app/src/develop ferrolite-app/src/app.rs
git commit -m "feat(develop): persist tool/tab selection across image switches (session)"
```

---

## Feature 1 — Remove from collection

### Task 4: Catalog `collections_for_images` membership query

**Files:**
- Modify: `ferrolite-catalog/src/queries.rs` (new function)
- Modify: `ferrolite-catalog/src/catalog.rs` (public method) and, if the app reads via the pool, `ferrolite-catalog/src/read_pool.rs` (wrapper next to `list_collections`, ~line 93)
- Test: `ferrolite-catalog/src/catalog.rs` `tests`

**Interfaces:**
- Produces: `Catalog::collections_for_images(&self, ids: &[i64]) -> Result<HashMap<i64, Vec<i64>>, CatalogError>` (image_id → collection_ids).

- [ ] **Step 1: Write the failing test** in `catalog.rs` `tests` (follow `create_and_populate_collection` ~line 630 for fixture/helpers):

```rust
#[test]
fn collections_for_images_maps_membership() {
    let cat = test_catalog();                 // reuse the in-memory helper sibling tests use
    let a = insert_test_image(&cat, "a.raf"); // reuse the existing image inserter
    let b = insert_test_image(&cat, "b.raf");
    let c1 = cat.create_collection("One", Color::default()).unwrap();
    let c2 = cat.create_collection("Two", Color::default()).unwrap();
    cat.add_image_to_collection(c1, a).unwrap();
    cat.add_image_to_collection(c2, a).unwrap();
    cat.add_image_to_collection(c1, b).unwrap();

    let map = cat.collections_for_images(&[a, b]).unwrap();
    let mut a_colls = map.get(&a).cloned().unwrap_or_default();
    a_colls.sort_unstable();
    assert_eq!(a_colls, vec![c1, c2]);
    assert_eq!(map.get(&b).cloned().unwrap_or_default(), vec![c1]);
}
```

(Use whatever in-memory `Catalog` constructor and image-insert helper the existing tests use — read them first.)

- [ ] **Step 2: Run it, verify it fails.** Run: `cargo test -p ferrolite-catalog collections_for_images_maps_membership` — Expected: FAIL (method missing).

- [ ] **Step 3: Implement the query** in `queries.rs`:

```rust
/// image_id -> collection_ids for the given images. One IN-list query.
pub(crate) fn collections_for_images(
    conn: &Connection,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<i64>>, CatalogError> {
    let mut out: std::collections::HashMap<i64, Vec<i64>> = std::collections::HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let ph = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "SELECT image_id, collection_id FROM collection_images WHERE image_id IN ({ph})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (img, coll) = row?;
        out.entry(img).or_default().push(coll);
    }
    Ok(out)
}
```

Expose it from `Catalog` (mirror `list_collections`):

```rust
    pub fn collections_for_images(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, Vec<i64>>, CatalogError> {
        let conn = self.conn.lock().expect("catalog conn");
        crate::queries::collections_for_images(&conn, ids)
    }
```

Match `Catalog`'s actual connection accessor (read how `list_collections`/`create_collection` obtain `conn`). If the app fetches `visible_tags` through `read_pool`, add the matching wrapper in `read_pool.rs` next to `list_collections`.

- [ ] **Step 4: Run test, verify pass.** Run: `cargo test -p ferrolite-catalog collections_for_images_maps_membership` — Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
cargo fmt
git add ferrolite-catalog/src
git commit -m "feat(catalog): add collections_for_images membership query"
```

### Task 5: `AppState.visible_collections` cache + add/remove methods + set math

**Files:**
- Create: `ferrolite-app/src/library/collection_menu.rs` (pure `addable`/`removable` helpers)
- Modify: `ferrolite-app/src/library/mod.rs` (`pub mod collection_menu;`)
- Modify: `ferrolite-app/src/state.rs` (`visible_collections` field, async fill mirroring `visible_tags`, `remove_*` methods, optimistic add cache)
- Test: `collection_menu.rs` `tests`; `state.rs` `tests`

**Interfaces:**
- Consumes: `Catalog::collections_for_images` (Task 4).
- Produces:
  - `AppState.visible_collections: HashMap<i64, Vec<i64>>`
  - `AppState::remove_images_from_collection(&mut self, ids: &[i64], coll_id: i64)` (shared core)
  - `AppState::remove_selection_from_collection(&mut self, coll_id: i64)`
  - `AppState::remove_image_from_collection_now(&mut self, image_id: i64, coll_id: i64)`
  - `collection_menu::addable_collections(all: &[CollectionRecord], target_ids: &[i64], membership: &HashMap<i64, Vec<i64>>) -> Vec<i64>`
  - `collection_menu::removable_collections(all: &[CollectionRecord], target_ids: &[i64], membership: &HashMap<i64, Vec<i64>>) -> Vec<i64>`

- [ ] **Step 1: Write the failing test** in a new `ferrolite-app/src/library/collection_menu.rs`:

```rust
//! Pure set math for the "Add/Remove to collection" context-menu submenus.
//! No egui, no DB — decides which collections are offered given membership.

use ferrolite_catalog::CollectionRecord;
use std::collections::HashMap;

// ... production fns added in Step 3 ...

#[cfg(test)]
mod tests {
    use super::*;

    // Build a CollectionRecord with the given id; fill remaining fields to match
    // the real struct (read ferrolite-catalog/src/model.rs for field names).
    fn coll(id: i64) -> CollectionRecord {
        CollectionRecord { id, name: format!("c{id}"), color: Default::default() }
    }

    #[test]
    fn addable_excludes_collections_all_targets_already_in() {
        let all = vec![coll(1), coll(2), coll(3)];
        let mut m: HashMap<i64, Vec<i64>> = HashMap::new();
        m.insert(10, vec![1]);
        m.insert(11, vec![1, 2]);
        // Coll 1: both members -> excluded. Coll 2: 10 not in it -> addable. Coll 3: addable.
        assert_eq!(addable_collections(&all, &[10, 11], &m), vec![2, 3]);
    }

    #[test]
    fn removable_includes_collections_any_target_belongs_to() {
        let all = vec![coll(1), coll(2), coll(3)];
        let mut m: HashMap<i64, Vec<i64>> = HashMap::new();
        m.insert(10, vec![1]);
        m.insert(11, vec![1, 2]);
        assert_eq!(removable_collections(&all, &[10, 11], &m), vec![1, 2]);
    }
}
```

Adjust the `coll(...)` literal to `CollectionRecord`'s real fields (from `model.rs`).

- [ ] **Step 2: Run it, verify it fails.** Run: `cargo test -p ferrolite-app addable_excludes` — Expected: FAIL (functions missing).

- [ ] **Step 3: Implement the pure helpers** in `collection_menu.rs`:

```rust
fn is_member(membership: &HashMap<i64, Vec<i64>>, image_id: i64, coll_id: i64) -> bool {
    membership.get(&image_id).is_some_and(|v| v.contains(&coll_id))
}

/// Collections offered for "Add": at least one target is NOT already a member.
pub fn addable_collections(
    all: &[CollectionRecord],
    target_ids: &[i64],
    membership: &HashMap<i64, Vec<i64>>,
) -> Vec<i64> {
    all.iter()
        .filter(|c| target_ids.iter().any(|&id| !is_member(membership, id, c.id)))
        .map(|c| c.id)
        .collect()
}

/// Collections offered for "Remove": at least one target IS a member.
pub fn removable_collections(
    all: &[CollectionRecord],
    target_ids: &[i64],
    membership: &HashMap<i64, Vec<i64>>,
) -> Vec<i64> {
    all.iter()
        .filter(|c| target_ids.iter().any(|&id| is_member(membership, id, c.id)))
        .map(|c| c.id)
        .collect()
}
```

Register: add `pub mod collection_menu;` to `library/mod.rs`.

- [ ] **Step 4: Run test, verify pass.** Run: `cargo test -p ferrolite-app collection_menu` — Expected: PASS.

- [ ] **Step 5: Add the `visible_collections` field + async fill.** In `state.rs`:
  - Add `pub visible_collections: HashMap<i64, Vec<i64>>,` to `AppState`; init `HashMap::new()` in both constructors (~292, ~859).
  - In `refresh_images` add `self.visible_collections.clear();` next to `self.visible_tags.clear();` (~541).
  - Populate on the SAME trigger that fills `visible_tags` for visible rows. Read the `visible_tags` fill path (~505-533 and its delivery) and mirror it exactly (same off-thread / read-pool mechanism), calling `catalog.collections_for_images(missing_ids)` and merging into `visible_collections`. Never a synchronous DB call on the UI thread; match whatever the tags code does so the two stay consistent.

- [ ] **Step 6: Add removal methods + optimistic add-cache** in `state.rs` next to `add_images_to_collection` (~922):

```rust
    /// Shared core: remove every id from the collection; refresh if viewing it.
    pub fn remove_images_from_collection(&mut self, ids: &[i64], coll_id: i64) {
        if ids.is_empty() {
            return;
        }
        {
            let w = self.writer.lock().expect("writer");
            for id in ids {
                let _ = w.remove_image_from_collection(coll_id, *id);
            }
        }
        for id in ids {
            if let Some(v) = self.visible_collections.get_mut(id) {
                v.retain(|c| *c != coll_id);
            }
        }
        if matches!(self.source, ViewSource::Collection(id) if id == coll_id) {
            self.dirty = true;
        }
    }

    pub fn remove_selection_from_collection(&mut self, coll_id: i64) {
        let mut targets: Vec<i64> = self.selection.iter().copied().collect();
        if targets.is_empty() {
            if let Some(id) = self.selected {
                targets.push(id);
            }
        }
        self.remove_images_from_collection(&targets, coll_id);
    }

    pub fn remove_image_from_collection_now(&mut self, image_id: i64, coll_id: i64) {
        self.remove_images_from_collection(&[image_id], coll_id);
    }
```

In `add_images_to_collection`, after the DB writes, optimistically update the cache so a just-added collection immediately drops out of the "Add" submenu:

```rust
        for id in ids {
            let entry = self.visible_collections.entry(*id).or_default();
            if !entry.contains(&coll_id) {
                entry.push(coll_id);
            }
        }
```

(Match the exact writer lock accessor `self.writer` used by `add_image_to_collection`; if the write path differs, mirror `add_images_to_collection` verbatim.)

- [ ] **Step 7: Write a state test** mirroring `add_selection_to_collection_adds_images_and_sets_dirty_when_viewing` (~1585): construct the catalog-backed `AppState`, add an image to a collection, view that collection, `remove_selection_from_collection`, assert `dirty == true` and membership gone. Copy the sibling test's fixture construction.

- [ ] **Step 8: Build + test.** Run: `cargo test -p ferrolite-app collection` and `cargo build -p ferrolite-app` — Expected: PASS/builds.

- [ ] **Step 9: Commit.**

```bash
cargo fmt
git add ferrolite-app/src/state.rs ferrolite-app/src/library/collection_menu.rs ferrolite-app/src/library/mod.rs
git commit -m "feat(state): collection membership cache + remove-from-collection methods"
```

### Task 6: Wire the context menu — filtered Add, new Remove submenus

**Files:**
- Modify: `ferrolite-app/src/library/image_context_menu.rs` (`show`)
- Test: existing `tests` (scoping) stay green; the addable/removable logic is covered by Task 5's pure tests. This task is UI wiring — verified by build + visual test.

**Interfaces:**
- Consumes: `collection_menu::addable_collections`/`removable_collections`, `AppState.visible_collections`, `AppState.collections`, the new `remove_*` methods, `state.source` (`ViewSource`).

- [ ] **Step 1: Replace the collections block.** Swap the existing `if !collections.is_empty() { ui.menu_button("Add to collection", …) }` (lines ~92-105) for the membership-aware version, and add the fast-path item. Add `use crate::library::filter::ViewSource;` to imports.

```rust
    if !collections.is_empty() {
        let target_ids: Vec<i64> = if use_selection {
            let mut v: Vec<i64> = state.selection.iter().copied().collect();
            v.sort_unstable();
            v
        } else {
            vec![image_id]
        };
        let membership = state.visible_collections.clone();
        let addable = crate::library::collection_menu::addable_collections(
            &collections, &target_ids, &membership,
        );
        let removable = crate::library::collection_menu::removable_collections(
            &collections, &target_ids, &membership,
        );

        if !addable.is_empty() {
            ui.menu_button("Add to collection", |ui| {
                for c in collections.iter().filter(|c| addable.contains(&c.id)) {
                    if ui.button(&c.name).clicked() {
                        if use_selection {
                            state.add_selection_to_collection(c.id);
                        } else {
                            state.add_image_to_collection_now(image_id, c.id);
                        }
                        ui.close_menu();
                    }
                }
            });
        }

        if !removable.is_empty() {
            ui.menu_button("Remove from collection", |ui| {
                for c in collections.iter().filter(|c| removable.contains(&c.id)) {
                    if ui.button(&c.name).clicked() {
                        if use_selection {
                            state.remove_selection_from_collection(c.id);
                        } else {
                            state.remove_image_from_collection_now(image_id, c.id);
                        }
                        ui.close_menu();
                    }
                }
            });
        }
    }

    if let ViewSource::Collection(coll_id) = state.source {
        if ui.button("Remove from this collection").clicked() {
            if use_selection {
                state.remove_selection_from_collection(coll_id);
            } else {
                state.remove_image_from_collection_now(image_id, coll_id);
            }
            ui.close_menu();
        }
    }
```

`collections` and `membership` are cloned up front (`collections` is already cloned into a local at the top of `show`), so the `state` borrow inside each closure is fine.

- [ ] **Step 2: Build.** Run: `cargo build -p ferrolite-app` — Expected: builds.

- [ ] **Step 3: Run existing menu tests.** Run: `cargo test -p ferrolite-app image_context_menu` — Expected: PASS.

- [ ] **Step 4: Commit.**

```bash
cargo fmt
git add ferrolite-app/src/library/image_context_menu.rs
git commit -m "feat(library): remove-from-collection menu + hide already-member collections from Add"
```

---

## Feature 2 — Image info overlay + tab

### Task 7: Add `focal_length_35mm` to decode `Metadata`

**Files:**
- Modify: `ferrolite-decode/src/metadata.rs` (struct field + any `Metadata` literals/`Default`)
- Modify: the EXIF reader that populates `Metadata` (grep `focal_length` in `ferrolite-decode/src`)
- Test: the decode crate's metadata test module

**Interfaces:**
- Produces: `Metadata.focal_length_35mm: Option<u32>` — from EXIF tag `FocalLengthIn35mmFilm` (0xA405), `None` when absent.

- [ ] **Step 1: Write the failing test.** Grep `focal_length` in `ferrolite-decode` to find the read helper + fixtures. Add:

```rust
#[test]
fn focal_length_35mm_defaults_none_when_absent() {
    let meta = read_metadata_for_test("fixtures/<a-file-without-35mm-tag>");
    assert!(meta.focal_length_35mm.is_none());
}
```

Prefer a fixture that HAS the tag and assert `Some(expected)` if one exists in `fixtures/`; otherwise the `None` assertion above is the minimum. Use the crate's existing metadata-read test helper.

- [ ] **Step 2: Run it, verify it fails.** Run: `cargo test -p ferrolite-decode focal_length_35mm` — Expected: FAIL (no field).

- [ ] **Step 3: Add the field + reader.** Add `pub focal_length_35mm: Option<u32>,` to `Metadata` (update every `Metadata` literal and any `Default` impl in the crate). In the EXIF reader (same function that reads `focal_length`/`iso`), read the tag. With `kamadak-exif` (workspace dep `exif`):

```rust
        focal_length_35mm: exif_reader
            .get_field(exif::Tag::FocalLengthIn35mmFilm, exif::In::PRIMARY)
            .and_then(|f| f.value.get_uint(0)),
```

Adapt `exif_reader` to the reader's real variable name.

- [ ] **Step 4: Run test, verify pass.** Run: `cargo test -p ferrolite-decode focal_length_35mm` — Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
cargo fmt
git add ferrolite-decode/src
git commit -m "feat(decode): read FocalLengthIn35mmFilm into Metadata.focal_length_35mm"
```

### Task 8: `ImageFacts` pure formatter

**Files:**
- Create: `ferrolite-app/src/develop/info.rs`
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod info;`)
- Test: `info.rs` `tests`

**Interfaces:**
- Consumes: `ferrolite_decode::Metadata`.
- Produces:
  - `struct ImageFacts { camera, lens, focal, aperture, shutter, iso, capture_time, dimensions, zoom: String }`
  - `ImageFacts::build(meta: &Metadata, view_zoom: f32, fit_zoom: f32, dims: (u32,u32)) -> ImageFacts`
  - `pub fn zoom_percent(view_zoom: f32, fit_zoom: f32) -> u32`

- [ ] **Step 1: Write the failing test** in `develop/info.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_decode::Metadata;

    fn meta() -> Metadata {
        Metadata {
            make: "FUJIFILM".into(),
            model: "X-T5".into(),
            width: 6000, height: 4000,
            iso: Some(400),
            aperture: Some(2.8),
            shutter: Some(1.0 / 250.0),
            focal_length: Some(35.0),
            focal_length_35mm: Some(53),
            capture_time: Some("2026:01:02 10:11:12".into()),
            lens: Some("XF35mmF1.4 R".into()),
            ..Metadata::default()
        }
    }

    #[test]
    fn zoom_percent_is_relative_to_fit() {
        assert_eq!(zoom_percent(0.2, 0.2), 100);
        assert_eq!(zoom_percent(0.4, 0.2), 200);
    }

    #[test]
    fn facts_format_focal_with_equiv() {
        let f = ImageFacts::build(&meta(), 0.2, 0.2, (6000, 4000));
        assert_eq!(f.focal, "35mm (53mm eq.)");
        assert_eq!(f.aperture, "f/2.8");
        assert_eq!(f.iso, "ISO 400");
        assert_eq!(f.dimensions, "6000 × 4000");
        assert_eq!(f.zoom, "100%");
    }

    #[test]
    fn facts_omit_equiv_when_absent() {
        let mut m = meta();
        m.focal_length_35mm = None;
        let f = ImageFacts::build(&m, 0.2, 0.2, (6000, 4000));
        assert_eq!(f.focal, "35mm");
    }
}
```

If `Metadata` doesn't derive `Default`, construct the test value with every field explicit instead of `..Metadata::default()`.

- [ ] **Step 2: Run it, verify it fails.** Run: `cargo test -p ferrolite-app info::` — Expected: FAIL (module missing).

- [ ] **Step 3: Implement `info.rs`:**

```rust
//! Pure, egui-free formatting of image EXIF + live viewer zoom into display
//! strings. Shared by the info overlay and the Info tab.

use ferrolite_decode::Metadata;

/// On-screen magnification relative to the fit transform, as a percent.
/// At the fit zoom this returns 100.
pub fn zoom_percent(view_zoom: f32, fit_zoom: f32) -> u32 {
    if fit_zoom <= 0.0 {
        return 100;
    }
    (view_zoom / fit_zoom * 100.0).round() as u32
}

fn fmt_shutter(secs: f32) -> String {
    if secs <= 0.0 {
        String::new()
    } else if secs >= 1.0 {
        format!("{secs:.0}\"")
    } else {
        format!("1/{}", (1.0 / secs).round() as u32)
    }
}

pub struct ImageFacts {
    pub camera: String,
    pub lens: String,
    pub focal: String,
    pub aperture: String,
    pub shutter: String,
    pub iso: String,
    pub capture_time: String,
    pub dimensions: String,
    pub zoom: String,
}

impl ImageFacts {
    pub fn build(meta: &Metadata, view_zoom: f32, fit_zoom: f32, dims: (u32, u32)) -> Self {
        let focal = match (meta.focal_length, meta.focal_length_35mm) {
            (Some(f), Some(eq)) => format!("{:.0}mm ({eq}mm eq.)", f),
            (Some(f), None) => format!("{:.0}mm", f),
            (None, _) => String::new(),
        };
        ImageFacts {
            camera: format!("{} {}", meta.make, meta.model).trim().to_string(),
            lens: meta.lens.clone().unwrap_or_default(),
            focal,
            aperture: meta.aperture.map(|a| format!("f/{a:.1}")).unwrap_or_default(),
            shutter: meta.shutter.map(fmt_shutter).unwrap_or_default(),
            iso: meta.iso.map(|v| format!("ISO {v}")).unwrap_or_default(),
            capture_time: meta.capture_time.clone().unwrap_or_default(),
            dimensions: format!("{} × {}", dims.0, dims.1),
            zoom: format!("{}%", zoom_percent(view_zoom, fit_zoom)),
        }
    }
}
```

Register: `pub mod info;` in `develop/mod.rs`.

- [ ] **Step 4: Run test, verify pass.** Run: `cargo test -p ferrolite-app info::` — Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
cargo fmt
git add ferrolite-app/src/develop/info.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(develop): ImageFacts pure EXIF + live-zoom formatter"
```

### Task 9: Info overlay (toggle like histogram) + move ISO out of the status bar

**Files:**
- Modify: `ferrolite-app/src/settings/mod.rs` (`show_info_overlay: bool`, default `false`)
- Create: `ferrolite-app/src/develop/info_overlay.rs` (HUD painter) + `pub mod info_overlay;` in `develop/mod.rs`
- Modify: `ferrolite-app/src/app.rs` (draw where the histogram overlay is drawn ~4075; handle the toggle where `show_histogram` flips ~3243)
- Modify: `ferrolite-app/src/chrome/mod.rs` (toggle button beside the histogram toggle ~225)
- Modify: `ferrolite-app/src/settings/ui.rs` (checkbox beside the histogram one ~178)
- Modify: `ferrolite-app/src/status_bar.rs:118-119` (remove ISO)
- Modify: `ferrolite-app/src/icons.rs` (add a Phosphor `info` alias if none exists)
- Test: `settings/mod.rs` `tests`

**Interfaces:**
- Consumes: `develop::info::ImageFacts` (Task 8), `ViewerState.meta`, viewer `view.zoom` + fit zoom.
- Produces: `settings.show_info_overlay: bool`; `info_overlay::draw(ui, &ImageFacts)`.

- [ ] **Step 1: Add the setting + a default test.** In `settings/mod.rs` add `pub show_info_overlay: bool,` to `Settings` and `show_info_overlay: false,` to `Default`. Add to `tests`:

```rust
#[test]
fn info_overlay_defaults_off() {
    assert!(!Settings::default().show_info_overlay);
}
```

Run: `cargo test -p ferrolite-app info_overlay_defaults_off` — Expected: PASS (guards the field's existence; `#[serde(default)]` keeps old files loading).

- [ ] **Step 2: Add the overlay painter** `develop/info_overlay.rs`:

```rust
//! Compact read-only HUD of the key photographic facts + live zoom, toggled
//! like the histogram. Consumes only a formatted `ImageFacts`.

pub fn draw(ctx: &egui::Context, facts: &crate::develop::info::ImageFacts) {
    egui::Area::new("info_overlay".into())
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 12.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                for line in [&facts.focal, &facts.aperture, &facts.shutter, &facts.iso, &facts.zoom] {
                    if !line.is_empty() {
                        ui.label(line.as_str());
                    }
                }
            });
        });
}
```

Register: `pub mod info_overlay;` in `develop/mod.rs`.

- [ ] **Step 3: Draw the overlay from `app.rs`.** Find the block guarded by `if self.state.settings.show_histogram {` (~4075) and add a sibling:

```rust
                if self.state.settings.show_info_overlay {
                    if let Some(v) = self.state.viewer.as_ref() {
                        if let Some(meta) = v.meta.as_ref() {
                            let dims = v.image_dims();
                            let fit = ferrolite_vt::ViewTransform::fit(dims, v.last_canvas_size).zoom;
                            let facts = crate::develop::info::ImageFacts::build(
                                meta, v.view.zoom, fit, dims,
                            );
                            crate::develop::info_overlay::draw(ctx, &facts);
                        }
                    }
                }
```

Adapt `image_dims()` / `last_canvas_size` / `v.view.zoom` / `ctx` to the real accessors used near the histogram draw (read that block first).

- [ ] **Step 4: Add toggle + checkbox + chrome button.**
  - `chrome/mod.rs` (~225): add an info-overlay toggle button beside the histogram toggle; its icon MUST come from `icons.rs` (add a Phosphor `info` alias if none exists). Thread a `show_info_overlay: bool` param + returned toggle signal exactly like `show_histogram`/`histogram_checked`.
  - `settings/ui.rs` (~178): add `.checkbox(&mut settings.show_info_overlay, "Show info overlay")`.
  - `app.rs` (~3243): where `show_histogram` flips, handle the new signal: `self.state.settings.show_info_overlay = !self.state.settings.show_info_overlay;`.

- [ ] **Step 5: Remove ISO from the status bar.** In `status_bar.rs:118-119`, drop the `iso` binding and change the format string to:

```rust
            format!("{} · {}", img.filename, dims)
```

- [ ] **Step 6: Build + test + clippy.** Run: `cargo build -p ferrolite-app`, `cargo test -p ferrolite-app`, `cargo clippy -p ferrolite-app --all-targets -- -D warnings` — Expected: builds, passes, no warnings.

- [ ] **Step 7: Commit.**

```bash
cargo fmt
git add ferrolite-app/src/settings ferrolite-app/src/app.rs ferrolite-app/src/chrome/mod.rs ferrolite-app/src/status_bar.rs ferrolite-app/src/develop/info_overlay.rs ferrolite-app/src/develop/mod.rs ferrolite-app/src/icons.rs
git commit -m "feat(viewer): toggleable image-info overlay; move ISO from status bar"
```

### Task 10: Info tab in the Develop tab bar (closes overlay)

**Files:**
- Create: `ferrolite-app/src/develop/info_tab.rs` (`PanelTab` impl)
- Modify: `ferrolite-app/src/develop/base_tabs.rs` (register in `base_tabs()`)
- Modify: `ferrolite-app/src/develop/mod.rs` (`pub mod info_tab;`)
- Modify: the tab-click handler (`develop/tool_panel.rs` or `develop/mod.rs`) so activating Info sets `show_info_overlay = false`
- Test: `info_tab.rs` `tests`

**Interfaces:**
- Consumes: `develop::info::ImageFacts`, the `PanelTab` trait (`id`, `label`, `show(&self, ui, state) -> Option<EditOutcome>`).
- Produces: a base tab with `TabId("info")`.

- [ ] **Step 1: Write the failing test** in `info_tab.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::tool::{PanelTab, TabId};

    #[test]
    fn info_tab_identity() {
        assert_eq!(InfoTab.id(), TabId("info"));
        assert_eq!(InfoTab.label(), "Info");
    }
}
```

- [ ] **Step 2: Run it, verify it fails.** Run: `cargo test -p ferrolite-app info_tab_identity` — Expected: FAIL (no `InfoTab`).

- [ ] **Step 3: Implement the read-only tab** (mirror a `base_tabs.rs` `PanelTab` impl):

```rust
use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::tool::{PanelTab, TabId};
use crate::state::AppState;

pub struct InfoTab;

impl PanelTab for InfoTab {
    fn id(&self) -> TabId { TabId("info") }
    fn label(&self) -> &str { "Info" }
    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        if let Some(v) = state.viewer.as_ref() {
            if let Some(meta) = v.meta.as_ref() {
                let dims = v.image_dims();
                let fit = ferrolite_vt::ViewTransform::fit(dims, v.last_canvas_size).zoom;
                let facts = crate::develop::info::ImageFacts::build(meta, v.view.zoom, fit, dims);
                for (k, val) in [
                    ("Camera", &facts.camera), ("Lens", &facts.lens),
                    ("Focal", &facts.focal), ("Aperture", &facts.aperture),
                    ("Shutter", &facts.shutter), ("ISO", &facts.iso),
                    ("Captured", &facts.capture_time), ("Size", &facts.dimensions),
                    ("Zoom", &facts.zoom),
                ] {
                    if !val.is_empty() {
                        ui.horizontal(|ui| { ui.label(k); ui.label(val.as_str()); });
                    }
                }
            } else {
                ui.label("No metadata available.");
            }
        }
        None // read-only: never produces an edit
    }
}
```

Adapt the viewer accessors to the real names. Register `pub mod info_tab;` in `develop/mod.rs`, and add `InfoTab` to `base_tabs()` in `base_tabs.rs` so it appears in the base tab bar.

- [ ] **Step 4: Close the overlay when Info becomes active.** `ToolState::select_tab` is pure and can't touch settings, so do this at the tab-click call site. Read the tab-bar click handler (`develop/tool_panel.rs` or `develop/mod.rs`) and, next to its `select_tab` call, add:

```rust
            if clicked_tab == TabId("info") {
                state.settings.show_info_overlay = false;
            }
```

(Use the variable that holds the clicked tab id in that handler.)

- [ ] **Step 5: Run test + build.** Run: `cargo test -p ferrolite-app info_tab` then `cargo build -p ferrolite-app` — Expected: PASS/builds.

- [ ] **Step 6: Commit.**

```bash
cargo fmt
git add ferrolite-app/src/develop/info_tab.rs ferrolite-app/src/develop/base_tabs.rs ferrolite-app/src/develop/mod.rs ferrolite-app/src/develop/tool_panel.rs
git commit -m "feat(develop): read-only Info tab; opening it closes the info overlay"
```

---

## Final workspace gate

- [ ] **Run the full gate.**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three MUST be green. Per CLAUDE.md, green is necessary but NOT sufficient — hand the author the visual test plan below and HOLD for hands-on feedback before finishing the branch.

## Visual test plan (for the author, Jann)

1. **Not-seen filter** (Library toolbar + Develop filter strip): toggle the new neutral flag → grid shows only images with no Pick/Reject. Combine with Pick to see both. Failure: no effect, or the glyph renders as tofu.
2. **Zoom hotkeys** (loupe): press **F** → image fits; scroll to zoom in, press **Z** → 100%. Rebind both in Settings ▸ Keyboard and confirm Help panel + control tooltips show the new keys. Failure: no zoom change, or Help/Settings missing the actions.
3. **Tool/tab persistence**: open an image, switch to a Color/tool tab, navigate next/prev → the same tab stays active. Failure: snaps back to Light/Adjust.
4. **Remove from collection**: open a collection, right-click an image → "Remove from this collection" + "Remove from collection ▸"; the image disappears from the collection view. Right-click an image already in collection X → "Add to collection" no longer lists X. Failure: X still listed, or removal doesn't refresh the view.
5. **Info overlay**: toggle it (chrome button / Settings). Key facts (focal + eq, aperture, shutter, ISO, live zoom %) show; zoom % updates as you zoom. Confirm ISO is gone from the bottom status bar. Failure: overlay empty, ISO still in status bar, or zoom % static.
6. **Info tab**: open the Develop "Info" tab → all facts listed; the overlay auto-closes. Use a non-full-frame RAW to confirm the 35mm-equiv line; use a file lacking the tag to confirm raw focal length only. Failure: overlay stays open alongside the tab, or equiv shown when EXIF lacks it.

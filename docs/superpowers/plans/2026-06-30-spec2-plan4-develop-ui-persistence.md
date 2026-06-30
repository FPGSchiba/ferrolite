# Spec 2 — Plan 4: Develop UI + persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Develop 296px right adjustment panel (Basic / Tone Curve / HSL / Detail / Geometry), interactive tone-curve + HSL + crop-overlay widgets, undo/redo + per-section reset, before/after toggle, and `frl:ops` sidecar read-on-open + off-thread write + a rebuildable `images.has_edits` badge — wiring slider → new `OpStack` → mark node dirty → repaint at both preview-res (interactive) and full-res (1:1 tiled).

**Architecture:** The pure document model (`OpStack`), the GPU `EditPipeline` (preview-res) and `TileEditPipeline` (full-res producer), the VT sparse-tile producer seam, and the `frl:`-namespace serializer already exist (Plans 1–3, green). This plan adds (a) the egui adjustment UI that emits a new immutable `OpStack` per interaction; (b) the **interactive preview-tier display wiring** that was never connected — on each edit, `EditPipeline::set_stack` + `evaluate` produces a new `PipelineImage` whose texture replaces the rung-1 single-texture the viewer paints; (c) the full-res producer update (color ops via `set_stack`, geometry/halo via rebuild) + opstack-version bump to invalidate cached edited tiles; (d) `frl:ops` sidecar persistence (off-thread write, read-on-open) co-located with `xmp:Rating`, merge-preserving; (e) the rebuildable `images.has_edits` cache column + filmstrip badge. Pure logic (point-curve math, crop hit-testing, undo/redo, op-building, serialization, has_edits) is unit-tested; egui rendering and GPU display wiring are verified by build + clippy + the author's hands-on visual test (spec §10).

**Tech Stack:** Rust, wgpu/`ferrolite-gpu` (`Graph<PipelineImage>`), `ferrolite-vt` (sparse VT + `TileProducer`), egui/eframe, `ferrolite-pipeline` (`OpStack`/`Op`/`EditPipeline`/`TileEditPipeline`/`serialize`), `ferrolite-catalog` (rusqlite **pinned 0.32** — do not bump, see memory `rusqlite-pinned-0-32`), quick-xml (sidecar), `ferrolite-jobs` (off-thread I/O).

## Global Constraints

Every task's requirements implicitly include this section. Values copied verbatim from the spec + CLAUDE.md:

- **Nothing slow on the UI thread.** Sidecar read/write and DB writes go to `ferrolite-jobs` (priority + cancellation) and report back over the `AppEvent` channel, then `ctx.request_repaint()`. Filmstrip stays virtualized (visible cells only). (CLAUDE.md §1)
- **GPU pipelines are built once and reused — never rebuilt per image/edit/frame.** The preview `EditPipeline` is built **once per opened image** and reused via `set_stack` for every subsequent edit. The full-res `TileEditPipeline` producer is updated via `set_stack` for color ops; it is **rebuilt only** when geometry (`stack.geometry()`) or the sharpen halo (`sharpen_halo(stack.sharpen())`) changes (its documented `set_stack` limitation). (CLAUDE.md §2)
- **Sidecar is the source of truth; the catalog is a cache.** `frl:ops` in the `.xmp` sidecar is authoritative; `images.has_edits` is a derived BOOLEAN cache, rebuildable by re-reading sidecars (a missing DB never loses edits). (spec §5 contract §2, §7)
- **Edits are non-destructive.** The original image file is never written; the op stack is the only persisted edit state. (spec §1)
- **Merge-preserving sidecar.** `frl:ops` writes preserve `xmp:Rating` **and** all foreign nodes (incl. `crs:`); a malformed sidecar is backed up to `.xmp.bak` then rewritten fresh; unknown/absent payload → `OpStack::default()` (unedited). Never panics. (spec §7, §9)
- **Two-tier recompute.** Preview-res is the interactive surface (single ~fit-res texture); full-res tiled editing is for 1:1 inspection only and streams lazily at `Visible` priority. (spec §6)
- **Licensing tiers preserved.** `ferrolite-vt`'s new display seam carries **no photo concepts** (it takes a `wgpu::Texture`); `ferrolite-pipeline` is photo-tier. (spec §3)
- **Testing:** pure CPU logic gets unit tests (80%+ target, run headless on every OS). egui rendering + GPU display wiring: `cargo build` + clippy + the author's visual test — **no golden/unit tests for egui** (spec §10).
- **Gate (necessary, not sufficient):** `cargo fmt --check` && `cargo clippy --workspace --all-targets -- -D warnings` && `cargo test --workspace` green → **then STOP and hold for the author's (Jann's) visual test** before finishing the branch (CLAUDE.md "Finishing a branch" rule).

---

## File Structure

**New files:**
- `ferrolite-app/src/develop/mod.rs` — Develop-module submodule root (declares the below).
- `ferrolite-app/src/develop/ops_edit.rs` — **pure**: map a UI value → a new `OpStack` (identity value removes the op). Unit-tested.
- `ferrolite-app/src/develop/curve_math.rs` — **pure**: tone-curve point insert/move/delete/clamp/sort/hit-test/identity. Unit-tested.
- `ferrolite-app/src/develop/crop_math.rs` — **pure**: crop handle enum + hit-test + resize-with-aspect + move + rotate-angle + aspect-ratio. Unit-tested.
- `ferrolite-app/src/develop/history.rs` — **pure**: bounded undo/redo `OpStack` ring with same-kind coalescing. Unit-tested.
- `ferrolite-app/src/develop/ops_persist.rs` — off-thread `frl:ops` write + read jobs (mirror `metadata.rs`).
- `ferrolite-app/src/develop/adjustment_panel.rs` — the 296px right panel (sections + sliders + resets). egui (visual-tested).
- `ferrolite-app/src/develop/curve_widget.rs` — interactive tone-curve widget. egui (visual-tested).
- `ferrolite-app/src/develop/hsl_widget.rs` — 8-band swatch row + per-band sliders. egui (visual-tested).
- `ferrolite-app/src/develop/crop_overlay.rs` — canvas crop overlay (routes pointer into `crop_math`). egui (visual-tested).

**Modified files:**
- `ferrolite-catalog/src/xmp.rs` — add `read_ops` / `write_ops` (`frl:ops`, merge-preserving). Pure-tested.
- `ferrolite-catalog/src/lib.rs` — `pub use xmp::{read_ops, write_ops}`.
- `ferrolite-catalog/src/schema.rs` — schema v4: `images.has_edits`.
- `ferrolite-catalog/src/catalog.rs` — `set_has_edits`.
- `ferrolite-catalog/src/queries.rs` — `IMAGE_COLS` + `row_to_record` read `has_edits`.
- `ferrolite-catalog/src/model.rs` — `ImageRecord.has_edits: bool`.
- `ferrolite-vt/src/view.rs` — `SingleResources.texture: Arc<wgpu::Texture>` + `update_single_from_texture`.
- `ferrolite-app/src/viewer/mod.rs` — `ViewerState` edit fields + helpers.
- `ferrolite-app/src/viewer/load.rs` (or `develop/ops_persist.rs`) — `spawn_ops_read`.
- `ferrolite-app/src/events.rs` — `AppEvent::OpsLoaded`.
- `ferrolite-app/src/app.rs` — build/reuse preview `EditPipeline`; apply-edit wiring; right panel; crop overlay; before/after + undo/redo keys; read-on-open; retain pyramid for rebuild.
- `ferrolite-app/src/lib.rs` — `mod develop;`.
- `ferrolite-app/src/library/filmstrip.rs` — `has_edits` badge.
- `ferrolite-app/src/state.rs`, `ferrolite-app/src/metadata.rs` — update `ImageRecord` struct-literal test helpers.

---

## Task 1: `frl:ops` sidecar read/write (merge-preserving)

Extend `xmp.rs` to carry an opaque `frl:ops` JSON payload alongside `xmp:Rating`, preserving foreign nodes and the rating. The catalog stays free of any `ferrolite-pipeline` dependency: the payload is an opaque `&str` (the app does `serialize`/`deserialize`).

**Files:**
- Modify: `ferrolite-catalog/src/xmp.rs`
- Modify: `ferrolite-catalog/src/lib.rs:24`
- Test: `ferrolite-catalog/src/xmp.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `pub fn read_ops(xmp_path: &Path) -> Option<String>` — the `frl:ops` payload string, or `None` if absent/parse-error.
  - `pub fn write_ops(xmp_path: &Path, ops_payload: &str) -> Result<(), CatalogError>` — sets `frl:ops` (+ `xmlns:frl`) as an attribute on the first `rdf:Description`, preserving `xmp:Rating` and all foreign nodes; absent → fresh template; malformed → `.xmp.bak` + fresh.
- Consumes: existing `sidecar_path`, `sidecar_bak`, `CatalogError`, quick-xml `Reader`/`Writer`.

- [ ] **Step 1: Write the failing tests** (append to `xmp.rs` `mod tests`)

```rust
    const FRL_NS_DECL: &str = "xmlns:frl=\"http://ns.ferrolite.app/1.0/\"";

    #[test]
    fn writes_fresh_ops_sidecar_when_absent() {
        let dir = std::env::temp_dir().join(format!("frl-ops-new-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("o.xmp");
        let _ = std::fs::remove_file(&p);
        write_ops(&p, r#"{"version":1,"ops":[]}"#).unwrap();
        assert_eq!(read_ops(&p).as_deref(), Some(r#"{"version":1,"ops":[]}"#));
    }

    #[test]
    fn write_ops_preserves_rating_and_foreign_nodes() {
        let dir = std::env::temp_dir().join(format!("frl-ops-merge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("o2.xmp");
        std::fs::write(
            &p,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
                 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                   <rdf:Description rdf:about=""
                     xmlns:xmp="http://ns.adobe.com/xap/1.0/"
                     xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
                     xmp:Rating="3" crs:Exposure2012="+0.50"/>
                 </rdf:RDF>
               </x:xmpmeta>"#,
        )
        .unwrap();
        write_ops(&p, r#"{"version":1,"ops":[{"Exposure":{"ev":0.5}}]}"#).unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("crs:Exposure2012"), "foreign attr preserved");
        assert_eq!(read_rating(&p), Some(Rating::new(3)), "rating preserved");
        assert_eq!(
            read_ops(&p).as_deref(),
            Some(r#"{"version":1,"ops":[{"Exposure":{"ev":0.5}}]}"#)
        );
    }

    #[test]
    fn ops_and_rating_writers_coexist_either_order() {
        let dir = std::env::temp_dir().join(format!("frl-ops-coexist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("o3.xmp");
        let _ = std::fs::remove_file(&p);
        write_ops(&p, r#"{"version":1,"ops":[]}"#).unwrap();
        write_rating(&p, Rating::new(5)).unwrap(); // must not clobber frl:ops
        assert_eq!(read_rating(&p), Some(Rating::new(5)));
        assert_eq!(read_ops(&p).as_deref(), Some(r#"{"version":1,"ops":[]}"#));
    }

    #[test]
    fn write_ops_backs_up_malformed_then_writes_fresh() {
        let dir = std::env::temp_dir().join(format!("frl-ops-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("o4.xmp");
        std::fs::write(&p, "<broken <<").unwrap();
        write_ops(&p, r#"{"version":1,"ops":[]}"#).unwrap();
        assert!(dir.join("o4.xmp.bak").exists(), "malformed original backed up");
        assert_eq!(read_ops(&p).as_deref(), Some(r#"{"version":1,"ops":[]}"#));
    }

    #[test]
    fn read_ops_missing_or_malformed_is_none() {
        assert_eq!(read_ops(Path::new("/no/such.xmp")), None);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-catalog xmp::tests::write_ops -- --nocapture`
Expected: FAIL with "cannot find function `read_ops`/`write_ops`".

- [ ] **Step 3: Implement `read_ops` / `write_ops`** (add to `xmp.rs`, near the rating fns)

```rust
const OPS_LOCAL: &[u8] = b"frl:ops";
const FRL_NS: &str = "http://ns.ferrolite.app/1.0/";

/// Read the `frl:ops` attribute payload (any element). Lenient: parse error or
/// missing file yields `None`.
pub fn read_ops(xmp_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(xmp_path).ok()?;
    let mut reader = Reader::from_str(&text);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == OPS_LOCAL {
                        return Some(attr.unescape_value().ok()?.into_owned());
                    }
                }
            }
            Err(_) => return None,
            _ => {}
        }
    }
    None
}

/// Build a copy of `src` (an `rdf:Description` open/empty tag) with `frl:ops` set/
/// replaced as an attribute, ensuring the `frl:` namespace decl is present; every
/// other attribute (incl. `xmp:Rating`, foreign `crs:`) is preserved verbatim.
fn description_with_ops(src: &BytesStart<'_>, ops: &str) -> BytesStart<'static> {
    let mut out = BytesStart::new(String::from_utf8_lossy(src.name().as_ref()).into_owned());
    let mut has_frl_ns = false;
    for attr in src.attributes().flatten() {
        if attr.key.as_ref() == OPS_LOCAL {
            continue; // replace
        }
        if attr.key.as_ref() == b"xmlns:frl" {
            has_frl_ns = true;
        }
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let val = String::from_utf8_lossy(&attr.value).into_owned();
        out.push_attribute((key.as_str(), val.as_str()));
    }
    if !has_frl_ns {
        out.push_attribute(("xmlns:frl", FRL_NS));
    }
    out.push_attribute(("frl:ops", ops));
    out
}

fn fresh_sidecar_ops(ops: &str) -> String {
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
         \x20<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
         \x20\x20<rdf:Description rdf:about=\"\" xmlns:frl=\"{FRL_NS}\" frl:ops=\"{ops}\"/>\n\
         \x20</rdf:RDF>\n\
         </x:xmpmeta>\n\
         <?xpacket end=\"w\"?>\n",
        // `ops` is JSON; quick-xml is not used for the fresh template, so escape
        // the five XML attribute-significant chars here.
        ops = xml_attr_escape(ops),
        FRL_NS = FRL_NS,
    )
}

/// Minimal XML attribute escaping for the hand-built fresh template.
fn xml_attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn rewrite_with_ops(text: &str, ops: &str) -> Option<Vec<u8>> {
    let mut reader = Reader::from_str(text);
    let mut events: Vec<Event<'static>> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(ev) => events.push(ev.into_owned()),
            Err(_) => return None,
        }
    }
    let mut writer = Writer::new(Vec::new());
    let mut done = false;
    for ev in events {
        match ev {
            Event::Start(ref e) if !done && e.name().as_ref() == b"rdf:Description" => {
                writer
                    .write_event(Event::Start(description_with_ops(e, ops)))
                    .ok()?;
                done = true;
            }
            Event::Empty(ref e) if !done && e.name().as_ref() == b"rdf:Description" => {
                writer
                    .write_event(Event::Empty(description_with_ops(e, ops)))
                    .ok()?;
                done = true;
            }
            other => {
                writer.write_event(other).ok()?;
            }
        }
    }
    if !done {
        return None;
    }
    Some(writer.into_inner())
}

/// Write the `frl:ops` payload into `xmp_path`, preserving `xmp:Rating` + foreign
/// nodes. Absent → fresh template; parse error → `.xmp.bak` + fresh template.
pub fn write_ops(xmp_path: &Path, ops_payload: &str) -> Result<(), CatalogError> {
    match std::fs::read_to_string(xmp_path) {
        Ok(text) => match rewrite_with_ops(&text, ops_payload) {
            Some(bytes) => std::fs::write(xmp_path, bytes)?,
            None => {
                let bak = sidecar_bak(xmp_path);
                let _ = std::fs::rename(xmp_path, &bak);
                std::fs::write(xmp_path, fresh_sidecar_ops(ops_payload))?;
            }
        },
        Err(_) => std::fs::write(xmp_path, fresh_sidecar_ops(ops_payload))?,
    }
    Ok(())
}
```

Note: `rewrite_with_ops` writes the `frl:ops` attribute via quick-xml's `Writer`, which escapes attribute values itself; only the hand-built `fresh_sidecar_ops` needs `xml_attr_escape`.

- [ ] **Step 4: Add the public re-export** — `ferrolite-catalog/src/lib.rs:24`

```rust
pub use xmp::{read_ops, read_rating, sidecar_path, write_ops, write_rating};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-catalog xmp::tests -- --nocapture`
Expected: PASS (all rating tests still green + the five new ops tests).

- [ ] **Step 6: Commit**

```bash
git add ferrolite-catalog/src/xmp.rs ferrolite-catalog/src/lib.rs
git commit -m "feat(catalog): frl:ops sidecar read/write, merge-preserving alongside xmp:Rating"
```

---

## Task 2: `images.has_edits` cache column

A rebuildable BOOLEAN cache so the grid/filmstrip show an "edited" badge without parsing every sidecar (spec §7). Schema v4 + setter + read into `ImageRecord`.

**Files:**
- Modify: `ferrolite-catalog/src/schema.rs:4` (version), `:88` (add a `version < 4` block)
- Modify: `ferrolite-catalog/src/catalog.rs` (add `set_has_edits`)
- Modify: `ferrolite-catalog/src/queries.rs:10-33` (`row_to_record` + `IMAGE_COLS`)
- Modify: `ferrolite-catalog/src/model.rs:50` (`ImageRecord.has_edits`)
- Modify (test literals): `ferrolite-app/src/metadata.rs:100`, `ferrolite-app/src/state.rs:726`, `ferrolite-app/src/state.rs:862`
- Test: `ferrolite-catalog/src/schema.rs`, `ferrolite-catalog/src/catalog.rs`

**Interfaces:**
- Produces:
  - `ImageRecord.has_edits: bool` (new field, last in the struct).
  - `Catalog::set_has_edits(&self, image_id: i64, has_edits: bool) -> Result<(), CatalogError>`.
  - `SCHEMA_VERSION == 4`.

- [ ] **Step 1: Write the failing tests**

In `schema.rs` `mod tests` extend `migrate_creates_v3_shape` (rename mentally to v4) — add after the existing `flag`/`added_at` asserts:

```rust
        assert_eq!(super::SCHEMA_VERSION, 4);
        assert!(img.contains(&"has_edits".to_string()), "has_edits column added");
```

In `catalog.rs` `mod tests` (or a new test) add:

```rust
    #[test]
    fn set_has_edits_roundtrips() {
        let dir = std::env::temp_dir().join(format!("frl-hasedits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Catalog::open(&dir.join("c.db")).unwrap();
        let folder = db.upsert_folder(std::path::Path::new("/p"), None).unwrap();
        let id = db
            .upsert_image(&crate::NewImage::failed(
                folder,
                "a.nef".into(),
                1,
                1,
                ferrolite_image::FileKind::Raw,
                0,
            ))
            .unwrap();
        db.set_has_edits(id, true).unwrap();
        let rec = db.list_images(folder).unwrap().into_iter().find(|r| r.id == id).unwrap();
        assert!(rec.has_edits, "has_edits read back true");
        db.set_has_edits(id, false).unwrap();
        let rec = db.list_images(folder).unwrap().into_iter().find(|r| r.id == id).unwrap();
        assert!(!rec.has_edits, "has_edits read back false");
    }
```

(Confirm `Catalog::list_images`/`upsert_image`/`upsert_folder` signatures by reading `catalog.rs`; mirror the existing `set_rating` test if a closer pattern exists.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p ferrolite-catalog schema::tests catalog::tests::set_has_edits`
Expected: FAIL (`SCHEMA_VERSION` is 3 / no `has_edits` field / no `set_has_edits`).

- [ ] **Step 3: Bump schema + add migration** — `schema.rs`

```rust
pub const SCHEMA_VERSION: i64 = 4;
```

After the `if version < 3 { … version = 3; }` block, before the `debug_assert_eq!`:

```rust
    if version < 4 {
        conn.execute_batch(
            "ALTER TABLE images ADD COLUMN has_edits INTEGER NOT NULL DEFAULT 0;",
        )?;
        version = 4;
    }
```

- [ ] **Step 4: Add `has_edits` to the model + read path**

`model.rs` — add as the last field of `ImageRecord`:

```rust
    pub flag: Flag,
    /// Cache of "has a non-identity frl:ops stack" (rebuildable from the sidecar).
    pub has_edits: bool,
```

`queries.rs` — `IMAGE_COLS` append `, has_edits`; in `row_to_record` read index 12 and set the field:

```rust
pub(crate) const IMAGE_COLS: &str = "id, folder_id, filename, width, height, orientation,
                          capture_time, iso, decode_status, kind, rating, flag, has_edits";
```

```rust
    let flag: i64 = row.get(11)?;
    let has_edits: i64 = row.get(12)?;
    Ok(ImageRecord {
        // … existing fields …
        flag: Flag::from_i64(flag),
        has_edits: has_edits != 0,
    })
```

`catalog.rs` — add next to `set_flag`:

```rust
    /// Update the cached `has_edits` flag for an image row.
    pub fn set_has_edits(&self, image_id: i64, has_edits: bool) -> Result<(), CatalogError> {
        self.conn().execute(
            "UPDATE images SET has_edits=?1 WHERE id=?2",
            rusqlite::params![has_edits as i64, image_id],
        )?;
        Ok(())
    }
```

- [ ] **Step 5: Fix the `ImageRecord` struct-literal test helpers** (compile breakage)

Add `has_edits: false,` as the last field in each literal:
- `ferrolite-app/src/metadata.rs:100` (`rec()`),
- `ferrolite-app/src/state.rs:726` (`mk_rec`),
- `ferrolite-app/src/state.rs:862` (`mk`).

- [ ] **Step 6: Run the catalog + app tests**

Run: `cargo test -p ferrolite-catalog && cargo test -p ferrolite-app`
Expected: PASS (migration shape asserts v4 + `has_edits`; setter round-trips; app helpers compile).

- [ ] **Step 7: Commit**

```bash
git add ferrolite-catalog/src ferrolite-app/src/metadata.rs ferrolite-app/src/state.rs
git commit -m "feat(catalog): add rebuildable images.has_edits cache column (schema v4)"
```

---

## Task 3: Pure op-building helpers (`develop/ops_edit.rs`)

Map a UI value to a new immutable `OpStack`, **removing** the op when the value is at its identity default so `is_identity()` (and therefore `has_edits`) stays correct and the stack stays minimal.

**Files:**
- Create: `ferrolite-app/src/develop/mod.rs`
- Create: `ferrolite-app/src/develop/ops_edit.rs`
- Modify: `ferrolite-app/src/lib.rs` (add `mod develop;`)
- Test: in `ops_edit.rs`

**Interfaces:**
- Consumes: `ferrolite_pipeline::{OpStack, Op, Exposure, WhiteBalance, Contrast, Sharpen, OpKind}`.
- Produces (all pure, take `&OpStack`, return a new `OpStack`):
  - `pub fn set_exposure(s: &OpStack, ev: f32) -> OpStack`
  - `pub fn set_white_balance(s: &OpStack, temp: f32, tint: f32) -> OpStack`
  - `pub fn set_contrast(s: &OpStack, amount: f32) -> OpStack`
  - `pub fn set_sharpen(s: &OpStack, amount: f32, radius: u32) -> OpStack`
  - `pub fn needs_full_rebuild(old: &OpStack, new: &OpStack) -> bool` (geometry or sharpen-halo change)

- [ ] **Step 1: Create the module root** — `ferrolite-app/src/develop/mod.rs`

```rust
//! Develop module: the right adjustment panel, its interactive widgets, the
//! op-stack edit helpers + undo/redo history, and off-thread frl:ops persistence.

pub mod crop_math;
pub mod curve_math;
pub mod history;
pub mod ops_edit;
pub mod ops_persist;

// egui widgets (visual-tested; no unit tests):
pub mod adjustment_panel;
pub mod crop_overlay;
pub mod curve_widget;
pub mod hsl_widget;
```

Add `mod develop;` to `ferrolite-app/src/lib.rs` (next to the other `mod` lines). (The widget submodules are added in later tasks; comment them out here and uncomment per task, or create empty stubs to keep the build green between tasks.)

- [ ] **Step 2: Write the failing tests** — `ops_edit.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{Op, OpStack};

    #[test]
    fn set_exposure_adds_then_identity_removes() {
        let s = set_exposure(&OpStack::default(), 0.5);
        assert_eq!(s.exposure().unwrap().ev, 0.5);
        let s2 = set_exposure(&s, 0.0);
        assert!(s2.exposure().is_none(), "identity ev removes the op");
        assert!(s2.is_identity());
    }

    #[test]
    fn set_white_balance_identity_when_both_zero() {
        let s = set_white_balance(&OpStack::default(), 0.0, 0.0);
        assert!(s.white_balance().is_none());
    }

    #[test]
    fn set_sharpen_identity_when_amount_zero() {
        let s = set_sharpen(&OpStack::default(), 0.0, 3);
        assert!(s.sharpen().is_none(), "zero amount = no sharpen");
        let s = set_sharpen(&OpStack::default(), 0.4, 2);
        assert_eq!(s.sharpen(), Some(ferrolite_pipeline::Sharpen { amount: 0.4, radius: 2 }));
    }

    #[test]
    fn needs_full_rebuild_on_geometry_and_halo_only() {
        let base = set_exposure(&OpStack::default(), 0.5);
        let color_only = set_contrast(&base, 0.3);
        assert!(!needs_full_rebuild(&base, &color_only), "color ops: no rebuild");
        let sharper = set_sharpen(&base, 0.5, 5);
        assert!(needs_full_rebuild(&base, &sharper), "halo change: rebuild");
        let geo = base.set_op(Op::Geometry(ferrolite_pipeline::Geometry {
            crop: ferrolite_pipeline::CropRect::full(),
            angle_deg: 5.0,
            aspect: ferrolite_pipeline::Aspect::Free,
        }));
        assert!(needs_full_rebuild(&base, &geo), "geometry change: rebuild");
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p ferrolite-app develop::ops_edit`
Expected: FAIL (functions not defined).

- [ ] **Step 4: Implement** — `ops_edit.rs` (top of file)

```rust
//! Pure helpers: map a UI value to a new immutable `OpStack`. A value at its
//! identity default REMOVES the op so `is_identity()`/`has_edits` stay correct.

use ferrolite_pipeline::{
    sharpen_halo, Contrast, Exposure, Op, OpStack, Sharpen, WhiteBalance,
};

pub fn set_exposure(s: &OpStack, ev: f32) -> OpStack {
    if ev == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::Exposure)
    } else {
        s.set_op(Op::Exposure(Exposure { ev }))
    }
}

pub fn set_white_balance(s: &OpStack, temp: f32, tint: f32) -> OpStack {
    if temp == 0.0 && tint == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::WhiteBalance)
    } else {
        s.set_op(Op::WhiteBalance(WhiteBalance { temp, tint }))
    }
}

pub fn set_contrast(s: &OpStack, amount: f32) -> OpStack {
    if amount == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::Contrast)
    } else {
        s.set_op(Op::Contrast(Contrast { amount }))
    }
}

pub fn set_sharpen(s: &OpStack, amount: f32, radius: u32) -> OpStack {
    if amount == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::Sharpen)
    } else {
        s.set_op(Op::Sharpen(Sharpen { amount, radius }))
    }
}

/// The full-res `TileEditPipeline` bakes geometry + the sharpen halo at
/// construction; only a change to either requires discarding + rebuilding it.
/// Color-only changes are applied via `TileEditPipeline::set_stack`.
pub fn needs_full_rebuild(old: &OpStack, new: &OpStack) -> bool {
    old.geometry() != new.geometry()
        || sharpen_halo(old.sharpen()) != sharpen_halo(new.sharpen())
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ferrolite-app develop::ops_edit`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/develop/mod.rs ferrolite-app/src/develop/ops_edit.rs ferrolite-app/src/lib.rs
git commit -m "feat(app): pure op-stack edit helpers + full-res rebuild predicate"
```

---

## Task 4: Tone-curve point math (`develop/curve_math.rs`)

Pure control-point editing for the interactive curve widget. Points are `(f32, f32)` in `[0,1]×[0,1]`, x-ascending, with fixed endpoints at x=0 and x=1.

**Files:**
- Create: `ferrolite-app/src/develop/curve_math.rs`
- Test: in `curve_math.rs`

**Interfaces:**
- Produces (pure):
  - `pub fn identity_points() -> Vec<(f32, f32)>` → `vec![(0.0,0.0),(1.0,1.0)]`
  - `pub fn is_identity(points: &[(f32, f32)]) -> bool`
  - `pub fn nearest_point(points: &[(f32, f32)], target: (f32, f32), max_dist: f32) -> Option<usize>`
  - `pub fn insert_point(points: &[(f32, f32)], p: (f32, f32)) -> Vec<(f32, f32)>`
  - `pub fn move_point(points: &[(f32, f32)], idx: usize, p: (f32, f32)) -> Vec<(f32, f32)>`
  - `pub fn delete_point(points: &[(f32, f32)], idx: usize) -> Vec<(f32, f32)>`
- Consumed by: `curve_widget` (Task 12) and `ops_edit`/panel via `Op::ToneCurve { points }`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-5 && (a.1 - b.1).abs() < 1e-5
    }

    #[test]
    fn identity_is_two_corner_points() {
        assert!(is_identity(&identity_points()));
        assert!(is_identity(&[]), "empty is identity too");
        assert!(!is_identity(&[(0.0, 0.0), (0.5, 0.7), (1.0, 1.0)]));
    }

    #[test]
    fn insert_keeps_x_sorted_and_clamps() {
        let pts = insert_point(&identity_points(), (1.5, -0.2));
        assert!(pts.windows(2).all(|w| w[0].0 <= w[1].0), "x ascending");
        assert!(pts.iter().all(|p| (0.0..=1.0).contains(&p.0) && (0.0..=1.0).contains(&p.1)));
    }

    #[test]
    fn nearest_finds_within_radius_else_none() {
        let pts = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        assert_eq!(nearest_point(&pts, (0.52, 0.48), 0.1), Some(1));
        assert_eq!(nearest_point(&pts, (0.3, 0.9), 0.05), None);
    }

    #[test]
    fn move_interior_clamps_between_neighbors() {
        let pts = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        // Try to drag the middle point past the right endpoint in x.
        let moved = move_point(&pts, 1, (1.4, 0.8));
        assert!(moved[1].0 < moved[2].0, "x stays left of the right neighbor");
        assert!(moved[1].0 > moved[0].0, "x stays right of the left neighbor");
    }

    #[test]
    fn move_endpoints_keep_x_fixed() {
        let pts = identity_points();
        let m0 = move_point(&pts, 0, (0.3, 0.4));
        assert!(approx((m0[0].0, m0[0].1), (0.0, 0.4)), "left endpoint x pinned at 0");
        let last = pts.len() - 1;
        let m1 = move_point(&pts, last, (0.7, 0.2));
        assert!(approx((m1[last].0, m1[last].1), (1.0, 0.2)), "right endpoint x pinned at 1");
    }

    #[test]
    fn delete_keeps_endpoints() {
        let pts = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        assert_eq!(delete_point(&pts, 1).len(), 2, "interior deletable");
        assert_eq!(delete_point(&pts, 0).len(), 3, "left endpoint not deletable");
        assert_eq!(delete_point(&pts, 2).len(), 3, "right endpoint not deletable");
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p ferrolite-app develop::curve_math` → FAIL.

- [ ] **Step 3: Implement**

```rust
//! Pure tone-curve control-point editing (normalized [0,1] space, x-ascending,
//! endpoints pinned at x=0 and x=1). Routed by `curve_widget`.

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

pub fn identity_points() -> Vec<(f32, f32)> {
    vec![(0.0, 0.0), (1.0, 1.0)]
}

pub fn is_identity(points: &[(f32, f32)]) -> bool {
    points.is_empty()
        || (points.len() == 2
            && (points[0].0 - 0.0).abs() < 1e-6
            && (points[0].1 - 0.0).abs() < 1e-6
            && (points[1].0 - 1.0).abs() < 1e-6
            && (points[1].1 - 1.0).abs() < 1e-6)
}

pub fn nearest_point(points: &[(f32, f32)], target: (f32, f32), max_dist: f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, p) in points.iter().enumerate() {
        let d = ((p.0 - target.0).powi(2) + (p.1 - target.1).powi(2)).sqrt();
        if d <= max_dist && best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

pub fn insert_point(points: &[(f32, f32)], p: (f32, f32)) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = points.to_vec();
    out.push((clamp01(p.0), clamp01(p.1)));
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

pub fn move_point(points: &[(f32, f32)], idx: usize, p: (f32, f32)) -> Vec<(f32, f32)> {
    let mut out = points.to_vec();
    if idx >= out.len() {
        return out;
    }
    let y = clamp01(p.1);
    let last = out.len() - 1;
    let x = if idx == 0 {
        0.0
    } else if idx == last {
        1.0
    } else {
        // Keep strictly between neighbors so x stays ascending.
        let lo = out[idx - 1].0 + 1e-4;
        let hi = out[idx + 1].0 - 1e-4;
        clamp01(p.0).clamp(lo, hi)
    };
    out[idx] = (x, y);
    out
}

pub fn delete_point(points: &[(f32, f32)], idx: usize) -> Vec<(f32, f32)> {
    // Endpoints (first/last) are not deletable.
    if idx == 0 || idx + 1 >= points.len() {
        return points.to_vec();
    }
    let mut out = points.to_vec();
    out.remove(idx);
    out
}
```

- [ ] **Step 4: Run to verify pass** — `cargo test -p ferrolite-app develop::curve_math` → PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/develop/curve_math.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(app): pure tone-curve control-point math"
```

---

## Task 5: Crop-overlay math (`develop/crop_math.rs`)

Pure geometry for the canvas crop overlay: which handle is under the pointer, resize with optional aspect constraint, move the body, and the rotate-handle angle. Works in image-normalized `[0,1]` space (`CropRect`), independent of egui.

**Files:**
- Create: `ferrolite-app/src/develop/crop_math.rs`
- Test: in `crop_math.rs`

**Interfaces:**
- Consumes: `ferrolite_pipeline::{CropRect, Aspect}`.
- Produces (pure):
  - `pub enum Handle { TopLeft, Top, TopRight, Right, BottomRight, Bottom, BottomLeft, Left, Body }`
  - `pub fn hit_test(crop: CropRect, pos: (f32, f32), handle_r: f32) -> Option<Handle>`
  - `pub fn resize(crop: CropRect, handle: Handle, pos: (f32, f32), aspect: Option<f32>) -> CropRect`
  - `pub fn move_body(crop: CropRect, delta: (f32, f32)) -> CropRect`
  - `pub fn rotate_angle(center: (f32, f32), pos: (f32, f32)) -> f32` (degrees)
  - `pub fn aspect_ratio(aspect: Aspect, img_w: u32, img_h: u32) -> Option<f32>` (w/h; `Free`→None)

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{Aspect, CropRect};

    fn full() -> CropRect { CropRect::full() }

    #[test]
    fn hit_test_corners_and_body() {
        let c = CropRect { x: 0.2, y: 0.2, w: 0.6, h: 0.6 };
        assert_eq!(hit_test(c, (0.2, 0.2), 0.05), Some(Handle::TopLeft));
        assert_eq!(hit_test(c, (0.8, 0.8), 0.05), Some(Handle::BottomRight));
        assert_eq!(hit_test(c, (0.5, 0.5), 0.05), Some(Handle::Body));
        assert_eq!(hit_test(c, (0.95, 0.05), 0.02), None, "outside any handle/body");
    }

    #[test]
    fn resize_clamps_into_unit_square() {
        let r = resize(full(), Handle::TopLeft, (-0.3, -0.3), None);
        assert!(r.x >= 0.0 && r.y >= 0.0, "clamped to image bounds");
    }

    #[test]
    fn resize_with_aspect_holds_ratio() {
        let c = CropRect { x: 0.1, y: 0.1, w: 0.4, h: 0.4 };
        let r = resize(c, Handle::BottomRight, (0.9, 0.6), Some(2.0)); // 2:1
        assert!((r.w / r.h - 2.0).abs() < 1e-3, "aspect held at 2:1, got {}", r.w / r.h);
    }

    #[test]
    fn move_body_clamps_inside() {
        let c = CropRect { x: 0.6, y: 0.6, w: 0.5, h: 0.5 };
        let m = move_body(c, (0.5, 0.5));
        assert!(m.x + m.w <= 1.0 + 1e-6 && m.y + m.h <= 1.0 + 1e-6, "stays inside");
    }

    #[test]
    fn rotate_angle_is_zero_to_the_right() {
        let a = rotate_angle((0.5, 0.5), (1.0, 0.5));
        assert!(a.abs() < 1e-3, "pointer due-right of center = 0°, got {a}");
    }

    #[test]
    fn aspect_ratio_maps_presets() {
        assert_eq!(aspect_ratio(Aspect::Square, 6000, 4000), Some(1.0));
        assert_eq!(aspect_ratio(Aspect::ThreeTwo, 6000, 4000), Some(1.5));
        assert_eq!(aspect_ratio(Aspect::Free, 6000, 4000), None);
        assert_eq!(aspect_ratio(Aspect::Original, 6000, 4000), Some(1.5));
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p ferrolite-app develop::crop_math` → FAIL.

- [ ] **Step 3: Implement**

```rust
//! Pure crop-overlay geometry in image-normalized [0,1] space. egui-free; the
//! overlay widget converts screen↔image coords and routes pointer events here.

use ferrolite_pipeline::{Aspect, CropRect};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    Body,
}

const MIN_SIZE: f32 = 0.02;

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

fn near(a: (f32, f32), b: (f32, f32), r: f32) -> bool {
    (a.0 - b.0).abs() <= r && (a.1 - b.1).abs() <= r
}

pub fn hit_test(c: CropRect, pos: (f32, f32), r: f32) -> Option<Handle> {
    let (l, t, rt, b) = (c.x, c.y, c.x + c.w, c.y + c.h);
    let (mx, my) = (c.x + c.w * 0.5, c.y + c.h * 0.5);
    let candidates = [
        (Handle::TopLeft, (l, t)),
        (Handle::TopRight, (rt, t)),
        (Handle::BottomRight, (rt, b)),
        (Handle::BottomLeft, (l, b)),
        (Handle::Top, (mx, t)),
        (Handle::Bottom, (mx, b)),
        (Handle::Left, (l, my)),
        (Handle::Right, (rt, my)),
    ];
    for (h, p) in candidates {
        if near(pos, p, r) {
            return Some(h);
        }
    }
    if pos.0 >= l && pos.0 <= rt && pos.1 >= t && pos.1 <= b {
        return Some(Handle::Body);
    }
    None
}

pub fn resize(c: CropRect, handle: Handle, pos: (f32, f32), aspect: Option<f32>) -> CropRect {
    let (mut l, mut t, mut rt, mut b) = (c.x, c.y, c.x + c.w, c.y + c.h);
    let px = clamp01(pos.0);
    let py = clamp01(pos.1);
    match handle {
        Handle::Left | Handle::TopLeft | Handle::BottomLeft => l = px.min(rt - MIN_SIZE),
        Handle::Right | Handle::TopRight | Handle::BottomRight => rt = px.max(l + MIN_SIZE),
        _ => {}
    }
    match handle {
        Handle::Top | Handle::TopLeft | Handle::TopRight => t = py.min(b - MIN_SIZE),
        Handle::Bottom | Handle::BottomLeft | Handle::BottomRight => b = py.max(t + MIN_SIZE),
        _ => {}
    }
    let mut out = CropRect { x: l, y: t, w: rt - l, h: b - t };
    if let Some(ar) = aspect {
        // Re-derive height from width at the dragged corner, anchored opposite.
        let new_h = (out.w / ar).clamp(MIN_SIZE, 1.0);
        match handle {
            Handle::TopLeft | Handle::TopRight | Handle::Top => out.y = (b - new_h).max(0.0),
            _ => {}
        }
        out.h = new_h;
        if out.y + out.h > 1.0 {
            out.h = 1.0 - out.y;
            out.w = out.h * ar;
        }
    }
    out.x = clamp01(out.x);
    out.y = clamp01(out.y);
    out.w = out.w.clamp(MIN_SIZE, 1.0 - out.x);
    out.h = out.h.clamp(MIN_SIZE, 1.0 - out.y);
    out
}

pub fn move_body(c: CropRect, delta: (f32, f32)) -> CropRect {
    let x = (c.x + delta.0).clamp(0.0, 1.0 - c.w);
    let y = (c.y + delta.1).clamp(0.0, 1.0 - c.h);
    CropRect { x, y, w: c.w, h: c.h }
}

pub fn rotate_angle(center: (f32, f32), pos: (f32, f32)) -> f32 {
    let dy = pos.1 - center.1;
    let dx = pos.0 - center.0;
    dy.atan2(dx).to_degrees()
}

pub fn aspect_ratio(aspect: Aspect, img_w: u32, img_h: u32) -> Option<f32> {
    match aspect {
        Aspect::Free => None,
        Aspect::Square => Some(1.0),
        Aspect::ThreeTwo => Some(3.0 / 2.0),
        Aspect::FourThree => Some(4.0 / 3.0),
        Aspect::SixteenNine => Some(16.0 / 9.0),
        Aspect::Original => {
            if img_h == 0 {
                None
            } else {
                Some(img_w as f32 / img_h as f32)
            }
        }
    }
}
```

- [ ] **Step 4: Run to verify pass** — `cargo test -p ferrolite-app develop::crop_math` → PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/develop/crop_math.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(app): pure crop-overlay hit-test/resize/rotate math"
```

---

## Task 6: Undo/redo history ring (`develop/history.rs`)

A bounded `OpStack`-snapshot ring. Immutable stacks make snapshots cheap. Coalesces consecutive edits of the **same op kind** into one entry (so a slider drag is one undo step, not hundreds). Per-open-image, not persisted.

**Files:**
- Create: `ferrolite-app/src/develop/history.rs`
- Test: in `history.rs`

**Interfaces:**
- Consumes: `ferrolite_pipeline::{OpStack, OpKind}`.
- Produces:
  - `pub struct History { /* private */ }`
  - `pub fn new(initial: OpStack, cap: usize) -> History`
  - `pub fn push(&mut self, kind: OpKind, stack: OpStack)` — append (truncating redo tail); coalesce when `kind` equals the immediately-preceding pushed kind and we are at the tip.
  - `pub fn undo(&mut self) -> Option<OpStack>` / `pub fn redo(&mut self) -> Option<OpStack>`
  - `pub fn current(&self) -> &OpStack`
  - `pub fn can_undo(&self) -> bool` / `pub fn can_redo(&self) -> bool`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{Exposure, Op, OpKind, OpStack};

    fn ev(stack: &OpStack, v: f32) -> OpStack {
        stack.set_op(Op::Exposure(Exposure { ev: v }))
    }

    #[test]
    fn coalesces_same_kind_into_one_step() {
        let mut h = History::new(OpStack::default(), 50);
        let s1 = ev(&OpStack::default(), 0.1);
        let s2 = ev(&OpStack::default(), 0.2);
        let s3 = ev(&OpStack::default(), 0.3);
        h.push(OpKind::Exposure, s1);
        h.push(OpKind::Exposure, s2);
        h.push(OpKind::Exposure, s3.clone());
        assert_eq!(h.current(), &s3);
        // One undo returns to the pre-drag state (identity), not 0.2/0.1.
        assert_eq!(h.undo(), Some(OpStack::default()));
        assert!(!h.can_undo());
    }

    #[test]
    fn different_kind_starts_new_step() {
        let mut h = History::new(OpStack::default(), 50);
        let s1 = ev(&OpStack::default(), 0.5);
        let s2 = s1.set_op(Op::Contrast(ferrolite_pipeline::Contrast { amount: 0.3 }));
        h.push(OpKind::Exposure, s1.clone());
        h.push(OpKind::Contrast, s2.clone());
        assert_eq!(h.undo(), Some(s1));
        assert_eq!(h.undo(), Some(OpStack::default()));
    }

    #[test]
    fn redo_after_undo_then_push_truncates() {
        let mut h = History::new(OpStack::default(), 50);
        let a = ev(&OpStack::default(), 0.5);
        h.push(OpKind::Exposure, a.clone());
        assert_eq!(h.undo(), Some(OpStack::default()));
        assert_eq!(h.redo(), Some(a));
        assert_eq!(h.undo(), Some(OpStack::default()));
        // A new push after an undo drops the redo tail.
        let b = OpStack::default().set_op(Op::Contrast(ferrolite_pipeline::Contrast { amount: 0.2 }));
        h.push(OpKind::Contrast, b);
        assert!(!h.can_redo(), "redo tail dropped after a fresh push");
    }

    #[test]
    fn cap_drops_oldest() {
        let mut h = History::new(OpStack::default(), 2); // initial + 1 more
        h.push(OpKind::Exposure, ev(&OpStack::default(), 0.1));
        h.push(OpKind::Contrast, OpStack::default().set_op(Op::Contrast(
            ferrolite_pipeline::Contrast { amount: 0.2 },
        )));
        // Capacity 2 means at most 2 entries; the oldest (identity) was dropped.
        let mut steps = 0;
        while h.undo().is_some() {
            steps += 1;
        }
        assert!(steps <= 1, "bounded history: at most cap-1 undos, got {steps}");
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p ferrolite-app develop::history` → FAIL.

- [ ] **Step 3: Implement**

```rust
//! Bounded undo/redo ring of `OpStack` snapshots with same-kind coalescing.
//! Per-open-image; not persisted (only the resulting `OpStack` persists).

use ferrolite_pipeline::{OpKind, OpStack};

pub struct History {
    entries: Vec<OpStack>,
    cursor: usize,
    cap: usize,
    last_kind: Option<OpKind>,
}

impl History {
    pub fn new(initial: OpStack, cap: usize) -> Self {
        Self {
            entries: vec![initial],
            cursor: 0,
            cap: cap.max(1),
            last_kind: None,
        }
    }

    pub fn current(&self) -> &OpStack {
        &self.entries[self.cursor]
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    pub fn push(&mut self, kind: OpKind, stack: OpStack) {
        // Drop any redo tail.
        self.entries.truncate(self.cursor + 1);
        if self.last_kind == Some(kind) && self.cursor > 0 {
            // Coalesce: replace the tip rather than append a new step.
            self.entries[self.cursor] = stack;
        } else {
            self.entries.push(stack);
            self.cursor += 1;
            self.last_kind = Some(kind);
        }
        // Enforce the bound (drop oldest).
        while self.entries.len() > self.cap {
            self.entries.remove(0);
            self.cursor -= 1;
        }
    }

    pub fn undo(&mut self) -> Option<OpStack> {
        if !self.can_undo() {
            return None;
        }
        self.cursor -= 1;
        self.last_kind = None; // next edit starts a fresh step
        Some(self.entries[self.cursor].clone())
    }

    pub fn redo(&mut self) -> Option<OpStack> {
        if !self.can_redo() {
            return None;
        }
        self.cursor += 1;
        self.last_kind = None;
        Some(self.entries[self.cursor].clone())
    }
}
```

- [ ] **Step 4: Run to verify pass** — `cargo test -p ferrolite-app develop::history` → PASS.

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/develop/history.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(app): bounded undo/redo OpStack history with same-kind coalescing"
```

---

## Task 7: VT engine-tier display seam (`update_single_from_texture`)

Let the rung-1 single-texture display sample an externally-owned GPU texture (the preview `EditPipeline` output) instead of re-uploading a CPU image. Engine-tier — carries **no photo concepts** (takes a `wgpu::Texture`).

**Files:**
- Modify: `ferrolite-vt/src/view.rs` (`SingleResources`, `single_texture`, new `update_single_from_texture`)
- Test: `ferrolite-vt/src/view.rs` (headless-guarded)

**Interfaces:**
- Produces:
  - `pub fn update_single_from_texture(&mut self, texture: std::sync::Arc<wgpu::Texture>, dims: (u32, u32))` — swap the displayed texture + dims; the next `prepare_single`/`draw_single` paints it. The texture must be `Rgba16Float` with `TEXTURE_BINDING` usage (the `PointOpNode`/`GeometryNode`/`SourceNode` outputs already satisfy this).
- Consumed by: the preview-tier wiring (Task 9).

- [ ] **Step 1: Change `SingleResources.texture` to `Arc`** — `view.rs:62`

```rust
struct SingleResources {
    texture: std::sync::Arc<wgpu::Texture>,
    texture_view: wgpu::TextureView,
    // … unchanged …
}
```

In `single_texture` (around `view.rs:210`), wrap the created texture: `texture: std::sync::Arc::new(texture),`. `render`/`render_to_image`/`prepare_single` call `single.texture.create_view(...)` / read `texture_view`; `Arc<wgpu::Texture>` derefs to `wgpu::Texture`, so they compile unchanged.

- [ ] **Step 2: Write the failing test** (append to a `view.rs` test module, or create one)

```rust
#[cfg(test)]
mod single_update_tests {
    use super::*;
    use ferrolite_image::LinearRgbaF32;

    #[test]
    fn update_single_swaps_dims() {
        let Some(ctx) = GpuContext::headless() else {
            return; // CI headless: skip (spec §10 GPU-test convention)
        };
        let pipelines = DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
        let img = LinearRgbaF32::new(2, 2, vec![0.0; 2 * 2 * 4]).unwrap();
        let mut vt = VirtualTexture::single_texture(&ctx, &img, &pipelines);
        assert_eq!(vt.single_dims(), Some((2, 2)));

        // A 4×4 Rgba16Float texture with TEXTURE_BINDING (mirrors a pipeline output).
        let tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test-edit-out"),
            size: wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        vt.update_single_from_texture(std::sync::Arc::new(tex), (4, 4));
        assert_eq!(vt.single_dims(), Some((4, 4)), "dims reflect the swapped texture");
    }
}
```

(Confirm `GpuContext::headless()` exists — used by Plan 1–3 goldens per spec §10; reuse that exact constructor.)

- [ ] **Step 3: Run to verify failure** — `cargo test -p ferrolite-vt single_update` → FAIL (method missing) or skip if no GPU; either way build fails until impl exists.

- [ ] **Step 4: Implement** (add to `impl VirtualTexture`, near `prepare_single`)

```rust
    /// Replace the rung-1 single texture with an externally-owned GPU texture
    /// (e.g. an edit-pipeline output). The texture must be `Rgba16Float` with
    /// `TEXTURE_BINDING` usage. The next `prepare_single` rebuilds the bind group
    /// from the new view; a no-op on a non-single VT.
    pub fn update_single_from_texture(
        &mut self,
        texture: std::sync::Arc<wgpu::Texture>,
        dims: (u32, u32),
    ) {
        if let Some(s) = self.single.as_mut() {
            s.texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            s.texture = texture;
            s.image_dims = dims;
            s.bind_group = None; // force rebuild in prepare_single
        }
    }
```

- [ ] **Step 5: Run** — `cargo test -p ferrolite-vt` (PASS or skip headless) + `cargo build -p ferrolite-vt`.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-vt/src/view.rs
git commit -m "feat(vt): single-texture display can sample an external GPU texture"
```

---

## Task 8: `ViewerState` edit fields + off-thread persistence + read-on-open job

Add the per-image edit state, the off-thread `frl:ops` write/read jobs, and the `OpsLoaded` event. No GPU here — that wiring is Task 9. The persist + read are off-thread (CLAUDE.md §1).

**Files:**
- Modify: `ferrolite-app/src/viewer/mod.rs` (`ViewerState` fields + `open`)
- Create: `ferrolite-app/src/develop/ops_persist.rs`
- Modify: `ferrolite-app/src/events.rs` (`AppEvent::OpsLoaded` + `apply` arm)
- Test: `develop/ops_persist.rs` is mostly job-spawning (needs a real `ctx`); the load-fold is tested in `events.rs`.

**Interfaces:**
- `ViewerState` new fields:
  - `pub preview_source: Option<std::sync::Arc<ferrolite_image::LinearRgbaF32>>`
  - `pub preview_edit: Option<ferrolite_pipeline::EditPipeline>` (!Send/!Sync — lives here like `edit_producer`)
  - `pub pyramid: Option<std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>>` (retained so the full-res producer can be rebuilt on geometry/halo change)
  - `pub opstack_version: u64`
  - `pub history: crate::develop::history::History`
  - `pub before_after: bool`
  - `pub crop_active: bool`
  - `pub hsl_band: usize`
  - `pub ops_loaded: bool`
  - `pub ops_read_handle: Option<ferrolite_jobs::JobHandle>`
- Produces:
  - `ops_persist::spawn_ops_write(jobs, writer, tx, ctx, image_id, path, stack)`
  - `ops_persist::spawn_ops_read(jobs, tx, ctx, image_id, path) -> JobHandle`
  - `AppEvent::OpsLoaded { image_id: i64, stack: ferrolite_pipeline::OpStack }`

- [ ] **Step 1: Add the `ViewerState` fields** — `viewer/mod.rs`

Add the fields to the struct and initialise them in `open`:

```rust
            op_stack: OpStack::default(),
            edit_producer: None,
            preview_source: None,
            preview_edit: None,
            pyramid: None,
            opstack_version: 0,
            history: crate::develop::history::History::new(OpStack::default(), 100),
            before_after: false,
            crop_active: false,
            hsl_band: 0,
            ops_loaded: false,
            ops_read_handle: None,
```

(Add the matching `pub` field declarations in the struct, with the use lines `use ferrolite_pipeline::{EditPipeline, GpuPyramidSource, OpStack};` extended.)

- [ ] **Step 2: Add the `OpsLoaded` event** — `events.rs`

In the `AppEvent` enum:

```rust
    /// An off-thread frl:ops sidecar read finished. Carries the hydrated stack
    /// (default = unedited). Handled in `app.rs` (needs GPU state), not folded.
    OpsLoaded {
        image_id: i64,
        stack: ferrolite_pipeline::OpStack,
    },
```

In `apply`, add the no-fold arm: `AppEvent::OpsLoaded { .. } => None,`. Add `#[derive(Debug)]` compatibility — `OpStack` derives `Debug`, so the enum's `#[derive(Debug)]` still holds.

- [ ] **Step 3: Implement the persist + read jobs** — `develop/ops_persist.rs`

```rust
//! Off-thread frl:ops sidecar persistence (mirrors `metadata.rs`): the in-memory
//! OpStack edit is immediate; this job follows and reports a MetadataResult.

use crate::events::AppEvent;
use ferrolite_catalog::Catalog;
use ferrolite_jobs::{JobHandle, JobSystem, Priority};
use ferrolite_pipeline::OpStack;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// Persist the op stack: write `frl:ops` to the sidecar (merge-preserving) and
/// update the catalog `has_edits` cache. A sidecar failure is a warning, not a
/// revert (the in-memory stack + has_edits intent are kept), per spec §9.
pub fn spawn_ops_write(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    path: PathBuf,
    stack: OpStack,
) {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Visible, move |_cancel| {
        let payload = ferrolite_pipeline::serialize(&stack);
        let mut warning = None;
        let xmp = ferrolite_catalog::sidecar_path(&path);
        if let Err(e) = ferrolite_catalog::write_ops(&xmp, &payload) {
            warning = Some(format!("sidecar write failed: {e}"));
        }
        let mut ok = true;
        {
            let db = writer.lock().expect("writer");
            if let Err(e) = db.set_has_edits(image_id, !stack.is_identity()) {
                ok = false;
                warning = Some(format!("catalog write failed: {e}"));
            }
        }
        let _ = tx.send(AppEvent::MetadataResult { ok, warning });
        ctx.request_repaint();
    });
}

/// Read `frl:ops` off-thread on viewer open; send an `OpsLoaded` (default stack
/// when absent/malformed/unknown-version, per spec §7).
pub fn spawn_ops_read(
    jobs: &Arc<JobSystem>,
    tx: &Sender<AppEvent>,
    ctx: &egui::Context,
    image_id: i64,
    path: PathBuf,
) -> JobHandle {
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Interactive, move |cancel| {
        if cancel.is_cancelled() {
            return;
        }
        let xmp = ferrolite_catalog::sidecar_path(&path);
        let stack = ferrolite_catalog::read_ops(&xmp)
            .and_then(|p| ferrolite_pipeline::deserialize(&p))
            .unwrap_or_default();
        let _ = tx.send(AppEvent::OpsLoaded { image_id, stack });
        ctx.request_repaint();
    })
}
```

- [ ] **Step 4: Build + run the existing tests**

Run: `cargo build -p ferrolite-app && cargo test -p ferrolite-app events`
Expected: PASS (compiles with new fields/event/jobs; existing `events` tests unaffected).

- [ ] **Step 5: Commit**

```bash
git add ferrolite-app/src/viewer/mod.rs ferrolite-app/src/events.rs ferrolite-app/src/develop/ops_persist.rs ferrolite-app/src/develop/mod.rs
git commit -m "feat(app): viewer edit state + off-thread frl:ops write/read + OpsLoaded event"
```

---

## Task 9: Preview-tier interactive display + full-res producer update wiring

The core wiring: build the preview `EditPipeline` **once per image**, and on each op-stack change update both tiers + persist. This is the gap from Plans 1–3 (the live preview was never edit-aware). Verified by build + clippy + the author's visual test (no unit tests — GPU/egui rendering, spec §10).

**Files:**
- Modify: `ferrolite-app/src/app.rs` (`apply_preview_ready` stash source; `apply_full_decoded` retain pyramid; new `set_preview_and_full`, `apply_edit`; `OpsLoaded` handling)

**Interfaces:**
- Produces (methods on `FerroliteApp`):
  - `fn set_preview_and_full(&mut self, frame: &eframe::Frame, stack: ferrolite_pipeline::OpStack)` — GPU/memory only: update `viewer.op_stack`, lazily build/reuse the preview `EditPipeline` from `preview_source`, evaluate (respecting `before_after`), `update_single_from_texture` the preview VT; update or rebuild the full-res producer + bump `opstack_version` via `set_opstack_version` + `set_producing(true)`; clear `idle`.
  - `fn apply_edit(&mut self, ctx, frame, kind: ferrolite_pipeline::OpKind, stack: OpStack, commit: bool)` — `set_preview_and_full(stack.clone())`; on `commit`: `history.push(kind, stack)`, set in-memory `has_edits`, `spawn_ops_write`.
- Consumes: `ferrolite_pipeline::{EditPipeline, OpStack, OpKind}`, `develop::ops_edit::needs_full_rebuild`, Task 7's `update_single_from_texture`.

- [ ] **Step 1: Stash the preview source** — in `apply_preview_ready`, after computing `linear`:

```rust
        let linear = viewer::load::preview_to_linear(image);
        let dims = (linear.width, linear.height);
        // Keep the display-linear source so the preview EditPipeline can be built
        // lazily on the first edit (built once, reused via set_stack thereafter).
        v.preview_source = Some(std::sync::Arc::new(linear.clone()));
```

(The existing code continues to build the rung-1 `single_texture` from `linear`.)

- [ ] **Step 2: Retain the pyramid for rebuilds** — in `apply_full_decoded`, where the `GpuPyramidSource` is built for a non-identity stack, store it on the viewer so a later geometry/halo change can rebuild the producer:

```rust
                        let pyramid = std::sync::Arc::new(
                            ferrolite_pipeline::GpuPyramidSource::new(&gpu, image),
                        );
                        v.pyramid = Some(std::sync::Arc::clone(&pyramid));
                        let tep = ferrolite_pipeline::TileEditPipeline::new(
                            ctx_arc, pyramid, v.op_stack.clone(),
                        );
```

Also build the pyramid **unconditionally** (even for an identity stack) so the producer can be created on the first edit of an image that opened unedited. Restructure: build `v.pyramid` whenever `full_installed`, and only attach `edit_producer` + `set_producing` when `!op_stack.is_identity()`.

- [ ] **Step 3: Implement `set_preview_and_full`** — new method on `FerroliteApp`

```rust
    /// Apply `stack` to both render tiers (GPU + memory only; no history/persist).
    /// Preview tier: build the EditPipeline once, reuse via set_stack; evaluate
    /// and swap the displayed single texture. Full-res tier: set_stack (color) or
    /// rebuild (geometry/halo), bump the opstack version to invalidate cached tiles.
    fn set_preview_and_full(&mut self, frame: &eframe::Frame, stack: ferrolite_pipeline::OpStack) {
        let Some(rs) = frame.wgpu_render_state() else { return };
        let Some(v) = self.state.viewer.as_mut() else { return };
        let old = v.op_stack.clone();
        v.op_stack = stack.clone();
        v.opstack_version = v.opstack_version.wrapping_add(1);

        // What the preview should show: the live stack, or the empty stack in
        // before/after mode. While the crop tool is active, show the image
        // uncropped (crop forced full) so the overlay handles remain reachable.
        let mut shown = if v.before_after { ferrolite_pipeline::OpStack::default() } else { stack.clone() };
        if v.crop_active {
            shown = shown.reset(ferrolite_pipeline::OpKind::Geometry);
        }

        // Preview tier (built once per image, reused).
        if v.preview_edit.is_none() {
            if let Some(src) = v.preview_source.clone() {
                let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
                v.preview_edit = Some(ferrolite_pipeline::EditPipeline::new(
                    ctx_arc, &src, shown.clone(),
                ));
            }
        }
        if let Some(ep) = v.preview_edit.as_mut() {
            ep.set_stack(shown);
            let img = ep.evaluate();
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == v.image_id {
                    g.preview.update_single_from_texture(img.texture.clone(), (img.width, img.height));
                }
            }
        }

        // Full-res tier (only meaningful once the full decode + pyramid exist).
        if v.full_ready {
            let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
            let rebuild = v.edit_producer.is_none()
                || crate::develop::ops_edit::needs_full_rebuild(&old, &stack);
            if rebuild {
                if let Some(pyr) = v.pyramid.clone() {
                    let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
                    let tep = ferrolite_pipeline::TileEditPipeline::new(ctx_arc, pyr, stack.clone());
                    v.edit_producer = Some(viewer::EditTileProducer::new(tep));
                }
            } else if let Some(_p) = v.edit_producer.as_mut() {
                // Color-only change: update params in place. (Expose a
                // `set_stack` passthrough on EditTileProducer in this step.)
                v.edit_producer.as_mut().unwrap().set_stack(stack.clone());
            }
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if let Some(full) = g.full.as_mut() {
                    full.set_producing(!stack.is_identity());
                    full.set_opstack_version(&g.ctx, v.opstack_version);
                }
            }
        }
        v.idle = false; // wake the drive loop so producer tiles re-render
    }
```

Add a `set_stack` passthrough to `EditTileProducer` in `viewer/edit_producer.rs`:

```rust
    pub fn set_stack(&mut self, stack: ferrolite_pipeline::OpStack) {
        self.pipeline.set_stack(stack);
    }
```

- [ ] **Step 4: Implement `apply_edit`** — new method on `FerroliteApp`

```rust
    /// Apply a panel/widget edit: update both tiers immediately; on commit (drag
    /// release / discrete change) push undo history + persist off-thread.
    fn apply_edit(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        kind: ferrolite_pipeline::OpKind,
        stack: ferrolite_pipeline::OpStack,
        commit: bool,
    ) {
        self.set_preview_and_full(frame, stack.clone());
        if !commit {
            return;
        }
        let Some(v) = self.state.viewer.as_mut() else { return };
        v.history.push(kind, stack.clone());
        let image_id = v.image_id;
        let path = v.path.clone();
        let has_edits = !stack.is_identity();
        if let Some(rec) = self.state.images.iter_mut().find(|r| r.id == image_id) {
            rec.has_edits = has_edits; // optimistic cache update (filmstrip badge)
        }
        crate::develop::ops_persist::spawn_ops_write(
            &self.state.jobs, &self.state.writer, &self.state.tx, ctx, image_id, path, stack,
        );
    }
```

- [ ] **Step 5: Handle `OpsLoaded` (read-on-open)** — in `update`'s event loop, add an arm that hydrates without re-persisting:

```rust
                crate::events::AppEvent::OpsLoaded { image_id, stack } => {
                    if let Some(v) = self.state.viewer.as_mut() {
                        if v.image_id == *image_id && !v.ops_loaded {
                            v.ops_loaded = true;
                            if !stack.is_identity() {
                                v.history = crate::develop::history::History::new(stack.clone(), 100);
                                self.set_preview_and_full(frame, stack.clone());
                            }
                        }
                    }
                    self.state.dirty = true;
                    continue;
                }
```

And spawn the read once per open, next to the `spawn_preview` block:

```rust
            if !v.ops_loaded && v.ops_read_handle.is_none() {
                let h = crate::develop::ops_persist::spawn_ops_read(
                    &self.state.jobs, &self.state.tx, ctx, v.image_id, v.path.clone(),
                );
                v.ops_read_handle = Some(h);
            }
```

- [ ] **Step 6: Build + clippy**

Run: `cargo build -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean. (No unit tests — GPU/egui rendering; the author's visual test confirms a slider visibly changes the preview, before/after restores the original, and 1:1 zoom shows the edited tiles.)

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/app.rs ferrolite-app/src/viewer/edit_producer.rs
git commit -m "feat(app): wire interactive preview-tier edit display + full-res producer update + read-on-open"
```

---

## Task 10: The 296px right adjustment panel (Basic + Detail + resets)

The `SidePanel::right` shown only in Develop with a viewer open: restyled `CollapsingHeader` sections, `EguiSlider` per param, per-section + global reset. Sliders emit a new `OpStack` via `ops_edit`. Returns the outcome so `app.rs` applies it. Visual-tested.

**Files:**
- Create: `ferrolite-app/src/develop/adjustment_panel.rs`
- Modify: `ferrolite-app/src/app.rs` (show the panel; apply the outcome)

**Interfaces:**
- Produces:
  - `pub struct EditOutcome { pub stack: OpStack, pub kind: OpKind, pub commit: bool }`
  - `pub fn show(ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome>` — reads `state.viewer.op_stack` + `hsl_band`/`crop_active`; renders sections; returns the single edit produced this frame (if any). Mutating `state.viewer` (e.g. `crop_active`, `hsl_band`) is allowed; the `OpStack` change is returned, not applied.

- [ ] **Step 1: Implement the panel** — `develop/adjustment_panel.rs`

```rust
//! Develop right adjustment panel (design-system §6, 296px). CollapsingHeader
//! sections; one EguiSlider per op param; per-section + global reset. Emits a new
//! OpStack via develop::ops_edit; the app applies it to both render tiers.

use crate::develop::{curve_widget, hsl_widget, ops_edit};
use crate::state::AppState;
use crate::widgets::slider::EguiSlider;
use ferrolite_pipeline::{Aspect, Geometry, Op, OpKind, OpStack};

pub struct EditOutcome {
    pub stack: OpStack,
    pub kind: OpKind,
    pub commit: bool,
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
    let stack = match state.viewer.as_ref() {
        Some(v) => v.op_stack.clone(),
        None => return None,
    };
    let mut out: Option<EditOutcome> = None;

    // ── Basic ──
    egui::CollapsingHeader::new("Basic").default_open(true).show(ui, |ui| {
        // Exposure (bipolar EV).
        let mut ev = stack.exposure().map(|e| e.ev).unwrap_or(0.0);
        let r = ui.add(EguiSlider {
            label: "Exposure", value: &mut ev, min: -5.0, max: 5.0, default: 0.0,
            step: 0.01, decimals: 2, unit: " EV", bipolar: true, signed: true,
        });
        if r.changed() {
            out = Some(EditOutcome {
                stack: ops_edit::set_exposure(&stack, ev),
                kind: OpKind::Exposure,
                commit: r.drag_stopped() || !r.dragged(),
            });
        }
        // Contrast (bipolar).
        let mut c = stack.contrast().map(|c| c.amount).unwrap_or(0.0);
        let r = ui.add(EguiSlider {
            label: "Contrast", value: &mut c, min: -1.0, max: 1.0, default: 0.0,
            step: 0.01, decimals: 2, unit: "", bipolar: true, signed: true,
        });
        if r.changed() {
            out = Some(EditOutcome {
                stack: ops_edit::set_contrast(&stack, c), kind: OpKind::Contrast,
                commit: r.drag_stopped() || !r.dragged(),
            });
        }
        // White balance Temp + Tint.
        let wb = stack.white_balance();
        let (mut temp, mut tint) = wb.map(|w| (w.temp, w.tint)).unwrap_or((0.0, 0.0));
        let rt = ui.add(EguiSlider {
            label: "Temp", value: &mut temp, min: -1.0, max: 1.0, default: 0.0,
            step: 0.01, decimals: 2, unit: "", bipolar: true, signed: true,
        });
        let rn = ui.add(EguiSlider {
            label: "Tint", value: &mut tint, min: -1.0, max: 1.0, default: 0.0,
            step: 0.01, decimals: 2, unit: "", bipolar: true, signed: true,
        });
        if rt.changed() || rn.changed() {
            out = Some(EditOutcome {
                stack: ops_edit::set_white_balance(&stack, temp, tint),
                kind: OpKind::WhiteBalance,
                commit: (rt.drag_stopped() || rn.drag_stopped()) || !(rt.dragged() || rn.dragged()),
            });
        }
        if ui.small_button("Reset").clicked() {
            let s = stack.reset(OpKind::Exposure).reset(OpKind::Contrast).reset(OpKind::WhiteBalance);
            out = Some(EditOutcome { stack: s, kind: OpKind::Exposure, commit: true });
        }
    });

    // ── Tone Curve ── (interactive widget, Task 11)
    egui::CollapsingHeader::new("Tone Curve").show(ui, |ui| {
        if let Some(o) = curve_widget::show(ui, &stack) {
            out = Some(o);
        }
    });

    // ── HSL ── (swatch row + per-band sliders, Task 12)
    egui::CollapsingHeader::new("HSL").show(ui, |ui| {
        if let Some(v) = state.viewer.as_mut() {
            if let Some(o) = hsl_widget::show(ui, &stack, &mut v.hsl_band) {
                out = Some(o);
            }
        }
    });

    // ── Detail ──
    egui::CollapsingHeader::new("Detail").show(ui, |ui| {
        let sh = stack.sharpen();
        let (mut amount, mut radius) = sh.map(|s| (s.amount, s.radius as f32)).unwrap_or((0.0, 1.0));
        let ra = ui.add(EguiSlider {
            label: "Amount", value: &mut amount, min: 0.0, max: 2.0, default: 0.0,
            step: 0.01, decimals: 2, unit: "", bipolar: false, signed: false,
        });
        let rr = ui.add(EguiSlider {
            label: "Radius", value: &mut radius, min: 1.0, max: 8.0, default: 1.0,
            step: 1.0, decimals: 0, unit: " px", bipolar: false, signed: false,
        });
        if ra.changed() || rr.changed() {
            out = Some(EditOutcome {
                stack: ops_edit::set_sharpen(&stack, amount, radius.round() as u32),
                kind: OpKind::Sharpen,
                commit: (ra.drag_stopped() || rr.drag_stopped()) || !(ra.dragged() || rr.dragged()),
            });
        }
    });

    // ── Geometry ── (angle + aspect; the crop overlay lives on the canvas, Task 13)
    egui::CollapsingHeader::new("Geometry").show(ui, |ui| {
        if let Some(v) = state.viewer.as_mut() {
            v.crop_active = true; // overlay shown while this section is expanded
        }
        let geo = stack.geometry().unwrap_or(Geometry {
            crop: ferrolite_pipeline::CropRect::full(), angle_deg: 0.0, aspect: Aspect::Original,
        });
        let mut angle = geo.angle_deg;
        let r = ui.add(EguiSlider {
            label: "Angle", value: &mut angle, min: -45.0, max: 45.0, default: 0.0,
            step: 0.1, decimals: 1, unit: "\u{b0}", bipolar: true, signed: true,
        });
        let mut aspect = geo.aspect;
        egui::ComboBox::from_label("Aspect")
            .selected_text(format!("{aspect:?}"))
            .show_ui(ui, |ui| {
                for a in [Aspect::Original, Aspect::Free, Aspect::Square, Aspect::ThreeTwo, Aspect::FourThree, Aspect::SixteenNine] {
                    ui.selectable_value(&mut aspect, a, format!("{a:?}"));
                }
            });
        if r.changed() || aspect != geo.aspect {
            let new_geo = Geometry { crop: geo.crop, angle_deg: angle, aspect };
            let s = if new_geo.angle_deg == 0.0
                && new_geo.aspect == Aspect::Original
                && new_geo.crop == ferrolite_pipeline::CropRect::full()
            {
                stack.reset(OpKind::Geometry)
            } else {
                stack.set_op(Op::Geometry(new_geo))
            };
            out = Some(EditOutcome { stack: s, kind: OpKind::Geometry, commit: r.drag_stopped() || !r.dragged() || aspect != geo.aspect });
        }
        if ui.small_button("Reset crop").clicked() {
            out = Some(EditOutcome { stack: stack.reset(OpKind::Geometry), kind: OpKind::Geometry, commit: true });
        }
    });
    // Geometry section collapsed → clear crop_active (overlay hidden) handled by
    // app.rs based on whether this section reported open; simplest: reset to false
    // at the top of the frame and set true inside the open section (above).

    ui.separator();
    if ui.button("Reset all").clicked() {
        out = Some(EditOutcome { stack: OpStack::default(), kind: OpKind::Exposure, commit: true });
    }

    out
}
```

Note on `crop_active`: set `viewer.crop_active = false` once per frame in `app.rs` **before** showing the panel, so the Geometry section sets it back to `true` only while expanded. (`CollapsingHeader::show`'s body closure runs only when open.)

- [ ] **Step 2: Show the panel + apply the outcome** — `app.rs`, after the develop bottom-meta panel block, add (Develop + viewer only):

```rust
        if self.module == crate::module::Module::Develop && self.state.viewer.is_some() {
            if let Some(v) = self.state.viewer.as_mut() {
                v.crop_active = false; // re-armed by the open Geometry section
            }
            let mut outcome = None;
            egui::SidePanel::right("develop_adjust")
                .exact_width(296.0)
                .frame(egui::Frame::none().fill(theme::BG_APP).inner_margin(egui::Margin::symmetric(12.0, 8.0)))
                .show(ctx, |ui| {
                    outcome = crate::develop::adjustment_panel::show(ui, &mut self.state);
                });
            if let Some(o) = outcome {
                self.apply_edit(ctx, frame, o.kind, o.stack, o.commit);
            }
        }
```

This `SidePanel::right` must be declared **before** the `CentralPanel` so the canvas gets the remaining width (egui panel ordering). Place it just above the existing `CentralPanel::default()` block.

- [ ] **Step 3: Build + clippy**

Run: `cargo build -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean. (The `curve_widget`/`hsl_widget` modules are implemented in Tasks 11–12; until then stub them to return `None` so this compiles — see those tasks. Provide one-line stubs now and flesh out next.)

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/develop/adjustment_panel.rs ferrolite-app/src/app.rs
git commit -m "feat(app): Develop 296px adjustment panel (Basic/Detail/Geometry + resets)"
```

---

## Task 11: Interactive tone-curve widget (`develop/curve_widget.rs`)

A painted square curve editor: grid + polyline through control points + draggable handles. Routes pointer events into `curve_math`; emits a `ToneCurve` op. Visual-tested.

**Files:**
- Create: `ferrolite-app/src/develop/curve_widget.rs`

**Interfaces:**
- Consumes: `develop::curve_math`, `ferrolite_pipeline::{Op, OpKind, OpStack, ToneCurve}`, `adjustment_panel::EditOutcome`.
- Produces: `pub fn show(ui: &mut egui::Ui, stack: &OpStack) -> Option<crate::develop::adjustment_panel::EditOutcome>`.

- [ ] **Step 1: Implement**

```rust
//! Interactive tone-curve widget. Pure point math in `curve_math`; this layer
//! only paints + routes pointer events. Visual-tested (no unit tests).

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::curve_math;
use crate::theme;
use ferrolite_pipeline::{Op, OpKind, OpStack, ToneCurve};

const SIZE: f32 = 260.0; // square edit area
const HIT_R: f32 = 0.04; // normalized hit radius

pub fn show(ui: &mut egui::Ui, stack: &OpStack) -> Option<EditOutcome> {
    let mut points = stack
        .tone_curve()
        .map(|t| t.points)
        .filter(|p| !p.is_empty())
        .unwrap_or_else(curve_math::identity_points);

    let (rect, resp) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::click_and_drag());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::BG_BASE);
    // Grid (quarters).
    for i in 1..4 {
        let f = i as f32 / 4.0;
        painter.line_segment(
            [egui::pos2(rect.left() + f * SIZE, rect.top()), egui::pos2(rect.left() + f * SIZE, rect.bottom())],
            egui::Stroke::new(1.0, theme::BORDER_STRONG),
        );
        painter.line_segment(
            [egui::pos2(rect.left(), rect.top() + f * SIZE), egui::pos2(rect.right(), rect.top() + f * SIZE)],
            egui::Stroke::new(1.0, theme::BORDER_STRONG),
        );
    }

    // Coord transforms: image y is inverted on screen (0 at bottom).
    let to_screen = |p: (f32, f32)| egui::pos2(rect.left() + p.0 * SIZE, rect.bottom() - p.1 * SIZE);
    let to_norm = |s: egui::Pos2| ((s.x - rect.left()) / SIZE, (rect.bottom() - s.y) / SIZE);

    // Curve polyline.
    let poly: Vec<egui::Pos2> = points.iter().map(|&p| to_screen(p)).collect();
    painter.add(egui::Shape::line(poly, egui::Stroke::new(1.5, theme::ACCENT)));
    for &p in &points {
        painter.circle(to_screen(p), 3.5, theme::ACCENT_BRIGHT, egui::Stroke::new(1.0, theme::BG_BASE));
    }

    let mut changed = false;
    let mut commit = false;
    if let Some(pos) = resp.interact_pointer_pos() {
        let norm = to_norm(pos);
        if resp.drag_started() || resp.clicked() {
            // Grab the nearest existing point, else insert a new one.
            match curve_math::nearest_point(&points, norm, HIT_R) {
                Some(idx) => ui.memory_mut(|m| m.data.insert_temp(resp.id, idx)),
                None => {
                    points = curve_math::insert_point(&points, norm);
                    let idx = curve_math::nearest_point(&points, norm, HIT_R).unwrap_or(0);
                    ui.memory_mut(|m| m.data.insert_temp(resp.id, idx));
                    changed = true;
                }
            }
        }
        if resp.dragged() {
            if let Some(idx) = ui.memory(|m| m.data.get_temp::<usize>(resp.id)) {
                points = curve_math::move_point(&points, idx, norm);
                changed = true;
            }
        }
    }
    if resp.drag_stopped() {
        commit = true;
    }
    // Right-click a point to delete it.
    if resp.secondary_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some(idx) = curve_math::nearest_point(&points, to_norm(pos), HIT_R) {
                points = curve_math::delete_point(&points, idx);
                changed = true;
                commit = true;
            }
        }
    }

    if changed {
        let s = if curve_math::is_identity(&points) {
            stack.reset(OpKind::ToneCurve)
        } else {
            stack.set_op(Op::ToneCurve(ToneCurve { points }))
        };
        return Some(EditOutcome { stack: s, kind: OpKind::ToneCurve, commit });
    }
    None
}
```

- [ ] **Step 2: Build + clippy**

Run: `cargo build -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings`
Expected: clean. (Visual test: drag adds/moves points and the preview tone shifts; right-click deletes; flattening to the diagonal returns to identity.)

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/develop/curve_widget.rs
git commit -m "feat(app): interactive tone-curve widget"
```

---

## Task 12: HSL widget (`develop/hsl_widget.rs`)

An 8-band swatch row (band select) + Hue/Sat/Lum sliders for the selected band. Emits an `Hsl` op. Visual-tested.

**Files:**
- Create: `ferrolite-app/src/develop/hsl_widget.rs`

**Interfaces:**
- Produces: `pub fn show(ui: &mut egui::Ui, stack: &OpStack, band: &mut usize) -> Option<EditOutcome>`.
- Consumes: `ferrolite_pipeline::{Hsl, HslBand, Op, OpKind, OpStack}`, `EguiSlider`.

- [ ] **Step 1: Implement**

```rust
//! HSL widget: 8-band swatch row + per-band Hue/Sat/Lum sliders. The canonical
//! band order is red, orange, yellow, green, aqua, blue, purple, magenta.

use crate::develop::adjustment_panel::EditOutcome;
use crate::widgets::slider::EguiSlider;
use ferrolite_pipeline::{Hsl, HslBand, Op, OpKind, OpStack};

const SWATCHES: [(u8, u8, u8); 8] = [
    (0xc7, 0x54, 0x50), (0xd8, 0x8c, 0x3a), (0xd8, 0xc8, 0x3a), (0x4c, 0xaf, 0x71),
    (0x3a, 0xc8, 0xc8), (0x6d, 0x97, 0xb5), (0x9a, 0x6d, 0xb5), (0xb5, 0x6d, 0x9a),
];

pub fn show(ui: &mut egui::Ui, stack: &OpStack, band: &mut usize) -> Option<EditOutcome> {
    let mut hsl = stack.hsl().unwrap_or(Hsl { bands: [HslBand { hue: 0.0, sat: 0.0, lum: 0.0 }; 8] });
    let mut out = None;

    ui.horizontal(|ui| {
        for (i, (r, g, b)) in SWATCHES.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
            ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgb(*r, *g, *b));
            if i == *band {
                ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(2.0, crate::theme::ACCENT_BRIGHT));
            }
            if resp.clicked() {
                *band = i;
            }
        }
    });

    let b = (*band).min(7);
    let mut hue = hsl.bands[b].hue;
    let mut sat = hsl.bands[b].sat;
    let mut lum = hsl.bands[b].lum;
    let rh = ui.add(EguiSlider { label: "Hue", value: &mut hue, min: -1.0, max: 1.0, default: 0.0, step: 0.01, decimals: 2, unit: "", bipolar: true, signed: true });
    let rs = ui.add(EguiSlider { label: "Sat", value: &mut sat, min: -1.0, max: 1.0, default: 0.0, step: 0.01, decimals: 2, unit: "", bipolar: true, signed: true });
    let rl = ui.add(EguiSlider { label: "Lum", value: &mut lum, min: -1.0, max: 1.0, default: 0.0, step: 0.01, decimals: 2, unit: "", bipolar: true, signed: true });
    if rh.changed() || rs.changed() || rl.changed() {
        hsl.bands[b] = HslBand { hue, sat, lum };
        let all_zero = hsl.bands.iter().all(|x| x.hue == 0.0 && x.sat == 0.0 && x.lum == 0.0);
        let s = if all_zero { stack.reset(OpKind::Hsl) } else { stack.set_op(Op::Hsl(hsl)) };
        let commit = rh.drag_stopped() || rs.drag_stopped() || rl.drag_stopped()
            || !(rh.dragged() || rs.dragged() || rl.dragged());
        out = Some(EditOutcome { stack: s, kind: OpKind::Hsl, commit });
    }
    out
}
```

- [ ] **Step 2: Build + clippy** — `cargo build -p ferrolite-app && cargo clippy -p ferrolite-app --all-targets -- -D warnings` → clean. (Visual: selecting a band + dragging sliders shifts the matching hue range; zeroing all bands returns to identity.)

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/develop/hsl_widget.rs
git commit -m "feat(app): HSL 8-band widget"
```

---

## Task 13: Interactive crop overlay on the canvas (`develop/crop_overlay.rs`)

When the Geometry section is active (`viewer.crop_active`), draw a draggable crop rectangle with 8 handles + rule-of-thirds grid + a rotate handle over the image, routing pointer events into `crop_math`. Emits a `Geometry` op. Visual-tested.

**Files:**
- Create: `ferrolite-app/src/develop/crop_overlay.rs`
- Modify: `ferrolite-app/src/app.rs` (call the overlay from the central canvas when `crop_active`; apply its outcome)

**Interfaces:**
- Produces: `pub fn show(ui: &mut egui::Ui, image_rect: egui::Rect, stack: &OpStack, aspect_dims: (u32, u32)) -> Option<EditOutcome>` — `image_rect` is the on-screen rect the fit/preview image occupies (computed from the view transform); converts screen↔normalized image space and routes into `crop_math`.
- Consumes: `develop::crop_math`, `ferrolite_pipeline::{CropRect, Geometry, Op, OpKind, OpStack}`.

- [ ] **Step 1: Implement** (overlay paints handles + grid; routes drags into `crop_math::{hit_test,resize,move_body}`)

```rust
//! Canvas crop overlay. Pure geometry in `crop_math`; this layer paints handles +
//! a rule-of-thirds grid and routes pointer events. Shown only while the Geometry
//! section is active (viewer.crop_active). Visual-tested.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::crop_math::{self, Handle};
use crate::theme;
use ferrolite_pipeline::{Aspect, CropRect, Geometry, Op, OpKind, OpStack};

const HANDLE_R: f32 = 0.03; // normalized hit radius

pub fn show(
    ui: &mut egui::Ui,
    image_rect: egui::Rect,
    stack: &OpStack,
    aspect_dims: (u32, u32),
) -> Option<EditOutcome> {
    let geo = stack.geometry().unwrap_or(Geometry {
        crop: CropRect::full(), angle_deg: 0.0, aspect: Aspect::Original,
    });
    let crop = geo.crop;
    let to_screen = |nx: f32, ny: f32| {
        egui::pos2(image_rect.left() + nx * image_rect.width(), image_rect.top() + ny * image_rect.height())
    };
    let to_norm = |p: egui::Pos2| {
        (((p.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0),
         ((p.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0))
    };

    // Crop rect + rule-of-thirds.
    let r = egui::Rect::from_min_max(to_screen(crop.x, crop.y), to_screen(crop.x + crop.w, crop.y + crop.h));
    let painter = ui.painter();
    painter.rect_stroke(r, 0.0, egui::Stroke::new(1.5, theme::ACCENT_BRIGHT));
    for i in 1..3 {
        let f = i as f32 / 3.0;
        painter.line_segment([egui::pos2(r.left() + f * r.width(), r.top()), egui::pos2(r.left() + f * r.width(), r.bottom())], egui::Stroke::new(1.0, theme::ACCENT));
        painter.line_segment([egui::pos2(r.left(), r.top() + f * r.height()), egui::pos2(r.right(), r.top() + f * r.height())], egui::Stroke::new(1.0, theme::ACCENT));
    }
    for (nx, ny) in [(crop.x, crop.y), (crop.x + crop.w, crop.y), (crop.x, crop.y + crop.h), (crop.x + crop.w, crop.y + crop.h)] {
        painter.circle(to_screen(nx, ny), 4.0, theme::ACCENT_BRIGHT, egui::Stroke::new(1.0, theme::BG_BASE));
    }

    let resp = ui.interact(image_rect, ui.id().with("crop_overlay"), egui::Sense::click_and_drag());
    let aspect = crop_math::aspect_ratio(geo.aspect, aspect_dims.0, aspect_dims.1);
    let mut new_crop = crop;
    let mut changed = false;
    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            let h = crop_math::hit_test(crop, to_norm(p), HANDLE_R);
            ui.memory_mut(|m| m.data.insert_temp(resp.id, h.map(|h| h as u8).unwrap_or(255)));
        }
    }
    if resp.dragged() {
        let active: u8 = ui.memory(|m| m.data.get_temp(resp.id)).unwrap_or(255);
        if let Some(p) = resp.interact_pointer_pos() {
            let norm = to_norm(p);
            match active {
                x if x == Handle::Body as u8 => {
                    let d = (resp.drag_delta().x / image_rect.width(), resp.drag_delta().y / image_rect.height());
                    new_crop = crop_math::move_body(crop, d);
                    changed = true;
                }
                255 => {}
                _ => {
                    let handle = HANDLES[active as usize];
                    new_crop = crop_math::resize(crop, handle, norm, aspect);
                    changed = true;
                }
            }
        }
    }
    if changed {
        let new_geo = Geometry { crop: new_crop, angle_deg: geo.angle_deg, aspect: geo.aspect };
        return Some(EditOutcome {
            stack: stack.set_op(Op::Geometry(new_geo)),
            kind: OpKind::Geometry,
            commit: resp.drag_stopped(),
        });
    }
    None
}

// Index map matching `Handle as u8` for the resize handles (Body handled separately).
const HANDLES: [Handle; 9] = [
    Handle::TopLeft, Handle::Top, Handle::TopRight, Handle::Right,
    Handle::BottomRight, Handle::Bottom, Handle::BottomLeft, Handle::Left, Handle::Body,
];
```

(Note: `Handle as u8` relies on its declaration order in `crop_math`; keep `HANDLES` in sync with that order. Add a `#[repr(u8)]` to `Handle` and a `debug_assert!` in a test if you prefer a hard guarantee.)

- [ ] **Step 2: Call the overlay from the canvas** — in `app.rs`'s `CentralPanel`, after `drive_viewer`, when `viewer.crop_active`:

```rust
                    if self.state.viewer.as_ref().map(|v| v.crop_active).unwrap_or(false) {
                        let (stack, dims, view, viewport) = {
                            let v = self.state.viewer.as_ref().unwrap();
                            (v.op_stack.clone(), v.image_dims.unwrap_or((1, 1)), v.view, v.viewport)
                        };
                        // The on-screen rect the fitted image occupies (image-space → screen).
                        let image_rect = crate::viewer::image_screen_rect(ui.min_rect(), dims, view, viewport);
                        if let Some(o) = crate::develop::crop_overlay::show(ui, image_rect, &stack, dims) {
                            self.apply_edit(ctx, frame, o.kind, o.stack, o.commit);
                        }
                    }
```

Add a small pure helper `viewer::image_screen_rect(canvas: egui::Rect, dims, view, viewport) -> egui::Rect` mapping image bounds to screen via the same zoom/pan math the shader uses (fit case: image fills the canvas centered). This helper IS pure and may carry a unit test (image centered at zoom=fit). Keep it simple: for the fit view, the image rect = the canvas; refine later for zoomed/panned crop.

- [ ] **Step 3: Build + clippy** — clean. (Visual: expanding Geometry shows the crop overlay over the uncropped image; dragging handles/corners reshapes it with the aspect lock; collapsing applies the crop to the preview + 1:1 tiles.)

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/develop/crop_overlay.rs ferrolite-app/src/app.rs ferrolite-app/src/viewer/mod.rs
git commit -m "feat(app): interactive crop overlay wired to the Geometry section"
```

---

## Task 14: Before/After toggle + undo/redo keybindings

`\` swaps the empty stack vs the current stack on the preview (full swap, not split — spec §8.3). `Ctrl+Z`/`Ctrl+Y` (or `Ctrl+Shift+Z`) move through history when no text field has focus, persisting the resulting stack. Visual-tested + leans on the pure `history` unit tests.

**Files:**
- Modify: `ferrolite-app/src/app.rs` (keyboard block)

- [ ] **Step 1: Add the keybindings** — in `app.rs`, in the Develop-with-viewer keyboard region (guarded by `!ctx.wants_keyboard_input()`):

```rust
        if self.module == crate::module::Module::Develop
            && self.state.viewer.is_some()
            && !ctx.wants_keyboard_input()
        {
            // Before/After: `\` toggles showing the empty stack vs the live stack.
            if ctx.input(|i| i.key_pressed(egui::Key::Backslash)) {
                if let Some(v) = self.state.viewer.as_mut() {
                    v.before_after = !v.before_after;
                }
                let stack = self.state.viewer.as_ref().unwrap().op_stack.clone();
                self.set_preview_and_full(frame, stack); // re-evaluates with before_after
            }
            // Undo / Redo.
            let (undo, redo) = ctx.input(|i| {
                let z = i.key_pressed(egui::Key::Z);
                let y = i.key_pressed(egui::Key::Y);
                let cmd = i.modifiers.command;
                let shift = i.modifiers.shift;
                ((cmd && z && !shift), (cmd && y) || (cmd && z && shift))
            });
            if undo || redo {
                let result = self.state.viewer.as_mut().and_then(|v| {
                    if undo { v.history.undo() } else { v.history.redo() }
                });
                if let Some(stack) = result {
                    self.set_preview_and_full(frame, stack.clone());
                    // Persist the resulting stack (undo/redo changes the on-disk state).
                    if let Some(v) = self.state.viewer.as_ref() {
                        let (image_id, path) = (v.image_id, v.path.clone());
                        if let Some(rec) = self.state.images.iter_mut().find(|r| r.id == image_id) {
                            rec.has_edits = !stack.is_identity();
                        }
                        crate::develop::ops_persist::spawn_ops_write(
                            &self.state.jobs, &self.state.writer, &self.state.tx, ctx, image_id, path, stack,
                        );
                    }
                }
            }
        }
```

(Place this block so it does not conflict with the existing ArrowLeft/ArrowRight nav block; both are under the same `Develop + viewer + !wants_keyboard_input` guard — merge into one `ctx.input` read if clippy flags duplicate guards.)

- [ ] **Step 2: Build + clippy** — clean. (Visual: `\` flips to the original and back; `Ctrl+Z` steps back one coalesced edit; `Ctrl+Shift+Z`/`Ctrl+Y` redoes.)

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/app.rs
git commit -m "feat(app): before/after toggle + undo/redo keybindings in Develop"
```

---

## Task 15: `has_edits` filmstrip badge

Draw a small "edited" pip on filmstrip thumbnails whose `ImageRecord.has_edits` is true (the optimistic in-memory cache, refreshed from the DB on query). Stays virtualized. Visual-tested.

**Files:**
- Modify: `ferrolite-app/src/library/filmstrip.rs` (near the rating/flag overlay drawing, ~`:70-90`)

- [ ] **Step 1: Add the badge** — inside the visible-cell block, after the flag overlay:

```rust
                            // "Edited" pip (top-right) when the image carries edits.
                            if rec.has_edits {
                                let c = rect.right_top() + egui::vec2(-7.0, 7.0);
                                ui.painter().circle_filled(c, 3.0, crate::theme::ACCENT_BRIGHT);
                            }
```

(Confirm the loop binds the row as `rec` with a `has_edits` field — it iterates `state.images`, which are `ImageRecord`s, so `rec.has_edits` is available.)

- [ ] **Step 2: Build + clippy** — clean. (Visual: editing an image lights its filmstrip pip immediately; reopening a previously-edited image shows the pip after the read-on-open hydrate / next query.)

- [ ] **Step 3: Commit**

```bash
git add ferrolite-app/src/library/filmstrip.rs
git commit -m "feat(app): has_edits badge on Develop filmstrip thumbnails"
```

---

## Task 16: Workspace gate + hold for the author's visual test

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Then: `cargo fmt --all --check`
Expected: no diff.

- [ ] **Step 2: Clippy (workspace, warnings as errors)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: Tests (workspace; GPU goldens auto-skip headless)**

Run: `cargo test --workspace`
Expected: PASS (all pure units green; GPU/golden tests skip or pass per the dev GPU).

- [ ] **Step 4: Commit any fmt-only changes**

```bash
git add -A
git commit -m "style(app,catalog,vt): cargo fmt Plan 4 sources"
```

- [ ] **Step 5: STOP — hold for the author's hands-on visual test (CLAUDE.md finishing rule)**

The workspace gate being green is **necessary but not sufficient**. Do NOT merge/PR/finish. Present the author with what to verify in the running app, then **hold for their feedback** and address any issues before completing the Spec 2 branch:

1. Open a RAW image → the right 296px adjustment panel appears; Exposure/Contrast/Temp/Tint/Sharpen sliders visibly change the preview **interactively** (sub-frame).
2. Tone Curve: drag/add/delete points; the preview tone shifts; flattening returns to identity.
3. HSL: pick a band, drag Hue/Sat/Lum; the matching color range shifts.
4. Geometry: expand → crop overlay appears over the uncropped image; drag handles (aspect lock works); Angle rotates; collapsing/Reset applies; the 1:1 zoom shows the edited, seam-free full-res tiles.
5. `\` toggles before/after; `Ctrl+Z`/`Ctrl+Y` undo/redo (slider drags are single steps).
6. Reset (per-section + Reset all) clears edits.
7. Edited images show the filmstrip pip; close + reopen → edits persist (read from the `.xmp` sidecar); inspect the sidecar to confirm `frl:ops` co-exists with `xmp:Rating` and any foreign nodes; `has_edits` survives a catalog rebuild.
8. No UI freeze on open/edit/navigate (off-thread I/O; bounded GPU per frame).

Only after the author confirms: use **superpowers:finishing-a-development-branch** to present merge/PR/cleanup options for the Spec 2 branch.

---

## Self-Review

**Spec coverage (§11.4 + §7 + §8):**
- §8.1 Basic/Tone Curve/HSL/Detail/Geometry sections + per-section + global reset → Tasks 10–13.
- §8.1 EguiSlider per param → Task 10 (reuses existing `widgets::slider::EguiSlider`).
- §8.2 bounded undo/redo + coalescing + keys → Tasks 6 (pure) + 14 (keys).
- §8.3 before/after toggle `\` → Task 14.
- §8.4 interactive crop overlay (hit-test/drag/aspect, pure-tested) → Tasks 5 (pure) + 13 (overlay).
- §8.1 interactive tone-curve (pure point math) → Tasks 4 (pure) + 11 (widget).
- §7 `frl:ops` read-on-open + off-thread write + merge-preserving + version tolerance → Tasks 1 (xmp) + 8 (jobs/event) + 9 (read-on-open wiring).
- §7 `images.has_edits` rebuildable cache + badge → Tasks 2 + 9 (optimistic update) + 15 (badge).
- §11.4 "wire slider → new OpStack → mark node dirty → repaint" at preview-res **and** full-res → Tasks 7 (VT seam) + 9 (both tiers).
- §9 error handling: malformed/unknown-version → default (Tasks 1, 8); sidecar write failure → warning, keep in-memory (Task 8 `MetadataResult`); never panics.
- §10 testing split: pure logic unit-tested (Tasks 1–6); egui/GPU via build+clippy+visual (Tasks 7, 9–15); workspace gate then hold (Task 16).

**Placeholder scan:** no "TBD"/"handle edge cases"/"similar to". The pure tasks carry full test + impl code; the rendering tasks carry concrete egui code (no unit tests by spec). Two items intentionally deferred-with-a-note (not placeholders): `viewer::image_screen_rect` is specified as a simple fit-case helper to refine for zoom/pan (Task 13 Step 2); pre-warming the edit-pipeline compute pipelines at startup is a Spec-4 perf item (the preview `EditPipeline` is still built once-per-image, honoring the GPU rule).

**Type consistency:** `OpStack`/`Op`/`OpKind`/op structs match `op.rs` (Plan 1). `EditPipeline::{new,set_stack,evaluate}`, `TileEditPipeline::{new,set_stack}`, `GpuPyramidSource`, `sharpen_halo`, `serialize`/`deserialize` match the `ferrolite-pipeline` public surface. `VirtualTexture::{single_texture,update_single_from_texture,set_producing,set_opstack_version,single_dims}` match `view.rs`. `EditTileProducer::{new,set_stack}` (set_stack added in Task 9). `AppState`/`ViewerState`/`AppEvent`/`spawn_metadata_write` patterns mirror existing app code. `Catalog::set_has_edits`, `ImageRecord.has_edits`, `IMAGE_COLS`, `read_ops`/`write_ops` consistent across catalog tasks. `EditOutcome { stack, kind, commit }` is produced by the panel + all three widgets and consumed by `apply_edit` uniformly.

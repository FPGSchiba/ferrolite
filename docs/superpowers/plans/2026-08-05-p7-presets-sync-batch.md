# P7 — Presets, Copy/Paste/Sync & Batch Edits — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make three phases of adjustment machinery reusable across a shoot — saved presets,
ad-hoc copy/paste, and batch application over a selection — and fix batch export, which currently
renders every image unedited.

**Architecture:** One pure merge function (`EditPatch::apply_to`) is the whole engine; presets,
copy/paste and sync are three sources of the same `EditPatch`. Applying writes N sidecars in one
Background job and flags the affected thumbnails stale; the already-virtualized grid regenerates
them lazily on scroll. Presets are plain JSON files on disk with no catalog table.

**Tech Stack:** Rust, `serde`/`serde_json` (already present), `rusqlite` 0.32, egui/eframe,
`ferrolite-jobs`. **No new dependencies.**

**Spec:** `docs/superpowers/specs/2026-08-05-p7-presets-sync-batch-design.md`

## Global Constraints

- **No new dependencies.** `GroupSet` is a hand-rolled `u16` newtype, not the `bitflags` crate.
- **No engine-tier changes.** Only `ferrolite-pipeline`, `ferrolite-catalog`, `ferrolite-app`,
  `ferrolite-export` may be touched. Never `ferrolite-image`/`-gpu`/`-vt`/`-jobs`/`-mask`.
- **Nothing multi-millisecond on the UI thread.** All file and DB I/O goes through
  `ferrolite-jobs` with a priority, and calls `ctx.request_repaint()` when done.
- **Scoped gate per task** (CLAUDE.md "Gate tiers"). You are a dispatched subagent: run
  `cargo fmt -p <crate> -- --check`, `cargo clippy -p <crate> --all-targets -- -D warnings`,
  `cargo test -p <crate>` for your task's crate(s) **plus any crate consuming your changed API**.
  Do **NOT** run the repo gate — the coordinator runs it once at the end.
- **Naming:** the spec says "Colour"; the codebase says `color_grade` / `ColorSwatch` /
  `ColorControl`. Rust identifiers use `COLOR`; user-visible labels use `"Color"`.
- **`BATCH_UNDO_MAX = 2_000`** — the single named constant bounding the undo snapshot.
- **`PRESET_VERSION = 1`** — preset file schema version; unknown versions load as `None`.
- **Masks are out of scope.** `GroupSet::MASKS` exists as a flag and is always rejected by
  `apply_to`; its UI checkbox is permanently greyed. Do not implement mask merging.
- Every disabled UI control needs an `on_disabled_hover_text` reason (house convention).
- Icons come from `ferrolite-app/src/icons.rs` only — never a raw emoji or `Painter` shape.

---

## File Structure

| File | Responsibility |
|---|---|
| `ferrolite-pipeline/src/patch.rs` **(new)** | `GroupSet`, `EditPatch`, `apply_to`. Pure, no I/O, no GPU. The heart. |
| `ferrolite-pipeline/src/lib.rs` | Re-export `GroupSet`, `EditPatch`. |
| `ferrolite-app/src/presets/mod.rs` **(new)** | `Preset` type, module wiring. |
| `ferrolite-app/src/presets/store.rs` **(new)** | Directory location, filename sanitization, load/save/delete. |
| `ferrolite-app/src/presets/apply.rs` **(new)** | `BatchTarget`, `BatchResult`, `spawn_batch_apply`, undo snapshot type. |
| `ferrolite-app/src/presets/modal.rs` **(new)** | The shared save/paste group-checkbox modal. |
| `ferrolite-catalog/src/schema.rs` | v8 migration: `thumbnails.stale`. |
| `ferrolite-catalog/src/catalog.rs` | `set_thumbnails_stale`, `is_thumbnail_stale`. |
| `ferrolite-app/src/state.rs` | Preset list, clipboard patch, undo snapshot fields. |
| `ferrolite-app/src/library/image_context_menu.rs` | Copy / Paste / Apply preset / Save preset items. |
| `ferrolite-app/src/develop/adjustment_panel.rs` | `Presets ▾` footer button. |
| `ferrolite-app/src/app.rs` | `apply_undo_redo` extension, event handling, modal driving. |
| `ferrolite-app/src/library/grid.rs` | Trigger regeneration when a stale cell realizes. |
| `ferrolite-app/src/develop/thumb_regen.rs` | Clear the stale flag once the new thumbnail persists. |
| `ferrolite-app/src/events.rs` | `BatchApplyDone`, `BatchApplyProgress`, `PresetsLoaded`. |
| `ferrolite-app/src/icons.rs` | `PRESET`, `COPY_SETTINGS`, `PASTE_SETTINGS` aliases. |
| `ferrolite-app/src/export/batch.rs` | Read persisted edits instead of `OpStack::default()`. |

**Task dependency order:** 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10. Tasks 1–5 are logic and
testable without the running app; 6–8 are the UI surface the author's hands-on test judges; 9 is
independent of the rest and could run any time; 10 depends only on 3 but is placed last because it
is the consumer side of the stale flag.

---

## Task 1: `EditPatch` + `GroupSet` + the merge

**Files:**
- Create: `ferrolite-pipeline/src/patch.rs`
- Modify: `ferrolite-pipeline/src/lib.rs` (add `mod patch;` and the re-export)
- Test: in-module `#[cfg(test)] mod tests` in `patch.rs`

**Interfaces:**
- Consumes: `crate::op::{EditDoc, Geometry, LensCorrection}`, `crate::local::AdjustmentSet`.
- Produces:
  - `GroupSet` — `Copy`, `PartialEq`, `Serialize`/`Deserialize` (as `u16`), with associated
    consts `EMPTY LIGHT COLOR CURVE HSL GRADING DETAIL EFFECTS GEOMETRY LENS MASKS`, and methods
    `contains(self, GroupSet) -> bool`, `insert(&mut self, GroupSet)`, `remove(&mut self, GroupSet)`,
    `union(self, GroupSet) -> GroupSet`, `is_empty(self) -> bool`, `bits(self) -> u16`,
    `from_bits(u16) -> GroupSet`, `ALL_APPLICABLE: [GroupSet; 9]`.
  - `EditPatch { version: u32, owns: GroupSet, doc: EditDoc }` with
    `EditPatch::from_doc(&EditDoc, GroupSet) -> EditPatch` and
    `EditPatch::apply_to(&self, &EditDoc) -> EditDoc`.
  - `pub const PATCH_VERSION: u32 = 1;`

- [ ] **Step 1: Write the failing test for group isolation**

Add to `ferrolite-pipeline/src/patch.rs` (create the file with just this test module plus
`use` lines for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::AdjustmentSet;
    use crate::op::{EditDoc, Geometry};

    /// A patch owning only LIGHT writes the light fields and leaves every other
    /// field of the target byte-identical.
    #[test]
    fn owned_group_overwrites_unowned_group_is_untouched() {
        let mut source = EditDoc::default();
        source.global.exposure = 1.5;
        source.global.saturation = 0.9; // COLOR — not owned, must not travel

        let mut target = EditDoc::default();
        target.global.exposure = -0.25;
        target.global.saturation = 0.1;
        target.geometry = Some(Geometry {
            crop: crate::op::CropRect { x: 0.1, y: 0.1, w: 0.8, h: 0.8 },
            ..Default::default()
        });

        let patch = EditPatch::from_doc(&source, GroupSet::LIGHT);
        let out = patch.apply_to(&target);

        assert_eq!(out.global.exposure, 1.5, "owned LIGHT must overwrite");
        assert_eq!(out.global.saturation, 0.1, "unowned COLOR must not travel");
        assert_eq!(out.geometry, target.geometry, "unowned GEOMETRY untouched");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p ferrolite-pipeline --lib patch::`
Expected: FAIL to compile — `EditPatch`, `GroupSet` not found.

- [ ] **Step 3: Implement `GroupSet`**

At the top of `patch.rs`:

```rust
//! `EditPatch` — a partial `EditDoc` plus the set of groups it authoritatively
//! writes (P7 design §3). The single currency of presets, copy/paste and sync:
//! all three build one of these and call `apply_to`.
//!
//! Hand-rolled bitflags rather than the `bitflags` crate — P7 adds no
//! dependencies (design §1.7).

use serde::{Deserialize, Serialize};

use crate::op::EditDoc;

/// Preset/patch schema version. Bump only on a breaking layout change.
pub const PATCH_VERSION: u32 = 1;

/// Which adjustment groups a patch writes. Groups outside the set are ignored
/// on read and left untouched on the target.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GroupSet(u16);

impl GroupSet {
    pub const EMPTY: GroupSet = GroupSet(0);
    pub const LIGHT: GroupSet = GroupSet(1 << 0);
    pub const COLOR: GroupSet = GroupSet(1 << 1);
    pub const CURVE: GroupSet = GroupSet(1 << 2);
    pub const HSL: GroupSet = GroupSet(1 << 3);
    pub const GRADING: GroupSet = GroupSet(1 << 4);
    pub const DETAIL: GroupSet = GroupSet(1 << 5);
    pub const EFFECTS: GroupSet = GroupSet(1 << 6);
    pub const GEOMETRY: GroupSet = GroupSet(1 << 7);
    pub const LENS: GroupSet = GroupSet(1 << 8);
    /// Present so a future phase can enable it; `apply_to` always ignores it
    /// (P7 design §2 P7-D2 — masks are out of scope).
    pub const MASKS: GroupSet = GroupSet(1 << 9);

    /// Every group `apply_to` actually honors, in UI order. Excludes MASKS.
    pub const ALL_APPLICABLE: [GroupSet; 9] = [
        GroupSet::LIGHT,
        GroupSet::COLOR,
        GroupSet::CURVE,
        GroupSet::HSL,
        GroupSet::GRADING,
        GroupSet::DETAIL,
        GroupSet::EFFECTS,
        GroupSet::GEOMETRY,
        GroupSet::LENS,
    ];

    pub fn contains(self, other: GroupSet) -> bool {
        self.0 & other.0 == other.0 && other.0 != 0
    }
    pub fn insert(&mut self, other: GroupSet) {
        self.0 |= other.0;
    }
    pub fn remove(&mut self, other: GroupSet) {
        self.0 &= !other.0;
    }
    pub fn union(self, other: GroupSet) -> GroupSet {
        GroupSet(self.0 | other.0)
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub fn bits(self) -> u16 {
        self.0
    }
    pub fn from_bits(bits: u16) -> GroupSet {
        GroupSet(bits)
    }
}
```

- [ ] **Step 4: Implement `EditPatch` and the merge**

Append to `patch.rs`, above the test module:

```rust
/// A partial edit document: values plus the groups it authoritatively writes.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct EditPatch {
    pub version: u32,
    pub owns: GroupSet,
    /// Value carrier. Only fields in an owned group are meaningful; the rest
    /// hold whatever `Default` produced.
    pub doc: EditDoc,
}

impl EditPatch {
    /// Capture `owns`'s groups from `doc`.
    pub fn from_doc(doc: &EditDoc, owns: GroupSet) -> Self {
        Self {
            version: PATCH_VERSION,
            owns,
            doc: doc.clone(),
        }
    }

    /// Return `target` with every owned group replaced by this patch's values.
    ///
    /// `GroupSet::MASKS` is deliberately NOT handled: mask layers are out of
    /// P7 (design §2 P7-D2), so `layers` always survives from the target even
    /// if a future preset file sets the flag. That is the safe direction — a
    /// patch can never silently destroy mask work.
    pub fn apply_to(&self, target: &EditDoc) -> EditDoc {
        let mut out = target.clone();
        let s = &self.doc;

        if self.owns.contains(GroupSet::LIGHT) {
            out.global.exposure = s.global.exposure;
            out.global.contrast = s.global.contrast;
            out.global.highlights = s.global.highlights;
            out.global.shadows = s.global.shadows;
            out.global.whites = s.global.whites;
            out.global.blacks = s.global.blacks;
        }
        if self.owns.contains(GroupSet::COLOR) {
            out.global.temp = s.global.temp;
            out.global.tint = s.global.tint;
            out.global.saturation = s.global.saturation;
            out.global.hue = s.global.hue;
            out.global.vibrance = s.global.vibrance;
            out.global.color = s.global.color;
        }
        if self.owns.contains(GroupSet::CURVE) {
            out.global.tone_curve = s.global.tone_curve.clone();
        }
        if self.owns.contains(GroupSet::HSL) {
            out.global.hsl = s.global.hsl.clone();
        }
        if self.owns.contains(GroupSet::GRADING) {
            out.global.color_grade = s.global.color_grade.clone();
        }
        if self.owns.contains(GroupSet::DETAIL) {
            out.global.sharpen = s.global.sharpen;
            out.global.noise_reduction = s.global.noise_reduction;
        }
        if self.owns.contains(GroupSet::EFFECTS) {
            out.global.dehaze = s.global.dehaze;
        }
        if self.owns.contains(GroupSet::GEOMETRY) {
            out.geometry = s.geometry;
        }
        if self.owns.contains(GroupSet::LENS) {
            apply_lens_amounts(&mut out, s);
        }
        out
    }
}

/// Copy ONLY the three correction amounts, never the capture context.
///
/// `LensCorrection` carries `lens_id`, `focal_len`, `aperture` and
/// `crop_factor` — all per-image EXIF. Copying those would stamp the source's
/// focal length onto the target and bake a wrong correction (design §3.2,
/// load-bearing). If either side has no `LensCorrection`, this is a no-op: an
/// unmatched target has no context to attach amounts to.
fn apply_lens_amounts(out: &mut EditDoc, source: &EditDoc) {
    let (Some(src), Some(dst)) = (source.lens.as_ref(), out.lens.as_mut()) else {
        return;
    };
    dst.distortion = src.distortion;
    dst.tca = src.tca;
    dst.vignetting = src.vignetting;
}
```

Then in `ferrolite-pipeline/src/lib.rs` add `mod patch;` next to `mod op;`, and add to the
public re-exports:

```rust
pub use patch::{EditPatch, GroupSet, PATCH_VERSION};
```

- [ ] **Step 5: Run the test and watch it pass**

Run: `cargo test -p ferrolite-pipeline --lib patch::`
Expected: PASS.

- [ ] **Step 6: Add the load-bearing lens test — write it first, watch it fail**

The `apply_lens_amounts` code above already satisfies this, so to keep the TDD cycle honest:
**temporarily** change `apply_lens_amounts` to `*dst = src.clone();`, add the test, confirm it
FAILS, then restore the correct body and confirm it PASSES. Record both outcomes in your report.

```rust
    /// LENS must carry the three correction AMOUNTS and never the capture
    /// context. Copying `focal_len`/`lens_id` would bake the source lens's
    /// correction into a photo shot on a different lens.
    #[test]
    fn lens_group_copies_amounts_but_never_capture_context() {
        use crate::op::{Correction, LensCorrection};
        let lens = |id: &str, focal: f32, amount: f32| LensCorrection {
            lens_id: Some(id.to_string()),
            focal_len: focal,
            aperture: 2.8,
            crop_factor: 1.0,
            distortion: Correction { enabled: true, amount },
            tca: Correction { enabled: true, amount },
            vignetting: Correction { enabled: true, amount },
        };

        let mut source = EditDoc::default();
        source.lens = Some(lens("sony-fe-16-35", 16.0, 0.8));
        let mut target = EditDoc::default();
        target.lens = Some(lens("nikon-35mm", 35.0, 0.2));

        let out = EditPatch::from_doc(&source, GroupSet::LENS).apply_to(&target);
        let got = out.lens.expect("target keeps its lens");

        assert_eq!(got.distortion.amount, 0.8, "amount must travel");
        assert_eq!(got.tca.amount, 0.8, "amount must travel");
        assert_eq!(got.vignetting.amount, 0.8, "amount must travel");
        assert_eq!(
            got.lens_id.as_deref(),
            Some("nikon-35mm"),
            "lens_id must NOT travel"
        );
        assert_eq!(got.focal_len, 35.0, "focal_len must NOT travel");
        assert_eq!(got.aperture, 2.8);
        assert_eq!(got.crop_factor, 1.0);
    }

    /// An unmatched target has no context to attach amounts to — no-op, no panic.
    #[test]
    fn lens_group_is_a_noop_when_the_target_has_no_lens() {
        use crate::op::{Correction, LensCorrection};
        let mut source = EditDoc::default();
        source.lens = Some(LensCorrection {
            lens_id: Some("x".into()),
            focal_len: 16.0,
            aperture: 2.8,
            crop_factor: 1.0,
            distortion: Correction { enabled: true, amount: 1.0 },
            tca: Correction { enabled: true, amount: 1.0 },
            vignetting: Correction { enabled: true, amount: 1.0 },
        });
        let target = EditDoc::default(); // lens: None
        let out = EditPatch::from_doc(&source, GroupSet::LENS).apply_to(&target);
        assert!(out.lens.is_none(), "must not fabricate a LensCorrection");
    }
```

- [ ] **Step 7: Add the identity-is-expressible test**

This is the entire justification for `owns` (design §2 P7-D3). Write it, run it, confirm PASS:

```rust
    /// A patch owning LIGHT with exposure == 0.0 must SET the target's exposure
    /// to 0, not skip it. If this ever fails, `owns` has been replaced by an
    /// is-identity check somewhere and presets can no longer reset a control.
    #[test]
    fn an_owned_identity_value_still_overwrites() {
        let source = EditDoc::default(); // exposure == 0.0
        let mut target = EditDoc::default();
        target.global.exposure = 2.0;

        let out = EditPatch::from_doc(&source, GroupSet::LIGHT).apply_to(&target);
        assert_eq!(out.global.exposure, 0.0, "identity value must still be written");
    }

    /// MASKS is never honored in P7 — a patch claiming it must not touch layers.
    #[test]
    fn masks_group_never_modifies_layers() {
        use crate::local::MaskLayer;
        let source = EditDoc::default();
        let mut target = EditDoc::default();
        target.layers = vec![MaskLayer {
            name: "Sky".into(),
            visible: true,
            mask: Default::default(),
            adjustments: AdjustmentSet::default(),
        }];
        let out = EditPatch::from_doc(&source, GroupSet::MASKS).apply_to(&target);
        assert_eq!(out.layers.len(), 1, "target's mask layers must survive");
        assert_eq!(out.layers[0].name, "Sky");
    }

    /// An empty patch is the identity transform.
    #[test]
    fn empty_patch_returns_the_target_unchanged() {
        let mut source = EditDoc::default();
        source.global.exposure = 9.0;
        let mut target = EditDoc::default();
        target.global.exposure = 1.0;
        let out = EditPatch::from_doc(&source, GroupSet::EMPTY).apply_to(&target);
        assert_eq!(out, target);
    }

    #[test]
    fn group_set_contains_insert_remove_roundtrip() {
        let mut g = GroupSet::EMPTY;
        assert!(g.is_empty());
        assert!(!g.contains(GroupSet::LIGHT));
        g.insert(GroupSet::LIGHT);
        g.insert(GroupSet::LENS);
        assert!(g.contains(GroupSet::LIGHT) && g.contains(GroupSet::LENS));
        assert!(!g.contains(GroupSet::COLOR));
        g.remove(GroupSet::LIGHT);
        assert!(!g.contains(GroupSet::LIGHT) && g.contains(GroupSet::LENS));
        assert_eq!(GroupSet::from_bits(g.bits()), g);
        // EMPTY is contained by nothing — guards the `other.0 != 0` clause.
        assert!(!g.contains(GroupSet::EMPTY));
    }
```

- [ ] **Step 8: Scoped gate**

```bash
cargo fmt -p ferrolite-pipeline -- --check
cargo clippy -p ferrolite-pipeline --all-targets -- -D warnings
cargo test -p ferrolite-pipeline --lib patch::
```
All must pass. Do NOT run the repo gate.

- [ ] **Step 9: Commit**

```bash
git add ferrolite-pipeline/src/patch.rs ferrolite-pipeline/src/lib.rs
git commit -m "feat(pipeline): EditPatch + GroupSet — partial edit-document merge

The engine of P7: a patch carries values plus the set of groups it
authoritatively writes, and apply_to merges only those into a target.

LENS copies the three correction amounts and never lens_id/focal_len/
aperture/crop_factor, which are per-image EXIF — copying them would bake
the source lens's correction into a photo shot on a different lens.
MASKS is accepted as a flag but never honored (masks are out of P7), so a
patch can never destroy mask work."
```

---

## Task 2: Preset store (files on disk)

**Files:**
- Create: `ferrolite-app/src/presets/mod.rs`, `ferrolite-app/src/presets/store.rs`
- Modify: `ferrolite-app/src/main.rs` **or** wherever the module list lives — add `mod presets;`
  (check `ferrolite-app/src/main.rs` first; follow whichever file declares `mod library;`)
- Test: in-module `#[cfg(test)] mod tests` in `store.rs`

**Interfaces:**
- Consumes: `ferrolite_pipeline::{EditDoc, EditPatch, GroupSet, PATCH_VERSION}` (Task 1).
- Produces:
  - `Preset { pub version: u32, pub name: String, pub owns: GroupSet, pub doc: EditDoc }`
  - `presets_dir() -> std::path::PathBuf`
  - `sanitize_filename(&str) -> Option<String>`
  - `load_all(&Path) -> Vec<Preset>`
  - `save(&Path, &Preset) -> Result<PathBuf, PresetError>`
  - `delete(&Path, &Preset) -> Result<(), PresetError>`
  - `PresetError` (`thiserror`, already a workspace dep): `Io(std::io::Error)`,
    `InvalidName`, `Duplicate(String)`
  - `impl Preset { pub fn to_patch(&self) -> EditPatch }`

- [ ] **Step 1: Write the failing sanitization tests**

Create `ferrolite-app/src/presets/store.rs` with only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_invalid_chars_and_collapses_runs() {
        assert_eq!(sanitize_filename("Warm portrait").as_deref(), Some("Warm portrait"));
        assert_eq!(sanitize_filename("Warm/Cool").as_deref(), Some("Warm_Cool"));
        assert_eq!(sanitize_filename("a***b").as_deref(), Some("a_b"), "runs collapse");
        assert_eq!(sanitize_filename("  padded  ").as_deref(), Some("padded"), "trimmed");
    }

    #[test]
    fn sanitize_rejects_empty_and_all_invalid_names() {
        assert_eq!(sanitize_filename(""), None);
        assert_eq!(sanitize_filename("   "), None);
        assert_eq!(sanitize_filename("///"), None, "all-invalid collapses to nothing usable");
    }

    #[test]
    fn sanitize_truncates_to_64_chars() {
        let long = "x".repeat(200);
        assert_eq!(sanitize_filename(&long).unwrap().len(), 64);
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p ferrolite-app --lib presets::store::`
Expected: FAIL to compile — `sanitize_filename` not found.

- [ ] **Step 3: Implement the store**

Prepend to `store.rs`:

```rust
//! Preset persistence: plain JSON files on disk, no catalog table.
//!
//! Contract 2 says the catalog is a cache rebuildable from disk. A user-authored
//! preset is NOT derivable from image files, so it cannot live only in the
//! catalog — it is a file. With the file as the source of truth a catalog index
//! buys nothing at realistic scale (tens to low hundreds of small JSON files
//! read once at startup), so no table is added: nothing cached means nothing
//! that can go stale. (P7 design §4.)

use std::path::{Path, PathBuf};

use ferrolite_pipeline::{EditDoc, EditPatch, GroupSet, PATCH_VERSION};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("preset name is empty or contains no usable characters")]
    InvalidName,
    #[error("a preset already exists with the filename \"{0}\"")]
    Duplicate(String),
}

/// One saved preset. The DISPLAY name lives inside the file; the filename is a
/// lossy sanitization of it (see `sanitize_filename`), so the sanitized form is
/// never shown to the user.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub version: u32,
    pub name: String,
    pub owns: GroupSet,
    pub doc: EditDoc,
}

impl Preset {
    pub fn to_patch(&self) -> EditPatch {
        EditPatch {
            version: self.version,
            owns: self.owns,
            doc: self.doc.clone(),
        }
    }
}

/// `<base>/ferrolite/presets`, resolved by the same logic as `catalog.db`
/// (`state::default_db_path`): LOCALAPPDATA, else XDG_DATA_HOME, else HOME,
/// else the current directory.
pub fn presets_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ferrolite").join("presets")
}

/// Derive a safe filename stem from a display name: every character outside
/// `[A-Za-z0-9 _-]` becomes `_`, runs of `_` collapse, the result is trimmed and
/// truncated to 64 chars. `None` when nothing usable remains.
///
/// Deliberately LOSSY — `Warm/Cool` and `Warm_Cool` collide — so uniqueness is
/// checked against this output, not the display name (see `save`).
pub fn sanitize_filename(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut last_underscore = false;
    for ch in name.trim().chars() {
        let ok = ch.is_ascii_alphanumeric() || ch == ' ' || ch == '_' || ch == '-';
        if ok {
            out.push(ch);
            last_underscore = ch == '_';
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    let trimmed = out.trim().trim_matches('_').trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(64).collect())
}

/// Read every `*.json` in `dir`. Unreadable, malformed, or wrong-version files
/// are SKIPPED (never a panic), mirroring `ferrolite_pipeline::deserialize`'s
/// contract. Returns presets sorted by display name, case-insensitively.
pub fn load_all(dir: &Path) -> Vec<Preset> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new(); // no dir yet == no presets
    };
    let mut out: Vec<Preset> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .filter_map(|text| serde_json::from_str::<Preset>(&text).ok())
        .filter(|p| p.version == PATCH_VERSION)
        .collect();
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

/// Write `preset` as `<dir>/<sanitized>.json`, creating `dir` if needed.
/// Rejects a name that sanitizes to nothing, and rejects a filename collision
/// rather than silently overwriting.
pub fn save(dir: &Path, preset: &Preset) -> Result<PathBuf, PresetError> {
    let stem = sanitize_filename(&preset.name).ok_or(PresetError::InvalidName)?;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{stem}.json"));
    if path.exists() {
        return Err(PresetError::Duplicate(stem));
    }
    let json = serde_json::to_string_pretty(preset).expect("Preset is always serializable");
    std::fs::write(&path, json)?;
    Ok(path)
}

/// Remove the file backing `preset`. A missing file is NOT an error — the
/// desired end state (no such preset) already holds.
pub fn delete(dir: &Path, preset: &Preset) -> Result<(), PresetError> {
    let Some(stem) = sanitize_filename(&preset.name) else {
        return Err(PresetError::InvalidName);
    };
    let path = dir.join(format!("{stem}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(PresetError::Io(e)),
    }
}
```

Create `ferrolite-app/src/presets/mod.rs`:

```rust
//! Presets, copy/paste/sync and batch apply (P7).

pub mod store;

pub use store::{delete, load_all, presets_dir, sanitize_filename, save, Preset, PresetError};
```

Add `mod presets;` alongside the other `mod` declarations (check `main.rs` first — follow
whichever file declares `mod library;`).

- [ ] **Step 4: Run the sanitization tests and watch them pass**

Run: `cargo test -p ferrolite-app --lib presets::store::`
Expected: PASS (3 tests).

- [ ] **Step 5: Write the round-trip and version-gate tests, watch them fail, then pass**

Add to `store.rs`'s test module. These need a temp dir; follow the existing house pattern —
`ferrolite-catalog/tests/catalog.rs` has a `tempdir()` helper; copy its approach (a
`std::env::temp_dir().join(format!("ferrolite-preset-test-{}", std::process::id()))` created with
`create_dir_all` and removed at the end).

```rust
    fn tmp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ferrolite-preset-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample(name: &str) -> Preset {
        let mut doc = EditDoc::default();
        doc.global.exposure = 0.75;
        Preset { version: PATCH_VERSION, name: name.into(), owns: GroupSet::LIGHT, doc }
    }

    #[test]
    fn save_then_load_all_round_trips() {
        let dir = tmp();
        let p = sample("Warm portrait");
        save(&dir, &p).expect("save");
        let loaded = load_all(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_rejects_a_filename_collision_rather_than_overwriting() {
        let dir = tmp();
        save(&dir, &sample("Warm/Cool")).expect("first save");
        // Sanitizes to the SAME stem "Warm_Cool" — must be refused.
        let err = save(&dir, &sample("Warm_Cool")).expect_err("collision must be refused");
        assert!(matches!(err, PresetError::Duplicate(_)), "got {err:?}");
        assert_eq!(load_all(&dir).len(), 1, "the original must survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_rejects_an_unusable_name() {
        let dir = tmp();
        let err = save(&dir, &sample("///")).expect_err("must reject");
        assert!(matches!(err, PresetError::InvalidName), "got {err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_all_skips_malformed_and_wrong_version_files_without_panicking() {
        let dir = tmp();
        save(&dir, &sample("Good")).expect("save");
        std::fs::write(dir.join("garbage.json"), "not json {{").unwrap();
        std::fs::write(
            dir.join("future.json"),
            r#"{"version":999,"name":"Future","owns":1,"doc":{"version":2}}"#,
        )
        .unwrap();
        let loaded = load_all(&dir);
        assert_eq!(loaded.len(), 1, "only the good preset loads");
        assert_eq!(loaded[0].name, "Good");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_the_file_and_is_idempotent() {
        let dir = tmp();
        let p = sample("Gone");
        save(&dir, &p).expect("save");
        delete(&dir, &p).expect("delete");
        assert!(load_all(&dir).is_empty());
        delete(&dir, &p).expect("second delete is a no-op, not an error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_all_on_a_missing_directory_is_empty_not_an_error() {
        assert!(load_all(&std::path::Path::new("definitely/not/here")).is_empty());
    }
```

Run: `cargo test -p ferrolite-app --lib presets::store::` — expect FAIL first if any behavior is
missing, then PASS after the Step 3 code is in place. Record which failed first in your report.

- [ ] **Step 6: Scoped gate**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app --lib presets::
```

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/presets/
git commit -m "feat(app): preset store — JSON files on disk, no catalog table

Contract 2 makes this the only correct shape: a user-authored preset is not
derivable from image files, so it cannot live only in the catalog. With the
file as source of truth an index buys nothing at realistic scale, so no
table is added at all.

Filename sanitization is lossy on purpose, so uniqueness is checked against
the sanitized stem rather than the display name — Warm/Cool and Warm_Cool
collide and the second is refused instead of silently overwriting the first.
Malformed and wrong-version files are skipped, never a panic."
```

---

## Task 3: Schema v8 — `thumbnails.stale`

**Files:**
- Modify: `ferrolite-catalog/src/schema.rs` (bump `SCHEMA_VERSION` 7→8, add the migration block,
  add the migration test)
- Modify: `ferrolite-catalog/src/catalog.rs` (add two methods)
- Test: in-module tests in both files

**Interfaces:**
- Produces:
  - `Catalog::set_thumbnails_stale(&self, image_ids: &[i64], stale: bool) -> Result<usize, CatalogError>`
    — returns rows updated.
  - `Catalog::is_thumbnail_stale(&self, image_id: i64) -> Result<bool, CatalogError>` — `false`
    when no thumbnail row exists.

- [ ] **Step 1: Write the failing migration test**

In `ferrolite-catalog/src/schema.rs`'s existing test module, mirroring
`migrate_creates_v7_lens_aperture_focal_columns`:

```rust
    #[test]
    fn migrate_creates_v8_thumbnail_stale_column_defaulting_fresh() {
        let conn = Connection::open_in_memory().unwrap();
        super::migrate(&conn).unwrap();
        let cols = table_columns(&conn, "thumbnails");
        assert!(cols.contains(&"stale".to_string()), "stale column added");

        // Existing rows must default to FRESH so upgrading an installed
        // catalog does not trigger a library-wide thumbnail regeneration.
        conn.execute(
            "INSERT INTO folders (id, path) VALUES (1, 'p')",
            [],
        )
        .ok();
        conn.execute(
            "INSERT INTO images (id, folder_id, filename, mtime, size)
             VALUES (1, 1, 'a.arw', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO thumbnails (image_id, level, w, h, format, blob)
             VALUES (1, 0, 8, 8, 'jpeg', x'00')",
            [],
        )
        .unwrap();
        let stale: i64 = conn
            .query_row("SELECT stale FROM thumbnails WHERE image_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stale, 0, "new rows default to fresh");
    }
```

> If the `folders` insert signature differs, read the `CREATE TABLE folders` statement at the top
> of `schema.rs` and adjust the columns — do not change the assertion.

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p ferrolite-catalog --lib schema::tests::migrate_creates_v8`
Expected: FAIL — "stale column added".

- [ ] **Step 3: Add the migration**

In `schema.rs`, change `pub const SCHEMA_VERSION: i64 = 7;` to `8`, and add after the `version < 7`
block:

```rust
    if version < 8 {
        // Thumbnail staleness for batch edits (P7 design §5.2). A batch apply
        // writes N sidecars and flags the affected thumbnails; the virtualized
        // grid regenerates a cell when it realizes, then clears the flag.
        //
        // DEFAULT 0 (fresh) is load-bearing: upgrading an existing catalog must
        // not mark every thumbnail stale and trigger a library-wide
        // regeneration on first launch.
        //
        // A re-derivable cache column, which contract 2 explicitly permits.
        conn.execute_batch(
            "ALTER TABLE thumbnails ADD COLUMN stale INTEGER NOT NULL DEFAULT 0;",
        )?;
        version = 8;
    }
```

- [ ] **Step 4: Run and watch it pass**

Run: `cargo test -p ferrolite-catalog --lib schema::`
Expected: PASS, including the pre-existing
`debug_assert_eq!(version, SCHEMA_VERSION)` at the end of `migrate`.

- [ ] **Step 5: Write the failing accessor tests**

In `ferrolite-catalog/src/catalog.rs`'s test module (follow its existing helpers for opening a
temp `Catalog` and inserting an image + thumbnail — reuse whatever
`get_thumbnail`'s tests already do):

```rust
    #[test]
    fn set_thumbnails_stale_flags_only_the_given_ids() {
        let (_dir, cat, ids) = catalog_with_three_thumbnails();
        assert_eq!(cat.set_thumbnails_stale(&[ids[0], ids[2]], true).unwrap(), 2);
        assert!(cat.is_thumbnail_stale(ids[0]).unwrap());
        assert!(!cat.is_thumbnail_stale(ids[1]).unwrap(), "untouched id stays fresh");
        assert!(cat.is_thumbnail_stale(ids[2]).unwrap());
    }

    #[test]
    fn set_thumbnails_stale_clears_the_flag_too() {
        let (_dir, cat, ids) = catalog_with_three_thumbnails();
        cat.set_thumbnails_stale(&[ids[0]], true).unwrap();
        cat.set_thumbnails_stale(&[ids[0]], false).unwrap();
        assert!(!cat.is_thumbnail_stale(ids[0]).unwrap());
    }

    #[test]
    fn is_thumbnail_stale_is_false_when_no_thumbnail_row_exists() {
        let (_dir, cat, _ids) = catalog_with_three_thumbnails();
        assert!(!cat.is_thumbnail_stale(999_999).unwrap());
    }

    #[test]
    fn set_thumbnails_stale_on_an_empty_slice_is_a_noop() {
        let (_dir, cat, _ids) = catalog_with_three_thumbnails();
        assert_eq!(cat.set_thumbnails_stale(&[], true).unwrap(), 0);
    }
```

Write `catalog_with_three_thumbnails()` as a local test helper returning
`(TempDirGuard, Catalog, [i64; 3])`, built from the crate's existing test scaffolding.

- [ ] **Step 6: Run and watch them fail, then implement**

Run: `cargo test -p ferrolite-catalog --lib catalog::tests::set_thumbnails_stale`
Expected: FAIL to compile.

Add to `impl Catalog` in `catalog.rs`:

```rust
    /// Mark (or clear) the stale flag on the given images' thumbnails. Returns
    /// the number of thumbnail rows updated — ids without a thumbnail row are
    /// silently skipped, which is correct: there is nothing cached to
    /// invalidate. See P7 design §5.2.
    pub fn set_thumbnails_stale(
        &self,
        image_ids: &[i64],
        stale: bool,
    ) -> Result<usize, CatalogError> {
        if image_ids.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut updated = 0usize;
        {
            let mut stmt = tx.prepare("UPDATE thumbnails SET stale = ?1 WHERE image_id = ?2")?;
            for id in image_ids {
                updated += stmt.execute(rusqlite::params![stale as i64, id])?;
            }
        }
        tx.commit()?;
        Ok(updated)
    }

    /// Whether this image's thumbnail needs regenerating. `false` when no
    /// thumbnail row exists (nothing cached, so nothing stale).
    pub fn is_thumbnail_stale(&self, image_id: i64) -> Result<bool, CatalogError> {
        let v: Option<i64> = self
            .conn
            .query_row(
                "SELECT stale FROM thumbnails WHERE image_id = ?1",
                rusqlite::params![image_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.unwrap_or(0) != 0)
    }
```

> `optional()` needs `use rusqlite::OptionalExtension;` — check whether `catalog.rs` already
> imports it and add it if not. If `self.conn` is wrapped differently (e.g. behind a field name
> other than `conn`), follow the file's existing accessor style rather than this literal code.

- [ ] **Step 7: Run and watch them pass**

Run: `cargo test -p ferrolite-catalog`
Expected: PASS, all suites.

- [ ] **Step 8: Scoped gate**

```bash
cargo fmt -p ferrolite-catalog -- --check
cargo clippy -p ferrolite-catalog --all-targets -- -D warnings
cargo test -p ferrolite-catalog
cargo test -p ferrolite-app   # consumes Catalog's API
```

- [ ] **Step 9: Commit**

```bash
git add ferrolite-catalog/src/schema.rs ferrolite-catalog/src/catalog.rs
git commit -m "feat(catalog): schema v8 — thumbnails.stale for batch edits

A batch apply flags affected thumbnails; the virtualized grid regenerates a
cell when it realizes it. DEFAULT 0 is load-bearing: upgrading an existing
catalog must not mark every thumbnail stale and regenerate the whole library
on first launch.

A re-derivable cache column, which contract 2 explicitly permits."
```

---

## Task 4: The batch apply job

**Files:**
- Create: `ferrolite-app/src/presets/apply.rs`
- Modify: `ferrolite-app/src/presets/mod.rs` (add `pub mod apply;`)
- Modify: `ferrolite-app/src/events.rs` (two new `AppEvent` variants)
- Test: in-module tests in `apply.rs`

**Interfaces:**
- Consumes: `EditPatch` (Task 1), `Catalog::set_thumbnails_stale` (Task 3),
  `ferrolite_catalog::{sidecar_path, read_ops, write_ops}`,
  `ferrolite_pipeline::{serialize, deserialize}`, `JobSystem`, `AppEvent`.
- Produces:
  - `BatchTarget { pub image_id: i64, pub path: PathBuf }`
  - `BatchResult { pub applied: usize, pub failed: usize, pub skipped: usize }`
  - `UndoSnapshot { pub entries: Vec<(i64, PathBuf, String)> }` — `(image_id, path,
    prior serialized doc)`
  - `pub const BATCH_UNDO_MAX: usize = 2_000;`
  - `apply_patch_to_targets(...) -> (BatchResult, UndoSnapshot)` — the **pure-ish** core,
    parameterized over read/write closures so it is testable without a real filesystem.
  - `spawn_batch_apply(...)` — the job wrapper.
  - `AppEvent::BatchApplyDone { result: BatchResult, snapshot: Option<UndoSnapshot>, label: String }`
  - `AppEvent::BatchApplyProgress { done: usize, total: usize }`

- [ ] **Step 1: Write the failing core test**

Create `ferrolite-app/src/presets/apply.rs` with the test module only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{EditDoc, EditPatch, GroupSet};
    use std::collections::HashMap;

    fn target(id: i64) -> BatchTarget {
        BatchTarget { image_id: id, path: std::path::PathBuf::from(format!("/img/{id}.arw")) }
    }

    /// Applies the patch to every target, returns accurate counts, and captures
    /// each target's PRIOR document for undo.
    #[test]
    fn applies_to_all_targets_and_snapshots_prior_docs() {
        let mut store: HashMap<i64, EditDoc> = HashMap::new();
        for id in 1..=3 {
            let mut d = EditDoc::default();
            d.global.exposure = id as f32;
            store.insert(id, d);
        }
        let written: std::cell::RefCell<HashMap<i64, EditDoc>> =
            std::cell::RefCell::new(HashMap::new());

        let mut source = EditDoc::default();
        source.global.exposure = 9.0;
        let patch = EditPatch::from_doc(&source, GroupSet::LIGHT);

        let (result, snap) = apply_patch_to_targets(
            &patch,
            &[target(1), target(2), target(3)],
            |t| store.get(&t.image_id).cloned(),
            |t, doc| {
                written.borrow_mut().insert(t.image_id, doc.clone());
                Ok(())
            },
            &mut |_done, _total| {},
        );

        assert_eq!(result.applied, 3);
        assert_eq!(result.failed, 0);
        assert_eq!(result.skipped, 0);
        for id in 1..=3 {
            assert_eq!(written.borrow()[&id].global.exposure, 9.0);
        }
        assert_eq!(snap.entries.len(), 3, "one snapshot entry per applied target");
    }

    /// A write failure is counted, does NOT abort the batch, and the failed
    /// target contributes no undo entry (there is nothing to roll back).
    #[test]
    fn a_failed_write_is_counted_and_does_not_abort_the_batch() {
        let mut store: HashMap<i64, EditDoc> = HashMap::new();
        for id in 1..=3 {
            store.insert(id, EditDoc::default());
        }
        let patch = EditPatch::from_doc(&EditDoc::default(), GroupSet::LIGHT);

        let (result, snap) = apply_patch_to_targets(
            &patch,
            &[target(1), target(2), target(3)],
            |t| store.get(&t.image_id).cloned(),
            |t, _doc| {
                if t.image_id == 2 {
                    Err("read-only".to_string())
                } else {
                    Ok(())
                }
            },
            &mut |_d, _t| {},
        );

        assert_eq!(result.applied, 2);
        assert_eq!(result.failed, 1);
        assert_eq!(snap.entries.len(), 2, "no undo entry for the failed write");
    }

    /// A target whose current document cannot be read is SKIPPED, not failed —
    /// they are different outcomes and the toast reports them separately.
    #[test]
    fn an_unreadable_target_is_skipped_not_failed() {
        let patch = EditPatch::from_doc(&EditDoc::default(), GroupSet::LIGHT);
        let (result, snap) = apply_patch_to_targets(
            &patch,
            &[target(1)],
            |_t| None,
            |_t, _doc| Ok(()),
            &mut |_d, _t| {},
        );
        assert_eq!(result.skipped, 1);
        assert_eq!(result.applied, 0);
        assert_eq!(result.failed, 0);
        assert!(snap.entries.is_empty());
    }

    /// Past BATCH_UNDO_MAX no snapshot is taken — the dialog warns up front.
    #[test]
    fn no_snapshot_is_taken_beyond_the_undo_cap() {
        let targets: Vec<BatchTarget> = (1..=(BATCH_UNDO_MAX as i64 + 1)).map(target).collect();
        let patch = EditPatch::from_doc(&EditDoc::default(), GroupSet::LIGHT);
        let (result, snap) = apply_patch_to_targets(
            &patch,
            &targets,
            |_t| Some(EditDoc::default()),
            |_t, _doc| Ok(()),
            &mut |_d, _t| {},
        );
        assert_eq!(result.applied, targets.len());
        assert!(snap.entries.is_empty(), "over the cap, no snapshot is retained");
    }

    #[test]
    fn progress_is_reported_for_every_target() {
        let patch = EditPatch::from_doc(&EditDoc::default(), GroupSet::LIGHT);
        let seen = std::cell::RefCell::new(Vec::new());
        let _ = apply_patch_to_targets(
            &patch,
            &[target(1), target(2)],
            |_t| Some(EditDoc::default()),
            |_t, _doc| Ok(()),
            &mut |done, total| seen.borrow_mut().push((done, total)),
        );
        assert_eq!(*seen.borrow(), vec![(1, 2), (2, 2)]);
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p ferrolite-app --lib presets::apply::`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the core**

Prepend to `apply.rs`:

```rust
//! Batch application of an `EditPatch` to N images (P7 design §5).
//!
//! **One job for the whole batch, not one-at-a-time.** Batch EXPORT processes
//! items sequentially because each is a full-res render plus a CPU-heavy encode
//! and running several saturated the machine (see `export/batch.rs`'s module
//! doc). Batch EDIT does no rendering at all — N sidecar writes is milliseconds
//! of I/O — so that constraint does not transfer and is not inherited.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ferrolite_catalog::Catalog;
use ferrolite_jobs::{JobSystem, Priority};
use ferrolite_pipeline::{EditDoc, EditPatch};

use crate::events::AppEvent;

/// Beyond this many targets no undo snapshot is retained. A serialized
/// `EditDoc` runs 0.5–2 KB, so 2,000 snapshots costs a few MB — comfortably
/// above any realistic batch, while ruling out a 50,000-image select-all
/// pinning ~100 MB for the session. The apply dialog warns BEFORE committing
/// when the target count exceeds this. Single tuning constant: change only
/// this value.
pub const BATCH_UNDO_MAX: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchTarget {
    pub image_id: i64,
    pub path: PathBuf,
}

/// Outcome counts. `skipped` (could not read the current document) and
/// `failed` (could not write) are distinct and reported separately.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatchResult {
    pub applied: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl BatchResult {
    pub fn total(&self) -> usize {
        self.applied + self.failed + self.skipped
    }
}

/// Prior documents captured for undo: `(image_id, path, serialized prior doc)`.
/// Serialized rather than held as `EditDoc` so the memory cost is the flat JSON
/// the cap is reasoned about in.
#[derive(Clone, Debug, Default)]
pub struct UndoSnapshot {
    pub entries: Vec<(i64, PathBuf, String)>,
}

/// Merge `patch` into every target and write the result.
///
/// Parameterized over `read` and `write` so the whole decision surface — counts,
/// snapshot, progress, partial failure — is testable without a filesystem.
/// `write` returns `Err(reason)` on failure; the batch continues regardless.
pub fn apply_patch_to_targets(
    patch: &EditPatch,
    targets: &[BatchTarget],
    read: impl Fn(&BatchTarget) -> Option<EditDoc>,
    write: impl Fn(&BatchTarget, &EditDoc) -> Result<(), String>,
    progress: &mut dyn FnMut(usize, usize),
) -> (BatchResult, UndoSnapshot) {
    let total = targets.len();
    let snapshot_wanted = total <= BATCH_UNDO_MAX;
    let mut result = BatchResult::default();
    let mut snapshot = UndoSnapshot::default();

    for (i, t) in targets.iter().enumerate() {
        match read(t) {
            None => result.skipped += 1,
            Some(prior) => {
                let merged = patch.apply_to(&prior);
                match write(t, &merged) {
                    Ok(()) => {
                        result.applied += 1;
                        if snapshot_wanted {
                            snapshot.entries.push((
                                t.image_id,
                                t.path.clone(),
                                ferrolite_pipeline::serialize(&prior),
                            ));
                        }
                    }
                    Err(_reason) => result.failed += 1,
                }
            }
        }
        progress(i + 1, total);
    }
    (result, snapshot)
}
```

- [ ] **Step 4: Run and watch the core tests pass**

Run: `cargo test -p ferrolite-app --lib presets::apply::`
Expected: PASS (5 tests).

- [ ] **Step 5: Add the two events**

In `ferrolite-app/src/events.rs`, add to `enum AppEvent`:

```rust
    /// A batch preset/paste apply finished. `snapshot` is `None` when the batch
    /// exceeded `BATCH_UNDO_MAX` (see `presets::apply`), in which case undo is
    /// not offered. `label` names the applied patch for the toast.
    BatchApplyDone {
        result: crate::presets::apply::BatchResult,
        snapshot: Option<crate::presets::apply::UndoSnapshot>,
        label: String,
    },
    /// Progress within a batch apply.
    BatchApplyProgress { done: usize, total: usize },
```

> If `AppEvent` derives anything (`Debug`, `Clone`), make sure `BatchResult` and `UndoSnapshot`
> satisfy it — both already derive `Clone` and `Debug` above. Follow whatever `apply` in
> `events.rs`/`state.rs` does for unhandled variants; wire these two in Task 5.

- [ ] **Step 6: Implement the job wrapper**

Append to `apply.rs`:

```rust
/// Submit the batch as ONE Background job (contract 1: priority, cancellation,
/// progress). Reads and writes each target's sidecar, flags the affected
/// thumbnails stale, and reports through `AppEvent::BatchApplyDone`.
#[allow(clippy::too_many_arguments)]
pub fn spawn_batch_apply(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    ctx: &egui::Context,
    patch: EditPatch,
    targets: Vec<BatchTarget>,
    label: String,
) {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Background, move |cancel| {
        let mut last_repaint = 0usize;
        let tx_progress = tx.clone();
        let ctx_progress = ctx.clone();
        let mut progress = |done: usize, total: usize| {
            let _ = tx_progress.send(AppEvent::BatchApplyProgress { done, total });
            // Throttle repaints like the export path: every 16 items and on
            // completion, so progress advances without flooding the UI thread.
            if done == total || done.saturating_sub(last_repaint) >= 16 {
                last_repaint = done;
                ctx_progress.request_repaint();
            }
        };

        let (result, snapshot) = apply_patch_to_targets(
            &patch,
            &targets,
            |t| {
                if cancel.is_cancelled() {
                    return None;
                }
                let xmp = ferrolite_catalog::sidecar_path(&t.path);
                match ferrolite_catalog::read_ops(&xmp) {
                    Some(text) => ferrolite_pipeline::deserialize(&text),
                    // No sidecar yet == an unedited image, which is a perfectly
                    // valid target: start from the default document.
                    None if !xmp.exists() => Some(EditDoc::default()),
                    None => None, // present but malformed → skip, do not clobber
                }
            },
            |t, doc| {
                let xmp = ferrolite_catalog::sidecar_path(&t.path);
                let payload = ferrolite_pipeline::serialize(doc);
                ferrolite_catalog::write_ops(&xmp, &payload).map_err(|e| e.to_string())?;
                let db = writer.lock().expect("writer");
                db.set_has_edits(t.image_id, !doc.is_identity())
                    .map_err(|e| e.to_string())?;
                Ok(())
            },
            &mut progress,
        );

        // Flag every touched thumbnail stale in one statement (design §5.2).
        let ids: Vec<i64> = targets.iter().map(|t| t.image_id).collect();
        {
            let db = writer.lock().expect("writer");
            let _ = db.set_thumbnails_stale(&ids, true);
        }

        let snapshot = (!snapshot.entries.is_empty()).then_some(snapshot);
        let _ = tx.send(AppEvent::BatchApplyDone { result, snapshot, label });
        ctx.request_repaint();
    });
}
```

> Check `CancelToken`'s actual method name (`is_cancelled` vs `cancelled`) in
> `ferrolite-jobs/src/` and use the real one.

- [ ] **Step 7: Scoped gate**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app --lib presets::
```

- [ ] **Step 8: Commit**

```bash
git add ferrolite-app/src/presets/apply.rs ferrolite-app/src/presets/mod.rs ferrolite-app/src/events.rs
git commit -m "feat(app): batch apply job for EditPatch

One Background job for the whole batch, deliberately NOT one-at-a-time:
batch export is sequential because each item is a full-res render plus a
CPU-heavy encode, but batch edit does no rendering, so that constraint does
not transfer.

The decision core is parameterized over read/write closures, so counts,
undo snapshot, progress and partial failure are all tested without touching
a filesystem. Skipped (unreadable) and failed (unwritable) are distinct
outcomes. A malformed existing sidecar is skipped rather than clobbered."
```

---

## Task 5: Undo — snapshot + `apply_undo_redo` extension

**Files:**
- Modify: `ferrolite-app/src/state.rs` (add fields)
- Modify: `ferrolite-app/src/app.rs` (`apply_undo_redo`, handle the two events)
- Modify: `ferrolite-app/src/chrome/mod.rs` (`can_undo` accounts for a snapshot)
- Test: in-module tests in `apply.rs` for the revert builder

**Interfaces:**
- Consumes: `UndoSnapshot`, `BatchResult` (Task 4).
- Produces:
  - `AppState.presets: Vec<Preset>`
  - `AppState.clipboard_patch: Option<EditPatch>`
  - `AppState.batch_undo: Option<UndoSnapshot>`
  - `presets::apply::spawn_batch_undo(...)` — restores the snapshot's documents.

- [ ] **Step 1: Add the state fields**

In `ferrolite-app/src/state.rs`, add to `AppState` (near `selection`), and initialize them in
**every** constructor (`new`, `for_test`, and any other — grep for `selection: HashSet::new()` or
`selected: None` to find them all):

```rust
    /// Presets loaded from disk at startup (P7). Source of truth is the files;
    /// this is just the in-memory list the UI renders.
    pub presets: Vec<crate::presets::Preset>,
    /// The last "Copy settings" capture, session-scoped.
    pub clipboard_patch: Option<ferrolite_pipeline::EditPatch>,
    /// Prior documents from the last batch apply, for a one-level undo.
    /// `None` when nothing to undo or the batch exceeded `BATCH_UNDO_MAX`.
    pub batch_undo: Option<crate::presets::apply::UndoSnapshot>,
```

- [ ] **Step 2: Write the failing revert test**

Add to `apply.rs`'s test module:

```rust
    /// Undo restores each snapshot entry's prior document verbatim.
    #[test]
    fn undo_restores_the_exact_prior_documents() {
        let mut prior = EditDoc::default();
        prior.global.exposure = -1.25;
        prior.global.saturation = 0.4;
        let snap = UndoSnapshot {
            entries: vec![(
                7,
                std::path::PathBuf::from("/img/7.arw"),
                ferrolite_pipeline::serialize(&prior),
            )],
        };

        let restored = snapshot_documents(&snap);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].0, 7);
        assert_eq!(restored[0].2, prior, "prior document restored byte-for-byte");
    }

    /// A snapshot entry that no longer deserializes is dropped, not panicked on.
    #[test]
    fn undo_drops_an_unparseable_snapshot_entry() {
        let snap = UndoSnapshot {
            entries: vec![(1, std::path::PathBuf::from("/a"), "garbage {{".into())],
        };
        assert!(snapshot_documents(&snap).is_empty());
    }
```

- [ ] **Step 3: Run, watch it fail, implement**

Run: `cargo test -p ferrolite-app --lib presets::apply::undo_`
Expected: FAIL — `snapshot_documents` not found.

Add to `apply.rs`:

```rust
/// Decode a snapshot back into `(image_id, path, prior document)` triples.
/// Entries that no longer deserialize are dropped — an undo that can restore
/// most of a batch is better than one that panics.
pub fn snapshot_documents(snap: &UndoSnapshot) -> Vec<(i64, PathBuf, EditDoc)> {
    snap.entries
        .iter()
        .filter_map(|(id, path, text)| {
            ferrolite_pipeline::deserialize(text).map(|doc| (*id, path.clone(), doc))
        })
        .collect()
}

/// Restore a batch's prior documents. Writes each sidecar back and re-flags the
/// thumbnails stale (they were regenerated, or marked, against the now-undone
/// edit either way).
pub fn spawn_batch_undo(
    jobs: &Arc<JobSystem>,
    writer: &Arc<Mutex<Catalog>>,
    tx: &std::sync::mpsc::Sender<AppEvent>,
    ctx: &egui::Context,
    snapshot: UndoSnapshot,
) {
    let writer = Arc::clone(writer);
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(Priority::Background, move |_cancel| {
        let docs = snapshot_documents(&snapshot);
        let mut result = BatchResult::default();
        let mut ids = Vec::with_capacity(docs.len());
        for (image_id, path, doc) in &docs {
            let xmp = ferrolite_catalog::sidecar_path(path);
            let payload = ferrolite_pipeline::serialize(doc);
            if ferrolite_catalog::write_ops(&xmp, &payload).is_err() {
                result.failed += 1;
                continue;
            }
            let db = writer.lock().expect("writer");
            let _ = db.set_has_edits(*image_id, !doc.is_identity());
            result.applied += 1;
            ids.push(*image_id);
        }
        {
            let db = writer.lock().expect("writer");
            let _ = db.set_thumbnails_stale(&ids, true);
        }
        let _ = tx.send(AppEvent::BatchApplyDone {
            result,
            snapshot: None, // undoing an undo is not offered
            label: "Undo".to_string(),
        });
        ctx.request_repaint();
    });
}
```

- [ ] **Step 4: Run and watch it pass**

Run: `cargo test -p ferrolite-app --lib presets::apply::`
Expected: PASS (7 tests).

- [ ] **Step 5: Extend `apply_undo_redo`**

In `ferrolite-app/src/app.rs`, at the very top of `apply_undo_redo`, before the existing
Develop-history logic:

```rust
        // P7: with no active Develop session, Ctrl+Z reverts the last batch
        // apply. Reusing the existing action rather than adding a binding means
        // Undo keeps meaning "undo the last thing I did", and the keybind is
        // already discoverable in the Settings keyboard tab and the Help panel
        // (CLAUDE.md), so no new GROUPS or Help entry is needed.
        if is_undo && self.state.viewer.is_none() {
            if let Some(snapshot) = self.state.batch_undo.take() {
                crate::presets::apply::spawn_batch_undo(
                    &self.state.jobs,
                    &self.state.catalog,
                    &self.state.tx,
                    ctx,
                    snapshot,
                );
                return;
            }
        }
```

> Check the real field names for the job system, catalog writer and sender on `AppState`
> (`spawn_ops_write`'s call sites in `develop/ops_persist.rs` show the exact ones) and the real
> way "no Develop session" is expressed (`self.state.viewer.is_none()` is the likely form —
> verify against how other code tests for an open viewer).

- [ ] **Step 6: Make `can_undo` account for the snapshot**

Find where `can_undo` is computed for `chrome::menu_button(ui, keymap, "Undo", Action::Undo,
can_undo)` and OR in the batch snapshot:

```rust
let can_undo = /* existing develop-history condition */
    || (self.state.viewer.is_none() && self.state.batch_undo.is_some());
```

- [ ] **Step 7: Handle the two events**

Where `AppEvent` is matched (follow `AppEvent::OpsSaved`'s handling), add:

```rust
            AppEvent::BatchApplyProgress { .. } => { /* status-bar only; no state change */ }
            AppEvent::BatchApplyDone { result, snapshot, label } => {
                self.state.batch_undo = snapshot;
                let msg = if result.failed == 0 && result.skipped == 0 {
                    format!("Applied \u{201c}{label}\u{201d} to {} images.", result.applied)
                } else {
                    format!(
                        "Applied \u{201c}{label}\u{201d} to {} images. {} failed, {} skipped.",
                        result.applied, result.failed, result.skipped
                    )
                };
                let msg = match self.state.batch_undo.is_some() {
                    true => format!("{msg} Press {} to undo.", keymap.hint(Action::Undo)),
                    false => msg,
                };
                let level = if result.failed > 0 {
                    crate::notifications::Level::Warning
                } else {
                    crate::notifications::Level::Info
                };
                self.state.notify(level, msg);
            }
```

> `keymap.hint(Action::Undo)` sources the key from the LIVE keymap, so a rebind updates the text —
> required by CLAUDE.md's keybind-tooltip rule. Get the keymap from wherever this scope already
> has it (`self.state.settings.keymap` or similar); check a nearby `km.hint(...)` call site.

- [ ] **Step 8: Load presets at startup, off the UI thread**

Nothing populates `state.presets` yet. Add a third `AppEvent` variant in `events.rs`:

```rust
    /// The startup preset-directory scan finished.
    PresetsLoaded { presets: Vec<crate::presets::Preset> },
```

Add the loader to `presets/store.rs`:

```rust
/// Scan the preset directory off the UI thread (contract 1 — this is file I/O,
/// however small) and deliver the list over the event channel.
pub fn spawn_load_all(
    jobs: &std::sync::Arc<ferrolite_jobs::JobSystem>,
    tx: &std::sync::mpsc::Sender<crate::events::AppEvent>,
    ctx: &egui::Context,
) {
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(ferrolite_jobs::Priority::Background, move |_cancel| {
        let presets = load_all(&presets_dir());
        let _ = tx.send(crate::events::AppEvent::PresetsLoaded { presets });
        ctx.request_repaint();
    });
}
```

Call it once during app construction, next to whatever other startup jobs are spawned (grep for
`prewarm_pipelines` or the initial catalog open in `app.rs`/`main.rs` and follow that placement).
Handle the event:

```rust
            AppEvent::PresetsLoaded { presets } => self.state.presets = presets,
```

- [ ] **Step 9: Scoped gate**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app
```

- [ ] **Step 10: Commit**

```bash
git add ferrolite-app/src/presets/ ferrolite-app/src/state.rs ferrolite-app/src/app.rs ferrolite-app/src/chrome/mod.rs ferrolite-app/src/events.rs
git commit -m "feat(app): one-level undo for batch applies via the existing Undo action

Reuses apply_undo_redo's single funnel rather than adding a keybind: with no
Develop session open and a pending snapshot, Undo reverts the batch. No new
Action variant, no Action::ALL resize, no new GROUPS or Help entry — Undo is
already discoverable in both required places.

The toast names the key via keymap.hint(Action::Undo), so a rebind updates
the text. Snapshot entries that no longer deserialize are dropped rather
than panicked on: restoring most of a batch beats restoring none."
```

---

## Task 6: The shared group-checkbox modal

**Files:**
- Create: `ferrolite-app/src/presets/modal.rs`
- Modify: `ferrolite-app/src/presets/mod.rs` (`pub mod modal;`)
- Modify: `ferrolite-app/src/icons.rs` (add aliases)
- Test: in-module tests for the pure label/default helpers

**Interfaces:**
- Consumes: `GroupSet` (Task 1), `BATCH_UNDO_MAX` (Task 4).
- Produces:
  - `GroupModalMode { Save { name: String }, Paste { target_count: usize } }`
  - `GroupModal { pub mode: GroupModalMode, pub owns: GroupSet }`
  - `GroupModalOutcome { None, Cancelled, Confirmed { name: Option<String>, owns: GroupSet } }`
  - `GroupModal::new_save() -> Self`, `GroupModal::new_paste(usize) -> Self`
  - `GroupModal::show(&mut self, ctx: &egui::Context) -> GroupModalOutcome`
  - `pub fn group_label(g: GroupSet) -> &'static str`
  - `pub fn default_owns() -> GroupSet`

- [ ] **Step 1: Write the failing helper tests**

Create `modal.rs` with the test module only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// GEOMETRY and LENS are off by default — framing and optics are per-image
    /// (design §3.2). Everything else applicable is on. MASKS is never on.
    #[test]
    fn geometry_and_lens_are_off_by_default_masks_never_on() {
        let d = default_owns();
        assert!(d.contains(GroupSet::LIGHT));
        assert!(d.contains(GroupSet::COLOR));
        assert!(d.contains(GroupSet::DETAIL));
        assert!(!d.contains(GroupSet::GEOMETRY), "framing is per-image");
        assert!(!d.contains(GroupSet::LENS), "optics are per-image");
        assert!(!d.contains(GroupSet::MASKS), "masks are out of P7");
    }

    /// Every applicable group has a distinct, non-empty label.
    #[test]
    fn every_applicable_group_has_a_unique_label() {
        let mut seen = std::collections::HashSet::new();
        for g in GroupSet::ALL_APPLICABLE {
            let l = group_label(g);
            assert!(!l.is_empty(), "empty label");
            assert!(seen.insert(l), "duplicate label {l}");
        }
        assert!(!group_label(GroupSet::MASKS).is_empty(), "MASKS still needs a label");
    }
}
```

- [ ] **Step 2: Run, watch it fail, implement the helpers + modal**

Run: `cargo test -p ferrolite-app --lib presets::modal::`
Expected: FAIL to compile.

Prepend to `modal.rs`:

```rust
//! The shared group-checkbox modal, used for BOTH "Save preset" and
//! "Paste settings" (P7 design §6.3).
//!
//! Applying a PRESET opens no dialog — a preset already declares its groups —
//! so this appears only when saving one or pasting an ad-hoc copy.

use ferrolite_pipeline::GroupSet;

use super::apply::BATCH_UNDO_MAX;

/// User-visible label. Uses "Color" (not "Colour") to match the codebase's
/// `ColorSwatch`/`color_grade`/`ColorControl` naming.
pub fn group_label(g: GroupSet) -> &'static str {
    match g {
        GroupSet::LIGHT => "Light",
        GroupSet::COLOR => "Color",
        GroupSet::CURVE => "Tone curve",
        GroupSet::HSL => "HSL",
        GroupSet::GRADING => "Color grading",
        GroupSet::DETAIL => "Detail",
        GroupSet::EFFECTS => "Effects",
        GroupSet::GEOMETRY => "Geometry",
        GroupSet::LENS => "Lens corrections",
        GroupSet::MASKS => "Masks",
        _ => "Unknown",
    }
}

/// One-line hint under a group, or `None`.
fn group_hint(g: GroupSet) -> Option<&'static str> {
    match g {
        GroupSet::LIGHT => Some("exposure, contrast, highlights, shadows, whites, blacks"),
        GroupSet::COLOR => Some("temperature, tint, saturation, vibrance"),
        GroupSet::DETAIL => Some("noise reduction, sharpening"),
        GroupSet::EFFECTS => Some("dehaze"),
        GroupSet::GEOMETRY => Some("crop, rotate, keystone"),
        GroupSet::LENS => Some("distortion, TCA, vignetting amounts"),
        _ => None,
    }
}

/// Everything applicable except GEOMETRY and LENS — framing and optics are
/// per-image, so they are available but not on by default (design §3.2).
pub fn default_owns() -> GroupSet {
    let mut g = GroupSet::EMPTY;
    for candidate in GroupSet::ALL_APPLICABLE {
        if candidate != GroupSet::GEOMETRY && candidate != GroupSet::LENS {
            g.insert(candidate);
        }
    }
    g
}

pub enum GroupModalMode {
    Save { name: String },
    Paste { target_count: usize },
}

pub struct GroupModal {
    pub mode: GroupModalMode,
    pub owns: GroupSet,
    /// Set when the entered name is rejected; shown inline.
    pub name_error: Option<String>,
}

pub enum GroupModalOutcome {
    /// Still open.
    None,
    Cancelled,
    Confirmed {
        /// `Some` in Save mode, `None` in Paste mode.
        name: Option<String>,
        owns: GroupSet,
    },
}

impl GroupModal {
    pub fn new_save() -> Self {
        Self {
            mode: GroupModalMode::Save { name: String::new() },
            owns: default_owns(),
            name_error: None,
        }
    }
    pub fn new_paste(target_count: usize) -> Self {
        Self {
            mode: GroupModalMode::Paste { target_count },
            owns: default_owns(),
            name_error: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> GroupModalOutcome {
        let title = match &self.mode {
            GroupModalMode::Save { .. } => "Save preset".to_string(),
            GroupModalMode::Paste { target_count } => {
                format!("Paste settings to {target_count} images")
            }
        };
        let mut outcome = GroupModalOutcome::None;

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                if let GroupModalMode::Save { name } = &mut self.mode {
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(name);
                    });
                    if let Some(err) = &self.name_error {
                        ui.colored_label(crate::theme::WARNING_COLOR, err);
                    }
                    ui.add_space(6.0);
                    ui.label("This preset sets:");
                }

                for g in GroupSet::ALL_APPLICABLE {
                    let mut on = self.owns.contains(g);
                    let resp = ui.checkbox(&mut on, group_label(g));
                    if let Some(hint) = group_hint(g) {
                        resp.on_hover_text(hint);
                    }
                    if on {
                        self.owns.insert(g);
                    } else {
                        self.owns.remove(g);
                    }
                }

                // Masks: permanently greyed with an honest reason (design §2 P7-D2).
                let mut masks_off = false;
                ui.add_enabled_ui(false, |ui| {
                    ui.checkbox(&mut masks_off, group_label(GroupSet::MASKS));
                })
                .response
                .on_disabled_hover_text("Mask sync comes with a later phase");

                if let GroupModalMode::Paste { target_count } = &self.mode {
                    if *target_count > BATCH_UNDO_MAX {
                        ui.add_space(6.0);
                        ui.colored_label(
                            crate::theme::WARNING_COLOR,
                            format!("Undo won't be available for more than {BATCH_UNDO_MAX} images."),
                        );
                    }
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Select all").clicked() {
                        for g in GroupSet::ALL_APPLICABLE {
                            self.owns.insert(g);
                        }
                    }
                    if ui.button("None").clicked() {
                        self.owns = GroupSet::EMPTY;
                    }
                    ui.add_space(16.0);
                    if ui.button("Cancel").clicked() {
                        outcome = GroupModalOutcome::Cancelled;
                    }
                    let can_confirm = !self.owns.is_empty();
                    let confirm = ui
                        .add_enabled(can_confirm, egui::Button::new("Apply"));
                    if !can_confirm {
                        confirm.on_disabled_hover_text("Select at least one group");
                    } else if confirm.clicked() {
                        let name = match &self.mode {
                            GroupModalMode::Save { name } => Some(name.clone()),
                            GroupModalMode::Paste { .. } => None,
                        };
                        outcome = GroupModalOutcome::Confirmed { name, owns: self.owns };
                    }
                });
            });

        outcome
    }
}
```

> `crate::theme::WARNING_COLOR` may not exist under that name — grep `theme.rs` for the warning /
> error colour the app already uses (`notifications::level_color` is one source) and use the real
> one. Do NOT introduce a new colour constant.

Add `pub mod modal;` to `presets/mod.rs`.

- [ ] **Step 3: Run and watch the helper tests pass**

Run: `cargo test -p ferrolite-app --lib presets::modal::`
Expected: PASS (2 tests).

- [ ] **Step 4: Add the icons**

In `ferrolite-app/src/icons.rs`, add semantic aliases following the file's existing style (they
alias `egui_phosphor` constants — copy the exact form used by neighbouring entries):

```rust
/// Presets menu / preset list entries.
pub const PRESET: &str = egui_phosphor::regular::SLIDERS;
/// Copy settings.
pub const COPY_SETTINGS: &str = egui_phosphor::regular::COPY;
/// Paste settings.
pub const PASTE_SETTINGS: &str = egui_phosphor::regular::CLIPBOARD_TEXT;
```

> Verify each constant exists in the bundled `egui-phosphor` version before using it; substitute a
> neighbouring real one if not. NEVER a raw emoji.

- [ ] **Step 5: Scoped gate**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app --lib presets::
```

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/presets/modal.rs ferrolite-app/src/presets/mod.rs ferrolite-app/src/icons.rs
git commit -m "feat(app): shared group-checkbox modal for save-preset and paste-settings

One modal serves both. Geometry and Lens default off because framing and
optics are per-image; Masks is permanently greyed with an honest reason.
Over BATCH_UNDO_MAX targets the dialog warns that undo will be unavailable
BEFORE the user commits, rather than after.

Applying a preset deliberately opens no dialog — a preset already declares
which groups it owns."
```

---

## Task 7: Library context menu

**Files:**
- Modify: `ferrolite-app/src/library/image_context_menu.rs`
- Modify: `ferrolite-app/src/app.rs` (own the open modal, dispatch the actions)

**Interfaces:**
- Consumes: everything from Tasks 1–6.
- Produces: new variants on the context menu's existing action enum — follow whatever it already
  returns (grep for `AddToQueue` / `RegenerateThumbnail` in that file to find the pattern):
  `CopySettings { image_id }`, `PasteSettings`, `ApplyPreset { index: usize }`,
  `SavePresetFrom { image_id }`.

- [ ] **Step 1: Read the file and follow its pattern**

Open `ferrolite-app/src/library/image_context_menu.rs` and identify: the action enum it returns,
how it decides single-vs-multi selection, and how existing items are rendered with counts
(`"Added {n} to export queue."`). **Match that pattern exactly** — do not invent a new one.

- [ ] **Step 2: Add the four items with their greyed reasons**

Insert a separator and these items, following the file's existing item style:

```rust
    ui.separator();

    // Copy: only meaningful from a single source image that actually has edits.
    let source_has_edits = /* the record's has_edits flag, as the file reads other fields */;
    let copy = ui.add_enabled(
        source_has_edits,
        egui::Button::new(format!("{} Copy settings", crate::icons::COPY_SETTINGS)),
    );
    if !source_has_edits {
        copy.on_disabled_hover_text("This image has no edits to copy");
    } else if copy.clicked() {
        action = Some(ContextAction::CopySettings { image_id: rec.id });
        ui.close_menu();
    }

    let has_clipboard = state.clipboard_patch.is_some();
    let paste_label = format!("{} Paste settings…", crate::icons::PASTE_SETTINGS);
    let paste = ui.add_enabled(has_clipboard, egui::Button::new(paste_label));
    if !has_clipboard {
        paste.on_disabled_hover_text("Copy settings from an image first");
    } else if paste.clicked() {
        action = Some(ContextAction::PasteSettings);
        ui.close_menu();
    }

    // Apply preset: a submenu, one entry per saved preset.
    let has_presets = !state.presets.is_empty();
    ui.add_enabled_ui(has_presets, |ui| {
        ui.menu_button(format!("{} Apply preset", crate::icons::PRESET), |ui| {
            for (i, p) in state.presets.iter().enumerate() {
                if ui.button(&p.name).clicked() {
                    action = Some(ContextAction::ApplyPreset { index: i });
                    ui.close_menu();
                }
            }
        });
    })
    .response
    .on_disabled_hover_text("Save a preset first");

    let save = ui.add_enabled(
        source_has_edits,
        egui::Button::new(format!("{} Save preset from this image…", crate::icons::PRESET)),
    );
    if !source_has_edits {
        save.on_disabled_hover_text("This image has no edits to save");
    } else if save.clicked() {
        action = Some(ContextAction::SavePresetFrom { image_id: rec.id });
        ui.close_menu();
    }
```

> `state` may not be in scope in this function — pass in the two slices it needs
> (`presets: &[Preset]`, `has_clipboard: bool`) rather than threading all of `AppState`, matching
> how the function already receives its data.

- [ ] **Step 3: Dispatch the actions in `app.rs`**

Where the context menu's action is matched:

- `CopySettings { image_id }` — read that image's doc off-thread (reuse `spawn_ops_read`'s shape),
  then `state.clipboard_patch = Some(EditPatch::from_doc(&doc, default_owns()))`. Simplest correct
  version: capture the FULL doc with `default_owns()`; the paste dialog narrows it.
- `PasteSettings` — open `GroupModal::new_paste(selection_count)`.
- `ApplyPreset { index }` — build targets from the selection and call `spawn_batch_apply` with
  `state.presets[index].to_patch()` and the preset's name as `label`. **No modal.**
- `SavePresetFrom { image_id }` — open `GroupModal::new_save()`, remembering the source id.

**Target construction (used by both paste and apply-preset)** — exclude the open Develop image:

```rust
/// Build batch targets from the current selection, EXCLUDING the image open in
/// Develop so a batch never races the live session's own sidecar writes
/// (design §5.1). Returns `(targets, excluded_open_image)`.
fn batch_targets(state: &AppState) -> (Vec<BatchTarget>, bool) {
    let open = state.viewer.as_ref().map(|v| v.image_id);
    let mut excluded = false;
    let targets = state
        .selection
        .iter()
        .filter(|id| {
            if Some(**id) == open {
                excluded = true;
                false
            } else {
                true
            }
        })
        .filter_map(|id| {
            state
                .image_path(*id)
                .map(|path| BatchTarget { image_id: *id, path })
        })
        .collect();
    (targets, excluded)
}
```

> `state.image_path(id)` may not exist — use whatever the codebase already does to get a path from
> an image id (grep how `regenerate thumbnail` resolves one from the context menu). If the viewer's
> id field is named differently, use the real name.

When `excluded` is true, append to the result toast: `" The image open in Develop was skipped."`

- [ ] **Step 4: Drive the modal from the frame loop**

Hold the modal on the app struct (`open_group_modal: Option<GroupModal>`, plus
`pending_save_source: Option<i64>` for save mode), and drive it once per frame:

```rust
        if let Some(modal) = self.open_group_modal.as_mut() {
            match modal.show(ctx) {
                GroupModalOutcome::None => {}
                GroupModalOutcome::Cancelled => {
                    self.open_group_modal = None;
                    self.pending_save_source = None;
                }
                GroupModalOutcome::Confirmed { name, owns } => {
                    match name {
                        // Save mode.
                        Some(name) => {
                            let doc = /* the source image's current doc */;
                            let preset = crate::presets::Preset {
                                version: ferrolite_pipeline::PATCH_VERSION,
                                name,
                                owns,
                                doc,
                            };
                            let dir = crate::presets::presets_dir();
                            match crate::presets::save(&dir, &preset) {
                                Ok(_) => {
                                    self.state.presets = crate::presets::load_all(&dir);
                                    self.open_group_modal = None;
                                    self.pending_save_source = None;
                                    self.state.notify(
                                        crate::notifications::Level::Info,
                                        format!("Saved preset \u{201c}{}\u{201d}.", preset.name),
                                    );
                                }
                                // Keep the modal OPEN so the user can fix the
                                // name rather than losing what they typed.
                                Err(e) => modal.name_error = Some(e.to_string()),
                            }
                        }
                        // Paste mode.
                        None => {
                            if let Some(clip) = self.state.clipboard_patch.clone() {
                                let patch = ferrolite_pipeline::EditPatch {
                                    version: clip.version,
                                    owns, // narrowed by the dialog
                                    doc: clip.doc,
                                };
                                let (targets, excluded) = batch_targets(&self.state);
                                crate::presets::apply::spawn_batch_apply(
                                    &self.state.jobs,
                                    &self.state.catalog,
                                    &self.state.tx,
                                    ctx,
                                    patch,
                                    targets,
                                    "Pasted settings".to_string(),
                                );
                                self.last_batch_excluded_open_image = excluded;
                            }
                            self.open_group_modal = None;
                        }
                    }
                }
            }
        }
```

> Save mode needs the source image's current document. If the source is the image open in Develop,
> take it from the live viewer state; otherwise read it off-thread first (reuse `spawn_ops_read`'s
> shape) and open the modal only once the doc has arrived — never block the UI thread on a file
> read. Use the real field names for `jobs`/`catalog`/`tx` (see `spawn_ops_write`'s call sites).

- [ ] **Step 5: Scoped gate**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app
```

- [ ] **Step 6: Commit**

```bash
git add ferrolite-app/src/library/image_context_menu.rs ferrolite-app/src/app.rs
git commit -m "feat(app): copy/paste/sync and apply-preset in the library context menu

Every disabled item carries an honest hover reason, per house convention.
Applying a preset takes no dialog (it declares its own groups); pasting opens
the group modal because an ad-hoc copy carries no intent.

Batch targets exclude the image open in Develop so a batch never races the
live session's own sidecar writes, and the toast says when that happened."
```

---

## Task 8: Develop `Presets ▾` button

**Files:**
- Modify: `ferrolite-app/src/develop/adjustment_panel.rs` (or whichever file renders the panel
  footer with *Reset all* — grep for `"Reset all"` and edit that file)
- Modify: `ferrolite-app/src/app.rs` (handle the emitted action)

**Interfaces:**
- Consumes: `state.presets`, `GroupModal`, `presets::{save, delete, load_all}`.
- Produces: a panel action enum variant per the file's existing pattern —
  `ApplyPresetToCurrent { index }`, `SaveCurrentAsPreset`, `RenamePreset { index }`,
  `DeletePreset { index }`.

- [ ] **Step 1: Add the menu button beside *Reset all***

```rust
    ui.menu_button(
        format!("{} Presets", crate::icons::PRESET),
        |ui| {
            if state_presets.is_empty() {
                ui.add_enabled(false, egui::Button::new("No presets saved"))
                    .on_disabled_hover_text("Save the current edit as a preset first");
            }
            for (i, p) in state_presets.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.button(&p.name).clicked() {
                        out = Some(PanelAction::ApplyPresetToCurrent { index: i });
                        ui.close_menu();
                    }
                    ui.menu_button("…", |ui| {
                        if ui.button("Rename…").clicked() {
                            out = Some(PanelAction::RenamePreset { index: i });
                            ui.close_menu();
                        }
                        if ui.button("Delete").clicked() {
                            out = Some(PanelAction::DeletePreset { index: i });
                            ui.close_menu();
                        }
                    });
                });
            }
            ui.separator();
            if ui.button("Save current as preset…").clicked() {
                out = Some(PanelAction::SaveCurrentAsPreset);
                ui.close_menu();
            }
        },
    );
```

- [ ] **Step 2: Handle the actions**

- `ApplyPresetToCurrent { index }` — apply `presets[index].to_patch()` to the **current** doc
  in-memory and push it through the SAME path a slider edit takes, so Develop's own undo history
  records it. Grep for how a slider commits an edit (`ops_edit.rs`) and reuse that call. Do NOT
  write the sidecar directly here — the existing path already does.
- `SaveCurrentAsPreset` — open `GroupModal::new_save()`; on confirm build
  `Preset { version: PATCH_VERSION, name, owns, doc: current_doc.clone() }` and `presets::save`.
- `RenamePreset { index }` — save under the new name, then delete the old file. If the save fails
  (duplicate/invalid), keep the old file and surface the error; never lose the preset.
- `DeletePreset { index }` — confirm first (the file is removed from disk), then `presets::delete`
  and reload the list.

- [ ] **Step 3: Scoped gate**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app
```

- [ ] **Step 4: Commit**

```bash
git add ferrolite-app/src/develop/ ferrolite-app/src/app.rs
git commit -m "feat(app): Presets menu in the Develop panel footer

Apply, save-as, rename and delete for a flat preset list, as a compact
button beside Reset all rather than a tab competing with Light/Color/Detail.

Applying to the current image goes through the same edit-commit path a
slider uses, so Develop's own undo history records it. Rename saves under
the new name before deleting the old file, so a failure never loses a preset."
```

---

## Task 9: Fix batch export to honour persisted edits

**Files:**
- Modify: `ferrolite-app/src/export/batch.rs` (module doc, line ~161, the comment at ~179)
- Test: `ferrolite-app/tests/` — a fixture-gated integration test (create the file if the crate
  has no `tests/` dir yet)

**Interfaces:**
- Consumes: `ferrolite_catalog::{read_ops, sidecar_path}`, `ferrolite_pipeline::deserialize`.

- [ ] **Step 1: Write the failing test**

The pure, testable part is *the stack a batch item resolves to*. Extract it first:

```rust
/// The edit document a batch item renders with: its persisted sidecar, or the
/// default document when there is no sidecar / it is malformed.
///
/// Batch export previously hardcoded `OpStack::default()` here, justified by a
/// comment reading "per-image edits are not persisted" — true when written,
/// stale once sidecars shipped. The effect was that editing 50 images and
/// batch-exporting produced 50 UNEDITED files.
pub(crate) fn stack_for_item(path: &std::path::Path) -> OpStack {
    let xmp = ferrolite_catalog::sidecar_path(path);
    ferrolite_catalog::read_ops(&xmp)
        .and_then(|text| ferrolite_pipeline::deserialize(&text))
        .unwrap_or_default()
}
```

Test it in-module in `batch.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_for_item_is_default_when_no_sidecar_exists() {
        let p = std::env::temp_dir().join("ferrolite-batch-no-sidecar.arw");
        let _ = std::fs::remove_file(ferrolite_catalog::sidecar_path(&p));
        assert!(stack_for_item(&p).is_identity());
    }

    #[test]
    fn stack_for_item_reads_a_persisted_edit() {
        let p = std::env::temp_dir().join("ferrolite-batch-with-sidecar.arw");
        let xmp = ferrolite_catalog::sidecar_path(&p);
        let mut doc = OpStack::default();
        doc.global.exposure = 1.25;
        ferrolite_catalog::write_ops(&xmp, &ferrolite_pipeline::serialize(&doc)).unwrap();

        let got = stack_for_item(&p);
        assert_eq!(got.global.exposure, 1.25, "batch export must honour persisted edits");

        let _ = std::fs::remove_file(&xmp);
    }

    #[test]
    fn stack_for_item_falls_back_to_default_on_a_malformed_sidecar() {
        let p = std::env::temp_dir().join("ferrolite-batch-bad-sidecar.arw");
        let xmp = ferrolite_catalog::sidecar_path(&p);
        ferrolite_catalog::write_ops(&xmp, "not json {{").unwrap();
        assert!(stack_for_item(&p).is_identity(), "malformed → default, never a panic");
        let _ = std::fs::remove_file(&xmp);
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p ferrolite-app --lib export::batch::`
Expected: FAIL to compile — `stack_for_item` not found.

- [ ] **Step 3: Add `stack_for_item` and call it**

Add the function from Step 1, then replace line ~161:

```rust
-    let stack = OpStack::default();
+    let stack = stack_for_item(&item.path);
```

- [ ] **Step 4: Correct the two stale comments**

Module doc (line ~5) — replace the "renders with `OpStack::default()` — per-image edits are not
persisted" sentence with:

```rust
//! renders each item's PERSISTED edit stack (read from its XMP sidecar), so a
//! batch export matches what the grid and Develop show. This was previously
//! hardcoded to `OpStack::default()`, which silently exported every image
//! unedited once sidecar persistence shipped (P7 design §7).
```

And the comment at ~179 similarly. **Do not leave a comment claiming edits are not persisted.**

- [ ] **Step 5: Match the single-file path's lens bake**

Read how the single-file export path bakes lens products for an image and mirror it per item, so
batch and single-file export produce the same pixels. If the single-file path does nothing extra,
record that in your report and skip.

- [ ] **Step 6: Run and watch it pass**

Run: `cargo test -p ferrolite-app --lib export::batch::`
Expected: PASS (3 tests).

- [ ] **Step 7: Scoped gate**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app
```

- [ ] **Step 8: Commit**

```bash
git add ferrolite-app/src/export/batch.rs
git commit -m "fix(export): batch export renders persisted edits, not OpStack::default()

Batch export hardcoded OpStack::default(), so editing 50 images and
batch-exporting produced 50 UNEDITED files. The justifying comment
(\"per-image edits are not persisted\") was true when written and went stale
once sidecar persistence shipped.

Resolving the stack is now a small pure function, so the no-sidecar,
persisted-edit and malformed-sidecar cases are all covered by test."
```

---

## Task 10: The grid consumes `stale` — lazy regeneration

**This task is what makes decision P7-D4 real.** Tasks 3 and 4 add and set the flag; without this
one nothing ever reads it and a batch-applied thumbnail stays wrong until the user opens the image
in Develop. Depends only on Task 3, but is placed last because it is the consumer side.

**Files:**
- Modify: `ferrolite-app/src/library/grid.rs` (the cell-realize path — grep for where
  `request_thumbnail` is called)
- Modify: `ferrolite-app/src/state.rs` (a de-dup guard set)
- Modify: `ferrolite-app/src/develop/thumb_regen.rs` (clear the flag on success)

**Interfaces:**
- Consumes: `Catalog::is_thumbnail_stale`, `Catalog::set_thumbnails_stale` (Task 3);
  `thumb_regen::spawn_regen_edited_thumbnail` (existing).
- Produces: `AppState.stale_regen_inflight: std::collections::HashSet<i64>`.

- [ ] **Step 1: Read the existing realize path**

Open `ferrolite-app/src/library/grid.rs` and find where a visible cell asks for its thumbnail.
Note how `ThumbMissing`'s **sticky guard** prevents a per-frame re-spawn storm (documented on
`AppEvent::ThumbMissing` in `events.rs`). Your regeneration trigger needs the same protection —
a cell stays on screen for many frames, and re-spawning a full decode+render every frame would be
catastrophic. This is the single biggest risk in this task.

- [ ] **Step 2: Add the in-flight guard to state**

In `state.rs`, alongside the other per-folder job state (and reset it in
`reset_for_new_folder`):

```rust
    /// Image ids with a stale-thumbnail regeneration currently in flight.
    /// Without this guard a stale cell that stays on screen would re-spawn a
    /// full decode + GPU render + encode EVERY FRAME — the same storm the
    /// `ThumbMissing` sticky guard exists to prevent.
    pub stale_regen_inflight: std::collections::HashSet<i64>,
```

- [ ] **Step 3: Trigger regeneration when a stale cell realizes**

At the realize site, after the existing thumbnail request:

```rust
            // P7: a batch apply flagged this thumbnail stale. Regenerate it now
            // that the cell is actually on screen — the lazy half of design
            // §5.2. Reading the flag is a single indexed lookup on a row the
            // grid is already loading; the expensive render only happens for
            // cells the user actually looks at.
            if !state.stale_regen_inflight.contains(&rec.id)
                && read_pool.is_thumbnail_stale(rec.id).unwrap_or(false)
            {
                state.stale_regen_inflight.insert(rec.id);
                crate::develop::thumb_regen::spawn_regen_edited_thumbnail(/* … */);
            }
```

> Use the **read pool**, not the writer `Mutex<Catalog>`, for `is_thumbnail_stale` — this runs in
> the render path and must never contend with a write lock. Check `ReadPool`'s API and add
> `is_thumbnail_stale` there too if it does not already proxy `Catalog`'s read methods. Fill in
> `spawn_regen_edited_thumbnail`'s real arguments from its existing call sites in `app.rs`.

- [ ] **Step 4: Clear the flag when regeneration succeeds**

In `thumb_regen.rs`, where the regenerated thumbnail is persisted and `ThumbReady` is sent, clear
the flag in the same job:

```rust
        // Clear the stale flag only AFTER the new thumbnail is persisted, so a
        // failed or cancelled regeneration leaves the cell stale and it is
        // retried the next time it realizes.
        let _ = db.set_thumbnails_stale(&[image_id], false);
```

And on the UI side, remove the id from `stale_regen_inflight` when `ThumbReady` arrives for it,
so a later re-staling can trigger again:

```rust
            AppEvent::ThumbReady { image_id, .. } => {
                self.state.stale_regen_inflight.remove(&image_id);
                /* … existing handling … */
            }
```

- [ ] **Step 5: Write the guard test**

The pure decision — *should this cell regenerate?* — is worth extracting so it can be tested
without a GPU or a grid:

```rust
/// Whether a realized cell should spawn a stale-thumbnail regeneration.
/// Extracted so the de-dup guard is testable without a grid or a GPU.
pub fn should_regen_stale(stale: bool, already_inflight: bool) -> bool {
    stale && !already_inflight
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_cell_regenerates_once_not_every_frame() {
        assert!(should_regen_stale(true, false), "first realize spawns");
        assert!(!should_regen_stale(true, true), "already in flight — must NOT re-spawn");
        assert!(!should_regen_stale(false, false), "fresh cell never spawns");
        assert!(!should_regen_stale(false, true));
    }
}
```

Run it, watch it fail (function absent), implement, watch it pass.

- [ ] **Step 6: Scoped gate**

```bash
cargo fmt -p ferrolite-app -- --check
cargo clippy -p ferrolite-app --all-targets -- -D warnings
cargo test -p ferrolite-app
cargo test -p ferrolite-catalog
```

- [ ] **Step 7: Commit**

```bash
git add ferrolite-app/src/library/grid.rs ferrolite-app/src/state.rs ferrolite-app/src/develop/thumb_regen.rs ferrolite-app/src/app.rs
git commit -m "feat(app): regenerate stale thumbnails lazily when a cell realizes

The consumer half of the stale flag, and what makes a 500-image preset apply
instant: the grid is already virtualized, so a cell only pays for its
decode + GPU render + encode when the user actually looks at it.

Guarded against the re-spawn storm the ThumbMissing sticky guard already
protects against — a stale cell stays on screen for many frames, so without
an in-flight set it would re-spawn a full render every frame. The flag is
cleared only after the new thumbnail is persisted, so a failed regeneration
is retried on the next realize."
```

---

## Coordinator: end-of-branch

After Task 9, the **coordinator** (not a subagent) runs:

```bash
rustup update stable
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --all-targets
cargo test --workspace
```

Then hands the author a numbered visual test plan citing fixtures by name from
`fixtures/raw/FIXTURES.md`, and **holds** for their hands-on results before finishing the branch.

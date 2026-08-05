# P7 — Presets, Copy/Paste/Sync & Batch Edits — Design

> **Status:** Design — approved in brainstorming (2026-08-05); pending the author's final review of
> this spec, then writing-plans.
> **Date:** 2026-08-05
> **Branch:** `feat/p7-presets-sync-batch` (off `main`) — one branch per v2 phase.
> **Parent:** v2 architecture map (`2026-07-05-ferrolite-v2-architecture-map.md`) §4 **P7**.
> **Builds on:** Spec 2's `EditDoc`/`OpStack` + XMP sidecar; Spec 3's export core and the Export
> module; P1's `MaskLayer` (present in the document model but deliberately out of scope here, §2
> P7-D2); the toast/notification system (`2026-07-17-toast-notifications-design.md`).
> **Proves:** Three phases of adjustment machinery (P2/P3/P4) become reusable across a shoot —
> saved presets, ad-hoc copy/paste, and batch application over a selection — with no new
> dependencies, no engine-tier changes, and no new rendering. Also closes a live defect: batch
> export currently ignores every persisted edit.

---

## 1. Goal & requirements

1. **Make edits reusable.** Today every edit is one image at a time, from scratch, with no way to
   carry it forward. Nothing in P2/P3/P4's adjustment machinery can be applied to a second image.
   This phase adds saved **presets**, ad-hoc **copy/paste**, and **batch apply** over a selection.
2. **One mechanism, three entry points.** Presets, copy/paste, and sync are the *same* operation —
   a partial edit document applied to N targets. They share one merge function, one job, one set
   of tests (§3).
3. **Partial by group.** Applying settings must not clobber the target's framing, optics, or
   unrelated adjustments. Both presets and paste operate on ~10 named groups (§3.2).
4. **Instant at scale.** Applying to 500 images must return immediately. The sidecar writes are
   milliseconds; thumbnail regeneration is deferred to the already-virtualized grid (§5.2).
5. **Recoverable.** A mis-aimed batch over a large selection must be undoable in one action (§5.4).
6. **Fix batch export.** `ferrolite-app/src/export/batch.rs` hardcodes `OpStack::default()`, so
   batch export renders every image **unedited**. Its justifying comment ("per-image edits are not
   persisted") is stale — sidecars persist edits and `read_ops` exists. This phase makes batch
   export honour persisted edits (§7).
7. **No new dependencies, no engine-tier changes.** All work is photo-tier
   (`ferrolite-app`, `ferrolite-catalog`, `ferrolite-pipeline`, `ferrolite-export`).

### Non-goals

* **No mask sync (P7-D2).** Mask layers are excluded; the Masks checkbox ships greyed with an
  honest reason. **This deviates from the parent map**, whose P7 entry says "Presets that carry
  masks (P1) apply through the same op-stack path". Deliberate, not an oversight — see §2 P7-D2.
* **No export-time preset application.** Exporting "as B&W" without touching the masters was
  considered and cut (§2 P7-D6). Batch export applies each image's *own* persisted edits, nothing
  more.
* **No preset folders, tags, ratings, or import/export of preset packs.** A flat list of
  user-authored presets. Revisit when the list actually gets unwieldy.
* **No auto-apply on import.** A "default develop preset applied at ingest" is a separate concern
  touching the ingest pipeline; out of scope.
* **No cross-image adaptive masks.** The map's "adaptive presets" open question is deferred with
  mask sync itself (P7-D2).
* **No `texture` / `clarity`.** Still unwired and unowned by any phase (see their comment in
  `ferrolite-pipeline/src/local.rs`). Excluded from every group.

---

## 2. Settled decisions (from the 2026-08-05 brainstorm — do not re-litigate)

| # | Question | Decision | Rationale |
|---|---|---|---|
| **P7-D1** | Partial-apply granularity | **Group-level checkboxes** (~10 groups: Light, Colour, Tone curve, HSL, Grading, Detail, Effects, Geometry, Lens, Masks). | Maps 1:1 onto `AdjustmentSet`'s existing structure, so no new taxonomy is invented. Per-field (~40 checkboxes) was rejected: a large dialog to build, and every future control must be registered in it. All-or-nothing was rejected as too blunt — crop and lens must not travel by default. |
| **P7-D2** | Do presets/sync carry mask layers? | **No — masks are out of P7.** The Masks checkbox is greyed with a reason. | Author's call, made with the tradeoff visible. Every mask component *is* normalized `[0,1]²` and would serialize, but `Brush` strokes are drawn against one photo's content and land arbitrarily on another, and `Imported` (AI) components carry a `RasterHandle` into a per-image store that dangles elsewhere. Deferring means the whole portability story — including A2's `Imported` masks and the map's "adaptive presets" question — gets designed **once**, alongside A2, rather than twice. Cost: the easy wins (gradient/range masks, which port cleanly) are left on the table for now. |
| **P7-D3** | Preset format & partial-apply semantics | **A preset declares which groups it owns.** At save time the author ticks the covered groups; applying overwrites exactly those and leaves the rest of the target untouched. | Symmetric with the paste dialog, and it is the only option that can express *"set exposure to 0"* — auto-partial (store non-identity fields only) cannot distinguish an identity value from "not covered". Carrying intent in the preset also means applying one needs **no dialog** (§6.3). |
| **P7-D4** | Thumbnail refresh after a batch apply | **Lazy** — mark affected thumbnails stale; the virtualized grid regenerates a cell when it realizes. | Each regeneration is a full decode + GPU render + encode (`spawn_regen_edited_thumbnail`). Eager regeneration of 500 images is minutes of GPU work and risks the saturation that already forced batch *export* to one-at-a-time. Lazy leans on the virtualization CLAUDE.md already mandates: you pay only for cells you actually look at. |
| **P7-D5** | Batch undo | **Reuse the existing `Action::Undo`**, dispatched by context, hinted in the result toast. | `apply_undo_redo(ctx, frame, is_undo)` is already a single funnel reached from the shortcut, the Edit menu and the Develop palette. Extending it means no new `Action` variant, no `Action::ALL` resize, no new `GROUPS`/Help entry — and `Undo` is already discoverable in both required places, so CLAUDE.md's keybind rule is satisfied without new work. Ctrl+Z keeps meaning "undo the last thing I did". |
| **P7-D6** | Does P7 fix batch export? | **Yes — read each item's persisted edits.** Export-time preset application was considered and cut. | The hardcoded `OpStack::default()` is a live defect: edit 50 images, batch-export, get 50 unedited files. The fix is small (the `read_ops` call `thumb_regen.rs` already makes) and is the other half of what the map names for this phase. Export-time presets is a second feature with its own UI; not now. |

---

## 3. Data model

### 3.1 `EditPatch`

The single currency of this phase — a partial `EditDoc`:

```rust
/// A partial edit document: values plus the set of groups it authoritatively
/// writes. Applying merges only the owned groups into a target.
pub struct EditPatch {
    pub version: u32,
    /// Which groups this patch writes. Groups outside this set are ignored on
    /// read and left untouched on the target.
    pub owns: GroupSet,
    /// Value carrier. Only fields belonging to an owned group are meaningful;
    /// the rest are whatever `Default` produced.
    pub doc: EditDoc,
}
```

`GroupSet` is a small bitflag set over §3.2's groups.

**The merge is one pure function:**

```rust
impl EditPatch {
    /// Return `target` with every owned group replaced by this patch's values.
    pub fn apply_to(&self, target: &EditDoc) -> EditDoc;
}
```

Everything else in P7 is plumbing around this call. A preset is an `EditPatch` loaded from disk; a
copy is one held in memory; a sync is one taken from the source image. Same struct, same merge,
same tests.

### 3.2 The groups

Derived from the real `AdjustmentSet` / `EditDoc` fields — no new taxonomy.

| Group | Fields written | Default in dialog |
|---|---|---|
| `LIGHT` | `exposure`, `contrast`, `highlights`, `shadows`, `whites`, `blacks` | on |
| `COLOUR` | `temp`, `tint`, `saturation`, `hue`, `vibrance`, `color` (`ColorSwatch`) | on |
| `CURVE` | `tone_curve` | on |
| `HSL` | `hsl` | on |
| `GRADING` | `color_grade` | on |
| `DETAIL` | `sharpen`, `noise_reduction` | on |
| `EFFECTS` | `dehaze` | on |
| `GEOMETRY` | `geometry` — crop, `angle_deg`, `aspect`, `keystone_v/h` | **off** |
| `LENS` | `distortion`, `tca`, `vignetting` **amounts only** — see below | **off** |
| `MASKS` | `layers` | **greyed** (P7-D2) |

**`LENS` writes amounts, never capture context (load-bearing).** `LensCorrection` carries
`lens_id`, `focal_len`, `aperture` and `crop_factor`, all per-image EXIF. Copying those wholesale
would stamp the source's focal length onto the target and bake a wrong correction. The `LENS`
group writes only the three `Correction { enabled, amount }` values; the target keeps its own
resolved lens and capture context. If the target has no `LensCorrection` at all (unmatched lens),
the group is a no-op for that image and is reported as skipped.

**`GEOMETRY` and `LENS` default off** because framing and optics are per-image. Both remain
available — the default just reflects what is usually wanted.

`texture` / `clarity` belong to no group (still unwired; see §1 non-goals).

---

## 4. Preset storage

**Presets are files on disk. There is no catalog table.**

```text
<base>/ferrolite/presets/<sanitized-name>.json
```

— where `<base>` is resolved by the same logic as `catalog.db` (`default_db_path` in `state.rs`:
`LOCALAPPDATA`, else `XDG_DATA_HOME`, else `HOME`, else the current directory). On the author's
Windows machine that is `%LOCALAPPDATA%/ferrolite/presets/`.

Contract 2 says the catalog is a cache, rebuildable from files on disk. A user-authored preset is
**not** derivable from image files, so it cannot live only in the catalog. Making it a plain file
satisfies the contract by construction. And with the file as the source of truth, a catalog index
buys nothing at realistic scale (tens to low hundreds of small JSON files, read once at startup),
so **no table is added** — nothing cached means nothing that can go stale. The `export_queue`
table remains the precedent for state that genuinely needs persisting; presets are not that.

**Format** is the existing `serde_json` codec with the same version gate as `EditDoc`: an unknown
`version` loads as `None`, the preset is skipped with a warning, and the app never panics. Mirrors
`ferrolite-pipeline/src/serialize.rs`'s contract and its test set.

**Names → filenames.** The display name is stored *inside* the JSON; the filename is derived from
it by: replacing every character outside `[A-Za-z0-9 _-]` with `_`, trimming surrounding
whitespace, collapsing runs of `_`, and truncating to 64 characters. An empty or all-invalid name
is rejected by the dialog. Because sanitization is lossy (`Warm/Cool` and `Warm_Cool` collide),
uniqueness is checked against the **resulting filename**, not the display name, and a collision is
reported in the dialog — never a silent overwrite. Listing reads display names from the files, so
the sanitized filename is never shown to the user.

---

## 5. Applying

### 5.1 The job

One Background `ferrolite-jobs` job for the whole batch, with priority, cancellation and a
progress sink (contract 1). For each target it reads the current doc, merges the patch, and writes
the sidecar.

**Deliberately not one-at-a-time.** Batch *export* processes items sequentially because each item
is a full-res render plus a CPU-heavy encode, and running several saturated the machine (see
`export/batch.rs`'s module doc). Batch *edit* does no rendering at all — 500 sidecar writes is
milliseconds of I/O. The export constraint does not transfer and is not inherited.

**The image currently open in Develop is excluded** from the target set, and the result reports
it, so a batch never races the live editing session's own sidecar writes.

### 5.2 Thumbnail staleness

`thumbnails` is `(image_id, level, w, h, format, blob)` — no version column. **Schema v7 → v8:**

```sql
ALTER TABLE thumbnails ADD COLUMN stale INTEGER NOT NULL DEFAULT 0;
```

The apply job sets `stale = 1` over the affected ids in a single statement. The grid regenerates a
stale cell when it realizes it, via the existing `spawn_regen_edited_thumbnail`, then clears the
flag. Existing rows default to `0`, so upgrading does not trigger a library-wide regeneration.

Develop's own on-leave regeneration (`should_regenerate_on_leave`) is unchanged — the flag exists
for writes that do *not* go through a Develop session.

**Rejected alternative, recorded:** storing `hash_serde(&EditDoc)` per thumbnail
(`ferrolite-previews/src/key.rs`) would self-correct across every write path, including external
sidecar edits. It requires reading each sidecar to compute the current hash *on realize* — file
I/O in the scroll path, exactly what the virtualization rule exists to prevent. The flag is
cheaper and sufficient because P7 controls every invalidating write path. This is the upgrade if
external-change detection is ever needed.

`stale` is a re-derivable cache column, which contract 2 explicitly permits.

### 5.3 Failure

Per-item failure is normal — a read-only file, a path that vanished, a `LENS` group against an
unmatched lens. The job returns:

```rust
pub struct BatchResult { pub applied: usize, pub failed: usize, pub skipped: usize }
```

The toast reports honestly and never claims a bare success when items failed:
*"Applied to 47 images. 3 failed (read-only)."*

### 5.4 Undo

The apply job captures each target's prior `EditDoc` into an in-memory snapshot, session-scoped,
replaced by the next batch apply. Undo restores those documents and re-flags the thumbnails stale.

Reached through the existing `apply_undo_redo` funnel (P7-D5): with no active Develop session and
a pending batch snapshot, `Undo` reverts the batch; otherwise behaviour is unchanged. `can_undo`
accounts for the snapshot so the Edit menu item enables correctly.

**Bounded at 2,000 targets.** A serialized `EditDoc` is on the order of 0.5–2 KB, so 2,000
snapshots costs a few MB — comfortably above any realistic batch (§1.4's benchmark is 500) while
ruling out a 50,000-image select-all pinning ~100 MB for the session. Past the cap the snapshot is
not taken and undo is not offered, and the apply dialog says so **before** the user commits
(*"Undo won't be available for more than 2,000 images"*), rather than after.

`BATCH_UNDO_MAX` is the single named constant governing this, in the same spirit as
`NR_STRENGTH_SCALE` — change only the constant to retune.

---

## 6. UI

### 6.1 Library context menu

Extends `image_context_menu.rs`, which already handles n-item actions and `"{n}"` messaging. Every
disabled item carries a hover reason, per house convention:

| Item | Disabled when | Reason shown |
|---|---|---|
| Copy settings | source is identity | "This image has no edits to copy" |
| Paste settings… | clipboard empty | "Copy settings from an image first" |
| Apply preset ▸ | no presets saved | "Save a preset first" |
| Save preset from this image… | source is identity | "This image has no edits to save" |

### 6.2 Develop

A compact **`Presets ▾`** button in the right panel footer beside *Reset all*. Its menu holds
exactly three things:

* the saved presets, each applying to the current image on click;
* **Save current as preset…** — opens the shared modal in save mode;
* **Rename…** / **Delete** per preset, as a submenu on each entry (delete asks for confirmation,
  since the file is removed from disk).

No separate manager window — that is the whole of "manage" for a flat list. A button rather than a
tab, so it does not compete with the Light/Colour/Detail row, and it matches the panel's compact
one-line house style.

### 6.3 The dialog

**Applying a preset opens no dialog** — the preset already declares its groups (P7-D3), so it is
one click. **Pasting does**, because an ad-hoc copy carries no intent and different targets may
want different groups.

One modal serves save and paste, following `mask_components_modal.rs`: the group list with
checkboxes, *Select all* / *None*, and the target count in the title (*"Paste settings to 12
images"*). The Masks row is greyed: *"Mask sync comes with a later phase."*

### 6.4 Conventions

* New icons (preset, copy, paste) get semantic aliases in `icons.rs` — no raw glyphs, no
  hand-drawn shapes.
* The result toast names the undo key via `km.hint(Action::Undo)`, so a rebind updates the text.
* **Per-control reset does not apply.** The group checkboxes are transient dialog state, not
  adjustable editing controls that persist a value; *Select all* / *None* serves the equivalent
  role. Recorded so a reviewer does not read this as a missed convention.
* No new keybinds (P7-D5), so `every_action_is_in_a_settings_group` is unaffected.

---

## 7. Batch export fix

`ferrolite-app/src/export/batch.rs:161` currently reads:

```rust
let stack = OpStack::default();
```

with a module comment justifying it as "per-image edits are not persisted" — true when written,
stale now. It becomes a per-item read of the persisted sidecar, the same call `thumb_regen.rs`
already makes:

```rust
let stack = read_ops(&sidecar_path(&item.path))
    .and_then(|s| deserialize(&s))
    .unwrap_or_default();
```

plus the per-item lens bake the single-file export path already performs, so batch and single-file
export produce the same pixels for the same image. The stale comments in the module doc and at the
call site are corrected.

No UI change. Batch export simply starts matching what the grid and Develop show.

---

## 8. Testing

The heart of the phase is pure and needs no GPU:

1. **`apply_to` per group** — table-driven: each owned group overwrites its fields; unowned groups
   leave the target byte-identical.
2. **`LENS` writes amounts only** — explicit assertion that `lens_id`, `focal_len`, `aperture` and
   `crop_factor` are *not* copied. This is the rule most likely to be broken by a later
   well-meaning edit, so it gets a test that names it.
3. **Identity is expressible** — a patch owning `LIGHT` with `exposure == 0.0` sets the target's
   exposure to 0, rather than skipping it. The whole justification for `owns` (P7-D3).
4. **Preset serde round-trip + version gate** — unknown version loads as `None` and is skipped, never
   a panic. Mirrors `serialize.rs`'s existing test set.
5. **Filename sanitization + duplicate rejection.**
6. **Undo restores the exact prior documents**, including for items that failed mid-batch.
7. **`BatchResult` counts** are accurate across mixed success/failure/skip.
8. **Schema v8 migration** adds `stale` with default 0 and leaves existing rows fresh — following
   the existing `migrate_creates_v7_*` test pattern.
9. **Batch export renders persisted edits** — fixture-gated on `fixtures/raw/`, following the
   `sample.rw2` precedent so it skips where fixtures are absent.

---

## 9. Contracts, tiers, and CLAUDE.md rules honored

* **Contract 1 (jobs)** — preset scan, preset save/delete, batch apply and thumbnail regeneration
  all run as `ferrolite-jobs` jobs with priority, cancellation and progress. Nothing multi-millisecond
  on the UI thread.
* **Contract 2 (catalog is a cache)** — presets are files on disk, with no catalog table at all;
  the one new column (`stale`) is re-derivable.
* **Contract 3 (decode products additive)** — untouched.
* **Contract 4 (executor is photo-agnostic)** — no new nodes, no executor changes. P7 adds no
  rendering.
* **Contract 5 (VT is source-agnostic)** — untouched.
* **Contract 6 (AI seam)** — untouched; `ferrolite-ai` does not exist yet.
* **Tiers** — all work is photo-tier. No engine-transferable crate is modified.
* **Responsiveness** — §5.1/§5.2. The lazy-thumbnail decision exists specifically to keep a
  500-image apply instant and to avoid the GPU saturation batch export already hit.
* **Icons / keybind tooltips / keybind discoverability** — §6.4.
* **Per-control reset** — §6.4 explains why it does not apply to this phase's controls.

---

## 10. Risks

1. **Write contention with a live Develop session.** Mitigated by excluding the open image from
   batch targets (§5.1) and reporting the exclusion.
2. **Preset filenames.** Collisions and filesystem-invalid characters; sanitize and reject
   duplicates visibly (§4).
3. **Undo memory at extreme selection sizes.** Bounded with an up-front warning (§5.4).
4. **Scope creep in the dialog.** The group list is fixed at §3.2's ten. New controls added by
   future phases join an existing group; adding a *group* is a spec change.
5. **Stale-flag drift** if a future write path forgets to set it. The rejected hash approach (§5.2)
   is the structural fix if this ever bites.

---

## 11. Plan decomposition

Suggested task shape for writing-plans (implementation order):

1. `EditPatch` + `GroupSet` + `apply_to`, with the §8.1–8.3 tests. Pure, no UI, no I/O.
2. Preset store: file format, load/save/delete jobs, sanitization, version gate (§8.4–8.5).
3. Schema v8 `stale` column + the migration test (§8.8).
4. The apply job: merge, write, stale-flag, `BatchResult`, Develop-image exclusion (§8.7).
5. Undo snapshot + `apply_undo_redo` extension + `can_undo` (§8.6).
6. The shared modal (group checkboxes, Select all/None, target count).
7. Library context-menu wiring + greyed reasons.
8. Develop `Presets ▾` footer button.
9. Batch export fix + fixture-gated test (§7, §8.9).

Tasks 1–5 are engine/logic and testable without the running app; 6–8 are the UI surface the
author's hands-on test will judge.

---

## 12. Reference

* **v2 architecture map** — `2026-07-05-ferrolite-v2-architecture-map.md` §4 P7, §5 contracts, §6
  build order. Note the §1 non-goal deviation on masks.
* **Spec 2 (Editing)** — `2026-06-30-spec2-editing-design.md`: `EditDoc`/`OpStack`, the XMP sidecar.
* **Spec 3 (Color & Export)** — `2026-07-01-spec3-color-and-export-design.md`: the export core the
  §7 fix lands in.
* **Unified maskable adjustments** — `2026-07-28-unified-maskable-adjustments-design.md`: the
  `AdjustmentSet` structure §3.2's groups are derived from.
* **P4 design** — `2026-07-31-p4-noise-reduction-and-sharpening-design.md`: `sharpen` /
  `noise_reduction`, the `DETAIL` group's fields.
* **Toasts** — `2026-07-17-toast-notifications-design.md`: the `Notifications` API §5.3 reports through.
* **Design system** — `docs/design/V2/README.md`: the Develop panel §6.2 adds to.
* **Fixtures** — `fixtures/raw/FIXTURES.md`: the set §8.9's export test draws from.

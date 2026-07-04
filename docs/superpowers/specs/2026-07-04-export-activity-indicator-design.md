# ferrolite — Export activity indicator (design)

> **Status:** Design — pending user review (2026-07-04); then writing-plans.
> **Date:** 2026-07-04
> **Branch:** `feat/spec4-2-avif-jxl-export` (follow-on QoL surfaced during Spec 4.2's
> visual test, alongside the batch-export serialization fix — same branch).
> **Context:** Spec 3 §8 export core; Spec 4.2 AVIF/JPEG-XL; the batch-export
> serialization fix (`spawn_batch` now one sequential Background job).
> **Proves:** a persistent, reliable "an export is running" indicator visible in every view.

---

## 1. Goal

A persistent indicator in the **global bottom status bar** (visible in Library, Develop, and
Export views) that, **while any export runs**, shows:

- the **current filename** being exported,
- **count + progress bar** (`completed/total` + a bar),
- a **failed count** (when any images failed),
- a **cancel ✕** that stops the export/batch from any view.

It **covers both** single-file and batch export from one code path, and **disappears when export
finishes** (the final "Exported…/failed" summary still flashes via the existing transient status
text). This is `ferrolite-app`-only QoL — no engine/GPU/decode changes.

**Problem it fixes:** today the only export feedback is the transient `state.warning` string
("Exporting… {done}/{total}"), styled red like an error and **overwritten** by any other warning
("Added to export queue.", "Preview cache purged.", errors). Batch has structured state
(`BatchExportState`) only while running; **single export has no persistent state at all** — so
there is no reliable, non-transient signal that an export is in progress.

---

## 2. Approach: one unified `ExportActivity`

Replace `BatchExportState` with a single `ExportActivity` that **both** the single-file and batch
flows populate — the one source of truth the indicator reads:

```rust
pub enum ExportKind {
    Single,
    Batch,
}

pub struct ExportActivity {
    pub kind: ExportKind,
    pub total: usize,               // images to export (1 for single)
    pub completed: usize,           // images finished (ok or failed)
    pub failed: usize,
    pub current_name: Option<String>, // filename of the in-flight image
    pub tile_done: u32,             // per-image render progress (current image)
    pub tile_total: u32,
    pub handles: Vec<JobHandle>,    // cancel target(s): single OR batch
    pub warnings: Vec<String>,      // rolled into the final summary
}
```

`ExportActivity` subsumes `BatchExportState`'s role (`total`/`completed`/`failed`/`handles`/
`warnings`). `state.batch: Option<BatchExportState>` becomes
`state.export_activity: Option<ExportActivity>`.

**Rejected alternative:** keep `BatchExportState` and add a separate single-export state, branching
in the indicator. Rejected — it duplicates the render + cancel logic and leaves single export a
second-class citizen with its own code path.

### 2.1 Helpers
```rust
impl ExportActivity {
    pub fn is_done(&self) -> bool { self.completed >= self.total }
    pub fn cancel_all(&self) { for h in &self.handles { h.cancel(); } }
    /// (completed + current-image tile fraction) / total, clamped 0..=1.
    pub fn fraction(&self) -> f32 { /* see §3 */ }
}
```

---

## 3. Data flow (events)

**Progress-bar fraction** = `(completed as f32 + tile_frac) / total`, where
`tile_frac = tile_done / tile_total` (0 when `tile_total == 0`), clamped to `0..=1`. This gives
smooth motion for both flows: single = the one image's tiles; batch = whole images plus the
in-flight image's tiles.

**Single-file export (`export/mod.rs::spawn_export`, `app.rs`):**
- Already emits `ExportProgress { image_id, done, total }` (per-tile) and `ExportFinished`.
- **Add:** at spawn, set `state.export_activity = Some(ExportActivity { kind: Single, total: 1,
  current_name: <open image filename>, handles: vec![handle], .. })` — which requires **capturing
  the job handle `spawn_export` currently discards** (this is what gives single export a cancel it
  lacks today).
- `ExportProgress` updates `tile_done`/`tile_total`. `ExportFinished` folds the (Single-kind)
  activity via `item_finished` — so `is_done()` becomes true and the segment hides (see §4) — and
  sets the transient summary text as today. The activity is **not** nulled (a new export replaces
  it); this keeps the mechanism identical to batch and preserves any consumer that reads the
  finished activity.

**Batch export (`export/batch.rs`, `app.rs`):**
- `spawn_batch` already returns one handle → stored in `export_activity.handles`.
- **Add** a lightweight `AppEvent::ExportItemStarted { name: String }` sent by the sequential batch
  job before each item (the output file basename); the app sets `current_name` from it (via
  `start_item`) and resets `tile_done/tile_total`. (Carrying the name directly avoids a
  `state.images` lookup and works even if the image isn't in the current folder view.)
- Wire `run_one`'s currently-`noop` progress closure to emit per-tile progress (throttled ~every 8
  tiles + on completion, exactly like single export) so the bar advances within each image.
- `BatchItemFinished` still bumps `completed`/`failed` (folded in `events.rs`); when
  `is_done()`, the segment hides (see §4). The activity is **not** nulled — it persists so the
  Export module's "Done — N exported, M failed" summary survives; a new export replaces it.

**Cancel ✕** → `activity.cancel_all()` cancels the stored handle(s); `run_export` already checks
the token per tile and aborts; the terminal finish event marks the activity done (segment hides).
The Export module's existing cancel button reuses the same `cancel_all()` path.

---

## 4. Rendering (`status_bar.rs`)

- A new export segment rendered only when `state.export_activity.is_some() && !is_done()`, placed
  just after the selected-image EXIF text (left of the right-aligned ingest cluster) so it reads
  left-to-right — **mirroring the ingest `ProgressBar` pattern already present** in
  `status_bar::show`.
- Styled as **active info** (normal/accent text + a progress bar), **not** the red
  `SEMANTIC_RED` the `warning` channel uses.
- Layout (single line, right cluster):
  `⭱ Exporting {name}  {completed}/{total}  [progress bar]  {K failed}  [✕]`
  - `{name}` **truncated** to ~24 chars (middle-ellipsis or tail-ellipsis) so the bar never
    overflows a narrow window; batch shows the in-flight image's name, single shows the one file.
  - `{K failed}` shown only when `failed > 0`.
  - `[✕]` is a small button; on click → `cancel_all()`.
- The text assembly is a **pure function** `export_status_text(&ExportActivity) -> String`
  (+ a `truncate_name` helper), unit-tested; egui only lays out label + bar + button.

**Completion behavior:** when `is_done()` becomes true (batch: last `BatchItemFinished`; single:
`ExportFinished`), the segment is **hidden by the `!is_done()` gate** — the activity itself is
**not** nulled, so the Export module keeps its "Done — N exported, M failed" summary. A subsequent
export replaces the activity. The final "Exported N/N" / failure summary also flashes via the
existing status text (decided 2026-07-04 — no lingering "done ✓" state in the status-bar segment,
to avoid a second transient mechanism).

---

## 5. Threading / contracts

Pure UI-thread reads of cheap in-memory state; **all export work stays on `ferrolite-jobs`**
(unchanged). No engine-crate, GPU, VT, decode, or catalog changes — `ferrolite-app` only (status
bar + `AppState` field + events). Honors CLAUDE.md (no UI-thread blocking; list/grid rendering
untouched) and the v1-map §5 contracts (nothing touched there). No new dependencies.

---

## 6. Error handling

- Per-image failures during a batch increment `failed` and push a warning; the batch continues
  (unchanged). The indicator shows the running `{K failed}` count.
- Cancel mid-export: the in-flight image aborts (`ExportError::Cancelled`), the finish event marks
  it and clears the activity; partial output handling is unchanged from Spec 3 §10.
- If `state.images` has no row for a started `image_id` (edge case), `current_name` falls back to
  `None` and the segment omits the filename — never panics.
- Activity is always cleared on the terminal event, so the indicator can never "stick" after an
  export ends.

---

## 7. Testing (TDD; CLAUDE.md gate, then hold for the author's visual test)

**Pure CPU / logic (every OS in CI):**
- `export_status_text`: single vs batch; `{completed}/{total}`; `{K failed}` present only when
  `failed > 0`; filename included/omitted; percent/fraction rendering.
- `truncate_name`: long names truncated to the cap with an ellipsis; short names unchanged;
  multi-byte-safe (no panic on a char boundary).
- `ExportActivity`: `fraction()` math (tile blend, `tile_total == 0`, clamp); `is_done()`;
  transition folds — start → progress → item finished → `is_done` clears (mirrors the existing
  `events.rs` `BatchItemFinished` fold test).

**egui UI** (the status-bar segment, progress bar, cancel ✕ across all three views):
`cargo build` + clippy + the **author's hands-on visual test**.

**Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace` green → **then STOP and hold for the author's (Jann's) hands-on visual
test** before finishing the branch (CLAUDE.md "Finishing a branch").

---

## 8. Decisions recorded (brainstorm 2026-07-04)

| Question | Decision | Rationale |
|---|---|---|
| Indicator form/placement | **Global status-bar segment** (mirrors the ingest indicator) | Consistent chrome, visible in every view, low risk. |
| State model | **Unified `ExportActivity`** replacing `BatchExportState` | One source of truth; single + batch share render + cancel; single export gains persistent state it lacked. |
| Content | **Filename + count + progress bar + cancel ✕ + failed count** | User selection — clearly answers "what is currently exported" and "is a job still running". |
| Single-export cancel | **Added** (store the previously-discarded job handle) | The unified cancel ✕ must work for single export too. |
| Completion | **Vanish** (final summary via existing transient text) | Avoids a second transient mechanism; indicator = presence-while-running. |
| Progress semantics | **`(completed + tile_frac)/total`** | Smooth bar for both single (tiles) and batch (images + in-flight tiles). |
| Scope / crate | **`ferrolite-app` only, one plan, no new deps** | UI + state + events; no engine/GPU/contract changes. |

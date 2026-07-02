# Thumbnail Perf + Follow-ups Implementation Plan (Round 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make thumbnail generation for a ~3320-image RAW library fast (currently 5+ min), finish the shutdown fix so the app closes promptly while thumbnailing, stop the ingest counter flickering, and give the user real feedback (progress + per-cell state + new work visible).

**Architecture:** The primary win is eliminating the confirmed **double-open of every RAW** — today the rayon metadata pass and the separate Background thumbnail job each independently open the file, read a 1 MiB prefix, and run `rawler::get_decoder`. We add a single-pass decode that returns metadata **and** the preview from ONE `get_decoder`, and restructure ingest so the rayon producer generates the thumbnail inline and streams it to the consumer, which batch-writes row+thumbnail — retiring the re-opening Background thumbnail job entirely. On top of that: mid-job cancel checkpoints + a non-blocking close, an `active_ingests`-gated status with a progress bar, a per-cell "generating" affordance, and surfacing newly-added images.

**Tech Stack:** Rust, egui/eframe 0.29.1, `ferrolite-decode` (rawler 0.7.x + `image`), `ferrolite-catalog` (rusqlite/SQLite WAL), `ferrolite-jobs`, `ferrolite-app` Library UI.

**Root-cause reference (authoritative, precise file:line):** [docs/superpowers/investigations/2026-07-02-thumbnail-perf-and-followups.md](../investigations/2026-07-02-thumbnail-perf-and-followups.md). Read it before starting — every task traces to a confirmed finding there.

## Global Constraints

- **Never block the UI/update thread (CLAUDE.md, load-bearing).** All decode/resize/encode/DB work stays off the UI thread (ingest runs on a job-pool worker + rayon; lazy-load on job-pool workers). The close-time wait MUST be short-bounded (~50–100 ms) and never unbounded. The grid stays virtualized (no per-frame O(all-images) work).
- **Correctness of the fast path already proven** — the R1 `ThumbPixelCache`/`request_thumbnail` fast path (state.rs) is correct; do not regress it. The persisted `thumbnails` table stays the source of truth.
- **RAW decode uses the embedded preview** (`preview_image`), never a full demosaic, on the thumbnail path. Preserve that.
- **`with_ingest_source`'s closure may be called twice** (prefix then mmap fallback) — any combined closure MUST stay side-effect-free on failure (pure reads only).
- **Close behavior (Jann's decision):** short bounded graceful wait (~50–100 ms) leaning on the new cancel checkpoints — not an unconditional instant kill.
- **SQLite is WAL + `synchronous=NORMAL`** (catalog.rs:22) — crash-safe against a killed mid-write; a thumbnail interrupted at close is simply regenerated later.
- **Rust style:** `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings` clean; no `unwrap()`/`expect()` outside tests except the existing `.lock().expect("writer")` idiom.
- **Gate (necessary, not sufficient):** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace` green → then **hold for Jann's hands-on visual test** (generation speed on his ~3320-image library; prompt close mid-thumbnailing; stable counter + progress bar; per-cell generating feedback; new images visible) before finishing the branch.
- **Branch:** `fix/thumbnail-and-shutdown` (continues from the R1 fixes 531e931/91324bb/db62a8f, already on the branch).

---

## File Structure

**Modified:**
- `ferrolite-decode/src/lib.rs` — add `decode_meta_and_preview` (single-pass); RAW impl reuses one `get_decoder`.
- `ferrolite-decode/src/preview.rs` — factored preview-from-decoder helper reused by the combined fn (avoids the redundant second `raw_metadata`).
- `ferrolite-catalog/src/thumbnail.rs` / `catalog.rs` — a batched write path (transaction) for ingest.
- `ferrolite-app/src/ingest.rs` — producer generates thumbnail inline via the single-pass decode; consumer batch-writes row+thumbnail and emits `ThumbReady`; retire ingest's `spawn_thumbnail`/`ThumbRegistered`; per-file cancel already present; add a total-to-process signal.
- `ferrolite-app/src/events.rs` — new ingest-progress model (retire `ThumbRegistered`/`thumb_jobs`-for-ingest); distinguish ingest vs lazy-load `ThumbReady` for counting.
- `ferrolite-app/src/state.rs` — progress fields; `on_exit` close path; keep lazy-load `request_thumbnail`/`ThumbPixelCache`.
- `ferrolite-app/src/app.rs` — `on_exit` short-bounded close.
- `ferrolite-jobs/src/system.rs` — (only if needed) expose `is_shutting_down` to job bodies for checkpoints (already added in R1).
- `ferrolite-app/src/library/status_bar.rs` — `active_ingests`-gated activity + `egui::ProgressBar`.
- `ferrolite-app/src/library/cell_state.rs` + `grid.rs` — a `Generating` cell state + affordance.
- `ferrolite-app/src/library/filter.rs` / `state.rs` — surface newly-added images.

---

## Task 1: Single-pass combined decode (metadata + preview in ONE parse)

**Files:**
- Modify: `ferrolite-decode/src/lib.rs`
- Modify: `ferrolite-decode/src/preview.rs`
- Test: `ferrolite-decode/tests/` (reuse existing fixtures) or a `#[cfg(test)]` module

**Interfaces:**
- Produces: `pub fn decode_meta_and_preview(path: &Path, kind: FileKind) -> Result<(Metadata, ImageBuffer), DecodeError>` — returns camera/exposure metadata AND an upright RGB8 preview `ImageBuffer`, doing ONE `rawler::get_decoder` + ONE `raw_metadata` for RAW (vs. today's two opens + two `get_decoder`s + three `raw_metadata`s across the separate metadata and preview paths).
- Consumes: existing `with_ingest_source` (source.rs:39), `apply_orientation` (orient.rs), `Metadata` (metadata.rs), `standard::{read_metadata_standard, decode_preview_standard}`.

**Design (confirmed against source):** Today RAW metadata (`read_metadata_raw`, lib.rs:58) and RAW preview (`decode_preview_raw`, preview.rs:10) each call `with_ingest_source` → `get_decoder` → (`raw_metadata`) independently. The combined fn does it once. Keep the existing `read_metadata`/`decode_preview` (lazy-load `request_thumbnail` and other callers still use `decode_preview`).

- [ ] **Step 1: Factor a preview-from-decoder helper in `preview.rs`.** Extract the decoder→preview→orient logic so it can run against an already-obtained decoder + already-read metadata (avoids the redundant second `raw_metadata` at preview.rs:23). Add:
```rust
use rawler::decoders::Decoder;
use rawler::rawsource::RawSource;

/// Extract an upright RGB8 preview using an already-constructed decoder and the
/// EXIF orientation already read from its metadata. Shared by `decode_preview_raw`
/// and the single-pass `decode_meta_and_preview` so the file is parsed once.
pub(crate) fn preview_from_decoder(
    decoder: &dyn Decoder,
    src: &RawSource,
    exif_orientation: u16,
) -> Result<ImageBuffer, DecodeError> {
    let params = RawDecodeParams::default();
    let dynimg = decoder
        .preview_image(src, &params)
        .ok()
        .flatten()
        .or_else(|| decoder.full_image(src, &params).ok().flatten())
        .or_else(|| decoder.thumbnail_image(src, &params).ok().flatten())
        .ok_or(DecodeError::NoPreview(std::path::PathBuf::new()))?;
    let oriented = apply_orientation(dynimg, Orientation::from_exif(exif_orientation));
    let rgb = oriented.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    Ok(ImageBuffer::new(w, h, PixelFormat::Rgb8, rgb.into_raw())
        .expect("RGB8 buffer length is w*h*3 by construction"))
}
```
Then refactor `decode_preview_raw` (preview.rs:10) to call it (it reads `raw_metadata` once for orientation, then delegates). Verify the exact `rawler` 0.7.x types (`Decoder` trait object, `RawSource`, `raw_metadata` return) against the vendored rawler — the `get_decoder` return type must be usable as `&dyn Decoder`; adapt the signature to whatever `get_decoder` actually yields (it may be `Box<dyn Decoder>` — then take `&decoder`).

- [ ] **Step 2: Add `decode_meta_and_preview` in `lib.rs`.** RAW branch does one pass; Standard branch composes the two standard calls:
```rust
/// Decode metadata AND an upright RGB8 preview in a SINGLE pass. For RAW this
/// runs ONE `get_decoder` + ONE `raw_metadata`, eliminating the double-open that
/// dominated ingest time (see investigation R2, RC-PERF-1).
pub fn decode_meta_and_preview(
    path: &Path,
    kind: FileKind,
) -> Result<(Metadata, ImageBuffer), DecodeError> {
    match kind {
        FileKind::Raw => crate::source::with_ingest_source(path, |src| {
            let decoder = rawler::get_decoder(src).map_err(rawler_err)?;
            let params = RawDecodeParams::default();
            let meta_raw = decoder.raw_metadata(src, &params).map_err(rawler_err)?;
            let dims = decoder.raw_image(src, &params, true).map_err(rawler_err)?;
            let exif_orientation = meta_raw.exif.orientation.unwrap_or(1);
            let metadata = build_metadata_from_raw(&meta_raw, &dims)?; // factored from read_metadata_raw
            let preview = crate::preview::preview_from_decoder(&decoder, src, exif_orientation)?;
            Ok((metadata, preview))
        }),
        FileKind::Standard => {
            let metadata = standard::read_metadata_standard(path)?;
            let preview = standard::decode_preview_standard(path)?;
            Ok((metadata, preview))
        }
    }
}
```
Factor the `Metadata { … }` construction currently inline in `read_metadata_raw` (lib.rs:67-82) into `fn build_metadata_from_raw(meta: &rawler::..Metadata, dims: &..) -> Result<Metadata, DecodeError>` and call it from BOTH `read_metadata_raw` and here (DRY — do not duplicate the field mapping). Verify the exact rawler metadata/dims types.

- [ ] **Step 3: Test.** Add a test using an existing decode fixture (see `ferrolite-decode/tests/decode.rs` / `standard.rs` for how fixtures are loaded). Assert that for a fixture file, `decode_meta_and_preview` returns metadata equal to `read_metadata(path, kind)` and a preview with the same dimensions as `decode_preview(path, kind)` (i.e. the combined path is consistent with the separate paths):
```rust
#[test]
fn combined_matches_separate_paths() {
    // use the same fixture path + FileKind the existing decode tests use
    let (m, p) = decode_meta_and_preview(FIXTURE, KIND).unwrap();
    let m2 = read_metadata(FIXTURE, KIND).unwrap();
    let p2 = decode_preview(FIXTURE, KIND).unwrap();
    assert_eq!((m.width, m.height), (m2.width, m2.height));
    assert_eq!((p.width, p.height), (p2.width, p2.height));
}
```
Run: `cargo test -p ferrolite-decode`. Expected: PASS (adapt FIXTURE/KIND to a real fixture; if only standard fixtures exist in-tree, cover Standard and note RAW is visual-tested).

- [ ] **Step 4: Gate + commit.**
Run `cargo clippy -p ferrolite-decode --all-targets -- -D warnings` (clean), `cargo fmt`.
```bash
git add ferrolite-decode/src/lib.rs ferrolite-decode/src/preview.rs ferrolite-decode/tests
git commit -m "perf(decode): single-pass decode_meta_and_preview (one get_decoder per RAW)"
```

---

## Task 2: Ingest generates thumbnails inline (retire the re-opening Background job)

**Files:**
- Modify: `ferrolite-app/src/ingest.rs`
- Modify: `ferrolite-app/src/events.rs`
- Modify: `ferrolite-app/src/state.rs`

**Interfaces:**
- Consumes: `decode_meta_and_preview` (Task 1); `generate_thumbnail(&ImageBuffer) -> Result<(Thumbnail, DecodedThumb)>` (thumbnail.rs:42); `put_thumbnail` (thumbnail.rs); `upsert_image`.
- Produces: a new ingest-progress model — `AppState.ingest_total: usize` / `ingest_done: usize` (replacing the `thumb_total`/`thumb_done`/`thumb_jobs`/`ThumbRegistered` machinery for ingest); ingest thumbnails delivered via `ThumbReady` carrying the already-decoded RGBA.

**Design:** The rayon producer (ingest.rs:318) currently reads only metadata and streams `(NewImage, path, kind)`; the consumer (ingest.rs:282-309) upserts and spawns a **separate** Background thumbnail job that re-opens the file. Change:
- Producer: call `decode_meta_and_preview`; on Ok, `generate_thumbnail(&preview)` → stream `(NewImage, Option<(Thumbnail, DecodedThumb)>, path, kind)`. On decode error, stream the `NewImage::failed` with `None`.
- Consumer: `upsert_image` → id; if the thumbnail is present, `put_thumbnail(id, &thumb)` and `tx.send(ThumbReady { image_id: id, rgba: decoded.rgba, w: decoded.w, h: decoded.h })`; emit `Indexed { added: 1 }`. **Remove** the `spawn_thumbnail(...)` call and the `ThumbRegistered` send (ingest.rs:301-307).
- Delete/retire `spawn_thumbnail` + `thumbnail_blocking` from the ingest path (grep for other callers first — if none, remove them; the on-demand edited-thumbnail feature later will add its own job). If removal is noisy, leave `thumbnail_blocking` unused-but-`#[allow(dead_code)]` and note it — but prefer removal.
- Progress: after Phase A + the `needs_reingest` filter, the producer knows how many files it will process. Count them and emit once, e.g. `AppEvent::IngestPlanned { total }`, folded into `state.ingest_total`. The consumer's per-row completion increments `state.ingest_done`. Reset both on ingest start (`active_ingests` 0→1) and clear on `IngestDone` when `active_ingests` hits 0.
- **Counting ingest vs lazy-load:** lazy-load `request_thumbnail` (state.rs) also emits `ThumbReady`. Only ingest completions should advance `ingest_done`. Simplest: drive `ingest_done` off the consumer's `Indexed` emission (one per ingested row) rather than off `ThumbReady` — i.e. `ingest_done` counts indexed rows, which in the new inline model equal thumbnails generated. Then `ThumbReady` is purely for texture upload and touches no counter. Retire `thumb_total`/`thumb_done`/`thumb_jobs`/`ThumbRegistered` (grep every use — status_bar, events, state, thumb_profile diag at app.rs:1324 — and update/remove them).

- [ ] **Step 1:** Grep all uses of `spawn_thumbnail`, `thumbnail_blocking`, `ThumbRegistered`, `thumb_jobs`, `thumb_total`, `thumb_done` across `ferrolite-app` and list them (report the list). This defines the blast radius before editing.
- [ ] **Step 2:** Rewrite the producer closure (ingest.rs:318-360) to use `decode_meta_and_preview` + `generate_thumbnail`, streaming `(NewImage, Option<(Thumbnail, DecodedThumb)>, PathBuf, FileKind)`. Keep the per-file `if cancel.is_cancelled() { return; }` (ingest.rs:319). Keep the `needs_reingest`/`force`/rating logic. Keep the `thumb_profile` meta timing; add optional decode/encode timing if trivial.
- [ ] **Step 3:** Rewrite the consumer (ingest.rs:282-309): `upsert_image` → id; `put_thumbnail` + `ThumbReady` when the thumbnail is `Some`; `Indexed` per row; drop `spawn_thumbnail`/`ThumbRegistered`.
- [ ] **Step 4:** Add `ingest_total`/`ingest_done` to `AppState` (+ init + reset) and `AppEvent::IngestPlanned { total }`; fold in `events.rs`. Retire the old thumb counters/events across the grepped sites (Step 1). Update the `thumb_profile::diag` call (app.rs:1324) to the new fields or drop the counts it no longer has.
- [ ] **Step 5:** Build + clippy + the existing app tests.
Run: `cargo clippy --workspace --all-targets -- -D warnings` (clean); `cargo test -p ferrolite-app` (green; fix any tests referencing retired fields/events — update them to the new model, do not delete coverage).
- [ ] **Step 6: Commit.**
```bash
git add ferrolite-app/src/ingest.rs ferrolite-app/src/events.rs ferrolite-app/src/state.rs ferrolite-app/src/app.rs
git commit -m "perf(app): generate thumbnails inline in ingest (one decode per file, no re-open)"
```

---

## Task 3: Batch ingest DB writes in transactions

**Files:**
- Modify: `ferrolite-catalog/src/catalog.rs` (or thumbnail.rs) — a batched write API
- Modify: `ferrolite-app/src/ingest.rs` — consumer commits in batches

**Interfaces:**
- Produces: a way to write N `(NewImage upsert + Thumbnail)` under ONE transaction, committing every ~128 rows and flushing on end/cancel. Reuse the existing `unchecked_transaction()` pattern (catalog.rs:164/203/509).

**Design:** Today the consumer calls `upsert_image` then (new in Task 2) `put_thumbnail`, each an autocommit INSERT under the writer lock (RC-PERF-3). Batch them: the consumer accumulates results and, every N rows (and at channel close), takes the writer lock once and commits a transaction. Must still return the `image_id` per row for `ThumbReady`/`Indexed` — so either upsert within the txn and read the id back, or keep upsert per-row for the id but batch the thumbnail blobs. Simplest correct approach: a `Catalog::with_ingest_txn(|txn| { … })`-style helper, or a `put_thumbnails_batch(&[(i64, Thumbnail)])` that wraps one transaction. Choose the one that keeps `image_id` retrieval correct; document the choice.

- [ ] **Step 1:** Add the batched write helper on `Catalog` (transaction-wrapped), mirroring `unchecked_transaction()` usage at catalog.rs:164. Keep autocommit behavior identical per-row semantically (INSERT ... ON CONFLICT).
- [ ] **Step 2:** Update the ingest consumer to accumulate and flush every ~128 rows + a final flush at channel close AND on cancel (so no rows are lost). Keep emitting `Indexed`/`ThumbReady` per row as they are committed (or per batch — pick per-row so the grid stays live; the texture upload is already frame-budgeted).
- [ ] **Step 3:** Build + clippy + `cargo test -p ferrolite-app -p ferrolite-catalog` green.
- [ ] **Step 4: Commit.**
```bash
git add ferrolite-catalog/src ferrolite-app/src/ingest.rs
git commit -m "perf(catalog): batch ingest row+thumbnail writes in transactions"
```

---

## Task 4: Finish the shutdown fix — cancel checkpoints + short bounded, non-hanging close

**Files:**
- Modify: `ferrolite-app/src/ingest.rs` (cancel checkpoints in the inline decode/generate loop)
- Modify: `ferrolite-app/src/app.rs` (`on_exit` short bound)
- Modify: `ferrolite-app/src/state.rs` (if `request_thumbnail`'s lazy job wants an is_shutting_down early-out)

**Design (confirmed):** `on_exit` runs on the UI thread then `process::exit(0)`. Today it blocks the full 500 ms `join_with_timeout` because in-flight `thumbnail_blocking` (now the inline producer decode+generate) has no cancel checkpoint. After Task 2 the heavy work is the producer's per-file closure, which ALREADY checks `cancel.is_cancelled()` at the top of each file (ingest.rs:319) — so a cancelled ingest bails within ~one file. Add one more checkpoint between decode and the (CPU-heavy) `generate_thumbnail` if a single file's work is large. Then shorten the close bound.

- [ ] **Step 1:** In the Task-2 producer closure, add a second `if cancel.is_cancelled() { return; }` after `decode_meta_and_preview` and before `generate_thumbnail`, so a cancel during a slow decode skips the resize/encode.
- [ ] **Step 2:** In `app.rs on_exit`, reduce the bounded join to ~75 ms (Jann's "short bounded graceful wait" decision): `cancel_pending_jobs()` → `jobs.request_shutdown()` → `let _ = jobs.join_with_timeout(Duration::from_millis(75));`. Add an `eprintln!`/log line if it returns `false` (the R1 reviewer's recommended diagnostic).
- [ ] **Step 3:** Verify `cancel_pending_jobs` cancels the ingest handle (it does, state.rs:462) so the producer's per-file checks fire. Confirm lazy-load `request_thumbnail` jobs are short (DB read + JPEG decode) — no checkpoint needed. Report the reasoning.
- [ ] **Step 4:** Build + clippy + `cargo test -p ferrolite-app -p ferrolite-jobs` green.
- [ ] **Step 5: Commit.**
```bash
git add ferrolite-app/src/ingest.rs ferrolite-app/src/app.rs ferrolite-app/src/state.rs
git commit -m "fix(app): cancellable in-flight ingest + short bounded close (no UI-thread hang)"
```

---

## Task 5: Stable, informative status — `active_ingests` gate + progress bar

**Files:**
- Modify: `ferrolite-app/src/library/status_bar.rs`

**Interfaces:**
- Consumes: `AppState.active_ingests: usize` (state.rs:77), `ingest_total`/`ingest_done` (Task 2).

**Design (confirmed):** The flicker is `thumb_done == stale thumb_total → "Idle"` per frame. Gate on `active_ingests > 0` (only flips at ingest start/end) and show a progress bar for `ingest_done/ingest_total`.

- [ ] **Step 1: Failing test.** Update `activity_text` (status_bar.rs:6) to take an `is_ingesting: bool` (from `active_ingests > 0`) + `ingest_done`/`ingest_total`, and add:
```rust
    #[test]
    fn activity_generating_while_ingesting_regardless_of_counts() {
        // done transiently == total mid-scan must NOT flip to Idle while ingesting.
        assert_eq!(activity_text(true, 7, 7), "Generating 7/…"); // or your chosen format
    }
    #[test]
    fn activity_idle_when_not_ingesting() {
        assert_eq!(activity_text(false, 0, 0), "Idle");
    }
```
- [ ] **Step 2:** Implement: `if !is_ingesting { "Idle" } else { format!("Generating {ingest_done}/{ingest_total}") }` (choose a clear format; total may still be growing early — that's fine, it no longer flips to Idle). Update the caller `show` (status_bar.rs:19) to pass `state.active_ingests > 0` + the new fields, and add an `egui::ProgressBar::new(done as f32 / total.max(1) as f32)` shown only while ingesting.
- [ ] **Step 3:** `cargo test -p ferrolite-app status_bar` green; clippy clean.
- [ ] **Step 4: Commit.**
```bash
git add ferrolite-app/src/library/status_bar.rs
git commit -m "fix(app): stable ingest status gated on active_ingests + progress bar"
```

---

## Task 6: Per-cell "generating" feedback

**Files:**
- Modify: `ferrolite-app/src/library/cell_state.rs`
- Modify: `ferrolite-app/src/library/grid.rs`

**Design:** `cell_state` (cell_state.rs:12) is 3-way (Ready/Placeholder/Failed); a generating cell looks identical to an untouched one. Add a `Generating` state (or a bool) shown while an ingest is active and the cell has no texture yet, rendered as a subtle animated affordance (spinner/shimmer) in the `Placeholder` branch (grid.rs:257).

- [ ] **Step 1:** Extend `cell_state` to distinguish "generating" — pass `is_ingesting` (and/or whether the id is in `thumb_pending`) into it; add the variant/flag with a unit test for the mapping (Ready when textured; Generating when not textured + ingesting; Placeholder when not textured + idle; Failed unchanged).
- [ ] **Step 2:** In `grid.rs` `paint_cell` placeholder branch (grid.rs:257), render a subtle spinner/shimmer for `Generating` (reuse `egui::Spinner` or an animated alpha; keep it cheap — O(visible cells)). `ctx.request_repaint()` while animating only if an ingest is active.
- [ ] **Step 3:** clippy clean; `cargo test -p ferrolite-app` green.
- [ ] **Step 4: Commit.**
```bash
git add ferrolite-app/src/library/cell_state.rs ferrolite-app/src/library/grid.rs
git commit -m "feat(app): per-cell generating affordance during ingest"
```

---

## Task 7: Surface newly-added images

**Files:**
- Modify: `ferrolite-app/src/library/filter.rs` and/or `ferrolite-app/src/state.rs`

**Design:** Default sort is EXIF `CaptureTime ASC`, so imports land by shoot date, not where the user watches. A `ViewSource::RecentlyAdded` (`added_at DESC`) already exists (query.rs:104, panel.rs:28). Smallest, clearest change: when an ingest is active/just finished for a freshly-opened folder, make new work visible — either (a) auto-select/scroll behavior, or (b) default a newly-opened folder's view sort toward `added_at DESC`, or (c) simply ensure the existing `RecentlyAdded` view is easily reachable and document it. **Pick the least-invasive option that satisfies "the user can see new thumbnails appear"** and confirm with the reviewer; do NOT globally change the default `CaptureTime` sort for all views without the author's sign-off (flag as a plan-mandated decision point).

- [ ] **Step 1:** Implement the chosen minimal change (recommend: default a just-opened folder / active-ingest view to `added_at DESC` so newly-thumbnailed rows appear at the top, leaving the general default unchanged). Add a unit test on the query/sort selection if it's pure.
- [ ] **Step 2:** clippy clean; `cargo test -p ferrolite-app` green.
- [ ] **Step 3: Commit.**
```bash
git add ferrolite-app/src/library
git commit -m "feat(app): surface newly-added images during ingest"
```

---

## Final gate (before holding for the author's visual test)

- [ ] `cargo fmt --check` — no diff.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- [ ] `cargo test --workspace` — green (new: `decode_meta_and_preview` consistency; `activity_text` gating; `cell_state` mapping; batched-write correctness if unit-testable; plus updated existing tests).
- [ ] **STOP and hold for Jann's visual test:**
  - Generation of his ~3320-image library is dramatically faster than 5 min (single-pass decode + batched writes).
  - Closing the app mid-thumbnailing exits promptly (no lasting "Not Responding").
  - Status shows a stable "Generating N/total" + progress bar during ingest, "Idle" when done — no Idle↔generating flicker.
  - Un-generated cells show a distinct "generating" affordance during ingest, not a flat gray identical to untouched.
  - Newly-thumbnailed images are visible where the user is looking (top), not buried by capture-date sort.

---

## Self-Review (checked against the investigation + codebase)

**Coverage:** RC-PERF-1 double-open → Task 1 (single-pass decode) + Task 2 (inline generation, retire re-opening job) ✓; RC-PERF-2 concurrency/worker-hogging → Task 2 removes the separate Background thumbnail job flood ✓ (further rayon bounding intentionally deferred — flagged, not silently dropped); RC-PERF-3 unbatched writes → Task 3 ✓; #3 shutdown → Task 4 ✓; #2 counter flicker → Task 5 ✓; #4 feedback → Task 6 (per-cell) + Task 5 (progress bar); #4 sort → Task 7 ✓; #1 scroll → largely resolved by faster generation; a DB warm-load was considered and deferred (noted, not silently dropped).

**Sequencing:** 1→2→3 build the perf core (each independently reviewable); 4 depends on 2's producer structure; 5/6/7 depend on 2's counter model. Task 2 is the largest and retires the old thumb-counter machinery — its Step 1 grep defines the blast radius before editing.

**Deferred (explicitly, for the author):** rayon-vs-jobpool pool unification (RC-PERF-2 deeper fix); DB warm-load of thumbnails for instant first-reveal (#1); global default-sort change (Task 7 keeps it minimal). Call these out at the final review so the author can decide if any are worth a follow-up.

**Type consistency:** `decode_meta_and_preview -> (Metadata, ImageBuffer)` feeds `generate_thumbnail(&ImageBuffer) -> (Thumbnail, DecodedThumb)`; `ThumbReady { image_id, rgba, w, h }` matches the existing event + `upload_thumbnail`. New `ingest_total`/`ingest_done: usize` replace `thumb_total`/`thumb_done: usize`. `activity_text`'s signature change is updated at its sole caller `status_bar::show`.

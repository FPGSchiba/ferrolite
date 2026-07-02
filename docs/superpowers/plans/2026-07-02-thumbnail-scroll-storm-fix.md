# Thumbnail Scroll-Storm + Counter Fix Plan (Round 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Fix the lazy-load "storm" that makes far-scrolling during ingest show no thumbnails (endless spinner flicker), starve generation, and hang shutdown; and fix the status counter that inflates to absurd values (e.g. "0 / 56440 indexed" for 3320 images).

**Architecture:** Two independent root causes (confirmed by investigation, HEAD ff5bf8a):
- **RC1 (storm):** `request_thumbnail` treats "no thumbnail row yet" (`get_thumbnail → Ok(None)`, i.e. ingest hasn't generated it) identically to a decode failure — both send `ThumbFailed`, which clears `thumb_pending`, so the grid re-spawns a `Visible` job for every not-yet-generated visible cell EVERY frame. The grid guard only excludes `Failed` rows, not `Pending` ones. Storm → starves `Background` ingest (Visible preempts Background) → saturates the UI thread (→ shutdown hang).
- **RC2 (counter):** `scanned` (and `ingest_total`) are monotonic `+=` accumulators fed once per ingest pass and never reset between passes; per-root startup rescans + the 10s watcher re-scan run repeatedly, inflating the counter.

**Fix:** (Task 1) stop requesting thumbnails for rows that don't have one yet + a sticky "known-missing" guard so a miss can't re-spawn every frame; (Task 2) reset the scan/ingest counters at the start of each ingest wave so they can't accumulate across passes.

**Tech Stack:** Rust, egui/eframe 0.29.1, ferrolite-app Library UI + ingest.

**Root-cause reference:** the round-2 follow-up investigation (in this session's transcript) + docs/superpowers/investigations/2026-07-02-thumbnail-perf-and-followups.md.

## Global Constraints
- Never block the UI thread; grid stays virtualized (O(visible cells)). No new per-frame O(all-images) work.
- The fix must NOT break the two legitimate paths: (a) a `Done` image with a cold texture must still lazy-load its thumbnail on scroll; (b) a `Pending` image must get its thumbnail via the ingest inline `ThumbReady`→`upload_thumbnail` path when generation reaches it (that path is unchanged).
- No unwrap/expect outside tests except existing idioms. cargo fmt + clippy --workspace --all-targets -D warnings clean.
- Gate green → hold for Jann's visual test (far-scroll during ingest shows no storm; thumbnails fill in as generation reaches them; counter sane; close is prompt) before finishing.
- Branch: fix/thumbnail-and-shutdown (continues R1+R2).

---

## Task 1: Kill the lazy-load re-spawn storm

**Files:** Modify `ferrolite-app/src/library/grid.rs`, `ferrolite-app/src/state.rs`, `ferrolite-app/src/events.rs`.

**Root cause:** grid.rs calls `request_thumbnail` for every visible cell where `!textures.contains(id) && decode_status != Failed` (grid.rs ~:231-235) — so `Pending` (not-yet-ingested) rows spawn jobs; the job finds no blob → `ThumbFailed` → `thumb_pending` cleared (events.rs) → re-spawn next frame.

**Fix (two layers):**
1. **Source guard:** only request thumbnails for rows that actually have one — i.e. `decode_status == Done` (a `Done` row's thumbnail blob is written in the same atomic batch as its row, so `Done ⟺ blob present`). `Pending`/`Failed` rows do not spawn lazy-load jobs; a `Pending` cell shows the `Generating` spinner (while ingesting) or the plain placeholder, and gets its texture from the ingest `ThumbReady` path when generation reaches it. Verify the `DecodeStatus` variants (`Pending`/`Done`/`Failed`?) against `ferrolite-catalog`/`ferrolite-image`.
2. **Sticky known-missing guard (defense):** if a lazy-load job still finds `Ok(None)` (a `Done` row whose blob is somehow absent, or a status race), do NOT let it re-spawn every frame. Distinguish "missing" from "failed": add `AppState.thumb_missing: HashSet<i64>`; when the job gets `Ok(None)` send a new `AppEvent::ThumbMissing { image_id }` (instead of `ThumbFailed`); fold it by removing from `thumb_pending` AND inserting into `thumb_missing`; add `!thumb_missing.contains(id)` to the `request_thumbnail` guard. Clear an id from `thumb_missing` in the `ThumbReady` fold (so once ingest generates it, a later scroll/refresh can pick it up) and clear the whole set on `IngestDone` and in `reset_for_new_folder` (so a completed ingest lets any still-missing cells retry once).

- [ ] **Step 1:** Verify `DecodeStatus` variants and how `rec.decode_status` is available in `grid.rs paint_cell` (it already reads `rec.decode_status != Failed`). Confirm a `Done` variant exists and that ingest sets it on successful generation (grep `DecodeStatus` in ferrolite-catalog + ingest.rs). Report findings.
- [ ] **Step 2:** In `grid.rs`, change the request guard so `request_thumbnail` is only called when `rec.decode_status == DecodeStatus::Done` (keep the `!textures.contains` check). Do not otherwise change cell painting (the `Generating` spinner for un-textured cells while ingesting stays).
- [ ] **Step 3:** Add `thumb_missing: HashSet<i64>` to `AppState` (init in `new` + `for_test`; clear in `reset_for_new_folder`). Add `|| self.thumb_missing.contains(&image_id)` to the early-return guard in `request_thumbnail` (state.rs ~:266).
- [ ] **Step 4:** Add `AppEvent::ThumbMissing { image_id }`. In `request_thumbnail`'s job body, the `Ok(None)` arm sends `ThumbMissing` (the `Err`/decode-failure arm keeps sending `ThumbFailed`). Fold `ThumbMissing` (events.rs): `thumb_pending.remove(id)` + `thumb_missing.insert(id)`. In the `ThumbReady` fold, also `thumb_missing.remove(&image_id)`. In the `IngestDone` fold, `thumb_missing.clear()`.
- [ ] **Step 5:** Test — add a state/events unit test proving the anti-storm invariant: after a `ThumbMissing { id }` fold, `thumb_missing` contains id and a subsequent `request_thumbnail(id)` does NOT submit (guard returns early); after a `ThumbReady { id }` fold, id is cleared from `thumb_missing`. (Test the pure guard/fold logic; the job submission can be asserted via `jobs.pending_count()`/a flag, or restructure so the guard decision is unit-testable.)
- [ ] **Step 6:** Gate: `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test -p ferrolite-app` green; fmt clean.
- [ ] **Step 7:** Commit: `fix(app): stop lazy-load thumbnail re-spawn storm for not-yet-generated cells`.

---

## Task 2: Reset scan/ingest counters per ingest wave

**Files:** Modify `ferrolite-app/src/ingest.rs` and/or `ferrolite-app/src/state.rs`, `ferrolite-app/src/events.rs`.

**Root cause:** `scanned`/`indexed`/`ingest_total` are monotonic `+=` accumulators (events.rs `Scanned`/`Indexed`/`IngestPlanned` folds) reset only by `reset_for_new_folder` (folder switch). Per-root startup rescans (ingest.rs ~:173) + the 10s watcher (app.rs ~:1346) run many passes over the same files, so the counters inflate (56440 ≈ 17×3320).

**Fix:** reset `scanned`, `indexed`, and `ingest_total` at the START of each ingest wave — when `active_ingests` transitions 0→1. `active_ingests` is incremented in `submit_ingest`/the ingest-spawn path (ingest.rs ~:80) on the main thread; reset the three counters there when the pre-increment value is 0. (Concurrent per-root passes within one wave then accumulate only for that wave — summing to the real file count — and a later watcher tick starts a fresh wave from 0.) Confirm the exact increment site and that resetting there is on the UI/main thread (safe to mutate `state`).

- [ ] **Step 1:** Find where `active_ingests` is incremented (ingest.rs ~:80 / submit_ingest) and confirm it runs on the main thread with `&mut AppState` access. Report the exact site.
- [ ] **Step 2:** When starting an ingest and `active_ingests == 0` (about to go 0→1), reset `scanned = 0`, `indexed = 0`, `ingest_total = 0`, `ingest_done = 0` before incrementing. (If multiple counters are better reset via a small `AppState::reset_ingest_counters()` helper, add it and call it here + reuse in `reset_for_new_folder`.)
- [ ] **Step 3:** Verify no double-reset / desync with `reset_for_new_folder` (folder switch already zeroes these — keep it working; the helper can be shared).
- [ ] **Step 4:** Test if a pure helper exists (e.g. `reset_ingest_counters` zeroes the four fields) — a small unit test. Otherwise rely on the gate + visual test.
- [ ] **Step 5:** Gate: clippy clean; `cargo test -p ferrolite-app` green; fmt clean.
- [ ] **Step 6:** Commit: `fix(app): reset scan/ingest counters per ingest wave (no cross-pass inflation)`.

---

## Final gate + hold
- [ ] `cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test --workspace` green.
- [ ] STOP and hold for Jann's visual test: scroll far ahead during ingest → cells show spinners (no thumbnails yet) but NO storm; thumbnails fill in progressively as generation reaches them; the "indexed" counter stays sane (≤ image count); closing during ingest/scroll is prompt.

## Deferred (note at review)
- The 10s watcher re-scanning the whole tree every tick (and per-root startup rescans) is inefficient (repeated full `scan_tree`); reducing re-scan scope is a separate optimization, not needed for these bug fixes.
- Overlapping-ingest shared-counter aggregation (known Minor from R2) remains; per-wave reset bounds it.

## Self-Review
Coverage: RC1 storm → Task 1 (source guard `decode_status==Done` + sticky `thumb_missing`); RC2 counter → Task 2 (per-wave reset). #2 shutdown hang is a consequence of RC1's UI saturation → fixed by Task 1 (no per-frame storm/repaint flood). #1 speed benefits (ingest no longer starved by the Visible storm when scrolling). Both legitimate paths preserved (Done cold-texture lazy-load; Pending → ingest ThumbReady upload). Types: `ThumbMissing{image_id: i64}` mirrors `ThumbFailed`; `thumb_missing: HashSet<i64>` mirrors `thumb_pending`.

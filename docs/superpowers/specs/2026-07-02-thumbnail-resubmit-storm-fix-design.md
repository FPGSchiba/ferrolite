# Spec: Thumbnail Re-Submit Storm Fix

- **Date:** 2026-07-02
- **Branch:** `fix/thumbnail-resubmit-storm` (off `fix/thumbnail-and-shutdown`, which now includes the merged `FERROLITE_DIAG` diagnostics dev-mode)
- **Status:** design approved; ready for implementation plan

## Goal

Eliminate the thumbnail lazy-load **re-submit storm** confirmed by the
`FERROLITE_DIAG` instrumented build on the author's ~2730-image library. This is
the root cause behind both still-reproducing symptoms:

1. After scrolling the Library grid fully down then back up, now-visible
   thumbnails do not load (or load extremely slowly).
2. Closing the app right after that scroll pattern hangs.

Observability only was the previous branch's job; **this branch fixes the bug.**

## Confirmed root cause (from the instrumented trace)

`request_thumbnail` dedups a request against `textures` (final state),
`thumb_pending` (job in flight), `thumb_missing`, and the `thumb_pixels`
fast-path. But there is **no lifecycle stage for "decoded, awaiting GPU
upload."** Decoded pixels sit in `pending_uploads`, which drains at only
`MAX_THUMB_UPLOADS_PER_FRAME` (16) per frame. During that wait an id is in
**none** of the dedup guards:

- `ThumbReady` (in `events.rs`) removes the id from `thumb_pending`/`thumb_handles`.
- `thumb_pixels` and `textures` are populated only inside `upload_thumbnail`,
  i.e. when the item is finally drained from `pending_uploads`.

So a still-visible cell re-requests it every frame, producing two storm feeders:

- **NewSubmit storm:** the guard misses, a new `Visible` job is submitted; it
  decodes and pushes another copy onto `pending_uploads`.
- **FastPath storm:** once pixels are in `thumb_pixels` (texture still queued),
  every frame re-pushes another copy onto `pending_uploads`.

`pending_uploads` fills far faster than 16/frame drains, widening the window —
a runaway positive-feedback loop.

**Trace evidence (post-ingest scroll, ~2730 images, ~64 visible):**
`sub V` (Visible jobs submitted) → **61,996+** and climbing ~5–7k/sec;
`backlog` (`pending_uploads`) → **36,702+**, growing monotonically;
`pix m/s` (pixel-cache miss) ~5,000–7,900/sec; `req/f new`/`fast` = 52–64 every
frame; `tex h/s` collapses to 0. Only ~2730 images exist, so each visible cell
was re-submitted hundreds of times.

Secondary observation (out of scope, see Non-goals): the round-4 off-screen
cancellation is near-ineffective (`cancel_removed 2` vs `cancel_absent 272`)
because jobs dispatch before cancellation lands. With the storm gone the queue
never balloons, so this becomes a cheap near-no-op naturally — no change needed.

## Non-goals

- No change to the round-4 `retain_visible_thumbnail_jobs` cancellation logic.
- No change to initial **ingest generation** performance (RAW decode / encode /
  DB upsert). That is a separate investigation using `FERROLITE_PROFILE_THUMBS`.
- No change to `MAX_THUMB_UPLOADS_PER_FRAME` (raising it does not fix duplicate
  work; frame time was already fine at 4–25 ms during the storm).
- No `ferrolite-jobs` or `ferrolite-vt` changes.

## Design (Approach A — dedicated "awaiting upload" guard)

Add the missing lifecycle stage as one new guard set that mirrors "ids currently
queued in `pending_uploads`."

### New state

`AppState.thumb_uploading: HashSet<i64>` — "ids decoded and queued in
`pending_uploads`, awaiting GPU upload; the bridge between a finished job and a
live texture." Initialised empty in both constructors (`new`, `for_test`).

### Gapless lifecycle

```
request_thumbnail ─▶ thumb_pending (+ thumb_handles)      [job in flight]
      │  ThumbReady (events.rs): remove thumb_pending/handles
      ▼
   thumb_uploading (+ push pending_uploads)                [decoded, awaiting upload]
      │  upload_thumbnail (≤16/frame): remove thumb_uploading
      ▼
   textures (+ thumb_pixels)                               [live texture]
```

### Guard

`request_thumbnail` returns early if
`textured || pending || missing || uploading`. This kills **both** storm
feeders: a cell awaiting upload neither re-submits (NewSubmit) nor re-pushes
(FastPath).

### Touch points (exact)

- **`state.rs`**
  - Add the field (+ both constructors).
  - `request_thumbnail`: add `uploading = self.thumb_uploading.contains(&id)` to
    the early-return guard; the FastPath branch inserts the id into
    `thumb_uploading` before pushing to `pending_uploads` (so it is guarded on
    the next frame and pushes exactly once).
  - `upload_thumbnail`: `self.thumb_uploading.remove(&image_id)` at the top
    (single choke point for *all* uploads — backlog-flush and inline — so the
    guard is always cleared when an id becomes textured; removing an
    id that was uploaded inline without ever being queued is a harmless no-op).
  - `cancel_pending_jobs`: clear both `pending_uploads` and `thumb_uploading`
    (alongside the existing `thumb_pending.clear()`), keeping the upload queue
    and its guard consistent on reset/shutdown and preventing stale old-folder
    thumbnails from uploading after a folder switch.
- **`app.rs`** (`update` upload loop): the stash-overflow branch
  (`else { pending_uploads.push(...) }`) inserts the id into `thumb_uploading`.
  The inline-upload branch (under cap) needs no insert — it uploads the same
  frame and is covered by the `textured` guard thereafter — but it flows through
  `upload_thumbnail`, whose `remove` is a no-op for it.
- **`events.rs`**: unchanged. `ThumbReady` still removes `thumb_pending`/
  `thumb_handles` and returns the pixels; the `thumb_uploading` insert happens at
  the `pending_uploads` push site in `app.rs` (single owner of the stash).

### Invariant

`thumb_uploading` == the set of ids currently sitting in `pending_uploads`.
Maintained by: insert at every push to `pending_uploads` (FastPath in `state.rs`;
stash-overflow in `app.rs`), remove at every upload (`upload_thumbnail`), and
clear both together in `cancel_pending_jobs`.

### Diagnostics (confirm the fix)

Extend the existing `FERROLITE_DIAG` module minimally so the instrumented build
proves the storm is dead:

- Add `ReqOutcome::DedupUploading` and count it in `request_thumbnail`'s guard
  classification (so the "dedup" breakdown shows requests short-circuited by the
  new guard).
- Add `thumb_uploading: usize` to `Gauges` (read `self.thumb_uploading.len()`)
  and surface it in `format_log`/`format_overlay` next to `pending`/`handles`.

## Testing

**Unit tests (`state.rs`), using the real `JobSystem` + a gate job to hold ids
queued (mirroring the existing round-4 tests):**

- `request_thumbnail` for an id already in `thumb_uploading` submits **no** job
  and pushes **nothing** to `pending_uploads` (assert `jobs.pending_count()`
  unchanged and `pending_uploads` length unchanged).
- FastPath (pixels in `thumb_pixels`, texture absent) pushes to `pending_uploads`
  and inserts `thumb_uploading` exactly **once** across repeated
  `request_thumbnail` calls (second call is a dedup no-op).
- `upload_thumbnail` removes the id from `thumb_uploading` and the id is then
  covered by the `textures` guard.
- `cancel_pending_jobs` clears both `pending_uploads` and `thumb_uploading`.

**Diagnostics unit tests:** `classify_request` returns `DedupUploading` when the
uploading guard is the active one; `format_log`/`format_overlay` include the
`uploading` gauge.

**Manual verification (instrumented build, the original repro):** scroll fully
down then up, then close. Success criteria:
- `sub V` stays **O(images)** (a few thousand at most across the whole session),
  not tens of thousands.
- `backlog` (`pending_uploads`) stays **bounded** (drains, never monotonically
  climbs).
- `pix m/s` stays low; `tex h/s` stays healthy; now-visible cells load promptly.
- Close is prompt; the `[diag close]` line shows small in-flight/pending counts
  and `joined=true`.

## Gate

Per CLAUDE.md, before finishing: `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
green — then **hold for the author's hands-on instrumented re-test** (the storm
is an egui-runtime behaviour only confirmable by running the real app) before
merge.

## Build note (Windows)

If `cargo test` hits `LNK1104: cannot open ...ferrolite_app-<hash>.exe`, re-run
with an isolated `CARGO_TARGET_DIR` (e.g. `target-diag`) rather than killing the
process.

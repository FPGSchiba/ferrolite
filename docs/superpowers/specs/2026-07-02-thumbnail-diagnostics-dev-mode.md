# Spec: Thumbnail Diagnostics / Dev-Mode

- **Date:** 2026-07-02
- **Branch:** `feat/thumbnail-diagnostics` (off `fix/thumbnail-and-shutdown`)
- **Status:** design approved; ready for implementation plan

## Goal

Add a permanent, env-flag-gated **observability** dev-mode that instruments the
thumbnail + background-job pipeline so the remaining performance bottleneck
becomes unambiguous. This work is **observability only** — it must not attempt
to fix the bottleneck. After it lands, the instrumented build is run through the
repro (scroll the Library grid fully down then fully up; then close the app) and
the trace is read to locate the cause.

### Symptoms being chased (make observable — do not assume)

1. Scroll the Library grid fully down, then fully back up → the now-visible
   thumbnails do not load, or load extremely slowly.
2. Closing the app right after that scroll pattern hangs ("Not Responding" on
   Windows).

Both became **more** prominent after the round-4 "cancel off-screen fetches"
change. Author hypotheses to confirm or refute with the instrumentation:

- Cancelling off-screen jobs may not actually work (already-dispatched jobs keep
  running; `JobSystem::cancel` may not remove a queued job; or jobs get
  re-submitted and thrash).
- Switching to the Develop viewer may flood the shared job queue (the viewer's
  `VirtualTexture` submits tile jobs to the **same** `JobSystem` the grid uses).
- The true time/queue sink is genuinely unknown — we need eyes on every stage.

## Non-goals

- No bottleneck fix, no queue redesign, no cache-tuning in this branch.
- No changes to `ferrolite-vt` (the viewer-flood question is answered from the
  job-system counters — see below).
- Not extending or altering `FERROLITE_PROFILE_THUMBS` (the narrow per-file
  ingest decode/encode/upsert profiler stays as-is).

## Design decisions (settled in brainstorming)

1. **Output form:** both a throttled **log trace** (stderr + session file) and a
   toggleable **egui overlay**. The log is effectively required regardless: the
   shutdown-hang numbers can only be captured there because `on_exit` runs after
   the final frame, when the overlay is already gone.
2. **Env flag:** a new, mode-valued `FERROLITE_DIAG`, independent of
   `FERROLITE_PROFILE_THUMBS`.
3. **Cross-crate architecture:** `ferrolite-jobs` gets self-contained, gated
   atomic counters plus a `stats()` snapshot getter (no new dependency — the
   crate stays zero-dep and engine-transferable). `ferrolite-app` owns the
   aggregating `diag` module and is the only aggregator.

## Architecture

Two instrumented layers, one aggregator.

### `ferrolite-jobs` (stays zero-dependency)

- Add gated atomic counters incremented at the points only this crate can see:
  `submit`, worker dispatch, completion, panic, cancel-before-dispatch, and
  `Queue::cancel` (removed-vs-absent).
- Add `pub fn stats(&self) -> JobStats` returning a plain snapshot struct.
- A local `diag_enabled()` (`OnceLock<bool>` reading `FERROLITE_DIAG`) gates
  every increment. When off: one cached bool check, nothing else runs.
- `JobStats` fields (all `u64`/`usize`, per priority where noted):
  - `submitted[Interactive|Visible|Background]`
  - `dispatched` (total; per-priority optional if cheap)
  - `completed`
  - `cancelled_before_dispatch` (the `token.is_cancelled()` skip in `worker_loop`)
  - `panicked`
  - `active` (running now) — reuse the existing `active` gauge
  - `pending[Interactive|Visible|Background]` — live count per bucket, computed
    by iterating the jobs map **only when `stats()` is called** (once/sec)
  - `cancel_removed` / `cancel_absent` — from `Queue::cancel`: whether the id was
    actually present (dropped from queue) or already running/gone. **This is the
    direct answer to "does cancelling off-screen jobs work?"**

### `ferrolite-app::diag` (new module, sibling to `thumb_profile.rs`)

Owns all app-side counters and both outputs:

- Cache hit/miss/evict (Texture + pixel caches).
- `request_thumbnail` outcome classification.
- Per-frame gauges (uploads, events, repaint, frame time).
- Ingest counters (reused).
- Shutdown line.
- Reads `jobs.stats()` each tick and merges; renders the ~1/sec log and the
  overlay from one per-frame snapshot struct (single source of truth).

### `ferrolite-vt` — unchanged

`VirtualTexture::sparse` and the rung-1 preview submit `Priority::Visible` jobs
**through** `JobSystem::submit`, so the counters above already capture them. A
spike in `submitted[Visible]` when the viewer opens **is** the flood — observable
without touching VT.

## Flag semantics (parsed once, cached)

| `FERROLITE_DIAG` | Effect                        |
|------------------|-------------------------------|
| unset            | total zero overhead           |
| `1` or `both`    | log + overlay                 |
| `log`            | log only                      |
| `overlay`        | overlay only                  |

- `FERROLITE_DIAG_FILE=<path>` optionally overrides the session-file path.
- `FERROLITE_PROFILE_THUMBS` is untouched and orthogonal.

**Zero-overhead-off guarantee:** identical pattern to `thumb_profile` — a single
cached `enabled()` bool short-circuits every hook before any atomic is touched;
overlay code and log formatting are never reached; the frame-time `Instant` calls
are themselves inside the gate.

## What gets measured (counter catalog → code sites)

**Job system** (`JobStats`, `ferrolite-jobs/src/system.rs` + `queue.rs`): per
priority `submitted`, `dispatched`, `completed`, `cancelled_before_dispatch`,
`panicked`; live `active` and `pending` per bucket; `cancel_removed` vs
`cancel_absent`.

**Lazy-load** (`ferrolite-app/src/state.rs`): `request_thumbnail` calls/frame
classified into {new submit, pixel-cache fast-path, dedup-skip: textured /
pending / missing}; live sizes of `thumb_pending`, `thumb_missing`,
`thumb_handles`; `retain_visible_thumbnail_jobs` cancellations/frame + handles
held.

**Caches** (`ferrolite-app/src/library/texture_cache.rs`, `thumb_pixel_cache.rs`):
`TextureCache`(512) and `ThumbPixelCache`(1024) — size, hit/s, miss/s, evict/s.

**Per-frame** (`ferrolite-app/src/app.rs` `update`): `pending_uploads` depth,
uploads-applied vs `MAX_THUMB_UPLOADS_PER_FRAME`, backlog-drain frame estimate,
events drained/frame, whether repaint was forced, and `update()` wall-clock
(frame time, plus a max seen).

**Ingest** (`ferrolite-app/src/state.rs`): `active_ingests`,
`ingest_done`/`ingest_total`, `scanned`/`indexed` (reuse existing counters).

**Viewer:** derived from the `submitted[Visible]` delta on open (flood question);
plus app-side Develop→Library `textures.clear()` + subsequent re-upload count.

**Shutdown** (`ferrolite-app/src/app.rs` `on_exit`): `active`+`pending` per bucket
at entry, `join_with_timeout` result (joined vs detached@75 ms), and `on_exit`
wall-clock duration — emitted to the log/file (the overlay is gone by then).

## Output formats

### Log (throttled ~1/sec, same tick discipline as `thumb_profile::diag`)

One multi-line block per tick to **stderr and** a session file. The file is
essential on Windows and guarantees the shutdown line survives window teardown.
Default path `%LOCALAPPDATA%/ferrolite/diag-<pid>.log` (printed once at startup),
overridable with `FERROLITE_DIAG_FILE`. Writes are best-effort, buffered, and
flushed on the shutdown line; a failed write is silently dropped (like
`thumb_profile`) — never blocks the UI thread on a slow disk.

Block shape (instantaneous gauges + per-second rates):

```
[diag +1.0s] frame 6.2ms(max 11.0) ev/f 3 repaint forced
 jobs  sub I/V/B 0/812/0  disp 806  done 172  cxl(pre)640  panic 0
       active 6  pending I/V/B 0/634/0  cancel removed 638/absent 2
 thumb req/f 44 = new 2 + fast 1 + dedup 41 (tex 30/pend 11/miss 0)
       pending 640  handles 640  missing 0  retain cxl/f 44
 cache tex 512 h/s 40 m/s 2 ev/s 38 | pix 1024 h/s 41 m/s 3 ev/s 40
 uploads 16/16 cap  backlog 210 (drains in ~13f)
 ingest active 0  done 3320/3320
```

Shutdown line (flushed):

```
[diag close] active 6 pending 640  joined=false(detach@75ms)  on_exit 78ms
```

### Overlay

An `egui::Window` (top-right, semi-transparent, non-interactive, `Order::Tooltip`)
painted at the end of `update()` only when enabled + visible. Shows the same
fields as live gauges. **Toggle: F9** (so `=overlay`/`=both` can still be
hidden/shown at will). Reads the same per-frame snapshot the log formats — one
source of truth, computed once per frame only when diag is on.

### Rate computation

A `DiagState` holds `last_tick_instant` + previous cumulative values; rates =
delta/dt. The overlay shows the **last completed 1-second rate** (not a
per-frame delta) so numbers stay stable and don't jitter.

## Testing

**Unit tests — `ferrolite-jobs`:**
- `JobStats` counts submit/dispatch/complete correctly.
- A panicking job increments `panicked` and the pool still runs the next job.
- `cancel` of a queued id increments `cancel_removed`; `cancel` of an
  unknown/running id increments `cancel_absent`.
- **All counters stay zero when `FERROLITE_DIAG` is unset** (zero-overhead
  invariant, explicitly asserted).

**Unit tests — `ferrolite-app::diag`:**
- Rate math (delta/dt) is correct across ticks.
- Mode parser maps `1`/`both`/`log`/`overlay`/unset correctly.
- Cache counter increments; `request_thumbnail` classification buckets.

**Non-perturbation guarantees:**
- All counters `Ordering::Relaxed`.
- Snapshot/format/overlay work happens only on the 1/sec tick or gated frame end.
- No locks added to the worker hot path beyond the existing queue mutex.
- Frame-time `Instant` calls gated behind `enabled()`.
- The instrumentation must not itself force repaints (it *reports* `repaint
  forced`, never causes it).

**Gate green** per CLAUDE.md before finishing: `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
— then **hold for the author's hands-on visual test** before merging/finishing.

## Build note (Windows)

A stray test binary sometimes locks the default target dir; if `cargo test` hits
`LNK1104: cannot open ...ferrolite_app-<hash>.exe`, re-run with an isolated
`CARGO_TARGET_DIR` rather than killing the process.

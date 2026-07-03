# Session prompt — fix the thumbnail GENERATION bottleneck (slow RAW-decode tail)

Copy everything below the line into a fresh Claude Code session to start the fix work.

---

Reduce ferrolite's thumbnail **generation** (ingest) time. On the author's ~3320-image RAW
library a full ingest takes **~251 s (~4 min)**. The `FERROLITE_DIAG` instrumentation (already
built and merged) has already **pinned the root cause** — this session is about confirming the
"why" at the file level and then implementing the fix. Do NOT re-do the observability work; it
exists and works.

FIRST: `git checkout main`, `git pull`, then create a NEW branch off main (e.g.
`fix/thumbnail-decode-tail`) — confirm you're on it with a clean tree.

Use superpowers:brainstorming to design the fix with me, then superpowers:writing-plans, then
superpowers:subagent-driven-development to implement it task-by-task. Honor CLAUDE.md (never block
the UI/update thread; any added instrumentation stays zero-overhead when its flag is off; wait for
my hands-on instrumented test before finishing the branch).

== CONFIRMED DIAGNOSIS (from the FERROLITE_DIAG `[ingest-summary]`, do not re-derive) ==
Full ingest of 3320 files in 251.1 s. Phase breakdown:
```
phases  scan 0.0s  phaseA 0.1s  filter 0.0s  decode(par) 250.9s
decode  Σ 2438s / 10 cores → 9.7x | p50 123ms p95 5305ms max 6540ms
encode  Σ 12.1s  avg 4ms
upsert  64 batches  avg 11ms (Σ 0.7s)
channel max depth 1 | producer done@251.1s consumer done@251.1s (tail 0.0s)
by kind  RAW 3307 (decode p50 123ms) | std 13
```
What this rules OUT (with evidence — do not chase these):
  - Serial phases (scan/phaseA/filter): 0.1 s total. Not the problem.
  - Rayon parallelism: **9.7× of 10 cores** — near-perfect. Do NOT try to "parallelize more."
  - Thumbnail encode (resize/JPEG): Σ 12.1 s, avg 4 ms. Negligible.
  - **SQLite blob writes (upsert): Σ 0.7 s.** Switching off SQLite blobs would NOT speed up
    generation — it is 0.3% of the time. (It may matter for scroll/read or DB size, but that is a
    different problem.)
  - Consumer/DB tail: channel depth 1, tail 0.0 s. The consumer keeps up perfectly.

The bottleneck is **`decode_meta_and_preview` CPU on a heavy right-skewed tail of RAW files.**
Median RAW decodes in ~123 ms, but p95 is **5.3 s** and max **6.5 s**; the mean (734 ms) is ~6× the
median. Decode is already CPU-bound and maxes all cores, so the only lever is **reducing decode CPU
on the slow tail** — not more threads, not the DB.

== LEADING HYPOTHESIS (confirm with instrumentation before fixing) ==
The slow-tail files likely have **no usable embedded preview**, so `rawler` (in `ferrolite-decode`)
falls back to a **full RAW demosaic** (seconds) instead of the fast embedded-JPEG path (~123 ms).
Different camera model / RAW variant / compression. CONFIRM this before assuming it: extend the
diagnostic to log the slow files (path, kind, decode_ms) for any file over a threshold (~500 ms),
run one instrumented ingest, and look for the pattern (same camera? same extension? preview
missing?). Only then design the fix.

== POSSIBLE FIX DIRECTIONS (brainstorm; do not pre-commit) ==
  - Detect "no embedded preview" cheaply and avoid/cap the full demosaic (e.g. generate the
    thumbnail from a fast half/quarter-res demosaic instead of full-res).
  - Downscale during demosaic (decode at reduced resolution) for the thumbnail path.
  - Defer the slow-tail thumbnails (index the row now, generate the thumbnail lazily/low-priority)
    so the grid fills fast and the slow ones trickle in.
  - A faster preview extraction path / a different rawler API.
  - Accept it as CPU-bound and just make progress visible (weakest option — the author wants it
    faster, not just prettier).

== INSTRUMENTATION AVAILABLE (use it; extend minimally) ==
`FERROLITE_DIAG=1` (log + F9 overlay) is merged. It emits a per-ingest `[ingest-summary]`
(phase wall-clock, decode Σ/parallel-speedup, p50/p95/max, per-kind, producer/consumer lag,
channel depth) plus a live `ingest:` line. All ingest/thumbnail timing lives in
`ferrolite-app/src/diag.rs` (`IngestProfile`/`IngestSummary`); `thumb_profile.rs` was removed and
folded in. To confirm the hypothesis, add per-file slow-decode logging to `IngestProfile` /
`ingest_job` (gated by `diag::enabled()`, zero-overhead-off). Also fix a cosmetic bug: the summary
uses `Σ`/`→` which render as mojibake (`Î£`/`â`) on Windows — switch those to ASCII (`sum`/`->`).

== KEY FILES ==
  - `ferrolite-app/src/ingest.rs` — `ingest_job` producer (`to_process.par_iter().for_each_with`),
    the `decode_meta_and_preview` call (~line 458 region) whose per-file time is the bottleneck;
    `IngestProfile` is already threaded here.
  - `ferrolite-decode` — `decode_meta_and_preview`, the embedded-preview extraction, and the
    full-demosaic fallback (the actual slow path). This is where a decode-side fix likely lands.
  - `ferrolite-app/src/diag.rs` — `IngestProfile`/`IngestSummary`/`format_ingest_summary`; extend
    here for slow-file logging + the ASCII fix.
  - `ferrolite-catalog` — `generate_thumbnail` (resize/encode; already fast, ~4 ms).

== BACKGROUND (the arc that led here) ==
Over several rounds we fixed a shutdown hang, a lazy-load re-spawn storm, a runaway counter, added
a two-tier thumbnail cache, single-pass RAW decode, batched DB writes, cancellation of off-screen
fetches, a `pending_uploads` re-submit-storm fix, and then built the `FERROLITE_DIAG` dev-mode +
the generation `[ingest-summary]` that produced the diagnosis above. Read these committed docs for
full context:
  - docs/superpowers/investigations/2026-07-02-thumbnail-perf-and-followups.md
  - docs/superpowers/specs/2026-07-02-thumbnail-diagnostics-dev-mode.md
  - docs/superpowers/specs/2026-07-03-thumbnail-generation-diagnostics-design.md
  - docs/superpowers/plans/2026-07-03-thumbnail-generation-diagnostics.md

== PROCESS / DELIVERABLES ==
  1. superpowers:brainstorming → agree the approach (confirm the hypothesis first via slow-file
     logging; then the fix). Write the spec under docs/superpowers/specs/ and get my review.
  2. superpowers:writing-plans → task-by-task plan under docs/superpowers/plans/.
  3. superpowers:subagent-driven-development → implement. Gate green (cargo fmt --check + cargo
     clippy --workspace --all-targets -D warnings + cargo test --workspace), then HOLD for my
     hands-on instrumented re-test (FERROLITE_DIAG=1, compare the new `[ingest-summary]` decode
     numbers against the 251 s / p95 5.3 s baseline) before finishing/merging.

BUILD NOTE (Windows): a stray test binary sometimes locks the default target dir; if cargo test
hits "LNK1104: cannot open ...ferrolite_app-<hash>.exe", re-run with an isolated CARGO_TARGET_DIR
instead of killing the process. Git attribution is disabled globally — no Co-Authored-By trailers.

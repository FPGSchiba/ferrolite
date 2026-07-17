# ferrolite — repo conventions for Claude

## Repository map

FerroLite is a Rust workspace: a photo catalog + RAW "develop" editor. `ferrolite-app` is the
egui/eframe + wgpu desktop binary; every other crate is a photo-agnostic engine piece it wires
together (several are marked "engine-transferable" so they can be reused outside the app).

| Crate | Responsibility |
|-------|----------------|
| `ferrolite-app` | The desktop binary (egui/eframe + wgpu): all UI, app state, wiring. ~31k LOC — see module layout below. |
| `ferrolite-image` | Core pixel/orientation vocabulary shared across all crates. |
| `ferrolite-decode` | Unified decode entry: routes RAW (rawler) vs. standard-image preview + metadata requests. |
| `ferrolite-catalog` | SQLite DAM catalog — schema, ingest, thumbnails, queries. `Catalog` (writer) + `ReadPool` (read conns). |
| `ferrolite-jobs` | Photo-agnostic priority threadpool (`JobSystem`) with cancellation. ALL off-UI-thread work goes here. |
| `ferrolite-pipeline` | The photo edit DAG — ordered `OpStack` document model; the edit engine. |
| `ferrolite-mask` | Engine-transferable mask machinery (brush / gradient / range / composite). |
| `ferrolite-color` | Pure, `unsafe`-free color math (moxcms-backed). |
| `ferrolite-lens` | Lens-correction adapter over the pure-Rust `lensfun` crate. |
| `ferrolite-export` | Photo-tier encode core: renders full-res edited output; encodes jpeg/png/tiff/webp/avif/jxl. |
| `ferrolite-gpu` | wgpu context + a generic retained-DAG GPU scaffold. |
| `ferrolite-vt` | Source-agnostic sparse virtual texture; `DisplayPipelines` (cached render pipelines). |
| `ferrolite-previews` | On-disk cache of downscaled, color-managed RAW previews. |

**`ferrolite-app/src/` layout:** `main.rs` (entry), `state.rs` (`AppState` — the central model),
`app.rs` (eframe `App` impl + frame loop), `events.rs` (job→UI event channel), `library/`
(grid/filmstrip/catalog browse), `develop/` (RAW develop module: crop, curves, masks, HSL, info
overlay), `viewer/`, `chrome/` (custom window chrome + generated app icon), `widgets/` (shared
widgets incl. the per-control reset affordance), `export/` + `export_module/`, `ingest.rs`,
`settings.rs`, `theme.rs` (fonts/phosphor install), `icons.rs` (Phosphor aliases),
`monitor_profile.rs`. The load-bearing conventions below (threading, icons, per-control reset,
keybind discoverability) constrain work in these modules.

## Responsiveness & threading (load-bearing)

1. **Never block the UI/update thread.** RAW/image decode, file & DB I/O, ingest
   directory walks, thumbnail generation, and any multi-millisecond CPU work MUST
   be submitted to `ferrolite-jobs` (with a priority + cancellation token) and
   delivered back over the app event channel, after which the job calls
   `ctx.request_repaint()`. UI-thread list/grid/filmstrip rendering MUST be
   virtualized (realize + decode only the items currently on screen) so it never
   does O(all-items) work per frame.

2. **GPU work stays on the render thread but must be bounded.** Build
   pipelines/shaders ONCE and reuse them (never rebuild per image/open/interaction);
   pre-warm expensive pipelines at startup; stream/upload incrementally (the sparse
   virtual texture) rather than in one synchronous build. Profile anything that
   could exceed a frame budget on open or navigation.

These two rules exist because both were violated and caused multi-second UI
freezes on image open — eager per-frame thumbnail decode in the Develop filmstrip
(fixed by virtualizing it), and a render-pipeline rebuild on every open (fixed by
caching pipelines in `ferrolite_vt::DisplayPipelines` and pre-warming at startup).
Keep them honored.

## Toolchain — run the gate on the latest stable rustc (load-bearing)

CI (`.github/workflows/ci.yml`) uses `dtolnay/rust-toolchain@stable`, which resolves to
the **newest stable rustc at run time**. Newer stable releases promote future-compat lints
to hard errors (e.g. the `f32: From<f64>` float-literal fallback that once reddened `main`
across ~44 egui call sites). A local build on an older stable will pass while CI still
fails on the same code. Therefore, **before running the repo gate (see "Gate tiers"), run
`rustup update stable`** so your local toolchain matches the runner. The repo gate is only
meaningful when run on the same (latest) stable CI uses. Keep the fix forward-compatible
(fix the code — e.g. suffix literals `_f32`); do NOT pin the toolchain to dodge a newer
lint. (The fast per-task **scoped gate** is a quick local check and need not chase the
latest stable — the end-of-branch repo gate and CI cover toolchain-specific lints.)

## Gate tiers — repo gate vs. scoped gate (load-bearing)

**First, work out which actor you are** (agents often don't realize they're agents):

- **You are a dispatched SDD subagent** if your task/prompt hands you a brief file under
  `.superpowers/sdd/` (e.g. `.superpowers/sdd/task-3-brief.md`), or you were dispatched to
  implement / review / fix ONE task of a plan. This is true even when nothing explicitly calls
  you an "agent." → Run the **scoped gate** for your task's crate(s) only. Do NOT run the repo
  gate unless your brief explicitly tells you to.
- **You are the coordinator / main session** if you are talking directly with the author and no
  sdd brief was handed to you. → You own the **repo gate**: run it once before finishing a
  branch, and dispatch subagents with instructions to run their scoped gate.

Two named gates, run by different actors. Do not conflate them.

**Repo gate (full, authoritative).** The whole-workspace checks, run on the latest stable
(see the Toolchain rule — `rustup update stable` first):

    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo build --all-targets
    cargo test --workspace

This is what CI enforces and the only gate that proves the whole tree is green. It MUST be
run: by the coordinator ONCE before finishing a branch (at the whole-branch review stage),
and by any fresh session that needs to establish the tree is clean.

**Scoped gate (per-task, fast).** For a change confined to crate(s) `X`, run the same four
checks limited to `X` plus any crate that consumes the code you changed:

    cargo fmt -p X -- --check
    cargo clippy -p X --all-targets -- -D warnings
    cargo test -p X            # add `-p <dependent>` for each crate that uses X's changed API

It skips the expensive whole-workspace clippy/build/test (wgpu, rav1e, golden-image suites)
that dominate the repo gate's wall-clock.

**Who runs which.** A subagent dispatched for a scoped SDD task (implementer, reviewer, or
fix) runs the SCOPED gate for its crate(s) — NOT the repo gate — unless its dispatch prompt
explicitly says "run the repo gate." Such dispatch prompts SHOULD name the crate(s) and say
"scoped gate on `X` only." The coordinator then runs the repo gate ONCE at the end
(whole-branch), which is the safety net for the cross-crate breakage a scoped gate can miss;
CI is the final authority. Rationale: running the full repo gate inside every per-task
subagent is redundant and slow — the end-of-branch repo gate plus CI already cover the whole
tree, so paying for it 5–10× per branch buys nothing.

## Finishing a branch — wait for the author's visual test (load-bearing)

Automated checks (`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`) being green is **necessary but not sufficient** to
finish a development branch. Much of this app is egui UI whose correctness can only be
confirmed by running the real app and looking at it. Therefore: after the repo gate
is green, **STOP and wait for the author (Jann) to visually test the running app and give
explicit feedback** before merging, pushing/PR-ing, or otherwise finishing the branch.
Do not present finish options as the final step — present them, then hold for the
author's hands-on test results, and address any issues found before completing.

**Always hand the author a concrete visual test plan (or an explicit "nothing to test").**
When the repo gate goes green, do not just say "please visually test" — produce a
short, specific checklist so the author knows exactly whether hands-on testing is needed
and what to look at. The plan MUST state one of:

- **Nothing to visually test** — and *why* (e.g. "engine-only crate, not wired into the
  running app; no UI or behavior reachable from FerroLite changed"). Point at any offline
  artifacts worth an optional glance instead (e.g. committed golden PNGs), and name the
  later phase where the real hands-on test lands.
- **A numbered, reproducible checklist** — for each item: the exact steps to reach the
  change in the running app (which module, panel, control, or gesture), the precise thing
  to look for (expected appearance/behavior), and the failure signature that means it is
  wrong. Cover the happy path plus the edges the automated tests can't judge (visual
  correctness, interaction feel, responsiveness/no-freeze on open/navigation, per-control
  reset). If a step needs a specific fixture (a particular RAW file, an edited image, a
  non-RGGB/rung-1 file), say which.

The point is that the author never has to reverse-engineer what changed to know if — and
how — to test it. A branch is not finish-ready until this test plan (or the justified
"nothing to test") has been handed over.

## Per-component reset (design, load-bearing)

Every adjustable component in the editing UI MUST expose its own individual
reset-to-default affordance — each slider, the tone curve, the crop/geometry,
HSL, and any future editable control — not only a section-level or global
"Reset all". A user must be able to revert any single control on its own
without touching its neighbors and without hunting for the original value.
Reuse the shared reset affordance (`ferrolite-app/src/widgets` `draw_reset_arrow`
+ the `EguiSlider` reset column) so it stays visually consistent. A new editable
control is not complete until it has a per-control reset.

## UI icons (load-bearing)

EVERY icon in the app comes from the `icons` module (`ferrolite-app/src/icons.rs`), which
aliases the bundled icon font (`egui-phosphor`, installed once in `theme::install_fonts`)
and is rendered in the icon font family (via `widgets::tool_button` or a `FontId` from
`icons::font`). This includes tool/sub-tool icons, undo/redo, the rating **stars**,
**flags**, **chevrons**, and the per-control **reset** glyph. NEVER put raw emoji/symbol
characters in IBM Plex text and do NOT hand-draw new icons with `Painter` shapes — Plex +
egui's bundled emoji subset don't cover symbols (they render as tofu), and ad-hoc vector
icons fragment the system. Add a new icon by adding a semantic alias in `icons.rs` sourced
from the Phosphor catalog. The per-control reset affordance and its placement remain
load-bearing (see "Per-component reset"); only its glyph comes from the library.

## UI keybind tooltips (load-bearing)

Any control bound to a keybind MUST display that key in its hover tooltip, sourced from
the live keymap (`Keymap::hint(action)`), so rebinding updates the shown key. Format the
label as `"<Label> (<Key>)"` (e.g. "Crop (C)", "Undo (Ctrl+Z)"). Non-rebindable input
gestures are documented in Help/Settings instead (see "Keybind discoverability").

## Keybind discoverability (load-bearing)

Every keybind or input gesture MUST be represented so the user can discover it: a
rebindable `Action` appears in BOTH the Settings keyboard tab (add it to a `GROUPS`
entry — enforced by `every_action_is_in_a_settings_group`) AND the Help panel's shortcut
list. A non-rebindable input gesture (e.g. Ctrl+scroll = brush size) appears at least in
the Help panel and is noted in the Settings keyboard tab's gestures line.

## Subagent-driven-development scratch (`.superpowers/sdd/`)

`.superpowers/` is git-ignored scratch. When `superpowers:subagent-driven-development`
runs, it fills `.superpowers/sdd/` with per-task briefs, reports, review-package diffs,
and a `progress.md` ledger — these accumulate fast (hundreds of files across a
multi-round effort). **When `superpowers:finishing-a-development-branch` completes by
merging or discarding a branch, clean that scratch** (`rm -rf .superpowers/sdd/*`, which
keeps the folder's own `.gitignore`) so the next task starts without stale briefs/reports
to wade through. Do NOT clean it for the "keep as-is" or "create PR" finish options (that
work is still in flight). Nothing there is tracked, so no commit is involved.

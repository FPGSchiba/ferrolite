# Session state anchor — feat/ui-v2-rewrite (written 2026-07-29, pre-compaction)

Read this first when resuming work on this branch with reduced context.

## Where things stand

- **Branch:** `feat/ui-v2-rewrite`, ~152 commits ahead of its fork point (`916039b`), repo gate
  green on latest stable (fmt, clippy `-D warnings`, build, 55 workspace test binaries) as of
  `92b07fd`. Working tree clean except the author's in-progress walkthrough annotations.
- **The unified-maskable-adjustments spec is COMPLETE and author-approved** — all four phases:
  - Phase 1: layer-shaped `EditDoc` (STACK_VERSION 2) behind the `OpStack` adapter API.
  - Phase 2a: adjustment registry + `EditScope`/`ScopedEdit`; mask mode = management header atop
    the shared Light/Color/Effects tabs; duplicate mask slider set deleted.
  - Phase 2b: per-mask tone curve / HSL / color grading (extended local-adjust pass, per-layer LUTs).
  - Phase 3: fused two-segment layer engine (Light engine before dehaze, Color engine at the
    local-adjust position); global H/S/W/B, Sat/Hue, swatch, Vibrance live; goldens re-pinned to
    the fused engine; perf gate re-based (author decisions recorded in
    `docs/benchmarks/2026-07-28-phase3-fused-engine.md`).
  - Phase 4: separable sharpen, dehaze recovery fused into the Color engine, per-mask dehaze
    amount + per-mask sharpen; cascade semantics author-accepted (spec §4.1).
- Spec: `docs/superpowers/specs/2026-07-28-unified-maskable-adjustments-design.md` (amended §4.1).
- Plans executed (all complete, SDD workspaces deleted, git history is the record):
  `2026-07-28-unified-doc-phase1-editdoc`, `2026-07-28-unified-ui-phase2a-scoped-tabs`,
  `2026-07-28-unified-ui-phase2b-mask-curve-hsl-grade`, `2026-07-28-unified-engine-phase3-fused-layers`,
  `2026-07-29-unified-engine-phase4-mask-neighborhood` (all under `docs/superpowers/plans/`).

## Current activity: workstream 2 — the UX feedback pass

- Walkthrough handed to the author: `docs/superpowers/2026-07-29-v2-ux-walkthrough.md`.
  **The author is annotating it now** (verdicts inline under each item).
- Agreed flow: author annotations → triage into a `systematic-ui-fixes-round-4` spec (author
  approves) → execute SDD-style → fold accepted UX changes into `docs/design/V2/README.md`
  (the living design frame; `.dc.html` mockups are never regenerated) → then
  superpowers:finishing-a-development-branch for the whole branch.
- **Early signals from the partial annotations already visible** (verify against the final file
  before triage — and note items that read as BUGS, not UX, get systematic-debugging treatment
  first, likely as their own high-priority spec items):
  - **2.3 (likely REGRESSION, highest priority): "no longer any edits on JPG files" and "no live
    edits — effect applies only on slider release" on all sliders in every tab (tone curves still
    live).** Suspect area: the ScopedEdit/EditOutcome commit plumbing or `edit_in_progress`
    preview deferral from the main-merge; needs root-cause work, not UX polish.
  - 2.6: panning/zooming "flashes the edited image in and out with the raw" (likely tier/present
    regression — also bug-class); plus a ~15px right-gutter/scrollbar offset complaint.
  - 1.4: Metadata popup closes when any dropdown is pressed (bug-class); useless icon right of
    the Metadata button; wants: reset-all-filters button, rethink Subfolders placement,
    range sliders for ISO/aperture/focal (defined ranges), resettable + multi-select file-type filter.
  - 1.3: wants drag-and-drop to establish/abolish collection parent-child relationships.
  - 2.1 + 2.2 (converging verdict): the H/S/W/B duplication is NOT clear — give the region-based
    H/S/W/B its own titled section (like Tone Curve) with a small subheader explaining the
    difference vs the parametric ones.
  - Mask scope (3.x), canvas (4.1), Library 1.1/1.2/1.5: happy. "V3" mentioned as a future
    horizon for status/feedback work.
  - Items 4.2 onward were truncated in the last visible read — RE-READ THE FULL FILE at triage.

## Load-bearing conventions (beyond CLAUDE.md)

- SDD execution pattern used all session: per-task briefs via the superpowers scripts, fresh
  implementer subagent per task (sonnet default; haiku for transcription; fable for final
  reviews/investigations), task review + scoped re-review per fix round, ledger in the plan's
  `.superpowers/sdd/<plan>/progress.md`, coordinator runs the repo gate.
- Golden/parity adjudication precedent: inherent-precision drifts (f16-removal, summation order,
  dehaze divide-by-t amplification) may regenerate goldens ONLY with proven mechanism +
  documentation; the author has approved this class three times.
- Known environment quirks: the dev laptop (Iris Xe) shows 15-55% ambient/thermal bench drift —
  interleaved A/B or cool-machine runs required for any perf claim. The `base_tabs` settings
  tests are hermetic now (reset `Settings::default()` after `AppState::new()`); keep new tests
  hermetic the same way.
- NR + clarity/texture remain unimplemented in ALL scopes (greyed; future effort). Heal is a
  disabled placeholder tool.

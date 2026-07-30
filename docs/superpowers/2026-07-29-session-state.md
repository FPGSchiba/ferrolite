# Session state anchor — feat/ui-v2-rewrite (updated 2026-07-30)

Read this first when resuming work on this branch with reduced context.

## Where things stand

- **Branch:** `feat/ui-v2-rewrite`. Repo gate green on latest stable (1.97.1).
- **DONE + author-accepted:**
  - Unified-maskable-adjustments spec, all 4 phases (see git history; spec in
    docs/superpowers/specs/2026-07-28-unified-maskable-adjustments-design.md).
  - Five walkthrough regressions fixed (cf8a440, 44af74d, 2531dfb, 32f3fc8): present-gate
    `full_synced_version` (live slider edits + pan/zoom flashing), warm-reveal source
    restoration (JPG edits), metadata popup single-slot fix, info panel resize.
  - Round-4 UX fixes: 14-task plan (docs/superpowers/plans/2026-07-29-systematic-ui-fixes-
    round-4.md) + final-review fix (c55f499) + two author-feedback rounds (255cc9b..fb85861,
    876f419/0da8d70/e437618). Highlights: schema v7 + EXIF backfill, real range filters w/
    manual entry, multi-select file types, reset-all, arbitrary-depth collection nesting,
    filmstrip free-scroll, titlebar underline (root cause: tab row overflowed the 30px panel
    clip — regression test in chrome/mod.rs), info overlay content-sized.
  - V2 README fold-in: cd2b14c.
- **IN FLIGHT: crop overhaul.** Spec: docs/superpowers/specs/2026-07-29-crop-overhaul-
  design.md (author-approved incl. manual-keystone-only scope). Plan:
  docs/superpowers/plans/2026-07-30-crop-overhaul.md (8 tasks). SDD workspace + ledger:
  .superpowers/sdd/2026-07-30-crop-overhaul/progress.md — RESUME FROM THE LEDGER; tasks with
  a "complete" line are done. Executing via subagent-driven-development (fresh implementer
  per task, task review, fix rounds, final fable review, repo gate, author visual test).

## Load-bearing session conventions

- SDD pattern: briefs via skill scripts, sonnet implementers (haiku for mechanical), review
  per task, resume-implementer fix rounds, ledger updated per task, coordinator commits docs.
- Golden adjudication: regenerate only with proven mechanism + documentation.
- Two failed blind fixes ⇒ evidence-first (shape dumps / headless repros) before attempt 3.
- disclosure_snapshot in app.rs must list every Settings `*_open` flag (count-asserting test).
- Author visual test required before finishing the branch; crop keystone strength constant
  K=0.35 is the single tuning knob.

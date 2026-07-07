# ferrolite — repo conventions for Claude

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

## Finishing a branch — wait for the author's visual test (load-bearing)

Automated checks (`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`) being green is **necessary but not sufficient** to
finish a development branch. Much of this app is egui UI whose correctness can only be
confirmed by running the real app and looking at it. Therefore: after the workspace gate
is green, **STOP and wait for the author (Jann) to visually test the running app and give
explicit feedback** before merging, pushing/PR-ing, or otherwise finishing the branch.
Do not present finish options as the final step — present them, then hold for the
author's hands-on test results, and address any issues found before completing.

**Always hand the author a concrete visual test plan (or an explicit "nothing to test").**
When the workspace gate goes green, do not just say "please visually test" — produce a
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

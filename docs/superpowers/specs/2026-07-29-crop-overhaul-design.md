# Crop tool overhaul (design)

**Date:** 2026-07-29 · **Branch:** `feat/ui-v2-rewrite` · **Source:** walkthrough item 4.2
("The Crop tool needs a major overhaul… a single major pass") + V2 design frame
(`docs/design/V2/README.md:58,69`) + author scope decision (manual keystone only).

## Problems being solved

1. **Aspect not kept while dragging** (bug, root-caused): `crop_math::resize` enforces the
   ratio, but two independent per-axis boundary clamps run AFTER the aspect enforcement
   (`crop_math.rs:106-114`) and silently break the ratio near image edges. Tests only cover
   aspect-hold away from edges.
2. **Edge-extrusion artifact on apply** (bug, root-caused): `geometry_uniform` rounds the
   output dims but derives the sampling matrix from the un-rounded crop rect
   (`uniforms.rs:389-403`), and the geometry sampler clamps to the whole source texture, not
   the crop bounds (`nodes.rs:457-464`) — fractional or rotated crops sample past the edge
   and smear the last texel outward.
3. **Cramped, wrong panel structure** (UX): crop options render as a 4th tab next to
   Light/Color/Effects. The V2 design (README:69) says: *"tabs disappear entirely, replaced
   by a dedicated panel."*
4. **Missing V2 controls**: no aspect chips (only a ComboBox), no GEOMETRY section (keystone
   does not exist in the pipeline at all; `rotate_angle` is dead code).
5. **"No edits while cropping / after exiting crop"**: no code cause found in the crop
   branch; almost certainly the present-gate regression already fixed (`cf8a440`).
   **Verified by the author's hands-on test before this spec executes; if it still
   reproduces, it becomes a systematic-debugging task in this pass.**

## Design

### C1 — Aspect-correct resize math (bug fix)

Rework `crop_math::resize` so every constraint pass preserves the ratio: after the free
resize + aspect derivation, compute the **maximum feasible aspect-true rect** that fits the
`[0,1]²` bounds and the minimum-size floor, anchored at the drag's fixed corner/edge — i.e.
clamp the SCALE of the aspect-true rect, never one axis alone. Pure function, exhaustively
tested at boundaries: dragging every handle into every edge/corner with several ratios
asserts `|w/h − ar·(sh/sw)| < 1e-4` at all times, plus the existing no-panic adversarial
sweep extended with ratio assertions.

### C2 — Geometry sampling correctness (bug fix)

Two changes in `ferrolite-pipeline`:
- Derive the sampling matrix/offset from the **rounded** output dims so output texel centers
  map exactly into the true crop extent (no over-run from the rounding remainder).
- Clamp sampling to the crop sub-rect: the shader clamps `base_uv` to the crop rectangle
  (inset by half a source texel) instead of relying on whole-texture ClampToEdge, so a
  rotated crop's out-of-bounds corners clamp to the crop's own edge, never smear the frame
  edge.

Tests: `geometry_uniform` unit tests for fractional crops (non-exact pixel extents) ×
rotation × aspect combined; plus one GPU golden/edge-assertion test rendering a rotated
fractional crop and asserting the output's last row/column are NOT duplicates of their
neighbors (the artifact's signature).

### C3 — Dedicated crop panel (tabs disappear)

In `tool_panel.rs` (~lines 84-97): when the Crop tool is active, do NOT build
`base_tabs()` — render a dedicated panel instead of the shared tab row (a sibling branch to
the Mask header injection, but replacing the tabs rather than augmenting them). Contents per
V2 README:69:

- **CROP & TRANSFORM** section: Angle slider (existing, with per-control reset) · Aspect
  ComboBox ("Original", existing presets) · a wrapping row of compact aspect chips —
  Original / 1:1 / 4:3 / 3:2 / 16:9 / 5:4 / Custom, selected chip accent-tinted, chips and
  combo stay in sync (both write the same aspect state) · "Reset crop" button (existing).
- **GEOMETRY** section: Keystone V and Keystone H sliders (C4), each with per-control reset ·
  "Auto Perspective" and "Guided Upright" buttons rendered **disabled** with hover reason
  "Coming with automatic perspective analysis" (the app's greyed-with-reason convention) ·
  section reset footer consistent with other sections.
- Both sections use `section_header` + their own disclosure-memory settings flags, like every
  Develop section.

### C4 — Manual keystone (new pipeline capability)

- `Geometry` op gains `keystone_v: f32` and `keystone_h: f32` (range −1..1, default 0,
  `#[serde(default)]` — old sidecars load unchanged; identity means no warp).
- The geometry node applies a projective (homography) warp built from keystone V/H composed
  with the existing rotation+crop mapping — one matrix path, still a single
  `textureSampleLevel` per output texel. CPU reference in `uniforms.rs` mirrors the WGSL
  (the crate's established lockstep pattern) with a parity test.
- `needs_full_rebuild` / dirty-routing treats keystone like angle (geometry-tier change).
- Export path (`ferrolite-export`) renders through the same geometry uniform — verify with
  one export test.
- History/undo: keystone edits are `OpKind::Geometry` commits like angle.

### C5 — Leaving crop feels safe

Exiting the crop tool (toolbar or keybind) commits nothing by itself: the committed state is
whatever the last handle-release/slider-release committed (already the semantics — verify
with a test on the tool-switch path). Escape while dragging a handle cancels that drag
without committing (new; matches slider Escape behavior if present, else document).

## Non-goals

- Auto Perspective / Guided Upright implementations (buttons ship disabled).
- Lens-profile-driven geometry (exists separately via lens correction).
- Any change to mask/adjustment behavior while crop is active beyond what C3 restructures.

## Conventions binding

Same as Round-4 spec: icons via `icons.rs`; keybind hints from `Keymap::hint`; per-control
resets; disclosure flags; hermetic tests; scoped gates per task (`ferrolite-pipeline`,
`ferrolite-app`, `ferrolite-export` as touched); repo gate + author visual test at the end.
Golden/parity adjudication rules from the session precedent apply to any pipeline goldens.

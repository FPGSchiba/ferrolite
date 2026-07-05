# ferrolite — v2 Architecture-Map Kickoff Prompt

> **Status:** Kickoff prompt (handoff artifact). **Authored as the Spec 4.6 finale** — the
> last deliverable of ferrolite **v1**. **Not a v2 design.**
> **Date:** 2026-07-05
> **Parent lineage:** v1 architecture map (`2026-06-28-ferrolite-v1-architecture-map.md`) →
> Spec 4 map (`2026-07-03-spec4-secondary-and-polish-map.md` §4.6) →
> Spec 4.6 design (`2026-07-05-spec4.6-camera-coverage-v2-kickoff-design.md` §5) → **this prompt**.
> **You are:** the future spec agent picking up **v2**. Read this whole file, then act.

---

## 0. What this prompt is (read first)

This is a **spec-creation prompt**, written in the same spirit as the prompt that kicked off
Spec 4: it tells you what to read, what to decide, and to run the full **superpowers cycle**.
It is **not** a design and it does **not** decide v2's architecture. Its job is to hand you a
clear starting brief and a proposed inventory so you can **brainstorm** the real answers.

**The deliverable you produce first** is a **v2 architecture MAP** — a *decomposition parent
document*, the direct successor to the v1 architecture map, structured identically (see §2).
It is a map, not an implementation spec: it carves v2 into phases, fixes the settled decisions
and the cross-cutting seams, and hands each phase off to its own cycle. It does **not** design
any phase.

**Then**, exactly as Spec 4 was kicked off from the v1 map, each phase your map defines becomes
its **own `spec → plan → implementation` cycle on its own branch off `main`**, running the
superpowers workflow end to end:

1. **`brainstorming`** — resolve that phase's open questions with the author; produce its design doc
   at `docs/superpowers/specs/YYYY-MM-DD-<phase>-design.md`.
2. **`writing-plans`** — turn the approved design into a step-by-step implementation plan.
3. **`implementation`** — build it (TDD where it applies), gate it green, then **hold for the
   author's hands-on visual test** before finishing (§4).

Write the v2 map to `docs/superpowers/specs/YYYY-MM-DD-v2-architecture-map.md` and get the
author's review of the **map** before opening the first phase's brainstorm.

---

## 1. One-paragraph brief for the v2 map

ferrolite v1 met its two founding goals — **beat RawTherapee on browse/load** and be a **GPU /
pipeline / streaming learning vehicle** — with non-destructive editing, a SQLite catalog, color
management, and multi-format export, decomposed across Specs 1–4. v2's mandate is a
**professional-grade successor** in the **Adobe Lightroom / RawTherapee class**: the editing and
asset-management depth v1 deliberately left out (local adjustments, healing, presets/sync, batch,
advanced tone, noise reduction, print/soft-proofing, tethered capture, extensibility) **plus** an
explicit **AI/ML capability track** (AI denoise, auto-masking, generative heal/remove). The v2
map must decompose all of this the way the v1 map decomposed v1 — carrying the licensing tiers,
the crate boundaries, and the five cross-cutting contracts forward — and must **explicitly
re-decide** whether v1's "image quality is secondary" tradeoff survives into a professional-grade
product (§5).

---

## 2. Structure the v2 map MUST mirror (from the v1 map)

Produce the v2 map with **the same section shape** as
`2026-06-28-ferrolite-v1-architecture-map.md`. Reuse its tone (authoritative, concise, "settled
decisions — do not re-litigate"). The required sections, in order:

1. **One-paragraph summary** — what v2 is and its founding goals (successor to §1 above).
2. **Settled decisions (FIXED)** — the constraints carried from v1 (language = Rust, egui/wgpu,
   `rusqlite`, `rawler` never forked, GPL-3.0 binary) **plus** any new v2-level decisions the
   brainstorm settles. Include the "image quality secondary" re-decision as a settled entry (§5).
3. **Workspace crate decomposition** — **carry the two tiers forward explicitly**:
   - **Engine-transferable tier** (`ferrolite-jobs`, `ferrolite-gpu`, `ferrolite-vt`,
     `ferrolite-image`, and any new engine crates): deps **only** permissive (`wgpu`, `rayon`,
     `wide`/`std::simd`), **no copyleft deps, no model weights, no photo concepts** — these stay
     liftable into the author's game engine.
   - **Photo-domain tier** (`ferrolite-decode`, `-catalog`, `-pipeline`, `-color`, `-export`,
     `-previews`, `-app`, and any new photo/AI crates): **may** pull LGPL/GPL deps; the binary is
     GPL-3.0 regardless. Show which existing crates gain responsibilities and which new crates v2
     introduces (e.g. masking, healing, AI runtime — see §3), each tagged with its tier.
4. **Spec → plan decomposition into phases** — carve v2 into independent `spec → plan →
   implementation` phases in dependency/build order, exactly as v1 became Specs 1–4 and Spec 4
   became 4.1–4.6. Each phase: crates touched, tier, one-liner scope, the contracts it must
   honor, what it builds on, and its open questions. The **AI/ML track must be its own
   decomposable area** (§3), not folded into a classical phase.
5. **Five cross-cutting interface contracts** — carry the v1 map's five contracts forward
   verbatim as the seams (§3 below). Extend them **only** with explicit justification; a new
   contract (e.g. for an AI-inference seam) is allowed if the brainstorm shows the existing five
   do not cover it — but the existing five must not drift.
6. **References** — v1 map, Spec 1–4 designs, design system, original proposal (§6 below).

---

## 3. Proposed v2 feature inventory (this prompt PROPOSES; the v2 brainstorm DECIDES)

The lists below are **candidates to scope and decompose**, not a settled feature set. Treat them
as the starting inventory your brainstorm refines, prioritizes, and carves into phases. Decide
inclusion, ordering, and crate placement during the brainstorm — do not take these as final.

### 3.1 Classical track (Lightroom / RawTherapee-class)

- **Local adjustments / masking** — brush, **linear + radial gradients**. These are the Develop
  toolbar's **Heal / Mask / Grad placeholders** made real; masking is the backbone many of the
  other features layer on.
- **Healing / clone / spot removal** — content-aware and clone-source repair.
- **Edit presets + copy-paste / sync across images** — save/apply op-stacks; copy settings from
  one image and sync to a selection.
- **Batch edits** — apply an op-stack or preset across many images as a job.
- **Advanced tone** — parametric curves, color-grading wheels (shadows/midtones/highlights).
- **Noise reduction** — classical (non-AI) denoise as an edit stage. (AI denoise is §3.2.)
- **Print / soft-proofing** — proof against an output ICC profile; out-of-gamut warnings;
  print layout. (v1 deliberately deferred these; Spec 3 named them non-goals.)
- **Tethered capture** — drive a connected camera and ingest shots live.
- **Plugin / extensibility** — a story for third-party edit ops / export targets / panels.

### 3.2 AI/ML track (EXPLICIT, its OWN decomposable area)

This is a first-class, separately-decomposable area of the v2 map — **not** a bullet inside a
classical phase. Candidate capabilities:

- **AI denoise** — learned denoising as a pipeline stage.
- **Subject / sky auto-masking** — ML-generated masks that feed the local-adjustment/masking
  system (3.1).
- **Generative heal / remove** — inpainting-based object removal.

**Open tier questions this prompt FLAGS but does NOT answer** (the v2 brainstorm resolves them):

- **Inference runtime** — ONNX Runtime vs `candle` vs `wgpu` compute (reusing the engine GPU
  surface) vs another option. Weigh dependency weight, licensing, portability, and how much of the
  existing `ferrolite-gpu` compute path can be reused.
- **Model-weight distribution + licensing** — how weights are shipped, versioned, and licensed;
  size/CI impact; whether weights are bundled, downloaded on demand, or optional.
- **Tier placement (load-bearing).** The AI runtime **and** model weights must **not** contaminate
  the engine-transferable tier — that tier stays copyleft-free **and** weight-free so it remains
  liftable into the author's game engine. The AI runtime + weights therefore live in the
  **photo/app tier** or in a **new dedicated tier the v2 map defines** (e.g. a `ferrolite-ai`
  crate, its own licensing/build-gating story). The v2 map must state where they live and why.

---

## 4. Carried-forward discipline the v2 map MUST mandate

Every v2 phase inherits these from v1. State them in the map as fixed, then bind each phase to them.

- **Licensing-tier invariant (load-bearing).** Engine-transferable crates carry **no copyleft
  deps and no model weights**, ever. Any new **heavy / AI / C toolchain** (an inference runtime,
  a native codec, a C lib) gets an explicit **build-gating decision** — a **Cargo feature flag**
  (default-off where appropriate) or **vendoring/bundling** — so **CI and contributors without the
  toolchain still build the workspace green**. This is exactly how Spec 4.2 gated **libjxl** and
  Spec 4.4 gated **Lensfun**; v2's AI runtime and any new native deps follow the same pattern.
- **The five cross-cutting contracts are the seams** (§2.5 / v1 map §5) — do not let them drift:
  1. **Jobs are universal** — everything slow (decode, tile production, export, **batch edits**,
     **AI inference**, model load) submits a `Job` to `ferrolite-jobs` with **priority +
     cancellation + progress sink**; navigation cancels superseded work.
  2. **The catalog is a cache, never the source of truth** — rebuildable from files + sidecars on
     disk; any new cached column/table must be re-derivable.
  3. **Decode yields separable products** — new decode outputs are **additive**.
  4. **The GPU executor is photo-agnostic** — new photo/AI ops are supplied to
     `ferrolite-gpu`'s generic retained-DAG executor as **nodes**, never by reaching into
     executor internals.
  5. **The virtual texture is source-agnostic** — `ferrolite-vt` streams tiles for any large
     source and stays free of photo concepts.
- **Per-control reset** — **every** new adjustable control (every mask slider, curve, grading
  wheel, denoise strength, healing parameter) ships with its own per-control reset affordance
  (shared `draw_reset_arrow` / `EguiSlider` reset column). A new editable control is **not
  complete** until it has one. (CLAUDE.md, load-bearing.)
- **Build-once GPU pipelines** — build pipelines/shaders **once** and reuse; pre-warm expensive
  ones; stream/upload incrementally; profile anything that could exceed a frame budget. Never
  rebuild per image/open/interaction, and never block the UI/update thread. (CLAUDE.md, load-bearing.)
- **Green-gate-then-author-visual-test** — for **every** phase, the workspace gate
  (`cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
  `cargo test --workspace`) being green is **necessary but not sufficient**. Then **STOP and hold
  for the author's (Jann's) hands-on visual test** of the running app before merging/finishing the
  branch. (CLAUDE.md finishing rule.)

---

## 5. The one settled decision v2 must explicitly RE-DECIDE

v1's accepted tradeoff — **"image quality is secondary to speed/architecture"** (v1 map §2;
"RawTherapee speed with weaker image quality is acceptable," parity with darktable/DxO/Adobe
image science explicitly **not** a target) — was correct for a learning-vehicle v1. It is **not**
automatically correct for a **professional-grade successor**.

The v2 brainstorm must **confirm or overturn** this tradeoff and record the outcome as a
**settled decision** in the v2 map's §2. A professional-grade product may well **promote image
quality to a primary goal** (real color science, competitive demosaic, competitive denoise,
gamut-correct soft-proofing) — which cascades into crate choices, dependency weight, the AI
track's scope, and the phase order. Do **not** carry v1's "secondary" stance forward silently;
name it, decide it, and let the decision drive the map.

---

## 6. References

- **v1 architecture map:** `2026-06-28-ferrolite-v1-architecture-map.md` — the **structural
  template** the v2 map mirrors (§1 summary, §2 settled decisions, §3 tiers/crate decomposition,
  §4 spec→plan decomposition, §5 five contracts, §6 two-tier load path, §7 reference).
- **Spec 1 (Speed core):** `2026-06-28-ferrolite-speed-core-design.md`.
- **Spec 2 (Editing):** `2026-06-30-spec2-editing-design.md` — the edit DAG, OpStack sidecar
  persistence, VT tile halo (basis for masking / geometry / healing in v2).
- **Spec 3 (Color & Export):** `2026-07-01-spec3-color-and-export-design.md` — color pipeline,
  swappable display tail, export core, `ColorProfile` decode product (basis for soft-proofing).
- **Spec 4 (Secondary & polish) map + sub-specs:** `2026-07-03-spec4-secondary-and-polish-map.md`
  — the decomposition-child template; libjxl (4.2) and Lensfun (4.4) build-gating precedents;
  this prompt's own parent (§4.6).
- **Design system:** `../../design/ferrolite-design-system.md` — canonical theme/widget/layout
  reference for all v2 UI (masking overlays, panels, new controls).
- **Original proposal:** `2026-06-28-ferrolite-proposal.md` — v1 goals/non-goals, accepted
  tradeoffs, settled stack; the source of truth v2 is measured against and departs from
  deliberately.
- RapidRAW (AGPL-3.0): read for ideas only; **no code copied** into this GPL-3.0 project.

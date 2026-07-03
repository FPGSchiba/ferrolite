# ferrolite — Spec 4: Secondary & Polish — Decomposition Map

> **Status:** Approved decomposition (parent document). **Not a single implementation spec.**
> **Date:** 2026-07-03
> **Parent:** `2026-06-28-ferrolite-v1-architecture-map.md` (§4 **Spec 4**, §5 cross-cutting
> interface contracts, §3 licensing tiers — read first for the settled seams). Completes the
> two items `2026-07-01-spec3-color-and-export-design.md` §2 "Out" deferred into Spec 4
> (AVIF + JPEG-XL export; monitor-profile display CM).
> **Purpose:** Overarching map for the final v1 phase. Spec 4 ("Secondary & polish") is a
> grab-bag of **six independent subsystems**, not one feature. This document is the **handoff
> artifact**: each **Spec 4.x** below becomes its own `spec → plan → implementation` cycle in
> this same directory, on its own branch. Read this first, then write/continue the sub-spec you
> are picking up.
> **Source of truth for goals/non-goals:** the original proposal + the v1 architecture map.

---

## 1. One-paragraph summary

Spec 4 is the **final v1 phase**. Specs 1–3 proved the four load-bearing goals (browse/load
speed, non-destructive editing, color management + multi-format export). Spec 4 is everything
the architecture map §4 filed under "Secondary & polish": Lensfun lens corrections, the two
export/color items deferred out of Spec 3, preview-rendering quality + performance tuning, UX
polish, and broader camera coverage — plus the kickoff for a professional-grade **v2**. These are
**six unrelated subsystems**; each is designed, planned, and shipped on its own. This document
carves them, fixes each one's scope and the seams it must honor, and recommends a build order. It
does **not** design any of them — that is each sub-spec's job.

---

## 2. How a spec agent uses this document

1. Read this whole file, then the v1 architecture map **§5 (cross-cutting contracts)** and
   **§3 (licensing tiers)** — these bind every sub-spec and must not drift.
2. Find your phase under **§4 The six sub-projects**. Read the predecessor spec(s) it names
   (e.g. Spec 3 for 4.2/4.3, Spec 2 for 4.4).
3. Confirm the v1 map **§2 Settled decisions** still hold (do not re-litigate them).
4. Resolve *your* sub-spec's **open questions** (listed per phase) during its brainstorm.
5. Write your phase's spec to `docs/superpowers/specs/YYYY-MM-DD-<phase>-design.md`, get user
   review, then proceed to a writing-plans cycle. One branch per sub-project (off `main`).

**Recommended next phase:** Spec 4.1 (UX polish & split fixes) — lowest risk, immediate value.
See §5 for the full recommended order.

---

## 3. Settled decisions carried in (FIXED — do not re-litigate)

Every Spec 4.x inherits these from the v1 map. They are repeated here because Spec 4's crates
span both licensing tiers and several touch the engine seams.

### 3.1 Licensing tiers (v1 map §3) — the load-bearing invariant

- **Engine-transferable tier** — `ferrolite-jobs`, `ferrolite-gpu`, `ferrolite-vt`,
  `ferrolite-image`. Deps: **only** permissive (`wgpu`, `rayon`, `wide`/`std::simd`). **No
  copyleft deps ever.** These must stay liftable into the author's game engine.
- **Photo-domain tier** — `ferrolite-decode`, `ferrolite-catalog`, `ferrolite-pipeline`,
  `ferrolite-color`, `ferrolite-export`, `ferrolite-previews`, `ferrolite-app`. **May** pull
  LGPL/GPL deps (the whole binary is GPL-3.0 anyway).
- **Consequence for Spec 4:** Lensfun (LGPL), libjxl/ravif codec toolchains, and monitor-ICC
  parsing all live **strictly in the photo tier**. Any engine-crate change a sub-project needs
  (e.g. 4.5's VT/render work) must carry **no photo concepts and no copyleft deps** — the same
  discipline Spec 2's halo and Spec 3's generic tail matrix followed.

### 3.2 Cross-cutting interface contracts (v1 map §5) — honored by every 4.x

1. **Job submission is universal** — anything slow (lens-DB load, export encode, tile
   production, monitor-profile read) submits a `Job` to `ferrolite-jobs` with priority +
   cancellation + progress sink; navigation cancels superseded work.
2. **The catalog is a cache, never source of truth** — rebuildable from files + sidecars on
   disk. New cached columns/tables (if any) must be re-derivable.
3. **Decode yields separable products** — `{ PreviewImage, RawImage, Metadata, ColorProfile }`
   are independently consumable; new decode outputs (e.g. 4.4's lens metadata) are **additive**.
4. **The GPU executor is photo-agnostic** — `ferrolite-gpu`'s generic `Graph<PipelineImage>`
   retained-DAG executor is **not modified**; new photo ops (4.4's lens-correction node) are
   supplied by `ferrolite-pipeline` as nodes, never by reaching into executor internals.
5. **The virtual texture is source-agnostic** — `ferrolite-vt` streams tiles for any large
   source; 4.5's rendering work must keep the VT free of photo concepts.

### 3.3 Accepted tradeoffs (v1 map §2) — still in force

Image quality remains **secondary** to speed/architecture. Spec 4 adds *capabilities* (more
formats, lens geometry, monitor accuracy, more cameras) — it does **not** chase
darktable/DxO/Adobe image-science parity. C bindings are acceptable. `rawler` is never forked;
missing cameras are addressed by **contributing samples upstream** (4.6).

### 3.4 CLAUDE.md rules (project) — bind every 4.x

- **Responsiveness/threading:** never block the UI/update thread; all multi-ms work goes to
  `ferrolite-jobs`; list/grid/filmstrip rendering stays virtualized.
- **GPU:** build pipelines/shaders **once** and reuse; pre-warm expensive pipelines; stream
  incrementally; profile anything that could exceed a frame budget. (Directly load-bearing for
  4.4's new edit pass and 4.5's render work.)
- **Per-component reset:** every new adjustable control (notably 4.4's lens sliders) ships with
  its own per-control reset affordance (shared `draw_reset_arrow` / `EguiSlider` reset column).
- **Finishing a branch:** the workspace gate (`cargo fmt --check` + `cargo clippy --workspace
  --all-targets -- -D warnings` + `cargo test --workspace`) being green is **necessary but not
  sufficient** — then STOP and hold for the author's (Jann's) hands-on visual test before
  merging/finishing every 4.x branch.

---

## 4. The six sub-projects

Each is an independent `spec → plan → implementation` cycle with its own design doc and branch.
**Numbered in recommended build order** (see §5) — the number *is* the order.

---

### Spec 4.1 — UX polish & before/after split fixes

**One-liner:** Small, app-only, no-new-deps polish; fix the before/after split's missing icon
and its lack of user feedback.

- **Crates:** `ferrolite-app` only. (Engine/photo crates untouched.)
- **Tier:** GPL binary; **no new deps**.
- **In (scope):**
  - **Before/after split-view toolbar icon** — the Develop-toolbar toggle for the split
    (Spec 3 §7.2) is **missing its icon**; add it, consistent with the design-system toolbar
    grammar.
  - **Before/after split feedback** — the split reportedly "does not work without user
    feedback": make its active state and the draggable divider **visibly discoverable** (cursor
    change on the handle, a visible divider line/handle, an obvious on/off affordance). The pure
    divider math already exists and is tested (`ferrolite-app/src/develop/split.rs`); this is the
    egui presentation/affordance layer around it.
  - **Grab-bag of small polish** — collect the remaining minor UI nits (the author can supply
    per-screen screenshots during this sub-spec's brainstorm to enumerate them).
- **Out:** the swapchain/tiling-effect fix (that is **Spec 4.5** — it is VT-render work, not UI
  polish); any new editing capability.
- **§5 contracts honored:** UI-thread work stays trivial (no slow work on the update thread).
- **Builds on:** Spec 3 §7.2 before/after split; `split.rs` (pure math, done) + the Develop
  toolbar. Author to provide screenshots of all pages/screens to enumerate the polish list.
- **Open questions for its spec:** the exact polish inventory (screenshot-driven); whether the
  split affordance also wants a keyboard hint surfaced in-UI.

---

### Spec 4.2 — AVIF + JPEG-XL export

**One-liner:** Add the two encoders deferred out of Spec 3 to the existing export core.

- **Crates:** `ferrolite-export` (encoders); `ferrolite-app` (format options in the export UI).
- **Tier:** **photo** — pulls codec toolchains (`ravif`; `jpegxl-rs`/libjxl → **C toolchain**).
- **In (scope):**
  - **AVIF** via `ravif` and **JPEG-XL** via `jpegxl-rs` (libjxl), slotted into the Spec 3
    encode core as two more formats behind the existing "output conversion → resize → encode →
    EXIF + ICC" path. Quality/effort settings per format; ICC embedding as with the quartet.
  - The single **Photo → Export** popup and the **Export module** batch settings gain the two
    new formats. Bit-depth/quality controls follow the existing pattern.
  - **Build-weight handling:** libjxl is the reason these were deferred (v1 map §2 / Spec 3 §2).
    The sub-spec must decide how the C toolchain is gated so CI and contributors without it still
    build (e.g. a Cargo **feature flag** defaulting off, or vendored/bundled build) — this is the
    central open question.
- **Out:** new color-management behavior (reuses Spec 3's `working→output` + ICC path unchanged);
  per-image batch overrides (still shared batch settings).
- **§5 contracts honored:** export stays a `ferrolite-jobs` **Background** job (contract §1);
  encoders live in the photo tier (§3.1); no engine-crate change.
- **Builds on:** Spec 3 §8 export core (`ferrolite-export`), `ferrolite-color`
  `working→output`, the export UI/Export module.
- **Open questions for its spec:** feature-flag vs always-on for libjxl; CI matrix impact +
  contributor build docs; AVIF/JXL quality-vs-effort UI; whether JXL lossless is offered.

---

### Spec 4.3 — Monitor-profile / display color management

**One-liner:** Replace the "assume sRGB display" tail with the real monitor profile; OS
auto-detect + manual picker. This is the clean drop-in Spec 3's swappable tail was built for.

- **Crates:** `ferrolite-color` (parse/apply a monitor ICC → the tail transform);
  `ferrolite-app` (OS detection + manual display-profile picker + persistence); the display
  shader uniform (`ferrolite-vt` `display.wgsl` / pipeline `blit.wgsl`).
- **Tier:** color math + app = **photo**; the shader change stays **engine-transferable** (a
  generic matrix/LUT uniform, no photo concepts — exactly as Spec 3 built it).
- **In (scope):**
  - **Swap the tail** `working→display`: today it is a hardcoded-sRGB 3×3 uniform
    (`display.wgsl` `DisplayColor.m`, Spec 3 §5.2). Replace the sRGB target with the **monitor's
    profile** so the composed `working→display` transform targets the actual display.
  - **OS monitor-profile auto-detection** (Windows first, per the dev platform; other OSes as
    the sub-spec scopes) + a **manual display-profile picker** fallback, with the choice
    **persisted**.
  - **Fallback:** no detectable/parseable profile → sRGB (today's behavior), logged. Never
    panics.
- **Out:** soft-proofing, out-of-gamut warnings (Spec 3 non-goals, unchanged).
- **§5 contracts honored:** the display shader stays photo-agnostic (§3.1 / contract §4/§5);
  profile reads that touch disk/OS go through the job system if non-trivial (contract §1).
- **Builds on:** Spec 3 §4.3 / §5.2 — the tail was **deliberately built swappable** (a matrix
  uniform pushed on working-space change, not per frame) precisely so this drops in without
  reworking the shader structure.
- **Open questions for its spec (flag early):** **many monitor ICC profiles are LUT/curve-based,
  not a single 3×3.** The sub-spec must decide whether to (a) approximate to a 3×3 (cheapest,
  matches "quality secondary"), or (b) extend the tail uniform to a small **1D/3D LUT** path
  (more correct, a real shader-structure change — still generic/engine-safe). Also: per-OS
  detection APIs; multi-monitor / per-window profile; when to re-read on display change.

---

### Spec 4.4 — Lensfun lens corrections

**One-liner:** A new geometry-class edit stage — distortion, vignetting, and chromatic
aberration correction — driven by the Lensfun database and the image's lens metadata.

- **Crates:** `ferrolite-pipeline` (new correction edit node(s) + params in the OpStack);
  `ferrolite-decode` (surface lens metadata — make/model, focal length, aperture — as an
  additive decode product per contract §3); `ferrolite-app` (a Lens Corrections panel section);
  possibly `ferrolite-catalog`/sidecar for persisting the chosen lens + toggles.
- **Tier:** **photo** — Lensfun is **LGPL-3.0** (C library via bindings) + its lens DB. Photo
  tier is correct; the binary stays GPL-3.0.
- **In (scope):**
  - **Lens identification:** match the image's EXIF lens/camera + focal/aperture against the
    Lensfun database to select a correction model (with manual override when auto-match fails).
  - **Corrections as pipeline ops:** **geometric distortion** (a resampling/geometry op — a
    **halo consumer**, like rotate, using the Spec 2 VT halo path), **vignetting** (a per-pixel
    gain), and **transverse CA** (per-channel geometric scale). Added to the canonical op order
    near `Geometry`; each with its own **per-control reset** (CLAUDE.md rule).
  - **Persistence:** the selected lens + enabled corrections persist in the `.xmp` sidecar
    op-stack (Spec 2 §7), merge-preserving.
- **Out:** manual/creative lens effects (this is *correction*, not simulation); building our own
  lens models (Lensfun's DB is the source); defringe/manual-CA sliders unless trivially free.
- **§5 contracts honored:** correction ops are `ferrolite-pipeline` **nodes** on the unchanged
  `Graph<PipelineImage>` (contract §4); lens metadata is an **additive** decode product
  (contract §3); the geometry op reuses the **source-agnostic** VT halo (contract §5); DB load
  is a job (contract §1). Lensfun stays photo-tier (§3.1).
- **Builds on:** Spec 2 edit DAG + `Geometry`/rotate resampling + **VT tile halo** (the neighbor-
  hood/geometry infrastructure already exists); v1 map's "Lensfun bindings (pragmatic,
  deferrable)" decision.
- **Open questions for its spec:** which Lensfun binding crate (or hand-rolled FFI) and its
  build/vendoring story on Windows; how the lens DB is shipped/updated; auto-match UX + manual
  override; whether distortion correction runs at preview-tier and full-res tiled (halo size
  from the distortion model); CPU vs GPU application of the correction.

---

### Spec 4.5 — Preview rendering quality + performance tuning

**One-liner:** Kill the visible **tiling effect** (the swapchain/preview-presentation fix) and do
the sparse-VT tiling refinement + pipeline caching. Engine-tier learning surface.

- **Crates:** `ferrolite-vt` + `ferrolite-gpu` (render/tiling/caching); `ferrolite-app` (viewer
  present path). **Strictly engine-transferable** — no photo concepts, no copyleft deps (§3.1).
- **Tier:** **engine-transferable** — this is the author's priority learning surface; keep it
  clean.
- **In (scope):**
  - **The tiling-effect fix (item 5.1).** Today the display shader (`ferrolite-vt`
    `display.wgsl` `fs_tiled`/`fs_sparse`) returns a flat `vec4(0.05,…)` for **non-resident
    tiles** and resolves visible **per-tile LOD popping** via the coarse fallback — the user sees
    tile boundaries and pop-in while panning/zooming. The fix (working title "swapchain"):
    guarantee a **resident base layer** (a full fit-preview texture composited under the sparse
    tiles) and/or **double-buffer the presented preview** so a coherent image is *always* on
    screen and tiles refine invisibly underneath — no bare/popping tiles ever shown to the user.
  - **Sparse-VT tiling refinement** — prefetch/eviction tuning, feedback-pass latency, LOD
    selection smoothing (architecture map §4 Spec 4 "tiling refinement").
  - **Pipeline caching** — audit/extend the "build once, reuse, pre-warm" discipline
    (`DisplayPipelines` already caches + pre-warms per Spec 1; find and close any remaining
    rebuild-on-interaction paths). Profile against a frame budget (CLAUDE.md GPU rule).
- **Out:** any photo-domain behavior; new edit ops; color changes.
- **§5 contracts honored:** the VT stays **source-agnostic** (contract §5) and photo-agnostic;
  all changes are engine-tier (§3.1). Tile production stays a job (contract §1).
- **Builds on:** Spec 1 VT rungs 1–4 + `DisplayPipelines`; Spec 2 VT halo + GPU tile producer;
  the two CLAUDE.md load-bearing rules (which were *written* because these exact classes of bug
  — eager per-frame decode, pipeline rebuild on open — bit before).
- **Open questions for its spec:** what "swapchain" concretely means here (resident base-layer
  composite vs. present-time double-buffer vs. both); the base-preview memory cost; how it
  interacts with the edited-tile versioning (Spec 2 §5.3); measurable acceptance criteria
  (no visible tile seam / pop-in during a pan at target zoom on the dev GPU).
- **Soft coupling:** shares VT surface with **4.4** (both touch halos/geometry/tiling). Neither
  blocks the other, but whichever ships second should re-run the other's visual check.

---

### Spec 4.6 — Broader camera coverage + v2 kickoff

**One-liner:** Widen RAW-model / color-matrix coverage the right way (upstream, never fork), and
**author the v2 kickoff** — the spec-creation prompt for a professional-grade successor.

- **Crates:** `ferrolite-decode` (verify/extend rawler coverage; surface matrices),
  `ferrolite-color` (add/validate camera color matrices + fallbacks); docs.
- **Tier:** **photo**.
- **In (scope):**
  - **Camera coverage:** identify gaps in `rawler` support / missing color matrices; **contribute
    RAW samples and matrices upstream to `rawler`** (v1 map §2: never fork the decoder);
    strengthen the `ColorProfile::srgb_fallback()` path (Spec 3 §6) for cameras still lacking a
    matrix; add coverage tests for the newly supported models.
  - **v2 kickoff deliverable (the finale):** author a **spec-creation prompt** — structured like
    the prompt that started Spec 4 — that hands off a brainstorm for a **v2 architecture map**
    targeting a **professional-grade photo editor** (Adobe Lightroom / RawTherapee class). That
    prompt should scope, at minimum: **local adjustments / masking** (brush, linear + radial
    gradients — the Develop toolbar's Heal/Mask/Grad placeholders), **healing / clone / spot
    removal**, **edit presets + copy-paste/sync across images**, **batch edits**, **advanced
    tone (parametric curves, color grading wheels)**, **noise reduction**, **print/soft-proofing**,
    **tethered capture**, and a **plugin/extensibility** story — decomposed the same way the v1
    map decomposed v1 into Specs 1–4, with licensing tiers and cross-cutting contracts carried
    forward. It is a **brainstorm-kickoff artifact**, not a v2 design itself.
- **Out:** actually designing v2 (that is the v2 map's job); forking rawler.
- **§5 contracts honored:** matrices/coverage stay additive decode/color products (contract §3);
  catalog remains a cache (contract §2).
- **Builds on:** Spec 3 `ColorProfile` decode product + `ferrolite-color`; the v1 architecture
  map as the template for the v2 kickoff prompt.
- **Open questions for its spec:** which camera families to prioritize; how upstream
  contribution cadence is tracked; the exact v2 feature inventory + decomposition the kickoff
  prompt proposes.

---

## 5. Dependencies & recommended build order

**Hard dependencies:** essentially none — every sub-project can be specced and shipped on its own
branch off `main`. The only ordering constraints are soft:

- **4.6 goes last** — it carries the v2 kickoff, which should reflect everything v1 shipped.
- **4.4 ↔ 4.5** share the VT halo/tiling surface (soft coupling, not blocking; the second to
  ship re-runs the other's visual check).

```
recommended order (low-risk / value-first → v2 last):

  4.1  UX polish & split fixes      (S,  app-only, no deps)         ── start here
   │
  4.2  AVIF + JPEG-XL export        (S–M, clean Spec-3 drop-in)
   │
  4.3  Monitor color management     (M,  clean Spec-3 drop-in)
   │
  4.4  Lensfun corrections          (M–L, biggest new feature) ─┐ soft VT
   │                                                            │ coupling
  4.5  Preview rendering + perf     (M–L, engine-tier) ─────────┘
   │
  4.6  Camera coverage + v2 kickoff (M,  goes last — carries v2)
```

**Rationale (recorded 2026-07-03):** front-load the low-risk, self-contained wins — 4.1 (app-only
polish), then the two items Spec 3 pre-built seams for (4.2 export encoders, 4.3 the swappable
tail) — before the larger features (4.4 Lensfun, 4.5 VT/perf), with 4.6 last so its v2 kickoff
reflects the finished v1. The order is a recommendation, not a hard chain: a spec agent may pull a
later phase forward if priorities change (e.g. the tiling effect in 4.5), provided 4.6 stays last.

---

## 6. Cross-cutting notes (apply to more than one sub-project)

- **Deferred-from-Spec-3 pointers:** 4.2 (AVIF/JXL) and 4.3 (monitor CM) are the two items Spec 3
  §2 explicitly deferred into Spec 4; Spec 3 §4.3/§5.2 built the display tail swappable *for* 4.3.
- **C-toolchain weight:** 4.2's libjxl (and 4.4's Lensfun) add native build dependencies. Each
  sub-spec owns a **build-gating decision** (feature flag / vendoring) so CI and contributors
  without the toolchain still build the workspace green.
- **Monitor profiles may need a LUT, not just a 3×3** (4.3) — flagged early because it is the one
  place a "drop-in" might turn into a (still engine-safe) shader-structure change.
- **Engine-tier discipline** (4.5, and the shader change in 4.3, and 4.4's VT halo use): any
  change to `ferrolite-gpu`/`ferrolite-vt`/`ferrolite-image` carries **no photo concepts and no
  copyleft deps** (§3.1). This is the author's transferable learning surface — keep it clean.
- **CLAUDE.md rules bind every branch** (§3.4): responsiveness/threading, build-once GPU
  pipelines, per-control reset for every new adjustable control (4.4), and — for **every** 4.x —
  green workspace gate **then hold for the author's visual test** before finishing.

---

## 7. Reference

- **v1 architecture map:** `2026-06-28-ferrolite-v1-architecture-map.md` — §2 settled decisions,
  §3 licensing tiers/crate decomposition, §4 Spec-4 entry, §5 cross-cutting contracts. The parent
  of this document and the template for 4.6's v2 kickoff prompt.
- **Spec 2 (Editing):** `2026-06-30-spec2-editing-design.md` — the edit DAG, OpStack sidecar
  persistence, VT halo + GPU tile producer (basis for 4.4 and 4.5).
- **Spec 3 (Color & Export):** `2026-07-01-spec3-color-and-export-design.md` — the swappable
  display tail (basis for 4.3), the export encode core (basis for 4.2), the before/after split
  (basis for 4.1), the `ColorProfile` decode product (basis for 4.6).
- **Design system:** `../../design/ferrolite-design-system.md` — canonical theme/widget/layout
  reference for all UI work (4.1's toolbar icon + polish; 4.3/4.4 panel controls).
- **Original proposal:** `2026-06-28-ferrolite-proposal.md` — goals/non-goals, settled stack.
- RapidRAW (AGPL-3.0): read for ideas only; no code copied into this GPL-3.0 project.

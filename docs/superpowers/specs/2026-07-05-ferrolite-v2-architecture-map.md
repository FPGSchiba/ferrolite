# ferrolite — v2 Architecture Map & Decomposition

> **Status:** Approved decomposition (parent document). **Not a single implementation spec,
> not a writing-plan.** The direct successor to the v1 architecture map.
> **Date:** 2026-07-05
> **Parent lineage:** v1 architecture map (`2026-06-28-ferrolite-v1-architecture-map.md`) →
> Spec 4 map (`2026-07-03-spec4-secondary-and-polish-map.md`) → Spec 4.6 design →
> **v2 kickoff prompt** (`2026-07-05-v2-kickoff-prompt.md`) → **this map**.
> **Purpose:** Overarching architecture map for ferrolite **v2** — a professional-grade,
> Lightroom/RawTherapee-class successor. This document is the **handoff artifact** for spec
> agents. Each phase below (P1–P9 classical, A0–A4 AI) becomes its own
> `spec → plan → implementation` cycle on its own branch off `main`. Read this first, then
> write/continue the spec for the phase you are picking up.
> **Source of truth for goals/non-goals:** the v2 kickoff prompt + this map (which supersede,
> where they differ, the v1 map's "image quality secondary" stance — see §2).

---

## How a spec agent uses this document

1. Read this whole file — it carries the settled decisions and the cross-cutting interface
   contracts every v2 phase must honor.
2. Find your phase under **§4 Spec → plan decomposition**. Read the predecessor spec(s) and
   phases it names (v1 Specs 1–4 + 4.x are all merged into `main`).
3. Confirm the **§2 Settled decisions** still hold (do not re-litigate them).
4. Honor the **§5 Cross-cutting interface contracts** — six seams now (v1's five, unchanged,
   plus the AI-inference seam). They must not drift between phases.
5. Honor the **§3 licensing tiers** — the engine-transferable tier stays copyleft-free **and**
   weight-free; the new `ferrolite-ai` tier and all model weights stay out of it.
6. Write your phase's spec to `docs/superpowers/specs/YYYY-MM-DD-<phase>-design.md`, get the
   author's review, then proceed to a writing-plans cycle. **One branch per phase.**

**Next phase to pick up:** **P1 — Masking & local-adjustments engine** (the backbone the most
features layer on; makes the Develop toolbar's Heal/Mask/Grad placeholders real). See §6 build order.

---

## 1. One-paragraph summary

ferrolite v1 met its two founding goals — **beat RawTherapee on browse/load** and be a **GPU /
pipeline / streaming learning vehicle** — with non-destructive editing, a SQLite catalog, color
management, and multi-format export, across Specs 1–4. **v2 is the professional-grade successor**
in the **Adobe Lightroom Classic / RawTherapee class**: the editing and asset-management depth v1
deliberately left out — **local adjustments & masking, healing, presets/sync, batch, advanced
tone & color grading, classical noise reduction & sharpening, geometry/perspective, print &
soft-proofing, and advanced DAM (culling/compare + HDR/panorama merge)** — **plus** a first-class,
**on-device AI/ML track** (AI denoise, AI auto-masking, non-generative object removal, super-
resolution). v2 **promotes image quality to a primary goal** (overturning v1's "quality is
secondary" tradeoff — §2), which cascades into real color science, a competitive demosaic, real
denoise, and gamut-correct proofing. v2 carries the v1 licensing tiers and cross-cutting contracts
forward verbatim, adds one new AI-inference seam and one new `ferrolite-ai` tier, and decomposes
the whole into fourteen independent phases in dependency/build order.

---

## 2. Settled decisions (FIXED — do not re-litigate)

### 2.1 Carried from v1 (still fixed)

| Layer | Decision |
|---|---|
| Language | Rust |
| GUI | egui (`eframe` + `egui-wgpu`) |
| GPU pipeline | `wgpu` + custom WGSL compute shaders (the primary learning surface) |
| CPU parallelism | `rayon`; SIMD via `wide`/`std::simd` |
| RAW decode | `rawler` — **never forked**; missing cameras addressed upstream |
| Catalog/DAM | SQLite via `rusqlite` (pinned 0.32) |
| Color management | `moxcms` (pure Rust) preferred; `lcms2` fallback |
| Project license | **GPL-3.0** (whole binary). Engine-transferable crates carry **no copyleft deps** so the author can relicense their own engine code regardless. |

### 2.2 New v2-level decisions (settled in this brainstorm, 2026-07-05)

| # | Question | Decision | Rationale / cascade |
|---|---|---|---|
| **D1** | **v1's "image quality is secondary" tradeoff** (kickoff §5 RE-DECISION) | **OVERTURNED — image quality is now a PRIMARY goal.** | A professional-grade product must be competitive on output. Cascades: **dual-illuminant** camera color (not v1's single-illuminant), a **competitive demosaic** option, **real denoise** (classical + AI raw-domain), and **gamut-correct** soft-proofing. Drives crate choices, dependency weight, the AI track's centrality, and the phase order (P2 color-science lands early). This does **not** relax the speed goals — G1/G2 (browse/load speed) remain non-negotiable; quality is added *alongside*, not instead. |
| **D2** | Is there an AI/ML track, and how bounded? | **YES — a first-class, on-device-only AI track**, its own decomposable area (A0–A4). | Grounded in the shipping landscape (LR Denoise/AI-masking on-device; DxO/Topaz on-device) and a GPL-3.0 precedent (below). |
| **D3** | AI scope | **In:** AI **denoise** (raw-domain + RGB), AI **auto-masking** (subject/sky/object), AI **object removal** (non-generative), AI **super-resolution**. **Out (deferred):** true **generative fill/remove**. | Generative fill is cloud-only even in Lightroom, and its weights (SD/SDXL, CreativeML OpenRAIL behavioral restrictions, 4–7 GB) neither ship cleanly under GPL nor fit a 6–8 GB GPU. Non-generative LaMa is the on-device "Remove tool" equivalent. |
| **D4** | AI inference runtime | **`ort` (ONNX Runtime)** via the `ort` crate, **`load-dynamic`**, behind a **default-off Cargo feature**. | `ort` + ONNX Runtime are both permissive (Apache-2.0/MIT). `load-dynamic` means CI/contributors build green with no native toolchain; the runtime lib ships only in release artifacts. DirectML (Win) / CoreML (macOS) / CUDA (Linux-NVIDIA). Directly runs the ONNX shortlist below. **Gating pattern = exactly Spec 4.2 libjxl / Spec 4.4 Lensfun.** (`burn`+`burn-wgpu` — pure-Rust, reuses our wgpu device — was considered and rejected as the *primary* runtime because `burn-import` has only partial ONNX operator coverage; it remains a possible future backend behind the seam D6/§5.6.) |
| **D5** | Model-weight distribution & licensing | **Opt-in downloads, versioned; never bundled in the binary.** Only **permissive/GPL-compatible weights** (MIT/BSD/Apache) ship. | Keeps the binary small, isolates per-model license notices, mirrors how LR/darktable store model data separately. **Reference shortlist = `darktable-org/darktable-ai`** (GPL-3.0 ONNX pipeline, *AI Model Integration Policy* forbidding generative/cloud/telemetry): NAFNet (RGB denoise, MIT), **UtNet2** (raw-Bayer denoise), **SAM 2.1** tiny/small/base-plus + **SegNext** (masking, Apache), BSRGAN/RealPLKSR (upscale), plus **LaMa** (Apache, object removal). **Excluded on license:** RMBG (CC-BY-NC), SD/SDXL inpainting (OpenRAIL). Each specific checkpoint's weight license is re-verified when its phase is specced. |
| **D6** | Where does the AI runtime + weights live? (kickoff §3.2, load-bearing) | **A new dedicated `ferrolite-ai` tier** (photo-side), feature-gated. **It must NOT contaminate the engine-transferable tier**, which stays copyleft-free **and** weight-free so it remains liftable into the author's game engine. | The kickoff's flagged invariant. `ort` + weights + the ONNX I/O glue live only here. See §3. |
| **D7** | New engine-transferable machinery placement | **Mask compositing, brush-stroke raster buffers, and tiled neighbourhood ops are engine-transferable** (no photo concepts) → **engine tier**. The *photo* meaning ("which adjustment applies through which mask") stays in `ferrolite-pipeline`. | Keeps the reusable masking/brush/tiling primitives liftable while photo semantics stay domain-specific — the same boundary v1 drew between the generic GPU executor and photo edit nodes. |

### 2.3 Deferred non-goals (explicitly OUT of v2 — record, do not design for)

- **NG-v2-1 — Cloud sync / online library / account services.** A possible v3 concern; out of v2
  (carried from v1 NG6, re-affirmed by the kickoff).
- **NG-v2-2 — Generative fill / generative remove.** Per D3: cloud-only in Lightroom, non-shippable
  weights, too heavy for the target GPU. Non-generative LaMa removal (A3) is the on-device answer.
- **NG-v2-3 — Tethered capture.** A large, vendor-SDK-heavy, self-contained subsystem orthogonal to
  the editing mandate (v1 NG4). Deferred to a later cycle / v3.
- **NG-v2-4 — Plugin / third-party extensibility system.** A cross-cutting capstone; premature to
  design before v2's internal op/mask/export/AI seams have proven out. Deferred to v2.x-capstone/v3.
- **NG-v2-5 — Focus stacking.** Not native even in Lightroom (delegates to Photoshop); out of scope.
- **NG-v2-6 — Mobile.** Desktop only (carried from v1 NG5).

### 2.4 Accepted tradeoffs still in force

- **Speed is still non-negotiable.** Every phase honors the CLAUDE.md responsiveness/threading and
  build-once-GPU rules; D1 adds quality *without* regressing G1/G2.
- **C bindings / native libs remain acceptable** where they buy real capability, **always behind a
  build-gating decision** (feature flag or vendoring) so CI + toolchain-less contributors build green.
- **`rawler` is never forked.** New-camera gaps go upstream (Spec 4.6 precedent).

---

## 3. Workspace crate decomposition

Organizing principle unchanged from v1: **separate engine-transferable machinery from photo-domain
logic at the crate boundary**, so reusable subsystems carry zero copyleft deps (and now zero model
weights) and stay liftable into the author's game engine even though the binary is GPL-3.0. v2 adds
one machinery seam to the engine tier and one entirely new tier for AI.

### 3.1 Engine-transferable tier (deps: only permissive — `wgpu`, `rayon`, `wide`/`std::simd`; **no copyleft, no model weights, ever**)

| Crate | v1 responsibility | v2 additions (photo-agnostic) |
|---|---|---|
| `ferrolite-jobs` | Threaded scheduler: priority, cancellation, dep-graph, progress sinks. | Unchanged API; now also carries batch-edit jobs and AI-inference jobs (still generic `Job`s). |
| `ferrolite-gpu` | Generic retained-DAG executor (dirty-flag, cached outputs). | New generic **mask-compositing** compute nodes + **tiled neighbourhood-op** helpers, supplied as generic nodes — **no photo concepts**. |
| `ferrolite-vt` | Sparse virtual texture; source-agnostic tile streaming + halo (Spec 2). | Brush-stroke raster buffers stream/composite through the VT as a generic large source; still no photo concepts. |
| `ferrolite-image` | Core pixel/buffer/tile/color vocabulary. | New generic **mask buffer** + **brush-stroke buffer** vocabulary types (D7). |
| **`ferrolite-mask`** *(new, optional)* | — | **Candidate new engine crate**: mask compositing + brush-stroke rasterization + tiled neighbourhood primitives, if these outgrow `ferrolite-image`/`-gpu`. Engine-tier, permissive-only. *P1 decides crate-vs-extend.* |

### 3.2 Photo-domain tier (may pull LGPL/GPL deps → binary is GPL-3.0 regardless)

| Crate | v1/v1.x responsibility | v2 additions |
|---|---|---|
| `ferrolite-decode` | `rawler` wrap: decode + preview + metadata + `ColorProfile`. | **Dual-illuminant** color data surfaced (D1); optional competitive-demosaic hook (P2). Additive per contract §3. |
| `ferrolite-catalog` | SQLite DAM; sidecar I/O; edit/rating cache columns. | Preset store; culling/compare metadata; merge-result provenance. Cache-only per contract §2. |
| `ferrolite-pipeline` | Photo edit DAG on `ferrolite-gpu`'s executor. | The bulk of v2: local-adjustment/mask ops, curves/grading, classical NR/sharpen, geometry/perspective, healing ops. Consumes mask/AI outputs as nodes. |
| `ferrolite-color` | Camera→working→display/output color math. | Dual-illuminant matrices, gamut-correct **soft-proof** transforms + out-of-gamut detection (P8). |
| `ferrolite-export` | Encoders + metadata write. | Batch export queue integration (P7); super-res as an export-time enhance (A4). |
| `ferrolite-lens` | Lensfun corrections (Spec 4.4). | Extended by P6 geometry/perspective (Upright/auto-perspective). |
| `ferrolite-previews` | Preview generation. | Merge/culling preview support (P9). |
| `ferrolite-app` | egui shell, panels, viewer, wiring. GPL binary. | All new UI: mask overlays, healing/clone canvas, curves/wheels, transform, print/soft-proof, compare/cull, AI panels. Every new control ships **per-control reset** (CLAUDE.md). |

### 3.3 AI tier (new — `ferrolite-ai`; photo-side; **feature-gated default-off**)

| Crate | Responsibility | Notes |
|---|---|---|
| **`ferrolite-ai`** *(new)* | The `ort` inference runtime, ONNX session/model management, weight download+versioning, tensor I/O glue, and the **AI-inference seam** (§5.6). Exposes capability interfaces (denoise / segment / inpaint / upscale) as `Job`s; consumers never touch `ort` directly. | **Default-off Cargo feature** (libjxl/Lensfun pattern). `ort` (Apache/MIT) + `load-dynamic`. **The only crate that may hold model weights or the inference runtime.** Must NOT be a dependency of any engine-tier crate (D6). |

**Critical boundaries (unchanged in spirit from v1):**
- `ferrolite-gpu` owns generic recompute/compositing machinery; `ferrolite-pipeline` owns photo ops.
- **`ferrolite-ai` is a leaf on the photo side** — engine-tier crates never depend on it, so the
  engine tier stays weight-free and copyleft-free and liftable.

---

## 4. Spec → plan decomposition (fourteen phases)

Each phase is an independent `spec → plan → implementation` cycle with its own dated design doc and
branch off `main`. **Classical/quality = P1–P9; AI track = A0–A4** (its own decomposable area).
Per phase: **crates · tier · one-liner · contracts honored · builds on · open questions.**

---

### Classical & quality track

#### P1 — Masking & local-adjustments engine  *(the backbone — build first)*
- **Crates:** `ferrolite-image`/`ferrolite-gpu` (+ candidate `ferrolite-mask`) — **engine tier**;
  `ferrolite-pipeline` + `ferrolite-app` — photo tier.
- **One-liner:** Make the Develop toolbar's **Heal/Mask/Grad placeholders real** — a local-
  adjustment framework with **brush, linear gradient, radial gradient, luminance-range, and
  color-range** masks; **add / subtract / intersect** compositing; per-mask adjustment sets.
- **Contracts:** new mask/brush/tile-neighbourhood machinery is **engine-tier, photo-agnostic**
  (§3.1/D7, contract 4/5); masks are supplied to the executor as **generic nodes**; per-mask
  strokes stream via the **source-agnostic VT** (contract 5); mask params persist in the OpStack
  sidecar (contract 2, catalog stays cache). Every mask control gets **per-control reset**.
- **Builds on:** Spec 2 edit DAG + OpStack sidecar + **VT halo + GPU tile producer**; Spec 2's
  two-tier (preview-res vs full-res) recompute.
- **Open questions:** mask representation (raster vs parametric-shape vs hybrid) & its OpStack
  encoding; how a masked adjustment slots into the canonical op order; brush-stroke buffering &
  full-res tiled application; interaction of mask edits with the edited-tile `opstack_version`
  invalidation.

#### P2 — Image-quality & color-science foundation  *(the D1 promotion, made concrete)*
- **Crates:** `ferrolite-color`, `ferrolite-decode`, `ferrolite-pipeline` — **photo tier**.
- **One-liner:** Deliver the D1 quality promotion at the pipeline head: **dual-illuminant** camera
  color, a **competitive demosaic** option, and a gamut-correct working path.
- **Contracts:** color/demosaic data is an **additive** decode product (contract 3); the
  camera→working transform stays a `ferrolite-pipeline` node on the unchanged executor (contract 4);
  the display tail stays the generic swappable matrix/LUT (Spec 3/4.3). No engine-tier photo leak.
- **Builds on:** Spec 3 `ferrolite-color` (single-illuminant `ColorMatrixNode`, working spaces) +
  `ColorProfile` decode product; Spec 4.3 monitor-profile tail.
- **Open questions:** dual-illuminant interpolation source (DNG-style A+D65 from `rawler`?);
  which demosaic (AHD/DCB/RCD-class) and whether it is CPU or a WGSL pass; default working space
  reassessment under a quality-primary stance; where the quality/speed toggle lives.

#### P3 — Advanced tone & color grading
- **Crates:** `ferrolite-pipeline`, `ferrolite-app` — **photo tier**.
- **One-liner:** **Parametric + point tone curves** (incl. per-channel R/G/B), **color-grading
  wheels** (shadow/mid/highlight hue-sat-lum), and **dehaze**.
- **Contracts:** new curve/grade/dehaze ops are executor **nodes** (contract 4); no new deps.
  Per-control reset on every wheel/curve/slider.
- **Builds on:** Spec 2 `ToneCurve` op + LUT bake; Spec 3 working-space color.
- **Open questions:** curve UI (parametric regions vs draggable points) & OpStack encoding; grading
  model (lift/gamma/gain vs shadows/mid/highlights); dehaze algorithm (classical, not ML — cheap
  local-contrast/atmospheric model); op-order placement.

#### P4 — Noise reduction & sharpening (classical)
- **Crates:** `ferrolite-pipeline`, `ferrolite-app` — **photo tier**.
- **One-liner:** Classical (non-AI) **luminance/color NR** + **capture sharpening** + **output
  sharpening** at export.
- **Contracts:** NR/sharpen are **halo consumers** on the source-agnostic VT (contract 5, Spec 2
  halo); executor nodes (contract 4); output sharpening runs in the export `Job` (contract 1).
- **Builds on:** Spec 2 `Sharpen` unsharp-mask op + halo-size plumbing.
- **Open questions:** NR algorithm (wavelet/bilateral) and its halo footprint; capture-vs-output
  sharpening split in the pipeline vs export; relationship to A1 AI-denoise (classical is the
  always-available baseline; AI-denoise is the opt-in heavier path).

#### P5 — Healing / clone / spot removal (classical)
- **Crates:** `ferrolite-pipeline`, `ferrolite-app` — **photo tier**.
- **One-liner:** Content-aware **heal**, **clone**-source, and **spot removal** — classical (the
  on-device non-AI baseline; A3 adds the ML LaMa path on top).
- **Contracts:** heal/clone regions ride the P1 mask machinery + VT halo (contract 5); ops are
  executor nodes (contract 4); persisted in the OpStack sidecar (contract 2). Per-control reset.
- **Builds on:** **P1** (mask/region infrastructure) + Spec 2 halo/geometry resampling.
- **Open questions:** heal algorithm (patch-match / Poisson blend) and full-res tiling; clone-source
  UX; how healing stacks with other local adjustments; interaction with A3 (shared UI, different
  engine).

#### P6 — Geometry / perspective / transform
- **Crates:** `ferrolite-lens`, `ferrolite-pipeline`, `ferrolite-app` — **photo tier**.
- **One-liner:** **Upright / auto-perspective / keystone** correction and manual geometry beyond
  Spec 4.4's Lensfun profile corrections.
- **Contracts:** a geometry op = **halo-consuming** resampling node (contracts 4/5), same class as
  Spec 4.4 distortion; per-control reset.
- **Builds on:** Spec 4.4 Lensfun geometry stage + Spec 2 `Geometry`/rotate resampling + VT halo.
- **Open questions:** auto-perspective detection (line-finding) CPU vs GPU; guided-upright UX
  (drawn guides); op-order interaction with crop and lens distortion.

#### P7 — Presets, copy/paste/sync & batch
- **Crates:** `ferrolite-catalog`, `ferrolite-export`, `ferrolite-app` — **photo tier**.
- **One-liner:** Save/apply **op-stack presets**; **copy settings** from one image and **sync** to a
  selection; **batch** edits/export as background jobs.
- **Contracts:** batch runs entirely through `ferrolite-jobs` (contract 1) with cancellation;
  presets are re-derivable data, catalog stays a **cache** (contract 2). Presets that carry masks
  (P1) apply through the same op-stack path.
- **Builds on:** Spec 2 OpStack + sidecar; Spec 3 export core / Export module; **P1** (so preset
  sync includes masked adjustments).
- **Open questions:** preset format & partial-apply semantics (which ops sync); mask/AI-mask
  portability across images (adaptive presets); batch progress/failure UX at scale.

#### P8 — Print & soft-proofing / gamut
- **Crates:** `ferrolite-color`, `ferrolite-app` — **photo tier**.
- **One-liner:** **Soft-proof** against an output/printer ICC, **out-of-gamut warnings**, and a
  **print layout** module. (v1 Spec 3 named these non-goals; D1 promotes them.)
- **Contracts:** proof/gamut math in `ferrolite-color` (pure, tested); the display tail stays the
  generic swappable matrix/**LUT** (contract 4/5, Spec 4.3 groundwork — soft-proof likely forces the
  LUT path); ICC reads that touch disk go through a `Job` (contract 1).
- **Builds on:** **P2** (gamut-correct color), Spec 3 `working→output` + ICC, Spec 4.3 tail.
- **Open questions:** rendering-intent handling; per-printer profile management; print-layout scope
  (templates, soft-proof-only-first?); LUT-tail extension shared with Spec 4.3.

#### P9 — Advanced DAM: culling/compare + HDR/panorama merge
- **Crates:** `ferrolite-catalog`, `ferrolite-previews`, `ferrolite-pipeline`, `ferrolite-app` —
  **photo tier**.
- **One-liner:** **Compare/survey/cull** view + flags/labels/stacking depth, and **Photo-Merge**
  (**HDR** + **Panorama**) producing a new editable master. (Focus-stacking excluded — NG-v2-5.)
- **Contracts:** merges are `ferrolite-jobs` background work (contract 1); merged masters are
  files-on-disk, catalog stays a **cache** (contract 2); merge output re-enters the normal
  decode/pipeline path (contract 3).
- **Builds on:** Spec 1 catalog/browser + rating/flag; Spec 1 VT + Spec 2 pipeline for merge preview.
- **Open questions:** alignment/deghosting (HDR) and stitching/projection (pano) algorithms &
  crates (permissive-only); compare-view virtualization (CLAUDE.md); merge-result provenance in the
  catalog cache.

---

### AI track  *(its own decomposable area — `ferrolite-ai`, feature-gated)*

#### A0 — AI runtime foundation  *(prerequisite for A1–A4)*
- **Crates:** **`ferrolite-ai`** (new, photo tier, default-off feature).
- **One-liner:** Stand up `ort` (`load-dynamic`), ONNX session/model management, **opt-in weight
  download + versioning**, tensor I/O glue, and the **AI-inference seam** (§5.6) — the capability
  interface every AI feature calls.
- **Contracts:** inference is submitted as a `Job` with priority + cancellation + progress
  (contract 1, **and the new contract 6**); `ferrolite-ai` is a **photo-side leaf** — no engine-tier
  crate depends on it (D6); weights never bundled (D5). **Build-gating** exactly like libjxl/Lensfun.
- **Builds on:** the v2 kickoff's runtime research; `darktable-ai` as the model/ONNX reference; the
  Spec 4.2/4.4 native-dep gating precedents.
- **Open questions:** execution-provider selection per OS (DirectML/CoreML/CUDA) & fallback to CPU;
  weight hosting/mirroring, integrity, and versioning; whether the ONNX Runtime lib ships in release
  artifacts or is fetched; memory coexistence of `ort`'s GPU context with our `wgpu` device on 6–8 GB.

#### A1 — AI denoise
- **Crates:** `ferrolite-ai` + `ferrolite-pipeline` (stage integration) + `ferrolite-app`.
- **One-liner:** Learned denoise as a pipeline stage — **raw-domain UtNet2** (joint demosaic+denoise,
  the DxO-DeepPRIME/LR approach) + **RGB NAFNet**.
- **Contracts:** inference via the A0 seam as a `Job` (contracts 1/6); the denoised result re-enters
  the pipeline as a node/product (contracts 3/4); per-control reset on strength.
- **Builds on:** **A0**; **P2** (raw/color path); **P4** (classical NR is the always-available
  baseline this augments).
- **Open questions:** where raw-domain denoise sits vs demosaic (before, as joint) vs RGB (after);
  tiling large images through the model within VRAM; preview-tier vs full-res application; caching a
  costly inference result against the OpStack version.

#### A2 — AI auto-masking
- **Crates:** `ferrolite-ai` + `ferrolite-pipeline`/`ferrolite-app` (feeds P1).
- **One-liner:** ML **subject / sky / object** masks (**SAM 2.1** interactive click/box + **SegNext**
  semantic) that generate masks **into the P1 masking engine**.
- **Contracts:** inference via the A0 seam as a `Job` (contracts 1/6); output is a **mask** consumed
  by P1's generic mask machinery — the AI produces the mask, P1 composites/applies it.
- **Builds on:** **A0** + **P1** (the masking engine the AI masks feed).
- **Open questions:** interactive latency for click-to-mask; mask hand-off format (raster into P1);
  refine/combine AI masks with manual brush/range masks; model size tier (tiny/small/base-plus) per
  GPU budget.

#### A3 — AI object removal (non-generative)
- **Crates:** `ferrolite-ai` + `ferrolite-pipeline`/`ferrolite-app`.
- **One-liner:** **LaMa** (Apache) content-aware inpainting — the on-device "Remove tool" for
  distractions, **non-generative** (NG-v2-2 keeps generative fill out).
- **Contracts:** inference via the A0 seam as a `Job` (contracts 1/6); shares P5's heal/clone UI and
  P1's region machinery; result persisted/recomputed like other ops (contracts 2/4).
- **Builds on:** **A0** + **P1/P5** (region selection + healing UI); verify the LaMa **weights**
  license explicitly at spec time (repo licenses code Apache; weights need confirmation).
- **Open questions:** mask/region hand-off; blend with surrounding pixels; full-res tiling; UX shared
  with classical heal (P5) — same tool, two engines.

#### A4 — AI super-resolution
- **Crates:** `ferrolite-ai` + `ferrolite-export`/`ferrolite-app`.
- **One-liner:** **Real-ESRGAN** (BSD) / RealPLKSR / BSRGAN upscaling as an enhance/export step.
- **Contracts:** inference via the A0 seam as a `Job` (contracts 1/6); typically an export-time
  enhance (contract 1); result is a new product (contract 3).
- **Builds on:** **A0**; Spec 3/4.2 export core.
- **Open questions:** 2×/4× tiers & tiling; interaction with output sharpening (P4) and export
  resize; where in the UI (Enhance action vs export option).

---

## 5. Cross-cutting interface contracts (honored by EVERY phase)

v1's **five** contracts carry forward **verbatim** (do not let them drift). v2 adds a **sixth** for
the AI-inference seam — justified because AI inference is a new slow, cancellable, runtime-backed
capability whose isolation (D6) the existing five do not, by themselves, express.

1. **Job submission is universal.** Everything slow — ingest, thumbnails, decode, tile production,
   export, **batch edits**, **AI inference**, model load — submits a `Job` to `ferrolite-jobs` with
   **priority + cancellation + progress sink**. Navigation cancels superseded work.
2. **The catalog is a cache, never the source of truth.** Rebuildable from files + sidecars on disk;
   any new cached column/table (presets, edit badges, merge provenance, culling state) is re-derivable.
3. **Decode yields separable products.** New decode outputs (dual-illuminant color data, lens
   metadata, merge inputs) are **additive**; existing consumers keep working.
4. **The GPU executor is photo-agnostic.** `ferrolite-gpu`'s generic `Graph<PipelineImage>` retained-
   DAG executor is **not modified**; new photo/mask/curve/geometry ops are supplied by
   `ferrolite-pipeline` as **nodes**, never by reaching into executor internals. New *generic*
   compositing/neighbourhood machinery may be added to `ferrolite-gpu` **only** if it carries no
   photo concepts.
5. **The virtual texture is source-agnostic.** `ferrolite-vt` streams tiles (and brush-stroke
   buffers, and merge/masked sources) for **any** large source and stays free of photo concepts.
6. **AI inference is bounded behind a runtime-agnostic seam in `ferrolite-ai`.** All AI features
   request inference through `ferrolite-ai`'s capability interfaces (denoise / segment / inpaint /
   upscale) — submitted as `Job`s (contract 1) — and **never touch `ort` or model weights directly**.
   `ferrolite-ai` is a **photo-side leaf**: no engine-tier crate may depend on it, keeping the engine
   tier copyleft-free and weight-free (D6). The seam keeps the runtime swappable (e.g. a future
   `burn-wgpu` backend) without touching any consumer.

---

## 6. Dependencies & recommended build order

**Hard prerequisites:** **P1** precedes P5 (healing regions), A2 (AI masks feed P1), and P7 (preset
sync of masked edits). **A0** precedes A1–A4. **P2** feeds P8 (gamut) and A1 (raw path). Everything
else is soft — each phase ships on its own branch off `main`.

```
build order (backbone & quality first → AI interleaves after A0):

  P1  Masking & local-adjustments engine   ── start here (backbone; engine + photo tiers)
   │
  P2  Image-quality & color-science         (D1 promotion; feeds P8, A1)
   │
  A0  AI runtime foundation                 (ferrolite-ai + ort; prereq for A1–A4 — can begin in parallel with P2)
   │
  P3  Advanced tone & color grading
  P4  Noise reduction & sharpening (classical)
  P5  Healing / clone / spot  (needs P1)
  P6  Geometry / perspective / transform
   │
  A1  AI denoise            (needs A0, P2; augments P4)
  A2  AI auto-masking       (needs A0, P1)
  A3  AI object removal     (needs A0, P1/P5)
  A4  AI super-resolution   (needs A0)
   │
  P7  Presets / copy-sync / batch  (needs P1 for masked-edit sync)
  P8  Print & soft-proofing        (needs P2)
  P9  Advanced DAM: cull/compare + HDR/pano merge
```

**Rationale (recorded 2026-07-05):** lead with the **masking backbone (P1)** — the most-requested
professional capability, the thing the Develop toolbar already gestures at, and the substrate P5/A2/
A3/P7 all build on — then the **quality foundation (P2)** the D1 promotion demands. Stand up **A0**
early (in parallel) so the AI features can interleave with classical phases rather than queue behind
all nine. The order is a recommendation, not a hard chain (except the prerequisites above); a spec
agent may reorder within the soft constraints as priorities shift.

---

## 7. Reference

- **v2 kickoff prompt:** `2026-07-05-v2-kickoff-prompt.md` — the authoritative starting brief
  (mandate §1, required map shape §2, candidate inventory §3, carried-forward discipline §4, the
  image-quality re-decision §5). This map is its first deliverable.
- **v1 architecture map:** `2026-06-28-ferrolite-v1-architecture-map.md` — the structural template;
  the five contracts (§5) and licensing tiers (§3) carried forward here.
- **Spec 4 map:** `2026-07-03-spec4-secondary-and-polish-map.md` — the decomposition-child template
  and the **libjxl (4.2) / Lensfun (4.4) build-gating precedents** the AI runtime (A0) follows.
- **Spec 1 (Speed core):** `2026-06-28-ferrolite-speed-core-design.md` — jobs/GPU/VT/decode/catalog.
- **Spec 2 (Editing):** `2026-06-30-spec2-editing-design.md` — the edit DAG, OpStack sidecar, **VT
  halo + GPU tile producer**, two-tier recompute — the foundation P1/P3/P4/P5 build on.
- **Spec 3 (Color & Export):** `2026-07-01-spec3-color-and-export-design.md` — color pipeline,
  swappable display tail, export core, `ColorProfile` decode product — the foundation P2/P8 build on.
- **Design system:** `../../design/ferrolite-design-system.md` — canonical theme/widget/layout for
  all v2 UI (mask overlays, healing canvas, curves/wheels, transform, print/soft-proof, compare, AI).
- **darktable-ai (GPL-3.0):** `github.com/darktable-org/darktable-ai` — the model/ONNX/licensing
  reference for the AI track (D5); read for models & policy, no code copied.
- **Original proposal:** `2026-06-28-ferrolite-proposal.md` — v1 goals/non-goals; the source v2
  departs from deliberately (esp. NG1 image-science, NG3 AI, NG4 tethered).
- RapidRAW (AGPL-3.0): read for ideas only; **no code copied** into this GPL-3.0 project.

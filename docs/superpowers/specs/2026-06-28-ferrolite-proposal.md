# Project Proposal: A Fast, Open-Source RAW Photo Editor in Rust

> **Archived source-of-truth proposal.** This is the original project proposal that
> seeded ferrolite. Its "Settled decisions" are fixed; its "Open questions" are resolved
> in the v1 architecture map (`2026-06-28-ferrolite-v1-architecture-map.md`). Kept in the
> repo so spec agents never depend on it living only in a chat transcript.

---

## 1. Summary

Build a cross-platform, open-source, multi-threaded **RAW photo editor and digital asset
manager (DAM)** written in Rust. Two reinforcing objectives:

1. **Be meaningfully faster than RawTherapee** at browsing a library and
   loading/previewing images.
2. **Serve as a deep learning vehicle for GPU, pipeline, and streaming architecture**
   that transfers directly to game-engine development.

Provides non-destructive RAW editing, a fast catalog-backed file browser, and export to
multiple formats. Explicitly **not** trying to match darktable, DxO, or Adobe on
image-science quality (demosaic, denoise, color accuracy).

## 2. Author context

- Experienced developer comfortable with Rust, Go, Python, systems programming, and ML.
- Concurrently building a **game engine**; wants reusable, transferable skills: wgpu/WGSL
  compute pipelines, texture streaming/tiling, retained dependency-graph computation with
  caching, threaded job systems.
- Hands-on, iterative, empirical working style; wants to write the architecturally
  interesting parts personally rather than wire together a black box.
- Both a "result matters" and "learning matters" project. Where they conflict, **preserve
  the learning** in the GPU/pipeline/streaming layers, and **buy/borrow** in the
  non-learning layers (RAW decoding, color management bindings).

## 3. Goals

### Primary
- **G1 — Browser speed.** Library browsing feels instantaneous on directories of thousands
  of RAWs, beating RawTherapee's perceived browse speed. SQLite-backed catalog +
  async thumbnail pipeline.
- **G2 — Load/preview speed.** Opening an image shows something near-instantly by decoding
  the embedded JPEG preview first, then rendering the full decode in the background.
  Full-resolution zoom/pan smooth on high-megapixel files (target: 45MP) via GPU tiling.
- **G3 — Non-destructive editing.** Edits stored as an ordered, replayable operation stack
  persisted to a sidecar; original never modified. Core adjustments: exposure, white
  balance, tone curve, contrast, HSL, crop/rotate.
- **G4 — Multi-format export.** JPEG, PNG, TIFF, WebP, and a modern format (AVIF and/or
  JPEG-XL), with correct EXIF/metadata propagation.
- **G5 — Transferable architecture.** GPU pipeline, tiling/streaming, dependency-graph
  recompute, and job system built as clean, reusable, engine-relevant subsystems.

### Secondary
- Cross-platform: Windows, macOS, Linux.
- Lens corrections (via Lensfun bindings).
- Color-managed output (ICC profiles).
- XMP sidecar compatibility.

## 4. Non-goals (do not scope in)

- **NG1 — State-of-the-art image science.** Basic demosaic and a correct-but-not-elite
  color path are acceptable. "RawTherapee speed with weaker image quality" is acceptable
  for a long time.
- **NG2 — Writing a RAW decoder.** Use `rawler`.
- **NG3 — AI features** (auto-tagging, AI masking, generative edits) — out for v1.
- **NG4 — Tethered shooting / studio capture** — out.
- **NG5 — Mobile** — out for v1 (desktop only).
- **NG6 — Cloud sync / online services** — out.

## 5. Settled technical decisions (fixed)

| Layer | Decision | Rationale |
|---|---|---|
| Language | Rust | Multi-threading, safety, engine-skill transfer. |
| Build fresh, not fork | New project; RapidRAW (AGPLv3) read as reference only. | Learning is a primary goal; inheriting Tauri+React would rob the author of the wgpu/pipeline/streaming work. |
| RAW decode | `rawler` (LGPL-2.1) | Maintained, broad coverage, de-facto rawloader successor. Pin versions. |
| GPU pipeline | `wgpu` + custom WGSL compute shaders | Production-grade; primary learning surface. |
| CPU parallelism | `rayon`; SIMD via `wide`/`pulp`/`std::simd` | Mature. |
| GUI | Pure-Rust GUI with a wgpu texture canvas: Slint or egui | Maximal canvas control, best engine-learning, no WebView. Avoid Tauri. |
| Catalog/DAM | SQLite via `rusqlite` (or `sqlx`) | darktable/digiKam model; indexed browse is key to G1. |
| Color management | `moxcms` (pure Rust) preferred; `lcms2` fallback | Pure-Rust where viable. |
| Thumbnails/resize | `fast_image_resize` (SIMD) | Throughput for G1. |
| Export encoders | `image`, `ravif`, `jpegxl-rs` | Viable today. |
| Metadata | `kamadak-exif` (read) + `little_exif` (read/write); hand-rolled XMP | Avoid GPL `rexiv2`. |
| Lens correction | Lensfun bindings | Pragmatic; secondary-goal, deferrable. |

**Licensing note:** `rawler` LGPL-2.1, `imagepipe` LGPL-3.0, RapidRAW + `dng` crate
AGPL-3.0 (cannot copy into non-AGPL project — read for ideas only), `rexiv2` GPL.
Permissive deps (`wgpu`, `moxcms`, `kamadak-exif`) keep options open.

## 6. Accepted tradeoffs

- Image quality secondary to speed and architecture. No scope creep toward color/denoise parity.
- Some deps are C-library bindings (lcms2, jpegxl-rs, Lensfun). Pure-Rust is not a hard requirement.
- `rawler` will lack some new cameras; contribute samples upstream, don't fork.
- Parity with darktable's OpenCL pipeline is not a target. Target = beating RawTherapee
  on browse/load (an I/O and caching problem).

## 7. Proposed architecture (high level)

```
                 ┌─────────────────────────────────────────────┐
                 │                   GUI layer                   │
                 │     Slint / egui  +  wgpu texture canvas      │
                 │  (panels, sliders, histogram, zoom/pan view)  │
                 └───────────────┬───────────────────────────────┘
                                 │ commands / state
        ┌────────────────────────┼───────────────────────────────┐
        │                        │                                │
┌───────▼────────┐   ┌───────────▼───────────┐      ┌─────────────▼───────────┐
│  Catalog/DAM   │   │   Edit pipeline (GPU)  │      │     Job system          │
│  SQLite        │   │   wgpu + WGSL passes   │      │  rayon worker pools:    │
│  - folders     │   │   - retained DAG       │      │  - async decode         │
│  - metadata    │   │   - dirty-flag recompute│     │  - thumbnail gen        │
│  - thumbnails  │   │   - tiling for >VRAM   │      │  - export               │
└───────┬────────┘   └───────────┬───────────┘      └─────────────┬───────────┘
        │                        │                                │
        └────────────┬───────────┴────────────────┬──────────────┘
                     │                             │
            ┌────────▼─────────┐         ┌─────────▼──────────┐
            │  Decode (rawler) │         │  Export encoders   │
            │  + embedded JPEG │         │ image/ravif/jpegxl │
            │  preview path    │         │ + metadata write   │
            └──────────────────┘         └────────────────────┘
```

Key ideas to preserve: two-tier load (embedded-preview-first, full-decode-in-background);
retained pipeline DAG with caching (recompute only downstream of a changed edit); GPU
tiling for images exceeding VRAM; threaded job system (decode/thumbnail/export).

## 8. Suggested phasing

Validation milestone (proves both primary goals at once):
> rawler decode → instant embedded-preview display → SQLite catalog with async-generated
> thumbnails → smooth GPU zoom/pan on a 45MP image with tiling.

- **Phase 0 — Foundations & spikes.** GUI shell with wgpu canvas; confirm rawler decodes
  target cameras; confirm GUI wgpu texture integration; choose license. Gate:
  GUI-renders-a-wgpu-texture works on all three OSes.
- **Phase 1 — Speed core (G1 + G2).** SQLite catalog + folder ingest; embedded-preview-first
  load; async thumbnail pipeline; GPU zoom/pan with tiling on 45MP. Benchmark vs RawTherapee.
- **Phase 2 — Non-destructive editing (G3).** Retained pipeline DAG; core WGSL edit passes;
  sidecar persistence (replayable op stack + XMP).
- **Phase 3 — Export & color (G4).** Export to all formats; metadata propagation;
  color-managed output via moxcms (lcms2 fallback).
- **Phase 4 — Secondary & polish.** Lens corrections (Lensfun); perf tuning; UX polish;
  broader camera coverage.

## 9. Open questions for the roadmap

1. GUI: Slint vs egui.
2. Edit-stack persistence format — XMP vs custom sidecar vs both.
3. Color pipeline definition — working color space; camera-matrix → XYZ → working → display;
   where ICC transforms sit relative to GPU passes.
4. Tiling strategy — tile size, overlap for neighborhood ops, cache eviction.
5. DAG granularity — per-operation vs per-region invalidation; preview-res vs full-res recompute.
6. Project license — final choice, verified against all dependency licenses.
7. Benchmark methodology — RawTherapee comparison setup (dataset, cameras, hardware, metrics).
8. Testing strategy — validating correctness of GPU passes (golden-image diffs, reference renders).

## 10. What "done for v1" means

A desktop app on Windows/macOS/Linux that browses a multi-thousand-image library faster
than RawTherapee; opens any supported RAW with an instant preview and smooth 45MP zoom/pan;
applies a core set of non-destructive edits persisted to sidecars; and exports to multiple
modern formats with correct metadata — built on a wgpu pipeline, tiling/streaming layer,
retained-DAG recompute, and a threaded job system the author can carry into game-engine work.

# Full-Res Pyramid Build Off the UI Thread — Design

> **Status:** Approved design (2026-07-08). A focused performance fix, not a phase.
> **Next step:** `superpowers:writing-plans` → implementation plan → subagent execution.

## 1. Problem

Opening an image freezes the UI for ~1 second. Root cause (traced, confirmed):

The tier-2 `FullDecoded` handler builds the full-resolution GPU pyramid **inline on the
UI thread** ([`ferrolite-app/src/app.rs:1010`](../../../ferrolite-app/src/app.rs)):

```rust
let pyramid = Arc::new(ferrolite_pipeline::GpuPyramidSource::new(&gpu, image));
```

`GpuPyramidSource::new` ([`ferrolite-pipeline/src/gpu_pyramid.rs:20`](../../../ferrolite-pipeline/src/gpu_pyramid.rs))
CPU box-downsamples the full-res image across every mip level, then uploads each as a
texture. For a multi-megapixel RAW this is hundreds of ms to ~1 s of synchronous CPU work
running inside `update()` on the UI thread — a direct violation of CLAUDE.md responsiveness
rule 1 ("never block the UI/update thread; multi-millisecond CPU work MUST be submitted to
`ferrolite-jobs`").

The color-correct rung-1 preview is **already revealed** at that point, so this work has no
reason to block interactivity.

**Pre-existing, not a regression** from the P3 color-grading work: the handler and
`GpuPyramidSource::new` are unchanged by it. The **export path already does the right
thing** — [`ferrolite-app/src/export/mod.rs:100`](../../../ferrolite-app/src/export/mod.rs)
runs the *same* `GpuPyramidSource::new` on a `ferrolite-jobs` worker ("never the UI
thread"). The interactive open path simply never adopted that pattern.

## 2. Goal / non-goals

- **Goal:** the UI stays interactive on image open; the full-res pyramid builds on a worker
  thread and installs when ready. No freeze.
- **Non-goal:** changing decode, the VT, the pipeline/shaders, `ferrolite-gpu`, or the export
  path. No new dependencies. Not addressing any other open-time cost beyond the pyramid build.

## 3. Hard constraint that shapes the design

- `GpuPyramidSource` is **`Send`** — its levels are `Arc<Texture>` (wgpu textures are
  `Send + Sync`). It can be built on a worker and moved to the UI thread.
- `TileEditPipeline` is **`!Send`** — it uses `Rc` internally (interior-mutable uniform/LUT
  cells and shared nodes, 29 sites). It **must** be built on the UI thread.

Therefore: move **only** the pyramid build off-thread. The tile-pipeline + producer build
stays on the UI thread (it is cheap once shader modules are pre-warmed at startup — it only
creates compute-pipeline objects, not shader compiles).

## 4. Design

### 4.1 New app event

Add one variant to `AppEvent` ([`ferrolite-app/src/events.rs`](../../../ferrolite-app/src/events.rs)):

```rust
/// A full-res GPU pyramid finished building off-thread (tier-2 open path).
/// Carries the ready pyramid for install on the UI thread. Handled in `app.rs`
/// (needs GPU render state + the `Rc`-based tile pipeline), not folded by `apply`.
PyramidReady {
    image_id: i64,
    pyramid: std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>,
},
```

### 4.2 `FullDecoded` handler (UI thread) — submit instead of build

Keep everything the handler does today **except** the inline pyramid build. In its place:

- Create the `Arc<GpuContext>` on the UI thread (`GpuContext::from_render_state(rs)` — cheap)
  and share the full-res image as `Arc<LinearRgbaF32>` (no full-buffer copy).
- Submit a **Background** `ferrolite-jobs` job (with cancel token) that runs
  `GpuPyramidSource::new(&gpu, &image)` and sends `AppEvent::PyramidReady { image_id, pyramid }`
  then `request_repaint()`.
- Leave the sparse full VT **not producing** (do NOT call `set_producing(true)` here). During
  the gap only the already-revealed color-correct preview shows — this preserves the existing
  invariant that the full VT must pass through the edit producer (camera→working), never the
  raw camera-native path reaching the working→display tail.

The reveal (`v.loaded = true`, `v.full_ready = true`, fit-to-dims, VT install) is unchanged —
it happens on `FullDecoded` as today.

### 4.3 `PyramidReady` handler (UI thread) — install + build producer

New match arm in the `app.rs` event loop:

- **Staleness guard:** if `image_id != v.image_id`, drop it (the user navigated away). The job
  also carries a cancel token so a superseded open stops early.
- Install `v.pyramid = Some(pyramid.clone())`.
- Build `GpuContext::from_render_state(rs)`, `TileEditPipeline::new(...)` (threading the
  current lens bake / mode-aware vignette pair exactly as the current handler does),
  `EditTileProducer::new(tep)`, set vignette amount/manual, and `v.edit_producer = Some(...)`.
- Then set the VT producing + opstack version (the `set_producing(true)` / `set_opstack_version`
  block currently at the tail of `FullDecoded`).

This is a near-verbatim move of ~15 lines from `FullDecoded` into `PyramidReady`; the only new
logic is the staleness guard and reading the pyramid from the event instead of building it.

### 4.4 Cancellation / correctness

- Background priority + cancel token: navigation cancels the superseded pyramid job (same
  convention as every other viewer job).
- `PyramidReady` is idempotent-safe via the `image_id` guard; a late result for a
  now-closed/other image is ignored.
- An image opened **with** persisted edits behaves identically: its edited rung-1 *preview*
  (built by the separate preview `EditPipeline`, unaffected) shows during the gap; full-res
  edited tiles stream in when the pyramid lands.

## 5. Testing

This is eframe/GPU threading glue: it needs the live render state and a real decode, so it is
not meaningfully unit-testable. Automated coverage is limited to what is pure — e.g. the new
`AppEvent::PyramidReady` variant compiles into the enum and the staleness-guard predicate — but
the **real gate is a hands-on visual test** (CLAUDE.md): open a large RAW and confirm (a) the UI
does not freeze on open, (b) the app is immediately interactive with the color-correct preview
shown, and (c) full-res sharpness resolves a beat later; plus edited-on-open and
rapid-navigation (open A then immediately B — no stale pyramid installs) cases.

## 6. Scope summary

| Change | File |
|---|---|
| `PyramidReady` event variant | `ferrolite-app/src/events.rs` |
| `FullDecoded`: submit pyramid job instead of inline build; leave VT not-producing | `ferrolite-app/src/app.rs` |
| `PyramidReady` handler: install pyramid + build tile pipeline/producer + set producing | `ferrolite-app/src/app.rs` |
| (job body) `GpuPyramidSource::new` on a worker | `ferrolite-app/src/app.rs` (or a small `viewer` helper) |

No changes to `ferrolite-pipeline`, `ferrolite-gpu`, `ferrolite-vt`, decode, shaders, or the
export path. No new dependencies.

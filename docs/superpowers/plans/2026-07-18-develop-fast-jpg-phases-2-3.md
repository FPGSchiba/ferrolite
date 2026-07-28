# Develop Fast-JPG Perceived Speed (Phases 2–3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make opening an image in Develop *feel* instant for both RAW and JPG — reveal the already-decoded grid thumbnail immediately and crossfade to the real render (Phase 2), and give JPGs the same on-disk 2048px preview cache RAW already has so re-opens reveal fast (Phase 3).

**Architecture:** Phase 2 adds a **Tier-0 placeholder** entirely in the egui layer: on open, the resident grid-thumbnail texture (`AppState.textures`) is drawn upscaled-to-fit to replace the black canvas + spinner instantly; when the real reveal lands (`v.loaded` flips true) the placeholder is drawn once more *over* the wgpu canvas with a short, timed alpha ramp (aligned to the revealed image via `viewer::image_screen_rect`) so it fades out with no pop. This reuses the crossfade *pattern* (a bounded ramp, like `crossfade_elapsed`) without touching the GPU blit path in `callback.rs`. Phase 3 extends the existing RAW preview-cache machinery (`develop::preview_cache`) to `FileKind::Standard`: `should_write_back` becomes format-agnostic, the Standard reveal writes its display-linear-sRGB `preview_source` back as a 2048px JPEG with an `identity()` display matrix, and Standard's full-JPEG decode is gated behind a cache read (mirroring RAW's full-decode gate) so a re-open reveals the 2048px entry from disk first. Phase 2's instant thumbnail is what keeps the now-debounced Standard cold-open feeling instant.

**Tech Stack:** Rust, egui/eframe 0.29.1, wgpu, existing `ferrolite-app` viewer + `develop::preview_cache`, `ferrolite-previews` (2048px sRGB JPEG store), `ferrolite-color` (`identity()`).

## Global Constraints

- **Scope:** all changes are in crate `ferrolite-app` (viewer + develop modules). `ferrolite-previews` / `ferrolite-color` are consumed unchanged. **Scoped gate = `ferrolite-app` only.**
- **Toolchain:** the coordinator runs `rustup update stable` before the end-of-branch repo gate; fix code forward-compatibly, never pin to dodge a newer lint. (CLAUDE.md "Toolchain".)
- **Newest-stable float-literal lint:** suffix every new egui/GPU float literal `_f32` (e.g. `Stroke::new(1.0_f32, …)`, `egui::vec2(8.0_f32, 8.0_f32)`, alpha `0.5_f32`) or `float-literal-f32-fallback` reddens CI. (CLAUDE.md "Toolchain"; commit 98f0576.)
- **Scoped gate for each task:** `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`. The coordinator runs the repo gate once at end of branch.
- **Threading (load-bearing):** never block the UI thread. No new decode/encode/IO on the UI thread — the only new job is the Standard preview-cache write (already a `Background` job via `spawn_cache_write`). (CLAUDE.md rule 1.)
- **GPU (load-bearing):** build pipelines once; Phase 2 adds NO new GPU pipeline and NO per-frame GPU allocation — the placeholder is an egui textured rect over the existing canvas. (CLAUDE.md rule 2.)
- **Icons:** no new icons. The spinner already comes from egui; the placeholder is an image, not an icon. No raw emoji/symbols. (CLAUDE.md "UI icons".)
- **No per-control-reset / keybind surface changes** — this feature adds no adjustable control and no keybind, so those load-bearing rules do not apply here.
- **Line width 100, rustfmt defaults, 4-space indent; `-D warnings` clippy.**

## Non-goals (explicitly out of scope — decided 2026-07-18)

- **No `develop::cache` RAM-LRU / warm-RAM neighbor reuse.** That is deferred Phase-1 work gated on measurement; not built here. Phase 3 is the **disk path only**.
- **No f16 footprint trim.** Full-res buffers stay f32; that is a separate memory branch.
- No second JPG pipeline (JPGs stay first-class on the unified pipeline; spec-locked).
- No changes to `callback.rs` GPU compositing, no changes to the RAW embedded-preview ordering beyond what Phase 3 Task 6 explicitly restructures.

---

## File Structure

- **Modify `ferrolite-app/src/viewer/mod.rs`** — add Tier-0 placeholder state + pure ramp helpers to `ViewerState`; add a pure `fit_rect` helper; thread an `Option<&egui::TextureHandle>` thumbnail into `paint` and draw the placeholder (`!loaded`) + fade (`loaded`).
- **Modify `ferrolite-app/src/app.rs`** — resolve the resident thumbnail handle at the `viewer::paint` call site (`drive_viewer`, ~L2419) and pass it in; keep the repaint loop alive while the Tier-0 fade advances (~L2423); Phase 3: gate Standard's full-JPEG decode behind a cache read in the open flow (~L3943) and hook the Standard preview-cache write-back into the reveal (`apply_preview_ready`, ~L256 / the write-back block ~L1122).
- **Modify `ferrolite-app/src/develop/preview_cache.rs`** — make `should_write_back` format-agnostic (drop `is_raw`); add `standard_writeback_matrix()` + a round-trip color test; update the module doc + existing `should_write_back` test.

No new files: both phases extend existing focused modules that already own this logic.

---

# Phase 2 — Tier-0 instant reveal

## Task 1: Tier-0 placeholder state + pure fade-ramp on `ViewerState`

**Files:**
- Modify: `ferrolite-app/src/viewer/mod.rs`

**Interfaces:**
- Produces:
  - `const TIER0_FADE_SECS: f32` — the placeholder fade-out duration.
  - `ViewerState.tier0_fading: bool`, `ViewerState.tier0_elapsed: f32` (new fields, default `false` / `0.0` in `open`).
  - `ViewerState::begin_tier0_fade(&mut self)` — start the fade ramp.
  - `ViewerState::tick_tier0_fade(&mut self, dt: f32) -> f32` — advance and return the placeholder opacity in `[0,1]` (1 → 0); clears `tier0_fading` at the end.
  - `fn tier0_fade_alpha(elapsed: f32) -> f32` — pure: `(1 - elapsed/TIER0_FADE_SECS).clamp(0,1)`.

- [ ] **Step 1: Write the failing test**

In `ferrolite-app/src/viewer/mod.rs`, inside the existing `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn tier0_fade_alpha_ramps_from_one_to_zero() {
    assert_eq!(tier0_fade_alpha(0.0), 1.0, "full opacity at start");
    assert_eq!(tier0_fade_alpha(TIER0_FADE_SECS), 0.0, "transparent at end");
    assert_eq!(tier0_fade_alpha(TIER0_FADE_SECS * 2.0), 0.0, "clamped past end");
    let mid = tier0_fade_alpha(TIER0_FADE_SECS * 0.5);
    assert!((mid - 0.5).abs() < 1e-6, "linear midpoint, got {mid}");
}

#[test]
fn tick_tier0_fade_advances_and_terminates() {
    let mut v = ViewerState::open(1, std::path::PathBuf::from("x.jpg"), FileKind::Standard);
    assert!(!v.tier0_fading, "not fading until begun");
    v.begin_tier0_fade();
    assert!(v.tier0_fading);
    // Half way: opacity ~0.5, still fading.
    let a = v.tick_tier0_fade(TIER0_FADE_SECS * 0.5);
    assert!((a - 0.5).abs() < 1e-6, "got {a}");
    assert!(v.tier0_fading);
    // Past the end: opacity 0, fading cleared.
    let a = v.tick_tier0_fade(TIER0_FADE_SECS);
    assert_eq!(a, 0.0);
    assert!(!v.tier0_fading, "fade terminates so the repaint loop can idle");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app viewer::tests::tier0 2>&1 | tail -20`
Expected: FAIL — `tier0_fade_alpha` / `begin_tier0_fade` / fields not found.

- [ ] **Step 3: Add the constant + fields + methods + helper**

Near the top of `viewer/mod.rs`, next to `CROSSFADE_SECS` (~L21), add:

```rust
/// Tier-0 placeholder (upscaled grid thumbnail) fade-out duration (seconds).
/// Short enough to read as an instant sharpen, long enough to hide the pop from
/// the thumbnail's lower resolution as the real reveal lands.
pub const TIER0_FADE_SECS: f32 = 0.18;
```

In `struct ViewerState`, next to the crossfade fields (`crossfading` / `crossfade_elapsed`, ~L104–119), add:

```rust
    /// True while the Tier-0 placeholder (the upscaled grid thumbnail shown at
    /// open, before the real reveal) is fading out over the revealed image.
    /// Begun when `loaded` first flips true; cleared when the ramp completes.
    pub tier0_fading: bool,
    /// Seconds elapsed into the active Tier-0 placeholder fade.
    pub tier0_elapsed: f32,
```

In `ViewerState::open` (~L296), initialise them next to `crossfading: false, … crossfade_elapsed: 0.0,`:

```rust
            tier0_fading: false,
            tier0_elapsed: 0.0,
```

Add the pure helper (free function, near `tick_crossfade` / after the `impl ViewerState` block that holds it, or alongside the other pure fns):

```rust
/// Tier-0 placeholder opacity for a given elapsed time: ramps linearly from 1.0
/// (fully covering the just-revealed image) to 0.0 over `TIER0_FADE_SECS`, then
/// stays 0. Pure — unit-tested; the draw + ramp advance live in `paint`.
pub fn tier0_fade_alpha(elapsed: f32) -> f32 {
    (1.0 - elapsed / TIER0_FADE_SECS).clamp(0.0, 1.0)
}
```

Add the two methods to `impl ViewerState` (next to `begin_crossfade` / `tick_crossfade`):

```rust
    /// Begin the Tier-0 placeholder fade (called once, when the real reveal
    /// first sets `loaded = true`). No-op semantics if called again mid-fade are
    /// avoided by the caller's one-shot guard in `paint`.
    pub fn begin_tier0_fade(&mut self) {
        self.tier0_fading = true;
        self.tier0_elapsed = 0.0;
    }

    /// Advance the Tier-0 fade by `dt` and return the current placeholder opacity
    /// in `[0,1]`. Clears `tier0_fading` once the ramp completes so the repaint
    /// loop can idle. Returns 0.0 when not fading.
    pub fn tick_tier0_fade(&mut self, dt: f32) -> f32 {
        if !self.tier0_fading {
            return 0.0;
        }
        self.tier0_elapsed += dt;
        let alpha = tier0_fade_alpha(self.tier0_elapsed);
        if alpha <= 0.0 {
            self.tier0_fading = false;
        }
        alpha
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app viewer::tests::tier0 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 5: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app viewer::
git add ferrolite-app/src/viewer/mod.rs
git commit -m "feat(develop): Tier-0 placeholder fade state + pure ramp on ViewerState"
```

---

## Task 2: `fit_rect` helper + draw the thumbnail placeholder in the `!loaded` branch

**Files:**
- Modify: `ferrolite-app/src/viewer/mod.rs`
- Modify: `ferrolite-app/src/app.rs` (the `viewer::paint` call site, ~L2419)

**Interfaces:**
- Consumes: `AppState.textures` (a `TextureCache` with `get(id) -> Option<&egui::TextureHandle>`), `viewer::image_screen_rect` (existing).
- Produces:
  - `fn fit_rect(canvas: egui::Rect, content: (f32, f32)) -> egui::Rect` — pure: the aspect-preserving centered sub-rect of `canvas` that fits a `content`-sized image (letterboxed). Used for the placeholder before `image_dims` is known.
  - `paint`'s signature gains a trailing `tier0_thumb: Option<&egui::TextureHandle>` parameter.

- [ ] **Step 1: Write the failing test**

Add to `viewer/mod.rs`'s `#[cfg(test)] mod tests`:

```rust
#[test]
fn fit_rect_letterboxes_wide_image_in_tall_canvas() {
    // 200x100 canvas, 200x200 content -> width-limited, centered vertically.
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 100.0));
    let r = fit_rect(canvas, (200.0, 200.0));
    // Square content in a 2:1 canvas fits to height 100 -> 100x100, centered.
    assert!((r.width() - 100.0).abs() < 1e-3, "w={}", r.width());
    assert!((r.height() - 100.0).abs() < 1e-3, "h={}", r.height());
    assert!((r.center().x - 100.0).abs() < 1e-3, "centered x");
    assert!((r.center().y - 50.0).abs() < 1e-3, "centered y");
}

#[test]
fn fit_rect_letterboxes_tall_image_in_wide_canvas() {
    let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 100.0));
    // 100x200 content in 200x100 canvas -> height-limited to 100 -> 50x100.
    let r = fit_rect(canvas, (100.0, 200.0));
    assert!((r.width() - 50.0).abs() < 1e-3, "w={}", r.width());
    assert!((r.height() - 100.0).abs() < 1e-3, "h={}", r.height());
    assert!((r.center().x - 100.0).abs() < 1e-3, "centered x");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app viewer::tests::fit_rect 2>&1 | tail -20`
Expected: FAIL — `fit_rect` not found.

- [ ] **Step 3: Implement `fit_rect`**

Add near `image_screen_rect` in `viewer/mod.rs`:

```rust
/// Aspect-preserving, centered sub-rect of `canvas` that a `content`-sized image
/// occupies when fit (letterboxed) — the same framing `ViewTransform::fit`
/// produces, but computed directly from sizes for the Tier-0 placeholder shown
/// before the real `image_dims` are known. Pure (no GPU/egui state).
pub fn fit_rect(canvas: egui::Rect, content: (f32, f32)) -> egui::Rect {
    let (cw, ch) = (content.0.max(1.0), content.1.max(1.0));
    let scale = (canvas.width() / cw).min(canvas.height() / ch);
    let (w, h) = (cw * scale, ch * scale);
    egui::Rect::from_center_size(canvas.center(), egui::vec2(w, h))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app viewer::tests::fit_rect 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Thread the thumbnail into `paint` and draw the placeholder**

In `paint` (~L515), add the trailing parameter:

```rust
pub fn paint(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    full_ready: bool,
    front_valid: bool,
    crossfade: f32,
    interactive: bool,
    tier0_thumb: Option<&egui::TextureHandle>,
) -> (bool, PresentSource) {
```

Replace the `!state.loaded` else-branch body (the spinner block, ~L603–622) so a resident thumbnail is drawn upscaled-to-fit *behind* the spinner (spinner stays so a thumbnail-less open still reads as loading):

```rust
    } else {
        // Tier-0: if the grid thumbnail for this image is already decoded, draw
        // it upscaled-to-fit so the open shows the picture instantly instead of a
        // black canvas. The real reveal replaces it (and Task 3 fades this out).
        if let Some(thumb) = tier0_thumb {
            let sz = thumb.size();
            let dst = fit_rect(rect, (sz[0] as f32, sz[1] as f32));
            ui.painter().image(
                thumb.id(),
                dst,
                egui::Rect::from_min_max(egui::pos2(0.0_f32, 0.0_f32), egui::pos2(1.0_f32, 1.0_f32)),
                egui::Color32::WHITE,
            );
        }
        // First pixel not ready yet: keep a spinner + "Loading…" so the wait reads
        // as working, and keep animating so we pick up the reveal as soon as it
        // arrives. Over a thumbnail the spinner is a subtle "sharpening" hint.
        let center = rect.center();
        let spinner_size = 32.0_f32;
        let spinner_rect = egui::Rect::from_center_size(
            center - egui::vec2(0.0_f32, 10.0_f32),
            egui::vec2(spinner_size, spinner_size),
        );
        ui.put(spinner_rect, egui::Spinner::new().size(spinner_size));
        ui.painter().text(
            center + egui::vec2(0.0_f32, 22.0_f32),
            egui::Align2::CENTER_CENTER,
            "Loading\u{2026}",
            egui::FontId::proportional(12.0),
            crate::theme::TEXT_DIM,
        );
        (true, source)
    }
```

- [ ] **Step 6: Update the `paint` call site to resolve + pass the thumbnail**

In `app.rs` `drive_viewer`, the block around L2412–2419 borrows `v` (the viewer) mutably for `paint`. Resolve the thumbnail handle from `self.state.textures` and clone it (an `egui::TextureHandle` is a cheap `Arc`-backed handle) **before** the `paint` borrow, so the `textures` borrow is released first. Replace:

```rust
        let (image_id, view, viewport, split_pos) = (v.image_id, v.view, v.viewport, v.split_pos);
```
…and the `viewer::paint(...)` call (~L2418–2419) with:

```rust
        let (image_id, view, viewport, split_pos) = (v.image_id, v.view, v.viewport, v.split_pos);

        // Tier-0 placeholder: the resident grid thumbnail for this image (if any),
        // cloned out of the texture cache BEFORE the `viewer::paint` borrow of `v`
        // so the `self.state.textures` borrow is released first. A `TextureHandle`
        // clone is a cheap refcount bump (no pixel copy).
        let tier0_thumb = self.state.textures.get(image_id).cloned();

        // `paint` applies this frame's pan/zoom and clears `idle` when the view
        // moved, so read `idle` AFTER it to catch an interaction this frame. It
        // also folds this frame's `interacting` into the present source and returns
        // the chosen source so the repaint gate can keep the loop alive mid-fade.
        let (loading_preview, present_source) =
            viewer::paint(ui, v, full_ready, front_valid, factor, interactive, tier0_thumb.as_ref());
```

> Note: `TextureCache::get` takes `&mut self`; `self.state.textures` is accessible here because `v` was obtained as `self.state.viewer.as_mut()` earlier in `drive_viewer`. If the borrow checker rejects the interleave, hoist the `tier0_thumb` resolution to the top of the `drive_viewer` frame body (before `v` is borrowed) keyed by the already-known `self.state.viewer.as_ref().map(|v| v.image_id)`, and thread it down as a local. Do NOT clone pixels — only the handle.

- [ ] **Step 7: Update any other `viewer::paint` callers**

Run: `grep -rn "viewer::paint(" ferrolite-app/src` → there is a single production call site (drive_viewer). If a test or other caller exists, pass `None` as the new final argument.

- [ ] **Step 8: Build + scoped gate**

Run: `cargo build -p ferrolite-app 2>&1 | tail -20 && cargo test -p ferrolite-app viewer:: 2>&1 | tail -10`
Expected: compiles; viewer tests pass.

- [ ] **Step 9: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/viewer/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): Tier-0 instant reveal — draw resident grid thumbnail before decode"
```

---

## Task 3: Fade the placeholder out over the revealed image (no pop)

**Files:**
- Modify: `ferrolite-app/src/viewer/mod.rs` (`paint`)
- Modify: `ferrolite-app/src/app.rs` (keep the repaint loop alive while fading, ~L2423–2450)

**Interfaces:**
- Consumes: `ViewerState::begin_tier0_fade` / `tick_tier0_fade` (Task 1), `viewer::image_screen_rect` (existing), `viewer::fit_rect` (Task 2).
- Produces: no new public API — the fade is drawn inside `paint`; `drive_viewer` gains `v.tier0_fading` in its repaint gate.

- [ ] **Step 1: Draw the fade in `paint`'s `loaded` branch**

The fade needs a one-shot "loaded just became true" trigger. Add a private per-frame trigger using the existing `tier0_elapsed == 0.0 && !tier0_fading` plus a new `tier0_started` guard is overkill — instead drive it off `loaded`: track that the fade begins the first frame `loaded` is true AND a placeholder was shown. Add a field to `ViewerState` (next to the Task 1 fields):

```rust
    /// One-shot: set true once the Tier-0 fade has been kicked off for this open,
    /// so the `loaded`-edge trigger in `paint` fires exactly once.
    pub tier0_started: bool,
```
Initialise `tier0_started: false` in `open`.

In `paint`, in the `if state.loaded { … }` branch (the branch that adds the wgpu `Callback`, ~L591–602), AFTER `ui.painter().add(egui_wgpu::Callback::new_paint_callback(...))` and before `(false, source)`, insert the fade draw:

```rust
        // Tier-0 fade-out: the first frame the real reveal is `loaded`, start a
        // short ramp; while it runs, draw the grid thumbnail once more OVER the
        // wgpu canvas at decreasing opacity, aligned to where the revealed image
        // sits, so the lower-res placeholder dissolves into the sharp render with
        // no hard pop. Only meaningful when a placeholder was actually shown
        // (a thumbnail exists) — otherwise the ramp is a no-op fade of nothing.
        if let Some(thumb) = tier0_thumb {
            if !state.tier0_started {
                state.tier0_started = true;
                state.begin_tier0_fade();
            }
            if state.tier0_fading {
                let dt = ui.input(|i| i.stable_dt);
                let alpha = state.tick_tier0_fade(dt);
                if alpha > 0.0 {
                    // Align to the revealed image rect when dims are known
                    // (seamless), else fall back to the fit letterbox.
                    let dst = match state.image_dims {
                        Some(dims) => image_screen_rect(rect, dims, state.view, viewport),
                        None => {
                            let sz = thumb.size();
                            fit_rect(rect, (sz[0] as f32, sz[1] as f32))
                        }
                    };
                    let tint = egui::Color32::from_white_alpha((alpha * 255.0) as u8);
                    ui.painter().image(
                        thumb.id(),
                        dst,
                        egui::Rect::from_min_max(
                            egui::pos2(0.0_f32, 0.0_f32),
                            egui::pos2(1.0_f32, 1.0_f32),
                        ),
                        tint,
                    );
                    ui.ctx().request_repaint();
                }
            }
        }
```

> Why `image_screen_rect` here vs `fit_rect` in Task 2: in the `!loaded` placeholder we do not yet have `image_dims`, so we letterbox by the thumbnail's own aspect (`fit_rect`). Once revealed, `image_dims` is set and the image may be fit OR zoomed/panned, so we align the fading thumbnail to the exact on-screen image rect with `image_screen_rect` (the same mapping the display shader uses) — that keeps the dissolve registered pixel-for-pixel.

- [ ] **Step 2: Keep the repaint loop alive while the fade advances**

In `drive_viewer` (`app.rs`), the repaint gate (~L2443–2450) currently repaints while `loading_preview || crossfading || crossfading_present || …`. Add the Tier-0 fade so the ramp animates to completion even when the image is otherwise idle. Find the condition that decides whether to `ctx.request_repaint()` (around L2446–2449) and add `|| v.tier0_fading`:

```rust
            if loading_preview
                || crossfading
                || crossfading_present
                || v.tier0_fading
                // …existing terms unchanged…
            {
                ctx.request_repaint();
            }
```

> The `paint` fade branch also calls `ui.ctx().request_repaint()` itself while `alpha > 0`, so this is belt-and-suspenders; keep both — the gate term makes the intent explicit and covers the frame where `paint` computed `alpha` but the gate is evaluated on the stored `v.tier0_fading`.

- [ ] **Step 3: Build + reason about correctness**

Run: `cargo build -p ferrolite-app 2>&1 | tail -20 && cargo test -p ferrolite-app viewer:: 2>&1 | tail -10`
Expected: compiles; viewer tests still pass (the ramp math is covered by Task 1; the draw is visual).

- [ ] **Step 4: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/viewer/mod.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): crossfade Tier-0 placeholder out over the reveal (no pop)"
```

---

# Phase 3 — JPG fast re-open (disk preview cache, disk path only)

## Task 4: Make `should_write_back` format-agnostic (RAW + Standard)

**Files:**
- Modify: `ferrolite-app/src/develop/preview_cache.rs`

**Interfaces:**
- Produces: `should_write_back(op_stack: &OpStack, is_cache_miss: bool) -> bool` (the `is_raw` parameter is REMOVED). Now: `*op_stack == OpStack::default() && is_cache_miss`.
- Consumers to update: `apply_full_decoded` (RAW write-back call, `app.rs:1129`).

- [ ] **Step 1: Update the failing test to the new signature + Standard case**

In `preview_cache.rs`, replace the `write_back_only_for_raw_default_stack_on_miss` test with:

```rust
    #[test]
    fn write_back_gated_on_default_stack_and_miss_for_any_kind() {
        let default_stack = OpStack::default();
        let edited_stack = default_stack.set_op(ferrolite_pipeline::Op::Exposure(
            ferrolite_pipeline::Exposure { ev: 0.5 },
        ));

        // Default stack + cache MISS -> write back (the only qualifying case).
        // Now format-agnostic: JPGs are first-class originals and cache the same
        // way RAWs do (spec: JPG Tier-1 write-back).
        assert!(should_write_back(&default_stack, true));
        // Default stack + cache HIT -> SKIP: the entry already exists on disk, so
        // re-encoding it is pure waste.
        assert!(!should_write_back(&default_stack, false));
        // Edited stack -> SKIP (guard: an identity render under an edited key
        // would reveal the wrong image), regardless of the miss flag.
        assert!(!should_write_back(&edited_stack, true));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app develop::preview_cache::tests::write_back 2>&1 | tail -20`
Expected: FAIL — `should_write_back` still takes 3 args / arity mismatch.

- [ ] **Step 3: Change the signature + doc**

Replace `should_write_back` (~L88–100):

```rust
/// Whether an open should write its render back to the preview cache.
///
/// Two conditions must both hold (format-agnostic — RAW and Standard/JPG are
/// treated equally, per the tiered-cache design: JPGs are first-class camera
/// originals, not quick looks):
/// * **default op stack** — the payload encoded is the *identity* render but the
///   key hashes the *actual* stack, so caching under an edited key would later
///   reveal the wrong image (see the module-level correctness guard).
/// * **cache miss** (`is_cache_miss`) — a cache *hit* already has the entry on
///   disk, so re-encoding it would be pure waste. The read path threads the real
///   miss flag here (`v.cache_write_back`).
pub fn should_write_back(op_stack: &OpStack, is_cache_miss: bool) -> bool {
    *op_stack == OpStack::default() && is_cache_miss
}
```

Also update the module-level doc (~L1–17): the sentence "on a qualifying **RAW** open" → "on a qualifying open (RAW or Standard)". Keep the identity-render correctness guard text unchanged.

- [ ] **Step 4: Update the RAW caller in `app.rs`**

In `apply_full_decoded` (`app.rs:1122–1135`), the write-back guard calls `should_write_back(is_raw, &v.op_stack, v.cache_write_back)`. Drop the `is_raw` argument:

```rust
                crate::develop::preview_cache::should_write_back(&v.op_stack, v.cache_write_back)
                    .then(|| (v.path.clone(), v.op_stack.clone()))
```

The surrounding `is_raw`-gated reveal logic in `apply_full_decoded` is unchanged (Standard never reaches `apply_full_decoded`; its write-back is added in Task 6). The `is_raw` local is still used elsewhere in that function, so leave it defined.

- [ ] **Step 5: Run test + build to verify pass**

Run: `cargo test -p ferrolite-app develop::preview_cache 2>&1 | tail -20 && cargo build -p ferrolite-app 2>&1 | tail -5`
Expected: PASS; compiles.

- [ ] **Step 6: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/develop/preview_cache.rs ferrolite-app/src/app.rs
git commit -m "feat(develop): make preview-cache write-back format-agnostic (RAW + JPG)"
```

---

## Task 5: Confirm the Standard color path yields an equivalent display matrix (`identity`)

**Files:**
- Modify: `ferrolite-app/src/develop/preview_cache.rs`

**Interfaces:**
- Produces: `standard_writeback_matrix() -> ferrolite_color::Mat3` — returns `ferrolite_color::identity()`, documented with the reason.

**Rationale (verify against source before coding):** `viewer::load::preview_to_linear` converts an 8-bit sRGB preview to **display-linear sRGB** (`viewer/load.rs:15`). `ferrolite_previews::encode_srgb_jpeg` expects a *working-space* render and applies `display_matrix` then the sRGB OETF (`ferrolite-previews/src/codec.rs:31`). A Standard image's `preview_source` is already display-linear sRGB, so the correct matrix to reproduce the source as an sRGB JPEG is the **identity** (skip working→display; apply only the OETF). This is the "equivalent display_matrix" the design asked to confirm — and it is `identity()`, NOT `working_to_display`.

- [ ] **Step 1: Write the failing round-trip test**

Add to `preview_cache.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn standard_writeback_matrix_is_identity() {
        // preview_source is already display-linear sRGB, so the write-back matrix
        // must be identity (encode applies matrix then sRGB OETF).
        assert_eq!(
            standard_writeback_matrix(),
            ferrolite_color::identity(),
            "Standard write-back must not re-apply a working->display transform"
        );
    }

    #[test]
    fn standard_writeback_round_trips_srgb() {
        // Known sRGB 8-bit -> display-linear (as the Standard preview path does),
        // encode with the Standard matrix, decode, back to linear: the round trip
        // must land close to the original linear values (JPEG q90 + 8-bit tolerance).
        use ferrolite_image::{ImageBuffer, PixelFormat};
        // A 8x8 solid patch of sRGB 128 (mid gray) so JPEG has no edges to ring.
        let src8 = ImageBuffer::new(8, 8, PixelFormat::Rgb8, vec![128u8; 8 * 8 * 3])
            .expect("valid rgb8 patch");
        let linear = crate::viewer::load::preview_to_linear(&src8); // display-linear sRGB
        let jpeg = encode_srgb_jpeg(
            &linear,
            standard_writeback_matrix(),
            PREVIEW_LONG_EDGE,
            PREVIEW_JPEG_QUALITY,
        )
        .expect("encode succeeds");
        let decoded = decode_srgb_jpeg(&jpeg).expect("decode succeeds");
        let round = crate::viewer::load::preview_to_linear(&decoded);
        // Center pixel comparison (avoid any border resampling); dims unchanged (8<2048).
        assert_eq!((round.width, round.height), (8, 8));
        let (o, r) = (linear.pixels[0], round.pixels[0]);
        assert!((o - r).abs() < 0.02, "sRGB round-trip within tolerance: {o} vs {r}");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app develop::preview_cache::tests::standard 2>&1 | tail -20`
Expected: FAIL — `standard_writeback_matrix` not found.

- [ ] **Step 3: Implement the helper**

Add to `preview_cache.rs` (near `should_write_back`):

```rust
/// The display matrix for a Standard (JPG/PNG/…) preview-cache write-back.
///
/// A Standard image's retained `preview_source` is produced by
/// [`crate::viewer::load::preview_to_linear`], which decodes 8-bit sRGB to
/// **display-linear sRGB**. [`encode_srgb_jpeg`] applies its `display_matrix`
/// and *then* the sRGB OETF, so to reproduce the source as an sRGB JPEG the
/// matrix must be the **identity** (the working→display step the RAW path needs
/// is already baked into `preview_source`). Confirmed by
/// `standard_writeback_round_trips_srgb`.
pub fn standard_writeback_matrix() -> Mat3 {
    ferrolite_color::identity()
}
```

`Mat3` and `decode_srgb_jpeg` / `encode_srgb_jpeg` / `PREVIEW_LONG_EDGE` are already imported at the top of the module; add `PREVIEW_JPEG_QUALITY` visibility is already `const` in-module. If `decode_srgb_jpeg` is not in the `use` list, it is (see the existing `read_cached_preview`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app develop::preview_cache::tests::standard 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/develop/preview_cache.rs
git commit -m "feat(develop): confirm Standard write-back matrix is identity (round-trip test)"
```

---

## Task 6: Gate Standard's full-JPG decode behind a preview-cache read (fast re-open)

**Files:**
- Modify: `ferrolite-app/src/app.rs` (open flow in `drive_viewer`, ~L3941–4010)

**Interfaces:**
- Consumes: `develop::preview_cache::spawn_cache_read` (existing), the existing `v.cache_read_requested` / `v.cache_resolved` / `v.cache_write_back` gating fields, `viewer::load::spawn_preview` (existing).
- Produces: no new API — restructures which decode fires when for Standard.

**Context:** Today Standard fires `spawn_preview` (full-JPG decode → reveal) unconditionally at open (`app.rs:3944`), and the cache-read gate (`app.rs:3968`) is `v.kind == Raw`-only, so JPGs get no disk cache benefit. RAW already does cache-read-first then gates its heavy `spawn_full` on `cache_resolved`. This task gives Standard the same shape: read the 2048px disk entry first; on a hit, reveal it instantly (via the existing `apply_preview_cache_hit` → `reveal_srgb_preview`); the full-JPG decode then streams in the 1:1 detail. Phase 2's Tier-0 thumbnail covers the now-debounced cold-open so it still feels instant.

- [ ] **Step 1: Split the preview-submit so Standard's full decode is gated**

In `drive_viewer` (~L3943), the `if !v.preview_requested { spawn_preview … }` block currently fires for BOTH kinds. Restrict the *immediate* submit to RAW (RAW's `spawn_preview` decodes the small embedded JPEG to keep the tier-1 alive and is not the heavy path), and route Standard through the cache gate below. Replace the block (~L3943–3955):

```rust
        // Submit the tier-1 preview decode. RAW: the small EMBEDDED preview is
        // submitted immediately (keeps the tier-1 alive; cheap). Standard: the
        // tier-1 preview IS the full-res JPG decode (heavy), so it is NOT fired
        // here — it is gated behind the preview-cache read below (mirrors RAW's
        // full-decode gate) so a re-open reveals the cached 2048px entry first.
        if let Some(v) = self.state.viewer.as_mut() {
            if !v.preview_requested && v.kind == ferrolite_image::FileKind::Raw {
                let h = viewer::load::spawn_preview(
                    &self.state.jobs,
                    &self.state.tx,
                    ctx,
                    v.image_id,
                    v.path.clone(),
                    v.kind,
                );
                v.preview_handle = Some(h);
                v.preview_requested = true;
            }
```

(Do NOT close the `if let Some(v)` here — the following steps continue inside it. The original block's closing brace stays where it was, at the end of the whole open-flow block ~L4040.)

- [ ] **Step 2: Extend the cache-read gate to Standard, dispatching the kind-appropriate decode**

Replace the RAW-only gate (~L3966–4010, the `if v.kind == Raw && (…) { … }` block) with a format-agnostic version. The cache read is identical for both; only the "full" decode differs (RAW → `spawn_full`; Standard → `spawn_preview`):

```rust
            // Tier-1 preview-cache read, then the heavy decode — for BOTH kinds.
            // Debounced (FULL_DECODE_DEBOUNCE) so fast arrow-nav doesn't submit a
            // read/decode per image flipped through — only the settled-on image
            // does, once `open_elapsed` crosses the threshold.
            //
            // Read-before-decode: consult the preview cache FIRST
            // (`spawn_cache_read`). The heavy decode is gated on the read having
            // resolved (`cache_resolved`), so a cache HIT reveals the 2048px entry
            // from disk (`apply_preview_cache_hit`) and the decode then streams in
            // the extra 1:1 detail — a MISS falls straight through to decode +
            // write-back. Phase 2's Tier-0 thumbnail covers the debounce window.
            let dt = ctx.input(|i| i.stable_dt);
            v.open_elapsed += dt;
            let heavy_pending = if v.kind == ferrolite_image::FileKind::Raw {
                !v.full_requested
            } else {
                !v.preview_requested
            };
            if !v.cache_read_requested || (heavy_pending && v.cache_resolved) {
                if v.open_elapsed >= FULL_DECODE_DEBOUNCE {
                    if !v.cache_read_requested {
                        let h = crate::develop::preview_cache::spawn_cache_read(
                            &self.state.jobs,
                            std::sync::Arc::clone(&self.state.preview_store),
                            &self.state.tx,
                            ctx,
                            v.image_id,
                            v.path.clone(),
                            v.op_stack.clone(),
                            self.state.working_space,
                        );
                        v.cache_read_handle = Some(h);
                        v.cache_read_requested = true;
                    } else if heavy_pending && v.cache_resolved {
                        if v.kind == ferrolite_image::FileKind::Raw {
                            if let Some(rs) = frame.wgpu_render_state() {
                                let gpu = std::sync::Arc::new(
                                    ferrolite_gpu::GpuContext::from_render_state(rs),
                                );
                                let h = viewer::load::spawn_full(
                                    &self.state.jobs,
                                    &self.state.tx,
                                    ctx,
                                    v.image_id,
                                    v.path.clone(),
                                    gpu,
                                );
                                v.full_handle = Some(h);
                                v.full_requested = true;
                            }
                        } else {
                            // Standard: the heavy tier-1 IS the full-res JPG decode.
                            let h = viewer::load::spawn_preview(
                                &self.state.jobs,
                                &self.state.tx,
                                ctx,
                                v.image_id,
                                v.path.clone(),
                                v.kind,
                            );
                            v.preview_handle = Some(h);
                            v.preview_requested = true;
                        }
                    }
                } else {
                    // Guarantee a frame fires once the debounce elapses even if the
                    // app would otherwise idle waiting on input, so a still
                    // (non-navigated) image's cache read still submits.
                    ctx.request_repaint_after(std::time::Duration::from_secs_f32(
                        FULL_DECODE_DEBOUNCE - v.open_elapsed,
                    ));
                }
            }
```

> Verify while editing: `spawn_cache_read`'s Standard payload works unchanged — it builds the key via `decode_color_profile(path)` which, for a Standard file, returns the fallback/identity profile; the key still hashes file identity + op stack + working space + profile, so a Standard entry keys distinctly and consistently. The `read_cached_preview` → `preview_to_linear` result is revealed by the existing `apply_preview_cache_hit` → `reveal_srgb_preview`, which already handles both kinds (it reads `preview_source`).

- [ ] **Step 3: Confirm the cache-hit reveal keeps the drive loop alive for Standard**

Read `apply_preview_cache_hit` (`app.rs:1283`). It already: sets `preview_source`, reveals via `reveal_srgb_preview`, sets `cache_write_back=false`, `cache_resolved=true`, and clears `idle` when revealed. For Standard this means: after a HIT the 2048px reveal shows, and because `heavy_pending` (`!preview_requested`) is still true and `cache_resolved` is now true, the gate in Step 2 submits the full-JPG `spawn_preview` next frame → `apply_preview_ready` reveals the full-res image (replacing the 2048px). No code change needed here; just verify the flow reads correctly and add a code comment at the `else` (Standard) branch noting the hit path also routes here.

- [ ] **Step 4: Build + full crate test**

Run: `cargo build -p ferrolite-app 2>&1 | tail -30 && cargo test -p ferrolite-app 2>&1 | tail -15`
Expected: compiles; all tests pass (no unit test for this wiring — it is exercised in the visual test plan; the pure key/read/write-back logic is already covered).

- [ ] **Step 5: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/app.rs
git commit -m "feat(develop): gate JPG full decode behind preview-cache read (fast re-open)"
```

---

## Task 7: Write the Standard reveal back to the disk preview cache

**Files:**
- Modify: `ferrolite-app/src/app.rs` (`apply_preview_ready`, ~L226–258)

**Interfaces:**
- Consumes: `develop::preview_cache::should_write_back` (Task 4), `standard_writeback_matrix` (Task 5), `spawn_cache_write` (existing), `v.preview_source` / `v.op_stack` / `v.cache_write_back` / `v.path` / `v.color_profile`.
- Produces: on a qualifying Standard reveal, a `Background` job that stores the 2048px identity JPEG (so the next open of the same JPG hits Task 6's read path).

**Context:** Standard reveals via `apply_preview_ready` → `reveal_srgb_preview` and never reaches `apply_full_decoded` (where RAW writes back). So the Standard write-back is hooked here. It reuses the retained display-linear-sRGB `preview_source` `Arc` as the payload (no second copy), with `standard_writeback_matrix()` (identity), gated by `should_write_back(&op_stack, v.cache_write_back)` — `cache_write_back` is `true` after a MISS (Task 6's read), `false` after a HIT (so no re-encode).

- [ ] **Step 1: Add the write-back after the Standard reveal**

In `apply_preview_ready` (`app.rs:255–257`), the Standard branch currently ends:

```rust
        // Standard: the preview IS the full-resolution image — reveal it now.
        self.reveal_srgb_preview(frame, image_id);
```

Replace with a reveal-then-write-back that mirrors the RAW write-back in `apply_full_decoded` (snapshot inputs, release the borrow, submit the `Background` job):

```rust
        // Standard: the preview IS the full-resolution image — reveal it now.
        let revealed = self.reveal_srgb_preview(frame, image_id);
        if !revealed {
            return;
        }

        // Preview-cache write-back (Phase 3): on a qualifying Standard open, cache
        // the identity color-managed 2048px render so a later open of the same JPG
        // reveals instantly from disk (Task 6's read path). `preview_source` is
        // already display-linear sRGB, so the write-back matrix is identity (Task
        // 5). Gated on a default op stack + a genuine cache MISS
        // (`v.cache_write_back`, set by the preview-cache read) so an edited image
        // or a re-open with the entry already on disk never re-encodes.
        //
        // Reuse the retained `preview_source` Arc as the payload (no second
        // O(pixels) copy); the Background job does the key stat + encode + disk IO
        // off the UI thread (CLAUDE.md rule 1).
        let write_back = self.state.viewer.as_ref().and_then(|v| {
            if v.image_id != image_id {
                return None;
            }
            crate::develop::preview_cache::should_write_back(&v.op_stack, v.cache_write_back).then(
                || {
                    (
                        v.path.clone(),
                        v.op_stack.clone(),
                        v.color_profile.clone(),
                        v.preview_source.clone(),
                    )
                },
            )
        });
        if let Some((path, op_stack, color_profile, Some(render))) = write_back {
            crate::develop::preview_cache::spawn_cache_write(
                &self.state.jobs,
                std::sync::Arc::clone(&self.state.preview_store),
                &self.state.tx,
                &self.egui_ctx_for_write_back(frame),
                path,
                op_stack,
                self.state.working_space,
                color_profile,
                render,
                crate::develop::preview_cache::standard_writeback_matrix(),
                ferrolite_previews::DEFAULT_CACHE_CAP_BYTES,
                image_id,
            );
        }
```

> Implementer notes:
> - `spawn_cache_write` needs an `&egui::Context`. `apply_preview_ready` takes `frame: &eframe::Frame` but not `ctx`. Check the call site of `apply_preview_ready` (grep `apply_preview_ready(`): it is dispatched from the `AppEvent::PreviewReady` handler in the events pump, which HAS the `ctx`. **Simplest fix: add a `ctx: &egui::Context` parameter to `apply_preview_ready`** and pass it at the call site, then use it directly here instead of the fictional `self.egui_ctx_for_write_back`. Remove that placeholder helper — it does not exist. Do NOT fabricate a context; thread the real one through (the same pattern `apply_full_decoded` uses — it already takes `ctx`).
> - `v.color_profile` for a Standard image is the fallback profile; it is only used by `key_for`'s `hash_color_profile` inside the job (the encode uses the passed `display_matrix`, not the profile), so it is correct and consistent with the read-path key (which also derives the profile via `decode_color_profile`).
> - `v.preview_source` is `Option<Arc<LinearRgbaF32>>`; the `Some(render)` pattern in the `if let` skips the write-back if it is somehow absent (it is always `Some` right after `reveal_srgb_preview` succeeded, but match defensively).

- [ ] **Step 2: Thread `ctx` into `apply_preview_ready`**

Change the signature (`app.rs:226`):

```rust
    fn apply_preview_ready(
        &mut self,
        frame: &eframe::Frame,
        ctx: &egui::Context,
        image_id: i64,
        linear: &ferrolite_image::LinearRgbaF32,
    ) {
```

Use `ctx` in the `spawn_cache_write` call (replacing the placeholder from Step 1). Update the single call site (grep `self.apply_preview_ready(`) to pass the handler's `ctx`.

- [ ] **Step 3: Build + full crate test**

Run: `cargo build -p ferrolite-app 2>&1 | tail -30 && cargo test -p ferrolite-app 2>&1 | tail -15`
Expected: compiles; all tests pass.

- [ ] **Step 4: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/app.rs
git commit -m "feat(develop): write JPG reveal back to the 2048px disk preview cache"
```

---

## Phase 2–3 — Coordinator wrap-up (not a subagent task)

After Task 7, the coordinator:

1. Runs `rustup update stable` (note if the sandbox blocks it — then run on the host), then the **repo gate**:
   `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo build --all-targets && cargo test --workspace`.
2. Runs the **memory guardrail** (design requirement): `FERROLITE_DIAG=1 cargo run --release`, press **F10** for the memory overlay, scroll a **JPG** folder fast, and confirm `rss` / `unattributed` / `pyr#` / `vt#` stay bounded (no new retained pyramids/VTs from the JPG cache path; the write-back is a transient Background encode).
3. Hands the author the **visual test plan** below and **HOLDS** for hands-on results before finishing the branch (CLAUDE.md "Finishing a branch").

### Visual test plan (for the author, Jann)

Fixtures: a folder with both **RAW** and **JPG (straight-from-camera)** originals; at least one large (24MP) JPG. Run `FERROLITE_DIAG=1 cargo run --release`.

1. **Tier-0 instant reveal — JPG (Phase 2).** From the grid, open a JPG you have already scrolled past (so its grid thumbnail is decoded). *Expected:* the picture appears **immediately** (upscaled thumbnail, faintly soft, spinner overlaid), then the sharp render fades in with **no black flash and no color/brightness pop**. *Failure:* black canvas before the image; a hard "snap" from blurry to sharp; a visible color jump.
2. **Tier-0 instant reveal — RAW (Phase 2).** Same for a RAW. *Expected:* identical instant-thumbnail → smooth sharpen. *Failure:* spinner on black for a beat before anything shows.
3. **Tier-0 with no thumbnail.** Open an image whose grid thumb is NOT yet decoded (scroll to a fresh folder region and open fast). *Expected:* graceful fallback to the old spinner-on-black — never a panic or a stretched-garbage frame.
4. **JPG fast re-open (Phase 3).** Open a large JPG (cold: watch it decode), navigate to another image, then re-open the **same** JPG. *Expected:* the second open reveals the sharp image **noticeably faster** than the first (served from the 2048px disk cache), with the Tier-0 thumbnail bridging instantly. *Failure:* the re-open is as slow as the cold open (cache not hit) — check the diag log for a `PreviewCacheHit`.
5. **JPG cache correctness.** Re-open the same JPG several times. *Expected:* the revealed image is the correct, color-correct picture every time (never a stale/wrong image, never a washed-out or double-gamma look). *Failure:* wrong image, or a color/gamma shift between the cached reveal and the full decode — indicates the identity-matrix write-back is wrong.
6. **Fast scroll bound (guardrail).** With F10 up, arrow-scroll quickly through the JPG folder. *Expected:* `rss` rises then recedes when you stop; `pyr#`/`vt#` stay small; `unattributed` flat. *Failure:* monotonic climb (a retained buffer from the new cache path).
7. **No regressions.** Edits, crop/mask overlays, before/after split, per-control reset, and the preview→full crossfade still behave; no freeze on open or navigation (frame-time line in the F9 text overlay stays within budget). The RAW path (which Task 6 restructured the open-flow gate around) still reveals correctly and still writes its cache.

---

## Self-Review

**1. Spec coverage (Phases 2–3 of the design doc):**
- Phase 2 "Tier-0 thumbnail placeholder reveal, reuses crossfade" → Tasks 1–3 (instant thumbnail + timed fade over the reveal, aligned via `image_screen_rect`). Deviation from "reuse `crossfading`/`crossfade_elapsed` literally": a **separate** `tier0_*` ramp is used because those fields are actively driving the preview→full swap and overloading them would corrupt it — the crossfade *pattern* is reused, per the spec's intent. Documented at Task 1.
- Phase 3 "relax `should_write_back`'s `is_raw` gate to include `FileKind::Standard`" → Task 4 (made fully format-agnostic).
- Phase 3 "confirm the Standard color path yields an equivalent `display_matrix`" → Task 5 (it is `identity()`, proven by a round-trip test).
- Phase 3 "JPGs get the 2048px reveal via `spawn_cache_read`/`spawn_cache_write`" → Tasks 6 (read-gate) + 7 (write-back).
- Explicitly-dropped optional items (warm-RAM neighbor reuse / RAM-LRU, f16 trim) → recorded under Non-goals per the 2026-07-18 scope decision.

**2. Placeholder scan:** One deliberate placeholder was called out and corrected in-plan — Task 7 Step 1 references a fictional `self.egui_ctx_for_write_back` and immediately instructs the implementer to instead thread a real `ctx: &egui::Context` through `apply_preview_ready` (Task 7 Step 2), matching `apply_full_decoded`. No "TODO/handle edge cases" left; every code step shows complete code.

**3. Type consistency:** `should_write_back(&OpStack, bool)` (Task 4) is called with the new arity in `app.rs` (Task 4 Step 4) and Task 7. `standard_writeback_matrix() -> Mat3` (Task 5) is consumed in Task 7. `tier0_fade_alpha` / `begin_tier0_fade` / `tick_tier0_fade` / `tier0_fading` / `tier0_elapsed` / `tier0_started` (Task 1 + Task 3) are used consistently in `paint` (Tasks 2–3) and the repaint gate (Task 3). `fit_rect` (Task 2) is used in both the placeholder (Task 2) and the fade fallback (Task 3). `paint`'s new trailing `tier0_thumb: Option<&egui::TextureHandle>` is threaded from the single call site (Task 2 Step 6) and used by Tasks 2–3.

**4. Known implementer verifications flagged inline:** the `viewer::paint` borrow interleave (Task 2 Step 6 note), the `apply_preview_ready` ctx-threading (Task 7 notes), the Standard cache-read key/profile behaviour (Task 6 Step 2 note), and the exact `drive_viewer` repaint-gate condition (Task 3 Step 2) — each says to verify against the real source, not assume.

# ferrolite — Spec 4.3: Monitor-profile / display color management (design)

> **Status:** Design — pending user review (2026-07-04); then writing-plans.
> **Date:** 2026-07-04
> **Parent map:** `2026-07-03-spec4-secondary-and-polish-map.md` (§4 **Spec 4.3**, §3 licensing
> tiers, §5 cross-cutting contracts — read first for the settled seams).
> **Predecessor:** `2026-07-01-spec3-color-and-export-design.md` (§4.3 / §5.2 built the display
> tail **swappable** precisely so this drops in; this spec is one of the two items Spec 3 §2
> deferred into Spec 4).
> **Proves:** the on-screen image is transformed `working → the physical monitor` instead of the
> hardcoded `working → sRGB` assumption, via the monitor's real ICC profile.
> **UI target:** the **Settings** window (General tab, a new "Display" group).
> **Branch:** `feat/monitor-color-management` (off `main`).

---

## 1. Goal & validation

Replace the "assume sRGB display" tail with the **real monitor profile**, so colors are correct
on wide-gamut (P3 / Adobe RGB) and calibrated displays instead of oversaturated:

> the edit pipeline still outputs **working-space linear** → the display tail transforms
> `working → the monitor's profile` (a 3D LUT baked from the monitor ICC) and encodes to the
> panel's response → the OS auto-detects the profile of the monitor the window is on (Windows +
> macOS), re-detecting when the window moves to another monitor → a **Settings → Display** control
> offers `Auto | sRGB | Custom(.icc)`, persisted → any failure falls back to sRGB, logged, never
> panics.

Image quality remains **secondary** to speed/architecture (map §3.3). The deliverable is the
*architecture*: a generic, engine-safe 3D-LUT display tail baked off-thread from a parsed monitor
profile, dropped into the swappable seam Spec 3 built — no C toolchain, no engine-tier photo
concepts, no change to the executor.

---

## 2. Scope

**In:**
- `ferrolite-color` (photo tier) — `DisplayProfile` (parse a monitor ICC via `moxcms`) and
  `bake_display_lut(working, &DisplayProfile, size) -> DisplayLut` (pure color math, no GPU/UI).
- `ferrolite-vt` `display.wgsl` (**engine tier**) — a **dual-path tail** on the **on-screen path**:
  the existing analytic `working→sRGB` matrix + `linear_to_srgb`, **or** a generic **3D-LUT
  texture** sampled via a gamma **shaper**, selected by a `use_lut` uniform flag. New generic
  bindings (3D texture + sampler); pipelines still built once + pre-warmed. **`ferrolite-pipeline`
  `blit.wgsl` is NOT touched** — it is offscreen readback (thumbnail regeneration + golden tests)
  and stays standard sRGB (§7).
- `ferrolite-app` (photo tier) — a `monitor_profile` detector (`#[cfg(windows)]`, `#[cfg(macos)]`,
  else stub), off-thread detect→parse→bake on `ferrolite-jobs`, event delivery + LUT upload,
  per-frame window-monitor-change re-detect, the **Settings → Display** control, and persistence.

**Out (non-goals / later):**
- **Soft-proofing, out-of-gamut warnings** (Spec 3 non-goals, unchanged).
- **Linux auto-detection** (X11 `_ICC_PROFILE` atom / Wayland has no standard) — a compile-time
  stub returns `None` → sRGB; the manual `Custom` picker still works on Linux.
- HDR / advanced-color swapchain output; Windows Auto Color Management (ACM) interop — v1 assumes a
  standard SDR swapchain with the app responsible for the transform (ACM off, the default).
- Per-image or per-document display profiles (this is an app-wide display setting).
- Changing the **histogram**'s space (stays a fixed sRGB reference — §7).

---

## 3. Architecture of the slice

```
ferrolite-app  (photo tier)
  Settings → General → Display: mode {Auto | sRGB | Custom(.icc)} · resolved name · Redetect
  monitor_profile::detect(window) -> (Option<Vec<u8>> icc, MonitorKey)
      #[cfg(windows)]  MonitorFromWindow → device → CreateDC → GetICMProfileW → read file
      #[cfg(macos)]    CGDirectDisplayID → CGDisplayCopyColorSpace → CGColorSpaceCopyICCData
      else             None
  per-frame: window MonitorKey changed? → enqueue re-detect (debounced)
   │
   │  ferrolite-jobs task  (contract §1):  detect → parse → bake LUT
   ▼
ferrolite-color  (photo tier, pure / testable — no GPU/UI/unsafe)
  DisplayProfile = moxcms::ColorProfile::new_from_slice(bytes)  (matrix+TRC OR cLUT/A2B, uniformly)
  bake_display_lut(working, &monitor, size=33) -> DisplayLut { size, rgba16f }
      moxcms transform:  linear-working source profile → monitor profile  (per LUT node)
   │  { DisplayLut, name }  OR  None → sRGB fallback,  over the app event channel
   ▼  on receipt: set_display_lut(..) (or set_display_matrix(..)+use_lut=0); request_repaint()
ferrolite-vt / display.wgsl   (ENGINE tier — generic; the ON-SCREEN path only)
  tail dual-path:
    use_lut==0 → linear_to_srgb(disp.m * lin)             ← EXACT Spec-3 path (sRGB / fallback)
    use_lut==1 → textureSampleLevel(lut3d, shaper(lin))   ← monitor-managed path
  + bindings 9 (texture_3d) + 10 (sampler) + uniform fields (use_lut, shaper_gamma); built once
   │
   ├── histogram (ferrolite-vt) — UNCHANGED, still bins working→sRGB (fixed reference, §7)
   └── blit.wgsl (ferrolite-pipeline) — UNCHANGED, offscreen sRGB readback (thumbnails, §7)
```

**Licensing tiers (map §3.1) preserved.** The engine crates (`ferrolite-vt`, `ferrolite-gpu`,
`ferrolite-image`) gain only a **generic 3D-LUT texture + a gamma shaper + a `use_lut` flag** — no
photo concepts, no copyleft deps → stays engine-transferable. All ICC parsing, monitor
colorimetry, and the LUT bake live in `ferrolite-color` / `ferrolite-app` (photo tier). `moxcms`
(already a workspace dep) does the cLUT math; `windows` / `core-graphics` / `core-foundation`
(permissive, cfg-gated, target-only) are app-tier. **No C toolchain → no build-gating decision**
(unlike 4.2 / 4.4). The generic `Graph<PipelineImage>` executor is **not modified** (contract §4).

---

## 4. `ferrolite-color` — monitor profile parse + 3D-LUT bake

Pure, `Clone`, no GPU/UI/`unsafe`; unit-testable on every OS in CI (like the rest of the crate).

### 4.1 `DisplayProfile`
- Wraps a parsed `moxcms::ColorProfile` (so **matrix/TRC and cLUT/A2B** monitor profiles are
  represented uniformly) plus a display `name` from the profile's description tag (for the UI).
- `parse(bytes: &[u8]) -> Result<DisplayProfile, ColorError>` — invalid/unsupported → `Err`
  (the app turns this into the sRGB fallback; never panics).

### 4.2 `DisplayLut` — GPU-ready table (data only, no wgpu dependency)
```rust
pub struct DisplayLut { pub size: u32, pub rgba16f: Vec<u16> } // size³ × 4 half-floats
```
- **Default `size = 33`** (industry-standard; 33³ · 4 · 2 B ≈ 287 KB). A tunable const.

### 4.3 `bake_display_lut(working: WorkingSpace, monitor: &DisplayProfile, size: u32) -> DisplayLut`
1. **Source = a *linear* working profile.** Build a `moxcms::ColorProfile` with the working
   space's primaries + white point and a **linear TRC** (gamma 1.0), so working-**linear** RGB can
   be fed straight through a moxcms profile→profile transform (moxcms otherwise expects
   TRC-encoded device values).
2. **Transform** `working_linear → monitor` via moxcms (f32 transform, relative-colorimetric
   intent, black-point-compensation off). moxcms internally does source → PCS(XYZ) → monitor-BtoA,
   applying the monitor's **full** matrix+TRC **or** cLUT — this is what makes the LUT correct for
   cLUT-based (hardware-calibrated) displays, the reason option C was chosen.
3. **Shaper indexing.** Node `(i,j,k)` maps to input `lin = shaper_decode(i/(N-1), …)`; the shaper
   is a plain **gamma encode** matching the shader's `shaper_encode`, so the [0,1] index grid puts
   more nodes in the shadows. Working-linear > 1 clamps to the LUT edge (SDR; acceptable).
4. Run each node through the transform → monitor-encoded RGB in [0,1] → store as half-float.
   Output is finite and clamped.

### 4.4 Regression safety
The bake is used **only** on the LUT path. The sRGB / fallback path still calls the existing
`working_to_display` matrix + analytic `linear_to_srgb`, so Spec 3's exact
`sRGB ≡ linear_to_srgb` golden is untouched (see §6, §8).

### 4.5 Tests (pure CPU)
- `parse`: valid `.icc` fixtures ok; garbage rejected.
- `bake_display_lut`: sRGB-working + sRGB-monitor-profile reproduces the sRGB OETF within
  trilinear tolerance; a known wide-gamut fixture profile → known corner/primary values vs a direct
  moxcms CPU reference; per-channel monotonic; all-finite.
- shaper `decode ∘ encode` round-trip.
- Two small `.icc` fixtures bundled under `fixtures/`.
- *Implementation detail deferred to the plan (not the spec):* the exact moxcms call to construct a
  linear-TRC working source profile. moxcms exposes primaries, per-channel `ToneReprCurve`, and
  profile→profile transforms, so it is feasible; the plan pins the precise API.

---

## 5. Engine-tier display tail — dual-path shader + generic 3D-LUT plumbing

The one real shader-structure change (the map anticipated this for option C); kept fully generic.

### 5.1 Shader (`display.wgsl` `fs_main`/`fs_tiled`/`fs_sparse` — the on-screen path only)
After computing `lin` (working-linear texel), the tail branches on a uniform flag:
```wgsl
struct DisplayColor { m: mat3x3<f32>, use_lut: u32, shaper_gamma: f32, _pad: vec2<f32> };
@group(0) @binding(8)  var<uniform> disp: DisplayColor;
@group(0) @binding(9)  var lut3d: texture_3d<f32>;
@group(0) @binding(10) var lut_samp: sampler;

fn shaper_encode(c: vec3<f32>) -> vec3<f32> {
    return pow(clamp(c, vec3(0.0), vec3(1.0)), vec3(1.0 / disp.shaper_gamma));
}
// tail:
if (disp.use_lut == 0u) {
    return vec4(linear_to_srgb(disp.m * lin), 1.0);                 // EXACT existing path
}
return vec4(textureSampleLevel(lut3d, lut_samp, shaper_encode(lin), 0.0).rgb, 1.0);
```
- The LUT already bakes working→monitor **and** the encode, so `disp.m` is not applied on the LUT
  path. `use_lut == 0` is byte-identical to today.

### 5.2 Pipelines (`DisplayPipelines`, built once + pre-warmed — CLAUDE.md GPU rule)
- `DisplayColorUniform` grows `use_lut: u32` + `shaper_gamma: f32` (still 16-byte aligned).
- Each of the four `DisplayPipelines` variants' bind-group layouts gains **binding 9 (3D texture)** +
  **binding 10 (sampler)**. A **1×1×1 identity 3D texture** is created at startup so every
  pipeline/bind-group is valid before any profile loads; `use_lut` defaults `0`. `blit.wgsl` and its
  pipeline are **not** touched.
- New methods: `set_display_lut(queue, &DisplayLut)` (uploads/recreates the 3D texture, sets
  `use_lut = 1` + `shaper_gamma`); the existing `set_display_matrix(...)` also sets `use_lut = 0`.
  The LUT texture is a cached, image-independent resource, re-created only when a new `DisplayLut`
  arrives (profile / working-space change) — **never per frame, never per image**.
- The `binding: 8` bind-group sites in `view.rs` each add the two new bindings (mechanical; all
  point at the cached LUT texture/sampler + matrix buffer).

### 5.3 Recovery
On GPU device-loss, the LUT texture + identity default are rebuilt with the pipelines (reuses Spec
1/2 recovery); the last-known LUT is re-uploaded **once**, not per edit.

---

## 6. App — detection, off-thread bake, re-detect, Settings, persistence (`ferrolite-app`)

### 6.1 `monitor_profile` module (new) — cfg-gated detector
Returns raw ICC bytes for the monitor a window is on, plus a stable `MonitorKey` (so we can tell
when it changes):
- `#[cfg(windows)]` — `MonitorFromWindow(hwnd)` → `GetMonitorInfo`/`EnumDisplayDevices` device name
  → `CreateDC` → `GetICMProfileW` → read the returned profile file. Via the `windows` crate
  (permissive, Windows-target-only).
- `#[cfg(target_os = "macos")]` — the window's `CGDirectDisplayID` → `CGDisplayCopyColorSpace` →
  `CGColorSpaceCopyICCData`. Via `core-graphics` / `core-foundation` (permissive, macOS-only).
- else → `None`.
- The window handle comes from eframe's raw-window-handle. These FFI functions are minimal and
  excluded from coverage; the **parse + mode-resolution** logic is what is unit-tested.

### 6.2 Resolution + off-thread bake
`resolve(mode, detected_bytes) -> Option<Vec<u8>>`:
- `Srgb` → `None` (analytic path). `Custom(path)` → read that file. `Auto` → the detected bytes.
- The bytes (if any) go to a **`ferrolite-jobs` task** (contract §1): `DisplayProfile::parse` →
  `bake_display_lut(working_space, &profile, 33)` → deliver `{ DisplayLut, name }` (or `None` on any
  failure, logged) over the **app event channel**. On receipt: `set_display_lut(...)` (or
  `set_display_matrix(...)` + `use_lut = 0` for `None`) then `request_repaint()`. No
  disk/OS/CPU-bake work ever runs on the UI thread.

### 6.3 Working-space coupling
The LUT is `f(working_space, monitor)`. When a profile is active, `apply_working_space` enqueues a
**re-bake** (same task) in addition to updating the sRGB matrix the histogram uses. Off-thread,
infrequent.

### 6.4 Multi-monitor re-detect
Each frame, read the window's current `MonitorKey` (cheap); if it differs from the last, enqueue a
detect+bake, **debounced** so a drag across monitors fires once on settle. This also catches most
display-config changes. Startup and the Settings "Redetect" button enqueue the same path.

### 6.5 Settings → General → "Display" group
Radio `Auto | sRGB | Custom…`; `Custom` opens an `rfd` `.icc`/`.icm` picker; a label shows the
**resolved profile name** (or "sRGB (default)" / "Not detected — using sRGB"); a **Redetect**
button. Changing the mode persists + enqueues resolution.
- This is an app **preference**, not an editing control, so CLAUDE.md's per-control **reset-arrow**
  rule does not apply. Its "reset" is honored in spirit by the **default mode = Auto** (stated
  explicitly here so the rule is addressed, not silently skipped).

### 6.6 Persistence
`Settings` gains `display_profile: PersistedDisplayProfile` (a serde DTO mirroring
`{ Auto | Srgb | Custom(PathBuf) }`, default `Auto`). `#[serde(default)]` keeps old settings files
loading cleanly. On startup: load → resolve → detect/bake.

---

## 7. Histogram and offscreen readback — unchanged (fixed sRGB reference)

Two paths stay on the fixed **sRGB** reference, deliberately decoupled from the physical monitor
transform:

- **Live histogram** — keeps binning in `working → sRGB` (`working_to_display` +
  `pack_display_matrix`), independent of the monitor. A histogram must not shift when the window is
  dragged to another monitor.
- **`blit.wgsl` / `blit_to_rgba8`** — offscreen readback used for **thumbnail regeneration**
  (`develop/thumb_regen.rs`) and golden tests. Thumbnails are standard sRGB catalog artifacts; they
  must **not** be monitor-tinted. Unchanged.

Only the **on-screen** `display.wgsl` path becomes monitor-managed. This separates the **analysis /
storage space** (sRGB, fixed) from the **physical display transform** (monitor, per-window). No
histogram or blit code changes.

---

## 8. Error handling (never panics; always a defined transform)

- Unsupported OS / no detectable profile / detection failure / parse failure / missing custom file /
  bake failure → the **sRGB analytic path** (`use_lut = 0`), logged; the Settings label reflects the
  fallback.
- GPU LUT-upload / device-loss → handled by the existing wgpu error-scope recovery (§5.3).
- A slow/failed detect or bake never blocks the UI thread — it is a cancellable job; superseded
  re-detects (rapid monitor drags) are debounced and the latest wins.

---

## 9. Testing (TDD; CLAUDE.md gate, then hold for the author's visual test)

**Pure CPU (every OS in CI — the 80%+ target):**
- `DisplayProfile::parse`: valid fixtures ok; garbage rejected.
- `bake_display_lut`: sRGB ≈ sRGB-OETF within tolerance; known wide-gamut fixture corners vs moxcms
  CPU reference; per-channel monotonic; all-finite.
- shaper `decode ∘ encode` round-trip.
- `resolve(mode, bytes)` truth table (Auto / sRGB / Custom, present vs absent bytes).
- `PersistedDisplayProfile` round-trip.

**Golden-image GPU diffs (auto-skip when `GpuContext::headless()` is `None`, per Spec 1):**
- **Analytic sRGB path ≡ Spec 3 reference** (regression, unchanged).
- **LUT path** vs a CPU application of the same baked LUT, within tolerance.

**FFI detection (Windows / macOS):** thin, cfg-gated, excluded from coverage; validated by hand +
the bundled-fixture parse tests. **macOS live verification is deferred** — it is not runnable on the
Windows dev machine; the manual `Custom`-file picker (testable on every OS) is the cross-OS safety
net.

**egui UI** (Settings → Display group): `cargo build` + clippy + the author's hands-on visual test.

**Gate:** `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` +
`cargo test --workspace` green → **then STOP and hold for the author's (Jann's) hands-on visual
test on real Windows monitors** before finishing the branch (CLAUDE.md "Finishing a branch" rule).

---

## 10. §5 contracts honored

1. **Job submission is universal** — detect / parse / bake is a `ferrolite-jobs` task with
   cancellation; superseded re-detects are debounced.
2. **Catalog is a cache** — the display profile is app *settings*, not the catalog; its loss never
   loses photos or edits.
3. **Decode products additive** — no new decode product is needed (the monitor profile is an OS/app
   concern, not a per-image decode output).
4. **GPU executor is photo-agnostic** — the generic `Graph<PipelineImage>` executor is unchanged;
   the tail is a display-shader concern, as in Spec 3.
5. **VT is source-agnostic** — the tail gains only a **generic 3D LUT + gamma shaper + flag**; no
   photo concepts enter `ferrolite-vt` / `ferrolite-gpu` / `ferrolite-image` (§3.1).

No C toolchain is introduced → **no build-gating decision** is required (contrast 4.2 / 4.4).

---

## 11. Decomposition into implementation plans

One branch `feat/monitor-color-management` off `main`; each plan is its own writing-plans → TDD
cycle, in dependency order.

1. **`ferrolite-color` foundation.** `DisplayProfile::parse`, `DisplayLut`,
   `bake_display_lut` (+ linear-source-profile helper, shaper), full CPU tests + `.icc` fixtures.
   No GPU/app.
2. **Engine-tier dual-path tail.** Extend `DisplayColorUniform` (`use_lut` / `shaper_gamma`); add
   the 3D-LUT texture + sampler bindings across the four `DisplayPipelines` variants (on-screen
   `display.wgsl` only — `blit` untouched); `set_display_lut`; the 1×1×1 identity default;
   `view.rs` bind-group updates; recovery. The two GPU goldens (sRGB regression + LUT path).
3. **App detection + off-thread bake + multi-monitor.** `monitor_profile` (Windows / macOS /
   stub); `resolve`; the jobs task + event delivery + LUT upload; working-space re-bake coupling;
   per-frame window-monitor-change re-detect (debounced).
4. **Settings UI + persistence.** `PersistedDisplayProfile`; the Settings → General "Display"
   group (Auto / sRGB / Custom + resolved name + Redetect); startup resolve; persist-on-change.

---

## 12. Decisions recorded (resolved during brainstorming, 2026-07-04)

| Question | Decision | Rationale |
|---|---|---|
| Fidelity of the display transform | **Full 3D LUT** (cLUT/A2B), generic texture sampled in the tail shader, baked off-thread | Most faithful; subsumes matrix + TRC; correct for cLUT / hardware-calibrated displays; gives the engine tier a reusable generic 3D-LUT capability. |
| OS auto-detection scope | **Windows + macOS**; Linux → compile-time stub (sRGB); manual picker on every OS | Windows is the dev platform; macOS adds ColorSync via permissive FFI. macOS live verification deferred (not runnable on the dev machine). |
| Multi-monitor + re-read | **Follow the window's monitor**; off-thread re-detect on monitor change / display-config change / startup / manual | Correct for a photo editor spanning monitors; cheap `MonitorKey` compare per frame, re-bake only on change. |
| Manual picker modes | **Auto / sRGB / Custom(.icc)**, resolved name + Redetect, persisted, in Settings → General | Covers auto-detect + manual override + explicit sRGB (disable CM) in one control. |
| Histogram space | **Fixed sRGB reference** (unchanged) | A histogram must not shift between monitors; separates analysis space from the physical display transform. |
| Render path selection | **Dual path** — analytic sRGB when mode is sRGB / detection fails; 3D-LUT only when a real profile is active | Preserves Spec 3's exact `sRGB ≡ linear_to_srgb` golden; zero interpolation error on the common sRGB case. |
| New deps | `windows` (Windows) + `core-graphics`/`core-foundation` (macOS), cfg-gated, app-tier; `moxcms` reused | Permissive, target-only; no C toolchain → no build-gating decision. |
| Engine-tier discipline | Tail gains only a **generic 3D LUT + gamma shaper + flag** | No photo concepts / copyleft in `ferrolite-vt`/`-gpu`/`-image` (§3.1); executor unchanged (contract §4). |
| Scope | **One spec, 4 implementation plans**, one branch | Mirrors Spec 3's decomposition; keeps each plan reviewable. |

---

## 13. Reference

- **Spec 4 map:** `2026-07-03-spec4-secondary-and-polish-map.md` — §3 tiers, §4.3 entry, §5 contracts.
- **Spec 3 (Color & Export):** `2026-07-01-spec3-color-and-export-design.md` — §4.3 / §5.2 the
  swappable display tail this replaces; §7.1 the histogram (kept on sRGB here).
- **v1 architecture map:** `2026-06-28-ferrolite-v1-architecture-map.md` — §3 licensing tiers,
  §5 cross-cutting contracts.
- **Design system:** `../../design/ferrolite-design-system.md` — Settings-window grammar for the
  Display control.
- **Code touch-points:** `ferrolite-color/src/tail.rs` (composition), `ferrolite-color/src/icc.rs`
  (moxcms parse), `ferrolite-vt/src/shaders/display.wgsl` + `ferrolite-vt/src/pipelines.rs`
  (`DisplayColorUniform`, `set_display_matrix`, binding 8; `blit.wgsl` stays sRGB, unchanged),
  `ferrolite-app/src/app.rs` (`working_to_display` push sites, `apply_working_space`),
  `ferrolite-app/src/settings/` (Settings modal + DTOs).

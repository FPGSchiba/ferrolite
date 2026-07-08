# Decode Active-Area Crop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Crop the decoded RAW to the camera's recommended image rectangle (`crop_area`, falling back to `active_area`) so the sensor's optically-black / masked border is no longer part of the image — removing the ~20px "stretched seam" the tiled renderer produces by edge-replicating that border at the right/bottom edges.

**Architecture:** `ferrolite-decode`'s `decode_full` currently returns the full sensor buffer (`img.width`×`img.height`) with no crop. rawler exposes `RawImage.crop_area` / `active_area` as `Rect { p: Point{x,y}, d: Dim2{w,h} }`. We crop the CFA/sensor sample buffer to that rectangle, update `width`/`height`, and — because cropping at an odd origin shifts the Bayer mosaic phase — shift the CFA pattern and permute the per-position black levels by the crop origin so the demosaic's `pos=(y%2)*2+(x%2)` stays aligned. At an even/even origin (the common case, and the bundled fixture's `(8,6)`) both shifts are no-ops, so the CFA pattern and black levels are byte-identical to today.

**Tech Stack:** Rust, `rawler` 0.7.2 (PINNED — do not bump; `RawImage.crop_area`/`active_area: Option<Rect>`, `Rect{p:Point{x,y}, d:Dim2{w,h}}`, `CFA::shift(x,y)`, `BlackLevel::as_bayer_array() -> [f32;4]`). Change is confined to `ferrolite-decode/src/raw.rs`.

## Global Constraints

- **rawler stays pinned at 0.7.2** — do not change the dependency. Use only the fields/methods named here (verified present in 0.7.2).
- **Even/even crop origin must be byte-identical to today** except for the cropped dimensions and pixel buffer: the CFA-pattern shift and black-level permutation MUST reduce to the identity when `crop_origin.x % 2 == 0 && crop_origin.y % 2 == 0`. The bundled fixture `fixtures/raw/sample.rw2` has origin `(8,6)` (even/even), full `4060×2250`, crop `3968×2232` — this is the regression anchor.
- **No behavior change when there is no crop:** if `crop_area`/`active_area` are both absent, or the rectangle already equals the full frame, `decode_full` returns exactly what it does today.
- **Coordinate space:** `Rect` is in the `data` buffer's pixel coordinates (sensor-native, before EXIF orientation). Crop is applied to the sensor buffer; `orientation` is unchanged and still applied downstream by the consumer.
- **Immutability / small focused files, comprehensive error handling** (CLAUDE.md). The crop helpers are pure functions with explicit bounds handling.
- **Green gate before finishing:** `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace` clean, THEN hand the author a visual test and hold.

## File Structure

- `ferrolite-decode/src/raw.rs` — the only file changed. Add pure helpers (`crop_sensor_buffer`, `permute_black_levels_by_origin`) + a small `CropRect` extraction, wire them into `decode_full`, and add unit + fixture-gated tests. The file is ~200 lines and stays well under the 800-line cap.

---

### Task 1: Pure crop + phase helpers (no fixture needed)

**Files:**
- Modify: `ferrolite-decode/src/raw.rs` (add private helpers + unit tests in the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces (private to the crate):
  - `fn crop_sensor_buffer(pixels: &[u16], full_w: usize, full_h: usize, cpp: usize, cx: usize, cy: usize, cw: usize, ch: usize) -> Vec<u16>` — returns the `cw*ch*cpp` sub-buffer starting at pixel `(cx,cy)`, row-major, `cpp` components per pixel.
  - `fn permute_black_levels_by_origin(bl: [f32; 4], cx: usize, cy: usize) -> [f32; 4]` — returns the 2×2 black-level array reordered so index `r*2+c` (cropped position) holds the sensor value at `((cy+r)%2, (cx+c)%2)`. Identity when `cx` and `cy` are both even.
- Consumes: nothing new.

- [ ] **Step 1: Write the failing test for `crop_sensor_buffer`**

Add to `ferrolite-decode/src/raw.rs` `mod tests`:

```rust
    #[test]
    fn crop_sensor_buffer_extracts_subrect_cpp1() {
        // 4x3 single-component buffer, values = y*10 + x.
        let full_w = 4usize;
        let full_h = 3usize;
        let px: Vec<u16> = (0..full_h)
            .flat_map(|y| (0..full_w).map(move |x| (y * 10 + x) as u16))
            .collect();
        // Crop the 2x2 starting at (1,1): expect [11,12, 21,22].
        let out = crop_sensor_buffer(&px, full_w, full_h, 1, 1, 1, 2, 2);
        assert_eq!(out, vec![11, 12, 21, 22]);
    }

    #[test]
    fn crop_sensor_buffer_respects_cpp() {
        // 2x2, cpp=2: pixel (x,y) -> [y*10+x, 100+y*10+x].
        let (full_w, full_h, cpp) = (2usize, 2usize, 2usize);
        let mut px = Vec::new();
        for y in 0..full_h {
            for x in 0..full_w {
                px.push((y * 10 + x) as u16);
                px.push((100 + y * 10 + x) as u16);
            }
        }
        // Crop the 1x2 column starting at (1,0): pixels (1,0) and (1,1).
        let out = crop_sensor_buffer(&px, full_w, full_h, cpp, 1, 0, 1, 2);
        assert_eq!(out, vec![1, 101, 11, 111]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ferrolite-decode crop_sensor_buffer -- --nocapture`
Expected: FAIL to compile — `crop_sensor_buffer` not defined.

- [ ] **Step 3: Implement `crop_sensor_buffer`**

Add to the private helpers section of `ferrolite-decode/src/raw.rs`:

```rust
/// Copy the `cw`×`ch` sub-rectangle whose top-left is sensor pixel `(cx, cy)`
/// out of a row-major `full_w`×`full_h` buffer with `cpp` components per pixel.
/// The caller guarantees `cx + cw <= full_w` and `cy + ch <= full_h`.
fn crop_sensor_buffer(
    pixels: &[u16],
    full_w: usize,
    full_h: usize,
    cpp: usize,
    cx: usize,
    cy: usize,
    cw: usize,
    ch: usize,
) -> Vec<u16> {
    debug_assert!(cx + cw <= full_w && cy + ch <= full_h, "crop rect out of bounds");
    debug_assert_eq!(pixels.len(), full_w * full_h * cpp, "pixel buffer size mismatch");
    let mut out = Vec::with_capacity(cw * ch * cpp);
    for row in 0..ch {
        let src_y = cy + row;
        let row_start = (src_y * full_w + cx) * cpp;
        out.extend_from_slice(&pixels[row_start..row_start + cw * cpp]);
    }
    out
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p ferrolite-decode crop_sensor_buffer -- --nocapture`
Expected: PASS (both).

- [ ] **Step 5: Write the failing test for `permute_black_levels_by_origin`**

```rust
    #[test]
    fn black_levels_permute_is_identity_for_even_origin() {
        let bl = [1.0, 2.0, 3.0, 4.0]; // [(0,0),(0,1),(1,0),(1,1)]
        assert_eq!(permute_black_levels_by_origin(bl, 8, 6), bl);
        assert_eq!(permute_black_levels_by_origin(bl, 0, 0), bl);
    }

    #[test]
    fn black_levels_permute_shifts_phase_for_odd_origin() {
        // bl indexed r*2+c: (0,0)=1 (0,1)=2 (1,0)=3 (1,1)=4.
        // Odd x (cx=1), even y (cy=0): cropped (r,c) -> sensor ((0+r)%2,(1+c)%2).
        //   (0,0)->(0,1)=2  (0,1)->(0,0)=1  (1,0)->(1,1)=4  (1,1)->(1,0)=3
        assert_eq!(permute_black_levels_by_origin([1.0, 2.0, 3.0, 4.0], 1, 0), [2.0, 1.0, 4.0, 3.0]);
        // Odd x and odd y (cx=1,cy=1): cropped (r,c) -> sensor ((1+r)%2,(1+c)%2).
        //   (0,0)->(1,1)=4  (0,1)->(1,0)=3  (1,0)->(0,1)=2  (1,1)->(0,0)=1
        assert_eq!(permute_black_levels_by_origin([1.0, 2.0, 3.0, 4.0], 1, 1), [4.0, 3.0, 2.0, 1.0]);
    }
```

- [ ] **Step 6: Run to verify failure**

Run: `cargo test -p ferrolite-decode black_levels_permute`
Expected: FAIL to compile — `permute_black_levels_by_origin` not defined.

- [ ] **Step 7: Implement `permute_black_levels_by_origin`**

```rust
/// Reorder a 2×2 per-position black-level array (indexed `row*2 + col`) so it
/// matches the CFA phase after cropping at sensor origin `(cx, cy)`. Cropped
/// position `(r, c)` reads sensor position `((cy + r) % 2, (cx + c) % 2)`.
/// Identity when `cx` and `cy` are both even.
fn permute_black_levels_by_origin(bl: [f32; 4], cx: usize, cy: usize) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for r in 0..2 {
        for c in 0..2 {
            let src = (((cy + r) % 2) * 2) + ((cx + c) % 2);
            out[r * 2 + c] = bl[src];
        }
    }
    out
}
```

- [ ] **Step 8: Run to verify pass + clippy**

Run: `cargo test -p ferrolite-decode crop_sensor_buffer black_levels_permute`
Expected: PASS (4 tests).
Run: `cargo clippy -p ferrolite-decode --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add ferrolite-decode/src/raw.rs
git commit -m "feat(decode): pure crop + Bayer-phase black-level helpers"
```

---

### Task 2: Apply the active-area crop in `decode_full`

**Files:**
- Modify: `ferrolite-decode/src/raw.rs` (`decode_full` body; `RawDecoded`/`decode_full` doc comments; add a fixture-gated integration test)

**Interfaces:**
- Consumes: `crop_sensor_buffer`, `permute_black_levels_by_origin` (Task 1); rawler `img.crop_area`/`img.active_area: Option<Rect>` with `rect.p.x/.y`, `rect.d.w/.h`; `CFA::shift(x, y) -> CFA`.
- Produces: no signature change — `decode_full` still returns `RawDecoded`, now cropped.

- [ ] **Step 1: Choose the crop rectangle and apply it (replace the `img.data`→`pixels` and CFA/black/dims sections)**

In `decode_full`, AFTER `let img = decoder.raw_image(...)?;` and the `orientation` block, and BEFORE building the `pixels`/CFA/levels, insert the crop selection. Then thread the cropped values through. Concretely:

1. Right after `orientation` is computed, add:

```rust
    // Crop to the camera's recommended image rectangle so the sensor's masked /
    // optically-black border is excluded (otherwise the tiled renderer edge-
    // replicates it into a stretched seam). Prefer `crop_area` (the intended
    // final image, matching the embedded preview); fall back to `active_area`
    // (optically-black-excluded); else no crop. `Rect { p: Point{x,y}, d: Dim2{w,h} }`
    // is in sensor-buffer pixel coords (pre-orientation).
    let full_w = img.width;
    let full_h = img.height;
    let crop = img
        .crop_area
        .or(img.active_area)
        .filter(|r| r.p.x + r.d.w <= full_w && r.p.y + r.d.h <= full_h)
        .filter(|r| !(r.p.x == 0 && r.p.y == 0 && r.d.w == full_w && r.d.h == full_h));
```

2. Replace the `let pixels = match img.data { ... };` block so the decoded buffer is cropped when `crop` is `Some`:

```rust
    // RawImageData is Integer(Vec<u16>) for almost all formats; a few DNGs are
    // Float — quantize to u16 for this plan's display-only consumer.
    let full_pixels = match img.data {
        RawImageData::Integer(v) => v,
        // NaN/Inf saturate to 0 / 65535 via Rust's defined float-to-int cast; acceptable for this display-only consumer.
        RawImageData::Float(v) => v
            .iter()
            .map(|f| f.round().clamp(0.0, 65535.0) as u16)
            .collect(),
    };
    let (pixels, width, height, crop_origin) = match crop {
        Some(r) => (
            crop_sensor_buffer(&full_pixels, full_w, full_h, img.cpp, r.p.x, r.p.y, r.d.w, r.d.h),
            r.d.w,
            r.d.h,
            (r.p.x, r.p.y),
        ),
        None => (full_pixels, full_w, full_h, (0, 0)),
    };
```

3. In the CFA section, shift the CFA by the crop origin BEFORE `cfa_to_pattern` (identity when origin is even/even):

```rust
    let cfa = match &img.photometric {
        RawPhotometricInterpretation::Cfa(cfg) => cfg.cfa.clone(),
        _ => img.camera.cfa.clone(),
    };
    // Cropping can move the top-left into a different Bayer phase; shift the
    // pattern to the crop origin so it describes the cropped buffer's (0,0).
    let cfa = cfa.shift(crop_origin.0, crop_origin.1);
    let cfa_pattern = cfa_to_pattern(&cfa);
```

4. In the black-level section, permute by the crop origin to stay aligned with the shifted pattern:

```rust
    // BlackLevel::as_bayer_array() -> [f32; 4]  (rawler 0.7.2)
    let black_levels = permute_black_levels_by_origin(
        img.blacklevel.as_bayer_array(),
        crop_origin.0,
        crop_origin.1,
    );
```

5. In the final `Ok(RawDecoded { ... })`, replace the `width`/`height` fields (which used `u32::try_from(img.width)`…) with the cropped `width`/`height` locals:

```rust
    Ok(RawDecoded {
        width: u32::try_from(width)
            .map_err(|_| DecodeError::Rawler("RAW width exceeds u32".into()))?,
        height: u32::try_from(height)
            .map_err(|_| DecodeError::Rawler("RAW height exceeds u32".into()))?,
        cpp: img.cpp,
        pixels,
        cfa_pattern,
        black_levels,
        white_level,
        wb_coeffs,
        color_profile: ColorProfile::from_color_matrix(&img.color_matrix),
        orientation,
    })
```

(The `white_level` and `wb_coeffs` blocks are unchanged — white level is scalar and WB coefficients are per-color `[R,G1,B,G2]`, both phase-independent.)

- [ ] **Step 2: Update the doc comments**

Update the `RawDecoded` struct doc and the `decode_full` doc to state that the buffer is cropped to `crop_area`/`active_area`, that `width`/`height` are the cropped dims, and that `cfa_pattern`/`black_levels` are phase-shifted to the crop origin. Update the `width`/`height` field docs on `RawDecoded` if they imply full-sensor dims. Example addition to `RawDecoded`'s top doc:

```rust
/// A fully decoded RAW cropped to the camera's recommended image rectangle
/// (`crop_area`, else `active_area`, else the full sensor): integer CFA/sensor
/// samples plus geometry and colour calibration metadata. `width`/`height` are
/// the CROPPED dimensions; `cfa_pattern` and `black_levels` are phase-aligned to
/// the crop origin. Consumed by the demosaic/display pipeline.
```

- [ ] **Step 3: Add a fixture-gated integration test asserting the crop**

Add to `mod tests`. The bundled fixture `../fixtures/raw/sample.rw2` is full `4060×2250`, crop `(8,6)` `3968×2232` (even/even origin). If the fixture is absent, skip (CI-safe).

```rust
    /// The active-area crop must shrink the decoded frame to the camera's
    /// recommended rectangle (removing the masked/optically-black border that
    /// otherwise seams at the right/bottom edge in the tiled renderer). For the
    /// bundled RW2 (`crop_area` origin (8,6), even/even) the crop preserves the
    /// Bayer phase, so `cfa_pattern` and `black_levels` are unchanged.
    #[test]
    fn decode_full_crops_to_active_area() {
        let fixture = Path::new("../fixtures/raw/sample.rw2");
        if !fixture.exists() {
            eprintln!("no RAW fixture; skipping active-area crop assertion");
            return;
        }
        let d = decode_full(fixture).expect("decode");
        // Cropped dims (NOT the full 4060x2250 sensor).
        assert_eq!((d.width, d.height), (3968, 2232), "decoded to crop_area dims");
        // Pixel buffer length matches cropped dims * cpp.
        assert_eq!(d.pixels.len(), (d.width as usize) * (d.height as usize) * d.cpp);
        // Even/even origin -> phase preserved; every black level finite.
        assert!(d.black_levels.iter().all(|b| b.is_finite()));
        assert!(d.white_level > 0.0);
    }
```

- [ ] **Step 4: Verify against the fixture (dev machine has it)**

Run: `cargo test -p ferrolite-decode decode_full_crops_to_active_area -- --nocapture`
Expected: PASS — `(d.width, d.height) == (3968, 2232)`. (If the fixture is absent on some machine, the test self-skips.)

- [ ] **Step 5: Run the decode crate's whole suite + clippy + fmt**

Run: `cargo test -p ferrolite-decode`
Expected: PASS (existing decode/standard/color tests + the new crop test; fixture-gated ones run where the fixture exists).
Run: `cargo clippy -p ferrolite-decode --all-targets --all-features -- -D warnings`
Expected: clean.
Run: `cargo fmt --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add ferrolite-decode/src/raw.rs
git commit -m "feat(decode): crop to active_area, dropping the masked sensor border"
```

---

### Task 3: Workspace green gate

**Files:** none (verification only).

- [ ] **Step 1: Format + clippy**

Run: `cargo fmt --all --check`
Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: both clean.

- [ ] **Step 2: Full test suite**

Run: `cargo test --workspace`
Expected: PASS. (Note: `ferrolite-app`'s `retain_visible_thumbnail_jobs_cancels_offscreen_only` is a known pre-existing flaky test unrelated to decode — if it fails under parallel load, re-run it in isolation to confirm, and do not treat it as a regression of this branch.)

- [ ] **Step 3: STOP — hand the author the visual test plan and hold**

Do NOT merge/push. Present the visual test plan and wait for the author's hands-on results.

**Visual test plan (author, hands-on):**
1. Open the RW2 that showed the seam (the fixture-class Panasonic file), enter Develop, view the full image fit-to-window and let the idle full-res tiles settle.
2. **Right edge:** the ~20px "stretched" seam on the right edge must be **gone** — the image ends cleanly with no smeared/replicated column. Check the bottom edge too (12px border was there).
3. **Zoom to 1:1 near the right/bottom edges:** no stretched border, no color fringe from the old masked pixels.
4. **Color sanity (phase check):** the whole image must have correct colours — NO magenta/green maze artifacts or channel swaps (these would appear if the Bayer phase were mis-cropped). Compare overall colour to before: it should look the same, just without the border.
5. **Framing vs preview:** the full-res image framing should now match the embedded preview / before-after split (both are the cropped camera image).
6. **A second camera if available:** open a RAW from a different camera (ideally non-Panasonic) and confirm it still decodes and looks correct (guards the general crop path). If none is available, note that only the RW2 path was hands-on tested.

The failure signature that means the phase handling is wrong: correct framing but **wrong colours / mosaic artifacts** across the whole image. The failure signature that means the crop didn't apply: the seam is **still present**.

---

## Self-Review

**1. Spec coverage:** Seam root cause (masked border not cropped) → Task 2 applies `crop_area`/`active_area`. Bayer-phase safety (odd-origin cameras) → Task 1 helpers + Task 2 `cfa.shift` + black-level permute, verified identity for the even/even fixture. No-crop fallback preserved. Fixture-gated dims assertion guards it. Green gate + visual test hand-off in Task 3. ✓

**2. Placeholder scan:** No TBD/TODO. Every code step has complete code. The one runtime-dependent value (fixture dims `3968×2232`, origin `(8,6)`) was measured directly from `fixtures/raw/sample.rw2` and is asserted, not guessed. ✓

**3. Type consistency:**
- `crop_sensor_buffer(pixels:&[u16], full_w, full_h, cpp, cx, cy, cw, ch) -> Vec<u16>` — defined Task 1, called Task 2 with `(&full_pixels, full_w, full_h, img.cpp, r.p.x, r.p.y, r.d.w, r.d.h)`. All `usize` (rawler `Point`/`Dim2` fields and `img.width`/`cpp` are `usize`). ✓
- `permute_black_levels_by_origin([f32;4], cx, cy) -> [f32;4]` — defined Task 1, called Task 2 with `(img.blacklevel.as_bayer_array(), crop_origin.0, crop_origin.1)`. ✓
- `crop_origin: (usize, usize)` sourced from `r.p.x/.y`; `cfa.shift(crop_origin.0, crop_origin.1)` — rawler `CFA::shift(x:usize, y:usize)`. ✓
- `width`/`height` locals are `usize`, converted via `u32::try_from` at the struct build (same as today). ✓

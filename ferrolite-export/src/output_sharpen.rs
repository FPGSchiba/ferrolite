//! Output sharpening (P4 design §5): a separable unsharp mask applied to the
//! quantized output-space RGB buffer AFTER resize and BEFORE encode, to
//! compensate for resampling softness.
//!
//! Two deliberate choices (design §5.2): it runs in the OUTPUT-ENCODED (gamma)
//! domain rather than linear — standard practice, and it avoids a linear
//! round-trip purely for sharpening — and it computes in `f32` internally with
//! ONE rounding at the end, so an 8-bit export does not compound quantization
//! error through the unsharp pass.

use rayon::prelude::*;

use crate::options::{BitDepth, OutputMedium, OutputSharpenAmount};

/// `(radius, amount)` for a medium/tier pair; `None` when output sharpening is
/// off. Radius is `f32` (sub-pixel radii are the point at output scale), unlike
/// the develop op's `u32` pixel radius. Starting table from design §5.1.
pub(crate) fn output_sharpen_params(
    medium: OutputMedium,
    amt: OutputSharpenAmount,
) -> Option<(f32, f32)> {
    let radius = match medium {
        OutputMedium::None => return None,
        OutputMedium::Screen => 0.7,
        OutputMedium::Glossy => 1.0,
        OutputMedium::Matte => 1.3,
    };
    let amount = match (medium, amt) {
        (OutputMedium::None, _) => return None,
        (OutputMedium::Screen, OutputSharpenAmount::Low) => 0.30,
        (OutputMedium::Screen, OutputSharpenAmount::Standard) => 0.50,
        (OutputMedium::Screen, OutputSharpenAmount::High) => 0.75,
        (OutputMedium::Glossy, OutputSharpenAmount::Low) => 0.35,
        (OutputMedium::Glossy, OutputSharpenAmount::Standard) => 0.60,
        (OutputMedium::Glossy, OutputSharpenAmount::High) => 0.90,
        (OutputMedium::Matte, OutputSharpenAmount::Low) => 0.45,
        (OutputMedium::Matte, OutputSharpenAmount::Standard) => 0.75,
        (OutputMedium::Matte, OutputSharpenAmount::High) => 1.10,
    };
    Some((radius, amount))
}

/// Gaussian-ish separable weights for a sub-pixel `radius`. Kernel half-width is
/// `ceil(radius)` capped at 3 (output radii are always <= 1.3).
fn weights(radius: f32) -> Vec<f32> {
    let half = (radius.ceil() as usize).clamp(1, 3);
    let sigma = (radius / 2.0).max(0.25);
    let mut w: Vec<f32> = (0..=2 * half)
        .map(|i| {
            let d = i as f32 - half as f32;
            (-(d * d) / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let sum: f32 = w.iter().sum();
    for v in &mut w {
        *v /= sum;
    }
    w
}

/// Separable unsharp mask over an interleaved RGB buffer, in place.
/// `rgb` is `u8` bytes for `BitDepth::Eight` and native-endian `u16` bytes for
/// `BitDepth::Sixteen` (the caller casts, exactly as `resize.rs` does).
///
/// **One transient f32 buffer, not three** (final-review FIX 2): a large
/// no-resize export (61 MP = 3 × 720 MB with three full-image `w*h*3` f32
/// buffers, 102 MP = 3.7 GB) could exhaust memory, and `Vec` allocation
/// failure ABORTS the process — no `ExportError`, no unwind. The horizontal
/// pass reads straight out of `rgb` (still fully original — nothing has been
/// written yet) into the one `tmp` buffer; the vertical pass reads the
/// per-pixel ORIGINAL value straight back out of `rgb` (again before this
/// SAME loop overwrites that pixel — see the inline comment below) instead of
/// from a separate `src` copy, and reads its blur input from `tmp` (never
/// from `rgb`, which is actively being overwritten), so `tmp` is the only
/// intermediate ever allocated. Purely an allocation-count optimization —
/// verified bit-for-bit identical to the three-buffer version on both 8-bit
/// and 16-bit fixtures before landing (final-review report).
pub(crate) fn apply_output_sharpen(
    rgb: &mut [u8],
    w: u32,
    h: u32,
    depth: BitDepth,
    radius: f32,
    amount: f32,
) {
    if amount <= 0.0 || w == 0 || h == 0 {
        return;
    }
    let (w, h) = (w as usize, h as usize);
    let max_val = match depth {
        BitDepth::Eight => 255.0f32,
        BitDepth::Sixteen => 65535.0f32,
    };
    let bpp = match depth {
        BitDepth::Eight => 1usize,
        BitDepth::Sixteen => 2usize,
    };

    // Read one pixel-channel (index `i`, NOT byte offset) out of `buf` as f32
    // (one rounding at the very end — design §5.2). Generic over the buffer
    // passed in: the horizontal pass reads the full `rgb`, the vertical pass
    // reads a single per-row slice of it.
    let read = |buf: &[u8], i: usize| -> f32 {
        match depth {
            BitDepth::Eight => buf[i] as f32,
            BitDepth::Sixteen => u16::from_ne_bytes([buf[i * 2], buf[i * 2 + 1]]) as f32,
        }
    };
    let n = w * h * 3;

    let kernel = weights(radius);
    let half = kernel.len() / 2;

    // Horizontal blur, reading straight out of `rgb` — still fully original
    // at this point, since nothing has written to it yet. `tmp` is the only
    // buffer this function allocates.
    let mut tmp = vec![0.0f32; n];
    tmp.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            for c in 0..3 {
                let mut acc = 0.0;
                for (k, kw) in kernel.iter().enumerate() {
                    let sx =
                        (x as isize + k as isize - half as isize).clamp(0, w as isize - 1) as usize;
                    acc += kw * read(rgb, (y * w + sx) * 3 + c);
                }
                row[x * 3 + c] = acc;
            }
        }
    });

    // Vertical blur (from `tmp`, never mutated after the pass above — safe to
    // read concurrently) + unsharp combine, written straight back into `rgb`.
    // Each row reads its OWN original source pixel out of `rgb` via `read`
    // strictly BEFORE this same iteration overwrites that pixel — safe under
    // per-row parallelism (no row ever touches another row's slice, and the
    // read of pixel `idx` always precedes the write of that same `idx`), and
    // correct because at the start of this pass `rgb` is still fully
    // original (the pass above only READ it).
    rgb.par_chunks_mut(w * bpp * 3)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                for c in 0..3 {
                    let idx = x * 3 + c;
                    let s = read(row, idx); // this row's original value, read before overwrite
                    let mut acc = 0.0;
                    for (k, kw) in kernel.iter().enumerate() {
                        let sy = (y as isize + k as isize - half as isize).clamp(0, h as isize - 1)
                            as usize;
                        acc += kw * tmp[(sy * w + x) * 3 + c];
                    }
                    let v = (s + amount * (s - acc)).clamp(0.0, max_val);
                    match depth {
                        BitDepth::Eight => row[idx] = v.round() as u8,
                        BitDepth::Sixteen => {
                            let b = (v.round() as u16).to_ne_bytes();
                            row[idx * 2] = b[0];
                            row[idx * 2 + 1] = b[1];
                        }
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{BitDepth, OutputMedium, OutputSharpenAmount};

    /// Gate 3 (design §7.2): the default combination is inactive, so existing
    /// exports stay byte-identical.
    #[test]
    fn defaults_are_inactive() {
        assert!(
            output_sharpen_params(OutputMedium::None, OutputSharpenAmount::Standard).is_none(),
            "None medium must be inactive at every amount tier"
        );
        for amt in [
            OutputSharpenAmount::Low,
            OutputSharpenAmount::Standard,
            OutputSharpenAmount::High,
        ] {
            assert!(output_sharpen_params(OutputMedium::None, amt).is_none());
        }
    }

    /// The table's shape: Matte widest, Screen crispest, amount tiers ordered.
    #[test]
    fn table_radii_and_amounts_are_ordered() {
        let r = |m| {
            output_sharpen_params(m, OutputSharpenAmount::Standard)
                .unwrap()
                .0
        };
        assert!(r(OutputMedium::Screen) < r(OutputMedium::Glossy));
        assert!(r(OutputMedium::Glossy) < r(OutputMedium::Matte));
        let a = |t| output_sharpen_params(OutputMedium::Screen, t).unwrap().1;
        assert!(a(OutputSharpenAmount::Low) < a(OutputSharpenAmount::Standard));
        assert!(a(OutputSharpenAmount::Standard) < a(OutputSharpenAmount::High));
    }

    /// A flat buffer has no edges, so an unsharp mask cannot change it. Weak
    /// alone (a fully no-op implementation would also pass this), but paired
    /// with `step_edge_gains_contrast_8bit` below, which a no-op fails.
    #[test]
    fn flat_buffer_is_unchanged_8bit() {
        let (w, h) = (16u32, 16u32);
        let mut px = vec![128u8; (w * h * 3) as usize];
        let before = px.clone();
        apply_output_sharpen(&mut px, w, h, BitDepth::Eight, 1.0, 0.6);
        assert_eq!(px, before, "flat buffer must be untouched");
    }

    /// Sharpening must increase local contrast at a hard edge. Non-square
    /// (16x8) dims: a transposed w/h axis would corrupt row indexing here but
    /// be invisible on a square buffer. A no-op implementation fails this
    /// (after_gap == before_gap, not >).
    #[test]
    fn step_edge_gains_contrast_8bit() {
        let (w, h) = (16u32, 8u32);
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 60u8 } else { 190u8 };
                px.extend_from_slice(&[v, v, v]);
            }
        }
        let before = px.clone();
        apply_output_sharpen(&mut px, w, h, BitDepth::Eight, 1.0, 0.8);
        let idx_dark = ((h / 2 * w + w / 2 - 1) * 3) as usize;
        let idx_light = ((h / 2 * w + w / 2) * 3) as usize;
        let before_gap = before[idx_light] as i32 - before[idx_dark] as i32;
        let after_gap = px[idx_light] as i32 - px[idx_dark] as i32;
        assert!(
            after_gap > before_gap,
            "edge contrast {after_gap} !> {before_gap}"
        );
    }

    /// The 16-bit path must work on the same logic, not silently no-op.
    #[test]
    fn sixteen_bit_path_sharpens() {
        let (w, h) = (16u32, 8u32);
        let mut vals: Vec<u16> = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 15000u16 } else { 48000u16 };
                vals.extend_from_slice(&[v, v, v]);
            }
        }
        let before = vals.clone();
        let bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut vals);
        apply_output_sharpen(bytes, w, h, BitDepth::Sixteen, 1.0, 0.8);
        assert_ne!(vals, before, "16-bit buffer must actually change");
    }

    /// Strengthens `sixteen_bit_path_sharpens`: `assert_ne!` alone would pass
    /// for e.g. a uniform brightness shift or noise injection, not just a
    /// sharpen. Prove edge CONTRAST increased, same as the 8-bit test, and use
    /// non-square dims (16x8) so a transposed axis would fail here too.
    #[test]
    fn sixteen_bit_edge_gains_contrast() {
        let (w, h) = (16u32, 8u32);
        let mut vals: Vec<u16> = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 15000u16 } else { 48000u16 };
                vals.extend_from_slice(&[v, v, v]);
            }
        }
        let before = vals.clone();
        let bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut vals);
        apply_output_sharpen(bytes, w, h, BitDepth::Sixteen, 1.0, 0.8);
        let idx_dark = ((h / 2 * w + w / 2 - 1) * 3) as usize;
        let idx_light = ((h / 2 * w + w / 2) * 3) as usize;
        let before_gap = before[idx_light] as i32 - before[idx_dark] as i32;
        let after_gap = vals[idx_light] as i32 - vals[idx_dark] as i32;
        assert!(
            after_gap > before_gap,
            "16-bit edge contrast {after_gap} !> {before_gap}"
        );
    }

    /// `amount <= 0.0` is an explicit early-return guard, not just an
    /// unreachable default — prove it directly rather than only via the table
    /// (which never emits 0 for an active medium).
    #[test]
    fn zero_amount_is_noop() {
        let (w, h) = (16u32, 8u32);
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 60u8 } else { 190u8 };
                px.extend_from_slice(&[v, v, v]);
            }
        }
        let before = px.clone();
        apply_output_sharpen(&mut px, w, h, BitDepth::Eight, 1.0, 0.0);
        assert_eq!(px, before, "zero amount must not alter the buffer");
    }
}

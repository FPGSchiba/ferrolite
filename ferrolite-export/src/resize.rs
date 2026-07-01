//! Optional output resize. Dims math is pure/tested; the pixel resample uses
//! `fast_image_resize` (same crate the thumbnailer uses) over the quantized RGB
//! buffer. Quality is secondary (spec §1), so resampling the encoded RGB rather
//! than linear light is acceptable.

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use crate::error::ExportError;
use crate::options::{BitDepth, ResizeSpec};

/// Target dimensions for a resize spec applied to a `w × h` image. Never returns
/// a zero axis (clamps to 1).
#[allow(dead_code)]
pub(crate) fn resize_dims(spec: ResizeSpec, w: u32, h: u32) -> (u32, u32) {
    let (tw, th) = match spec {
        ResizeSpec::None => (w, h),
        ResizeSpec::Exact { w: ew, h: eh } => (ew, eh),
        ResizeSpec::LongEdge(px) => {
            let long = w.max(h) as f64;
            if long == 0.0 {
                (w, h)
            } else {
                let s = px as f64 / long;
                ((w as f64 * s).round() as u32, (h as f64 * s).round() as u32)
            }
        }
        ResizeSpec::Percent(p) => (
            (w as f64 * p as f64).round() as u32,
            (h as f64 * p as f64).round() as u32,
        ),
    };
    (tw.max(1), th.max(1))
}

/// Resize an interleaved RGB byte buffer to `tw × th`. `depth` selects the pixel
/// type (`U8x3` / `U16x3`). No-op (clone) when the size is unchanged.
#[allow(dead_code)]
pub(crate) fn apply_resize(
    rgb: &[u8],
    w: u32,
    h: u32,
    tw: u32,
    th: u32,
    depth: BitDepth,
) -> Result<Vec<u8>, ExportError> {
    if (w, h) == (tw, th) {
        return Ok(rgb.to_vec());
    }
    let pt = match depth {
        BitDepth::Eight => PixelType::U8x3,
        BitDepth::Sixteen => PixelType::U16x3,
    };
    let src = Image::from_vec_u8(w, h, rgb.to_vec(), pt)
        .map_err(|e| ExportError::Encode(format!("resize src: {e}")))?;
    let mut dst = Image::new(tw, th, pt);
    let opts = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    Resizer::new()
        .resize(&src, &mut dst, &opts)
        .map_err(|e| ExportError::Encode(format!("resize: {e}")))?;
    Ok(dst.buffer().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::ResizeSpec;

    #[test]
    fn none_is_identity() {
        assert_eq!(resize_dims(ResizeSpec::None, 6000, 4000), (6000, 4000));
    }

    #[test]
    fn long_edge_preserves_aspect() {
        // Landscape 6000x4000, long edge 1200 -> 1200x800.
        assert_eq!(
            resize_dims(ResizeSpec::LongEdge(1200), 6000, 4000),
            (1200, 800)
        );
        // Portrait 4000x6000, long edge 1200 -> 800x1200.
        assert_eq!(
            resize_dims(ResizeSpec::LongEdge(1200), 4000, 6000),
            (800, 1200)
        );
    }

    #[test]
    fn exact_is_verbatim() {
        assert_eq!(
            resize_dims(ResizeSpec::Exact { w: 1024, h: 768 }, 6000, 4000),
            (1024, 768)
        );
    }

    #[test]
    fn percent_scales_both_axes() {
        assert_eq!(
            resize_dims(ResizeSpec::Percent(0.5), 6000, 4000),
            (3000, 2000)
        );
        assert_eq!(resize_dims(ResizeSpec::Percent(0.25), 800, 600), (200, 150));
    }

    #[test]
    fn dims_never_zero() {
        assert_eq!(resize_dims(ResizeSpec::Percent(0.0001), 100, 100), (1, 1));
        assert_eq!(resize_dims(ResizeSpec::LongEdge(0), 100, 50), (1, 1));
    }
}

//! EXIF-orientation application shared by the RAW and standard decode routes.

use ferrolite_image::{LinearRgbaF32, Orientation};
use image::DynamicImage;

/// Apply an EXIF orientation to a decoded image using the `image` crate's
/// rotate/flip ops. (rotate90/270 are clockwise in the `image` crate.)
pub(crate) fn apply_orientation(img: DynamicImage, o: Orientation) -> DynamicImage {
    match o {
        Orientation::Normal => img,
        Orientation::FlipH => img.fliph(),
        Orientation::Rotate180 => img.rotate180(),
        Orientation::FlipV => img.flipv(),
        Orientation::Transpose => img.rotate90().fliph(),
        Orientation::Rotate90 => img.rotate90(),
        Orientation::Transverse => img.rotate270().fliph(),
        Orientation::Rotate270 => img.rotate270(),
    }
}

/// Apply an EXIF orientation to a linear RGBA f32 image, returning an upright
/// copy. Used by the RAW full-decode path, whose demosaic output is sensor-
/// native (the embedded preview is already uprighted via `apply_orientation`).
/// The per-orientation source-coordinate maps reproduce the same visual result
/// as the `image`-crate ops above so the preview and full tiers agree.
pub fn apply_orientation_linear(img: LinearRgbaF32, o: Orientation) -> LinearRgbaF32 {
    if matches!(o, Orientation::Normal) {
        return img;
    }
    let (w, h) = (img.width, img.height);
    // 90°/270° rotations and the diagonal mirrors transpose the dimensions.
    let (nw, nh) = match o {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Transpose
        | Orientation::Transverse => (h, w),
        _ => (w, h),
    };
    let mut px = vec![0.0f32; LinearRgbaF32::expected_len(nw, nh)];
    for dy in 0..nh {
        for dx in 0..nw {
            // Source pixel that lands at destination (dx, dy).
            let (sx, sy) = match o {
                Orientation::Normal => (dx, dy),
                Orientation::FlipH => (w - 1 - dx, dy),
                Orientation::FlipV => (dx, h - 1 - dy),
                Orientation::Rotate180 => (w - 1 - dx, h - 1 - dy),
                Orientation::Rotate90 => (dy, h - 1 - dx),
                Orientation::Rotate270 => (w - 1 - dy, dx),
                Orientation::Transpose => (dy, dx),
                Orientation::Transverse => (w - 1 - dy, h - 1 - dx),
            };
            let si = ((sy * w + sx) * 4) as usize;
            let di = ((dy * nw + dx) * 4) as usize;
            px[di..di + 4].copy_from_slice(&img.pixels[si..si + 4]);
        }
    }
    LinearRgbaF32::new(nw, nh, px).expect("oriented length matches dims")
}

#[cfg(test)]
mod tests {
    use super::apply_orientation_linear;
    use ferrolite_image::{LinearRgbaF32, Orientation};

    // 2×1 image: left pixel red, right pixel green (alpha 1).
    fn two_by_one() -> LinearRgbaF32 {
        LinearRgbaF32::new(2, 1, vec![1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0]).unwrap()
    }

    #[test]
    fn normal_is_identity() {
        let img = two_by_one();
        let out = apply_orientation_linear(img.clone(), Orientation::Normal);
        assert_eq!((out.width, out.height), (2, 1));
        assert_eq!(out.pixels, img.pixels);
    }

    #[test]
    fn rotate90_transposes_dims_and_moves_left_pixel_to_top() {
        // 90° CW: the left (red) pixel of a landscape row ends up at the TOP of
        // the resulting portrait column.
        let out = apply_orientation_linear(two_by_one(), Orientation::Rotate90);
        assert_eq!((out.width, out.height), (1, 2));
        assert_eq!(
            &out.pixels[0..4],
            &[1.0, 0.0, 0.0, 1.0],
            "top = red (was left)"
        );
        assert_eq!(
            &out.pixels[4..8],
            &[0.0, 1.0, 0.0, 1.0],
            "bottom = green (was right)"
        );
    }

    #[test]
    fn rotate270_moves_left_pixel_to_bottom() {
        let out = apply_orientation_linear(two_by_one(), Orientation::Rotate270);
        assert_eq!((out.width, out.height), (1, 2));
        assert_eq!(
            &out.pixels[0..4],
            &[0.0, 1.0, 0.0, 1.0],
            "top = green (was right)"
        );
        assert_eq!(
            &out.pixels[4..8],
            &[1.0, 0.0, 0.0, 1.0],
            "bottom = red (was left)"
        );
    }

    #[test]
    fn fliph_swaps_left_and_right() {
        let out = apply_orientation_linear(two_by_one(), Orientation::FlipH);
        assert_eq!((out.width, out.height), (2, 1));
        assert_eq!(&out.pixels[0..4], &[0.0, 1.0, 0.0, 1.0], "left now green");
        assert_eq!(&out.pixels[4..8], &[1.0, 0.0, 0.0, 1.0], "right now red");
    }

    #[test]
    fn every_orientation_produces_expected_len() {
        for o in [
            Orientation::Normal,
            Orientation::FlipH,
            Orientation::FlipV,
            Orientation::Rotate180,
            Orientation::Rotate90,
            Orientation::Rotate270,
            Orientation::Transpose,
            Orientation::Transverse,
        ] {
            let out = apply_orientation_linear(two_by_one(), o);
            assert_eq!(out.pixels.len(), (out.width * out.height * 4) as usize);
        }
    }
}

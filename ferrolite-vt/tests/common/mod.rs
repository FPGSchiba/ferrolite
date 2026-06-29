use ferrolite_image::LinearRgbaF32;

/// A 4×4 image: left half red, right half green (linear).
pub fn split_image() -> LinearRgbaF32 {
    let (w, h) = (4u32, 4u32);
    let mut px = Vec::new();
    for _y in 0..h {
        for x in 0..w {
            if x < w / 2 {
                px.extend_from_slice(&[1.0, 0.0, 0.0, 1.0]);
            } else {
                px.extend_from_slice(&[0.0, 1.0, 0.0, 1.0]);
            }
        }
    }
    LinearRgbaF32::new(w, h, px).unwrap()
}

/// Max per-channel absolute difference between two equal-length RGBA8 buffers.
pub fn max_abs_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

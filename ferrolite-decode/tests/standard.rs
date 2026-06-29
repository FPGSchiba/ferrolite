use ferrolite_image::FileKind;
use std::path::PathBuf;

/// Write a tiny PNG to a temp path and return it.
fn temp_png() -> PathBuf {
    let path = std::env::temp_dir().join(format!("ferro-dec-{}-{}.png", std::process::id(), "a"));
    let img = image::RgbImage::from_pixel(8, 4, image::Rgb([10, 20, 30]));
    img.save(&path).expect("write png");
    path
}

#[test]
fn standard_metadata_reports_dimensions_and_empty_make() {
    let path = temp_png();
    let meta = ferrolite_decode::read_metadata(&path, FileKind::Standard).expect("meta");
    assert_eq!(meta.width, 8);
    assert_eq!(meta.height, 4);
    assert_eq!(meta.make, "", "PNG has no EXIF make");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn standard_preview_is_nonempty_rgb8() {
    let path = temp_png();
    let buf = ferrolite_decode::decode_preview(&path, FileKind::Standard).expect("preview");
    assert_eq!(buf.width, 8);
    assert_eq!(buf.height, 4);
    assert!(!buf.pixels.is_empty());
    let _ = std::fs::remove_file(&path);
}

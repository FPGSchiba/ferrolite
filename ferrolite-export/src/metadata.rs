//! Copy source EXIF into the exported file (spec §8.1). Best-effort: any failure
//! is returned as a message the orchestrator records as a warning (never fatal,
//! never panics; spec §10). Path-based read/write lets little_exif infer the
//! container format from the extension.

use std::path::Path;

use little_exif::metadata::Metadata;

/// Read EXIF from `source` and write it into `dest` (which must already exist as a
/// valid encoded image). Returns `Err` with a human message on any failure.
///
/// Not yet called outside tests: the orchestrator (Task 9) wires this in behind the
/// `ExportOptions::copy_exif` flag.
#[allow(dead_code)]
pub(crate) fn copy_exif(source: &Path, dest: &Path) -> Result<(), String> {
    let meta = Metadata::new_from_path(source).map_err(|e| format!("read source EXIF: {e}"))?;
    meta.write_to_file(dest)
        .map_err(|e| format!("write EXIF to output: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use little_exif::exif_tag::ExifTag;
    use little_exif::filetype::FileExtension;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ferrolite-exif-test-{name}"))
    }

    #[test]
    fn copies_a_tag_from_source_to_dest() {
        // Source: a minimal JPEG with an EXIF ImageDescription tag.
        let src = tmp("src.jpg");
        let dst = tmp("dst.jpg");
        // Write two solid JPEGs via the image crate (both valid JPEG containers).
        let buf = image::RgbImage::from_pixel(8, 8, image::Rgb([10, 20, 30]));
        buf.save(&src).unwrap();
        buf.save(&dst).unwrap();

        // Tag the source.
        let mut m = Metadata::new();
        m.set_tag(ExifTag::ImageDescription("ferrolite-test".to_string()));
        m.write_to_file(&src).unwrap();
        let _ = FileExtension::JPEG; // ensure the enum is linked

        // Copy EXIF source -> dest and read it back.
        copy_exif(&src, &dst).expect("copy");
        let back = Metadata::new_from_path(&dst).expect("read back");
        let found = back
            .get_tag(&ExifTag::ImageDescription(String::new()))
            .next()
            .is_some();
        assert!(found, "ImageDescription should have been copied");

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
    }
}

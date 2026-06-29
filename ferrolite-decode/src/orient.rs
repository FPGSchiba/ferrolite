//! EXIF-orientation application shared by the RAW and standard decode routes.

use ferrolite_image::Orientation;
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

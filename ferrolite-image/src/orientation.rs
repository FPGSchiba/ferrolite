//! EXIF orientation (tag values 1..=8) with pure mapping logic. The pixel
//! transform itself lives in the consumer (ferrolite-decode applies it via the
//! `image` crate); this enum is the shared, testable vocabulary.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    #[default]
    Normal, // 1
    FlipH,     // 2: mirror horizontal
    Rotate180, // 3
    FlipV,     // 4: mirror vertical
    Transpose, // 5: mirror across main diagonal
    Rotate90,  // 6: 90° clockwise
    Transverse, // 7: mirror across anti-diagonal
    Rotate270, // 8: 270° clockwise
}

impl Orientation {
    /// Map an EXIF orientation tag value to the enum. Unknown/absent → `Normal`.
    pub fn from_exif(value: u16) -> Orientation {
        match value {
            2 => Orientation::FlipH,
            3 => Orientation::Rotate180,
            4 => Orientation::FlipV,
            5 => Orientation::Transpose,
            6 => Orientation::Rotate90,
            7 => Orientation::Transverse,
            8 => Orientation::Rotate270,
            _ => Orientation::Normal, // 1 and anything unexpected
        }
    }

    pub fn to_exif(self) -> u16 {
        match self {
            Orientation::Normal => 1,
            Orientation::FlipH => 2,
            Orientation::Rotate180 => 3,
            Orientation::FlipV => 4,
            Orientation::Transpose => 5,
            Orientation::Rotate90 => 6,
            Orientation::Transverse => 7,
            Orientation::Rotate270 => 8,
        }
    }

    /// True when applying the orientation swaps width and height.
    pub fn swaps_dimensions(self) -> bool {
        matches!(
            self,
            Orientation::Transpose
                | Orientation::Rotate90
                | Orientation::Transverse
                | Orientation::Rotate270
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_exif_maps_all_eight_values() {
        assert_eq!(Orientation::from_exif(1), Orientation::Normal);
        assert_eq!(Orientation::from_exif(2), Orientation::FlipH);
        assert_eq!(Orientation::from_exif(3), Orientation::Rotate180);
        assert_eq!(Orientation::from_exif(4), Orientation::FlipV);
        assert_eq!(Orientation::from_exif(5), Orientation::Transpose);
        assert_eq!(Orientation::from_exif(6), Orientation::Rotate90);
        assert_eq!(Orientation::from_exif(7), Orientation::Transverse);
        assert_eq!(Orientation::from_exif(8), Orientation::Rotate270);
    }

    #[test]
    fn from_exif_defaults_unknown_to_normal() {
        assert_eq!(Orientation::from_exif(0), Orientation::Normal);
        assert_eq!(Orientation::from_exif(99), Orientation::Normal);
    }

    #[test]
    fn to_exif_round_trips() {
        for v in 1..=8u16 {
            assert_eq!(Orientation::from_exif(v).to_exif(), v);
        }
    }

    #[test]
    fn swaps_dimensions_only_for_quarter_turns_and_diagonals() {
        assert!(!Orientation::Normal.swaps_dimensions());
        assert!(!Orientation::Rotate180.swaps_dimensions());
        assert!(Orientation::Rotate90.swaps_dimensions());
        assert!(Orientation::Rotate270.swaps_dimensions());
        assert!(Orientation::Transpose.swaps_dimensions());
        assert!(Orientation::Transverse.swaps_dimensions());
    }

    #[test]
    fn default_is_normal() {
        assert_eq!(Orientation::default(), Orientation::Normal);
    }
}

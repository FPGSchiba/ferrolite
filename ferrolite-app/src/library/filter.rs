//! Pure mapping from toolbar UI state to a `LibraryQuery`. No egui here.

use ferrolite_catalog::{
    FileTypeChip, LibraryQuery, RatingFilter, Scope, Sort, SortKey, TagFilter, TagMode,
};
use ferrolite_image::{Flag, TagId};
use std::collections::BTreeSet;

/// How many images "Recently Added" shows.
const RECENT_LIMIT: i64 = 200;

/// Which comparison operator to apply to the rating filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RatingCmp {
    #[default]
    AtLeast,
    Exactly,
    AtMost,
}

impl RatingCmp {
    /// Cycle through AtLeast → Exactly → AtMost → AtLeast.
    pub fn next(self) -> Self {
        match self {
            RatingCmp::AtLeast => RatingCmp::Exactly,
            RatingCmp::Exactly => RatingCmp::AtMost,
            RatingCmp::AtMost => RatingCmp::AtLeast,
        }
    }

    /// Short ASCII label for the toggle button (no ≥/≤ glyphs — IBM Plex lacks them).
    pub fn label(self) -> &'static str {
        match self {
            RatingCmp::AtLeast => ">=",
            RatingCmp::Exactly => "=",
            RatingCmp::AtMost => "<=",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewSource {
    Folder(i64),
    All,
    Collection(i64),
    RecentlyAdded,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilterState {
    pub search: String,
    pub sort_key: SortKey,
    pub sort_desc: bool,
    pub min_rating: u8,
    pub rating_cmp: RatingCmp,
    pub flags: Vec<Flag>,
    pub tag_ids: Vec<TagId>,
    pub tag_mode: TagMode,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub file_types: BTreeSet<FileTypeChip>,
    pub iso: Option<(u32, u32)>,
    pub aperture: Option<(f32, f32)>,
    pub focal: Option<(f32, f32)>,
    pub date: Option<(String, String)>,
}

impl Default for FilterState {
    fn default() -> Self {
        FilterState {
            search: String::new(),
            sort_key: SortKey::CaptureTime,
            sort_desc: false,
            min_rating: 0,
            rating_cmp: RatingCmp::default(),
            flags: Vec::new(),
            tag_ids: Vec::new(),
            tag_mode: TagMode::Any,
            camera: None,
            lens: None,
            file_types: BTreeSet::new(),
            iso: None,
            aperture: None,
            focal: None,
            date: None,
        }
    }
}

/// Maps a `RangeSlider`'s handle positions to the optional min/max filter
/// tuple it drives: handles sitting at the FULL `[min, max]` bounds mean the
/// filter is inactive (`None`); anything narrower is a real filter and maps
/// to `Some((lo, hi))`. Pure/egui-free so it's unit-testable on its own —
/// used by the toolbar's ISO/Aperture/Focal `RangeSlider`s (`toolbar.rs`).
/// Equality uses `f32::EPSILON` to match the exact bounds the widget itself
/// resets to (see `RangeSlider`'s own `modified` check in `widgets/range_slider.rs`).
pub fn range_to_filter(lo: f32, hi: f32, min: f32, max: f32) -> Option<(f32, f32)> {
    let full_range = (lo - min).abs() < f32::EPSILON && (hi - max).abs() < f32::EPSILON;
    if full_range {
        None
    } else {
        Some((lo, hi))
    }
}

impl FilterState {
    /// Reset metadata popup filter selections to their default (unfiltered) state.
    pub fn reset_metadata_filters(&mut self) {
        self.camera = None;
        self.lens = None;
        self.file_types.clear();
        self.min_rating = 0;
        self.iso = None;
        self.aperture = None;
        self.focal = None;
    }

    /// True when every user-facing filter is at its default (search empty, no
    /// rating/flag constraint, no tags with `tag_mode` back to `Any`, no
    /// metadata filters, empty `file_types`, no date range). Sort order
    /// (`sort_key`/`sort_desc`) does NOT count — it is a view preference, not
    /// a filter (see `reset_all`, which preserves it).
    pub fn is_default(&self) -> bool {
        self.search.trim().is_empty()
            && self.min_rating == 0
            && self.flags.is_empty()
            && self.tag_ids.is_empty()
            && self.tag_mode == TagMode::Any
            && self.camera.is_none()
            && self.lens.is_none()
            && self.file_types.is_empty()
            && self.iso.is_none()
            && self.aperture.is_none()
            && self.focal.is_none()
            && self.date.is_none()
    }

    /// Reset every user-facing filter to its default (unfiltered) state;
    /// leaves `sort_key`/`sort_desc` untouched, since sort is a view
    /// preference rather than a filter (see `is_default`).
    pub fn reset_all(&mut self) {
        self.reset_metadata_filters();
        self.search.clear();
        self.rating_cmp = RatingCmp::default();
        self.flags.clear();
        self.tag_ids.clear();
        self.tag_mode = TagMode::Any;
        self.date = None;
    }

    pub fn to_query(&self, source: ViewSource, include_subfolders: bool) -> LibraryQuery {
        let scope = match source {
            ViewSource::Folder(id) => Scope::Folder {
                id,
                recursive: include_subfolders,
            },
            ViewSource::All => Scope::AllPhotographs,
            ViewSource::Collection(id) => Scope::Collection { id },
            ViewSource::RecentlyAdded => Scope::RecentlyAdded {
                limit: RECENT_LIMIT,
            },
        };
        let search = {
            let t = self.search.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        };
        let rating = if self.min_rating == 0 {
            None
        } else {
            Some(match self.rating_cmp {
                RatingCmp::AtLeast => RatingFilter::AtLeast(self.min_rating),
                RatingCmp::Exactly => RatingFilter::Exactly(self.min_rating),
                RatingCmp::AtMost => RatingFilter::AtMost(self.min_rating),
            })
        };
        LibraryQuery {
            scope,
            search,
            sort: Sort {
                key: self.sort_key,
                desc: self.sort_desc,
            },
            rating,
            flags: self.flags.clone(),
            tags: TagFilter {
                ids: self.tag_ids.clone(),
                mode: self.tag_mode,
            },
            camera: self.camera.clone(),
            lens: self.lens.clone(),
            file_types: self.file_types.clone(),
            iso: self.iso,
            aperture: self.aperture,
            focal: self.focal,
            date: self.date.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_source_maps_recursive_flag() {
        let fs = FilterState::default();
        let q = fs.to_query(ViewSource::Folder(7), true);
        assert_eq!(
            q.scope,
            Scope::Folder {
                id: 7,
                recursive: true
            }
        );
        let q = fs.to_query(ViewSource::Folder(7), false);
        assert_eq!(
            q.scope,
            Scope::Folder {
                id: 7,
                recursive: false
            }
        );
    }

    #[test]
    fn min_rating_zero_means_no_filter() {
        let fs = FilterState {
            min_rating: 0,
            ..Default::default()
        };
        assert!(fs.to_query(ViewSource::All, true).rating.is_none());
        let fs = FilterState {
            min_rating: 3,
            ..Default::default()
        };
        assert!(matches!(
            fs.to_query(ViewSource::All, true).rating,
            Some(RatingFilter::AtLeast(3))
        ));
    }

    #[test]
    fn rating_cmp_modes_map_to_correct_filter_variants() {
        // AtLeast (default)
        let fs = FilterState {
            min_rating: 4,
            rating_cmp: RatingCmp::AtLeast,
            ..Default::default()
        };
        assert!(matches!(
            fs.to_query(ViewSource::All, false).rating,
            Some(RatingFilter::AtLeast(4))
        ));

        // Exactly
        let fs = FilterState {
            min_rating: 4,
            rating_cmp: RatingCmp::Exactly,
            ..Default::default()
        };
        assert!(matches!(
            fs.to_query(ViewSource::All, false).rating,
            Some(RatingFilter::Exactly(4))
        ));

        // AtMost
        let fs = FilterState {
            min_rating: 4,
            rating_cmp: RatingCmp::AtMost,
            ..Default::default()
        };
        assert!(matches!(
            fs.to_query(ViewSource::All, false).rating,
            Some(RatingFilter::AtMost(4))
        ));
    }

    #[test]
    fn min_rating_zero_disables_filter_for_all_cmp_modes() {
        for cmp in [RatingCmp::AtLeast, RatingCmp::Exactly, RatingCmp::AtMost] {
            let fs = FilterState {
                min_rating: 0,
                rating_cmp: cmp,
                ..Default::default()
            };
            assert!(
                fs.to_query(ViewSource::All, false).rating.is_none(),
                "expected None for cmp={cmp:?} when min_rating=0"
            );
        }
    }

    #[test]
    fn blank_search_is_none() {
        let fs = FilterState {
            search: "   ".into(),
            ..Default::default()
        };
        assert!(fs.to_query(ViewSource::All, true).search.is_none());
        let fs = FilterState {
            search: "cat".into(),
            ..Default::default()
        };
        assert_eq!(
            fs.to_query(ViewSource::All, true).search.as_deref(),
            Some("cat")
        );
    }

    #[test]
    fn recently_added_source_maps_with_limit() {
        let q = FilterState::default().to_query(ViewSource::RecentlyAdded, true);
        assert!(matches!(q.scope, Scope::RecentlyAdded { limit } if limit > 0));
    }

    /// A folder view sorted by `AddedAt`/desc (set on folder-open, see
    /// `AppState::select_folder`) must compile to a normal `Folder` scope
    /// with an `added_at DESC` sort — not the limited `RecentlyAdded` scope.
    #[test]
    fn folder_source_with_added_at_desc_sorts_newest_first() {
        let fs = FilterState {
            sort_key: SortKey::AddedAt,
            sort_desc: true,
            ..Default::default()
        };
        let q = fs.to_query(ViewSource::Folder(7), true);
        assert_eq!(
            q.scope,
            Scope::Folder {
                id: 7,
                recursive: true
            }
        );
        assert_eq!(q.sort.key, SortKey::AddedAt);
        assert!(q.sort.desc);
    }

    #[test]
    fn to_query_forwards_lens_file_types_aperture_focal_verbatim() {
        let mut file_types = BTreeSet::new();
        file_types.insert(FileTypeChip::Jpeg);
        file_types.insert(FileTypeChip::Png);
        let fs = FilterState {
            lens: Some("Sigma 50mm f/1.4".into()),
            file_types: file_types.clone(),
            aperture: Some((2.8, 11.0)),
            focal: Some((24.0, 70.0)),
            ..Default::default()
        };
        let q = fs.to_query(ViewSource::All, true);
        assert_eq!(q.lens.as_deref(), Some("Sigma 50mm f/1.4"));
        assert_eq!(q.file_types, file_types);
        assert_eq!(q.aperture, Some((2.8, 11.0)));
        assert_eq!(q.focal, Some((24.0, 70.0)));
    }

    /// Step 1 (Task 8): `file_types` behaves as a genuine set — toggling a
    /// chip in and back out again must round-trip to the exact same query
    /// state, and an emptied set (the "all types" reset state) must never
    /// reach `to_query` as an active file-type predicate.
    #[test]
    fn file_types_toggle_behaves_as_a_set_and_empty_means_no_predicate() {
        let mut fs = FilterState::default();
        assert!(fs.to_query(ViewSource::All, true).file_types.is_empty());

        // Toggle a chip in: set gains exactly that member.
        fs.file_types.insert(FileTypeChip::Jpeg);
        let q = fs.to_query(ViewSource::All, true);
        assert_eq!(q.file_types.len(), 1);
        assert!(q.file_types.contains(&FileTypeChip::Jpeg));

        // Toggle a second, independent chip in: both are members (a set, not
        // a single-choice replacement).
        fs.file_types.insert(FileTypeChip::Raw);
        let q = fs.to_query(ViewSource::All, true);
        assert_eq!(q.file_types.len(), 2);
        assert!(q.file_types.contains(&FileTypeChip::Jpeg));
        assert!(q.file_types.contains(&FileTypeChip::Raw));

        // Toggle the first chip back out: only the second remains.
        fs.file_types.remove(&FileTypeChip::Jpeg);
        let q = fs.to_query(ViewSource::All, true);
        assert_eq!(q.file_types.len(), 1);
        assert!(q.file_types.contains(&FileTypeChip::Raw));

        // Toggle the last one out: empty set again, no predicate.
        fs.file_types.remove(&FileTypeChip::Raw);
        assert!(fs.to_query(ViewSource::All, true).file_types.is_empty());
    }

    #[test]
    fn to_query_defaults_forward_as_none_or_empty() {
        let q = FilterState::default().to_query(ViewSource::All, true);
        assert_eq!(q.lens, None);
        assert!(q.file_types.is_empty());
        assert_eq!(q.aperture, None);
        assert_eq!(q.focal, None);
    }

    #[test]
    fn range_to_filter_full_range_is_none() {
        // Handles sitting exactly at [min, max] (e.g. after a widget reset,
        // or an untouched filter) must never reach the query as a real
        // filter — the whole point of the None/Some split.
        assert_eq!(range_to_filter(50.0, 102_400.0, 50.0, 102_400.0), None);
        assert_eq!(range_to_filter(0.7, 32.0, 0.7, 32.0), None);
    }

    #[test]
    fn range_to_filter_narrowed_range_is_some() {
        assert_eq!(
            range_to_filter(100.0, 3200.0, 50.0, 102_400.0),
            Some((100.0, 3200.0))
        );
        // Narrowed on only one side still counts as active.
        assert_eq!(
            range_to_filter(50.0, 3200.0, 50.0, 102_400.0),
            Some((50.0, 3200.0))
        );
    }

    // ── is_default / reset_all (Task 9) ─────────────────────────────────────

    #[test]
    fn fresh_default_state_is_default() {
        assert!(FilterState::default().is_default());
    }

    #[test]
    fn search_text_flips_is_default_false() {
        let fs = FilterState {
            search: "cat".into(),
            ..Default::default()
        };
        assert!(!fs.is_default());
    }

    #[test]
    fn min_rating_flips_is_default_false() {
        let fs = FilterState {
            min_rating: 3,
            ..Default::default()
        };
        assert!(!fs.is_default());
    }

    #[test]
    fn a_flag_flips_is_default_false() {
        let fs = FilterState {
            flags: vec![Flag::Pick],
            ..Default::default()
        };
        assert!(!fs.is_default());
    }

    #[test]
    fn a_tag_flips_is_default_false() {
        let fs = FilterState {
            tag_ids: vec![TagId(1)],
            ..Default::default()
        };
        assert!(!fs.is_default());
    }

    #[test]
    fn non_any_tag_mode_flips_is_default_false() {
        // tag_mode=All with no tags selected is still a user-facing deviation
        // from the default (Any) — it would change results the instant a tag
        // is picked, so treat it as non-default on its own.
        let fs = FilterState {
            tag_mode: TagMode::All,
            ..Default::default()
        };
        assert!(!fs.is_default());
    }

    #[test]
    fn camera_flips_is_default_false() {
        let fs = FilterState {
            camera: Some("Sony A7IV".into()),
            ..Default::default()
        };
        assert!(!fs.is_default());
    }

    #[test]
    fn lens_flips_is_default_false() {
        let fs = FilterState {
            lens: Some("24-70mm f/2.8".into()),
            ..Default::default()
        };
        assert!(!fs.is_default());
    }

    #[test]
    fn iso_flips_is_default_false() {
        let fs = FilterState {
            iso: Some((100, 3200)),
            ..Default::default()
        };
        assert!(!fs.is_default());
    }

    #[test]
    fn aperture_flips_is_default_false() {
        let fs = FilterState {
            aperture: Some((2.8, 11.0)),
            ..Default::default()
        };
        assert!(!fs.is_default());
    }

    #[test]
    fn focal_flips_is_default_false() {
        let fs = FilterState {
            focal: Some((24.0, 70.0)),
            ..Default::default()
        };
        assert!(!fs.is_default());
    }

    #[test]
    fn date_flips_is_default_false() {
        let fs = FilterState {
            date: Some(("2026-01-01".into(), "2026-12-31".into())),
            ..Default::default()
        };
        assert!(!fs.is_default());
    }

    #[test]
    fn file_type_chip_flips_is_default_false() {
        let mut file_types = BTreeSet::new();
        file_types.insert(FileTypeChip::Raw);
        let fs = FilterState {
            file_types,
            ..Default::default()
        };
        assert!(!fs.is_default());
    }

    #[test]
    fn sort_order_alone_does_not_count_as_a_filter() {
        // Sort is a view preference, not a filter — is_default ignores it.
        let fs = FilterState {
            sort_key: SortKey::AddedAt,
            sort_desc: true,
            ..Default::default()
        };
        assert!(fs.is_default());
    }

    #[test]
    fn reset_all_restores_default_while_preserving_a_non_default_sort() {
        let mut file_types = BTreeSet::new();
        file_types.insert(FileTypeChip::Jpeg);
        let mut fs = FilterState {
            search: "cat".into(),
            sort_key: SortKey::AddedAt,
            sort_desc: true,
            min_rating: 4,
            rating_cmp: RatingCmp::Exactly,
            flags: vec![Flag::Pick],
            tag_ids: vec![TagId(1)],
            tag_mode: TagMode::All,
            camera: Some("Sony A7IV".into()),
            lens: Some("24-70mm f/2.8".into()),
            file_types,
            iso: Some((100, 3200)),
            aperture: Some((2.8, 11.0)),
            focal: Some((24.0, 70.0)),
            date: Some(("2026-01-01".into(), "2026-12-31".into())),
        };
        assert!(!fs.is_default());

        fs.reset_all();

        assert!(fs.is_default());
        // The non-default sort preference must survive the reset.
        assert_eq!(fs.sort_key, SortKey::AddedAt);
        assert!(fs.sort_desc);
    }
}

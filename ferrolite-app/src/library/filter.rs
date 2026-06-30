//! Pure mapping from toolbar UI state to a `LibraryQuery`. No egui here.

use ferrolite_catalog::{LibraryQuery, RatingFilter, Scope, Sort, SortKey, TagFilter, TagMode};
use ferrolite_image::{Flag, TagId};

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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub iso: Option<(u32, u32)>,
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
            iso: None,
            date: None,
        }
    }
}

impl FilterState {
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
            iso: self.iso,
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
}

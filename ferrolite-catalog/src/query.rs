//! A declarative, parameterised catalog query (filter + sort + search), compiled
//! to one `SELECT`. Pure: `compile()` is unit-tested without a database.

use crate::error::CatalogError;
use crate::model::ImageRecord;
use crate::queries::{IMAGE_COLS, THUMB_JOIN};
use ferrolite_image::{Flag, TagId};
use rusqlite::{types::Value, Connection};
use std::collections::BTreeSet;

/// A file-type filter chip. HEIC is not a member: `ferrolite-catalog`'s scanner
/// (`scan.rs`) never recognizes `.heic`/`.heif` as an ingestable extension, so a
/// chip for it could never match a row.
///
/// Matching is by lower-cased path extension (`filename`), not the `kind`
/// column: `kind` is only a 2-way `FileKind::{Raw, Standard}` split and cannot
/// distinguish Jpeg/Png/Tiff from each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum FileTypeChip {
    #[default]
    Raw,
    Jpeg,
    Png,
    Tiff,
}

impl FileTypeChip {
    /// Lower-cased, dot-less path extensions this chip matches. The ONE place
    /// the extension<->chip mapping lives, shared by the SQL predicate builder
    /// (`LibraryQuery::compile`, below) and any UI that renders/describes a
    /// chip — see `ferrolite-app/src/library/toolbar.rs`.
    ///
    /// `Raw`'s list is `scan.rs`'s own `RAW_EXTS` (not a duplicated copy), so
    /// the ingest classifier and this filter can never drift apart.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            FileTypeChip::Raw => crate::scan::RAW_EXTS,
            FileTypeChip::Jpeg => &["jpg", "jpeg"],
            FileTypeChip::Png => &["png"],
            FileTypeChip::Tiff => &["tif", "tiff"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Folder { id: i64, recursive: bool },
    AllPhotographs,
    Collection { id: i64 },
    RecentlyAdded { limit: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    CaptureTime,
    Filename,
    Rating,
    AddedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub desc: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatingFilter {
    AtLeast(u8),
    Exactly(u8),
    AtMost(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagMode {
    Any,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagFilter {
    pub ids: Vec<TagId>,
    pub mode: TagMode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LibraryQuery {
    pub scope: Scope,
    pub search: Option<String>,
    pub sort: Sort,
    pub rating: Option<RatingFilter>,
    pub flags: Vec<Flag>,
    pub tags: TagFilter,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub file_types: BTreeSet<FileTypeChip>,
    pub iso: Option<(u32, u32)>,
    pub aperture: Option<(f32, f32)>,
    pub focal: Option<(f32, f32)>,
    pub date: Option<(String, String)>,
}

impl Default for LibraryQuery {
    fn default() -> Self {
        LibraryQuery {
            scope: Scope::AllPhotographs,
            search: None,
            sort: Sort {
                key: SortKey::CaptureTime,
                desc: false,
            },
            rating: None,
            flags: Vec::new(),
            tags: TagFilter {
                ids: Vec::new(),
                mode: TagMode::Any,
            },
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

fn sort_column(key: SortKey) -> &'static str {
    match key {
        SortKey::CaptureTime => "capture_time",
        SortKey::Filename => "filename",
        SortKey::Rating => "rating",
        SortKey::AddedAt => "added_at",
    }
}

impl LibraryQuery {
    /// Compile to `(sql, params)`. All user input is bound as parameters — never
    /// interpolated — so the query is injection-safe.
    pub fn compile(&self) -> (String, Vec<Value>) {
        let mut params: Vec<Value> = Vec::new();
        let mut prefix = String::new();
        let mut joins = String::new();
        let mut where_clauses: Vec<String> = Vec::new();

        // RecentlyAdded short-circuits scope + ordering.
        if let Scope::RecentlyAdded { limit } = self.scope {
            let sql = format!(
                "SELECT {IMAGE_COLS} FROM images{THUMB_JOIN} WHERE added_at IS NOT NULL \
                 ORDER BY added_at DESC LIMIT ?"
            );
            params.push(Value::Integer(limit));
            return (sql, params);
        }

        match &self.scope {
            Scope::Folder { id, recursive } => {
                if *recursive {
                    prefix.push_str(
                        "WITH RECURSIVE subtree(id) AS (\
                         SELECT id FROM folders WHERE id = ? \
                         UNION ALL \
                         SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id) ",
                    );
                    params.push(Value::Integer(*id));
                    where_clauses.push("folder_id IN (SELECT id FROM subtree)".into());
                } else {
                    where_clauses.push("folder_id = ?".into());
                    params.push(Value::Integer(*id));
                }
            }
            Scope::Collection { id } => {
                joins.push_str(
                    " JOIN collection_images ci ON ci.image_id = images.id AND ci.collection_id = ?",
                );
                params.push(Value::Integer(*id));
            }
            Scope::AllPhotographs => {}
            Scope::RecentlyAdded { .. } => unreachable!(),
        }

        if let Some(rf) = self.rating {
            match rf {
                RatingFilter::AtLeast(n) => {
                    where_clauses.push("rating >= ?".into());
                    params.push(Value::Integer(n as i64));
                }
                RatingFilter::Exactly(n) => {
                    where_clauses.push("rating = ?".into());
                    params.push(Value::Integer(n as i64));
                }
                RatingFilter::AtMost(n) => {
                    where_clauses.push("rating <= ?".into());
                    params.push(Value::Integer(n as i64));
                }
            }
        }

        if !self.flags.is_empty() {
            let ph = vec!["?"; self.flags.len()].join(",");
            where_clauses.push(format!("flag IN ({ph})"));
            for f in &self.flags {
                params.push(Value::Integer(f.as_i64()));
            }
        }

        if !self.tags.ids.is_empty() {
            let ph = vec!["?"; self.tags.ids.len()].join(",");
            match self.tags.mode {
                TagMode::Any => {
                    where_clauses.push(format!(
                        "images.id IN (SELECT image_id FROM image_tags WHERE tag_id IN ({ph}))"
                    ));
                }
                TagMode::All => {
                    where_clauses.push(format!(
                        "images.id IN (SELECT image_id FROM image_tags WHERE tag_id IN ({ph}) \
                         GROUP BY image_id HAVING COUNT(DISTINCT tag_id) = {})",
                        self.tags.ids.len()
                    ));
                }
            }
            for t in &self.tags.ids {
                params.push(Value::Integer(t.0));
            }
        }

        if let Some(s) = &self.search {
            let like = format!("%{s}%");
            where_clauses.push(
                "(filename LIKE ? OR images.id IN \
                 (SELECT it.image_id FROM image_tags it JOIN tags t ON t.id = it.tag_id \
                 WHERE t.name LIKE ?))"
                    .into(),
            );
            params.push(Value::Text(like.clone()));
            params.push(Value::Text(like));
        }

        if let Some(cam) = &self.camera {
            where_clauses.push("camera_model = ?".into());
            params.push(Value::Text(cam.clone()));
        }

        if let Some(lens) = &self.lens {
            // NULL `lens` rows (unread/unsupported metadata, or ingested before
            // the v7 migration — see schema.rs) are excluded, same as any other
            // active metadata filter.
            where_clauses.push("lens = ?".into());
            params.push(Value::Text(lens.clone()));
        }

        if !self.file_types.is_empty() {
            // Matches on the lower-cased path extension (`filename`), not the
            // 2-way `kind` column — see `FileTypeChip::extensions`. One `LIKE`
            // per accepted extension across every selected chip, OR'd together
            // and parenthesized so it composes correctly with the `AND`-joined
            // clauses around it.
            let exts: Vec<&'static str> = self
                .file_types
                .iter()
                .flat_map(|chip| chip.extensions().iter().copied())
                .collect();
            let ph = vec!["LOWER(filename) LIKE ?"; exts.len()].join(" OR ");
            where_clauses.push(format!("({ph})"));
            for ext in exts {
                params.push(Value::Text(format!("%.{ext}")));
            }
        }

        if let Some((lo, hi)) = self.iso {
            where_clauses.push("iso BETWEEN ? AND ?".into());
            params.push(Value::Integer(lo as i64));
            params.push(Value::Integer(hi as i64));
        }

        if let Some((lo, hi)) = self.aperture {
            // NULL `aperture` rows are excluded (standard SQL BETWEEN
            // behavior — same NULL-exclusion note as `lens`, above).
            where_clauses.push("aperture BETWEEN ? AND ?".into());
            params.push(Value::Real(lo as f64));
            params.push(Value::Real(hi as f64));
        }

        if let Some((lo, hi)) = self.focal {
            // NULL `focal_length` rows are excluded, same as `aperture`.
            where_clauses.push("focal_length BETWEEN ? AND ?".into());
            params.push(Value::Real(lo as f64));
            params.push(Value::Real(hi as f64));
        }

        if let Some((from, to)) = &self.date {
            where_clauses.push("capture_time BETWEEN ? AND ?".into());
            params.push(Value::Text(from.clone()));
            params.push(Value::Text(to.clone()));
        }

        let mut sql = format!("{prefix}SELECT {IMAGE_COLS} FROM images{THUMB_JOIN}{joins}");
        if !where_clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&where_clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY ");
        sql.push_str(sort_column(self.sort.key));
        sql.push_str(if self.sort.desc { " DESC" } else { " ASC" });
        (sql, params)
    }
}

/// Execute a `LibraryQuery` against an open connection and return the matching rows.
pub(crate) fn run(conn: &Connection, q: &LibraryQuery) -> Result<Vec<ImageRecord>, CatalogError> {
    let (sql, params) = q.compile();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(params),
        crate::queries::row_to_record,
    )?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> LibraryQuery {
        LibraryQuery::default()
    }

    #[test]
    fn all_photographs_default_sort_has_no_where() {
        let q = LibraryQuery {
            scope: Scope::AllPhotographs,
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("FROM images"));
        assert!(!sql.contains("WHERE"), "no predicates → no WHERE: {sql}");
        assert!(sql.contains("ORDER BY"));
        assert!(params.is_empty());
    }

    #[test]
    fn folder_recursive_uses_subtree_cte() {
        let q = LibraryQuery {
            scope: Scope::Folder {
                id: 7,
                recursive: true,
            },
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("WITH RECURSIVE subtree"));
        assert!(sql.contains("folder_id IN (SELECT id FROM subtree)"));
        assert_eq!(params, vec![Value::Integer(7)]);
    }

    #[test]
    fn rating_flag_and_tags_any_compile_to_params() {
        let q = LibraryQuery {
            scope: Scope::AllPhotographs,
            rating: Some(RatingFilter::AtLeast(3)),
            flags: vec![Flag::Pick],
            tags: TagFilter {
                ids: vec![TagId(1), TagId(2)],
                mode: TagMode::Any,
            },
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("rating >= ?"));
        assert!(sql.contains("flag IN (?)"));
        assert!(sql.contains("image_tags WHERE tag_id IN (?,?)"));
        assert!(!sql.contains("HAVING"));
        assert_eq!(
            params,
            vec![
                Value::Integer(3),
                Value::Integer(1),
                Value::Integer(1),
                Value::Integer(2)
            ]
        );
    }

    #[test]
    fn tags_all_uses_having_count() {
        let q = LibraryQuery {
            scope: Scope::AllPhotographs,
            tags: TagFilter {
                ids: vec![TagId(1), TagId(2)],
                mode: TagMode::All,
            },
            ..base()
        };
        let (sql, _params) = q.compile();
        assert!(sql.contains("GROUP BY image_id HAVING COUNT(DISTINCT tag_id) = 2"));
    }

    #[test]
    fn search_matches_filename_or_tag_name() {
        let q = LibraryQuery {
            search: Some("port".into()),
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("filename LIKE ?"));
        assert!(sql.contains("t.name LIKE ?"));
        assert_eq!(
            params,
            vec![Value::Text("%port%".into()), Value::Text("%port%".into())]
        );
    }

    #[test]
    fn rating_exactly_compiles_to_eq() {
        let q = LibraryQuery {
            rating: Some(RatingFilter::Exactly(3)),
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("rating = ?"), "sql: {sql}");
        assert!(!sql.contains("rating >= ?"), "sql: {sql}");
        assert!(!sql.contains("rating <= ?"), "sql: {sql}");
        assert_eq!(params, vec![Value::Integer(3)]);
    }

    #[test]
    fn rating_at_most_compiles_to_lte() {
        let q = LibraryQuery {
            rating: Some(RatingFilter::AtMost(3)),
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("rating <= ?"), "sql: {sql}");
        assert!(!sql.contains("rating >= ?"), "sql: {sql}");
        assert!(!sql.contains("rating = ?"), "sql: {sql}");
        assert_eq!(params, vec![Value::Integer(3)]);
    }

    #[test]
    fn recently_added_orders_desc_with_limit() {
        let q = LibraryQuery {
            scope: Scope::RecentlyAdded { limit: 50 },
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("ORDER BY added_at DESC"));
        assert!(sql.contains("LIMIT ?"));
        assert_eq!(params, vec![Value::Integer(50)]);
    }

    /// A normal (unlimited) `Folder` scope must be able to sort by
    /// `added_at DESC` too — used when a folder is freshly opened so newly
    /// ingested thumbnails surface at the top, without going through the
    /// limited `RecentlyAdded` scope.
    #[test]
    fn folder_scope_can_sort_by_added_at_desc() {
        let q = LibraryQuery {
            scope: Scope::Folder {
                id: 7,
                recursive: false,
            },
            sort: Sort {
                key: SortKey::AddedAt,
                desc: true,
            },
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("folder_id = ?"), "sql: {sql}");
        assert!(sql.contains("ORDER BY added_at DESC"), "sql: {sql}");
        assert!(!sql.contains("LIMIT"), "folder scope must not be limited");
        assert_eq!(params, vec![Value::Integer(7)]);
    }

    #[test]
    fn not_seen_flag_compiles_to_flag_in_zero() {
        let q = LibraryQuery {
            flags: vec![Flag::None],
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("flag IN (?)"), "sql: {sql}");
        assert_eq!(params, vec![Value::Integer(0)]);
    }

    #[test]
    fn lens_filter_compiles_to_eq_param() {
        let q = LibraryQuery {
            lens: Some("50mm f/1.8".into()),
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("lens = ?"), "sql: {sql}");
        assert_eq!(params, vec![Value::Text("50mm f/1.8".into())]);
    }

    #[test]
    fn no_lens_filter_omits_predicate() {
        let (sql, _) = base().compile();
        assert!(!sql.contains("lens"), "sql: {sql}");
    }

    #[test]
    fn aperture_range_compiles_to_between() {
        let q = LibraryQuery {
            aperture: Some((2.8, 11.0)),
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("aperture BETWEEN ? AND ?"), "sql: {sql}");
        assert_eq!(
            params,
            vec![Value::Real(2.8_f32 as f64), Value::Real(11.0_f32 as f64)]
        );
    }

    #[test]
    fn focal_range_compiles_to_between() {
        let q = LibraryQuery {
            focal: Some((24.0, 70.0)),
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("focal_length BETWEEN ? AND ?"), "sql: {sql}");
        assert_eq!(
            params,
            vec![Value::Real(24.0_f32 as f64), Value::Real(70.0_f32 as f64)]
        );
    }

    #[test]
    fn empty_file_types_omits_predicate() {
        let (sql, _) = base().compile();
        assert!(!sql.contains("LOWER(filename)"), "sql: {sql}");
    }

    #[test]
    fn file_types_compile_to_ored_like_with_one_param_per_extension() {
        let mut file_types = BTreeSet::new();
        file_types.insert(FileTypeChip::Jpeg);
        file_types.insert(FileTypeChip::Png);
        let q = LibraryQuery {
            file_types,
            ..base()
        };
        let (sql, params) = q.compile();
        assert!(sql.contains("LOWER(filename) LIKE ?"), "sql: {sql}");
        // Jpeg -> jpg,jpeg (2 exts) + Png -> png (1 ext) = 3 placeholders/params.
        assert_eq!(params.len(), 3);
        assert!(params.contains(&Value::Text("%.jpg".into())));
        assert!(params.contains(&Value::Text("%.jpeg".into())));
        assert!(params.contains(&Value::Text("%.png".into())));
    }

    #[test]
    fn file_type_chip_extension_mapping_is_exhaustive_and_disjoint() {
        // The "ONE place" mapping (`FileTypeChip::extensions`) must cover every
        // chip with a non-empty, mutually exclusive extension list so a file
        // never matches two chips and the Raw list never drifts from the
        // scanner's own `RAW_EXTS`.
        let all = [
            FileTypeChip::Raw,
            FileTypeChip::Jpeg,
            FileTypeChip::Png,
            FileTypeChip::Tiff,
        ];
        let mut seen: Vec<&str> = Vec::new();
        for chip in all {
            let exts = chip.extensions();
            assert!(!exts.is_empty(), "{chip:?} has no extensions");
            for e in exts {
                assert!(
                    !seen.contains(e),
                    "extension {e} claimed by more than one chip"
                );
                seen.push(e);
            }
        }
        assert_eq!(FileTypeChip::Raw.extensions(), crate::scan::RAW_EXTS);
        assert_eq!(FileTypeChip::Jpeg.extensions(), &["jpg", "jpeg"]);
        assert_eq!(FileTypeChip::Png.extensions(), &["png"]);
        assert_eq!(FileTypeChip::Tiff.extensions(), &["tif", "tiff"]);
    }

    #[test]
    fn file_type_chip_default_is_raw() {
        assert_eq!(FileTypeChip::default(), FileTypeChip::Raw);
    }
}

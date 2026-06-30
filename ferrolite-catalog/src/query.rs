//! A declarative, parameterised catalog query (filter + sort + search), compiled
//! to one `SELECT`. Pure: `compile()` is unit-tested without a database.

use crate::error::CatalogError;
use crate::model::ImageRecord;
use crate::queries::IMAGE_COLS;
use ferrolite_image::{Flag, TagId};
use rusqlite::{types::Value, Connection};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryQuery {
    pub scope: Scope,
    pub search: Option<String>,
    pub sort: Sort,
    pub rating: Option<RatingFilter>,
    pub flags: Vec<Flag>,
    pub tags: TagFilter,
    pub camera: Option<String>,
    pub iso: Option<(u32, u32)>,
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
            iso: None,
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
                "SELECT {IMAGE_COLS} FROM images WHERE added_at IS NOT NULL \
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

        if let Some((lo, hi)) = self.iso {
            where_clauses.push("iso BETWEEN ? AND ?".into());
            params.push(Value::Integer(lo as i64));
            params.push(Value::Integer(hi as i64));
        }

        if let Some((from, to)) = &self.date {
            where_clauses.push("capture_time BETWEEN ? AND ?".into());
            params.push(Value::Text(from.clone()));
            params.push(Value::Text(to.clone()));
        }

        let mut sql = format!("{prefix}SELECT {IMAGE_COLS} FROM images{joins}");
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
}

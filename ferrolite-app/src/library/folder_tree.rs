//! Pure folder-tree construction for the left panel: turns the catalog's flat
//! `FolderRecord` list into a depth-ordered, roll-up-counted, render-ready list
//! honoring the user's expanded/collapsed set. No egui here (unit-tested).

use ferrolite_catalog::FolderRecord;
use std::collections::HashSet;

/// A render-ready tree row: indentation depth, roll-up count, expandability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderNode {
    pub id: i64,
    pub name: String,
    pub rollup_count: u64,
    pub depth: usize,
    pub has_children: bool,
}

fn children_of(folders: &[FolderRecord], parent: Option<i64>) -> Vec<&FolderRecord> {
    let mut kids: Vec<&FolderRecord> = folders.iter().filter(|f| f.parent_id == parent).collect();
    kids.sort_by(|a, b| a.path.cmp(&b.path));
    kids
}

/// Sum of `image_count` over `folder_id` and all its descendants.
pub fn subtree_count(folders: &[FolderRecord], folder_id: i64) -> u64 {
    let own = folders
        .iter()
        .find(|f| f.id == folder_id)
        .map(|f| f.image_count)
        .unwrap_or(0);
    let kids: u64 = folders
        .iter()
        .filter(|f| f.parent_id == Some(folder_id))
        .map(|f| subtree_count(folders, f.id))
        .sum();
    own + kids
}

fn leaf_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Flatten the forest into a depth-ordered list, descending only into folders
/// present in `expanded`. Roots = rows whose `parent_id` is `None` or points
/// outside the set.
pub fn flatten(folders: &[FolderRecord], expanded: &HashSet<i64>) -> Vec<FolderNode> {
    let id_set: HashSet<i64> = folders.iter().map(|f| f.id).collect();
    let mut out = Vec::new();
    // Roots: parent_id None or a parent not in this set.
    let roots: Vec<&FolderRecord> = {
        let mut r: Vec<&FolderRecord> = folders
            .iter()
            .filter(|f| f.parent_id.map(|p| !id_set.contains(&p)).unwrap_or(true))
            .collect();
        r.sort_by(|a, b| a.path.cmp(&b.path));
        r
    };
    for root in roots {
        push_node(folders, root, 0, expanded, &mut out);
    }
    out
}

fn push_node(
    folders: &[FolderRecord],
    node: &FolderRecord,
    depth: usize,
    expanded: &HashSet<i64>,
    out: &mut Vec<FolderNode>,
) {
    let kids = children_of(folders, Some(node.id));
    out.push(FolderNode {
        id: node.id,
        name: leaf_name(&node.path),
        rollup_count: subtree_count(folders, node.id),
        depth,
        has_children: !kids.is_empty(),
    });
    if expanded.contains(&node.id) {
        for kid in kids {
            push_node(folders, kid, depth + 1, expanded, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_catalog::FolderRecord;
    use std::collections::HashSet;

    fn rec(id: i64, path: &str, parent: Option<i64>, count: u64) -> FolderRecord {
        FolderRecord {
            id,
            path: path.into(),
            parent_id: parent,
            image_count: count,
        }
    }

    fn fixture() -> Vec<FolderRecord> {
        // root(1)[2 direct] -> 2024(2)[3], 2025(3)[5]; 2024 -> jan(4)[7]
        vec![
            rec(1, "/p", None, 2),
            rec(2, "/p/2024", Some(1), 3),
            rec(3, "/p/2025", Some(1), 5),
            rec(4, "/p/2024/jan", Some(2), 7),
        ]
    }

    #[test]
    fn subtree_count_rolls_up_descendants() {
        let f = fixture();
        assert_eq!(subtree_count(&f, 1), 2 + 3 + 5 + 7);
        assert_eq!(subtree_count(&f, 2), 3 + 7);
        assert_eq!(subtree_count(&f, 4), 7);
    }

    #[test]
    fn flatten_collapsed_root_hides_descendants() {
        let f = fixture();
        let expanded = HashSet::new(); // nothing expanded
        let nodes = flatten(&f, &expanded);
        assert_eq!(nodes.len(), 1, "only root shows when collapsed");
        assert_eq!(nodes[0].id, 1);
        assert_eq!(nodes[0].depth, 0);
        assert!(nodes[0].has_children);
        assert_eq!(nodes[0].rollup_count, 17);
    }

    #[test]
    fn flatten_expanded_shows_children_in_order() {
        let f = fixture();
        let expanded: HashSet<i64> = [1, 2].into_iter().collect();
        let nodes = flatten(&f, &expanded);
        let ids: Vec<i64> = nodes.iter().map(|n| n.id).collect();
        // root, 2024 (depth1), jan (depth2), 2025 (depth1) — sorted by path.
        assert_eq!(ids, vec![1, 2, 4, 3]);
        let jan = nodes.iter().find(|n| n.id == 4).unwrap();
        assert_eq!(jan.depth, 2);
        assert!(!jan.has_children);
    }
}

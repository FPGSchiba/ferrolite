//! Pure set math for the "Add/Remove to collection" context-menu submenus.
//! No egui, no DB — decides which collections are offered given membership.

use ferrolite_catalog::CollectionRecord;
use std::collections::HashMap;

#[allow(dead_code)] // wired into the context-menu submenus in a follow-up task
fn is_member(membership: &HashMap<i64, Vec<i64>>, image_id: i64, coll_id: i64) -> bool {
    membership
        .get(&image_id)
        .is_some_and(|v| v.contains(&coll_id))
}

/// Collections offered for "Add": at least one target is NOT already a member.
#[allow(dead_code)] // wired into the context-menu submenus in a follow-up task
pub fn addable_collections(
    all: &[CollectionRecord],
    target_ids: &[i64],
    membership: &HashMap<i64, Vec<i64>>,
) -> Vec<i64> {
    all.iter()
        .filter(|c| {
            target_ids
                .iter()
                .any(|&id| !is_member(membership, id, c.id))
        })
        .map(|c| c.id)
        .collect()
}

/// Collections offered for "Remove": at least one target IS a member.
#[allow(dead_code)] // wired into the context-menu submenus in a follow-up task
pub fn removable_collections(
    all: &[CollectionRecord],
    target_ids: &[i64],
    membership: &HashMap<i64, Vec<i64>>,
) -> Vec<i64> {
    all.iter()
        .filter(|c| target_ids.iter().any(|&id| is_member(membership, id, c.id)))
        .map(|c| c.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a CollectionRecord with the given id; fill remaining fields to match
    // the real struct (ferrolite-catalog/src/model.rs: id, name, color, sort_order).
    fn coll(id: i64) -> CollectionRecord {
        CollectionRecord {
            id,
            name: format!("c{id}"),
            color: Default::default(),
            sort_order: id,
        }
    }

    #[test]
    fn addable_excludes_collections_all_targets_already_in() {
        let all = vec![coll(1), coll(2), coll(3)];
        let mut m: HashMap<i64, Vec<i64>> = HashMap::new();
        m.insert(10, vec![1]);
        m.insert(11, vec![1, 2]);
        // Coll 1: both members -> excluded. Coll 2: 10 not in it -> addable. Coll 3: addable.
        assert_eq!(addable_collections(&all, &[10, 11], &m), vec![2, 3]);
    }

    #[test]
    fn removable_includes_collections_any_target_belongs_to() {
        let all = vec![coll(1), coll(2), coll(3)];
        let mut m: HashMap<i64, Vec<i64>> = HashMap::new();
        m.insert(10, vec![1]);
        m.insert(11, vec![1, 2]);
        assert_eq!(removable_collections(&all, &[10, 11], &m), vec![1, 2]);
    }
}

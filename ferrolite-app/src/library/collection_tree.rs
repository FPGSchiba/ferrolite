//! Collection tree rendering for the Library left panel: the hierarchical
//! collection list, rendered recursively to arbitrary nesting depth (bounded
//! defensively by `MAX_COLLECTION_TREE_DEPTH`), including drag-and-drop:
//!   - Dragging images from the grid onto a row (any depth) adds them to
//!     that collection (`crate::library::drag::row_drop_target`).
//!   - Dragging a collection row onto the "COLLECTIONS" root header un-nests
//!     it. This module's `show` owns the whole COLLECTIONS section — header,
//!     "+" button, the root-header drop target, and the un-nest write —
//!     `panel::show` only delegates to it, it does not render any of this
//!     itself.
//!   - Dragging a collection row onto another collection row (any depth)
//!     nests the dragged collection under the target — cycle-safe (see
//!     `crate::library::drag::would_create_cycle`). A rejected (cycle) drop
//!     makes no state change and flashes the target row red briefly instead.
//!
//! Every row, regardless of depth, is both a whole-row drag source
//! (`DraggedCollection`) and a drop target for both `DraggedCollection`
//! (nest) and `DraggedImages` (add-to-collection) — there is no
//! depth-limited "top level only" behavior.

use crate::library::drag::{self, DraggedCollection};
use crate::library::filter::ViewSource;
use crate::state::{AppState, RenameKind};
use crate::theme;
use ferrolite_catalog::CollectionRecord;
use std::collections::HashMap;

/// Render the "COLLECTIONS" section: header + "+" new-collection button,
/// the root-header drop target that un-nests a dropped collection, and the
/// collection tree itself.
pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.add_space(8.0);
    let collections_header_resp = ui
        .horizontal(|ui| {
            ui.label(
                egui::RichText::new("COLLECTIONS")
                    .color(theme::TEXT_DIM)
                    .size(10.0),
            );
            if ui.small_button("+").clicked() {
                let name = format!("Collection {}", state.collections.len() + 1);
                if state
                    .writer
                    .lock()
                    .expect("writer")
                    .create_collection(&name, ferrolite_image::Color::default())
                    .is_ok()
                {
                    state.reload_vocab();
                }
            }
        })
        .response;

    if ui.ctx().dragged_id().is_some() && ui.ctx().dragged_id() != Some(collections_header_resp.id)
    {
        if let Some(pointer) = ui.ctx().pointer_interact_pos() {
            if collections_header_resp.rect.contains(pointer) {
                ui.painter().rect_stroke(
                    collections_header_resp.rect,
                    2.0,
                    egui::Stroke::new(1.5_f32, theme::ACCENT),
                );
            }
        }
    }

    if let Some(dragged) = collections_header_resp.dnd_release_payload::<DraggedCollection>() {
        let res = state
            .writer
            .lock()
            .expect("writer")
            .update_collection_parent(dragged.0, None);
        if let Err(err) = res {
            eprintln!("failed to reparent collection: {err}");
        } else {
            state.reload_vocab();
            state.dirty = true;
        }
    }

    let collections = state.collections.clone();
    let tree = build_collection_tree(&collections);
    let raw_counts = state.reads.collection_image_counts().unwrap_or_default();
    let total_counts = compute_collection_counts(&collections, &raw_counts);
    // Child->parent map for cycle-safe re-parenting, built from the already-
    // loaded collection records (no extra DB read).
    let parent_of: HashMap<i64, Option<i64>> =
        collections.iter().map(|c| (c.id, c.parent_id)).collect();

    for node in &tree {
        render_collection_node(ui, state, node, 0, &total_counts, &parent_of);
    }
}

/// Delete a collection and clean up source / dirty state accordingly.
fn delete_collection(state: &mut AppState, collection_id: i64) {
    let _ = state
        .writer
        .lock()
        .expect("writer")
        .delete_collection(collection_id);
    if matches!(state.source, ViewSource::Collection(id) if id == collection_id) {
        state.source = ViewSource::All;
        state.current_folder = None;
        state.dirty = true;
    }
    state.reload_vocab();
}

/// Defensive cap on rendered/built collection-tree nesting depth. The
/// cycle-safe drop guard (`drag::would_create_cycle`) prevents the UI from
/// ever creating a cycle, but a directly-edited or otherwise corrupt catalog
/// DB could still contain a chain deeper than any real user would nest by
/// hand (or, in principle, a cycle isolated from every root). Recursion here
/// is bounded by this constant so a corrupt chain cannot stack-overflow tree
/// building or rendering: at this depth, remaining descendants are gathered
/// into one flat, non-nested batch of children (see `build_node`) instead of
/// nested further — every id is still preserved (see
/// `flatten_descendants`), just no longer indented per level. Because that
/// batch is still one `children` level under the capped node, the deepest
/// nodes in a built tree can visually sit at `MAX_COLLECTION_TREE_DEPTH + 1`
/// levels — recursion itself never goes any deeper than this constant.
const MAX_COLLECTION_TREE_DEPTH: usize = 32;

/// Indentation added per nesting level, in points — matches the single
/// child-indent step this module used before it supported depth > 1.
const CHILD_INDENT_STEP: f32 = 16.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionNode {
    pub collection: CollectionRecord,
    pub children: Vec<CollectionNode>,
}

/// Build the full collection tree to arbitrary depth from a flat list of
/// records (`CollectionRecord.parent_id` is the only relationship). Records
/// with no parent are roots; everything else nests under its `parent_id`,
/// recursively, capped at `MAX_COLLECTION_TREE_DEPTH` (see `build_node`).
///
/// Records left over after every reachable-from-a-root id has been claimed
/// have a `parent_id` that doesn't resolve to any root — a dangling
/// reference (e.g. the parent row was deleted without cleanup) or a cycle
/// isolated from every root. These are surfaced as top-level nodes too
/// (with no reconstructed children) rather than silently dropped, matching
/// this function's prior behavior for orphaned records.
pub fn build_collection_tree(collections: &[CollectionRecord]) -> Vec<CollectionNode> {
    let mut children_by_parent: HashMap<i64, Vec<CollectionRecord>> = HashMap::new();
    for c in collections {
        if let Some(pid) = c.parent_id {
            children_by_parent.entry(pid).or_default().push(c.clone());
        }
    }

    let mut top_level: Vec<CollectionNode> = Vec::new();
    for c in collections {
        if c.parent_id.is_none() {
            top_level.push(build_node(c.clone(), &mut children_by_parent, 1));
        }
    }

    for (_pid, orphans) in children_by_parent {
        for orphan in orphans {
            top_level.push(CollectionNode {
                collection: orphan,
                children: Vec::new(),
            });
        }
    }

    top_level
}

/// Recursively build one node and its descendants, consuming claimed
/// entries out of `children_by_parent` as it goes. Since each id can appear
/// in at most one bucket (keyed by that record's own fixed `parent_id`),
/// removing an id's bucket as it's visited means no id can be visited
/// twice — so this cannot loop even over corrupt/cyclic input; `depth` only
/// needs to bound recursion for genuinely deep chains.
fn build_node(
    collection: CollectionRecord,
    children_by_parent: &mut HashMap<i64, Vec<CollectionRecord>>,
    depth: usize,
) -> CollectionNode {
    let direct_children = children_by_parent
        .remove(&collection.id)
        .unwrap_or_default();

    if depth >= MAX_COLLECTION_TREE_DEPTH {
        let mut flattened = Vec::new();
        flatten_descendants(direct_children, children_by_parent, &mut flattened);
        return CollectionNode {
            collection,
            children: flattened,
        };
    }

    let children = direct_children
        .into_iter()
        .map(|child| build_node(child, children_by_parent, depth + 1))
        .collect();

    CollectionNode {
        collection,
        children,
    }
}

/// Iterative (not recursive) breadth-first flatten of everything under a
/// depth-capped node: pulls in every remaining descendant, however deep,
/// as flat leaf siblings instead of nesting them further. Used only once
/// `MAX_COLLECTION_TREE_DEPTH` is reached, so hitting the cap degrades to
/// "show the rest at one level" rather than truncating data or recursing
/// unboundedly.
fn flatten_descendants(
    mut queue: Vec<CollectionRecord>,
    children_by_parent: &mut HashMap<i64, Vec<CollectionRecord>>,
    out: &mut Vec<CollectionNode>,
) {
    while let Some(c) = queue.pop() {
        if let Some(mut grandchildren) = children_by_parent.remove(&c.id) {
            queue.append(&mut grandchildren);
        }
        out.push(CollectionNode {
            collection: c,
            children: Vec::new(),
        });
    }
}

/// Per-collection image counts, rolled up through ancestors: a node's total
/// is its own direct count plus the (already-rolled-up) totals of all its
/// children, recursively. This generalizes the original 2-level semantics
/// (`parent total = direct + sum(child direct counts)`, which held because
/// children never had children of their own) to arbitrary depth — a count
/// added anywhere propagates up through every ancestor, not just the
/// immediate parent.
pub fn compute_collection_counts(
    collections: &[CollectionRecord],
    image_counts: &HashMap<i64, usize>,
) -> HashMap<i64, usize> {
    let tree = build_collection_tree(collections);
    let mut result = HashMap::new();
    for node in &tree {
        compute_node_counts(node, image_counts, &mut result);
    }
    result
}

fn compute_node_counts(
    node: &CollectionNode,
    image_counts: &HashMap<i64, usize>,
    result: &mut HashMap<i64, usize>,
) -> usize {
    let mut total = image_counts.get(&node.collection.id).copied().unwrap_or(0);
    for child in &node.children {
        total += compute_node_counts(child, image_counts, result);
    }
    result.insert(node.collection.id, total);
    total
}

/// Render one collection row and, if expanded, recurse into its children —
/// to arbitrary depth (bounded by however deep `build_collection_tree`
/// actually built, itself capped at `MAX_COLLECTION_TREE_DEPTH`). Every row
/// gets the full row behavior regardless of depth: expand/collapse when it
/// has children, whole-row drag source, and drop target for both
/// `DraggedCollection` (nest) and `DraggedImages` (add-to-collection).
fn render_collection_node(
    ui: &mut egui::Ui,
    state: &mut AppState,
    node: &CollectionNode,
    depth: usize,
    total_counts: &HashMap<i64, usize>,
    parent_of: &HashMap<i64, Option<i64>>,
) {
    let c = &node.collection;
    let has_children = !node.children.is_empty();
    let count = total_counts.get(&c.id).copied().unwrap_or(0);
    let is_renaming = matches!(
        &state.renaming,
        Some((RenameKind::Collection, id, _)) if *id == c.id
    );
    let open = has_children && state.expanded_collections.contains(&c.id);

    let row_resp = ui
        .horizontal(|ui| {
            if depth > 0 {
                ui.add_space(CHILD_INDENT_STEP * depth as f32);
            }

            if has_children {
                let chev = if open {
                    crate::icons::CARET_DOWN
                } else {
                    crate::icons::CARET_RIGHT
                };
                if ui.selectable_label(false, chev).clicked() {
                    if open {
                        state.expanded_collections.remove(&c.id);
                    } else {
                        state.expanded_collections.insert(c.id);
                    }
                }

                // Set icon (layered boxes)
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                let p = ui.painter();
                let col = egui::Color32::from_rgb(c.color.r, c.color.g, c.color.b);
                p.rect_stroke(
                    egui::Rect::from_min_size(
                        rect.min + egui::vec2(2.0, 0.0),
                        egui::vec2(8.0, 7.0),
                    ),
                    1.0,
                    egui::Stroke::new(1.0_f32, theme::TEXT_DIM),
                );
                p.rect_filled(
                    egui::Rect::from_min_size(
                        rect.min + egui::vec2(0.0, 3.0),
                        egui::vec2(8.0, 7.0),
                    ),
                    1.0,
                    col,
                );
            } else {
                let col = egui::Color32::from_rgb(c.color.r, c.color.g, c.color.b);
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, col);
            }

            if is_renaming {
                let buf = match &mut state.renaming {
                    Some((RenameKind::Collection, id, buf)) if *id == c.id => buf,
                    _ => unreachable!(),
                };
                let edit_resp = ui.add(
                    egui::TextEdit::singleline(buf).desired_width(ui.available_width() - 20.0),
                );
                edit_resp.request_focus();
                let commit =
                    edit_resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter));
                if commit {
                    if let Some((RenameKind::Collection, id, buf)) = state.renaming.take() {
                        if !buf.is_empty() {
                            let _ = state
                                .writer
                                .lock()
                                .expect("writer")
                                .rename_collection(id, &buf);
                            state.reload_vocab();
                        }
                    }
                }
            } else {
                let name_resp = ui.selectable_label(
                    matches!(state.source, ViewSource::Collection(id) if id == c.id),
                    &c.name,
                );
                if name_resp.clicked() {
                    state.source = ViewSource::Collection(c.id);
                    state.current_folder = None;
                    state.dirty = true;
                }
                if name_resp.double_clicked() {
                    state.renaming = Some((RenameKind::Collection, c.id, c.name.clone()));
                }
                name_resp.context_menu(|ui| {
                    if ui.button("Add Sub-collection...").clicked() {
                        let sub_name = format!("Sub-collection {}", state.collections.len() + 1);
                        if crate::library::collection_menu::create_sub_collection(
                            &state.writer.lock().expect("writer"),
                            c.id,
                            &sub_name,
                        )
                        .is_ok()
                        {
                            state.expanded_collections.insert(c.id);
                            state.reload_vocab();
                        }
                        ui.close_menu();
                    }
                    if ui.button("Rename").clicked() {
                        state.renaming = Some((RenameKind::Collection, c.id, c.name.clone()));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Delete").clicked() {
                        delete_collection(state, c.id);
                        ui.close_menu();
                    }
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let x_slot = ui.allocate_response(egui::vec2(14.0, 14.0), egui::Sense::click());
                    if name_resp.hovered() || x_slot.hovered() {
                        let r = x_slot.rect.shrink(4.0);
                        let color = if x_slot.hovered() {
                            theme::TEXT_PRIMARY
                        } else {
                            theme::TEXT_DIM
                        };
                        let stroke = egui::Stroke::new(1.2_f32, color);
                        let p = ui.painter();
                        p.line_segment([r.left_top(), r.right_bottom()], stroke);
                        p.line_segment([r.left_bottom(), r.right_top()], stroke);
                    }
                    if x_slot.clicked() {
                        delete_collection(state, c.id);
                    }

                    ui.label(
                        egui::RichText::new(format!("({})", count))
                            .color(theme::TEXT_DIM)
                            .size(11.0),
                    );
                });
            }
        })
        .response;

    // `ui.horizontal(...)`'s own response only ever senses `hover` (see
    // `egui::Ui::allocate_new_ui_dyn`), so `drag_started()` can never fire
    // on it and `dnd_set_drag_payload` below would be a silent no-op.
    // Re-`interact` on the row's own id (not a new one) so this merges drag
    // sense onto its already-registered, early widget slot rather than
    // shadowing the row's children (see `egui::WidgetRects::insert` —
    // updates in place, keeping the row "behind" its later-registered,
    // click-only children in hit-test order). This mirrors how
    // `library::grid` makes image cells draggable via `ui.interact(rect,
    // id, Sense::click_and_drag())` before checking `drag_started()`.
    let row_resp = ui.interact(row_resp.rect, row_resp.id, egui::Sense::drag());
    row_resp.dnd_set_drag_payload(DraggedCollection(c.id));

    if ui.ctx().is_being_dragged(row_resp.id) {
        drag::draw_collection_drag_chip(ui.ctx(), &c.name);
    }

    if ui.ctx().dragged_id().is_some() && ui.ctx().dragged_id() != Some(row_resp.id) {
        if let Some(pointer) = ui.ctx().pointer_interact_pos() {
            if row_resp.rect.contains(pointer) {
                ui.painter().rect_stroke(
                    row_resp.rect,
                    2.0,
                    egui::Stroke::new(1.5_f32, theme::ACCENT),
                );
            }
        }
    }

    // Every row, at any depth, is a nest-by-drop target.
    if let Some(dragged_id) = drag::collection_drop_target(ui, row_resp.rect) {
        if drag::would_create_cycle(dragged_id, c.id, parent_of) {
            drag::flash_reject(ui.ctx(), c.id);
        } else {
            let res = state
                .writer
                .lock()
                .expect("writer")
                .update_collection_parent(dragged_id, Some(c.id));
            if let Err(err) = res {
                eprintln!("failed to reparent collection: {err}");
            } else {
                state.reload_vocab();
                state.dirty = true;
            }
        }
    }
    drag::paint_reject_flash(ui.ctx(), ui.painter(), row_resp.rect, c.id);

    if let Some(ids) = drag::row_drop_target(ui, row_resp.rect) {
        let (cid, cname) = (c.id, c.name.clone());
        state.add_images_to_collection(&ids, cid);
        state.notify(
            crate::notifications::Level::Info,
            format!("Added {} image(s) to \"{}\".", ids.len(), cname),
        );
    }

    if open {
        for child in &node.children {
            render_collection_node(ui, state, child, depth + 1, total_counts, parent_of);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_image::Color;

    #[test]
    fn test_collection_tree_building() {
        let c1 = CollectionRecord {
            id: 1,
            name: "Vacation 2026".to_string(),
            color: Color { r: 255, g: 0, b: 0 },
            sort_order: 0,
            parent_id: None,
        };
        let c2 = CollectionRecord {
            id: 2,
            name: "Beach".to_string(),
            color: Color { r: 0, g: 255, b: 0 },
            sort_order: 1,
            parent_id: Some(1),
        };
        let c3 = CollectionRecord {
            id: 3,
            name: "Mountains".to_string(),
            color: Color { r: 0, g: 0, b: 255 },
            sort_order: 2,
            parent_id: Some(1),
        };
        let c4 = CollectionRecord {
            id: 4,
            name: "Favorites".to_string(),
            color: Color {
                r: 255,
                g: 255,
                b: 0,
            },
            sort_order: 3,
            parent_id: None,
        };

        let collections = vec![c1.clone(), c2.clone(), c3.clone(), c4.clone()];
        let tree = build_collection_tree(&collections);

        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].collection.id, 1);
        assert_eq!(tree[0].children.len(), 2);
        assert_eq!(tree[0].children[0].collection.id, 2);
        assert_eq!(tree[0].children[1].collection.id, 3);
        assert_eq!(tree[1].collection.id, 4);
        assert_eq!(tree[1].children.len(), 0);
    }

    /// A parent -> child -> grandchild -> great-grandchild chain (depth 4)
    /// must build as one fully nested `CollectionNode` tree, not flatten
    /// after the first level — this is the core "nest more than one level"
    /// regression this feature closes.
    #[test]
    fn test_collection_tree_building_depth_four() {
        let make = |id: i64, parent_id: Option<i64>| CollectionRecord {
            id,
            name: format!("Node {id}"),
            color: Color::default(),
            sort_order: id,
            parent_id,
        };
        let collections = vec![
            make(1, None),
            make(2, Some(1)),
            make(3, Some(2)),
            make(4, Some(3)),
        ];

        let tree = build_collection_tree(&collections);

        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].collection.id, 1);
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].collection.id, 2);
        assert_eq!(tree[0].children[0].children.len(), 1);
        assert_eq!(tree[0].children[0].children[0].collection.id, 3);
        assert_eq!(tree[0].children[0].children[0].children.len(), 1);
        assert_eq!(tree[0].children[0].children[0].children[0].collection.id, 4);
        assert!(tree[0].children[0].children[0].children[0]
            .children
            .is_empty());
    }

    /// Recursive helper for tests: the greatest depth reached by any node in
    /// the tree (a lone root is depth 1).
    fn max_tree_depth(nodes: &[CollectionNode]) -> usize {
        nodes
            .iter()
            .map(|n| 1 + max_tree_depth(&n.children))
            .max()
            .unwrap_or(0)
    }

    /// Recursive helper for tests: total node count across the whole tree,
    /// used to confirm the depth cap flattens rather than drops data.
    fn count_tree_nodes(nodes: &[CollectionNode]) -> usize {
        nodes
            .iter()
            .map(|n| 1 + count_tree_nodes(&n.children))
            .sum()
    }

    /// A chain far deeper than `MAX_COLLECTION_TREE_DEPTH` must not stack
    /// overflow (or hang) building the tree, must respect the depth cap,
    /// and must not lose any records — everything past the cap is
    /// flattened into children of the capped node instead of dropped.
    #[test]
    fn test_collection_tree_depth_cap_flattens_without_losing_nodes() {
        let chain_len = MAX_COLLECTION_TREE_DEPTH + 20;
        let mut collections = Vec::with_capacity(chain_len);
        collections.push(CollectionRecord {
            id: 1,
            name: "Node 1".to_string(),
            color: Color::default(),
            sort_order: 1,
            parent_id: None,
        });
        for id in 2..=chain_len as i64 {
            collections.push(CollectionRecord {
                id,
                name: format!("Node {id}"),
                color: Color::default(),
                sort_order: id,
                parent_id: Some(id - 1),
            });
        }

        let tree = build_collection_tree(&collections);

        // Recursion is bounded by `MAX_COLLECTION_TREE_DEPTH`; the flattened
        // overflow batch still sits one `children` level under the capped
        // node (see the constant's doc comment), so the tree's rendered
        // depth is allowed to be one more than the cap itself — the
        // important invariant is that it's still a small constant bound,
        // not proportional to `chain_len`.
        assert_eq!(tree.len(), 1);
        assert!(
            max_tree_depth(&tree) <= MAX_COLLECTION_TREE_DEPTH + 1,
            "tree depth {} exceeded cap {} (+1 for the flattened batch)",
            max_tree_depth(&tree),
            MAX_COLLECTION_TREE_DEPTH
        );
        assert_eq!(
            count_tree_nodes(&tree),
            chain_len,
            "depth cap must flatten remaining descendants, not drop them"
        );
    }

    #[test]
    fn test_compute_collection_counts() {
        let c1 = CollectionRecord {
            id: 1,
            name: "Set 1".to_string(),
            color: Color::default(),
            sort_order: 0,
            parent_id: None,
        };
        let c2 = CollectionRecord {
            id: 2,
            name: "Sub 1".to_string(),
            color: Color::default(),
            sort_order: 1,
            parent_id: Some(1),
        };
        let c3 = CollectionRecord {
            id: 3,
            name: "Sub 2".to_string(),
            color: Color::default(),
            sort_order: 2,
            parent_id: Some(1),
        };
        let c4 = CollectionRecord {
            id: 4,
            name: "Single".to_string(),
            color: Color::default(),
            sort_order: 3,
            parent_id: None,
        };

        let collections = vec![c1, c2, c3, c4];
        let mut raw_counts = HashMap::new();
        raw_counts.insert(2, 5);
        raw_counts.insert(3, 3);
        raw_counts.insert(4, 10);

        let counts = compute_collection_counts(&collections, &raw_counts);
        assert_eq!(counts.get(&1), Some(&8));
        assert_eq!(counts.get(&2), Some(&5));
        assert_eq!(counts.get(&3), Some(&3));
        assert_eq!(counts.get(&4), Some(&10));
    }

    /// Counts must roll up through every ancestor, not just the immediate
    /// parent: root -> child -> grandchild (depth 3), each with its own
    /// direct images. The existing (level-2) semantics is `ancestor total =
    /// direct + sum(descendant direct counts)` — this asserts that holds
    /// unchanged at depth 3 rather than only summing one level down.
    #[test]
    fn test_compute_collection_counts_rolls_up_through_depth_three() {
        let root = CollectionRecord {
            id: 1,
            name: "Root".to_string(),
            color: Color::default(),
            sort_order: 0,
            parent_id: None,
        };
        let child = CollectionRecord {
            id: 2,
            name: "Child".to_string(),
            color: Color::default(),
            sort_order: 1,
            parent_id: Some(1),
        };
        let grandchild = CollectionRecord {
            id: 3,
            name: "Grandchild".to_string(),
            color: Color::default(),
            sort_order: 2,
            parent_id: Some(2),
        };
        let collections = vec![root, child, grandchild];

        let mut raw_counts = HashMap::new();
        raw_counts.insert(1, 2); // root's own direct images
        raw_counts.insert(2, 4); // child's own direct images
        raw_counts.insert(3, 8); // grandchild's own direct images

        let counts = compute_collection_counts(&collections, &raw_counts);
        assert_eq!(counts.get(&3), Some(&8)); // grandchild: leaf, direct only
        assert_eq!(counts.get(&2), Some(&12)); // child: 4 + grandchild's 8
        assert_eq!(counts.get(&1), Some(&14)); // root: 2 + child's rolled-up 12
    }

    #[test]
    fn test_collection_reparenting_and_vocab_reload() {
        let mut state = AppState::for_test();
        let (id1, id2) = {
            let writer = state.writer.lock().expect("writer");
            let id1 = writer.create_collection("Set A", Color::default()).unwrap();
            let id2 = writer
                .create_collection("Collection B", Color::default())
                .unwrap();
            (id1, id2)
        };

        state.reload_vocab();
        let tree = build_collection_tree(&state.collections);
        assert_eq!(tree.len(), 2);

        // Re-parent B under A
        {
            let writer = state.writer.lock().expect("writer");
            writer.update_collection_parent(id2, Some(id1)).unwrap();
        }

        state.reload_vocab();
        let tree = build_collection_tree(&state.collections);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].collection.id, id1);
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].collection.id, id2);

        // Un-nest B back to top-level
        {
            let writer = state.writer.lock().expect("writer");
            writer.update_collection_parent(id2, None).unwrap();
        }

        state.reload_vocab();
        let tree = build_collection_tree(&state.collections);
        assert_eq!(tree.len(), 2);
    }

    /// Ties `would_create_cycle` to the real `child->parent` map derived from
    /// `AppState::collections`, so the pure logic and the state shape it
    /// operates on in `show`/`render_collection_node` are exercised together.
    #[test]
    fn cycle_detected_using_real_collection_parent_map() {
        let mut state = AppState::for_test();
        let (id1, id2) = {
            let writer = state.writer.lock().expect("writer");
            let id1 = writer
                .create_collection("Parent", Color::default())
                .unwrap();
            let id2 = writer.create_collection("Child", Color::default()).unwrap();
            writer.update_collection_parent(id2, Some(id1)).unwrap();
            (id1, id2)
        };
        state.reload_vocab();

        let parent_of: HashMap<i64, Option<i64>> = state
            .collections
            .iter()
            .map(|c| (c.id, c.parent_id))
            .collect();

        // Dropping the parent onto its own child would create a cycle.
        assert!(drag::would_create_cycle(id1, id2, &parent_of));
        // Dropping the child onto its current parent is a no-op, not a cycle.
        assert!(!drag::would_create_cycle(id2, id1, &parent_of));
    }
}

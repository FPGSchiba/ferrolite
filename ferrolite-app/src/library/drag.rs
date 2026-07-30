//! Shared drag-and-drop payload + helpers for dragging Library grid images onto
//! collection/tag rows. Mirrors the export-queue DnD pattern (egui native
//! `DragAndDrop` payload + manual drop detection).

use std::collections::{HashMap, HashSet};

/// The images being dragged from the grid. egui `DragAndDrop` payload type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraggedImages(pub Vec<i64>);

/// A collection row being dragged (onto another collection row, to nest it,
/// or onto the Collections root header, to un-nest it). A distinct type from
/// `DraggedImages` and from the raw `i64` dnd payload the export queue uses
/// for its own reorder drags (`export_module::queue_list`) — egui's
/// `DragAndDrop` payload is matched by type, so without this newtype a
/// collection drag could be misread as an export-queue reorder drag (or vice
/// versa) if both ever hover the same frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DraggedCollection(pub i64);

/// Pure: true if making `dragged` a child of `target` would create a cycle —
/// i.e. `target` is `dragged` itself, or `target` is already a descendant of
/// `dragged` (walking up `target`'s ancestor chain reaches `dragged`).
/// Dropping onto `dragged`'s *current* parent is NOT a cycle: it's a no-op
/// move and is allowed.
///
/// Guards against a `parent_of` map that is itself corrupt — a cycle in the
/// ancestor chain that does NOT pass through `dragged` (this should never
/// happen via normal reparenting, since this function is exactly what
/// prevents it, but could arise from DB tampering, a second writer, or a
/// future bug elsewhere). Without a visited-set guard, walking such a map
/// would loop forever and hang the UI thread at drop time. When corrupt
/// ancestry is detected, the walk stops and this returns `true` — treating
/// it as an unsafe drop and refusing it is the safer choice over silently
/// allowing a write into unknown-shaped ancestry.
pub fn would_create_cycle(
    dragged: i64,
    target: i64,
    parent_of: &HashMap<i64, Option<i64>>,
) -> bool {
    if dragged == target {
        return true;
    }
    let mut cur = Some(target);
    let mut visited: HashSet<i64> = HashSet::new();
    while let Some(id) = cur {
        if id == dragged {
            return true;
        }
        if !visited.insert(id) {
            // Revisited a node without passing through `dragged`: the map's
            // ancestry is corrupt (a cycle elsewhere). Refuse rather than loop.
            return true;
        }
        cur = parent_of.get(&id).copied().flatten();
    }
    false
}

/// Which images a drag starting on `grabbed` should carry: the whole
/// multi-selection when `grabbed` is part of it, otherwise just `grabbed`.
/// Result is sorted ascending and never empty.
pub fn ids_for_drag(grabbed: i64, selection: &HashSet<i64>) -> Vec<i64> {
    if selection.contains(&grabbed) && selection.len() > 1 {
        let mut ids: Vec<i64> = selection.iter().copied().collect();
        ids.sort_unstable();
        ids
    } else {
        vec![grabbed]
    }
}

/// Paint a small chip that follows the cursor while a `DraggedImages` drag is
/// active, so the user sees how many images they're dragging. Drawn on a
/// foreground area layer so it sits above panels.
pub fn draw_drag_chip(ctx: &egui::Context, count: usize) {
    let Some(pos) = ctx.pointer_interact_pos() else {
        return;
    };
    let text = if count == 1 {
        "1 image".to_string()
    } else {
        format!("{count} images")
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("library_drag_chip"),
    ));
    let anchor = pos + egui::vec2(12.0, 8.0);
    let galley = painter.layout_no_wrap(
        text,
        egui::FontId::proportional(11.0),
        crate::theme::TEXT_PRIMARY,
    );
    let pad = egui::vec2(6.0, 3.0);
    let rect = egui::Rect::from_min_size(anchor, galley.size() + pad * 2.0);
    painter.rect_filled(rect, 3.0, crate::theme::ACCENT);
    painter.galley(anchor + pad, galley, crate::theme::TEXT_PRIMARY);
}

/// Paint a small chip that follows the cursor while a `DraggedCollection`
/// drag is active, naming the collection being moved. Mirrors
/// `draw_drag_chip`'s visual style and foreground-layer approach (same font,
/// padding, and accent fill) so collection drags read as the same affordance
/// family as image drags, just with a different id/text.
pub fn draw_collection_drag_chip(ctx: &egui::Context, name: &str) {
    let Some(pos) = ctx.pointer_interact_pos() else {
        return;
    };
    let text = format!("Moving \"{name}\"");
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("library_collection_drag_chip"),
    ));
    let anchor = pos + egui::vec2(12.0, 8.0);
    let galley = painter.layout_no_wrap(
        text,
        egui::FontId::proportional(11.0),
        crate::theme::TEXT_PRIMARY,
    );
    let pad = egui::vec2(6.0, 3.0);
    let rect = egui::Rect::from_min_size(anchor, galley.size() + pad * 2.0);
    painter.rect_filled(rect, 3.0, crate::theme::ACCENT);
    painter.galley(anchor + pad, galley, crate::theme::TEXT_PRIMARY);
}

/// Manual drop-target hit test for a left-panel row (egui 0.29.1 has no
/// `dnd_drop_zone`). While a `DraggedImages` drag hovers `row_rect`, paints a
/// highlight; on pointer release over the row, takes the payload and returns
/// the dragged ids. Returns `None` otherwise. Caller performs the action.
pub fn row_drop_target(ui: &egui::Ui, row_rect: egui::Rect) -> Option<Vec<i64>> {
    let _dragged = egui::DragAndDrop::payload::<DraggedImages>(ui.ctx())?;
    let pointer = ui.ctx().pointer_interact_pos()?;
    if !row_rect.contains(pointer) {
        return None;
    }
    // Highlight the row as a valid drop target (before any state mutation by caller).
    ui.painter().rect_filled(
        row_rect.expand2(egui::vec2(4.0, 1.0)),
        3.0,
        crate::theme::ACCENT_BG_SEL,
    );
    if ui.input(|i| i.pointer.any_released()) {
        return egui::DragAndDrop::take_payload::<DraggedImages>(ui.ctx()).map(|p| p.0.clone());
    }
    None
}

/// Manual drop-target hit test for a collection row accepting a
/// `DraggedCollection` payload (nesting one collection under another).
/// Mirrors `row_drop_target`'s pattern and highlight (same `ACCENT_BG_SEL`
/// fill) but for the collection payload type. Returns the dragged
/// collection's id on release over `row_rect`. The caller decides whether
/// the drop is cycle-safe (see `would_create_cycle`) before writing it.
pub fn collection_drop_target(ui: &egui::Ui, row_rect: egui::Rect) -> Option<i64> {
    let _dragged = egui::DragAndDrop::payload::<DraggedCollection>(ui.ctx())?;
    let pointer = ui.ctx().pointer_interact_pos()?;
    if !row_rect.contains(pointer) {
        return None;
    }
    ui.painter().rect_filled(
        row_rect.expand2(egui::vec2(4.0, 1.0)),
        3.0,
        crate::theme::ACCENT_BG_SEL,
    );
    if ui.input(|i| i.pointer.any_released()) {
        return egui::DragAndDrop::take_payload::<DraggedCollection>(ui.ctx()).map(|p| p.0);
    }
    None
}

/// How long a rejected-drop row flashes red for (`flash_reject` /
/// `paint_reject_flash`). No timers or threads: driven entirely by
/// `ui.input(|i| i.time)` plus a bounded `request_repaint_after` so the
/// flash reliably clears itself even without further input.
const REJECT_FLASH_SECS: f64 = 0.6;

fn reject_flash_id() -> egui::Id {
    egui::Id::new("library_collection_reject_flash")
}

/// Mark `row_id` as having just rejected a drop (e.g. a would-be cycle).
/// Call once, on release; `paint_reject_flash` renders the resulting flash
/// on every frame until it expires.
pub fn flash_reject(ctx: &egui::Context, row_id: i64) {
    let until = ctx.input(|i| i.time) + REJECT_FLASH_SECS;
    ctx.data_mut(|d| d.insert_temp(reject_flash_id(), (row_id, until)));
}

/// True if the stored `(row_id, until)` flash state means `row_id` should
/// currently be painted red, given the current time `now`. Pure — split out
/// from `paint_reject_flash` so the expiry/row-matching logic is testable
/// without an egui context.
fn is_flashing(flash: Option<(i64, f64)>, row_id: i64, now: f64) -> bool {
    matches!(flash, Some((id, until)) if id == row_id && until > now)
}

/// Paint `row_rect` with a red-tinted fill if `row_id` is currently flashing
/// from a rejected drop (see `flash_reject`); no-op otherwise. Requests a
/// follow-up repaint so the flash disappears on schedule even with no other
/// input driving frames.
pub fn paint_reject_flash(
    ctx: &egui::Context,
    painter: &egui::Painter,
    row_rect: egui::Rect,
    row_id: i64,
) {
    let flash = ctx.data(|d| d.get_temp::<(i64, f64)>(reject_flash_id()));
    let now = ctx.input(|i| i.time);
    if !is_flashing(flash, row_id, now) {
        return;
    }
    let until = flash.expect("is_flashing confirmed Some").1;
    let red = crate::theme::SEMANTIC_RED;
    painter.rect_filled(
        row_rect.expand2(egui::vec2(4.0, 1.0)),
        3.0,
        egui::Color32::from_rgba_unmultiplied(red.r(), red.g(), red.b(), 90),
    );
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(until - now));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grabbed_in_multiselection_drags_all_sorted() {
        let sel: HashSet<i64> = [3, 1, 2].into_iter().collect();
        assert_eq!(ids_for_drag(2, &sel), vec![1, 2, 3]);
    }

    #[test]
    fn grabbed_outside_selection_drags_only_itself() {
        let sel: HashSet<i64> = [1, 2].into_iter().collect();
        assert_eq!(ids_for_drag(9, &sel), vec![9]);
    }

    #[test]
    fn single_selection_drags_only_grabbed() {
        // A lone selected image drags just itself (len==1 → not "multi").
        let sel: HashSet<i64> = [5].into_iter().collect();
        assert_eq!(ids_for_drag(5, &sel), vec![5]);
    }

    #[test]
    fn self_drop_is_a_cycle() {
        let parent_of: HashMap<i64, Option<i64>> = HashMap::new();
        assert!(would_create_cycle(1, 1, &parent_of));
    }

    #[test]
    fn dropping_onto_own_child_is_a_cycle() {
        // 2's parent is 1: making 1 a child of 2 would cycle.
        let parent_of: HashMap<i64, Option<i64>> = [(2, Some(1))].into_iter().collect();
        assert!(would_create_cycle(1, 2, &parent_of));
    }

    #[test]
    fn dropping_onto_own_grandchild_is_a_cycle() {
        // 3's parent is 2, 2's parent is 1: making 1 a child of 3 would cycle.
        let parent_of: HashMap<i64, Option<i64>> =
            [(2, Some(1)), (3, Some(2))].into_iter().collect();
        assert!(would_create_cycle(1, 3, &parent_of));
    }

    #[test]
    fn dropping_onto_sibling_is_not_a_cycle() {
        let parent_of: HashMap<i64, Option<i64>> =
            [(2, Some(1)), (3, Some(1))].into_iter().collect();
        assert!(!would_create_cycle(2, 3, &parent_of));
    }

    #[test]
    fn dropping_onto_unrelated_node_is_not_a_cycle() {
        let parent_of: HashMap<i64, Option<i64>> = [(2, None), (3, None)].into_iter().collect();
        assert!(!would_create_cycle(2, 3, &parent_of));
    }

    #[test]
    fn dropping_onto_current_parent_is_not_a_cycle() {
        // 2's current parent is 1; re-dropping 2 onto 1 is a no-op move.
        let parent_of: HashMap<i64, Option<i64>> = [(2, Some(1))].into_iter().collect();
        assert!(!would_create_cycle(2, 1, &parent_of));
    }

    #[test]
    fn corrupted_ancestor_cycle_not_involving_dragged_is_refused_without_hanging() {
        // 10 and 11 point at each other — a corrupt parent map that should
        // never arise from normal reparenting (this function is what
        // prevents it), but could come from DB tampering, a second writer,
        // or a future bug elsewhere. Dragging unrelated `1` onto `10` must
        // not loop forever walking this corrupt ancestry; the chosen safe
        // semantics is to refuse the drop (treat as a cycle).
        let parent_of: HashMap<i64, Option<i64>> =
            [(10, Some(11)), (11, Some(10))].into_iter().collect();
        assert!(would_create_cycle(1, 10, &parent_of));
    }

    #[test]
    fn is_flashing_true_for_matching_row_before_expiry() {
        assert!(is_flashing(Some((5, 10.0)), 5, 9.9));
    }

    #[test]
    fn is_flashing_false_for_different_row() {
        assert!(!is_flashing(Some((5, 10.0)), 7, 9.9));
    }

    #[test]
    fn is_flashing_false_after_expiry() {
        assert!(!is_flashing(Some((5, 10.0)), 5, 10.1));
    }

    #[test]
    fn is_flashing_false_when_nothing_flashing() {
        assert!(!is_flashing(None, 5, 0.0));
    }
}

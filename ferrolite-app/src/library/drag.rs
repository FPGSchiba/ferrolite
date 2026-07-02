//! Shared drag-and-drop payload + helpers for dragging Library grid images onto
//! collection/tag rows. Mirrors the export-queue DnD pattern (egui native
//! `DragAndDrop` payload + manual drop detection).

use std::collections::HashSet;

/// The images being dragged from the grid. egui `DragAndDrop` payload type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraggedImages(pub Vec<i64>);

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
}

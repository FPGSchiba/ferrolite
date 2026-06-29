//! Library left panel: Catalog header, Open-folder action, and the folder tree
//! (indented, expandable, roll-up counts) read from the catalog. A ✕ on hover
//! and a right-click "Remove" trigger folder removal (subtree-confirm via state).

use crate::ingest::spawn_ingest;
use crate::library::folder_tree::{flatten, subtree_count};
use crate::state::{AppState, PendingRemove};
use crate::theme;

pub fn show(ui: &mut egui::Ui, state: &mut AppState, ctx: &egui::Context) {
    ui.add_space(8.0);
    ui.colored_label(theme::TEXT_FAINT, "CATALOG");
    ui.label("All Photographs");
    ui.add_space(8.0);

    if ui.button("Open folder…").clicked() {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            spawn_ingest(state, ctx, folder);
        }
    }

    ui.add_space(12.0);
    ui.colored_label(theme::TEXT_FAINT, "FOLDERS");

    let folders = state.reads.list_folders().unwrap_or_default();
    let nodes = flatten(&folders, &state.expanded_folders);

    for node in nodes {
        let node_path = folders
            .iter()
            .find(|f| f.id == node.id)
            .map(|f| f.path.clone())
            .unwrap_or_default();

        ui.horizontal(|ui| {
            ui.add_space(node.depth as f32 * 14.0);

            // Disclosure triangle — painted (egui's native rotating icon), never a
            // font glyph. Non-expandable rows reserve the same 14px width.
            if node.has_children {
                let open = state.expanded_folders.contains(&node.id);
                let resp = ui.allocate_response(egui::vec2(14.0, 14.0), egui::Sense::click());
                let openness = if open { 1.0 } else { 0.0 };
                egui::collapsing_header::paint_default_icon(ui, openness, &resp);
                if resp.clicked() {
                    if open {
                        state.expanded_folders.remove(&node.id);
                    } else {
                        state.expanded_folders.insert(node.id);
                    }
                }
            } else {
                ui.add_space(14.0);
            }

            let selected = state.current_folder == Some(node.id);
            let label = format!("{}  ({})", node.name, node.rollup_count);
            let resp = ui.selectable_label(selected, label);
            if resp.clicked() {
                state.select_folder(node.id);
            }
            resp.context_menu(|ui| {
                if ui.button("Reindex — new files").clicked() {
                    crate::ingest::spawn_reindex(
                        state,
                        ctx,
                        std::path::PathBuf::from(&node_path),
                        crate::ingest::ReindexMode::Incremental,
                    );
                    ui.close_menu();
                }
                if ui.button("Reindex — full rebuild").clicked() {
                    crate::ingest::spawn_reindex(
                        state,
                        ctx,
                        std::path::PathBuf::from(&node_path),
                        crate::ingest::ReindexMode::Full,
                    );
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Remove from catalog").clicked() {
                    request_remove(state, &folders, node.id, &node.name);
                    ui.close_menu();
                }
            });

            // Remove ✕ — always reserve a 14px slot (no hover relayout); paint an
            // X (two line segments) only when the row or slot is hovered.
            let x_slot = ui.allocate_response(egui::vec2(14.0, 14.0), egui::Sense::click());
            if resp.hovered() || x_slot.hovered() {
                let r = x_slot.rect.shrink(4.0);
                let color = if x_slot.hovered() {
                    theme::TEXT_PRIMARY
                } else {
                    theme::TEXT_DIM
                };
                let stroke = egui::Stroke::new(1.2, color);
                let p = ui.painter();
                p.line_segment([r.left_top(), r.right_bottom()], stroke);
                p.line_segment([r.left_bottom(), r.right_top()], stroke);
            }
            if x_slot.clicked() {
                request_remove(state, &folders, node.id, &node.name);
            }
        });
    }
}

/// A leaf folder removes immediately; one with subfolders stages a confirm.
fn request_remove(
    state: &mut AppState,
    folders: &[ferrolite_catalog::FolderRecord],
    id: i64,
    name: &str,
) {
    let has_children = folders.iter().any(|f| f.parent_id == Some(id));
    if has_children {
        state.pending_remove = Some(PendingRemove {
            id,
            name: name.to_string(),
            subtree_count: subtree_count(folders, id),
        });
    } else {
        state.remove_folder_cascade(id);
    }
}

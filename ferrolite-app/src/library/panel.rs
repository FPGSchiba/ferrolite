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
        ui.horizontal(|ui| {
            ui.add_space(node.depth as f32 * 14.0);

            // Expand/collapse triangle (only when the node has children).
            if node.has_children {
                let open = state.expanded_folders.contains(&node.id);
                let arrow = if open { "▾" } else { "▸" };
                if ui.add(egui::Button::new(arrow).frame(false)).clicked() {
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
            // Right-click context menu → Remove.
            resp.context_menu(|ui| {
                if ui.button("Remove from catalog").clicked() {
                    request_remove(state, &folders, node.id, &node.name);
                    ui.close_menu();
                }
            });
            // Hover ✕.
            if resp.hovered() && ui.small_button("✕").clicked() {
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

//! Library left panel: Catalog sources, Open-folder action, folder tree,
//! Collections list, and Tag manager. A ✕ on hover and a right-click "Remove"
//! trigger folder removal (subtree-confirm via state).

use crate::ingest::spawn_ingest;
use crate::library::filter::ViewSource;
use crate::library::folder_tree::{flatten, subtree_count};
use crate::state::{AppState, PendingRemove, RenameKind};
use crate::theme;

pub fn show(ui: &mut egui::Ui, state: &mut AppState, ctx: &egui::Context) {
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("CATALOG")
            .color(theme::TEXT_DIM)
            .size(10.0),
    );
    if ui
        .selectable_label(matches!(state.source, ViewSource::All), "All Photographs")
        .clicked()
    {
        state.source = ViewSource::All;
        state.current_folder = None;
        state.dirty = true;
    }
    if ui
        .selectable_label(
            matches!(state.source, ViewSource::RecentlyAdded),
            "Recently Added",
        )
        .clicked()
    {
        state.source = ViewSource::RecentlyAdded;
        state.current_folder = None;
        state.dirty = true;
    }
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
            // Tighter gap between the disclosure cell and the name (the item
            // spacing is the dominant remaining gap once the cell hugs the icon).
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.add_space(node.depth as f32 * 14.0);

            // Disclosure triangle — painted (egui's native rotating icon), never a
            // font glyph. The click cell is sized to the icon (8px, matching the
            // leaf-row `add_space` below so labels stay column-aligned) so the
            // triangle hugs the name instead of floating in an oversized box.
            if node.has_children {
                let open = state.expanded_folders.contains(&node.id);
                let resp = ui.allocate_response(egui::vec2(8.0, 8.0), egui::Sense::click());
                let openness = if open { 1.0 } else { 0.0 };
                // Hover changes the triangle's colour (via fg_stroke) but must not
                // change its size: paint with the widget expansion zeroed so it
                // doesn't grow on hover.
                ui.scope(|ui| {
                    let w = &mut ui.style_mut().visuals.widgets;
                    w.inactive.expansion = 0.0;
                    w.hovered.expansion = 0.0;
                    w.active.expansion = 0.0;
                    w.open.expansion = 0.0;
                    egui::collapsing_header::paint_default_icon(ui, openness, &resp);
                });
                if resp.clicked() {
                    if open {
                        state.expanded_folders.remove(&node.id);
                    } else {
                        state.expanded_folders.insert(node.id);
                    }
                }
            } else {
                ui.add_space(8.0);
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
                        node_path.clone().into(),
                        crate::ingest::ReindexMode::Incremental,
                    );
                    ui.close_menu();
                }
                if ui.button("Reindex — full rebuild").clicked() {
                    crate::ingest::spawn_reindex(
                        state,
                        ctx,
                        node_path.clone().into(),
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

    // ── Collections ──────────────────────────────────────────────────────────
    ui.add_space(8.0);
    ui.horizontal(|ui| {
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
    });
    let collections = state.collections.clone();
    for c in &collections {
        // Snapshot whether this collection is actively being renamed.
        let is_renaming = matches!(
            &state.renaming,
            Some((RenameKind::Collection, id, _)) if *id == c.id
        );

        ui.horizontal(|ui| {
            let col = egui::Color32::from_rgb(c.color.r, c.color.g, c.color.b);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, col);

            if is_renaming {
                // Inline rename TextEdit for collection.
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
                // Normal clickable label + context menu.
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

                // Delete ✕ affordance (mirrors folder rows).
                let x_slot = ui.allocate_response(egui::vec2(14.0, 14.0), egui::Sense::click());
                if name_resp.hovered() || x_slot.hovered() {
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
                    delete_collection(state, c.id);
                }
            }
        });
    }

    // ── Tags ─────────────────────────────────────────────────────────────────
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("TAGS")
                .color(theme::TEXT_DIM)
                .size(10.0),
        );
        if ui.small_button("+").clicked() {
            let name = format!("tag{}", state.tags.len() + 1);
            if state
                .writer
                .lock()
                .expect("writer")
                .create_tag(&name, ferrolite_image::Color::default())
                .is_ok()
            {
                state.reload_vocab();
            }
        }
    });
    let tags = state.tags.clone();
    for t in &tags {
        // Snapshot whether this tag is actively being renamed.
        let is_renaming = matches!(
            &state.renaming,
            Some((RenameKind::Tag, id, _)) if *id == t.id.0
        );

        ui.horizontal(|ui| {
            let mut col = [
                t.color.r as f32 / 255.0,
                t.color.g as f32 / 255.0,
                t.color.b as f32 / 255.0,
            ];
            if ui.color_edit_button_rgb(&mut col).changed() {
                let c = ferrolite_image::Color {
                    r: (col[0] * 255.0) as u8,
                    g: (col[1] * 255.0) as u8,
                    b: (col[2] * 255.0) as u8,
                };
                let _ = state.writer.lock().expect("writer").set_tag_color(t.id, c);
                state.reload_vocab();
            }

            if is_renaming {
                // Inline rename TextEdit for tag.
                let buf = match &mut state.renaming {
                    Some((RenameKind::Tag, id, buf)) if *id == t.id.0 => buf,
                    _ => unreachable!(),
                };
                let edit_resp = ui.add(
                    egui::TextEdit::singleline(buf).desired_width(ui.available_width() - 20.0),
                );
                edit_resp.request_focus();
                let commit =
                    edit_resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter));
                if commit {
                    if let Some((RenameKind::Tag, _, buf)) = state.renaming.take() {
                        if !buf.is_empty() {
                            let _ = state.writer.lock().expect("writer").rename_tag(t.id, &buf);
                            state.reload_vocab();
                        }
                    }
                }
            } else {
                // Normal label + context menu + painted delete ✕.
                let name_resp = ui.label(&t.name);
                if name_resp.double_clicked() {
                    state.renaming = Some((RenameKind::Tag, t.id.0, t.name.clone()));
                }
                name_resp.context_menu(|ui| {
                    if ui.button("Rename").clicked() {
                        state.renaming = Some((RenameKind::Tag, t.id.0, t.name.clone()));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Delete").clicked() {
                        let _ = state.writer.lock().expect("writer").delete_tag(t.id);
                        state.filter.tag_ids.retain(|x| *x != t.id);
                        state.reload_vocab();
                        state.dirty = true;
                        ui.close_menu();
                    }
                });

                // Delete ✕ affordance — two line segments, consistent with folder rows.
                let x_slot = ui.allocate_response(egui::vec2(14.0, 14.0), egui::Sense::click());
                if name_resp.hovered() || x_slot.hovered() {
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
                    let _ = state.writer.lock().expect("writer").delete_tag(t.id);
                    state.filter.tag_ids.retain(|x| *x != t.id);
                    state.reload_vocab();
                    state.dirty = true;
                }
            }
        });
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

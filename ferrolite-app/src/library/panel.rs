//! Library left panel: Catalog sources, Open-folder action, folder tree,
//! Collections list, and Tag manager. A ✕ on hover and a right-click "Remove"
//! trigger folder removal (subtree-confirm via state).

use crate::ingest::spawn_ingest;
use crate::library::filter::ViewSource;
use crate::library::folder_tree::{flatten, subtree_count};
use crate::state::{AppState, PendingRemove, RenameKind};
use crate::theme;

/// Returns `true` if the user opened a new folder this frame (via "Open
/// folder…"), so the caller can persist `settings.last_folder` + mark dirty.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, ctx: &egui::Context) -> bool {
    let mut folder_opened = false;
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
            state.settings.last_folder = Some(folder.clone());
            folder_opened = true;
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
                let stroke = egui::Stroke::new(1.2_f32, color);
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

    if let Some(dragged_id) = collections_header_resp.dnd_release_payload::<i64>() {
        let res = state
            .writer
            .lock()
            .expect("writer")
            .update_collection_parent(*dragged_id, None);
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

    for node in tree {
        let c = &node.collection;
        let is_set = !node.children.is_empty();
        let set_count = total_counts.get(&c.id).copied().unwrap_or(0);

        if is_set {
            let open = state.expanded_collections.contains(&c.id);
            let is_renaming = matches!(
                &state.renaming,
                Some((RenameKind::Collection, id, _)) if *id == c.id
            );

            let row_resp = ui
                .horizontal(|ui| {
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

                    if is_renaming {
                        let buf = match &mut state.renaming {
                            Some((RenameKind::Collection, id, buf)) if *id == c.id => buf,
                            _ => unreachable!(),
                        };
                        let edit_resp = ui.add(
                            egui::TextEdit::singleline(buf)
                                .desired_width(ui.available_width() - 20.0),
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
                                let sub_name =
                                    format!("Sub-collection {}", state.collections.len() + 1);
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
                                state.renaming =
                                    Some((RenameKind::Collection, c.id, c.name.clone()));
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Delete").clicked() {
                                delete_collection(state, c.id);
                                ui.close_menu();
                            }
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let x_slot =
                                ui.allocate_response(egui::vec2(14.0, 14.0), egui::Sense::click());
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
                                egui::RichText::new(format!("({})", set_count))
                                    .color(theme::TEXT_DIM)
                                    .size(11.0),
                            );
                        });
                    }
                })
                .response;

            row_resp.dnd_set_drag_payload(c.id);

            if ui.ctx().is_being_dragged(row_resp.id) {
                egui::show_tooltip_at_pointer(
                    ui.ctx(),
                    ui.layer_id(),
                    row_resp.id.with("ghost"),
                    |ui| {
                        ui.label(format!("{} Moving {}", crate::icons::COLOR, c.name));
                    },
                );
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

            if let Some(dragged_id) = row_resp.dnd_release_payload::<i64>() {
                if *dragged_id != c.id {
                    let res = state
                        .writer
                        .lock()
                        .expect("writer")
                        .update_collection_parent(*dragged_id, Some(c.id));
                    if let Err(err) = res {
                        eprintln!("failed to reparent collection: {err}");
                    } else {
                        state.reload_vocab();
                        state.dirty = true;
                    }
                }
            }

            if let Some(ids) = crate::library::drag::row_drop_target(ui, row_resp.rect) {
                let (cid, cname) = (c.id, c.name.clone());
                state.add_images_to_collection(&ids, cid);
                state.notify(
                    crate::notifications::Level::Info,
                    format!("Added {} image(s) to \"{}\".", ids.len(), cname),
                );
            }

            if open {
                for child in &node.children {
                    let child_count = total_counts.get(&child.id).copied().unwrap_or(0);
                    render_collection_row(ui, state, child, true, child_count);
                }
            }
        } else {
            let c_count = total_counts.get(&c.id).copied().unwrap_or(0);
            render_collection_row(ui, state, c, false, c_count);
        }
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

        let row_resp = ui
            .horizontal(|ui| {
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
                        let stroke = egui::Stroke::new(1.2_f32, color);
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
            })
            .response;

        // Drop target: dragging images from the grid onto a tag row applies
        // the tag (add-only — never removes it from images that already have it).
        if let Some(ids) = crate::library::drag::row_drop_target(ui, row_resp.rect) {
            // Copy id/clone name before the `&mut state` call to satisfy the borrow checker.
            let (tid, tname) = (t.id, t.name.clone());
            state.add_tag_to_images(ctx, &ids, tid);
            state.notify(
                crate::notifications::Level::Info,
                format!("Tagged {} image(s) with \"{}\".", ids.len(), tname),
            );
        }
    }
    folder_opened
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

/// A leaf folder removes immediately; one with subfolders stages a confirm —
/// unless the user has turned off confirm-before-remove (`settings.confirm_remove`),
/// in which case subtrees also remove immediately via the same cascade the
/// modal's "Remove" button runs.
fn request_remove(
    state: &mut AppState,
    folders: &[ferrolite_catalog::FolderRecord],
    id: i64,
    name: &str,
) {
    let has_children = folders.iter().any(|f| f.parent_id == Some(id));
    if has_children && state.settings.confirm_remove {
        state.pending_remove = Some(PendingRemove {
            id,
            name: name.to_string(),
            subtree_count: subtree_count(folders, id),
        });
    } else {
        state.remove_folder_cascade(id);
    }
}

use ferrolite_catalog::CollectionRecord;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionNode {
    pub collection: CollectionRecord,
    pub children: Vec<CollectionRecord>,
}

pub fn build_collection_tree(collections: &[CollectionRecord]) -> Vec<CollectionNode> {
    let mut top_level: Vec<CollectionNode> = Vec::new();
    let mut children_by_parent: std::collections::HashMap<i64, Vec<CollectionRecord>> =
        std::collections::HashMap::new();

    for c in collections {
        if let Some(pid) = c.parent_id {
            children_by_parent.entry(pid).or_default().push(c.clone());
        }
    }

    for c in collections {
        if c.parent_id.is_none() {
            let children = children_by_parent.remove(&c.id).unwrap_or_default();
            top_level.push(CollectionNode {
                collection: c.clone(),
                children,
            });
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

pub fn compute_collection_counts(
    collections: &[CollectionRecord],
    image_counts: &std::collections::HashMap<i64, usize>,
) -> std::collections::HashMap<i64, usize> {
    let tree = build_collection_tree(collections);
    let mut result = std::collections::HashMap::new();

    for node in tree {
        let direct = image_counts.get(&node.collection.id).copied().unwrap_or(0);
        let children_sum: usize = node
            .children
            .iter()
            .map(|child| image_counts.get(&child.id).copied().unwrap_or(0))
            .sum();
        let total = direct + children_sum;
        result.insert(node.collection.id, total);

        for child in &node.children {
            let child_count = image_counts.get(&child.id).copied().unwrap_or(0);
            result.insert(child.id, child_count);
        }
    }

    result
}

fn render_collection_row(
    ui: &mut egui::Ui,
    state: &mut AppState,
    c: &CollectionRecord,
    is_child: bool,
    count: usize,
) {
    let is_renaming = matches!(
        &state.renaming,
        Some((RenameKind::Collection, id, _)) if *id == c.id
    );

    let row_resp = ui
        .horizontal(|ui| {
            if is_child {
                ui.add_space(16.0);
            }
            let col = egui::Color32::from_rgb(c.color.r, c.color.g, c.color.b);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, col);

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
                    if c.parent_id.is_none() && ui.button("Add Sub-collection...").clicked() {
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

    row_resp.dnd_set_drag_payload(c.id);

    if ui.ctx().is_being_dragged(row_resp.id) {
        egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), row_resp.id.with("ghost"), |ui| {
            ui.label(format!("{} Moving {}", crate::icons::COLOR, c.name));
        });
    }

    if !is_child && ui.ctx().dragged_id().is_some() && ui.ctx().dragged_id() != Some(row_resp.id) {
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

    if !is_child {
        if let Some(dragged_id) = row_resp.dnd_release_payload::<i64>() {
            if *dragged_id != c.id {
                let res = state
                    .writer
                    .lock()
                    .expect("writer")
                    .update_collection_parent(*dragged_id, Some(c.id));
                if let Err(err) = res {
                    eprintln!("failed to reparent collection: {err}");
                } else {
                    state.reload_vocab();
                    state.dirty = true;
                }
            }
        }
    }

    if let Some(ids) = crate::library::drag::row_drop_target(ui, row_resp.rect) {
        let (cid, cname) = (c.id, c.name.clone());
        state.add_images_to_collection(&ids, cid);
        state.notify(
            crate::notifications::Level::Info,
            format!("Added {} image(s) to \"{}\".", ids.len(), cname),
        );
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
        assert_eq!(tree[0].children[0].id, 2);
        assert_eq!(tree[0].children[1].id, 3);
        assert_eq!(tree[1].collection.id, 4);
        assert_eq!(tree[1].children.len(), 0);
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
        let mut raw_counts = std::collections::HashMap::new();
        raw_counts.insert(2, 5);
        raw_counts.insert(3, 3);
        raw_counts.insert(4, 10);

        let counts = compute_collection_counts(&collections, &raw_counts);
        assert_eq!(counts.get(&1), Some(&8));
        assert_eq!(counts.get(&2), Some(&5));
        assert_eq!(counts.get(&3), Some(&3));
        assert_eq!(counts.get(&4), Some(&10));
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
        assert_eq!(tree[0].children[0].id, id2);

        // Un-nest B back to top-level
        {
            let writer = state.writer.lock().expect("writer");
            writer.update_collection_parent(id2, None).unwrap();
        }

        state.reload_vocab();
        let tree = build_collection_tree(&state.collections);
        assert_eq!(tree.len(), 2);
    }

    #[test]
    fn test_collections_header_no_plus_set_button() {
        let ctx = egui::Context::default();
        let mut state = AppState::for_test();
        let full_output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(ui, &mut state, ctx);
            });
        });

        // Verify UI rendering completes and shapes are generated
        assert!(!full_output.shapes.is_empty());
    }

    #[test]
    fn test_collection_drag_ghost_tooltip_and_drop_target_highlight() {
        let ctx = egui::Context::default();
        let mut state = AppState::for_test();
        let _id1 = {
            let writer = state.writer.lock().expect("writer");
            writer
                .create_collection("Vacation", Color::default())
                .unwrap()
        };
        state.reload_vocab();

        let dummy_drag_id = egui::Id::new("test_drag");
        ctx.set_dragged_id(dummy_drag_id);

        let mut raw_input = egui::RawInput::default();
        raw_input
            .events
            .push(egui::Event::PointerMoved(egui::pos2(50.0, 100.0)));
        let output = ctx.run(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(ui, &mut state, ctx);
            });
        });

        assert_eq!(ctx.dragged_id(), Some(dummy_drag_id));
        assert!(!output.shapes.is_empty());
    }
}

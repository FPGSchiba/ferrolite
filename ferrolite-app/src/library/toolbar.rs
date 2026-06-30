//! Library top toolbar: search, sort, rating/flag/tag filters, and the
//! thumbnail-size slider pinned to the right.  All widgets drive `state.filter`
//! and `state.include_subfolders` directly; the caller sets `state.dirty` when
//! the returned `changed` flag is true (so the read pool re-queries off-thread).
//!
//! Icon rendering follows the panel.rs "draw shapes, no font glyphs" pattern:
//! IBM Plex Sans lacks symbol glyphs (★ ⚑ ▾ etc.) so all icons are painted via
//! `egui::Painter` using the helpers in `library::icons`.

use crate::library::icons;
use crate::state::AppState;
use crate::theme;
use crate::widgets::EguiSlider;
use ferrolite_catalog::{SortKey, TagMode};
use ferrolite_image::Flag;

/// Width of the thumbnail-size slider's box on the right.
const SIZE_SLIDER_W: f32 = 208.0;

/// Star circumradius (px) and gap between stars.
const STAR_R: f32 = 5.5;
const STAR_GAP: f32 = 2.0;

/// Flag icon height (px).
const FLAG_H: f32 = 12.0;

/// Caret half-width (px).
const CARET_HW: f32 = 4.5;

/// Returns `true` if any filter/sort/source field changed this frame.
pub fn show(ui: &mut egui::Ui, thumb_size: &mut f32, state: &mut AppState) -> bool {
    let mut changed = false;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;

        // Search (debounced upstream by the dirty flag; query runs off-thread).
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.filter.search)
                .hint_text("Search filename or tag…")
                .desired_width(206.0),
        );
        if resp.changed() {
            changed = true;
        }

        // Sort key + direction.
        egui::ComboBox::from_id_salt("sort")
            .selected_text(sort_label(state.filter.sort_key))
            .show_ui(ui, |ui| {
                for (k, lbl) in [
                    (SortKey::CaptureTime, "Capture Time"),
                    (SortKey::Filename, "Filename"),
                    (SortKey::Rating, "Rating"),
                    (SortKey::AddedAt, "Date Added"),
                ] {
                    if ui
                        .selectable_value(&mut state.filter.sort_key, k, lbl)
                        .clicked()
                    {
                        changed = true;
                    }
                }
            });

        // Sort-direction button: caret drawn as a shape — no ▼/▲ font glyph.
        {
            let size = egui::vec2(16.0, 16.0);
            let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
            let color = if resp.hovered() {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_DIM
            };
            icons::caret(
                &ui.painter().with_clip_rect(rect),
                rect.center(),
                CARET_HW,
                color,
                state.filter.sort_desc, // down when descending
            );
            if resp.clicked() {
                state.filter.sort_desc = !state.filter.sort_desc;
                changed = true;
            }
        }

        // Rating threshold: draw 5 star shapes; first `min_rating` filled.
        // Clicking star N sets min_rating=N, or clears if already active.
        {
            let star_cell = STAR_R * 2.0 + STAR_GAP;
            let total_w = icons::advance_width(STAR_R, STAR_GAP, 5);
            let size = egui::vec2(total_w, STAR_R * 2.0 + 4.0);
            let (rect, overall) = ui.allocate_exact_size(size, egui::Sense::hover());

            // Check individual star clicks via pointer position.
            let pointer_pos = ui.input(|i| i.pointer.interact_pos());
            for n in 1..=5u8 {
                let cx = rect.left() + STAR_R + (n as f32 - 1.0) * star_cell;
                let star_rect = egui::Rect::from_center_size(
                    egui::Pos2::new(cx, rect.center().y),
                    egui::vec2(star_cell, size.y),
                );
                let hovered = pointer_pos.map(|p| star_rect.contains(p)).unwrap_or(false);
                let clicked = hovered && overall.ctx.input(|i| i.pointer.primary_clicked());
                if clicked {
                    state.filter.min_rating = if state.filter.min_rating == n { 0 } else { n };
                    changed = true;
                }

                let filled = state.filter.min_rating >= n;
                let color = if filled {
                    theme::ACCENT
                } else if hovered {
                    theme::TEXT_DIM
                } else {
                    theme::TEXT_FAINT
                };
                icons::star(
                    ui.painter(),
                    egui::Pos2::new(cx, rect.center().y),
                    STAR_R,
                    filled,
                    color,
                );
            }
        }

        // Flag filter toggles: draw pennant shapes (no ⚑/⚐ font glyphs).
        for (f, is_pick) in [(Flag::Pick, true), (Flag::Reject, false)] {
            let on = state.filter.flags.contains(&f);
            let base_color = if is_pick {
                theme::SEMANTIC_GREEN
            } else {
                theme::SEMANTIC_RED
            };

            let size = egui::vec2(14.0, FLAG_H + 4.0);
            let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());

            // Subtle background rect when active (toggle affordance).
            if on {
                ui.painter()
                    .rect_filled(rect.expand(1.0), 2.0, base_color.gamma_multiply(0.18));
            } else if resp.hovered() {
                ui.painter().rect_filled(
                    rect.expand(1.0),
                    2.0,
                    theme::TEXT_FAINT.gamma_multiply(0.12),
                );
            }

            let color = if on || resp.hovered() {
                base_color
            } else {
                theme::TEXT_FAINT
            };
            // Place pole bottom near the bottom of the rect, centered.
            let base = egui::Pos2::new(rect.center().x - 2.0, rect.bottom() - 2.0);
            icons::flag(ui.painter(), base, FLAG_H, on, color);

            if resp.clicked() {
                toggle_flag(&mut state.filter.flags, f);
                changed = true;
            }
        }

        // Tag filter dropdown (multi-select over the global vocabulary) + Any/All.
        egui::ComboBox::from_id_salt("tagfilter")
            .selected_text(format!("Tags ({})", state.filter.tag_ids.len()))
            .show_ui(ui, |ui| {
                let mode_all = matches!(state.filter.tag_mode, TagMode::All);
                if ui.selectable_label(!mode_all, "Any").clicked() {
                    state.filter.tag_mode = TagMode::Any;
                    changed = true;
                }
                if ui.selectable_label(mode_all, "All").clicked() {
                    state.filter.tag_mode = TagMode::All;
                    changed = true;
                }
                ui.separator();
                for t in &state.tags {
                    let mut on = state.filter.tag_ids.contains(&t.id);
                    if ui.checkbox(&mut on, &t.name).changed() {
                        toggle_tag(&mut state.filter.tag_ids, t.id);
                        changed = true;
                    }
                }
            });

        if ui
            .checkbox(&mut state.include_subfolders, "Subfolders")
            .changed()
        {
            changed = true;
        }

        // Metadata range popover: camera model (inline list), ISO range, date range.
        //
        // The camera selector was previously a nested ComboBox, which spawned its own
        // area-popup.  Clicking that popup registered as a click *outside* the parent
        // popup, immediately closing it (bug #9).  Fix: replace the ComboBox with inline
        // `selectable_label` rows inside a ScrollArea — no nested popup, so the parent
        // stays open.  `CloseOnClickOutside` still closes when the user clicks outside.
        let popup_id = ui.make_persistent_id("meta_popover");
        let btn_resp = show_metadata_button(ui, popup_id);
        if btn_resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &btn_resp,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(240.0);

                // Camera model — inline selectable list (no nested ComboBox popup).
                ui.label("Camera");
                egui::ScrollArea::vertical()
                    .id_salt("meta_camera_scroll")
                    .max_height(160.0)
                    .show(ui, |ui| {
                        if ui
                            .selectable_label(state.filter.camera.is_none(), "Any")
                            .clicked()
                        {
                            state.filter.camera = None;
                            changed = true;
                        }
                        for c in &state.camera_options {
                            let sel = state.filter.camera.as_deref() == Some(c.as_str());
                            if ui.selectable_label(sel, c.as_str()).clicked() {
                                state.filter.camera = Some(c.clone());
                                changed = true;
                            }
                        }
                    });

                ui.separator();

                // ISO range (cached).
                if let Some((lo, hi)) = state.iso_range {
                    let (mut a, mut b) = state.filter.iso.unwrap_or((lo, hi));
                    let mut af = a as f32;
                    let mut bf = b as f32;
                    let r1 =
                        ui.add(egui::Slider::new(&mut af, lo as f32..=hi as f32).text("ISO min"));
                    let r2 =
                        ui.add(egui::Slider::new(&mut bf, lo as f32..=hi as f32).text("ISO max"));
                    if r1.changed() || r2.changed() {
                        a = af as u32;
                        b = bf as u32;
                        state.filter.iso = Some((a.min(b), a.max(b)));
                        changed = true;
                    }
                    if ui.button("Clear ISO").clicked() {
                        state.filter.iso = None;
                        changed = true;
                    }
                }

                // Date range (cached; ISO-8601 text inputs for lexical compare).
                if let Some((lo, hi)) = state.date_range.clone() {
                    let (mut from, mut to) = state
                        .filter
                        .date
                        .clone()
                        .unwrap_or((lo.clone(), hi.clone()));
                    let r1 = ui.add(egui::TextEdit::singleline(&mut from).hint_text("from"));
                    let r2 = ui.add(egui::TextEdit::singleline(&mut to).hint_text("to"));
                    if r1.changed() || r2.changed() {
                        state.filter.date = Some((from, to));
                        changed = true;
                    }
                    if ui.button("Clear dates").clicked() {
                        state.filter.date = None;
                        changed = true;
                    }
                }
            },
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(SIZE_SLIDER_W, ui.available_height()),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add(EguiSlider {
                        label: "Size",
                        value: thumb_size,
                        min: 0.0,
                        max: 100.0,
                        default: 46.0,
                        step: 1.0,
                        decimals: 0,
                        unit: "",
                        bipolar: false,
                        signed: false,
                    });
                },
            );
        });
    });
    changed
}

/// Render the "Metadata" button with a small painted down-caret to the right.
/// Returns the `Response` for the text button (used to anchor + toggle the popup).
fn show_metadata_button(ui: &mut egui::Ui, _popup_id: egui::Id) -> egui::Response {
    // Lay out text button + caret in a tight inline group.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        let btn = ui.button("Metadata");
        // Small caret to the right of the button text.
        let caret_size = egui::vec2(12.0, 12.0);
        let (rect, _) = ui.allocate_exact_size(caret_size, egui::Sense::hover());
        icons::caret(
            ui.painter(),
            rect.center(),
            CARET_HW - 1.0,
            theme::TEXT_DIM,
            true,
        );
        btn
    })
    .inner
}

fn sort_label(k: SortKey) -> &'static str {
    match k {
        SortKey::CaptureTime => "Capture Time",
        SortKey::Filename => "Filename",
        SortKey::Rating => "Rating",
        SortKey::AddedAt => "Date Added",
    }
}

fn toggle_flag(flags: &mut Vec<Flag>, f: Flag) {
    if let Some(p) = flags.iter().position(|x| *x == f) {
        flags.remove(p);
    } else {
        flags.push(f);
    }
}

fn toggle_tag(ids: &mut Vec<ferrolite_image::TagId>, id: ferrolite_image::TagId) {
    if let Some(p) = ids.iter().position(|x| *x == id) {
        ids.remove(p);
    } else {
        ids.push(id);
    }
}

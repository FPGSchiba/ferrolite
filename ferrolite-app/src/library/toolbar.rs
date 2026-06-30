//! Library top toolbar: search, sort, rating/flag/tag filters, and the
//! thumbnail-size slider pinned to the right.  All widgets drive `state.filter`
//! and `state.include_subfolders` directly; the caller sets `state.dirty` when
//! the returned `changed` flag is true (so the read pool re-queries off-thread).
//!
//! Icon rendering follows the panel.rs "draw shapes, no font glyphs" pattern:
//! IBM Plex Sans lacks symbol glyphs (★ ⚑ ▾ etc.) so all icons are painted via
//! `egui::Painter` using the helpers in `library::icons`.

use crate::library::filter_widgets as fw;
use crate::library::icons;
use crate::state::AppState;
use crate::theme;
use crate::widgets::EguiSlider;

/// Width of the thumbnail-size slider's box on the right.
const SIZE_SLIDER_W: f32 = 208.0;

/// Caret half-width (px) used in the Metadata button.
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

        // Sort key + direction (combo + caret toggle).
        if fw::sort_controls(ui, &mut state.filter.sort_key, &mut state.filter.sort_desc) {
            changed = true;
        }

        // Rating threshold: 5 clickable stars; clicking active star clears to 0.
        if fw::rating_threshold(ui, &mut state.filter.min_rating) {
            changed = true;
        }

        // Flag filter toggles (Pick green, Reject red).
        if fw::flag_filters(ui, &mut state.filter.flags) {
            changed = true;
        }

        // Tag filter dropdown (multi-select over the global vocabulary) + Any/All.
        if fw::tag_filter_dropdown(
            ui,
            &mut state.filter.tag_ids,
            &mut state.filter.tag_mode,
            &state.tags,
        ) {
            changed = true;
        }

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

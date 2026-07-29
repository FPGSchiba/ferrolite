//! Library top toolbar: search, sort, rating/flag/tag filters, Metadata Filters popup, and the
//! thumbnail-size slider pinned to the right. All widgets drive `state.filter`
//! and `state.include_subfolders` directly; the caller sets `state.dirty` when
//! the returned `changed` flag is true (so the read pool re-queries off-thread).

use crate::library::filter::FileTypeChip;
use crate::library::filter_widgets as fw;
use crate::library::icons;
use crate::state::AppState;
use crate::theme;
use crate::widgets::{EguiSlider, SegmentedControl};
use egui::{pos2, Color32, FontId, Rounding, Stroke};

/// Toolbar layout constants (Spec 3.3).
#[allow(dead_code)]
pub const TOOLBAR_HEIGHT: f32 = 38.0_f32;
#[allow(dead_code)]
pub const TOOLBAR_BG: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);
#[allow(dead_code)]
pub const TOOLBAR_BORDER: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);
#[allow(dead_code)]
pub const METADATA_POPUP_BG: Color32 = Color32::from_rgb(0x1d, 0x1d, 0x1d);
#[allow(dead_code)]
pub const METADATA_POPUP_BORDER: Color32 = Color32::from_rgb(0x35, 0x35, 0x35);

/// Width of the thumbnail-size slider's box on the right.
const SIZE_SLIDER_W: f32 = 208.0_f32;

/// Caret half-width (px) used in the Metadata button.
const CARET_HW: f32 = 4.5_f32;

/// Returns `true` if any filter/sort/source field changed this frame.
pub fn show(ui: &mut egui::Ui, thumb_size: &mut f32, state: &mut AppState) -> bool {
    let mut changed = false;

    // Toolbar background + bottom border
    let bar = ui.max_rect();
    ui.painter().rect_filled(bar, Rounding::ZERO, TOOLBAR_BG);
    ui.painter().line_segment(
        [
            pos2(bar.left(), bar.bottom() - 0.5_f32),
            pos2(bar.right(), bar.bottom() - 0.5_f32),
        ],
        Stroke::new(1.0_f32, TOOLBAR_BORDER),
    );

    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0_f32;

        // Search field (210px desired width).
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.filter.search)
                .hint_text("Search filename or tag…")
                .desired_width(210.0_f32),
        );
        if resp.changed() {
            changed = true;
        }

        // Sort key + direction (combo + caret toggle).
        if fw::sort_controls(ui, &mut state.filter.sort_key, &mut state.filter.sort_desc) {
            changed = true;
        }

        // Rating threshold: operator toggle (>= / = / <=) + 5 clickable stars.
        if fw::rating_threshold(
            ui,
            &mut state.filter.min_rating,
            &mut state.filter.rating_cmp,
        ) {
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

        // Metadata Filters popup panel (300px wide, #1d1d1d bg, 1px #353535 border).
        //
        // NOTE: this is a hand-rolled `egui::Area`-based popup, NOT
        // `Memory::toggle_popup` + `popup_below_widget`. egui's `Memory` tracks the
        // open popup in a single global slot (`Memory.popup: Option<Id>`), and the
        // three `egui::ComboBox`es inside this popup use that exact same slot for
        // their own dropdown. Clicking a combo overwrote the slot with the combo's
        // id, so next frame `popup_below_widget`'s `is_popup_open` guard failed and
        // the whole Metadata popup vanished. Tracking our own open/closed bool in
        // temp data sidesteps the shared slot entirely, so the combos are free to
        // use it for themselves without disturbing us.
        let popup_id = ui.make_persistent_id("metadata_filters_popup");
        let mut popup_open = ui.data(|d| d.get_temp::<bool>(popup_id)).unwrap_or(false);

        // `show_metadata_button` only reserves the caret's rect and does not paint
        // it yet: the caret is painted once, at the very bottom of this block,
        // from `popup_open`'s FINAL value for the frame (after the toggle below
        // and after the close-decision that may also flip it via Escape/outside
        // click). Painting it here — before those — would show last frame's
        // direction for one frame on the exact click that opens/closes the popup.
        let (btn_resp, caret_rect) = show_metadata_button(ui);
        if btn_resp.clicked() {
            popup_open = !popup_open;
        }

        let mut combo_selection_this_frame = false;

        if popup_open {
            let mut close_popup = false;

            let area_resp = egui::Area::new(popup_id.with("area"))
                .order(egui::Order::Foreground)
                .fixed_pos(btn_resp.rect.left_bottom())
                .default_width(300.0_f32)
                .show(ui.ctx(), |ui| {
                    ui.set_min_width(300.0_f32);
                    ui.set_max_width(300.0_f32);

                    egui::Frame::popup(ui.style())
                        .fill(METADATA_POPUP_BG)
                        .stroke(Stroke::new(1.0_f32, METADATA_POPUP_BORDER))
                        .inner_margin(egui::Margin::same(12.0_f32))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.0_f32;

                            // Header "METADATA FILTERS" + "Reset" link
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("METADATA FILTERS")
                                        .font(FontId::proportional(11.0_f32))
                                        .strong()
                                        .color(theme::TEXT_PRIMARY),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.link("Reset").clicked() {
                                            state.filter.reset_metadata_filters();
                                            changed = true;
                                        }
                                    },
                                );
                            });

                            ui.separator();

                            // Combos: Camera, Lens, Rating
                            ui.horizontal(|ui| {
                                ui.label("Camera");
                                let selected_cam =
                                    state.filter.camera.as_deref().unwrap_or("All Cameras");
                                egui::ComboBox::from_id_salt("camera_combo")
                                    .selected_text(selected_cam)
                                    .show_ui(ui, |ui| {
                                        if ui
                                            .selectable_label(
                                                state.filter.camera.is_none(),
                                                "All Cameras",
                                            )
                                            .clicked()
                                        {
                                            state.filter.camera = None;
                                            changed = true;
                                            combo_selection_this_frame = true;
                                        }
                                        for c in &state.camera_options {
                                            let sel =
                                                state.filter.camera.as_deref() == Some(c.as_str());
                                            if ui.selectable_label(sel, c.as_str()).clicked() {
                                                state.filter.camera = Some(c.clone());
                                                changed = true;
                                                combo_selection_this_frame = true;
                                            }
                                        }
                                    });
                            });

                            ui.horizontal(|ui| {
                                ui.label("Lens");
                                let selected_lens =
                                    state.filter.lens.as_deref().unwrap_or("All Lenses");
                                egui::ComboBox::from_id_salt("lens_combo")
                                    .selected_text(selected_lens)
                                    .show_ui(ui, |ui| {
                                        if ui
                                            .selectable_label(
                                                state.filter.lens.is_none(),
                                                "All Lenses",
                                            )
                                            .clicked()
                                        {
                                            state.filter.lens = None;
                                            changed = true;
                                            combo_selection_this_frame = true;
                                        }
                                        for l in &state.lens_options {
                                            let sel =
                                                state.filter.lens.as_deref() == Some(l.as_str());
                                            if ui.selectable_label(sel, l.as_str()).clicked() {
                                                state.filter.lens = Some(l.clone());
                                                changed = true;
                                                combo_selection_this_frame = true;
                                            }
                                        }
                                    });
                            });

                            ui.horizontal(|ui| {
                                ui.label("Rating");
                                let rating_text = match state.filter.min_rating {
                                    0 => "Any Rating".to_string(),
                                    r => format!("{r}+ Stars"),
                                };
                                egui::ComboBox::from_id_salt("rating_combo")
                                    .selected_text(rating_text)
                                    .show_ui(ui, |ui| {
                                        for r in 0..=5 {
                                            let label = if r == 0 {
                                                "Any Rating".to_string()
                                            } else {
                                                format!("{r}+ Stars")
                                            };
                                            let sel = state.filter.min_rating == r;
                                            if ui.selectable_label(sel, label).clicked() {
                                                state.filter.min_rating = r;
                                                changed = true;
                                                combo_selection_this_frame = true;
                                            }
                                        }
                                    });
                            });

                            ui.separator();

                            // "FILE TYPE" segmented chips using SegmentedControl
                            ui.label(
                                egui::RichText::new("FILE TYPE")
                                    .font(FontId::proportional(10.5_f32))
                                    .color(theme::TEXT_DIM),
                            );
                            let chip_options = [
                                (FileTypeChip::Raw, "RAW"),
                                (FileTypeChip::Jpeg, "JPEG"),
                                (FileTypeChip::Heic, "HEIC"),
                                (FileTypeChip::Tiff, "TIFF"),
                            ];
                            let mut current_chip = state.filter.file_type.unwrap_or_default();
                            if SegmentedControl::new(&mut current_chip, &chip_options)
                                .ui(ui, "file_type_segmented")
                                .changed()
                            {
                                state.filter.file_type = Some(current_chip);
                                changed = true;
                            }

                            ui.separator();

                            // "EXPOSURE RANGE" sliders: ISO, Aperture, Focal
                            ui.label(
                                egui::RichText::new("EXPOSURE RANGE")
                                    .font(FontId::proportional(10.5_f32))
                                    .color(theme::TEXT_DIM),
                            );

                            // ISO Slider
                            let mut iso_val = state
                                .filter
                                .iso
                                .map(|(lo, _)| lo as f32)
                                .unwrap_or(100.0_f32);
                            if ui
                                .add(EguiSlider {
                                    label: "ISO",
                                    value: &mut iso_val,
                                    min: 100.0_f32,
                                    max: 12800.0_f32,
                                    default: 100.0_f32,
                                    step: 100.0_f32,
                                    decimals: 0,
                                    unit: "",
                                    bipolar: false,
                                    signed: false,
                                    custom_label_w: Some(60.0_f32),
                                })
                                .changed()
                            {
                                let v = iso_val as u32;
                                state.filter.iso = Some((v, 12800));
                                changed = true;
                            }

                            // Aperture Slider
                            let mut ap_val =
                                state.filter.aperture.map(|(lo, _)| lo).unwrap_or(1.4_f32);
                            if ui
                                .add(EguiSlider {
                                    label: "Aperture",
                                    value: &mut ap_val,
                                    min: 1.0_f32,
                                    max: 22.0_f32,
                                    default: 1.4_f32,
                                    step: 0.1_f32,
                                    decimals: 1,
                                    unit: "f/",
                                    bipolar: false,
                                    signed: false,
                                    custom_label_w: Some(60.0_f32),
                                })
                                .changed()
                            {
                                state.filter.aperture = Some((ap_val, 22.0_f32));
                                changed = true;
                            }

                            // Focal Slider
                            let mut focal_val =
                                state.filter.focal.map(|(lo, _)| lo).unwrap_or(24.0_f32);
                            if ui
                                .add(EguiSlider {
                                    label: "Focal",
                                    value: &mut focal_val,
                                    min: 14.0_f32,
                                    max: 600.0_f32,
                                    default: 24.0_f32,
                                    step: 1.0_f32,
                                    decimals: 0,
                                    unit: "mm",
                                    bipolar: false,
                                    signed: false,
                                    custom_label_w: Some(60.0_f32),
                                })
                                .changed()
                            {
                                state.filter.focal = Some((focal_val, 600.0_f32));
                                changed = true;
                            }

                            ui.separator();

                            // Footer: "Apply Filters" (accent-filled) and "Close" buttons
                            ui.horizontal(|ui| {
                                let apply_btn = egui::Button::new(
                                    egui::RichText::new("Apply Filters").color(theme::ACCENT_TEXT),
                                )
                                .fill(theme::ACCENT_FILL);
                                if ui.add(apply_btn).clicked() {
                                    changed = true;
                                    close_popup = true;
                                }
                                if ui.button("Close").clicked() {
                                    close_popup = true;
                                }
                            });
                        });
                });

            // Close on outside click (button and popup both excluded) unless a
            // ComboBox dropdown inside is open. A combo's dropdown area can extend
            // past the popup's own Area rect, so a raw "click outside the popup"
            // check would treat picking a combo option as an outside click and
            // slam the whole Metadata popup shut.
            //
            // `any_inner_popup_open` is read AFTER the Area above (and the combos
            // inside it) already ran, so it is NOT a reliable "a combo dropdown
            // was open" signal for the exact frame a selection is made: egui's
            // `ComboBox` uses `PopupCloseBehavior::CloseOnClick` internally, which
            // closes its own dropdown (clearing the shared `Memory` popup slot)
            // synchronously the moment any of its options is clicked — before we
            // get to read `any_popup_open()` here. So on that frame this would
            // read `false` even though the click was really "pick a combo option",
            // and `clicked_outside` would read `true` (the option can render
            // outside the popup's own Area rect), closing the whole popup right
            // after the pick. `combo_selection_this_frame` is set directly at each
            // `selectable_label` click site above and short-circuits the close
            // for exactly that frame, independent of the (here, unreliable)
            // `any_inner_popup_open` snapshot.
            let clicked_outside =
                btn_resp.clicked_elsewhere() && area_resp.response.clicked_elsewhere();
            let any_inner_popup_open = ui.memory(|m| m.any_popup_open());
            let escape_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));

            if close_popup
                || should_close_metadata_popup(
                    clicked_outside,
                    any_inner_popup_open,
                    escape_pressed,
                    combo_selection_this_frame,
                )
            {
                popup_open = false;
            }
        }

        // Paint the caret from `popup_open`'s value at the END of the frame's
        // decision-making (after the toggle click above AND the close-decision
        // just above, which can also flip it via Escape/outside-click) so it
        // never shows a stale direction for a frame — see the comment on
        // `show_metadata_button`.
        icons::caret(
            ui.painter(),
            caret_rect.center(),
            CARET_HW - 1.0_f32,
            theme::TEXT_DIM,
            !popup_open,
        );

        ui.data_mut(|d| d.insert_temp(popup_id, popup_open));

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(SIZE_SLIDER_W, ui.available_height()),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add(EguiSlider {
                        label: "Size",
                        value: thumb_size,
                        min: 0.0_f32,
                        max: 100.0_f32,
                        default: 46.0_f32,
                        step: 1.0_f32,
                        decimals: 0,
                        unit: "",
                        bipolar: false,
                        signed: false,
                        custom_label_w: None,
                    });
                },
            );
        });
    });
    changed
}

/// Render the "Metadata" button and reserve the rect for its caret glyph.
/// Returns the button `Response` (used to anchor + toggle the popup) and the
/// caret's paint rect — the caller paints the caret itself, once, from
/// `popup_open`'s value at the end of this frame's decision-making (see the
/// call site in `show`). Painting it here, before that toggle/close logic
/// runs, would show the previous frame's direction for one frame on the exact
/// click that opens or closes the popup.
fn show_metadata_button(ui: &mut egui::Ui) -> (egui::Response, egui::Rect) {
    // Lay out text button + caret rect in a tight inline group.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0_f32;
        let btn = ui.button("Metadata");
        // Small caret to the right of the button text (painted by the caller).
        let caret_size = egui::vec2(12.0_f32, 12.0_f32);
        let (rect, _) = ui.allocate_exact_size(caret_size, egui::Sense::hover());
        (btn, rect)
    })
    .inner
}

/// Pure close-decision for the Metadata popup, factored out so it's testable
/// without an `egui::Context`.
///
/// - `any_inner_popup_open` should reflect `ui.memory(|m| m.any_popup_open())`
///   sampled after this frame's ComboBoxes ran. It's kept as an input (rather
///   than dropped) because it still guards the case where a dropdown is open
///   but nothing was clicked this frame. It is NOT sufficient on its own for a
///   combo *selection* click: egui's `ComboBox` uses
///   `PopupCloseBehavior::CloseOnClick`, which closes the combo's own dropdown
///   (clearing the shared `Memory` popup slot) synchronously as part of
///   rendering it, before this flag is sampled — so on the very frame a
///   selection is made, this reads `false` even though the click was a combo
///   pick, not a click outside the popup.
/// - `combo_selection_this_frame` covers exactly that frame: it's set directly
///   at each `selectable_label` click site inside the popup's ComboBoxes and
///   unconditionally keeps the popup open, since a selection can render past
///   the popup's own Area rect and would otherwise look like an outside click.
fn should_close_metadata_popup(
    clicked_outside: bool,
    any_inner_popup_open: bool,
    escape_pressed: bool,
    combo_selection_this_frame: bool,
) -> bool {
    if escape_pressed {
        return true;
    }
    if combo_selection_this_frame {
        return false;
    }
    clicked_outside && !any_inner_popup_open
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Context;

    #[test]
    fn toolbar_layout_constants() {
        assert_eq!(TOOLBAR_HEIGHT, 38.0_f32);
        assert_eq!(TOOLBAR_BG, Color32::from_rgb(0x1a, 0x1a, 0x1a));
        assert_eq!(TOOLBAR_BORDER, Color32::from_rgb(0x26, 0x26, 0x26));
        assert_eq!(METADATA_POPUP_BG, Color32::from_rgb(0x1d, 0x1d, 0x1d));
        assert_eq!(METADATA_POPUP_BORDER, Color32::from_rgb(0x35, 0x35, 0x35));
    }

    #[test]
    fn metadata_popup_open_state_is_a_plain_bool_in_temp_data() {
        // The Metadata popup no longer competes for egui's single global
        // `Memory.popup` slot (that's the bug the ComboBoxes triggered); its
        // open/closed state lives in `Ui::data` under its own persistent id and
        // is untouched by `Memory::toggle_popup`.
        let ctx = Context::default();
        let popup_id = egui::Id::new("metadata_filters_popup");

        assert_eq!(ctx.data(|d| d.get_temp::<bool>(popup_id)), None);
        ctx.data_mut(|d| d.insert_temp(popup_id, true));
        assert_eq!(ctx.data(|d| d.get_temp::<bool>(popup_id)), Some(true));

        // A ComboBox toggling the shared Memory popup slot must not affect it.
        ctx.memory_mut(|m| m.toggle_popup(egui::Id::new("some_combo_box")));
        assert_eq!(ctx.data(|d| d.get_temp::<bool>(popup_id)), Some(true));
    }

    #[test]
    fn should_close_metadata_popup_escape_always_closes() {
        assert!(should_close_metadata_popup(false, false, true, false));
        assert!(should_close_metadata_popup(false, true, true, false));
        assert!(should_close_metadata_popup(true, true, true, false));
        // Escape wins even in the (contrived) case a selection flag is also set.
        assert!(should_close_metadata_popup(false, false, true, true));
    }

    #[test]
    fn should_close_metadata_popup_outside_click_closes_when_no_inner_popup_open() {
        assert!(should_close_metadata_popup(true, false, false, false));
    }

    #[test]
    fn should_close_metadata_popup_stays_open_while_combo_dropdown_is_open() {
        // A dropdown is open but nothing was clicked yet this frame: don't close.
        assert!(!should_close_metadata_popup(true, true, false, false));
    }

    #[test]
    fn should_close_metadata_popup_stays_open_on_combo_selection_even_if_it_reads_as_outside_click()
    {
        // This is the exact interaction that used to close the popup before the
        // fix: picking a ComboBox option renders past the popup's own Area rect,
        // so the click reads as "outside the popup" (`clicked_outside`), AND by
        // the time we sample it, `any_inner_popup_open` has already gone false
        // (the combo closed its own dropdown as part of handling that same
        // click, before we get to check). Only `combo_selection_this_frame`
        // — set directly at the selection site — can save the popup here.
        assert!(!should_close_metadata_popup(true, false, false, true));
    }

    #[test]
    fn should_close_metadata_popup_stays_open_with_no_signal() {
        assert!(!should_close_metadata_popup(false, false, false, false));
        assert!(!should_close_metadata_popup(false, true, false, false));
    }

    #[test]
    fn filter_state_reset_metadata_filters() {
        let mut fs = crate::library::filter::FilterState {
            camera: Some("Sony A7IV".to_string()),
            lens: Some("24-70mm f/2.8".to_string()),
            file_type: Some(FileTypeChip::Raw),
            min_rating: 4,
            iso: Some((100, 3200)),
            aperture: Some((2.8_f32, 11.0_f32)),
            focal: Some((24.0_f32, 70.0_f32)),
            ..Default::default()
        };

        fs.reset_metadata_filters();

        assert_eq!(fs.camera, None);
        assert_eq!(fs.lens, None);
        assert_eq!(fs.file_type, None);
        assert_eq!(fs.min_rating, 0);
        assert_eq!(fs.iso, None);
        assert_eq!(fs.aperture, None);
        assert_eq!(fs.focal, None);
    }
}

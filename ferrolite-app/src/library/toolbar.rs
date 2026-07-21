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

        // Metadata Filters popup panel (300px wide, #1d1d1d bg, 1px #353535 border)
        let popup_id = ui.make_persistent_id("metadata_filters_popup");
        let btn_resp = show_metadata_button(ui, popup_id);
        if btn_resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }

        let mut close_popup = false;

        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &btn_resp,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(300.0_f32);
                ui.set_max_width(300.0_f32);

                egui::Frame::none()
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
                                    }
                                    for c in &state.camera_options {
                                        let sel =
                                            state.filter.camera.as_deref() == Some(c.as_str());
                                        if ui.selectable_label(sel, c.as_str()).clicked() {
                                            state.filter.camera = Some(c.clone());
                                            changed = true;
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
                                        .selectable_label(state.filter.lens.is_none(), "All Lenses")
                                        .clicked()
                                    {
                                        state.filter.lens = None;
                                        changed = true;
                                    }
                                    for l in &state.lens_options {
                                        let sel = state.filter.lens.as_deref() == Some(l.as_str());
                                        if ui.selectable_label(sel, l.as_str()).clicked() {
                                            state.filter.lens = Some(l.clone());
                                            changed = true;
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
                        let mut ap_val = state.filter.aperture.map(|(lo, _)| lo).unwrap_or(1.4_f32);
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
            },
        );

        if close_popup {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }

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

/// Render the "Metadata" button with a small painted down-caret to the right.
/// Returns the `Response` for the text button (used to anchor + toggle the popup).
fn show_metadata_button(ui: &mut egui::Ui, _popup_id: egui::Id) -> egui::Response {
    // Lay out text button + caret in a tight inline group.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0_f32;
        let btn = ui.button("Metadata");
        // Small caret to the right of the button text.
        let caret_size = egui::vec2(12.0_f32, 12.0_f32);
        let (rect, _) = ui.allocate_exact_size(caret_size, egui::Sense::hover());
        icons::caret(
            ui.painter(),
            rect.center(),
            CARET_HW - 1.0_f32,
            theme::TEXT_DIM,
            true,
        );
        btn
    })
    .inner
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
    fn metadata_filters_popup_toggle_state() {
        let ctx = Context::default();
        let popup_id = egui::Id::new("metadata_filters_popup");

        assert!(!ctx.memory(|m| m.is_popup_open(popup_id)));

        ctx.memory_mut(|m| m.toggle_popup(popup_id));
        assert!(ctx.memory(|m| m.is_popup_open(popup_id)));

        ctx.memory_mut(|m| m.toggle_popup(popup_id));
        assert!(!ctx.memory(|m| m.is_popup_open(popup_id)));
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

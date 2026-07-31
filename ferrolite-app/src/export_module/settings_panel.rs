//! Export settings panel (spec §8.4): V2 reverse control-left / label-right layout
//! for configuring format, color space, bit depth, quality, effort, resize,
//! and metadata preservation options.

use crate::state::AppState;
use crate::theme;
use crate::widgets::SegmentedControl;
use ferrolite_color::WorkingSpace;
use ferrolite_export::{BitDepth, Effort, ExportFormat, ResizeSpec};

/// Format human-readable string for WorkingSpace options.
fn ws_label(ws: WorkingSpace) -> &'static str {
    match ws {
        WorkingSpace::Srgb => "sRGB",
        WorkingSpace::AdobeRgb => "AdobeRGB",
        WorkingSpace::DisplayP3 => "DisplayP3",
        WorkingSpace::Rec2020 => "Rec2020",
        WorkingSpace::ProPhoto => "ProPhoto",
    }
}

/// Render the V2 export settings panel into `ui`.
/// Reverse conventional layout: controls sit on the left, labels sit on the right.
pub fn export_settings_panel(ui: &mut egui::Ui, state: &mut AppState) {
    ui.spacing_mut().item_spacing.y = 8.0_f32;

    // Header label: 10px, letter-spaced/monospace, faint text
    ui.label(
        egui::RichText::new("EXPORT SETTINGS")
            .font(egui::FontId::monospace(10.0_f32))
            .color(theme::TEXT_FAINT),
    );
    ui.add_space(4.0_f32);

    let opts = &mut state.export_settings;

    // 1. Format combo (JPEG, PNG, TIFF, WebP, AVIF, JXL)
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("export_format_combo")
            .selected_text(opts.format.label())
            .show_ui(ui, |ui| {
                for &f in ExportFormat::ALL.iter() {
                    ui.selectable_value(&mut opts.format, f, f.label());
                }
            });
        ui.label(egui::RichText::new("Format").color(theme::TEXT_PRIMARY));
    });

    // 2. Output color space combo (sRGB, AdobeRGB, Rec2020, DisplayP3, ProPhoto)
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("export_color_space_combo")
            .selected_text(ws_label(opts.output_space))
            .show_ui(ui, |ui| {
                for &ws in WorkingSpace::ALL.iter() {
                    ui.selectable_value(&mut opts.output_space, ws, ws_label(ws));
                }
            });
        ui.label(egui::RichText::new("Color space").color(theme::TEXT_PRIMARY));
    });

    // 3. Bit depth 2-way SegmentedControl (8-bit / 16-bit)
    let supports_16bit = opts.format.supports_16bit();
    ui.horizontal(|ui| {
        ui.add_enabled_ui(supports_16bit, |ui| {
            SegmentedControl::new(
                &mut opts.bit_depth,
                &[(BitDepth::Eight, "8-bit"), (BitDepth::Sixteen, "16-bit")],
            )
            .ui(ui, "export_bit_depth_chips");
        });
        ui.label(egui::RichText::new("Bit depth").color(theme::TEXT_PRIMARY));
    });
    if !supports_16bit {
        opts.bit_depth = BitDepth::Eight;
    }

    // 4. Quality slider (1..=100) + label
    let supports_quality = opts.format.supports_quality();
    ui.horizontal(|ui| {
        ui.add_enabled_ui(supports_quality, |ui| {
            ui.add(egui::Slider::new(&mut opts.quality, 1..=100));
        });
        ui.label(egui::RichText::new("Quality").color(theme::TEXT_PRIMARY));
    });

    // 5. Effort 3-way SegmentedControl (Fast / Balanced / Best)
    let supports_effort = opts.format.supports_effort();
    ui.horizontal(|ui| {
        ui.add_enabled_ui(supports_effort, |ui| {
            SegmentedControl::new(
                &mut opts.effort,
                &[
                    (Effort::Fast, "Fast"),
                    (Effort::Balanced, "Balanced"),
                    (Effort::Best, "Best"),
                ],
            )
            .ui(ui, "export_effort_chips");
        });
        ui.label(egui::RichText::new("Effort").color(theme::TEXT_PRIMARY));
    });

    // 6. Resize combo (None, Fit Long Edge, Fit Short Edge)
    let mut mode = match opts.resize {
        ResizeSpec::None => 0,
        ResizeSpec::LongEdge(_) => 1,
        ResizeSpec::Exact { .. } => 2,
        ResizeSpec::Percent(_) => 3,
    };
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("export_resize_combo")
            .selected_text(["None", "Fit Long Edge", "Fit Short Edge", "Percent"][mode])
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut mode, 0, "None");
                ui.selectable_value(&mut mode, 1, "Fit Long Edge");
                ui.selectable_value(&mut mode, 2, "Fit Short Edge");
                ui.selectable_value(&mut mode, 3, "Percent");
            });
        ui.label(egui::RichText::new("Resize").color(theme::TEXT_PRIMARY));
    });

    opts.resize = match mode {
        1 => {
            let mut px = if let ResizeSpec::LongEdge(p) = opts.resize {
                p
            } else {
                2048
            };
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut px)
                        .range(1..=100_000)
                        .prefix("px "),
                );
                ui.label(egui::RichText::new("Long edge px").color(theme::TEXT_DIM));
            });
            ResizeSpec::LongEdge(px)
        }
        2 => {
            let (mut w, mut h) = if let ResizeSpec::Exact { w, h } = opts.resize {
                (w, h)
            } else {
                (1920, 1080)
            };
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut w).range(1..=100_000).prefix("W "));
                ui.add(egui::DragValue::new(&mut h).range(1..=100_000).prefix("H "));
                ui.label(egui::RichText::new("Dimensions").color(theme::TEXT_DIM));
            });
            ResizeSpec::Exact { w, h }
        }
        3 => {
            let mut pct = if let ResizeSpec::Percent(p) = opts.resize {
                p * 100.0_f32
            } else {
                50.0_f32
            };
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut pct, 1.0_f32..=100.0_f32).suffix("%"));
                ui.label(egui::RichText::new("Scale").color(theme::TEXT_DIM));
            });
            ResizeSpec::Percent(pct / 100.0_f32)
        }
        _ => ResizeSpec::None,
    };

    // 7. Divider line
    ui.add_space(4.0_f32);
    ui.add(egui::Separator::default());
    ui.add_space(4.0_f32);

    // 8. Metadata checkboxes
    ui.checkbox(&mut opts.copy_exif, "Copy EXIF metadata");
    ui.checkbox(&mut opts.embed_icc, "Embed ICC profile");
    ui.checkbox(&mut opts.strip_metadata, "Strip metadata");
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_export::ExportOptions;

    #[test]
    fn test_export_settings_defaults() {
        let opts = ExportOptions::default();
        assert_eq!(opts.format, ExportFormat::Jpeg);
        assert_eq!(opts.output_space, WorkingSpace::Srgb);
        assert_eq!(opts.bit_depth, BitDepth::Eight);
        assert_eq!(opts.quality, 90);
        assert_eq!(opts.effort, Effort::Balanced);
        assert_eq!(opts.resize, ResizeSpec::None);
        assert!(opts.copy_exif);
        assert!(opts.embed_icc);
        assert!(!opts.strip_metadata);
    }

    #[test]
    fn test_format_supports_16bit_enforcement() {
        let opts = ExportOptions {
            format: ExportFormat::Jpeg,
            bit_depth: BitDepth::Sixteen,
            ..Default::default()
        };

        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut state = crate::state::AppState::for_test();
                state.export_settings = opts;
                export_settings_panel(ui, &mut state);
                // JPEG does not support 16-bit so bit_depth should be forced to 8-bit
                assert_eq!(state.export_settings.bit_depth, BitDepth::Eight);
            });
        });
    }

    #[test]
    fn test_control_bindings_in_panel() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut state = crate::state::AppState::for_test();

                state.export_settings.format = ExportFormat::Png;
                state.export_settings.bit_depth = BitDepth::Sixteen;
                state.export_settings.copy_exif = false;
                state.export_settings.strip_metadata = true;

                export_settings_panel(ui, &mut state);

                // PNG supports 16-bit, so bit_depth remains Sixteen
                assert_eq!(state.export_settings.format, ExportFormat::Png);
                assert_eq!(state.export_settings.bit_depth, BitDepth::Sixteen);
                assert!(!state.export_settings.copy_exif);
                assert!(state.export_settings.strip_metadata);
            });
        });
    }
}

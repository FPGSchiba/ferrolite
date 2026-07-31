//! The shared export-options form (spec §8.2). Extracted from the single-file
//! dialog so the batch Export module's settings panel renders identical controls.

use ferrolite_color::WorkingSpace;
use ferrolite_export::{
    BitDepth, Effort, ExportFormat, ExportOptions, OutputMedium, OutputSharpenAmount, ResizeSpec,
};

/// Draw every export option control into `ui`. Callers own the surrounding
/// window/panel and any confirm/cancel affordances.
pub fn settings_form(ui: &mut egui::Ui, o: &mut ExportOptions) {
    egui::ComboBox::from_label("Format")
        .selected_text(o.format.label())
        .show_ui(ui, |ui| {
            for f in ExportFormat::ALL {
                ui.selectable_value(&mut o.format, f, f.label());
            }
        });

    egui::ComboBox::from_label("Output color space")
        .selected_text(format!("{:?}", o.output_space))
        .show_ui(ui, |ui| {
            for ws in WorkingSpace::ALL {
                ui.selectable_value(&mut o.output_space, ws, format!("{ws:?}"));
            }
        });

    ui.horizontal(|ui| {
        ui.label("Bit depth");
        ui.selectable_value(&mut o.bit_depth, BitDepth::Eight, "8-bit");
        ui.add_enabled_ui(o.format.supports_16bit(), |ui| {
            ui.selectable_value(&mut o.bit_depth, BitDepth::Sixteen, "16-bit");
        });
    });
    // 16-bit only for TIFF/PNG; force back to 8-bit otherwise.
    if !o.format.supports_16bit() {
        o.bit_depth = BitDepth::Eight;
    }

    ui.add_enabled_ui(o.format.supports_quality(), |ui| {
        ui.add(egui::Slider::new(&mut o.quality, 1..=100).text("Quality"));
    });

    ui.add_enabled_ui(o.format.supports_effort(), |ui| {
        ui.horizontal(|ui| {
            ui.label("Effort");
            ui.selectable_value(&mut o.effort, Effort::Fast, "Fast");
            ui.selectable_value(&mut o.effort, Effort::Balanced, "Balanced");
            ui.selectable_value(&mut o.effort, Effort::Best, "Best");
        });
    });

    let mut mode = match o.resize {
        ResizeSpec::None => 0,
        ResizeSpec::LongEdge(_) => 1,
        ResizeSpec::Exact { .. } => 2,
        ResizeSpec::Percent(_) => 3,
    };
    egui::ComboBox::from_label("Resize")
        .selected_text(["None", "Long edge", "Exact", "Percent"][mode])
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut mode, 0, "None");
            ui.selectable_value(&mut mode, 1, "Long edge");
            ui.selectable_value(&mut mode, 2, "Exact");
            ui.selectable_value(&mut mode, 3, "Percent");
        });
    o.resize = match mode {
        1 => {
            let mut px = if let ResizeSpec::LongEdge(p) = o.resize {
                p
            } else {
                2048
            };
            ui.add(
                egui::DragValue::new(&mut px)
                    .range(1..=100_000)
                    .prefix("px "),
            );
            ResizeSpec::LongEdge(px)
        }
        2 => {
            let (mut w, mut h) = if let ResizeSpec::Exact { w, h } = o.resize {
                (w, h)
            } else {
                (1920, 1080)
            };
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut w).range(1..=100_000).prefix("W "));
                ui.add(egui::DragValue::new(&mut h).range(1..=100_000).prefix("H "));
            });
            ResizeSpec::Exact { w, h }
        }
        3 => {
            let mut pct = if let ResizeSpec::Percent(p) = o.resize {
                p * 100.0
            } else {
                50.0
            };
            ui.add(egui::Slider::new(&mut pct, 1.0..=100.0).suffix("%"));
            ResizeSpec::Percent(pct / 100.0)
        }
        _ => ResizeSpec::None,
    };

    egui::ComboBox::from_label("Sharpen for")
        .selected_text(match o.sharpen_for {
            OutputMedium::None => "None",
            OutputMedium::Screen => "Screen",
            OutputMedium::Glossy => "Glossy paper",
            OutputMedium::Matte => "Matte paper",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut o.sharpen_for, OutputMedium::None, "None");
            ui.selectable_value(&mut o.sharpen_for, OutputMedium::Screen, "Screen");
            ui.selectable_value(&mut o.sharpen_for, OutputMedium::Glossy, "Glossy paper");
            ui.selectable_value(&mut o.sharpen_for, OutputMedium::Matte, "Matte paper");
        });

    // Greyed with a reason while no medium is chosen — the amount tier only
    // means something once output sharpening is on (same greyed-with-reason
    // convention as the Develop panel's unavailable controls).
    ui.add_enabled_ui(o.sharpen_for != OutputMedium::None, |ui| {
        egui::ComboBox::from_label("Sharpen amount")
            .selected_text(match o.sharpen_amount {
                OutputSharpenAmount::Low => "Low",
                OutputSharpenAmount::Standard => "Standard",
                OutputSharpenAmount::High => "High",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut o.sharpen_amount, OutputSharpenAmount::Low, "Low");
                ui.selectable_value(
                    &mut o.sharpen_amount,
                    OutputSharpenAmount::Standard,
                    "Standard",
                );
                ui.selectable_value(&mut o.sharpen_amount, OutputSharpenAmount::High, "High");
            });
    })
    .response
    .on_hover_text(if o.sharpen_for == OutputMedium::None {
        "Choose an output medium to enable output sharpening"
    } else {
        ""
    });

    ui.separator();
    ui.checkbox(&mut o.copy_exif, "Copy EXIF metadata");
    ui.checkbox(&mut o.embed_icc, "Embed ICC profile");
    ui.checkbox(&mut o.strip_metadata, "Strip metadata");
}

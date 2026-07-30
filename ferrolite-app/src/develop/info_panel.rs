//! Read-only 300px Develop left info panel listing photographic facts (Camera, Lens,
//! Focal, Aperture, Shutter, ISO, Captured, Size, Zoom).

use crate::state::AppState;

/// Color tokens for label and value columns.
const COLOR_LABEL: egui::Color32 = egui::Color32::from_rgb(0x7a, 0x7a, 0x7a);
const COLOR_VALUE: egui::Color32 = egui::Color32::from_rgb(0xd0, 0xd0, 0xd0);
const COLOR_BORDER: egui::Color32 = egui::Color32::from_rgb(0x26, 0x26, 0x26);

/// Format read-only EXIF + viewer zoom facts into (label, value) pairs.
pub fn format_info_rows(
    facts: Option<&crate::develop::info::ImageFacts>,
) -> Vec<(&'static str, String)> {
    let fmt = |s: &str| {
        if s.is_empty() {
            "-".to_string()
        } else {
            s.to_string()
        }
    };
    match facts {
        Some(f) => vec![
            ("Camera", fmt(&f.camera)),
            ("Lens", fmt(&f.lens)),
            ("Focal", fmt(&f.focal)),
            ("Aperture", fmt(&f.aperture)),
            ("Shutter", fmt(&f.shutter)),
            ("ISO", fmt(&f.iso)),
            ("Captured", fmt(&f.capture_time)),
            ("Size", fmt(&f.dimensions)),
            ("Zoom", fmt(&f.zoom)),
        ],
        None => vec![
            ("Camera", "-".to_string()),
            ("Lens", "-".to_string()),
            ("Focal", "-".to_string()),
            ("Aperture", "-".to_string()),
            ("Shutter", "-".to_string()),
            ("ISO", "-".to_string()),
            ("Captured", "-".to_string()),
            ("Size", "-".to_string()),
            ("Zoom", "-".to_string()),
        ],
    }
}

/// Helper function to toggle the show_info_panel state boolean.
#[allow(dead_code)]
pub fn toggle_info_panel(show: &mut bool) {
    *show = !*show;
}

/// Extract current image facts if available from AppState.
fn extract_facts(state: &AppState) -> Option<crate::develop::info::ImageFacts> {
    let v = state.viewer.as_ref()?;
    let meta = v.meta.as_ref()?;
    let dims = v.image_dims?;
    let fit = ferrolite_vt::ViewTransform::fit(dims, v.viewport).zoom;
    Some(crate::develop::info::ImageFacts::build(
        meta,
        v.view.zoom,
        fit,
        dims,
    ))
}

/// Render the read-only left info panel contents.
pub fn show(ui: &mut egui::Ui, state: &AppState) {
    // Fill whatever width the resizable SidePanel allocated this frame. Pinning
    // this to a hardcoded constant fought the panel's drag-resize: egui persists
    // the panel's next-frame width from this Ui's returned rect, so a fixed
    // min==max width here overwrote the user's drag back to the constant every
    // frame (the "snaps back" bug).
    ui.set_width(ui.available_width());

    ui.label(
        egui::RichText::new("INFO")
            .small()
            .color(crate::theme::TEXT_FAINT),
    );
    ui.add_space(8.0);

    let facts = extract_facts(state);
    let rows = format_info_rows(facts.as_ref());

    for (label, val) in rows {
        ui.horizontal(|ui| {
            ui.add_sized(
                [66.0, 18.0],
                egui::Label::new(egui::RichText::new(label).color(COLOR_LABEL)),
            );
            ui.label(egui::RichText::new(val).color(COLOR_VALUE));
        });
        ui.add_space(4.0);
    }

    // Draw 1px #262626 right border along the right edge of the panel.
    let clip = ui.clip_rect();
    ui.painter().line_segment(
        [
            egui::pos2(clip.right(), clip.top()),
            egui::pos2(clip.right(), clip.bottom()),
        ],
        egui::Stroke::new(1.0_f32, COLOR_BORDER),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::info::ImageFacts;

    #[test]
    fn format_rows_with_facts() {
        let facts = ImageFacts {
            camera: "FUJIFILM X-T5".into(),
            lens: "XF35mmF1.4 R".into(),
            focal: "35mm (53mm eq.)".into(),
            aperture: "f/2.8".into(),
            shutter: "1/250".into(),
            iso: "ISO 400".into(),
            capture_time: "2026:01:02 10:11:12".into(),
            dimensions: "6000 × 4000".into(),
            zoom: "100%".into(),
        };

        let rows = format_info_rows(Some(&facts));
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[0], ("Camera", "FUJIFILM X-T5".to_string()));
        assert_eq!(rows[1], ("Lens", "XF35mmF1.4 R".to_string()));
        assert_eq!(rows[2], ("Focal", "35mm (53mm eq.)".to_string()));
        assert_eq!(rows[3], ("Aperture", "f/2.8".to_string()));
        assert_eq!(rows[4], ("Shutter", "1/250".to_string()));
        assert_eq!(rows[5], ("ISO", "ISO 400".to_string()));
        assert_eq!(rows[6], ("Captured", "2026:01:02 10:11:12".to_string()));
        assert_eq!(rows[7], ("Size", "6000 × 4000".to_string()));
        assert_eq!(rows[8], ("Zoom", "100%".to_string()));
    }

    #[test]
    fn format_rows_empty_or_none() {
        let rows = format_info_rows(None);
        assert_eq!(rows.len(), 9);
        for (_label, val) in rows {
            assert_eq!(val, "-");
        }
    }

    #[test]
    fn toggle_state_logic() {
        let mut show_info_panel = false;
        toggle_info_panel(&mut show_info_panel);
        assert!(show_info_panel);
        toggle_info_panel(&mut show_info_panel);
        assert!(!show_info_panel);
    }
}

//! Read-only Develop tab listing all `ImageFacts` (design §7): camera, lens, focal
//! (+35mm-equiv), aperture, shutter, ISO, capture time, dimensions, and live zoom.
//! Never produces an edit. While this tab is active the info overlay is suppressed
//! (they show the same facts; showing both at once is redundant), but the user's
//! overlay preference is preserved — the overlay reappears when they switch to
//! another tab. That suppression is applied at the overlay's draw site (gated on
//! `tool_state.active_tab != "info"`), NOT by mutating `show_info_overlay` here, so
//! toggling the tab is non-destructive to the setting.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::tool::{PanelTab, TabId};
use crate::state::AppState;

pub struct InfoTab;

impl PanelTab for InfoTab {
    fn id(&self) -> TabId {
        TabId("info")
    }

    fn label(&self) -> &str {
        "Info"
    }

    fn show(&self, ui: &mut egui::Ui, state: &mut AppState) -> Option<EditOutcome> {
        // The overlay is suppressed while this tab is active by the overlay's own
        // draw guard (see `app.rs`, gated on the active tab) — non-destructive, so
        // the overlay returns when the user leaves this tab. Nothing to do here.
        if let Some(v) = state.viewer.as_ref() {
            if let (Some(meta), Some(dims)) = (v.meta.as_ref(), v.image_dims) {
                let fit = ferrolite_vt::ViewTransform::fit(dims, v.viewport).zoom;
                let facts = crate::develop::info::ImageFacts::build(meta, v.view.zoom, fit, dims);
                for (label, value) in [
                    ("Camera", &facts.camera),
                    ("Lens", &facts.lens),
                    ("Focal", &facts.focal),
                    ("Aperture", &facts.aperture),
                    ("Shutter", &facts.shutter),
                    ("ISO", &facts.iso),
                    ("Captured", &facts.capture_time),
                    ("Size", &facts.dimensions),
                    ("Zoom", &facts.zoom),
                ] {
                    if !value.is_empty() {
                        ui.horizontal(|ui| {
                            ui.label(label);
                            ui.label(value.as_str());
                        });
                    }
                }
            } else {
                ui.label("No metadata available.");
            }
        }
        None // read-only: never produces an edit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::tool::{PanelTab, TabId};

    #[test]
    fn info_tab_identity() {
        assert_eq!(InfoTab.id(), TabId("info"));
        assert_eq!(InfoTab.label(), "Info");
    }
}

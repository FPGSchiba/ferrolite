//! Read-only Develop tab listing all `ImageFacts` (design §7): camera, lens, focal
//! (+35mm-equiv), aperture, shutter, ISO, capture time, dimensions, and live zoom.
//! Never produces an edit. Activating this tab closes the info overlay (they show
//! the same facts; showing both at once is redundant) — enforced here rather than
//! at the tab-bar click site because `PanelTab::show` is only invoked for the
//! active tab, so setting the flag on entry is sufficient and can't drift out of
//! sync with tab selection.

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
        // This tab and the info overlay show the same facts; while this tab is
        // active the overlay would be redundant screen clutter, so close it. Set
        // before borrowing `state.viewer` so there is no overlapping borrow with
        // the `state.settings` mutation below.
        state.settings.show_info_overlay = false;

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

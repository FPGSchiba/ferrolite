//! The shared group-checkbox modal, used for BOTH "Save preset" and
//! "Paste settings" (P7 design §6.3).
//!
//! Applying a PRESET opens no dialog — a preset already declares its groups —
//! so this appears only when saving one or pasting an ad-hoc copy.

use ferrolite_pipeline::GroupSet;

use super::apply::BATCH_UNDO_MAX;

/// User-visible label. Uses "Color" (not "Colour") to match the codebase's
/// `ColorSwatch`/`color_grade`/`ColorControl` naming.
pub fn group_label(g: GroupSet) -> &'static str {
    match g {
        GroupSet::LIGHT => "Light",
        GroupSet::COLOR => "Color",
        GroupSet::CURVE => "Tone curve",
        GroupSet::HSL => "HSL",
        GroupSet::GRADING => "Color grading",
        GroupSet::DETAIL => "Detail",
        GroupSet::EFFECTS => "Effects",
        GroupSet::GEOMETRY => "Geometry",
        GroupSet::LENS => "Lens corrections",
        GroupSet::MASKS => "Masks",
        _ => "Unknown",
    }
}

/// One-line hint under a group, or `None`.
fn group_hint(g: GroupSet) -> Option<&'static str> {
    match g {
        GroupSet::LIGHT => Some("exposure, contrast, highlights, shadows, whites, blacks"),
        GroupSet::COLOR => Some("temperature, tint, saturation, vibrance"),
        GroupSet::DETAIL => Some("noise reduction, sharpening"),
        GroupSet::EFFECTS => Some("dehaze"),
        GroupSet::GEOMETRY => Some("crop, rotate, keystone"),
        GroupSet::LENS => Some("distortion, TCA, vignetting amounts"),
        _ => None,
    }
}

/// Everything applicable except GEOMETRY and LENS — framing and optics are
/// per-image, so they are available but not on by default (design §3.2).
pub fn default_owns() -> GroupSet {
    let mut g = GroupSet::EMPTY;
    for candidate in GroupSet::ALL_APPLICABLE {
        if candidate != GroupSet::GEOMETRY && candidate != GroupSet::LENS {
            g.insert(candidate);
        }
    }
    g
}

pub enum GroupModalMode {
    Save { name: String },
    Paste { target_count: usize },
}

pub struct GroupModal {
    pub mode: GroupModalMode,
    pub owns: GroupSet,
    /// Set when the entered name is rejected; shown inline.
    pub name_error: Option<String>,
}

pub enum GroupModalOutcome {
    /// Still open.
    None,
    Cancelled,
    Confirmed {
        /// `Some` in Save mode, `None` in Paste mode.
        name: Option<String>,
        owns: GroupSet,
    },
}

impl GroupModal {
    pub fn new_save() -> Self {
        Self {
            mode: GroupModalMode::Save {
                name: String::new(),
            },
            owns: default_owns(),
            name_error: None,
        }
    }
    pub fn new_paste(target_count: usize) -> Self {
        Self {
            mode: GroupModalMode::Paste { target_count },
            owns: default_owns(),
            name_error: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> GroupModalOutcome {
        let title = match &self.mode {
            GroupModalMode::Save { .. } => "Save preset".to_string(),
            GroupModalMode::Paste { target_count } => {
                format!("Paste settings to {target_count} images")
            }
        };

        // The closure returns the outcome as the window's inner result, rather
        // than writing into a `let mut outcome` captured from the enclosing
        // scope — keeps the borrow of `self` inside the closure simple (one
        // mutable borrow for the closure's body, no separate captured
        // variable to reconcile) and mirrors `Window::show`'s
        // `Option<InnerResponse<R>>` return shape used elsewhere in the app.
        let response = egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| self.body(ui));

        response
            .and_then(|r| r.inner)
            .unwrap_or(GroupModalOutcome::None)
    }

    fn body(&mut self, ui: &mut egui::Ui) -> GroupModalOutcome {
        let mut outcome = GroupModalOutcome::None;

        if let GroupModalMode::Save { name } = &mut self.mode {
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(name);
            });
            if let Some(err) = &self.name_error {
                ui.colored_label(crate::theme::SEMANTIC_AMBER, err);
            }
            ui.add_space(6.0);
            ui.label("This preset sets:");
        }

        for g in GroupSet::ALL_APPLICABLE {
            let mut on = self.owns.contains(g);
            let resp = ui.checkbox(&mut on, group_label(g));
            if let Some(hint) = group_hint(g) {
                resp.on_hover_text(hint);
            }
            if on {
                self.owns.insert(g);
            } else {
                self.owns.remove(g);
            }
        }

        // Masks: permanently greyed with an honest reason (design §2 P7-D2).
        let mut masks_off = false;
        ui.add_enabled_ui(false, |ui| {
            ui.checkbox(&mut masks_off, group_label(GroupSet::MASKS));
        })
        .response
        .on_disabled_hover_text("Mask sync comes with a later phase");

        if let GroupModalMode::Paste { target_count } = &self.mode {
            if *target_count > BATCH_UNDO_MAX {
                ui.add_space(6.0);
                ui.colored_label(
                    crate::theme::SEMANTIC_AMBER,
                    format!("Undo won't be available for more than {BATCH_UNDO_MAX} images."),
                );
            }
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Select all").clicked() {
                for g in GroupSet::ALL_APPLICABLE {
                    self.owns.insert(g);
                }
            }
            if ui.button("None").clicked() {
                self.owns = GroupSet::EMPTY;
            }
            ui.add_space(16.0);
            if ui.button("Cancel").clicked() {
                outcome = GroupModalOutcome::Cancelled;
            }
            let can_confirm = !self.owns.is_empty();
            let confirm = ui.add_enabled(can_confirm, egui::Button::new("Apply"));
            if !can_confirm {
                confirm.on_disabled_hover_text("Select at least one group");
            } else if confirm.clicked() {
                let name = match &self.mode {
                    GroupModalMode::Save { name } => Some(name.clone()),
                    GroupModalMode::Paste { .. } => None,
                };
                outcome = GroupModalOutcome::Confirmed {
                    name,
                    owns: self.owns,
                };
            }
        });

        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GEOMETRY and LENS are off by default — framing and optics are per-image
    /// (design §3.2). Everything else applicable is on. MASKS is never on.
    #[test]
    fn geometry_and_lens_are_off_by_default_masks_never_on() {
        let d = default_owns();
        assert!(d.contains(GroupSet::LIGHT));
        assert!(d.contains(GroupSet::COLOR));
        assert!(d.contains(GroupSet::DETAIL));
        assert!(!d.contains(GroupSet::GEOMETRY), "framing is per-image");
        assert!(!d.contains(GroupSet::LENS), "optics are per-image");
        assert!(!d.contains(GroupSet::MASKS), "masks are out of P7");
    }

    /// Every applicable group has a distinct, non-empty label.
    #[test]
    fn every_applicable_group_has_a_unique_label() {
        let mut seen = std::collections::HashSet::new();
        for g in GroupSet::ALL_APPLICABLE {
            let l = group_label(g);
            assert!(!l.is_empty(), "empty label");
            assert!(seen.insert(l), "duplicate label {l}");
        }
        assert!(
            !group_label(GroupSet::MASKS).is_empty(),
            "MASKS still needs a label"
        );
    }
}

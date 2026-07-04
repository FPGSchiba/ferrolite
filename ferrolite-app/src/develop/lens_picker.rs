//! Searchable camera+lens picker (Spec 4.4, U8 Task 13): a modal listing
//! `LensDb::find_lenses` hits for the current search needle. `find_lenses` is
//! a cheap in-memory string filter over the bundled DB (no I/O, no XML
//! re-parse), so it runs directly on the UI thread — unlike the bake
//! (`lens_bake::spawn_lens_bake`), which does real per-node polynomial work
//! and always goes through `ferrolite-jobs` (CLAUDE.md rule 1).

/// Hard cap on the rendered result list. `find_lenses` can return every lens
/// in the bundled DB for a very short/common needle (e.g. "f/"); rendering
/// thousands of rows would itself become the per-frame cost this module is
/// meant to avoid. Capped, NOT silently: `show` reports whether the cap was
/// hit so the caller can render a "N+ results, refine your search" label
/// (CLAUDE.md: no silent caps).
pub const MAX_RESULTS: usize = 200;

/// One frame's picker outcome.
pub enum PickerOutcome {
    /// The user selected a lens.
    Picked(ferrolite_lens::LensMatch),
    /// The user dismissed the picker without choosing (close, click-outside, Esc).
    Dismissed,
}

/// Draw the picker modal. `camera_hint` is the current image's camera make
/// (or model), used by `find_lenses` to pick each hit's reported crop factor;
/// pass `""` when unknown (matches no camera, `find_lenses` falls back to
/// each lens's own calibration crop — see `ferrolite-lens/src/backend.rs`).
/// Returns `Some` the frame a choice is made (pick or dismiss); `None` while
/// still open and undecided.
pub fn show(
    ctx: &egui::Context,
    db: &dyn ferrolite_lens::LensDb,
    camera_hint: &str,
    query: &mut String,
) -> Option<PickerOutcome> {
    let mut outcome = None;
    let mut open = true;
    egui::Window::new("Choose lens")
        .collapsible(false)
        .resizable(true)
        .default_width(360.0)
        .default_height(420.0)
        .open(&mut open)
        .show(ctx, |ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(query)
                    .hint_text("Search lenses (e.g. \"24-70\")")
                    .desired_width(f32::INFINITY),
            );
            if ui.memory(|m| m.focused()).is_none() {
                resp.request_focus();
            }
            ui.separator();

            let hits = db.find_lenses(camera_hint, query);
            let truncated = hits.len() > MAX_RESULTS;
            let shown = &hits[..hits.len().min(MAX_RESULTS)];

            egui::ScrollArea::vertical()
                .id_salt("lens_picker_results")
                .max_height(320.0)
                .show(ui, |ui| {
                    if shown.is_empty() {
                        ui.weak("No lenses match.");
                    }
                    for m in shown {
                        if ui.selectable_label(false, &m.display_name).clicked() {
                            outcome = Some(PickerOutcome::Picked(m.clone()));
                        }
                    }
                });

            if truncated {
                ui.label(
                    egui::RichText::new(format!(
                        "Showing first {MAX_RESULTS} of {} matches — refine your search.",
                        hits.len()
                    ))
                    .weak(),
                );
            }

            ui.separator();
            if ui.button("Cancel").clicked() {
                outcome = Some(PickerOutcome::Dismissed);
            }
        });

    if !open && outcome.is_none() {
        outcome = Some(PickerOutcome::Dismissed);
    }
    outcome
}

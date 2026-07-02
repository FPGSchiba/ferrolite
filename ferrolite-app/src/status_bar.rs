//! The live status bar: selected-image EXIF, "N indexed", and job activity.

use crate::state::AppState;

/// Pure formatter for the right-hand activity string, so it is unit-testable.
///
/// Gated on `is_ingesting` (`state.active_ingests > 0`), which only flips at
/// ingest start/end, rather than comparing `ingest_done`/`ingest_total` per
/// frame. The done/total comparison is unstable mid-scan (total may still be
/// growing, or done can transiently equal a stale total) and previously
/// caused the status to flicker Idle <-> Ingesting during a single pass.
pub fn activity_text(is_ingesting: bool, ingest_done: usize, ingest_total: usize) -> String {
    if !is_ingesting {
        "Idle".to_string()
    } else {
        format!("Ingesting {ingest_done}/{ingest_total}")
    }
}

pub fn show(ui: &mut egui::Ui, state: &AppState) {
    let is_ingesting = state.active_ingests > 0;
    ui.horizontal_centered(|ui| {
        ui.monospace(selected_exif(state));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.monospace("GPU: idle"); // static until Plan 4
            ui.monospace("·");
            // "indexed / scanned": Phase A inserts stat-only rows (scanned),
            // Phase B fills their metadata (indexed).
            ui.monospace(format!("{} / {} indexed", state.indexed, state.scanned));
            ui.monospace("·");
            ui.monospace(activity_text(
                is_ingesting,
                state.ingest_done,
                state.ingest_total,
            ));
            // Only visible while an ingest pass is actually running, so it
            // disappears the instant `active_ingests` drops back to 0.
            if is_ingesting {
                let fraction = state.ingest_done as f32 / state.ingest_total.max(1) as f32;
                ui.add(
                    egui::ProgressBar::new(fraction.clamp(0.0, 1.0))
                        .desired_width(80.0)
                        .show_percentage(),
                );
            }
        });
        if let Some(w) = &state.warning {
            ui.separator();
            ui.label(
                egui::RichText::new(w)
                    .color(crate::theme::SEMANTIC_RED)
                    .size(11.0),
            );
        }
    });
}

fn selected_exif(state: &AppState) -> String {
    match state
        .selected
        .and_then(|id| state.images.iter().find(|i| i.id == id))
    {
        Some(img) => {
            let dims = match (img.width, img.height) {
                (Some(w), Some(h)) => format!("{w}×{h}"),
                _ => "—".to_string(),
            };
            let iso = img.iso.map(|v| format!("ISO {v}")).unwrap_or_default();
            format!("{} · {} · {}", img.filename, dims, iso)
        }
        None => "No selection".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_idle_when_not_ingesting() {
        assert_eq!(activity_text(false, 0, 0), "Idle");
    }

    #[test]
    fn activity_shows_progress_when_ingesting() {
        assert_eq!(activity_text(true, 12, 40), "Ingesting 12/40");
    }

    #[test]
    fn activity_generating_while_ingesting_regardless_of_counts() {
        // `done` transiently equal to `total` mid-scan (or a stale/zero total
        // early in the pass) must NOT flip the display to "Idle" while an
        // ingest is still active. `active_ingests > 0` is the only gate.
        assert_eq!(activity_text(true, 7, 7), "Ingesting 7/7");
    }

    #[test]
    fn activity_idle_when_totals_nonzero_but_not_ingesting() {
        // Once the ingest ends (active_ingests hits 0) the last pass's
        // done/total must not keep showing progress.
        assert_eq!(activity_text(false, 40, 40), "Idle");
    }
}

//! The live status bar: selected-image EXIF, "N indexed", and job activity.

use crate::state::AppState;

/// Pure formatter for the right-hand activity string, so it is unit-testable.
pub fn activity_text(
    active: usize,
    pending: usize,
    thumb_done: usize,
    thumb_total: usize,
) -> String {
    if active + pending == 0 {
        "Idle".to_string()
    } else {
        format!("Thumbnails {thumb_done}/{thumb_total}")
    }
}

pub fn show(ui: &mut egui::Ui, state: &AppState) {
    let active = state.jobs.active_count();
    let pending = state.jobs.pending_count();
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
                active,
                pending,
                state.thumb_done,
                state.thumb_total,
            ));
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
    fn activity_idle_when_no_jobs() {
        assert_eq!(activity_text(0, 0, 0, 0), "Idle");
    }

    #[test]
    fn activity_shows_progress_when_busy() {
        assert_eq!(activity_text(1, 5, 12, 40), "Thumbnails 12/40");
    }
}

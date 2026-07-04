//! The live status bar: selected-image EXIF, "N indexed", and job activity.

use crate::export::{ExportActivity, ExportKind};
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
        // Export activity indicator — visible in every view while an export runs
        // (hidden once done; the Export module keeps the "Done" summary).
        if let Some(a) = &state.export_activity {
            if !a.is_done() {
                ui.separator();
                ui.label(egui::RichText::new(export_status_text(a)).size(11.0));
                ui.add(egui::ProgressBar::new(a.fraction()).desired_width(70.0));
                if ui
                    .small_button("✕")
                    .on_hover_text("Cancel export")
                    .clicked()
                {
                    a.cancel_all();
                }
            }
        }
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

/// Truncate a filename to at most `max` chars, appending an ellipsis when cut.
/// Char-based (never splits a multi-byte codepoint).
pub fn truncate_name(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        return name.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = name.chars().take(keep).collect();
    out.push('…');
    out
}

/// Label text for the export indicator: filename (+ `completed/total` for a
/// batch), plus `(K failed)` when any failed. Single omits the count (total = 1).
pub fn export_status_text(a: &ExportActivity) -> String {
    let name = a
        .current_name
        .as_deref()
        .map(|n| truncate_name(n, 24))
        .unwrap_or_else(|| "…".to_string());
    let mut s = match a.kind {
        ExportKind::Single => format!("Exporting {name}"),
        ExportKind::Batch => format!("Exporting {name}  {}/{}", a.completed, a.total),
    };
    if a.failed > 0 {
        s.push_str(&format!("  ({} failed)", a.failed));
    }
    s
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

    #[test]
    fn truncate_name_keeps_short_and_ellipsizes_long() {
        assert_eq!(truncate_name("sunset.avif", 24), "sunset.avif");
        let long = "a_very_long_filename_that_overflows.avif";
        let t = truncate_name(long, 24);
        assert_eq!(t.chars().count(), 24);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn truncate_name_is_multibyte_safe() {
        // 30 accented chars — must not panic on a char boundary and must cap at 24.
        let s: String = "é".repeat(30);
        let t = truncate_name(&s, 24);
        assert_eq!(t.chars().count(), 24);
    }

    #[test]
    fn export_status_text_single_shows_filename_only() {
        let a = crate::export::ExportActivity::new_single(Some("hero.avif".into()));
        assert_eq!(export_status_text(&a), "Exporting hero.avif");
    }

    #[test]
    fn export_status_text_batch_shows_name_and_count() {
        let mut a = crate::export::ExportActivity::new_batch(8);
        a.completed = 3;
        a.start_item(Some("sunset.avif".into()));
        assert_eq!(export_status_text(&a), "Exporting sunset.avif  3/8");
    }

    #[test]
    fn export_status_text_appends_failed_only_when_nonzero() {
        let mut a = crate::export::ExportActivity::new_batch(8);
        a.completed = 5;
        a.failed = 1;
        a.start_item(Some("x.avif".into()));
        assert!(export_status_text(&a).ends_with("(1 failed)"));
        let mut b = crate::export::ExportActivity::new_batch(8);
        b.start_item(Some("x.avif".into()));
        assert!(!export_status_text(&b).contains("failed"));
    }

    #[test]
    fn export_status_text_missing_name_uses_placeholder() {
        let a = crate::export::ExportActivity::new_batch(2);
        assert!(export_status_text(&a).starts_with("Exporting …"));
    }
}

//! Library left panel: Catalog header, the Open-folder action, and a flat folder
//! list (with counts) read from the catalog. A nested tree is a later refinement.

use crate::ingest::spawn_ingest;
use crate::state::AppState;
use crate::theme;

pub fn show(ui: &mut egui::Ui, state: &mut AppState, ctx: &egui::Context) {
    ui.add_space(8.0);
    ui.colored_label(theme::TEXT_FAINT, "CATALOG");
    ui.label("All Photographs");
    ui.add_space(8.0);

    if ui.button("Open folder…").clicked() {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            spawn_ingest(state, ctx, folder);
        }
    }

    ui.add_space(12.0);
    ui.colored_label(theme::TEXT_FAINT, "FOLDERS");
    let folders = state.reads.list_folders().unwrap_or_default();
    for f in folders {
        let name = std::path::Path::new(&f.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| f.path.clone());
        let selected = state.current_folder == Some(f.id);
        if ui
            .selectable_label(selected, format!("{name}  ({})", f.image_count))
            .clicked()
        {
            state.current_folder = Some(f.id);
            state.selected = None;
            state.refresh_images();
        }
    }
}

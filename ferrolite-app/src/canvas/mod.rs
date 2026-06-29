mod callback;
pub use callback::CanvasResources;

use callback::CanvasCallback;

/// Add the wgpu paint callback that fills `rect` with the gradient.
pub fn paint(ui: &mut egui::Ui, rect: egui::Rect) {
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        CanvasCallback,
    ));
}

mod app;
mod canvas;
mod chrome;
mod module;
mod theme;
mod widgets;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default().with_inner_size([1440.0, 810.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Ferrolite",
        native_options,
        Box::new(|cc| Ok(Box::new(app::FerroliteApp::new(cc)))),
    )
}

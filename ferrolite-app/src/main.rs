mod app;
mod canvas;
mod chrome;
mod events;
mod ingest;
mod library;
mod metadata;
mod module;
mod state;
mod status_bar;
mod theme;
mod thumb_profile;
mod viewer;
mod widgets;

fn main() -> eframe::Result<()> {
    let icon = egui::IconData {
        rgba: chrome::icon::icon_rgba(256),
        width: 256,
        height: 256,
    };
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 810.0])
            .with_min_inner_size([960.0, 600.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        "Ferrolite",
        native_options,
        Box::new(|cc| Ok(Box::new(app::FerroliteApp::new(cc)))),
    )
}

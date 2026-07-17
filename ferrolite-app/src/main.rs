#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod camera_matrix;
mod canvas;
mod chrome;
mod develop;
mod diag;
mod diag_mem;
mod events;
mod export;
mod export_module;
mod help;
mod icons;
mod ingest;
mod library;
mod mem_probe;
mod metadata;
mod module;
mod monitor_profile;
mod notifications;
mod read_gate;
mod settings;
mod state;
mod status_bar;
mod theme;
mod viewer;
mod widgets;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--export-icons") {
        let dir = args
            .get(pos + 1)
            .cloned()
            .unwrap_or_else(|| "packaging/icons".to_string());
        export_icons(std::path::Path::new(&dir)).expect("icon export failed");
        return Ok(());
    }

    diag::init();
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

fn export_icons(dir: &std::path::Path) -> std::io::Result<()> {
    use image::{ImageBuffer, Rgba};
    let iconset = dir.join("AppIcon.iconset");
    std::fs::create_dir_all(&iconset)?;

    let render = |px: u32| -> ImageBuffer<Rgba<u8>, Vec<u8>> {
        let rgba = chrome::icon::icon_rgba(px);
        ImageBuffer::from_raw(px, px, rgba).expect("icon_rgba size mismatch")
    };

    // Main PNG.
    render(512)
        .save(dir.join("icon.png"))
        .map_err(std::io::Error::other)?;

    // ICO: image 0.25's IcoEncoder writes a single-image ICO; encode a 256px master
    // (the ICO format max) which Windows/NSIS accept and downscale for smaller slots.
    {
        use image::codecs::ico::IcoEncoder;
        use image::{ExtendedColorType, ImageEncoder};
        let master = render(256);
        let mut ico = std::fs::File::create(dir.join("icon.ico"))?;
        IcoEncoder::new(&mut ico)
            .write_image(master.as_raw(), 256, 256, ExtendedColorType::Rgba8)
            .map_err(std::io::Error::other)?;
    }

    // Apple .iconset PNGs.
    let apple: [(u32, &str); 10] = [
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ];
    for (px, name) in apple {
        render(px)
            .save(iconset.join(name))
            .map_err(std::io::Error::other)?;
    }
    Ok(())
}

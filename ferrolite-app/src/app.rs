use crate::canvas::{self, CanvasResources};
use crate::module::Module;
use crate::theme;
use crate::widgets::EguiSlider;

pub struct FerroliteApp {
    module: Module,
    thumb_size: f32,
}

impl FerroliteApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            let res = CanvasResources::new(rs);
            rs.renderer.write().callback_resources.insert(res);
        }
        Self {
            module: Module::default(),
            thumb_size: 46.0,
        }
    }
}

impl eframe::App for FerroliteApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("titlebar")
            .exact_height(30.0)
            .frame(egui::Frame::none().fill(theme::BG_TITLEBAR))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.colored_label(theme::ACCENT, "■");
                    ui.label("FERROLITE");
                    ui.add_space(12.0);
                    for m in ["File", "Edit", "Photo", "View", "Help"] {
                        ui.label(m);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.monospace("v0.0.1");
                        ui.add_space(12.0);
                        if ui
                            .selectable_label(!self.module.is_library(), "Develop")
                            .clicked()
                        {
                            self.module = Module::Develop;
                        }
                        if ui
                            .selectable_label(self.module.is_library(), "Library")
                            .clicked()
                        {
                            self.module = Module::Library;
                        }
                    });
                });
            });

        egui::SidePanel::left("left")
            .exact_width(236.0)
            .frame(egui::Frame::none().fill(theme::BG_PANEL))
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.colored_label(theme::TEXT_FAINT, "CATALOG");
                ui.label("All Photographs");
                ui.add_space(12.0);
                ui.colored_label(theme::TEXT_FAINT, "THUMBNAIL SIZE");
                ui.add(EguiSlider {
                    label: "Size",
                    value: &mut self.thumb_size,
                    min: 0.0,
                    max: 100.0,
                    default: 46.0,
                    step: 1.0,
                    decimals: 0,
                    unit: "",
                    bipolar: false,
                    signed: false,
                });
            });

        egui::TopBottomPanel::bottom("status")
            .exact_height(24.0)
            .frame(egui::Frame::none().fill(theme::BG_TITLEBAR))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.monospace("NEF · 8256×5504 · ISO 100 · 14mm · f/8 · 1/250s");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.monospace("GPU: idle");
                        ui.monospace("·");
                        ui.monospace("0 indexed");
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG_CANVAS))
            .show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();
                canvas::paint(ui, rect);
            });
    }
}

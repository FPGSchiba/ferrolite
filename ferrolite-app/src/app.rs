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

/// Invisible 6px resize grips along the four window edges.
/// Uses four transparent `egui::Area`s with `Sense::drag()` (the Area path),
/// because `ctx.interact(LayerId, Id, Rect, Sense)` does not exist in egui 0.29.
fn window_resize_grips(ctx: &egui::Context) {
    use egui::{Area, CursorIcon, Id, Order, Rect, ResizeDirection, Sense, ViewportCommand};
    let r = ctx.screen_rect();
    let m = 6.0_f32; // grip thickness
    let edges: [(Rect, ResizeDirection, CursorIcon); 4] = [
        (
            Rect::from_min_max(r.left_top(), egui::pos2(r.right(), r.top() + m)),
            ResizeDirection::North,
            CursorIcon::ResizeVertical,
        ),
        (
            Rect::from_min_max(egui::pos2(r.left(), r.bottom() - m), r.right_bottom()),
            ResizeDirection::South,
            CursorIcon::ResizeVertical,
        ),
        (
            Rect::from_min_max(r.left_top(), egui::pos2(r.left() + m, r.bottom())),
            ResizeDirection::West,
            CursorIcon::ResizeHorizontal,
        ),
        (
            Rect::from_min_max(egui::pos2(r.right() - m, r.top()), r.right_bottom()),
            ResizeDirection::East,
            CursorIcon::ResizeHorizontal,
        ),
    ];
    for (i, (rect, dir, cursor)) in edges.into_iter().enumerate() {
        let resp = Area::new(Id::new(("resize_grip", i)))
            .order(Order::Foreground)
            .fixed_pos(rect.min)
            .interactable(true)
            .sense(Sense::drag())
            .show(ctx, |ui| {
                // allocate the full grip rect so the Area covers the edge
                ui.allocate_rect(rect, Sense::drag())
            });
        if resp.inner.hovered() {
            ctx.set_cursor_icon(cursor);
        }
        if resp.inner.drag_started() {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
    }
}

impl eframe::App for FerroliteApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("titlebar")
            .exact_height(30.0)
            .frame(egui::Frame::none().fill(theme::BG_TITLEBAR))
            .show(ctx, |ui| {
                crate::chrome::title_bar(ctx, ui, &mut self.module, "v0.0.1");
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
                    // TODO(catalog): bind to selected-image metadata once ferrolite-decode/catalog land (Plan 2).
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

        // 1px window border — full-window foreground stroke so it never double-draws
        // against the side panel or status bar edges.
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("win_border"),
        ))
        .rect_stroke(
            ctx.screen_rect().shrink(0.5),
            0.0,
            egui::Stroke::new(1.0, theme::BORDER_STRONG),
        );

        window_resize_grips(ctx);
    }
}

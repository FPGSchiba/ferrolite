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

/// Title-bar height; resize grips start below it so they never fight the bar.
const TITLE_BAR_H: f32 = 30.0;

/// Invisible resize grips: West/East side edges (below the title bar), the South
/// bottom edge, and the two bottom corners (diagonal). The top edge/corners are
/// intentionally omitted — the 30px custom title bar owns the top (drag-to-move +
/// double-click-to-maximize), and any `Order::Foreground` grip there would hijack
/// the bar's pointer events (incl. the close button). Edges are listed before
/// corners so the corners (shown last, hence on top) win at the overlaps and give
/// a diagonal resize. Uses transparent `egui::Area`s with `Sense::drag()` because
/// `ctx.interact(LayerId, Id, Rect, Sense)` does not exist in egui 0.29.
fn window_resize_grips(ctx: &egui::Context) {
    use egui::{Area, CursorIcon, Id, Order, Rect, ResizeDirection, Sense, ViewportCommand};
    let r = ctx.screen_rect();
    let t = 8.0_f32; // edge thickness
    let c = 14.0_f32; // corner size
    let top = r.top() + TITLE_BAR_H; // side grips begin below the title bar
    let grips: [(Rect, ResizeDirection, CursorIcon); 5] = [
        // West edge
        (
            Rect::from_min_max(
                egui::pos2(r.left(), top),
                egui::pos2(r.left() + t, r.bottom()),
            ),
            ResizeDirection::West,
            CursorIcon::ResizeHorizontal,
        ),
        // East edge
        (
            Rect::from_min_max(egui::pos2(r.right() - t, top), r.right_bottom()),
            ResizeDirection::East,
            CursorIcon::ResizeHorizontal,
        ),
        // South edge
        (
            Rect::from_min_max(egui::pos2(r.left(), r.bottom() - t), r.right_bottom()),
            ResizeDirection::South,
            CursorIcon::ResizeVertical,
        ),
        // South-west corner (diagonal)
        (
            Rect::from_min_max(
                egui::pos2(r.left(), r.bottom() - c),
                egui::pos2(r.left() + c, r.bottom()),
            ),
            ResizeDirection::SouthWest,
            CursorIcon::ResizeNeSw,
        ),
        // South-east corner (diagonal)
        (
            Rect::from_min_max(egui::pos2(r.right() - c, r.bottom() - c), r.right_bottom()),
            ResizeDirection::SouthEast,
            CursorIcon::ResizeNwSe,
        ),
    ];
    for (i, (rect, dir, cursor)) in grips.into_iter().enumerate() {
        let resp = Area::new(Id::new(("resize_grip", i)))
            .order(Order::Foreground)
            .fixed_pos(rect.min)
            .interactable(true)
            .sense(Sense::drag())
            .show(ctx, |ui| ui.allocate_rect(rect, Sense::drag()))
            .inner;
        if resp.hovered() {
            ctx.set_cursor_icon(cursor);
        }
        if resp.drag_started() {
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

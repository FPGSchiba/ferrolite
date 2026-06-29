use crate::canvas::{self, CanvasResources};
use crate::module::Module;
use crate::theme;

pub struct FerroliteApp {
    module: Module,
    thumb_size: f32,
    state: crate::state::AppState,
}

impl FerroliteApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            let res = CanvasResources::new(rs);
            rs.renderer.write().callback_resources.insert(res);
        }
        let state = crate::state::AppState::new().expect("open catalog");
        Self {
            module: Module::default(),
            thumb_size: 46.0,
            state,
        }
    }
}

/// Title-bar height; resize edges start below it so they never fight the bar.
const TITLE_BAR_H: f32 = 30.0;

/// Borderless-window edge/corner resize, driven purely by the pointer position —
/// deliberately NOT via overlay `egui::Area`s: an interactable `Order::Foreground`
/// Area over the edges steals the custom title bar's pointer input after a
/// maximize/restore transition (buttons + drag on the right half go dead). Instead
/// we read the latest pointer position; when it is over a window edge *below* the
/// title bar we show the resize cursor and start an OS resize on primary press.
/// The top edge/corners are omitted — the title bar owns the top (drag + maximize).
fn window_resize(ctx: &egui::Context) {
    use egui::{CursorIcon, ResizeDirection, ViewportCommand};
    let Some(pos) = ctx.pointer_latest_pos() else {
        return;
    };
    let r = ctx.screen_rect();
    let m = 8.0_f32; // edge band thickness
    if pos.y < r.top() + TITLE_BAR_H {
        return; // never resize from within the title bar
    }
    let left = pos.x <= r.left() + m;
    let right = pos.x >= r.right() - m;
    let bottom = pos.y >= r.bottom() - m;
    let dir = if bottom && right {
        Some((ResizeDirection::SouthEast, CursorIcon::ResizeNwSe))
    } else if bottom && left {
        Some((ResizeDirection::SouthWest, CursorIcon::ResizeNeSw))
    } else if right {
        Some((ResizeDirection::East, CursorIcon::ResizeHorizontal))
    } else if left {
        Some((ResizeDirection::West, CursorIcon::ResizeHorizontal))
    } else if bottom {
        Some((ResizeDirection::South, CursorIcon::ResizeVertical))
    } else {
        None
    };
    if let Some((dir, cursor)) = dir {
        ctx.set_cursor_icon(cursor);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
    }
}

impl eframe::App for FerroliteApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain job results into state; upload textures for ThumbReady events.
        while let Ok(event) = self.state.rx.try_recv() {
            if let Some((id, jpeg)) = self.state.apply(event) {
                self.state.upload_thumbnail(ctx, id, jpeg);
            }
            self.state.dirty = true;
        }
        if self.state.dirty {
            self.state.refresh_images();
            self.state.dirty = false;
        }

        egui::TopBottomPanel::top("titlebar")
            .exact_height(30.0)
            .frame(egui::Frame::none().fill(theme::BG_TITLEBAR))
            .show(ctx, |ui| {
                crate::chrome::title_bar(ctx, ui, &mut self.module, "v0.0.1");
            });

        egui::TopBottomPanel::top("toolbar")
            .exact_height(40.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::BG_TOOLBAR)
                    .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
            )
            .show(ctx, |ui| {
                if self.module.is_library() {
                    crate::library::toolbar::show(ui, &mut self.thumb_size);
                }
            });

        egui::TopBottomPanel::bottom("status")
            .exact_height(24.0)
            .frame(egui::Frame::none().fill(theme::BG_TITLEBAR))
            .show(ctx, |ui| {
                crate::status_bar::show(ui, &self.state);
            });

        egui::SidePanel::left("left")
            .exact_width(236.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::BG_PANEL)
                    // Clear left/right padding so content doesn't hug the window edge.
                    .inner_margin(egui::Margin {
                        left: 14.0,
                        right: 12.0,
                        top: 4.0,
                        bottom: 8.0,
                    }),
            )
            .show(ctx, |ui| {
                crate::library::panel::show(ui, &mut self.state, ctx);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG_CANVAS))
            .show(ctx, |ui| {
                if self.module.is_library() {
                    crate::library::grid::show(ui, &mut self.state, self.thumb_size + 60.0);
                } else {
                    let rect = ui.available_rect_before_wrap();
                    canvas::paint(ui, rect); // Develop stub keeps the wgpu canvas
                }
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

        window_resize(ctx);
    }
}

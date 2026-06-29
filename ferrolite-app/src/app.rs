use crate::canvas::{self, CanvasResources};
use crate::module::Module;
use crate::theme;
use crate::viewer;

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

impl FerroliteApp {
    /// Handle a tier-1 preview: convert to display-linear, build the rung-1
    /// `VirtualTexture`, stash it (+ GpuContext) in eframe's `callback_resources`,
    /// and fit the view. Stale events (no open viewer, or a different image_id)
    /// are dropped — the user may have closed/switched the viewer mid-decode.
    fn apply_preview_ready(
        &mut self,
        frame: &eframe::Frame,
        image_id: i64,
        image: &ferrolite_image::ImageBuffer,
    ) {
        let Some(v) = self.state.viewer.as_mut() else {
            return; // viewer closed while decoding
        };
        if v.image_id != image_id {
            return; // stale: a different image is now open
        }
        let Some(rs) = frame.wgpu_render_state() else {
            return; // no wgpu backend (should not happen in this build)
        };

        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
        let linear = viewer::load::preview_to_linear(image);
        let dims = (linear.width, linear.height);
        let vt = ferrolite_vt::VirtualTexture::single_texture(&gpu, &linear, rs.target_format);

        // Fit to the last-known viewport; fall back to the image's own size when
        // the canvas has not painted yet (zoom is normalized away by fit anyway).
        let viewport = if v.viewport.0 > 0.0 && v.viewport.1 > 0.0 {
            v.viewport
        } else {
            (dims.0 as f32, dims.1 as f32)
        };
        v.view = ferrolite_vt::ViewTransform::fit(dims, viewport);
        v.loaded = true;

        rs.renderer
            .write()
            .callback_resources
            .insert(viewer::ViewerGpu {
                ctx: gpu,
                vt,
                image_id,
            });
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
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Drain job results into state; upload textures for ThumbReady events and
        // build the viewer's rung-1 VirtualTexture for PreviewReady events.
        while let Ok(event) = self.state.rx.try_recv() {
            if let crate::events::AppEvent::PreviewReady { image_id, image } = &event {
                self.apply_preview_ready(frame, *image_id, image);
                self.state.dirty = true;
                continue;
            }
            if let Some((id, jpeg)) = self.state.apply(event) {
                self.state.upload_thumbnail(ctx, id, jpeg);
            }
            self.state.dirty = true;
        }
        if self.state.dirty {
            self.state.refresh_images();
            self.state.dirty = false;
        }

        // One-time startup rescan of all roots (first frame, ctx available here).
        if !self.state.startup_rescan_done {
            crate::ingest::spawn_startup_rescan(&mut self.state, ctx);
            self.state.startup_rescan_done = true;
        }

        // Periodic background watcher for new files in the selected subtree.
        let now = std::time::Instant::now();
        if crate::ingest::should_watch(
            now,
            self.state.last_watch_check,
            crate::ingest::WATCH_INTERVAL,
            self.state.current_folder,
            self.state.active_ingests,
        ) {
            self.state.last_watch_check = Some(now);
            crate::ingest::spawn_watch_scan(&mut self.state, ctx);
        }
        // Wake on the watcher cadence even when otherwise idle.
        ctx.request_repaint_after(crate::ingest::WATCH_INTERVAL);

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
                    let changed = crate::library::toolbar::show(
                        ui,
                        &mut self.thumb_size,
                        &mut self.state.include_subfolders,
                    );
                    if changed {
                        self.state.dirty = true;
                    }
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

        // Esc closes the viewer.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.state.viewer = None;
        }

        // Submit the tier-1 preview decode once when a viewer opens.
        if let Some(v) = self.state.viewer.as_mut() {
            if !v.preview_requested {
                viewer::load::spawn_preview(
                    &self.state.jobs,
                    &self.state.tx,
                    ctx,
                    v.image_id,
                    v.path.clone(),
                    v.kind,
                );
                v.preview_requested = true;
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG_CANVAS))
            .show(ctx, |ui| {
                if let Some(v) = self.state.viewer.as_mut() {
                    let loading = viewer::paint(ui, v);
                    if loading {
                        // Keep animating until the first pixel is ready.
                        ui.ctx().request_repaint();
                    }
                } else if self.module.is_library() {
                    crate::library::grid::show(ui, &mut self.state, self.thumb_size + 60.0);
                } else {
                    let rect = ui.available_rect_before_wrap();
                    canvas::paint(ui, rect); // Develop stub keeps the wgpu canvas
                }
            });

        // Remove-folder confirmation (subtrees only; leaves remove immediately).
        if let Some(pending) = self.state.pending_remove.clone() {
            let mut open = true;
            egui::Window::new("Remove folder from catalog")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Remove \u{201c}{}\u{201d} and its subfolders ({} images) from the catalog?",
                        pending.name, pending.subtree_count
                    ));
                    ui.label(
                        egui::RichText::new("Files on disk are not deleted.")
                            .color(theme::TEXT_DIM)
                            .size(11.0),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Remove").clicked() {
                            self.state.remove_folder_cascade(pending.id);
                            self.state.pending_remove = None;
                            open = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.pending_remove = None;
                            open = false;
                        }
                    });
                });
            if !open {
                self.state.pending_remove = None;
            }
        }

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

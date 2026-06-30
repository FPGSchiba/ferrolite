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
            // Pre-warm all four display pipelines once at startup. Every image
            // open will borrow from this holder instead of compiling a new pipeline.
            let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
            let pipelines = ferrolite_vt::DisplayPipelines::new(&gpu, rs.target_format);
            rs.renderer
                .write()
                .callback_resources
                .insert(viewer::ViewerPipelines { pipelines });
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
        // Fetch the pre-warmed pipelines, build the VT while borrowing them,
        // then release the lock before inserting ViewerGpu (separate write scope).
        let vt = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            ferrolite_vt::VirtualTexture::single_texture(&gpu, &linear, &vp.pipelines)
        };

        // Fit to the last-known viewport; fall back to the image's own size when
        // the canvas has not painted yet (zoom is normalized away by fit anyway).
        let viewport = if v.viewport.0 > 0.0 && v.viewport.1 > 0.0 {
            v.viewport
        } else {
            (dims.0 as f32, dims.1 as f32)
        };
        v.view = ferrolite_vt::ViewTransform::fit(dims, viewport);
        v.image_dims = Some(dims);
        v.loaded = true;
        // A Standard image's preview IS the full-resolution image, so there is no
        // tier-2 to wait for — go idle once the preview is up so the repaint loop
        // does not spin.
        if v.kind != ferrolite_image::FileKind::Raw {
            v.idle = true;
        }

        rs.renderer
            .write()
            .callback_resources
            .insert(viewer::ViewerGpu {
                ctx: gpu,
                preview: vt,
                full: None,
                image_id,
            });
    }

    /// Handle a tier-2 full decode: build a `PyramidTileSource` from the
    /// display-linear image, wrap it as a sparse (rung-4) `VirtualTexture`,
    /// store it alongside the preview in `ViewerGpu`, and begin the preview→full
    /// crossfade. Stale events (no open viewer / different image_id) are dropped.
    fn apply_full_decoded(
        &mut self,
        frame: &eframe::Frame,
        image_id: i64,
        image: &ferrolite_image::LinearRgbaF32,
    ) {
        let Some(v) = self.state.viewer.as_mut() else {
            return; // viewer closed while decoding
        };
        if v.image_id != image_id {
            return; // stale: a different image is now open
        }
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };

        // `v` only guarded staleness above; release the borrow before taking the
        // renderer lock so we can re-borrow afterwards. (Both live on `self` but
        // do not alias.)
        let _ = v;

        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
        let source: std::sync::Arc<dyn ferrolite_vt::TileSource + Send + Sync> =
            std::sync::Arc::new(ferrolite_vt::PyramidTileSource::new(image.clone()));
        // Fetch the pre-warmed pipelines, build the sparse VT while borrowing them,
        // then release the read lock before the write scope that installs it.
        let full = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            ferrolite_vt::VirtualTexture::sparse(
                &gpu,
                source,
                std::sync::Arc::clone(&self.state.jobs),
                VIEWER_TILE_BUDGET,
                &vp.pipelines,
            )
        };

        // Store the full VT into the existing holder (keep the preview around so
        // the crossfade can keep showing it until the full tiles are resident).
        // Only flip `full_ready` / start the crossfade if the holder was actually
        // updated for THIS image — otherwise (stale holder) the viewer would
        // permanently idle on the preview with no full VT to swap to.
        let mut full_installed = false;
        {
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == image_id {
                    g.full = Some(full);
                    full_installed = true;
                }
            }
        }

        if full_installed {
            if let Some(v) = self.state.viewer.as_mut() {
                if v.image_id == image_id {
                    v.full_ready = true;
                    v.begin_crossfade();
                    if !v.op_stack.is_identity() {
                        // Build the GPU-resident pyramid + per-tile edit pipeline.
                        let ctx_arc = std::sync::Arc::new(
                            ferrolite_gpu::GpuContext::from_render_state(rs),
                        );
                        let pyramid = std::sync::Arc::new(
                            ferrolite_pipeline::GpuPyramidSource::new(&gpu, image),
                        );
                        let tep = ferrolite_pipeline::TileEditPipeline::new(
                            ctx_arc,
                            pyramid,
                            v.op_stack.clone(),
                        );
                        v.edit_producer = Some(viewer::EditTileProducer::new(tep));
                        // Mark the VT producer-driven + bump its version so the
                        // producer fills tiles instead of the CPU path.
                        let mut renderer = rs.renderer.write();
                        if let Some(g) =
                            renderer.callback_resources.get_mut::<viewer::ViewerGpu>()
                        {
                            if let Some(full) = g.full.as_mut() {
                                full.set_producing(true);
                                full.set_opstack_version(&g.ctx, 1);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Physical tile-pool budget for the viewer's sparse VT. 256 tiles × 256² ×
/// RGBA16F ≈ 128 MB of GPU memory — generous headroom for a fit-to-window view
/// plus a few zoom levels of the quad-binned (half-res) full image.
const VIEWER_TILE_BUDGET: u32 = 256;

/// Max edited tiles rendered per frame on the render thread (bounds GPU work;
/// CLAUDE.md GPU rule). Remaining needed tiles are produced on subsequent frames.
const MAX_PRODUCE_PER_FRAME: usize = 8;

impl FerroliteApp {
    /// Per-frame viewer drive: advance the crossfade, drive the sparse VT
    /// (reconcile against GPU-truth feedback + drain finished loads), paint the
    /// preview or full image (swap-on-ready), and request a repaint ONLY while
    /// there is still work — so a finished/failed viewer goes idle (no busy-loop).
    ///
    /// Crossfade approach 4b (swap-on-ready): we keep showing the sharp preview
    /// until the crossfade ramp completes AND the current view's tiles are all
    /// resident (`sparse_pending() == 0`), then hard-swap to the full VT. The
    /// full is already sharp at that point, so there is no blurry pop. True alpha
    /// blending in the callback would need a second alpha-blended pipeline pass;
    /// 4b avoids that cost and reads as instant at the 150 ms ramp.
    fn drive_viewer(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        let dt = ui.ctx().input(|i| i.stable_dt);

        // First, reconcile any stale GPU holder: if the holder belongs to an
        // image other than the open viewer (navigation happened), cancel its
        // tile jobs so they stop competing with the new image's loads.
        let open_id = self.state.viewer.as_ref().map(|v| v.image_id);

        // Drive the sparse VT for the open viewer and learn how many tiles are
        // still pending (so we can both gate the swap and terminate the repaint).
        // `request_view_feedback` reconciles residency against the PRIOR frame's
        // GPU feedback marks (one frame latent); the paint callback's `draw_sparse`
        // marks the CURRENT frame. This converges over frames; the coarse-LOD
        // fallback keeps showing tiles meanwhile.
        let mut tiles_pending: Option<usize> = None;
        if let (Some(rs), Some(_v)) = (frame.wgpu_render_state(), self.state.viewer.as_ref()) {
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if Some(g.image_id) != open_id {
                    // Stale holder from a superseded viewer: stop its tile jobs.
                    if let Some(full) = g.full.as_mut() {
                        full.cancel_sparse();
                    }
                } else if let Some(full) = g.full.as_mut() {
                    full.request_view_feedback(&g.ctx);
                    // Plan 3: when an edit producer is present, render the needed
                    // tiles on the render thread (bounded). `produce_view` borrows
                    // the producer (which lives in ViewerState) by &mut per call.
                    if let Some(v) = self.state.viewer.as_mut() {
                        if let Some(producer) = v.edit_producer.as_mut() {
                            let needed = full.needed_now();
                            full.produce_view(&g.ctx, producer, &needed, MAX_PRODUCE_PER_FRAME);
                        }
                    }
                    tiles_pending = full.sparse_pending();
                }
            }
        }

        let Some(v) = self.state.viewer.as_mut() else {
            return;
        };

        // If the view changed (pan/zoom in `viewer::paint` already cleared `idle`,
        // but a programmatic change might not), `request_view_feedback` above may
        // have submitted new tile loads. Resume the drive loop so they drain + display.
        if matches!(tiles_pending, Some(n) if n > 0) {
            v.idle = false;
        }

        // Advance the crossfade ramp; swap to full once it has completed and the
        // current view's tiles are all resident.
        let factor = v.tick_crossfade(dt);
        let tiles_settled = matches!(tiles_pending, Some(0));
        let show_full = v.full_ready && factor >= 1.0 && tiles_settled;

        // Terminal state: full ready, crossfade done, nothing pending -> idle.
        if show_full && !v.crossfading {
            v.idle = true;
        }

        let crossfading = v.crossfading;
        // `paint` applies this frame's pan/zoom and clears `idle` when the view
        // moved, so read `idle` AFTER it to catch an interaction this frame.
        let loading_preview = viewer::paint(ui, v, show_full);
        let idle = v.idle;

        // Repaint only while there is pending work:
        //  - preview not yet uploaded, or
        //  - crossfade ramp still advancing, or
        //  - sparse tiles still loading.
        // Once `idle` (full ready + settled, or a failure marked it idle) we stop.
        // A pan/zoom clears `idle` so the loop resumes and the new view's tiles
        // (requested next frame) drain and display.
        let tiles_loading = matches!(tiles_pending, Some(n) if n > 0);
        if !idle && (loading_preview || crossfading || tiles_loading) {
            ui.ctx().request_repaint();
        }
    }

    /// The single image-open path: cancel the previously-open viewer's in-flight
    /// tile jobs, open the new image's two-tier load, switch to Develop, and request
    /// a repaint so the viewer is drawn on the very next frame (otherwise egui would
    /// idle on the grid until the next input event, which reads as a stall).
    fn open_record(
        &mut self,
        ctx: &egui::Context,
        frame: &mut eframe::Frame,
        rec: &ferrolite_catalog::ImageRecord,
    ) {
        if let Some(old) = self.state.viewer.as_ref() {
            let old_id = old.image_id;
            old.cancel_loads();
            self.cancel_viewer_tiles(frame, old_id);
        }
        self.state.open_image_in_viewer(rec);
        self.module = crate::module::Module::Develop;
        ctx.request_repaint();
    }

    /// Cancel the sparse VT's in-flight tile-load jobs for the named viewer.
    /// The VT lives in `callback_resources`; the decode jobs are cancelled
    /// separately via `ViewerState::cancel_loads`. Guarded on `image_id` so we
    /// never cancel a holder that already belongs to a newer viewer.
    fn cancel_viewer_tiles(&self, frame: &eframe::Frame, image_id: i64) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let mut renderer = rs.renderer.write();
        if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
            if g.image_id == image_id {
                if let Some(full) = g.full.as_mut() {
                    full.cancel_sparse();
                }
            }
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
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Drain job results into state; upload textures for ThumbReady events and
        // build the viewer's rung-1 VirtualTexture for PreviewReady events.
        let mut ingest_done = false;
        while let Ok(event) = self.state.rx.try_recv() {
            match &event {
                crate::events::AppEvent::PreviewReady { image_id, image } => {
                    self.apply_preview_ready(frame, *image_id, image);
                    self.state.dirty = true;
                    continue;
                }
                crate::events::AppEvent::FullDecoded { image_id, image } => {
                    self.apply_full_decoded(frame, *image_id, image);
                    self.state.dirty = true;
                    continue;
                }
                crate::events::AppEvent::FullFailed { image_id } => {
                    // Keep the preview; mark the viewer idle so the repaint loop
                    // can stop (the error was already logged on the job thread).
                    if let Some(v) = self.state.viewer.as_mut() {
                        if v.image_id == *image_id {
                            v.idle = true;
                        }
                    }
                    self.state.dirty = true;
                    continue;
                }
                crate::events::AppEvent::IngestDone => {
                    ingest_done = true;
                }
                _ => {}
            }
            if let Some((id, jpeg)) = self.state.apply(event) {
                self.state.upload_thumbnail(ctx, id, jpeg);
            }
            self.state.dirty = true;
        }
        // Refresh toolbar metadata-filter caches once per completed ingest (bounded).
        if ingest_done {
            self.state.reload_vocab();
        }
        if self.state.dirty {
            self.state.refresh_images();
            self.state.dirty = false;
        }

        // One-time startup rescan of all roots (first frame, ctx available here).
        if !self.state.startup_rescan_done {
            crate::ingest::spawn_startup_rescan(&mut self.state, ctx);
            self.state.reload_vocab();
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

        let mut film_clicked: Option<i64> = None;
        if self.module.is_library() {
            egui::TopBottomPanel::top("toolbar")
                .exact_height(40.0)
                .frame(
                    egui::Frame::none()
                        .fill(theme::BG_TOOLBAR)
                        .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
                )
                .show(ctx, |ui| {
                    let changed =
                        crate::library::toolbar::show(ui, &mut self.thumb_size, &mut self.state);
                    if changed {
                        self.state.dirty = true;
                    }
                });
        } else {
            egui::TopBottomPanel::top("develop_filter")
                .exact_height(36.0)
                .frame(
                    egui::Frame::none()
                        .fill(theme::BG_TOOLBAR)
                        .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
                )
                .show(ctx, |ui| {
                    if crate::library::develop_filter_bar::show(ui, &mut self.state) {
                        self.state.dirty = true;
                    }
                });
            egui::TopBottomPanel::top("develop_filmstrip")
                .exact_height(72.0)
                .frame(
                    egui::Frame::none()
                        .fill(theme::BG_TOOLBAR)
                        .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
                )
                .show(ctx, |ui| {
                    let current = self.state.viewer.as_ref().map(|v| v.image_id);
                    film_clicked = crate::library::filmstrip::show(ui, &mut self.state, current);
                });
        }
        if let Some(id) = film_clicked {
            if let Some(rec) = self.state.images.iter().find(|r| r.id == id).cloned() {
                self.open_record(ctx, frame, &rec);
            }
        }

        egui::TopBottomPanel::bottom("status")
            .exact_height(24.0)
            .frame(egui::Frame::none().fill(theme::BG_TITLEBAR))
            .show(ctx, |ui| {
                crate::status_bar::show(ui, &self.state);
            });

        if self.module == crate::module::Module::Develop {
            if let Some(image_id) = self.state.viewer.as_ref().map(|v| v.image_id) {
                egui::TopBottomPanel::bottom("develop_meta")
                    .exact_height(34.0)
                    .frame(
                        egui::Frame::none()
                            .fill(theme::BG_TOOLBAR)
                            .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
                    )
                    .show(ctx, |ui| {
                        crate::library::develop_metadata_bar::show(
                            ui,
                            &mut self.state,
                            ctx,
                            image_id,
                        );
                    });
            }
        }

        if self.module.is_library() {
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
        }

        // Esc closes the viewer. Cancel its in-flight decode + tile jobs first so a
        // closed image's work stops competing with whatever is opened next.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if let Some(v) = self.state.viewer.take() {
                v.cancel_loads();
                self.cancel_viewer_tiles(frame, v.image_id);
                self.module = crate::module::Module::Library;
            }
        }

        // Enter opens the selected image in the viewer (library grid only, no
        // viewer already open, exactly one image selected). Suppressed while the
        // remove-confirmation modal is up or a text field holds focus (so a
        // future search box's Enter won't pop the viewer).
        if self.module.is_library()
            && self.state.viewer.is_none()
            && self.state.pending_remove.is_none()
            && !ctx.wants_keyboard_input()
            && ctx.input(|i| i.key_pressed(egui::Key::Enter))
        {
            if let Some(sel_id) = self.state.selected {
                if let Some(rec) = self.state.images.iter().find(|r| r.id == sel_id).cloned() {
                    self.open_record(ctx, frame, &rec);
                }
            }
        }

        // Keyboard metadata commands: rating 0–5 (I = Pick, O = Reject), all as
        // toggles. In Library (no viewer) they apply to the grid selection; in
        // Develop or Library+viewer they apply to the open viewer image.
        if self.state.pending_remove.is_none() && !ctx.wants_keyboard_input() {
            use ferrolite_image::{Flag, Rating};

            // --- 1. Read key intent ---
            enum KeyIntent {
                Rating(u8),
                Flag(Flag),
            }
            let intent = ctx.input(|i| {
                for n in 0..=5u8 {
                    let key = match n {
                        0 => egui::Key::Num0,
                        1 => egui::Key::Num1,
                        2 => egui::Key::Num2,
                        3 => egui::Key::Num3,
                        4 => egui::Key::Num4,
                        _ => egui::Key::Num5,
                    };
                    if i.key_pressed(key) {
                        return Some(KeyIntent::Rating(n));
                    }
                }
                if i.key_pressed(egui::Key::I) {
                    Some(KeyIntent::Flag(Flag::Pick))
                } else if i.key_pressed(egui::Key::O) {
                    Some(KeyIntent::Flag(Flag::Reject))
                } else {
                    None
                }
            });

            if let Some(intent) = intent {
                // --- 2. Resolve target image id ---
                let target_id = if self.module.is_library() && self.state.viewer.is_none() {
                    self.state.selected
                } else {
                    self.state.viewer.as_ref().map(|v| v.image_id)
                };

                if let Some(target_id) = target_id {
                    // --- 3. Look up current value ---
                    let rec = self.state.images.iter().find(|r| r.id == target_id);
                    let cur_rating = rec.map(|r| r.rating.get()).unwrap_or(0);
                    let cur_flag = rec.map(|r| r.flag).unwrap_or(Flag::None);

                    // --- 4. Build toggled edit ---
                    let edit = match intent {
                        KeyIntent::Rating(n) => crate::metadata::MetaEdit::SetRating(Rating::new(
                            crate::metadata::toggle_rating(cur_rating, n),
                        )),
                        KeyIntent::Flag(f) => crate::metadata::MetaEdit::SetFlag(
                            crate::metadata::toggle_flag(cur_flag, f),
                        ),
                    };

                    // --- 5. Apply ---
                    if self.module.is_library() && self.state.viewer.is_none() {
                        self.state.apply_metadata_edit(ctx, edit);
                    } else {
                        self.state
                            .apply_metadata_edit_to_image(ctx, target_id, edit);
                    }
                }
            }
        }

        // Left/Right move between images while viewing (Develop), non-cyclic.
        if self.module == crate::module::Module::Develop
            && self.state.viewer.is_some()
            && !ctx.wants_keyboard_input()
        {
            let dir = ctx.input(|i| {
                if i.key_pressed(egui::Key::ArrowRight) {
                    Some(crate::viewer::nav::Step::Next)
                } else if i.key_pressed(egui::Key::ArrowLeft) {
                    Some(crate::viewer::nav::Step::Prev)
                } else {
                    None
                }
            });
            if let Some(dir) = dir {
                let cur_id = self.state.viewer.as_ref().map(|v| v.image_id);
                if let Some(cur_id) = cur_id {
                    let ids: Vec<i64> = self.state.images.iter().map(|r| r.id).collect();
                    if let Some(next_id) = crate::viewer::nav::neighbor_in_set(&ids, cur_id, dir) {
                        if let Some(rec) =
                            self.state.images.iter().find(|r| r.id == next_id).cloned()
                        {
                            self.open_record(ctx, frame, &rec);
                        }
                    }
                }
            }
        }

        // Submit the tier-1 preview decode once when a viewer opens, and (for RAW)
        // the tier-2 full decode in parallel.
        if let Some(v) = self.state.viewer.as_mut() {
            if !v.preview_requested {
                let h = viewer::load::spawn_preview(
                    &self.state.jobs,
                    &self.state.tx,
                    ctx,
                    v.image_id,
                    v.path.clone(),
                    v.kind,
                );
                v.preview_handle = Some(h);
                v.preview_requested = true;
            }
            // Tier-2 is RAW-only: a Standard image's preview is already full-res.
            if !v.full_requested && v.kind == ferrolite_image::FileKind::Raw {
                let h = viewer::load::spawn_full(
                    &self.state.jobs,
                    &self.state.tx,
                    ctx,
                    v.image_id,
                    v.path.clone(),
                );
                v.full_handle = Some(h);
                v.full_requested = true;
            }
        }

        let mut opened: Option<i64> = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG_CANVAS))
            .show(ctx, |ui| {
                if self.module.is_library() {
                    // Grid; capture a double-clicked id to open after the panel closes.
                    opened =
                        crate::library::grid::show(ui, &mut self.state, self.thumb_size + 60.0);
                } else if self.state.viewer.is_some() {
                    self.drive_viewer(ui, frame);
                    if let Some(image_id) = self.state.viewer.as_ref().map(|v| v.image_id) {
                        let rect = ui.min_rect();
                        let resp =
                            ui.interact(rect, ui.id().with("loupe_ctx"), egui::Sense::click());
                        resp.context_menu(|ui| {
                            crate::library::image_context_menu::show(
                                ui,
                                &mut self.state,
                                image_id,
                                true,
                            );
                        });
                    }
                } else {
                    let rect = ui.available_rect_before_wrap();
                    canvas::paint(ui, rect); // Develop with no image open: stub canvas
                }
            });
        if let Some(id) = opened {
            if let Some(rec) = self.state.images.iter().find(|r| r.id == id).cloned() {
                self.open_record(ctx, frame, &rec);
            }
        }

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

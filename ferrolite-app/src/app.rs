pub mod shortcuts;
pub mod controller;

use crate::canvas::{self, CanvasResources};
use crate::module::Module;
use crate::theme;
use crate::viewer;

pub struct FerroliteApp {
    pub(crate) module: Module,
    thumb_size: f32,
    pub(crate) state: crate::state::AppState,
    /// Last frame's `viewer.crop_active`. A transition (enter/exit crop mode)
    /// with no other edit does not otherwise re-render the preview, so we detect
    /// the edge and force a `set_preview_and_full` on the same frame before paint:
    /// enter → crop=full+angle view; exit → the real crop applied.
    crop_active_prev: bool,
    /// Set when a Develop→Library switch happens mid-frame, after the filmstrip
    /// has already painted (and thus recorded) its thumbnail textures. Clearing
    /// `state.textures` in that same frame would free textures egui's paint jobs
    /// still reference, and `queue.submit` would panic on a destroyed texture.
    /// Instead we defer the clear to the top of the next frame, before anything
    /// paints, so the grid/filmstrip re-upload fresh textures on the frame after.
    pending_texture_clear: bool,
    /// Per-frame diagnostics state (env-gated via `FERROLITE_DIAG`); see `diag.rs`.
    diag: crate::diag::DiagState,
    /// Set by `mark_settings_dirty()` whenever `state.settings` is mutated;
    /// cleared by `save_settings_if_dirty()`, which coalesces any number of
    /// per-frame edits into a single off-thread write per frame.
    settings_dirty: bool,
    /// One-shot restore-session guard: set `true` on the first `update()` frame,
    /// whether or not a restore actually happened, so the check runs exactly once.
    did_restore: bool,
    /// Whether the Help modal (`crate::help::show`) is open. Opened by
    /// `Action::OpenHelp` (F1, global) or the Help menu.
    pub(crate) show_help: bool,
    /// Whether the Settings window (`crate::settings::ui::show`) is open.
    /// Opened by `Action::OpenSettings` (Ctrl+, global) or the File menu.
    pub(crate) show_settings: bool,
    /// One-shot guard: set `true` the first frame that has a valid render state
    /// (pipelines pre-warmed), after kicking off the initial display-profile
    /// detect. Ensures the startup detect fires exactly once.
    did_display_detect: bool,
    /// The Develop tool/tab registry (design §4): base adjustment tabs + the
    /// ordered canvas tools shown in the palette. Built once here; read in
    /// Tasks 10-11 to render the palette/tab bar/canvas overlay.
    pub(crate) tool_registry: crate::develop::tool::DevelopToolRegistry,
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
            // Build-once guard (CLAUDE.md GPU rule): every render pipeline used by
            // the viewer is compiled exactly once, here, inside `DisplayPipelines::new`.
            // No other call site may construct a pipeline — image open/navigation must
            // only ever borrow from this pre-warmed holder.
            debug_assert_eq!(
                pipelines.pipelines_built(),
                5,
                "all display pipelines must be built once at pre-warm (build-once, CLAUDE.md GPU rule)"
            );
            let histogram = ferrolite_vt::HistogramPipeline::new(&gpu);
            rs.renderer
                .write()
                .callback_resources
                .insert(viewer::ViewerPipelines {
                    pipelines,
                    histogram,
                });
            // Pre-warm the edit-pass shaders too, on the same device, so the
            // first image open reuses cached modules instead of compiling
            // ~8 compute shaders synchronously on the UI thread.
            ferrolite_pipeline::prewarm_shaders(&gpu);
            // `prewarm_shaders` only compiles shader MODULES; the driver compiles
            // a pipeline on its first DISPATCH. Build + evaluate tiny dummy edit
            // pipelines now so that first-use compile cost lands at startup, not
            // on the first image open (~2.4s cold-open spike).
            ferrolite_pipeline::prewarm_pipelines(std::sync::Arc::new(
                ferrolite_gpu::GpuContext::from_render_state(rs),
            ));
        }
        let state = crate::state::AppState::new().expect("open catalog");
        let thumb_size = state.settings.grid_size;
        Self {
            module: Module::default(),
            thumb_size,
            state,
            crop_active_prev: false,
            pending_texture_clear: false,
            diag: crate::diag::DiagState::new(),
            settings_dirty: false,
            did_restore: false,
            show_help: false,
            show_settings: false,
            did_display_detect: false,
            tool_registry: crate::develop::tool::DevelopToolRegistry::standard(),
        }
    }

    /// Mark `state.settings` as changed so `save_settings_if_dirty()` writes
    /// it off the UI thread at the end of this frame's `update()`. Every
    /// settings mutation site must call this (see `settings::keymap::Keymap`
    /// doc comment).
    pub(crate) fn mark_settings_dirty(&mut self) {
        self.settings_dirty = true;
    }

    /// Coalesced end-of-frame save: if `settings_dirty`, persist `state.settings`
    /// off the UI thread (`crate::settings::persist::save`) and clear the flag.
    /// Called once per `update()`.
    fn save_settings_if_dirty(&mut self) {
        if self.settings_dirty {
            crate::settings::persist::save(&self.state.jobs, &self.state.settings);
            self.settings_dirty = false;
        }
    }

    /// Step the open viewer's edit history one step (undo when `undo`, else
    /// redo), re-render the preview/full-res, mark the image dirty, and
    /// persist the resulting op stack. Shared by the `Ctrl+Z`/`Ctrl+Shift+Z`/
    /// `Ctrl+Y` keyboard path and the Edit menu's Undo/Redo items so both
    /// route through the exact same logic.
    pub(crate) fn apply_undo_redo(&mut self, ctx: &egui::Context, frame: &eframe::Frame, undo: bool) {
        let result = self.state.viewer.as_mut().and_then(|v| {
            if undo {
                v.history.undo()
            } else {
                v.history.redo()
            }
        });
        if let Some(stack) = result {
            crate::app::controller::AppController::set_preview_and_full(self, frame, stack.clone());
            if let Some(v) = self.state.viewer.as_mut() {
                v.edits_dirty = true;
                // A stale in-progress gesture or cached overlay must not carry over
                // onto the newly-restored stack: drop any in-flight brush/handle
                // gesture and force the overlay to rebuild against the new stack.
                v.mask.gesture = None;
                v.mask.overlay_key = None;
                // A restored stack may have removed/reordered components out from
                // under an open components modal (or the component being edited in
                // it): drop both so the modal never shows/edits stale indices.
                v.mask.components_modal_open = false;
                v.mask.editing_component = None;
                v.mask.preview_component = None;
                v.mask
                    .clamp_selection(crate::develop::mask_edit::layers(&stack).layers.len());
            }
            // Persist the resulting stack (undo/redo changes the on-disk state).
            // Gather viewer scalars into locals before the iter_mut borrow.
            if let Some(v) = self.state.viewer.as_ref() {
                let (image_id, path) = (v.image_id, v.path.clone());
                if let Some(rec) = self.state.images.iter_mut().find(|r| r.id == image_id) {
                    rec.has_edits = !stack.is_identity();
                }
                self.persist_ops(ctx, image_id, path, stack);
            }
        }
    }

    /// Move the open Develop viewer to the previous/next image in the current
    /// image set, non-cyclic. Shared by the ←/→ keyboard path and the Photo
    /// menu's Previous/Next image items.
    pub(crate) fn navigate_step(
        &mut self,
        ctx: &egui::Context,
        frame: &mut eframe::Frame,
        dir: crate::viewer::nav::Step,
    ) {
        let cur_id = self.state.viewer.as_ref().map(|v| v.image_id);
        if let Some(cur_id) = cur_id {
            let ids: Vec<i64> = self.state.images.iter().map(|r| r.id).collect();
            if let Some(next_id) = crate::viewer::nav::neighbor_in_set(&ids, cur_id, dir) {
                if let Some(rec) = self.state.images.iter().find(|r| r.id == next_id).cloned() {
                    self.open_record(ctx, frame, &rec);
                }
            }
        }
    }

    /// Toggle the open viewer's before/after SPLIT-compare (draggable
    /// divider), mirroring the `develop_filter_bar` toggle button's click
    /// handling exactly: flips `split_compare` and, only when turning it on,
    /// resets `split_pos` to center. Shared by the `Y` keyboard shortcut, the
    /// View menu's "Before/After split" item, and the filter-bar toggle button.
    ///
    /// The split only renders on the preview tier (`drive_viewer`'s
    /// `split_active = v.split_compare && !show_full`) — once the sparse
    /// "full" tile tier has taken over the toggle would otherwise be a dead
    /// click. So on an off→on transition while the full tier is actually
    /// showing on screen right now (`v.showing_full`, the real per-frame
    /// `show_full` persisted by `drive_viewer` — NOT merely `full_ready`,
    /// which stays true while tiles are still streaming in after a pan/zoom),
    /// force the view back to fit so the preview tier (and thus the divider)
    /// is immediately visible again.
    pub(crate) fn toggle_split_compare(&mut self) {
        if let Some(v) = self.state.viewer.as_mut() {
            let turning_on = !v.split_compare;
            v.split_compare = !v.split_compare;
            if v.split_compare {
                v.split_pos = 0.5;
                if turning_on && v.showing_full {
                    if let Some(dims) = v.image_dims {
                        v.view = ferrolite_vt::ViewTransform::fit(dims, v.viewport);
                        v.idle = false; // resume the drive loop so the fit takes effect
                    }
                }
            }
        }
    }
}

impl FerroliteApp {
    /// Build the rung-1 preview `VirtualTexture` from the retained sRGB
    /// `preview_source` via one `sRGB→working` color pass, install the holder,
    /// fit the view, and mark the viewer `loaded` + `idle`. Shared by the
    /// Standard preview reveal (`apply_preview_ready`) and the RAW
    /// full-decode-failure fallback (`FullFailed`). Returns `true` on success,
    /// `false` if a prerequisite (GPU / viewer / source) is missing.
    pub(crate) fn reveal_srgb_preview(&mut self, frame: &eframe::Frame, image_id: i64) -> bool {
        let pw = self.preview_to_working();
        let Some(rs) = frame.wgpu_render_state() else {
            return false; // no wgpu backend (should not happen in this build)
        };
        let src = match self.state.viewer.as_ref() {
            Some(v) if v.image_id == image_id => match v.preview_source.clone() {
                Some(src) => src,
                None => return false,
            },
            _ => return false,
        };

        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
        let dims = (src.width, src.height);
        // Initial preview: ONE sRGB→working color pass (not a full 9-node
        // pipeline). Display its working-space output directly.
        let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let converted = ferrolite_pipeline::color_convert(ctx_arc, &src, pw);
        let vt = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            // A Standard image never reaches apply_full_decoded, so set the tail
            // here — routed through the current display state so an active monitor
            // LUT stays applied instead of reverting to analytic sRGB on open.
            crate::app::controller::AppController::apply_display_tail(self, &gpu, vp);
            ferrolite_vt::VirtualTexture::single_from_texture(
                &gpu,
                converted.texture.clone(),
                (converted.width, converted.height),
                &vp.pipelines,
            )
        };

        let Some(v) = self.state.viewer.as_mut() else {
            return false;
        };
        if v.image_id != image_id {
            return false;
        }
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
        // This tier has no tier-2 to wait for (Standard preview IS full-res, and
        // the RAW fallback has given up on the full) — go idle so the repaint
        // loop does not spin.
        v.idle = true;

        // Placeholder (1,1) present-buffer size: `drive_viewer`'s per-frame
        // resize corrects it to the canvas's physical viewport before paint.
        let present = ferrolite_vt::PresentBuffers::new(&gpu, (1, 1), rs.target_format);
        let present_alpha = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-present-alpha"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        rs.renderer
            .write()
            .callback_resources
            .insert(viewer::ViewerGpu {
                ctx: gpu,
                preview: vt,
                full: None,
                preview_before: None,
                image_id,
                present,
                present_alpha,
                blit_bind_front: None,
            });
        self.mark_histogram_dirty();
        true
    }

    /// Flag the histogram stale so the next frame recomputes it (debounced).
    pub(crate) fn mark_histogram_dirty(&mut self) {
        if let Some(v) = self.state.viewer.as_mut() {
            v.histogram.mark_dirty();
        }
    }

    /// Debounced GPU histogram recompute over the on-screen preview texture.
    /// While a readback is in flight, poll the device (non-blocking) so the
    /// `map_async` callback fires and keep repainting until it delivers. Never
    /// blocks the UI thread and never reads back the image (only the 4 KB bins).
    fn maybe_update_histogram(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let dt = ctx.input(|i| i.stable_dt);
        let (inflight, do_dispatch, image_id) = {
            let Some(v) = self.state.viewer.as_mut() else {
                return;
            };
            v.histogram.tick(dt);
            (
                v.histogram.inflight,
                v.histogram.should_dispatch(),
                v.image_id,
            )
        };

        // A readback is pending: drive the map callback + keep the frame loop alive.
        if inflight {
            let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
            gpu.device.poll(wgpu::Maintain::Poll);
            ctx.request_repaint();
            return;
        }
        if !do_dispatch {
            return;
        }

        let matrix = ferrolite_color::working_to_display(self.state.working_space);
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
        let dispatched = {
            let renderer = rs.renderer.read();
            let Some(g) = renderer.callback_resources.get::<viewer::ViewerGpu>() else {
                return;
            };
            if g.image_id != image_id {
                return;
            }
            let (Some(tex), Some(dims)) = (g.preview.single_texture_arc(), g.preview.single_dims())
            else {
                return;
            };
            let Some(vp) = renderer.callback_resources.get::<viewer::ViewerPipelines>() else {
                return;
            };
            vp.histogram.dispatch(&gpu, &tex, dims, matrix);
            let tx = self.state.tx.clone();
            let egui_ctx = ctx.clone();
            vp.histogram.read_async(move |maybe| {
                let bins = maybe.unwrap_or_default();
                let _ = tx.send(crate::events::AppEvent::HistogramReady { image_id, bins });
                egui_ctx.request_repaint();
            });
            true
        };
        if dispatched {
            if let Some(v) = self.state.viewer.as_mut() {
                v.histogram.inflight = true;
                v.histogram.dirty = false;
            }
            // Poll now so the just-submitted work can complete promptly.
            gpu.device.poll(wgpu::Maintain::Poll);
            ctx.request_repaint();
        }
    }

    /// Compose a source→working 3×3 for `profile` under the current working space.
    fn source_to_working(&self, profile: &ferrolite_decode::ColorProfile) -> [[f32; 3]; 3] {
        ferrolite_color::camera_to_working(
            profile.xyz_to_cam,
            ferrolite_color::Xy {
                x: profile.white_xy[0],
                y: profile.white_xy[1],
            },
            self.state.working_space,
        )
    }

    /// Normalized WhiteBalance temperature of the open viewer's current op stack
    /// (0.0 = as-shot/identity when there is no WB op or no viewer).
    pub(crate) fn current_wb_temp(&self) -> f32 {
        self.state
            .viewer
            .as_ref()
            .and_then(|v| v.op_stack.white_balance())
            .map(|w| w.temp)
            .unwrap_or(0.0)
    }

    /// camera→working for the open viewer's RAW profile at the given normalized WB
    /// `temp` (full-res tier). Dual-illuminant profiles re-interpolate with `temp`
    /// (P2 Plan 2 / S3); single-illuminant reduce to the static matrix. Already
    /// row-normalized by `wb_camera_to_working` (the demosaic applied as-shot
    /// gains). The sRGB preview tier is NOT normalized — see `preview_to_working`.
    pub(crate) fn camera_to_working(&self, temp: f32) -> [[f32; 3]; 3] {
        match self.state.viewer.as_ref() {
            Some(v) => crate::camera_matrix::wb_camera_to_working(
                &v.color_profile,
                temp,
                self.state.working_space,
            ),
            None => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    /// True while any modal overlay is on screen (Help, Settings, the
    /// remove-folder confirmation). Used to suppress the app's global keyboard
    /// shortcuts underneath the modal so its own input handling (e.g. Esc) is
    /// the only thing that reacts, and so shortcuts like Enter/Ctrl+A don't
    /// leak through to the grid/viewer while a modal is up. Extend this with
    /// new modals as they're added.
    ///
    /// The mask Components window is intentionally NOT included here: unlike
    /// the modals above, it must stay non-blocking so the canvas keeps
    /// receiving input behind it (live preview, color-eyedropper sampling,
    /// brush drawing all route through the canvas while the window is open).
    fn modal_active(&self) -> bool {
        self.show_help || self.show_settings || self.state.pending_remove.is_some()
    }

    /// If the current viewer's edit stack changed this session, spawn a
    /// Background job to regenerate its Library thumbnail from the in-memory
    /// stack, then clear the flag so re-entrant frames do not double-spawn.
    /// Called at every "leave Develop for this image" transition. No-op when
    /// there is no viewer, no session edits, or no GPU render state.
    pub(crate) fn maybe_regen_on_leave(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let (image_id, path, kind, stack) = {
            let Some(v) = self.state.viewer.as_mut() else {
                return;
            };
            if !crate::develop::thumb_regen::should_regenerate_on_leave(v.edits_dirty) {
                return;
            }
            // Clear before spawning so an edge-triggered re-check this frame
            // (e.g. module switch) cannot enqueue a duplicate job.
            v.edits_dirty = false;
            (v.image_id, v.path.clone(), v.kind, v.op_stack.clone())
        };
        let Some(rs) = frame.wgpu_render_state() else {
            // No GPU this frame: keep the existing thumbnail. An on-demand
            // "Regenerate thumbnail" can recover it later.
            return;
        };
        let gpu = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let cam =
            crate::develop::thumb_regen::srgb_fallback_camera_to_working(self.state.working_space);
        crate::develop::thumb_regen::spawn_regen_edited_thumbnail(
            &self.state.jobs,
            &self.state.writer,
            &self.state.tx,
            ctx,
            gpu,
            self.state.lens_db.clone(),
            image_id,
            path,
            kind,
            cam,
            crate::develop::thumb_regen::RegenStackSource::InMemory(stack),
        );
    }

    /// Open the single-file export dialog for the current viewer image, seeded
    /// once from `settings.export` (the same slot the Export module panel
    /// writes to via `state.export_settings`) at the moment it opens. The
    /// dialog is a plain floating `egui::Window` — NOT modal — so the
    /// titlebar/module tabs stay reachable while it's open and the user could
    /// switch to the Export module panel and change `state.export_settings`
    /// before coming back. If that happens, `confirm_export` persists
    /// whatever the user leaves in the dialog: benign last-writer-wins on a
    /// preference value, not a data-loss risk.
    fn open_export_dialog(&mut self) {
        if self.state.viewer.is_some() {
            self.state.export_dialog = Some(crate::export::ExportDialogState {
                options: self.state.settings.export.to_options(),
            });
        }
    }

    /// The user confirmed the export dialog: pick a destination and spawn the job.
    fn confirm_export(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(dialog) = self.state.export_dialog.take() else {
            return;
        };
        let options = dialog.options;
        // Guard the write: persist `dialog.options` (whatever the user left
        // it as) as the new `settings.export`, but only mark dirty when it
        // actually differs from what's persisted, so this can't become an
        // unconditional per-frame write. If the Export module panel changed
        // `state.export_settings` while this dialog was open, this is a
        // last-writer-wins overwrite of that — benign, since both are just a
        // preference value with no data-loss risk.
        if self.state.settings.export.to_options() != options {
            self.state.settings.export =
                crate::settings::dto::PersistedExport::from_options(&options);
            self.mark_settings_dirty();
        }

        // Compute camera→working BEFORE any borrow of `self.state.viewer` is held,
        // since `camera_to_working()` itself immutably borrows `self`.
        let camera_to_working = self.camera_to_working(self.current_wb_temp());

        let Some(v) = self.state.viewer.as_ref() else {
            return;
        };
        // Pick the full-res source: RAW uses its tier-2 GPU pyramid; a Standard
        // image (never tier-2 decoded) exports from its full-res tier-1 preview,
        // whose pyramid is built inside the Background job.
        let source = if let Some(p) = v.pyramid.clone() {
            crate::export::ExportSource::Pyramid(p)
        } else if v.kind != ferrolite_image::FileKind::Raw {
            match v.preview_source.clone() {
                Some(src) => crate::export::ExportSource::FullResCpu(src),
                None => {
                    self.state.notify(
                        crate::notifications::Level::Warning,
                        "Image still loading; cannot export yet.",
                    );
                    return;
                }
            }
        } else {
            self.state.notify(
                crate::notifications::Level::Warning,
                "Image still loading; cannot export yet.",
            );
            return;
        };
        let source_path = v.path.clone();
        let image_id = v.image_id;
        let stack = v.op_stack.clone();

        // Default filename: source basename + new extension.
        let stem = source_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "export".to_string());
        let ext = options.format.extension();
        let default_name = format!("{stem}.{ext}");

        let Some(dest) = rfd::FileDialog::new()
            .set_file_name(default_name)
            .add_filter(options.format.label(), &[ext])
            .save_file()
        else {
            return; // user cancelled the save dialog
        };

        // Build the shared GpuContext from eframe's render state.
        let Some(rs) = frame.wgpu_render_state() else {
            self.state.notify(
                crate::notifications::Level::Warning,
                "No GPU render state; cannot export.",
            );
            return;
        };
        let gpu = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));

        let working_space = self.state.working_space;

        let current_name = dest.file_name().map(|s| s.to_string_lossy().to_string());
        let handle = crate::export::spawn_export(
            &self.state,
            ctx,
            gpu,
            source,
            stack,
            camera_to_working,
            working_space,
            options,
            source_path,
            dest,
            image_id,
        );
        let mut activity = crate::export::ExportActivity::new_single(current_name);
        activity.handles = vec![handle];
        self.state.export_activity = Some(activity);
    }

    /// Resolve output filenames and spawn one Background export job per queued
    /// image (spec §8.4). Filenames are expanded + collision-resolved up front on
    /// the UI thread so {seq} is deterministic and disk collisions are avoided.
    fn start_batch(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(dest_dir) = self.state.export_dest.clone() else {
            self.state.notify(
                crate::notifications::Level::Warning,
                "Choose a destination folder first.",
            );
            return;
        };
        let ids = self.state.export_queue.clone();
        if ids.is_empty() {
            return;
        }
        // Metadata for {name}/{date}.
        let recs = self.state.reads.images_by_ids(&ids).unwrap_or_default();
        let options = self.state.export_settings;
        let template = self.state.export_template.clone();
        let ext = options.format.extension();

        // Seed collision set with files already on disk in the destination.
        let mut taken: std::collections::HashSet<String> = std::fs::read_dir(&dest_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let mut items: Vec<crate::export::batch::BatchItem> = Vec::new();
        let mut seq = 0usize;
        let mut skipped = 0usize;
        for &id in &ids {
            let Some(rec) = recs.iter().find(|r| r.id == id) else {
                skipped += 1;
                continue;
            };
            // Skip images whose folder can't be resolved (moved/deleted on disk);
            // the batch proceeds with the remaining queued images.
            let Some(path) = self.state.image_path(rec) else {
                skipped += 1;
                continue;
            };
            seq += 1;
            let stem = std::path::Path::new(&rec.filename)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| rec.filename.clone());
            let fctx = ferrolite_export::FilenameCtx {
                name: stem,
                seq,
                date: ferrolite_export::format_capture_date(rec.capture_time.as_deref()),
            };
            let expanded = ferrolite_export::expand_filename(&template, &fctx);
            let safe = ferrolite_export::sanitize_component(&expanded);
            let filename = ferrolite_export::resolve_collision(&safe, ext, &mut taken);
            items.push(crate::export::batch::BatchItem {
                image_id: id,
                path,
                kind: rec.kind,
                dest: dest_dir.join(&filename),
            });
        }

        if items.is_empty() {
            self.state.notify(
                crate::notifications::Level::Info,
                if skipped > 0 {
                    format!("No images could be resolved for export ({skipped} skipped).")
                } else {
                    "No queued images could be resolved to a file on disk.".to_string()
                },
            );
            return;
        }

        let Some(rs) = frame.wgpu_render_state() else {
            self.state.notify(
                crate::notifications::Level::Warning,
                "No GPU render state; cannot export.",
            );
            return;
        };
        let gpu = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let working_space = self.state.working_space;

        // Item count is the number of images (NOT the job-handle count — the
        // batch is a single sequential job, so it returns one handle).
        let total = items.len();
        let handles =
            crate::export::batch::spawn_batch(&self.state, ctx, gpu, items, working_space, options);
        let mut activity = crate::export::ExportActivity::new_batch(total);
        activity.handles = handles;
        self.state.export_activity = Some(activity);
        self.state.notify(
            crate::notifications::Level::Info,
            if skipped > 0 {
                format!("Exporting {total} image(s)… (skipped {skipped} with unresolved paths)")
            } else {
                format!("Exporting {total} image(s)…")
            },
        );
    }

    /// sRGB→working for the preview tier: the embedded preview and Standard images
    /// are sRGB-primaries, so they convert via the sRGB fallback profile.
    pub(crate) fn preview_to_working(&self) -> [[f32; 3]; 3] {
        self.source_to_working(&ferrolite_decode::ColorProfile::srgb_fallback())
    }

    /// Ensure `ViewerGpu.preview_before` holds the unedited (identity stack)
    /// rung-1 preview while split-compare is active. For Standard it is built from
    /// the retained sRGB `preview_source` via one `color_convert` pass; for RAW it
    /// is built from the demosaic `raw_preview_source` through an identity op stack
    /// with the camera→working matrix (`cam`) — the SAME color path as the RAW
    /// after-view, so the split compares like-with-like (no color/tone shift).
    /// Rebuilt only when missing (invalidated on WS change / image open), so edits
    /// do not recompute it — the before never changes.
    fn ensure_before_view(&mut self, frame: &eframe::Frame) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let (active, image_id, is_raw, srgb_src, raw_src) = match self.state.viewer.as_ref() {
            Some(v) => (
                v.split_compare,
                v.image_id,
                v.kind == ferrolite_image::FileKind::Raw,
                v.preview_source.clone(),
                v.raw_preview_source.clone(),
            ),
            None => return,
        };
        if !active {
            return;
        }
        // Already built for this image? Nothing to do.
        {
            let renderer = rs.renderer.read();
            if let Some(g) = renderer.callback_resources.get::<viewer::ViewerGpu>() {
                if g.image_id == image_id && g.preview_before.is_some() {
                    return;
                }
            }
        }
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
        // Compute the unedited "before" texture on the same color path as the
        // after-view: RAW via the raw pipeline (demosaic + identity + `cam`),
        // Standard via the sRGB `color_convert`.
        let (tex, dims) = if is_raw {
            // `cam` borrows the viewer immutably; compute before the write below.
            let cam = self.camera_to_working(self.current_wb_temp());
            let Some(src) = raw_src else {
                return; // RAW before-view not available until the full decode.
            };
            let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
            let mut ep = ferrolite_pipeline::EditPipeline::new(
                ctx_arc,
                &src,
                ferrolite_pipeline::OpStack::default(),
                cam,
            );
            let out = ep.evaluate();
            (out.texture.clone(), (out.width, out.height))
        } else {
            let pw = self.preview_to_working();
            let Some(src) = srgb_src else {
                return;
            };
            let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
            let out = ferrolite_pipeline::color_convert(ctx_arc, &src, pw);
            (out.texture.clone(), (out.width, out.height))
        };
        let vt = {
            let renderer = rs.renderer.read();
            let Some(vp) = renderer.callback_resources.get::<viewer::ViewerPipelines>() else {
                return;
            };
            ferrolite_vt::VirtualTexture::single_from_texture(&gpu, tex, dims, &vp.pipelines)
        };
        let mut renderer = rs.renderer.write();
        if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
            if g.image_id == image_id {
                g.preview_before = Some(vt);
            }
        }
    }
}


/// Physical tile-pool budget for the viewer's sparse VT. 256 tiles × 256² ×
/// RGBA16F ≈ 128 MB of GPU memory — generous headroom for a fit-to-window view
/// plus a few zoom levels of the quad-binned (half-res) full image.
pub(crate) const VIEWER_TILE_BUDGET: u32 = 256;

/// Max edited tiles rendered per frame on the render thread (bounds GPU work
/// per CLAUDE.md's GPU-frame-budget rule: pipelines run on the render thread
/// but bounded, never unbounded per-frame work). Remaining needed tiles are
/// produced on subsequent frames.
///
/// Task 15: raised from 8 to 32. Production here only feeds the OFF-SCREEN
/// sparse pool that `drive_viewer` composes+swaps once converged (see
/// `compose_sparse_into`) — none of it is presented mid-burst — so a larger
/// per-frame burst is invisible to the user and just reaches convergence (and
/// the visible swap) sooner. The value stays a bounded named const rather than
/// unbounded so a single frame's production cannot blow the frame budget;
/// the author profiles this bound in the next phase (Task 17) and will tune
/// it further if 32 proves too expensive on slower GPUs.
const MAX_PRODUCE_PER_FRAME: usize = 32;

/// Spec 4.5 §4.2: prefetch ring for the sparse producer's needed set — the ring
/// of tiles around the visible rect (plus the coarse base) produced ahead of a
/// pan/zoom so the off-screen compose has the neighbours ready and convergence
/// includes them. A one-tile ring keeps the extra production bounded.
const PREFETCH_RING: u32 = 1;

/// Max thumbnail texture uploads per frame (bounds per-frame GPU/texture work
/// during bulk thumbnail delivery; CLAUDE.md responsiveness rule). Overflow is
/// stashed in `AppState.pending_uploads` and flushed over subsequent frames.
pub(crate) const MAX_THUMB_UPLOADS_PER_FRAME: usize = 16;

/// Debounce (seconds) before the tier-2 full-RAW decode is submitted after a
/// viewer opens. The tier-1 preview shows immediately regardless; the full
/// decode is only needed for the 1:1 crossfade, so delaying it lets fast
/// arrow-navigation cancel each superseded viewer's full decode WHILE IT IS
/// STILL QUEUED (or never submit it at all), instead of piling up one
/// `Visible`-priority full decode per image flipped through.
const FULL_DECODE_DEBOUNCE: f32 = 0.05;

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
        // Producer-drive convergence signals (Plan 3): CPU load jobs stay at 0 in
        // producer mode, so the sparse VT's producer progress is tracked here to
        // decide when the shown full view is fully rendered.
        let mut produce_pending: Option<usize> = None;
        let mut needed_established = false;
        let mut produced_this_frame = 0usize;
        // Spec 4.5 §4.2: whether the sparse pool is fully resident for the current
        // transform+version this frame (CPU-rect predicate). Drives the off-screen
        // compose+swap below and the `present_source` selection for `paint`.
        let mut converged = false;
        // Set true on the frame the compose+swap actually ran, so the caller can
        // (re)start the crossfade ramp exactly once per convergence.
        let mut swapped_this_frame = false;
        // Set true when `g.present.resize` actually reallocated (canvas size
        // changed), meaning `front`/`back` are now blank. `converged` frequently
        // stays true across a resize (same zoom -> same tiles resident), so the
        // compose+swap guard below would otherwise never re-fire and the canvas
        // would show blank/clear-color until the next pan/zoom/edit. Re-armed
        // below alongside the `!converged` re-arm.
        let mut present_reallocated = false;
        if let (Some(rs), Some(v)) = (frame.wgpu_render_state(), self.state.viewer.as_ref()) {
            // The view/viewport for feedback, prefetch, convergence, and the
            // off-screen compose. One-frame-latent (recorded by the PRIOR frame's
            // `viewer::paint`), matching `request_view_feedback`'s existing latency.
            let cur_view = v.view;
            let cur_viewport = v.viewport;
            // The `(opstack_version, view)` the compose+swap keys on, captured
            // (Copy) before the `&mut ViewerGpu` borrow. `front` is composed for
            // the CURRENT state iff `cur_present_key == Some((cur_version, cur_view))`.
            let cur_version = v.opstack_version;
            let cur_present_key = v.present_key;
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                // Resize the off-screen present buffers to the canvas viewport
                // (converted logical→physical px) every frame; `resize` no-ops
                // when the size is unchanged. `v.viewport` is one-frame-latent
                // here (this runs before `viewer::paint` records this frame's
                // rect below), matching `request_view_feedback`'s existing
                // one-frame latency.
                let ppp = ui.ctx().pixels_per_point();
                let phys = (
                    (v.viewport.0 * ppp).round().max(1.0) as u32,
                    (v.viewport.1 * ppp).round().max(1.0) as u32,
                );
                present_reallocated = g.present.resize(&g.ctx, phys);
                if Some(g.image_id) != open_id {
                    // Stale holder from a superseded viewer: stop its tile jobs.
                    if let Some(full) = g.full.as_mut() {
                        full.cancel_sparse();
                    }
                } else if g.full.is_some() {
                    // Scope the `&mut g.full` alias so it is DROPPED before the
                    // compose+swap below reborrows `g.full` alongside `g.present`
                    // / `g.ctx` (disjoint-field borrows must not overlap an alias).
                    {
                        let full = g.full.as_mut().expect("checked is_some");
                        full.request_view_feedback(&g.ctx);
                        // Plan 3: when an edit producer is present, render the needed
                        // tiles on the render thread (bounded). `produce_view` borrows
                        // the producer (which lives in ViewerState) by &mut per call.
                        // Spec 4.5 §4.2: drive production from the PREFETCHED CPU-rect
                        // set (visible + ring + coarse base) so convergence includes
                        // the neighbours a pan/zoom will need, and the visible tiles
                        // converge first.
                        if let Some(v) = self.state.viewer.as_mut() {
                            if let Some(producer) = v.edit_producer.as_mut() {
                                let needed =
                                    full.needed_prefetched(&cur_view, cur_viewport, PREFETCH_RING);
                                produced_this_frame = full.produce_view(
                                    &g.ctx,
                                    producer,
                                    &needed,
                                    MAX_PRODUCE_PER_FRAME,
                                );
                            }
                        }
                        tiles_pending = full.sparse_pending();
                        produce_pending = full.produce_pending();
                        needed_established = full.needed_established();
                        // CPU-rect convergence for the current transform+version.
                        converged = full.is_converged(&cur_view, cur_viewport);
                    }

                    // Compose+swap when the pool is converged AND `front` is stale
                    // or missing for the current state — i.e. its key does not match
                    // `(cur_version, cur_view)`. An edit bumps `opstack_version` and
                    // a pan/zoom changes `view`, so the key mismatches immediately;
                    // once composed for this state the key matches and the swap does
                    // not re-fire (at most ONCE per (version, view)). This is the
                    // keying that fixes edits/split not showing until a zoom nudge.
                    // The pool is converged so this is one bounded render pass.
                    // Disjoint-field borrows: `g.full`, `g.ctx`, and `g.present` are
                    // three distinct fields borrowed in the same expression.
                    if converged && cur_present_key != Some((cur_version, cur_view)) {
                        let mut enc =
                            g.ctx
                                .device
                                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                    label: Some("viewer-present-compose"),
                                });
                        if let Some(full) = g.full.as_mut() {
                            full.compose_sparse_into(
                                &g.ctx,
                                &mut enc,
                                g.present.back_view(),
                                &cur_view,
                                cur_viewport,
                            );
                        }
                        g.ctx.queue.submit([enc.finish()]);
                        g.present.swap();
                        swapped_this_frame = true;
                    }
                }
            }
        }

        let Some(v) = self.state.viewer.as_mut() else {
            return;
        };

        // Task 17 (spec 4.5 §9): dev-mode viewer frame-time profiling hook.
        // Entirely behind `diag::enabled()` — the branch not taken means zero
        // added cost (no float math, no atomic store) on the hot pan/zoom path
        // when diagnostics are off, matching every other recorder in `diag.rs`.
        // Records this frame's `stable_dt` (ms) and the sparse producer's
        // tiles-produced-this-frame count so the author can measure the
        // ≤16.6 ms/frame budget on the dev GPU via the existing diag log/overlay
        // (see `diag::format_viewer_line`); no new UI.
        if crate::diag::enabled() {
            crate::diag::record_viewer_frame(dt, produced_this_frame);
        }

        // If the view changed (pan/zoom in `viewer::paint` already cleared `idle`,
        // but a programmatic change might not), `request_view_feedback` above may
        // have submitted new tile loads. Resume the drive loop so they drain + display.
        if matches!(tiles_pending, Some(n) if n > 0) {
            v.idle = false;
        }

        // Spec 4.5 §4.2: key the composed `front` on `(opstack_version, view)`.
        // A canvas resize reallocates (blanks) the present buffers, so invalidate
        // the key — `front` no longer holds anything valid until recomposed. On the
        // frame the compose+swap ran, record the key it was composed at and (re)start
        // the crossfade ramp so the freshly-composed `front` fades in over the preview.
        // Recompute the current key here (post-block) since `opstack_version`/`view`
        // are stable across this function and were captured above as Copy locals.
        let cur_version = v.opstack_version;
        let cur_view = v.view;
        if present_reallocated {
            v.present_key = None;
        }
        if swapped_this_frame {
            v.present_key = Some((cur_version, cur_view));
            v.begin_crossfade();
        }

        // Advance the crossfade ramp. `factor` in [0,1] rides the swap: 0 right
        // after a swap, 1 once the ramp completes (or immediately once idle+ready).
        let factor = v.tick_crossfade(dt);
        let tiles_settled = matches!(tiles_pending, Some(0));
        // `front` holds a valid composed image for the CURRENT `(version, view)`
        // iff the recorded key matches. False during edits (version bumped), motion
        // (view changed each frame), and right after a resize (key set to `None`) —
        // in all of which `present_source` must fall back to `Preview`.
        let front_valid = v.present_key == Some((cur_version, cur_view));
        // The full (sparse) tier is actually on screen once `front` is valid for
        // the current state, the crossfade has completed, its tiles are all
        // resident, AND the before/after split is NOT active (the split is a
        // preview-tier-only compare — never claim the full tier is "shown" while
        // it renders, which is the split fix). Consulted by `toggle_split_compare`
        // (via `showing_full`) to decide whether enabling the split dead-ends.
        let show_full =
            v.full_ready && front_valid && factor >= 1.0 && tiles_settled && !v.split_compare;
        // Present-source inputs handed to `viewer::paint` (which also folds in the
        // per-frame `interacting` and `split_compare` it reads): the sparse tier
        // exists, `front` is valid for the current `(version, view)`, and the
        // crossfade factor.
        let full_ready = v.full_ready;
        // Persist the real, per-frame-current value so `toggle_split_compare`
        // (which runs outside this per-frame borrow, e.g. from a keyboard
        // shortcut or menu click) can consult an accurate "is the full tier
        // actually on screen right now" signal instead of a `full_ready`-only
        // proxy that stays true while tiles are still streaming in.
        v.showing_full = show_full;

        // Producer convergence: the shown full view is fully rendered only once
        // the GPU-truth needed set has been established (the sparse shader painted
        // + its feedback read back) AND every needed tile is produced at the
        // current version AND nothing was produced this frame. Because feedback is
        // one frame latent and production is bounded per frame, this takes several
        // frames after `show_full` first flips true.
        let full_converged = needed_established
            && matches!(produce_pending, Some(0) | None)
            && produced_this_frame == 0;

        // Terminal state: full shown, crossfade done, AND the producer has
        // converged. Gating idle on `full_converged` (not merely `show_full`)
        // keeps the drive loop alive across the feedback→produce frames so tiles
        // stream in without a manual pan/zoom.
        if show_full && !v.crossfading && full_converged {
            v.idle = true;
        }

        let crossfading = v.crossfading;
        // While the crop tool is active, the crop overlay is the sole input
        // target: gate the canvas pan/zoom interaction off so it doesn't compete.
        // While the before/after SPLIT is shown on the preview tier, the divider
        // strip (drawn below) owns pointer input instead, so gate pan/zoom off
        // then too (at 1:1 the split is suppressed and pan/zoom resumes).
        let interactive = !v.crop_active && (show_full || !v.split_compare);

        let canvas_rect = ui.available_rect_before_wrap();
        // Split only renders on the preview tier; once `show_full` takes over it
        // dead-ends here (silently — `toggle_split_compare` now forces a fit on
        // enable, so this state is reached only via zooming/navigating in while
        // already split, not via the toggle itself).
        let split_active = v.split_compare && !show_full;
        let (image_id, view, viewport, split_pos) = (v.image_id, v.view, v.viewport, v.split_pos);

        // `paint` applies this frame's pan/zoom and clears `idle` when the view
        // moved, so read `idle` AFTER it to catch an interaction this frame. It
        // also folds this frame's `interacting` into the present source and returns
        // the chosen source so the repaint gate can keep the loop alive mid-fade.
        let (loading_preview, present_source) =
            viewer::paint(ui, v, full_ready, front_valid, factor, interactive);
        let idle = v.idle;
        let crossfading_present = matches!(present_source, viewer::PresentSource::Crossfade(_));

        // Repaint only while there is pending work:
        //  - preview not yet uploaded, or
        //  - crossfade ramp still advancing, or
        //  - sparse tiles still loading.
        // Once `idle` (full ready + settled, or a failure marked it idle) we stop.
        // A pan/zoom clears `idle` so the loop resumes and the new view's tiles
        // (requested next frame) drain and display.
        let tiles_loading = matches!(tiles_pending, Some(n) if n > 0);
        // Keep repainting while the producer is still converging on the shown full
        // view (feedback is one frame latent + production is bounded per frame),
        // so the sparse tiles stream in on open without a manual pan/zoom.
        let full_warming = show_full && !full_converged;
        // Spec 4.5 §4.2: while the sparse tier exists but is not yet converged, keep
        // the loop alive so production advances and the off-screen compose+swap can
        // fire; and keep it alive while a present-crossfade is mid-ramp so the
        // freshly-swapped `front` fades in without a manual nudge. Additionally, keep
        // repainting while `front` is stale for the current state (`!front_valid`) —
        // e.g. an edit at an already-settled fit view bumped `opstack_version` so the
        // key mismatches but `converged` may already be true: without this the loop
        // could idle before the recompose+swap fires and the edit would only appear
        // after a manual zoom. Once `front_valid` (and not crossfading + converged),
        // none of these terms hold, so the loop goes idle (no busy-loop).
        let compose_pending = full_ready && (!converged || !front_valid);
        if !idle
            && (loading_preview
                || crossfading
                || crossfading_present
                || tiles_loading
                || full_warming
                || compose_pending)
        {
            ui.ctx().request_repaint();
        }

        // The `v` borrow has ended; the split render/drag needs `&mut self`
        // (`ensure_before_view` + writing `split_pos` on drag).
        if split_active {
            self.ensure_before_view(frame);
            let div_x = crate::develop::split::divider_x(
                canvas_rect.left(),
                canvas_rect.width(),
                split_pos,
            );
            // Paint the "before" clipped to the left of the divider, on top of
            // the already-painted "after". Same `canvas_rect` for both callbacks
            // keeps the image geometry identical; only the clip rect (scissor)
            // differs, so left = before, right = after.
            let left_clip =
                egui::Rect::from_min_max(canvas_rect.min, egui::pos2(div_x, canvas_rect.max.y));
            ui.painter()
                .with_clip_rect(left_clip)
                .add(egui_wgpu::Callback::new_paint_callback(
                    canvas_rect,
                    viewer::ViewerCallback {
                        image_id,
                        view,
                        viewport,
                        // The `Before` path always draws the preview-tier
                        // `preview_before`; the present source is ignored by that
                        // arm, but pass `Preview` for a well-defined value.
                        present_source: viewer::PresentSource::Preview,
                        which: viewer::PreviewWhich::Before,
                    },
                ));
            // Divider line + a grab handle at mid-height.
            let painter = ui.painter();
            painter.vline(
                div_x,
                canvas_rect.y_range(),
                egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
            );
            let handle_center = egui::pos2(div_x, canvas_rect.center().y);
            painter.circle(
                handle_center,
                7.0,
                egui::Color32::from_black_alpha(120),
                egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
            );
            // Side labels: which half is the unedited original vs. the current
            // edit. Bottom corners keep them clear of the top-right histogram
            // overlay and the mid-height divider handle. Left of the divider is
            // the "before" (original), right is the "after" (edited).
            let label_font = egui::FontId::proportional(12.0);
            let label_pad = egui::vec2(6.0, 3.0);
            let label_margin = 8.0;
            let draw_side_label = |text: &str, right_aligned: bool| {
                let galley = painter.layout_no_wrap(
                    text.to_owned(),
                    label_font.clone(),
                    egui::Color32::WHITE,
                );
                let size = galley.size() + label_pad * 2.0;
                let x = if right_aligned {
                    canvas_rect.right() - label_margin - size.x
                } else {
                    canvas_rect.left() + label_margin
                };
                let min = egui::pos2(x, canvas_rect.bottom() - label_margin - size.y);
                painter.rect_filled(
                    egui::Rect::from_min_size(min, size),
                    3.0,
                    egui::Color32::from_black_alpha(140),
                );
                painter.galley(min + label_pad, galley, egui::Color32::WHITE);
            };
            draw_side_label("Original", false);
            draw_side_label("Edited", true);
            // Drag: a thin full-height strip around the divider owns the pointer.
            let hit = crate::develop::split::HANDLE_TOL;
            let strip = egui::Rect::from_min_max(
                egui::pos2(div_x - hit, canvas_rect.top()),
                egui::pos2(div_x + hit, canvas_rect.bottom()),
            );
            let resp = ui.interact(
                strip,
                ui.id().with(("split-divider", image_id)),
                egui::Sense::click_and_drag(),
            );
            // Precise hover check against the divider itself (not just the strip
            // rect) via the pure hit-test, so the cursor only swaps within the
            // documented `HANDLE_TOL` of the actual divider line.
            let hovering_divider = resp.hover_pos().is_some_and(|pos| {
                crate::develop::split::hit_divider(
                    canvas_rect.left(),
                    canvas_rect.width(),
                    split_pos,
                    pos.x,
                    hit,
                )
            });
            if hovering_divider || resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if resp.dragged() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let new_pos = crate::develop::split::pos_from_pointer(
                        canvas_rect.left(),
                        canvas_rect.width(),
                        pos.x,
                    );
                    if let Some(v) = self.state.viewer.as_mut() {
                        v.split_pos = new_pos;
                    }
                    ui.ctx().request_repaint();
                }
            }
        }
    }

    /// Draw the read-only, GPU-computed histogram as a floating, non-interactive
    /// overlay anchored to the Develop canvas's top-right corner (spec 4.1 §7.1).
    /// Data comes straight from `ViewerState::histogram` (already computed by
    /// `maybe_update_histogram`'s GPU dispatch this frame or an earlier one) — this
    /// is display placement only, no recompute. `Order::Middle` sits above the
    /// canvas paint but below modal `Order::Foreground` windows (Help/Settings),
    /// and `.interactable(false)` means canvas pan/zoom keeps working underneath it.
    fn draw_histogram_overlay(&self, ui: &egui::Ui) {
        const MARGIN: f32 = 12.0;
        const WIDTH: f32 = 220.0;

        let canvas_rect = ui.min_rect();
        let bins = self
            .state
            .viewer
            .as_ref()
            .and_then(|v| v.histogram.bins.as_deref());

        let pos = egui::pos2(
            canvas_rect.right() - WIDTH - MARGIN,
            canvas_rect.top() + MARGIN,
        );

        egui::Area::new(egui::Id::new("develop_histogram_overlay"))
            .order(egui::Order::Middle)
            .fixed_pos(pos)
            .interactable(false)
            .show(ui.ctx(), |ui| {
                ui.set_width(WIDTH);
                egui::Frame::none()
                    .fill(egui::Color32::from_black_alpha(160))
                    .rounding(4.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ui.set_width(WIDTH - 12.0);
                        crate::develop::histogram_widget::show(ui, bins);
                    });
            });
    }

    /// The single image-open path: cancel the previously-open viewer's in-flight
    /// tile jobs, open the new image's two-tier load, switch to Develop, and request
    /// a repaint so the viewer is drawn on the very next frame (otherwise egui would
    /// idle on the grid until the next input event, which reads as a stall).
    pub(crate) fn open_record(
        &mut self,
        ctx: &egui::Context,
        frame: &mut eframe::Frame,
        rec: &ferrolite_catalog::ImageRecord,
    ) {
        let mem_before = crate::diag::enabled().then(|| self.gather_mem_breakdown());
        self.maybe_regen_on_leave(ctx, frame);
        if let Some(old) = self.state.viewer.as_ref() {
            let old_id = old.image_id;
            old.cancel_loads();
            self.cancel_viewer_tiles(frame, old_id);
        }
        self.state.open_image_in_viewer(rec);
        self.module = crate::module::Module::Develop;
        if let Some(before) = mem_before {
            let after = self.gather_mem_breakdown();
            let kind = if rec.kind == ferrolite_image::FileKind::Raw {
                "RAW"
            } else {
                "JPG"
            };
            crate::diag::write_log(&crate::diag_mem::format_mem_event_line(
                &format!("open #{} {}", rec.id, kind),
                &before,
                &after,
            ));
        }
        ctx.request_repaint();
    }

    /// Increment the inflight counter and spawn an ops-persist job. Both call
    /// sites (apply_edit commit branch + undo/redo handler) must go through here
    /// so the counter stays balanced with the single `OpsSaved` event each job emits.
    pub(crate) fn persist_ops(
        &mut self,
        ctx: &egui::Context,
        image_id: i64,
        path: std::path::PathBuf,
        stack: ferrolite_pipeline::OpStack,
    ) {
        self.state.ops_save_inflight += 1;
        crate::develop::ops_persist::spawn_ops_write(
            &self.state.jobs,
            &self.state.writer,
            &self.state.tx,
            ctx,
            image_id,
            path,
            stack,
        );
    }

    /// Cancel the sparse VT's in-flight tile-load jobs for the named viewer.
    /// The VT lives in `callback_resources`; the decode jobs are cancelled
    /// separately via `ViewerState::cancel_loads`. Guarded on `image_id` so we
    /// never cancel a holder that already belongs to a newer viewer.
    pub(crate) fn cancel_viewer_tiles(&self, frame: &eframe::Frame, image_id: i64) {
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

    /// Drain "Regenerate thumbnail" requests queued by the grid context menu.
    /// Runs once per frame where the GPU render state is available; each image
    /// loads its edit stack from its `.xmp` sidecar inside the Background job
    /// (missing/malformed → identity, i.e. a color-managed unedited thumbnail).
    pub(crate) fn drain_thumb_regen_requests(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.state.pending_thumb_regen.is_empty() {
            return;
        }
        let Some(rs) = frame.wgpu_render_state() else {
            // No GPU this frame; keep the requests for a later frame.
            return;
        };
        let ids = std::mem::take(&mut self.state.pending_thumb_regen);
        let gpu = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let cam =
            crate::develop::thumb_regen::srgb_fallback_camera_to_working(self.state.working_space);
        for id in ids {
            let Some(rec) = self.state.images.iter().find(|r| r.id == id).cloned() else {
                continue;
            };
            let Ok(Some(folder)) = self.state.reads.folder_path(rec.folder_id) else {
                continue;
            };
            let path = std::path::PathBuf::from(folder).join(&rec.filename);
            crate::develop::thumb_regen::spawn_regen_edited_thumbnail(
                &self.state.jobs,
                &self.state.writer,
                &self.state.tx,
                ctx,
                std::sync::Arc::clone(&gpu),
                self.state.lens_db.clone(),
                id,
                path,
                rec.kind,
                cam,
                crate::develop::thumb_regen::RegenStackSource::Sidecar,
            );
        }
    }

    /// Build a point-in-time memory attribution from live app state. Impure
    /// (reads `ViewerState`, caches, in-flight gauges, and the OS RSS). Only call
    /// behind `diag::enabled()`. GPU/VRAM figures are documented estimates.
    fn gather_mem_breakdown(&self) -> crate::diag_mem::MemBreakdown {
        use crate::diag_mem::{linear_bytes, MemBreakdown, MemCategory};
        let mut b = MemBreakdown::empty();
        b.rss = crate::mem_probe::process_rss_bytes();
        b.budget = crate::diag_mem::adaptive_budget(crate::mem_probe::total_ram_bytes());

        if let Some(v) = self.state.viewer.as_ref() {
            let preview_src = [v.preview_source.as_ref(), v.raw_preview_source.as_ref()]
                .into_iter()
                .flatten()
                .map(|a| linear_bytes(a.width, a.height))
                .sum::<u64>();
            b.set(MemCategory::ViewerPreviewSrc, preview_src);
        }

        // GPU pyramids: EXACT summed bytes across all live `GpuPyramidSource`
        // instances (Rgba16Float = 8 B/px per level), plus the live count. This
        // is process-global on purpose — if it exceeds one image's worth while
        // the viewer sits on a single image, prior-image pyramids are being
        // retained (the develop-scroll leak). On unified memory this is real RSS.
        b.set(
            MemCategory::GpuPyramid,
            ferrolite_pipeline::live_gpu_pyramid_bytes(),
        );
        b.pyramid_live = ferrolite_pipeline::live_gpu_pyramids();
        // CPU-side `PyramidTileSource` LOD pyramids (retained f32, ~545 MB per
        // open) — the largest previously-unattributed chunk of `rss`.
        b.set(
            MemCategory::CpuPyramid,
            ferrolite_vt::live_pyramid_tile_source_bytes(),
        );
        // Live `VirtualTexture` GPU bytes (single textures + streaming/sparse
        // tile pools) and count; a count above the small expected number flags
        // retained VTs.
        b.set(
            MemCategory::VtPools,
            ferrolite_vt::live_virtual_texture_bytes(),
        );
        b.vt_live = ferrolite_vt::live_virtual_textures();

        // In-flight buffers (decode + pyramid jobs holding large Arcs).
        b.set(
            MemCategory::InflightDecode,
            crate::diag_mem::inflight_decode_bytes(),
        );
        b.set(
            MemCategory::InflightPyramid,
            crate::diag_mem::inflight_pyramid_bytes(),
        );

        // Thumb pixel cache (real bytes) and texture cache (VRAM estimate: entries ×
        // 256×256 RGBA8).
        b.set(
            MemCategory::ThumbPix,
            self.state.thumb_pixels.resident_bytes(),
        );
        b.set(
            MemCategory::ThumbTex,
            self.state.textures.len() as u64 * 256 * 256 * 4,
        );

        b
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
        // Free textures retired last frame BEFORE anything paints this frame (see
        // TextureCache::begin_frame): prevents destroying a texture still referenced by
        // this frame's paint jobs.
        self.state.textures.begin_frame();

        // One-shot restore-session (opt-in via `settings.restore_session`), run on
        // the very first frame. Reopens the last folder through the SAME job-based
        // ingest path "Open folder…" uses (never a synchronous walk on the UI
        // thread — CLAUDE.md) and then restores the last active module.
        if !self.did_restore {
            self.did_restore = true;
            if self.state.settings.restore_session {
                if let Some(folder) = self.state.settings.last_folder.clone() {
                    if folder.is_dir() {
                        crate::ingest::spawn_ingest(&mut self.state, ctx, folder);
                        self.module = self.state.settings.last_module.to_module();
                    } else {
                        eprintln!(
                            "ferrolite: restore-session skipped, folder missing: {}",
                            folder.display()
                        );
                    }
                }
            }
        }

        // One-shot startup display-profile detect, once the render state is valid
        // (ViewerPipelines pre-warmed in `new`). Ordering is guaranteed: `new()`
        // inserts `ViewerPipelines` into `cc.wgpu_render_state`'s callback
        // resources synchronously (same `if let Some(rs) = ..` block that also
        // gates this check's `wgpu_render_state()`), and `AppState::new()`
        // (which loads persisted settings, including `settings.display_profile`)
        // runs after that block completes — both finish before the first
        // `update()` call, i.e. before this line can ever run. So the mode this
        // reads via `redetect_display_profile` -> `self.state.settings.display_profile`
        // is always the persisted one, and the pipelines it targets are always
        // ready. Fires exactly once; the resulting LUT/matrix is applied when
        // the off-thread bake job reports back.
        if !self.did_display_detect && frame.wgpu_render_state().is_some() {
            self.did_display_detect = true;
            crate::app::controller::AppController::redetect_display_profile(self, ctx, frame);
        } else if self.did_display_detect {
            // Multi-monitor follow: cheap per-frame monitor-key check. When the
            // window moves to a display with a different profile, re-detect+bake.
            use raw_window_handle::HasWindowHandle;
            if let Ok(h) = frame.window_handle() {
                let (_src, key) = crate::monitor_profile::detect(h.as_raw());
                if key != self.state.last_monitor_key {
                    crate::app::controller::AppController::redetect_display_profile(self, ctx, frame);
                }
            }
        }

        let diag_t0 = crate::diag::enabled().then(std::time::Instant::now);

        if crate::diag::enabled() && ctx.input(|i| i.key_pressed(egui::Key::F9)) {
            self.diag.toggle_overlay();
        }

        if crate::diag::enabled() && ctx.input(|i| i.key_pressed(egui::Key::F10)) {
            if ctx.input(|i| i.modifiers.shift) {
                // Shift+F10: dump a full categorized snapshot to the diag log.
                let b = self.gather_mem_breakdown();
                crate::diag::write_log(&crate::diag_mem::format_mem_dump(&b));
            } else {
                self.diag.toggle_mem_overlay();
                // Populate the cache immediately on toggle-ON so the overlay
                // isn't blank until the next ~1/sec diag tick.
                if self.diag.mem_overlay_visible {
                    self.diag.last_mem = Some(self.gather_mem_breakdown());
                }
            }
        }

        // Deferred from a previous Develop→Library switch: clearing thumbnail
        // textures must happen BEFORE anything paints this frame, never in the same
        // frame they were painted (egui frees dropped textures before queue.submit).
        if self.pending_texture_clear {
            self.state.textures.clear();
            self.pending_texture_clear = false;
        }

        // Module at the start of the frame; if the title bar or Esc switches us
        // from Develop back to Library this frame, the grid's thumbnail textures
        // may be stale after the viewer's GPU work — drop them before the grid
        // paints (below) so it re-uploads fresh instead of showing grey cells.
        let module_at_frame_start = self.module;

        // Drain job results into state; upload textures and route events via controller.
        crate::app::controller::AppController::handle_events(self, ctx, frame);

        if self.state.dirty {
            self.state.refresh_images();
            self.state.dirty = false;
        }

        // One-time startup rescan of all roots (first frame, ctx available here).
        if !self.state.startup_rescan_done {
            crate::ingest::spawn_startup_rescan(&mut self.state, ctx);
            self.state.reload_vocab();
            self.state.load_export_queue();
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
                // Exportable once a full-res source exists: RAW has the tier-2
                // GPU pyramid; a Standard image's tier-1 preview already IS the
                // full-res image (no tier-2), so its retained source qualifies.
                let export_enabled = self.state.viewer.as_ref().is_some_and(|v| {
                    v.pyramid.is_some()
                        || (v.kind != ferrolite_image::FileKind::Raw && v.preview_source.is_some())
                });
                let viewer_open = self.state.viewer.is_some();
                let module_before = self.module;
                let can_undo = self
                    .state
                    .viewer
                    .as_ref()
                    .is_some_and(|v| v.history.can_undo());
                let can_redo = self
                    .state
                    .viewer
                    .as_ref()
                    .is_some_and(|v| v.history.can_redo());
                let menu_action = crate::chrome::title_bar(
                    ctx,
                    ui,
                    &mut self.module,
                    concat!("v", env!("CARGO_PKG_VERSION")),
                    export_enabled,
                    viewer_open,
                    &self.state.settings.keymap,
                    can_undo,
                    can_redo,
                    self.state.settings.show_histogram,
                    self.state.settings.show_info_overlay,
                    self.state.settings.show_tool_palette,
                );
                if self.module != module_before {
                    self.state.settings.last_module =
                        crate::settings::dto::PersistedModule::from_module(self.module);
                    self.mark_settings_dirty();
                }
                match menu_action {
                    Some(crate::chrome::MenuAction::ExportImage) => self.open_export_dialog(),
                    Some(crate::chrome::MenuAction::AddToQueue) => {
                        if let Some(id) = self.state.viewer.as_ref().map(|v| v.image_id) {
                            self.state.queue_add(id);
                            self.state.notify(
                                crate::notifications::Level::Info,
                                "Added to export queue.",
                            );
                        }
                    }
                    Some(crate::chrome::MenuAction::PurgePreviews) => {
                        // The purge (dir walk + deletes) is I/O and must never run
                        // on the UI thread; only the button click + warning text
                        // happen here.
                        let store = std::sync::Arc::clone(&self.state.preview_store);
                        self.state.jobs.submit(
                            ferrolite_jobs::Priority::Background,
                            move |_cancel| {
                                if let Err(err) = store.purge_all() {
                                    eprintln!("preview cache: purge_all failed: {err}");
                                }
                            },
                        );
                        self.state
                            .notify(crate::notifications::Level::Info, "Preview cache purged.");
                    }
                    Some(crate::chrome::MenuAction::Exit) => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Some(crate::chrome::MenuAction::Undo) => {
                        self.apply_undo_redo(ctx, frame, true);
                    }
                    Some(crate::chrome::MenuAction::Redo) => {
                        self.apply_undo_redo(ctx, frame, false);
                    }
                    Some(crate::chrome::MenuAction::SelectAll) => {
                        self.state.toggle_select_all();
                    }
                    Some(crate::chrome::MenuAction::PrevImage) => {
                        self.navigate_step(ctx, frame, crate::viewer::nav::Step::Prev);
                    }
                    Some(crate::chrome::MenuAction::NextImage) => {
                        self.navigate_step(ctx, frame, crate::viewer::nav::Step::Next);
                    }
                    Some(crate::chrome::MenuAction::SwitchModule(m)) => {
                        self.module = m;
                    }
                    Some(crate::chrome::MenuAction::ToggleSplit) => {
                        self.toggle_split_compare();
                    }
                    Some(crate::chrome::MenuAction::ZoomFit) => {
                        if let Some(v) = self.state.viewer.as_mut() {
                            if let Some(dims) = v.image_dims {
                                v.view = ferrolite_vt::ViewTransform::fit(dims, v.viewport);
                                v.idle = false;
                            }
                        }
                    }
                    Some(crate::chrome::MenuAction::ZoomActual) => {
                        if let Some(v) = self.state.viewer.as_mut() {
                            v.view = ferrolite_vt::ViewTransform {
                                zoom: 1.0,
                                pan: (0.0, 0.0),
                            };
                            v.idle = false;
                        }
                    }
                    Some(crate::chrome::MenuAction::ToggleHistogram) => {
                        self.state.settings.show_histogram = !self.state.settings.show_histogram;
                        self.mark_settings_dirty();
                    }
                    Some(crate::chrome::MenuAction::ToggleInfoOverlay) => {
                        self.state.settings.show_info_overlay =
                            !self.state.settings.show_info_overlay;
                        self.mark_settings_dirty();
                    }
                    Some(crate::chrome::MenuAction::ToggleToolPalette) => {
                        self.state.settings.show_tool_palette =
                            !self.state.settings.show_tool_palette;
                        self.mark_settings_dirty();
                    }
                    Some(crate::chrome::MenuAction::OpenHelp) => {
                        self.show_help = true;
                    }
                    Some(crate::chrome::MenuAction::OpenSettings) => {
                        self.show_settings = true;
                    }
                    None => {}
                }
            });

        let mut film_clicked: Option<i64> = None;
        match self.module {
            crate::module::Module::Library => {
                egui::TopBottomPanel::top("toolbar")
                    .exact_height(40.0)
                    .frame(
                        egui::Frame::none()
                            .fill(theme::BG_TOOLBAR)
                            .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
                    )
                    .show(ctx, |ui| {
                        let thumb_size_before = self.thumb_size;
                        let changed = crate::library::toolbar::show(
                            ui,
                            &mut self.thumb_size,
                            &mut self.state,
                        );
                        if changed {
                            self.state.dirty = true;
                            let mut pf = crate::settings::dto::PersistedFilter::from_filter(
                                &self.state.filter,
                            );
                            pf.include_subfolders = self.state.include_subfolders;
                            self.state.settings.filter = pf;
                            self.mark_settings_dirty();
                        }
                        if self.thumb_size != thumb_size_before {
                            self.state.settings.grid_size = self.thumb_size;
                            self.mark_settings_dirty();
                        }
                    });
            }
            crate::module::Module::Develop => {
                egui::TopBottomPanel::top("develop_filter")
                    .exact_height(36.0)
                    .frame(
                        egui::Frame::none()
                            .fill(theme::BG_TOOLBAR)
                            .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
                    )
                    .show(ctx, |ui| {
                        let outcome = crate::library::develop_filter_bar::show(ui, &mut self.state);
                        if outcome.changed {
                            self.state.dirty = true;
                            let mut pf = crate::settings::dto::PersistedFilter::from_filter(
                                &self.state.filter,
                            );
                            pf.include_subfolders = self.state.include_subfolders;
                            self.state.settings.filter = pf;
                            self.mark_settings_dirty();
                        }
                        if outcome.toggle_split {
                            self.toggle_split_compare();
                        }
                    });
                egui::TopBottomPanel::top("develop_filmstrip")
                    .exact_height(80.0)
                    .frame(
                        egui::Frame::none()
                            .fill(theme::BG_TOOLBAR)
                            .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
                    )
                    .show(ctx, |ui| {
                        let current = self.state.viewer.as_ref().map(|v| v.image_id);
                        film_clicked =
                            crate::library::filmstrip::show(ui, &mut self.state, current);
                    });
            }
            crate::module::Module::Export => {
                egui::TopBottomPanel::top("export_toolbar")
                    .exact_height(40.0)
                    .frame(
                        egui::Frame::none()
                            .fill(theme::BG_TOOLBAR)
                            .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
                    )
                    .show(ctx, |ui| {
                        crate::export_module::toolbar(ui, &mut self.state);
                    });
            }
        }
        if let Some(id) = film_clicked {
            if let Some(rec) = self.state.images.iter().find(|r| r.id == id).cloned() {
                self.open_record(ctx, frame, &rec);
            }
        }

        egui::TopBottomPanel::bottom("status")
            .exact_height(24.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::BG_TITLEBAR)
                    .inner_margin(egui::Margin::symmetric(12.0, 0.0)),
            )
            .show(ctx, |ui| {
                crate::status_bar::show(ui, &self.state);
            });

        crate::notifications::show(ctx, &mut self.state.notifications);

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
                .resizable(true)
                .default_width(236.0)
                .width_range(180.0..=460.0)
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
                    if crate::library::panel::show(ui, &mut self.state, ctx) {
                        self.mark_settings_dirty();
                    }
                });
        }

        // All app-level keyboard shortcuts below are suppressed while a modal
        // (Help, remove-confirmation, ...) is on screen — see `modal_active`.
        // This keeps a modal's own key handling (e.g. Help's Esc) as the only
        // thing that reacts to a keypress, and stops shortcuts like Enter or
        // Ctrl+A from leaking through to the grid/viewer underneath.
        if !self.modal_active() {
            crate::app::shortcuts::dispatch(ctx, self, frame);
        }

        // Submit the tier-1 preview decode once when a viewer opens, and (for RAW,
        // once the debounce has elapsed) the tier-2 full decode.
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
            // Debounced (FULL_DECODE_DEBOUNCE) so fast arrow-nav doesn't submit a
            // read/full decode per image flipped through — only the settled-on
            // image does, once `open_elapsed` crosses the threshold.
            //
            // Task 6 read-before-full: once the debounce elapses, consult the
            // preview cache FIRST (`spawn_cache_read`). The RAW full decode is
            // then gated on the read having resolved (`cache_resolved`), so a
            // cache HIT reveals from disk and the full decode streams in only the
            // extra zoom/1:1 detail — a MISS falls straight through to decode.
            let dt = ctx.input(|i| i.stable_dt);
            v.open_elapsed += dt;
            if v.kind == ferrolite_image::FileKind::Raw
                && (!v.cache_read_requested || (!v.full_requested && v.cache_resolved))
            {
                if v.open_elapsed >= FULL_DECODE_DEBOUNCE {
                    if !v.cache_read_requested {
                        let h = crate::develop::preview_cache::spawn_cache_read(
                            &self.state.jobs,
                            std::sync::Arc::clone(&self.state.preview_store),
                            &self.state.tx,
                            ctx,
                            v.image_id,
                            v.path.clone(),
                            v.op_stack.clone(),
                            self.state.working_space,
                        );
                        v.cache_read_handle = Some(h);
                        v.cache_read_requested = true;
                    } else if !v.full_requested && v.cache_resolved {
                        if let Some(rs) = frame.wgpu_render_state() {
                            let gpu = std::sync::Arc::new(
                                ferrolite_gpu::GpuContext::from_render_state(rs),
                            );
                            let h = viewer::load::spawn_full(
                                &self.state.jobs,
                                &self.state.tx,
                                ctx,
                                v.image_id,
                                v.path.clone(),
                                gpu,
                            );
                            v.full_handle = Some(h);
                            v.full_requested = true;
                        }
                    }
                } else {
                    // Guarantee a frame fires once the debounce elapses even if
                    // the app would otherwise go idle waiting on input, so a
                    // still (non-navigated) image's cache read still submits.
                    ctx.request_repaint_after(std::time::Duration::from_secs_f32(
                        FULL_DECODE_DEBOUNCE - v.open_elapsed,
                    ));
                }
            }
            // Read the persisted frl:ops sidecar once per open; the OpsLoaded
            // event hydrates the stack + both tiers without re-persisting.
            if !v.ops_loaded && v.ops_read_handle.is_none() {
                let h = crate::develop::ops_persist::spawn_ops_read(
                    &self.state.jobs,
                    &self.state.tx,
                    ctx,
                    v.image_id,
                    v.path.clone(),
                );
                v.ops_read_handle = Some(h);
            }
            // Spec 4.4 (U9): read this image's EXIF off-thread once per open so
            // the lens panel/picker can seed real camera/lens values instead of
            // placeholders, and so the auto-match (below, once both this AND
            // OpsLoaded resolve) has something to query. The catalog does not
            // carry focal_length/aperture/lens at all (see `meta_read`'s doc
            // comment), so this is a real (lightweight) decode, never inline.
            if !v.meta_loaded && v.meta_read_handle.is_none() {
                let h = crate::develop::meta_read::spawn_meta_read(
                    &self.state.jobs,
                    &self.state.tx,
                    ctx,
                    v.image_id,
                    v.path.clone(),
                    v.kind,
                );
                v.meta_read_handle = Some(h);
            }
        }

        // Task 7: once the current image has revealed, fire ONE low-priority
        // prefetch pass for its nearest filmstrip neighbors (radius 2, RAW
        // only) so scrubbing to them later reveals instantly from the preview
        // cache. Gathered as scalars/locals BEFORE borrowing `self.state.jobs`/
        // `preview_store` for the spawn, since `self.state.viewer` cannot stay
        // borrowed across that call (borrow discipline used elsewhere in this
        // loop, e.g. the persist-ops block above).
        if self
            .state
            .viewer
            .as_ref()
            .is_some_and(|v| v.loaded && !v.prefetch_requested)
        {
            if let Some(current_id) = self.state.viewer.as_ref().map(|v| v.image_id) {
                let ids: Vec<i64> = self.state.images.iter().map(|r| r.id).collect();
                let targets = crate::develop::preview_cache::prefetch_targets(&ids, current_id, 2);
                let neighbors: Vec<(i64, std::path::PathBuf)> = targets
                    .into_iter()
                    .filter_map(|id| {
                        let rec = self.state.images.iter().find(|r| r.id == id)?;
                        if rec.kind != ferrolite_image::FileKind::Raw {
                            return None; // Standard images are never prefetch-cached
                        }
                        let path = self.state.image_path(rec)?;
                        Some((id, path))
                    })
                    .collect();
                let handles = crate::develop::preview_cache::spawn_prefetch(
                    &self.state.jobs,
                    std::sync::Arc::clone(&self.state.preview_store),
                    ctx,
                    &neighbors,
                    self.state.working_space,
                    ferrolite_previews::DEFAULT_CACHE_CAP_BYTES,
                );
                if let Some(v) = self.state.viewer.as_mut() {
                    if v.image_id == current_id {
                        v.prefetch_handles = handles;
                        v.prefetch_requested = true;
                    }
                }
            }
        }

        if self.module == crate::module::Module::Develop && self.state.viewer.is_some() {
            self.maybe_update_histogram(ctx, frame);
        }

        if self.module == crate::module::Module::Develop && self.state.viewer.is_some() {
            let active = self.state.tool_state.active;
            if let Some(v) = self.state.viewer.as_mut() {
                v.crop_active = active == crate::develop::tool::ToolId::Crop;
                let mask_active = active == crate::develop::tool::ToolId::Mask;
                if v.mask.active && !mask_active {
                    // Mask tool deselected: close the components modal along with
                    // any in-progress edit so it doesn't linger over a different
                    // tool's panel (mirrors the gesture/overlay_key resets on
                    // stack-invalidating transitions).
                    v.mask.components_modal_open = false;
                    v.mask.editing_component = None;
                    v.mask.preview_component = None;
                }
                v.mask.active = mask_active;
                v.mask.adjusting = false; // still reset each frame; panel sets it on a drag
            }
            let mut outcome = None;
            let working_space = self.state.working_space;
            egui::SidePanel::right("develop_adjust")
                .resizable(true)
                .default_width(296.0)
                .width_range(250.0..=400.0)
                .frame(
                    egui::Frame::none()
                        .fill(theme::BG_APP)
                        .inner_margin(egui::Margin::symmetric(12.0, 8.0)),
                )
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            outcome = Some(crate::develop::tool_panel::show(
                                ui,
                                &mut self.state,
                                &self.tool_registry,
                                working_space,
                            ));
                        });
                });
            if let Some(outcome) = outcome {
                if let Some(ws) = outcome.working_space {
                    crate::app::controller::AppController::apply_working_space(self, ctx, frame, ws);
                }
                if let Some(o) = outcome.edit {
                    crate::app::controller::AppController::apply_edit(self, ctx, frame, o.kind, o.stack, o.commit);
                }
            }
        }

        // If we switched Develop → Library this frame, the filmstrip above already
        // painted (and thus recorded) these textures in this frame's paint jobs —
        // clearing now would free them before queue.submit and panic. Defer the
        // clear to the top of next frame instead (fixes all-grey cells after
        // Develop once the clear runs, without racing this frame's submit).
        if !module_at_frame_start.is_library() && self.module.is_library() {
            self.maybe_regen_on_leave(ctx, frame);
            self.pending_texture_clear = true;
        }

        if self.module == crate::module::Module::Export {
            egui::TopBottomPanel::bottom("export_bottom")
                .frame(
                    egui::Frame::none()
                        .fill(theme::BG_TOOLBAR)
                        .inner_margin(egui::Margin::symmetric(12.0, 8.0)),
                )
                .show(ctx, |ui| {
                    if let Some(a) = crate::export_module::bottom_bar::show(ui, &mut self.state) {
                        match a {
                            crate::export_module::ExportModuleAction::Start => {
                                self.start_batch(ctx, frame)
                            }
                            crate::export_module::ExportModuleAction::Cancel => {
                                if let Some(a) = self.state.export_activity.as_ref() {
                                    a.cancel_all();
                                }
                            }
                        }
                    }
                });
            egui::SidePanel::right("export_settings")
                .resizable(true)
                .default_width(296.0)
                .width_range(250.0..=400.0)
                .frame(
                    egui::Frame::none()
                        .fill(theme::BG_APP)
                        .inner_margin(egui::Margin::symmetric(12.0, 8.0)),
                )
                .show(ctx, |ui| {
                    ui.label(
                        egui::RichText::new("EXPORT SETTINGS")
                            .small()
                            .color(theme::TEXT_FAINT),
                    );
                    ui.add_space(6.0);
                    let before = self.state.export_settings;
                    crate::export::settings_form::settings_form(
                        ui,
                        &mut self.state.export_settings,
                    );
                    if self.state.export_settings != before {
                        self.state.settings.export =
                            crate::settings::dto::PersistedExport::from_options(
                                &self.state.export_settings,
                            );
                        self.mark_settings_dirty();
                    }
                });
        }

        let mut opened: Option<i64> = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BG_CANVAS))
            .show(ctx, |ui| match self.module {
                crate::module::Module::Library => {
                    // Grid; capture a double-clicked id to open after the panel closes.
                    opened =
                        crate::library::grid::show(ui, &mut self.state, self.thumb_size + 60.0);
                }
                crate::module::Module::Develop => {
                    if self.state.viewer.is_some() {
                        // FIX C: crop mode enter/exit transition. `crop_active` was
                        // (re)armed above by the Geometry section this frame; if it
                        // just changed, re-evaluate the preview NOW (before paint) so
                        // entering shows crop=full+angle and exiting applies the real
                        // crop — neither transition otherwise triggers a re-render.
                        // Gather the op_stack into a local first (borrow discipline:
                        // `set_preview_and_full(&mut self, …)` needs an exclusive
                        // borrow, so no live `&self.state.viewer` may overlap it).
                        let crop_active = self
                            .state
                            .viewer
                            .as_ref()
                            .map(|v| v.crop_active)
                            .unwrap_or(false);
                        if crop_active != self.crop_active_prev {
                            let stack = self.state.viewer.as_ref().map(|v| v.op_stack.clone());
                            if let Some(stack) = stack {
                                crate::app::controller::AppController::set_preview_and_full(self, frame, stack);
                            }
                            self.crop_active_prev = crop_active;
                        }
                        // Ctrl+scroll brush-size gesture (Mask tool active, a mask
                        // selected, Brush sub-tool — the same three conditions
                        // `mask_overlay::show` requires before it dispatches to
                        // `route_brush`, so the gesture only fires when the brush
                        // cursor/affordance is actually shown — pointer over the
                        // image): must run BEFORE `drive_viewer`,
                        // because `drive_viewer` → `viewer::paint` reads the same
                        // `i.raw_scroll_delta.y` this frame to drive canvas zoom
                        // (viewer/mod.rs `paint`). Handling the brush gesture first
                        // and zeroing `raw_scroll_delta` here (via `ctx.input_mut`)
                        // consumes the scroll so the zoom handler sees none left —
                        // the only place both concerns are close enough to serialize
                        // without restructuring `viewer::paint`'s own scroll read.
                        if let Some(v) = self.state.viewer.as_ref() {
                            if v.mask.active
                                && v.mask.selected.is_some()
                                && v.mask.tool == crate::develop::mask_ui::MaskTool::Brush
                            {
                                let dims = v.image_dims.unwrap_or((1, 1));
                                let image_rect = crate::viewer::image_screen_rect(
                                    ui.min_rect(),
                                    dims,
                                    v.view,
                                    v.viewport,
                                );
                                let ctrl_scroll_over_image = ctx.input(|i| {
                                    let ctrl = i.modifiers.command || i.modifiers.ctrl;
                                    let scroll_y = i.raw_scroll_delta.y;
                                    let over_image = i
                                        .pointer
                                        .hover_pos()
                                        .is_some_and(|p| image_rect.contains(p));
                                    (ctrl && scroll_y.abs() > f32::EPSILON && over_image)
                                        .then_some(scroll_y)
                                });
                                if let Some(scroll_y) = ctrl_scroll_over_image {
                                    if let Some(v) = self.state.viewer.as_mut() {
                                        v.mask.brush_radius =
                                            crate::develop::mask_overlay::brush_radius_from_scroll(
                                                v.mask.brush_radius,
                                                scroll_y,
                                                crate::develop::mask_panel::BRUSH_RADIUS_MIN,
                                                crate::develop::mask_panel::BRUSH_RADIUS_MAX,
                                            );
                                    }
                                    // Consume: zero the scroll so `drive_viewer`'s zoom
                                    // handler (reading the same field) does not also fire.
                                    ctx.input_mut(|i| i.raw_scroll_delta = egui::Vec2::ZERO);
                                }
                            }
                        }
                        self.drive_viewer(ui, frame);
                        if self.state.settings.show_histogram {
                            self.draw_histogram_overlay(ui);
                        }
                        // Overlay is suppressed while the Info tab is active (it shows
                        // the same facts) — a non-destructive gate, so the overlay
                        // returns when the user leaves that tab without touching the
                        // persisted `show_info_overlay` preference.
                        if self.state.settings.show_info_overlay
                            && self.state.tool_state.active_tab
                                != crate::develop::tool::TabId("info")
                        {
                            if let Some(v) = self.state.viewer.as_ref() {
                                if let (Some(meta), Some(dims)) = (v.meta.as_ref(), v.image_dims) {
                                    let fit =
                                        ferrolite_vt::ViewTransform::fit(dims, v.viewport).zoom;
                                    let facts = crate::develop::info::ImageFacts::build(
                                        meta,
                                        v.view.zoom,
                                        fit,
                                        dims,
                                    );
                                    crate::develop::info_overlay::draw(ui, &facts);
                                }
                            }
                        }
                        if self.state.settings.show_tool_palette && self.state.viewer.is_some() {
                            let ts = self.state.tool_state;
                            let can_undo = self
                                .state
                                .viewer
                                .as_ref()
                                .is_some_and(|v| v.history.can_undo());
                            let can_redo = self
                                .state
                                .viewer
                                .as_ref()
                                .is_some_and(|v| v.history.can_redo());
                            let ctx_ro = crate::develop::tool::DevelopCtx { state: &self.state };
                            let action = crate::develop::tool_palette::show(
                                ui,
                                &self.tool_registry,
                                ts,
                                &ctx_ro,
                                can_undo,
                                can_redo,
                            );
                            match action {
                                Some(crate::develop::tool_palette::PaletteAction::SelectTool(
                                    id,
                                )) => {
                                    let enabled = self
                                        .tool_registry
                                        .get(id)
                                        .map(|t| {
                                            let c = crate::develop::tool::DevelopCtx {
                                                state: &self.state,
                                            };
                                            t.enabled(&c)
                                        })
                                        .unwrap_or(false);
                                    if self.state.viewer.is_some() {
                                        self.state.tool_state.select_tool(
                                            id,
                                            enabled,
                                            &self.tool_registry,
                                        );
                                    }
                                }
                                Some(crate::develop::tool_palette::PaletteAction::Undo) => {
                                    self.apply_undo_redo(ctx, frame, true)
                                }
                                Some(crate::develop::tool_palette::PaletteAction::Redo) => {
                                    self.apply_undo_redo(ctx, frame, false)
                                }
                                None => {}
                            }
                        }
                        // Active-tool canvas overlay (crop handles, mask coverage tint,
                        // etc.) — a single dispatch to the active tool's `canvas()`
                        // replaces the old per-section crop_overlay/mask_overlay calls.
                        // Keep the mask overlay's bounded rebuild glue (needs &mut self)
                        // here, before dispatch, so `state.mask_overlay_native` is
                        // current when `MaskTool::canvas` reads it.
                        let active_tool = self
                            .state
                            .viewer
                            .is_some()
                            .then_some(self.state.tool_state.active);
                        if active_tool == Some(crate::develop::tool::ToolId::Mask) {
                            crate::app::controller::AppController::rebuild_mask_overlay_if_needed(self, frame);
                        }
                        if let Some(id) = active_tool {
                            if let Some((dims, view, viewport)) = self
                                .state
                                .viewer
                                .as_ref()
                                .map(|v| (v.image_dims.unwrap_or((1, 1)), v.view, v.viewport))
                            {
                                let image_rect = crate::viewer::image_screen_rect(
                                    ui.min_rect(),
                                    dims,
                                    view,
                                    viewport,
                                );
                                if let Some(tool) = self.tool_registry.get(id) {
                                    if let Some(o) = tool.canvas(ui, image_rect, &mut self.state) {
                                        crate::app::controller::AppController::apply_edit(self, ctx, frame, o.kind, o.stack, o.commit);
                                    }
                                }
                            }
                        }
                        // Loupe context-menu widget covers the whole canvas; while any
                        // canvas tool (Crop/Mask/Heal) is active it must NOT be
                        // registered, or it competes with that tool's own interact for
                        // input (e.g. it stole clicks from the mask color-eyedropper).
                        // Only register it in the Adjust tool, where no canvas tool
                        // owns the pointer.
                        let is_adjust_active =
                            self.state.tool_state.active == crate::develop::tool::ToolId::Adjust;
                        let ctx_menu_id = self
                            .state
                            .viewer
                            .as_ref()
                            .filter(|_| is_adjust_active)
                            .map(|v| v.image_id);
                        if let Some(image_id) = ctx_menu_id {
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
                }
                crate::module::Module::Export => {
                    crate::export_module::queue_list::show(ui, &mut self.state);
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

        // Help modal (About + live keyboard-shortcut reference). Opened by
        // F1 (`Action::OpenHelp`) or the Help menu.
        {
            let mut open = self.show_help;
            crate::help::show(ctx, &mut open, &self.state.settings.keymap);
            self.show_help = open;
        }

        // Settings window (General + Keyboard rebinding tabs). Opened by
        // Ctrl+, (`Action::OpenSettings`) or the File menu.
        {
            let mut open = self.show_settings;
            // Clone the resolved display name: `settings::ui::show` needs
            // `&mut self.state.settings` and `&self.state.display_profile_name`
            // simultaneously, which the borrow checker can't reconcile through
            // a single `&mut self.state` field-by-field borrow at this call site.
            let display_name = self.state.display_profile_name.clone();
            if crate::settings::ui::show(ctx, &mut open, &mut self.state.settings, &display_name) {
                self.mark_settings_dirty();
                crate::app::controller::AppController::redetect_display_profile(self, ctx, frame);
            }
            self.show_settings = open;
        }

        // Mask component-management modal (list + delete + Luma/Color edit).
        // `egui::Window` renders directly on `ctx`, so this can run outside the
        // SidePanel/CentralPanel closures like the other modals above. Mirrors
        // the mask-panel borrow pattern: pre-extract the (cheap, Arc-backed)
        // `OpStack` clone first, releasing the shared borrow, before taking
        // `&mut v.mask` for the call.
        {
            let stack = self.state.viewer.as_ref().map(|v| v.op_stack.clone());
            let modal_out = match (stack, self.state.viewer.as_mut()) {
                (Some(stack), Some(v)) => {
                    crate::develop::mask_components_modal::show(ctx, &stack, &mut v.mask)
                }
                _ => None,
            };
            if let Some(o) = modal_out {
                crate::app::controller::AppController::apply_edit(self, ctx, frame, o.kind, o.stack, o.commit);
            }
        }

        // Single-file export dialog (spec §8.3). Non-modal (see
        // `open_export_dialog`): the Export module panel stays reachable while
        // this is open. `dialog.options` is seeded once on open and edited
        // in-place by the dialog widgets from then on — do NOT re-sync it from
        // `state.export_settings` here, or every in-dialog edit gets reverted
        // on the next frame (see the regression this comment replaced).
        if self.state.export_dialog.is_some() {
            let outcome = {
                let dialog = self.state.export_dialog.as_mut().unwrap();
                crate::export::draw_dialog(ctx, dialog)
            };
            match outcome {
                Some(crate::export::DialogOutcome::Cancel) => {
                    self.state.export_dialog = None;
                }
                Some(crate::export::DialogOutcome::Confirm) => {
                    self.confirm_export(ctx, frame);
                }
                None => {}
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
            egui::Stroke::new(1.0_f32, theme::BORDER_STRONG),
        );

        window_resize(ctx);

        if let Some(t0) = diag_t0 {
            let frame_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let gauges = crate::diag::Gauges {
                thumb_pending: self.state.thumb_pending.len(),
                thumb_missing: self.state.thumb_missing.len(),
                thumb_handles: self.state.thumb_handles.len(),
                thumb_uploading: self.state.thumb_uploading.len(),
                pending_uploads: self.state.pending_uploads.len(),
                active_ingests: self.state.active_ingests,
                ingest_done: self.state.ingest_done,
                ingest_total: self.state.ingest_total,
                uploads_cap: MAX_THUMB_UPLOADS_PER_FRAME,
                ingest_phase: crate::diag::ingest_phase(),
                ingest_chan: crate::diag::ingest_chan(),
                export_active: crate::diag::export_active(),
                export_done: crate::diag::export_done(),
                export_failed: crate::diag::export_failed(),
                export_last_ms: crate::diag::export_last_ms(),
                // Task 17 (spec 4.5 §9): last Develop `drive_viewer` frame time
                // + this tick's max + last tiles-produced count. Recorded by
                // `drive_viewer` only when diag is enabled; reads as 0.0/0
                // otherwise (e.g. Library module, or diag off — this whole
                // block is already gated on `diag_t0.is_some()`).
                viewer_frame_ms: crate::diag::viewer_frame_ms(),
                viewer_frame_max_ms: crate::diag::viewer_frame_max_ms(),
                viewer_tiles_produced: crate::diag::viewer_tiles_produced(),
            };
            let stats = self.state.jobs.stats();
            let repaint_forced = !self.state.pending_uploads.is_empty();
            if let Some(snap) = self.diag.tick(
                std::time::Instant::now(),
                stats,
                gauges,
                frame_ms,
                repaint_forced,
            ) {
                if crate::diag::log_enabled() {
                    crate::diag::write_log(&crate::diag::format_log(&snap));
                }
                // Memory: gather once per diag tick, push to the growth ring,
                // cache for the overlay draw site (avoids a per-frame RSS
                // syscall), and log the structured line. `mem_elapsed_s` is
                // cumulative (not `snap.dt`, the ~1s inter-tick delta) so the
                // log line reads e.g. `t+12.0s` and a scroll session's log is
                // reconstructable.
                let mem = self.gather_mem_breakdown();
                self.diag.mem_elapsed_s += snap.dt;
                self.diag.mem_history.push(crate::diag_mem::MemSample {
                    t_secs: self.diag.mem_elapsed_s as f32,
                    rss: mem.rss,
                    cpu_known: mem.total_modeled(),
                    cache: mem.get(crate::diag_mem::MemCategory::RamCache),
                });
                self.diag.last_mem = Some(mem);
                if crate::diag::log_enabled() {
                    crate::diag::write_log(&crate::diag_mem::format_mem_log_line(
                        self.diag.mem_elapsed_s,
                        &mem,
                    ));
                }
            }
            if crate::diag::overlay_enabled() && self.diag.overlay_visible {
                if let Some(snap) = self.diag.last_snapshot() {
                    crate::diag::draw_overlay(ctx, snap);
                }
            }
            if crate::diag::enabled() && self.diag.mem_overlay_visible {
                if let Some(mem) = self.diag.last_mem {
                    crate::diag_mem::draw_mem_overlay(ctx, &mem, &self.diag.mem_history);
                }
            }
        }

        // End-of-frame: persist any settings mutated this frame, off the UI
        // thread. `settings_dirty` is set by the various settings-mutating call
        // sites (export options, Library filter, confirm-before-remove,
        // last-folder/module) whenever a value actually changes; this is a
        // no-op on frames where nothing did.
        self.save_settings_if_dirty();
    }

    /// Prevent the UI thread from blocking unboundedly on worker joins at close
    /// (docs/superpowers/investigations/2026-07-02-thumbnail-and-shutdown-bugs.md
    /// §C). Cancel in-flight tracked work, stop new dispatch, then bounded-join
    /// so the later implicit `Drop for JobSystem` finds workers already stopped
    /// and returns instantly.
    ///
    /// `on_exit` runs synchronously on the UI thread, so the join bound must
    /// stay short enough that Windows/the OS never flags the app as
    /// "Not Responding" during close. A prior version waited up to 500ms here,
    /// which — combined with in-flight thumbnailing having no mid-file cancel
    /// checkpoint — was long enough to trip the hang detector. With the
    /// producer's per-file and mid-file `cancel.is_cancelled()` checkpoints
    /// (see `ingest_job` in ingest.rs) workers now observe cancellation almost
    /// immediately, so a short bounded wait (~75ms) is a graceful-but-quick
    /// compromise rather than an instant kill. `join_with_timeout` still
    /// detaches on timeout, so close always stays bounded even if a worker
    /// doesn't stop in time.
    ///
    /// This build has `default-features = false, features = ["wgpu", ...]`
    /// (no `glow`), so `App::on_exit` takes no `gl` parameter — see
    /// eframe-0.29.1 src/epi.rs `#[cfg(not(feature = "glow"))] fn on_exit(&mut self)`.
    fn on_exit(&mut self) {
        let t0 = crate::diag::enabled().then(std::time::Instant::now);
        let before = crate::diag::enabled().then(|| self.state.jobs.stats());

        self.state.cancel_pending_jobs();
        self.state.jobs.request_shutdown();
        let timeout_ms = 75u64;
        let joined = self
            .state
            .jobs
            .join_with_timeout(std::time::Duration::from_millis(timeout_ms));
        if !joined {
            eprintln!(
                "ferrolite: worker(s) still running at close after {timeout_ms}ms; detaching so the app can exit"
            );
        }

        if let (Some(t0), Some(before)) = (t0, before) {
            let on_exit_ms = t0.elapsed().as_secs_f64() * 1000.0;
            crate::diag::write_log(&crate::diag::format_shutdown(
                before, joined, timeout_ms, on_exit_ms,
            ));
        }
    }
}

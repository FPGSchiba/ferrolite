use crate::canvas::{self, CanvasResources};
use crate::module::Module;
use crate::theme;
use crate::viewer;

pub struct FerroliteApp {
    module: Module,
    thumb_size: f32,
    state: crate::state::AppState,
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
    show_help: bool,
    /// Whether the Settings window (`crate::settings::ui::show`) is open.
    /// Opened by `Action::OpenSettings` (Ctrl+, global) or the File menu.
    show_settings: bool,
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
        }
    }

    /// Mark `state.settings` as changed so `save_settings_if_dirty()` writes
    /// it off the UI thread at the end of this frame's `update()`. Every
    /// settings mutation site must call this (see `settings::keymap::Keymap`
    /// doc comment).
    fn mark_settings_dirty(&mut self) {
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
    fn apply_undo_redo(&mut self, ctx: &egui::Context, frame: &eframe::Frame, undo: bool) {
        let result = self.state.viewer.as_mut().and_then(|v| {
            if undo {
                v.history.undo()
            } else {
                v.history.redo()
            }
        });
        if let Some(stack) = result {
            self.set_preview_and_full(frame, stack.clone());
            if let Some(v) = self.state.viewer.as_mut() {
                v.edits_dirty = true;
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
    fn navigate_step(
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
    fn toggle_split_compare(&mut self) {
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
    /// Handle a tier-1 preview: run ONE sRGB→working color pass on the
    /// already-off-thread-converted linear buffer, build the rung-1
    /// `VirtualTexture` wrapping its output directly (no throwaway upload),
    /// stash it (+ GpuContext) in eframe's `callback_resources`, and fit the
    /// view. The full 9-node preview `EditPipeline` stays lazy — built on the
    /// first edit by `set_preview_and_full`, not here. Stale events (no open
    /// viewer, or a different image_id) are dropped — the user may have
    /// closed/switched the viewer mid-decode.
    fn apply_preview_ready(
        &mut self,
        frame: &eframe::Frame,
        image_id: i64,
        linear: &ferrolite_image::LinearRgbaF32,
    ) {
        let Some(v) = self.state.viewer.as_mut() else {
            return; // viewer closed while decoding
        };
        if v.image_id != image_id {
            return; // stale: a different image is now open
        }
        // Retain the sRGB-linear source. For Standard it is displayed and feeds
        // the lazy preview `EditPipeline`; for RAW it is kept ONLY as the
        // full-decode-failure fallback (`FullFailed`) and is never shown on the
        // happy path.
        let is_raw = v.kind == ferrolite_image::FileKind::Raw;
        v.preview_source = Some(std::sync::Arc::new(linear.clone()));

        // RAW: do NOT reveal the embedded JPEG. Keep the spinner up until the
        // color-managed raw render is built at full-decode (`apply_full_decoded`),
        // so the reveal comes from the same pipeline as the sparse full — a
        // sharpness-only ramp with no color/tone shift.
        if is_raw {
            return;
        }

        // Standard: the preview IS the full-resolution image — reveal it now.
        self.reveal_srgb_preview(frame, image_id);
    }

    /// Build the rung-1 preview `VirtualTexture` from the retained sRGB
    /// `preview_source` via one `sRGB→working` color pass, install the holder,
    /// fit the view, and mark the viewer `loaded` + `idle`. Shared by the
    /// Standard preview reveal (`apply_preview_ready`) and the RAW
    /// full-decode-failure fallback (`FullFailed`). Returns `true` on success,
    /// `false` if a prerequisite (GPU / viewer / source) is missing.
    fn reveal_srgb_preview(&mut self, frame: &eframe::Frame, image_id: i64) -> bool {
        let pw = self.preview_to_working();
        let w2d = ferrolite_color::working_to_display(self.state.working_space);
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
            // A Standard image never reaches apply_full_decoded, so set the tail here.
            vp.pipelines.set_display_matrix(&gpu.queue, w2d);
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

        rs.renderer
            .write()
            .callback_resources
            .insert(viewer::ViewerGpu {
                ctx: gpu,
                preview: vt,
                full: None,
                preview_before: None,
                image_id,
            });
        self.mark_histogram_dirty();
        true
    }

    /// Flag the histogram stale so the next frame recomputes it (debounced).
    fn mark_histogram_dirty(&mut self) {
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

    /// camera→working for the open viewer's RAW profile (full-res tier).
    ///
    /// Row-normalized (`normalize_neutral`) because the RAW demosaic already
    /// applied the as-shot white-balance gains; without this the DNG color
    /// matrix re-neutralizes the camera response and neutrals skew red (double
    /// white balance). The sRGB preview tier is NOT normalized — see
    /// `preview_to_working`.
    fn camera_to_working(&self) -> [[f32; 3]; 3] {
        match self.state.viewer.as_ref() {
            Some(v) => ferrolite_color::normalize_neutral(self.source_to_working(&v.color_profile)),
            None => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    /// True while any modal overlay is on screen (Help, Settings, the
    /// remove-folder confirmation). Used to suppress the app's global
    /// keyboard shortcuts underneath the modal so its own input handling
    /// (e.g. Esc) is the only thing that reacts, and so shortcuts like
    /// Enter/Ctrl+A don't leak through to the grid/viewer while a modal is
    /// up. Extend this with new modals as they're added.
    fn modal_active(&self) -> bool {
        self.show_help || self.show_settings || self.state.pending_remove.is_some()
    }

    /// If the current viewer's edit stack changed this session, spawn a
    /// Background job to regenerate its Library thumbnail from the in-memory
    /// stack, then clear the flag so re-entrant frames do not double-spawn.
    /// Called at every "leave Develop for this image" transition. No-op when
    /// there is no viewer, no session edits, or no GPU render state.
    fn maybe_regen_on_leave(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
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
        let camera_to_working = self.camera_to_working();

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
                    self.state.warning =
                        Some("Image still loading; cannot export yet.".to_string());
                    return;
                }
            }
        } else {
            self.state.warning = Some("Image still loading; cannot export yet.".to_string());
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
            self.state.warning = Some("No GPU render state; cannot export.".to_string());
            return;
        };
        let gpu = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));

        let working_space = self.state.working_space;

        crate::export::spawn_export(
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
        self.state.warning = Some("Exporting…".to_string());
    }

    /// Resolve output filenames and spawn one Background export job per queued
    /// image (spec §8.4). Filenames are expanded + collision-resolved up front on
    /// the UI thread so {seq} is deterministic and disk collisions are avoided.
    fn start_batch(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(dest_dir) = self.state.export_dest.clone() else {
            self.state.warning = Some("Choose a destination folder first.".to_string());
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
            self.state.warning = if skipped > 0 {
                Some(format!(
                    "No images could be resolved for export ({skipped} skipped)."
                ))
            } else {
                Some("No queued images could be resolved to a file on disk.".to_string())
            };
            return;
        }

        let Some(rs) = frame.wgpu_render_state() else {
            self.state.warning = Some("No GPU render state; cannot export.".to_string());
            return;
        };
        let gpu = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let working_space = self.state.working_space;

        let handles =
            crate::export::batch::spawn_batch(&self.state, ctx, gpu, items, working_space, options);
        let total = handles.len();
        let mut bs = crate::export::batch::BatchExportState::new(total);
        bs.handles = handles;
        self.state.batch = Some(bs);
        self.state.warning = Some(if skipped > 0 {
            format!("Exporting {total} image(s)… (skipped {skipped} with unresolved paths)")
        } else {
            format!("Exporting {total} image(s)…")
        });
    }

    /// sRGB→working for the preview tier: the embedded preview and Standard images
    /// are sRGB-primaries, so they convert via the sRGB fallback profile.
    fn preview_to_working(&self) -> [[f32; 3]; 3] {
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
            let cam = self.camera_to_working();
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

    /// Handle a tier-2 full decode: build a `PyramidTileSource` from the
    /// display-linear image, wrap it as a sparse (rung-4) `VirtualTexture`,
    /// store it alongside the preview in `ViewerGpu`, and begin the preview→full
    /// crossfade. Stale events (no open viewer / different image_id) are dropped.
    fn apply_full_decoded(
        &mut self,
        frame: &eframe::Frame,
        ctx: &egui::Context,
        image_id: i64,
        image: &ferrolite_image::LinearRgbaF32,
        color_profile: &ferrolite_decode::ColorProfile,
    ) {
        let Some(v) = self.state.viewer.as_mut() else {
            return; // viewer closed while decoding
        };
        if v.image_id != image_id {
            return; // stale: a different image is now open
        }
        v.color_profile = color_profile.clone();
        let is_raw = v.kind == ferrolite_image::FileKind::Raw;
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };

        // `v` only guarded staleness above; release the borrow before taking the
        // renderer lock so we can re-borrow afterwards. (Both live on `self` but
        // do not alias.)
        let _ = v;

        // Compute camera→working BEFORE any exclusive `viewer` borrow below:
        // `camera_to_working` itself borrows `self.state.viewer` immutably.
        let cam = self.camera_to_working();
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);

        // RAW rung-1 reveal render (Approach A): run the demosaiced camera-native
        // `image` through the op stack with the SAME camera→working matrix + op
        // stack as the sparse full below, so the preview→full swap is a
        // sharpness-only ramp with no color/tone shift. Build the preview
        // `EditPipeline` ONCE here and retain it (`v.preview_edit`) for reuse by
        // `set_preview_and_full` — never compiled per edit (CLAUDE.md rule 2).
        // Standard images never reach `apply_full_decoded`.
        // Build the camera-native reveal source ONCE for RAW. This same `Arc` is
        // reused both as `v.raw_preview_source` (the rung-1 reveal render input)
        // AND as the preview-cache write-back payload below — the demosaiced
        // buffer is never memcpy'd a second time onto the UI thread.
        let raw_preview_source: Option<std::sync::Arc<ferrolite_image::LinearRgbaF32>> =
            is_raw.then(|| std::sync::Arc::new(image.clone()));
        let raw_preview: Option<(std::sync::Arc<wgpu::Texture>, (u32, u32))> =
            if let Some(src) = raw_preview_source.as_ref() {
                match self.state.viewer.as_mut() {
                    Some(v) if v.image_id == image_id => {
                        v.raw_preview_source = Some(std::sync::Arc::clone(src));
                        let ctx_arc =
                            std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
                        let mut ep = ferrolite_pipeline::EditPipeline::new(
                            ctx_arc,
                            src,
                            v.op_stack.clone(),
                            cam,
                        );
                        let out = ep.evaluate();
                        let tex = out.texture.clone();
                        let dims = (out.width, out.height);
                        v.preview_edit = Some(ep);
                        Some((tex, dims))
                    }
                    _ => None,
                }
            } else {
                None
            };

        let source: std::sync::Arc<dyn ferrolite_vt::TileSource + Send + Sync> =
            std::sync::Arc::new(ferrolite_vt::PyramidTileSource::new(image.clone()));
        // Fetch the pre-warmed pipelines, build the sparse full VT (and, for RAW,
        // the rung-1 preview VT wrapping the reveal render) while borrowing them,
        // then release the read lock before the write scope that installs them.
        let (preview_vt, full) = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            vp.pipelines.set_display_matrix(
                &gpu.queue,
                ferrolite_color::working_to_display(self.state.working_space),
            );
            let full = ferrolite_vt::VirtualTexture::sparse(
                &gpu,
                source,
                std::sync::Arc::clone(&self.state.jobs),
                VIEWER_TILE_BUDGET,
                &vp.pipelines,
            );
            let preview_vt = raw_preview.as_ref().map(|(tex, dims)| {
                ferrolite_vt::VirtualTexture::single_from_texture(
                    &gpu,
                    std::sync::Arc::clone(tex),
                    *dims,
                    &vp.pipelines,
                )
            });
            (preview_vt, full)
        };

        // Install the full VT. For RAW the rung-1 preview IS the reveal render, so
        // install a fresh holder (there is no JPEG holder — `apply_preview_ready`
        // kept the spinner up). Replaces any stale holder from a superseded image.
        // Only flip `full_ready` / start the crossfade if the holder is for THIS
        // image — otherwise (stale) the viewer would permanently idle with no full
        // VT to swap to.
        let mut full_installed = false;
        {
            let mut renderer = rs.renderer.write();
            if let Some(preview) = preview_vt {
                renderer.callback_resources.insert(viewer::ViewerGpu {
                    ctx: ferrolite_gpu::GpuContext::from_render_state(rs),
                    preview,
                    full: Some(full),
                    preview_before: None,
                    image_id,
                });
                full_installed = true;
            } else if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                // Non-RAW defensive path (Standard never submits a tier-2 decode).
                if g.image_id == image_id {
                    g.full = Some(full);
                    full_installed = true;
                }
            }
        }

        if full_installed {
            if let Some(v) = self.state.viewer.as_mut() {
                if v.image_id == image_id {
                    // Step 3 reveal: the rung-1 raw render is now on screen, so
                    // drop the spinner. (For RAW `loaded` was held false in
                    // `apply_preview_ready` until this color-correct reveal.)
                    v.loaded = true;
                    v.full_ready = true;
                    v.begin_crossfade();
                    // The full tier's dimensions (uprighted, half-res demosaic)
                    // are the reveal render's dims too. Fit to them; fall back to
                    // the image's own size if the canvas has not painted yet (the
                    // user has not interacted at open time).
                    let full_dims = (image.width, image.height);
                    v.image_dims = Some(full_dims);
                    let viewport = if v.viewport.0 > 0.0 && v.viewport.1 > 0.0 {
                        v.viewport
                    } else {
                        (full_dims.0 as f32, full_dims.1 as f32)
                    };
                    v.view = ferrolite_vt::ViewTransform::fit(full_dims, viewport);
                    // Build the GPU-resident pyramid UNCONDITIONALLY so the
                    // full-res edit producer can be created on the first edit even
                    // for an image that opened unedited (identity stack).
                    let pyramid =
                        std::sync::Arc::new(ferrolite_pipeline::GpuPyramidSource::new(&gpu, image));
                    v.pyramid = Some(std::sync::Arc::clone(&pyramid));
                    // Always attach the full-res producer so the sparse VT tiles
                    // pass through camera→working (the raw camera-native CPU path
                    // must never reach the working→display tail). Identity stack =
                    // unedited-but-color-managed.
                    let ctx_arc =
                        std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
                    let tep = ferrolite_pipeline::TileEditPipeline::new(
                        ctx_arc,
                        pyramid,
                        v.op_stack.clone(),
                        cam,
                    );
                    v.edit_producer = Some(viewer::EditTileProducer::new(tep));
                    let version = v.opstack_version.max(1);
                    let mut renderer = rs.renderer.write();
                    if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                        if g.image_id == image_id {
                            if let Some(full) = g.full.as_mut() {
                                full.set_producing(true);
                                full.set_opstack_version(&g.ctx, version);
                            }
                        }
                    }
                }
            }
        }

        // Preview-cache write-back (Task 5): on a qualifying RAW open, cache the
        // identity (unedited) color-managed render so a later open of the same
        // file can reveal instantly from disk (Task 6's read path).
        //
        // CORRECTNESS GUARD: `preview_cache::key_for` hashes the ACTUAL op stack,
        // but the payload encoded here is the IDENTITY (camera→working→display)
        // render computed on the CPU from `image` — never the GPU op-stack
        // result. Caching an identity render under an *edited* key would later
        // reveal the wrong (unedited) image, so `should_write_back` gates on the
        // stack being `OpStack::default()`. Edited images are a deliberate cache
        // miss until a later task reads back the real GPU render.
        //
        // Task 6 threads the real "cache miss" flag through the read path:
        // `v.cache_write_back` is `false` after a cache HIT (the entry already
        // exists) and `true` after a MISS, so a hit never re-encodes. Key
        // assembly (`key_for`'s `fs::metadata` stat), encode/JPEG, and disk I/O
        // all run inside the Background job. The only UI-thread work here is the
        // `should_write_back` guard plus cheap refcount bumps (the reveal render
        // `Arc` is reused — no second full-buffer clone) (CLAUDE.md rule 1).
        if full_installed {
            // Snapshot the viewer inputs, then release the borrow before the
            // job submit (which borrows other `self.state` fields).
            let write_back = self.state.viewer.as_ref().and_then(|v| {
                if v.image_id != image_id {
                    return None;
                }
                crate::develop::preview_cache::should_write_back(
                    is_raw,
                    &v.op_stack,
                    v.cache_write_back,
                )
                .then(|| (v.path.clone(), v.op_stack.clone()))
            });
            // `should_write_back` requires `is_raw`, so `raw_preview_source` is
            // always `Some` here — but match defensively rather than unwrap.
            if let (Some((path, op_stack)), Some(render)) =
                (write_back, raw_preview_source.as_ref())
            {
                // Identity display pipeline: camera→working (`cam`) then
                // working→display. `mul_mat3(a, b)` = a·b, so this applies
                // `cam` first, matching the identity reveal (minus 8-bit
                // quantization). The op stack is NOT applied here — the guard
                // above ensures we only reach this when it is identity anyway.
                let display_matrix = ferrolite_color::mul_mat3(
                    &ferrolite_color::working_to_display(self.state.working_space),
                    &cam,
                );
                // Reuse the reveal `Arc` (no second full-buffer clone) and let
                // the job assemble the key off-thread (`key_for` does an
                // `fs::metadata` stat — never on the UI thread).
                crate::develop::preview_cache::spawn_cache_write(
                    &self.state.jobs,
                    std::sync::Arc::clone(&self.state.preview_store),
                    &self.state.tx,
                    ctx,
                    path,
                    op_stack,
                    self.state.working_space,
                    color_profile.clone(),
                    std::sync::Arc::clone(render),
                    display_matrix,
                    ferrolite_previews::DEFAULT_CACHE_CAP_BYTES,
                    image_id,
                );
            }
        }

        self.mark_histogram_dirty();
    }

    /// A preview-cache READ resolved to a HIT (Task 6): the cached JPEG for
    /// `image_id` was decoded off-thread to `linear`. Reveal it via the same
    /// Improvement-1 sRGB path Standard images use (`reveal_srgb_preview`, which
    /// runs one bounded `sRGB→working` GPU pass, fits, and installs the VT), so a
    /// second visit to a RAW shows instantly WITHOUT the RAW pixel decode. Then
    /// mark `cache_resolved` so the sparse full decode still fires next frame for
    /// zoom/1:1 detail, and `cache_write_back = false` so that full decode does
    /// NOT re-encode an entry that already exists. Stale `image_id` is dropped.
    fn apply_preview_cache_hit(
        &mut self,
        frame: &eframe::Frame,
        image_id: i64,
        linear: &ferrolite_image::LinearRgbaF32,
    ) {
        match self.state.viewer.as_mut() {
            Some(v) if v.image_id == image_id => {
                // Reuse the sRGB reveal path, which reads `preview_source`.
                v.preview_source = Some(std::sync::Arc::new(linear.clone()));
            }
            _ => return, // stale: viewer closed or a different image is open
        }
        let revealed = self.reveal_srgb_preview(frame, image_id);
        if revealed {
            self.mark_histogram_dirty();
        }
        if let Some(v) = self.state.viewer.as_mut() {
            if v.image_id == image_id {
                // A hit already has the entry on disk: do not write it back.
                v.cache_write_back = false;
                // Let the debounced full decode fire next frame (zoom detail).
                v.cache_resolved = true;
                // reveal_srgb_preview marks the viewer idle (no tier-2 for the
                // Standard path); the RAW hit still wants the sparse full, so
                // clear idle to keep the drive loop alive until it arrives.
                if revealed {
                    v.idle = false;
                }
            }
        }
    }

    /// A preview-cache READ resolved to a MISS (Task 6): no usable entry, so let
    /// the existing full-decode path run (`cache_resolved`) and have it cache its
    /// result (`cache_write_back`, consumed by `should_write_back` in
    /// `apply_full_decoded`). Stale `image_id` is dropped.
    fn apply_preview_cache_miss(&mut self, image_id: i64) {
        if let Some(v) = self.state.viewer.as_mut() {
            if v.image_id == image_id {
                v.cache_write_back = true;
                v.cache_resolved = true;
            }
        }
    }

    /// Apply `stack` to both render tiers (GPU + memory only; no history/persist).
    /// Preview tier: build the EditPipeline once, reuse via set_stack; evaluate
    /// and swap the displayed single texture. Full-res tier: set_stack (color) or
    /// rebuild (geometry/halo), bump the opstack version to invalidate cached tiles.
    fn set_preview_and_full(&mut self, frame: &eframe::Frame, stack: ferrolite_pipeline::OpStack) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        // Compute before taking the exclusive `viewer` borrow below:
        // `camera_to_working`/`preview_to_working` themselves borrow
        // `self.state.viewer` immutably.
        let cam = self.camera_to_working();
        let pw = self.preview_to_working();
        let Some(v) = self.state.viewer.as_mut() else {
            return;
        };
        let old = v.op_stack.clone();
        v.op_stack = stack.clone();
        v.opstack_version = v.opstack_version.wrapping_add(1);

        // What the preview should show: the live stack, or the empty stack in
        // before/after mode. While the crop tool is active, keep the ROTATION
        // (and aspect) applied but force crop = full: the crop rectangle is then
        // represented by the overlay drawn over the full, rotated image, and the
        // Angle slider rotates the preview live. (In before/after mode `shown` is
        // identity — no geometry — so this branch is a no-op, which is correct.)
        let mut shown = if v.before_after {
            ferrolite_pipeline::OpStack::default()
        } else {
            stack.clone()
        };
        if v.crop_active {
            if let Some(g) = shown.geometry() {
                shown = shown.set_op(ferrolite_pipeline::Op::Geometry(
                    ferrolite_pipeline::Geometry {
                        crop: ferrolite_pipeline::CropRect::full(),
                        angle_deg: g.angle_deg,
                        aspect: g.aspect,
                    },
                ));
            }
        }

        // Preview tier (built once per image, reused). For RAW this pipeline was
        // already built at full-decode (`apply_full_decoded`) from the demosaic
        // source with `cam`; this rebuild branch is the Standard/lazy fallback.
        // Source + matrix must match the tier the image is displayed on: RAW =
        // demosaic + camera→working (`cam`); Standard = sRGB source + sRGB→working
        // (`pw`). Sourcing RAW from the sRGB JPEG here would reintroduce the color
        // shift this task removes.
        if v.preview_edit.is_none() {
            let (src, matrix) = v.preview_tier_source(cam, pw);
            if let Some(src) = src {
                let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
                v.preview_edit = Some(ferrolite_pipeline::EditPipeline::new(
                    ctx_arc,
                    &src,
                    shown.clone(),
                    matrix,
                ));
            }
        }
        if let Some(ep) = v.preview_edit.as_mut() {
            ep.set_stack(shown.clone());
            // Evaluate BEFORE taking the renderer lock; pass the resulting texture
            // (cheap Arc clone) into the write scope. (`ep` borrows `self.state`,
            // `renderer` borrows `frame` — disjoint, so they may coexist, but we
            // keep the evaluate out of the lock scope to stay close to the
            // apply_full_decoded discipline.)
            let img = ep.evaluate();
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == v.image_id {
                    g.preview
                        .update_single_from_texture(img.texture.clone(), (img.width, img.height));
                }
            }
        }

        // Full-res tier (only meaningful once the full decode + pyramid exist).
        // Render `shown` here too (not the live `stack`): in before/after mode
        // `shown` is identity. The sparse VT is now ALWAYS producer-driven: the
        // "before" (identity `shown`) is rendered by the producer with an
        // identity op-stack + camera→working — the correct unedited image in
        // working space — never the raw camera-native CPU path. The
        // opstack_version bump above invalidates stale produced tiles so the new
        // (edited or unedited) tiles are re-produced on toggle.
        if v.full_ready {
            let rebuild = v.edit_producer.is_none()
                || crate::develop::ops_edit::needs_full_rebuild(&old, &shown);
            if rebuild {
                if let Some(pyr) = v.pyramid.clone() {
                    let ctx_arc =
                        std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
                    let tep =
                        ferrolite_pipeline::TileEditPipeline::new(ctx_arc, pyr, shown.clone(), cam);
                    v.edit_producer = Some(viewer::EditTileProducer::new(tep));
                }
            } else if let Some(producer) = v.edit_producer.as_mut() {
                // Color-only change: update params in place.
                producer.set_stack(shown.clone());
            }
            let version = v.opstack_version;
            let image_id = v.image_id;
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == image_id {
                    if let Some(full) = g.full.as_mut() {
                        full.set_producing(true);
                        full.set_opstack_version(&g.ctx, version);
                    }
                }
            }
        }
        v.idle = false; // wake the drive loop so producer tiles re-render
        self.mark_histogram_dirty();
    }

    /// Apply a panel/widget edit: update both tiers immediately; on commit (drag
    /// release / discrete change) push undo history + persist off-thread.
    fn apply_edit(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        kind: ferrolite_pipeline::OpKind,
        stack: ferrolite_pipeline::OpStack,
        commit: bool,
    ) {
        self.set_preview_and_full(frame, stack.clone());
        if !commit {
            return;
        }
        let Some(v) = self.state.viewer.as_mut() else {
            return;
        };
        v.edits_dirty = true;
        v.history.push(kind, stack.clone());
        let image_id = v.image_id;
        let path = v.path.clone();
        let has_edits = !stack.is_identity();
        if let Some(rec) = self.state.images.iter_mut().find(|r| r.id == image_id) {
            rec.has_edits = has_edits; // optimistic cache update (filmstrip badge)
        }
        self.persist_ops(ctx, image_id, path, stack);
    }

    /// Change the editing working space: recompose camera→working + working→display,
    /// push the tail matrix to the display pipelines (once), update both edit tiers,
    /// and invalidate full-res tiles so they re-render. Never rebuilds pipelines.
    ///
    /// Wired to the Develop adjustment panel's working-space `ComboBox`.
    fn apply_working_space(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        ws: ferrolite_color::WorkingSpace,
    ) {
        if ws == self.state.working_space {
            return;
        }
        self.state.working_space = ws;
        self.state.settings.working_space =
            crate::settings::dto::PersistedWorkingSpace::from_ws(ws);
        self.mark_settings_dirty();
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);

        // Push the working→display tail (shared uniform; not per-frame).
        {
            let renderer = rs.renderer.read();
            if let Some(vp) = renderer.callback_resources.get::<viewer::ViewerPipelines>() {
                vp.pipelines
                    .set_display_matrix(&gpu.queue, ferrolite_color::working_to_display(ws));
            }
        }

        let cam = self.camera_to_working();
        let pw = self.preview_to_working();
        let Some(v) = self.state.viewer.as_mut() else {
            ctx.request_repaint();
            return;
        };

        // Preview tier: update the matrix, re-evaluate, swap the displayed texture.
        // Source + matrix must match the tier the image is displayed on (the same
        // choice `set_preview_and_full`/`apply_full_decoded` make): RAW = demosaic
        // camera-native `raw_preview_source` + camera→working (`cam`); Standard =
        // sRGB `preview_source` + sRGB→working (`pw`). Applying `pw` to a RAW
        // preview would diverge it (and the histogram that reads it) from the full
        // tier and reintroduce the RAW color/tone shift progressive reveal removes.
        let (pv_src, pv_matrix) = v.preview_tier_source(cam, pw);
        if let Some(ep) = v.preview_edit.as_mut() {
            ep.set_color_matrix(pv_matrix);
            let img = ep.evaluate();
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == v.image_id {
                    g.preview
                        .update_single_from_texture(img.texture.clone(), (img.width, img.height));
                }
            }
        } else if let Some(src) = pv_src {
            // No edit yet: re-run the one-shot color pass from the kind-correct
            // source (RAW demosaic / sRGB) with its matrix.
            let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
            let converted = ferrolite_pipeline::color_convert(ctx_arc, &src, pv_matrix);
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == v.image_id {
                    g.preview.update_single_from_texture(
                        converted.texture.clone(),
                        (converted.width, converted.height),
                    );
                }
            }
        }

        // Full-res tier: update the producer's matrix + invalidate cached tiles.
        if let Some(producer) = v.edit_producer.as_mut() {
            producer.set_color_matrix(cam);
        }
        v.opstack_version = v.opstack_version.wrapping_add(1);
        let version = v.opstack_version;
        let image_id = v.image_id;
        {
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == image_id {
                    g.preview_before = None; // rebuilt by ensure_before_view with new WS
                    if let Some(full) = g.full.as_mut() {
                        full.set_opstack_version(&g.ctx, version);
                    }
                }
            }
        }
        v.idle = false;
        self.mark_histogram_dirty();
        ctx.request_repaint();
    }
}

/// Physical tile-pool budget for the viewer's sparse VT. 256 tiles × 256² ×
/// RGBA16F ≈ 128 MB of GPU memory — generous headroom for a fit-to-window view
/// plus a few zoom levels of the quad-binned (half-res) full image.
const VIEWER_TILE_BUDGET: u32 = 256;

/// Max edited tiles rendered per frame on the render thread (bounds GPU work;
/// CLAUDE.md GPU rule). Remaining needed tiles are produced on subsequent frames.
const MAX_PRODUCE_PER_FRAME: usize = 8;

/// Max thumbnail texture uploads per frame (bounds per-frame GPU/texture work
/// during bulk thumbnail delivery; CLAUDE.md responsiveness rule). Overflow is
/// stashed in `AppState.pending_uploads` and flushed over subsequent frames.
const MAX_THUMB_UPLOADS_PER_FRAME: usize = 16;

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
                            produced_this_frame =
                                full.produce_view(&g.ctx, producer, &needed, MAX_PRODUCE_PER_FRAME);
                        }
                    }
                    tiles_pending = full.sparse_pending();
                    produce_pending = full.produce_pending();
                    needed_established = full.needed_established();
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
        // moved, so read `idle` AFTER it to catch an interaction this frame.
        let loading_preview = viewer::paint(ui, v, show_full, interactive);
        let idle = v.idle;

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
        if !idle && (loading_preview || crossfading || tiles_loading || full_warming) {
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
                        show_full: false,
                        which: viewer::PreviewWhich::Before,
                    },
                ));
            // Divider line + a grab handle at mid-height.
            let painter = ui.painter();
            painter.vline(
                div_x,
                canvas_rect.y_range(),
                egui::Stroke::new(1.5, egui::Color32::WHITE),
            );
            let handle_center = egui::pos2(div_x, canvas_rect.center().y);
            painter.circle(
                handle_center,
                7.0,
                egui::Color32::from_black_alpha(120),
                egui::Stroke::new(1.5, egui::Color32::WHITE),
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
    fn open_record(
        &mut self,
        ctx: &egui::Context,
        frame: &mut eframe::Frame,
        rec: &ferrolite_catalog::ImageRecord,
    ) {
        self.maybe_regen_on_leave(ctx, frame);
        if let Some(old) = self.state.viewer.as_ref() {
            let old_id = old.image_id;
            old.cancel_loads();
            self.cancel_viewer_tiles(frame, old_id);
        }
        self.state.open_image_in_viewer(rec);
        self.module = crate::module::Module::Develop;
        ctx.request_repaint();
    }

    /// Increment the inflight counter and spawn an ops-persist job. Both call
    /// sites (apply_edit commit branch + undo/redo handler) must go through here
    /// so the counter stays balanced with the single `OpsSaved` event each job emits.
    fn persist_ops(
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

    /// Drain "Regenerate thumbnail" requests queued by the grid context menu.
    /// Runs once per frame where the GPU render state is available; each image
    /// loads its edit stack from its `.xmp` sidecar inside the Background job
    /// (missing/malformed → identity, i.e. a color-managed unedited thumbnail).
    fn drain_thumb_regen_requests(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
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
                id,
                path,
                rec.kind,
                cam,
                crate::develop::thumb_regen::RegenStackSource::Sidecar,
            );
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

        let diag_t0 = crate::diag::enabled().then(std::time::Instant::now);

        if crate::diag::enabled() && ctx.input(|i| i.key_pressed(egui::Key::F9)) {
            self.diag.toggle_overlay();
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

        // Drain job results into state; upload textures for ThumbReady events and
        // build the viewer's rung-1 VirtualTexture for PreviewReady events.
        //
        // Texture uploads are capped at MAX_THUMB_UPLOADS_PER_FRAME per frame so
        // a burst of finished thumbnails (bulk generation) can't blow the frame
        // budget. First flush any backlog stashed on a previous frame, then drain
        // the channel; overflow decoded thumbnails are stashed for next frame.
        let mut uploads_this_frame = 0usize;
        {
            // Drain the stashed backlog first (FIFO) up to the per-frame budget.
            let take = self
                .state
                .pending_uploads
                .len()
                .min(MAX_THUMB_UPLOADS_PER_FRAME);
            if take > 0 {
                let backlog: Vec<(i64, Vec<u8>, u32, u32)> =
                    self.state.pending_uploads.drain(..take).collect();
                for (id, rgba, w, h) in backlog {
                    self.state.upload_thumbnail(ctx, id, rgba, w, h);
                    uploads_this_frame += 1;
                }
                self.state.dirty = true;
            }
        }
        let mut ingest_done = false;
        let mut events_this_frame = 0usize;
        while let Ok(event) = self.state.rx.try_recv() {
            events_this_frame += 1;
            match &event {
                crate::events::AppEvent::PreviewReady { image_id, linear } => {
                    self.apply_preview_ready(frame, *image_id, linear);
                    self.state.dirty = true;
                    continue;
                }
                crate::events::AppEvent::FullDecoded {
                    image_id,
                    image,
                    color_profile,
                } => {
                    self.apply_full_decoded(frame, ctx, *image_id, image, color_profile);
                    self.state.dirty = true;
                    continue;
                }
                crate::events::AppEvent::FullFailed { image_id } => {
                    let image_id = *image_id;
                    // For RAW: if we never revealed (the color-managed raw render
                    // never built because the full decode failed), fall back to the
                    // embedded JPEG so an undecodable-full still shows *something*.
                    // This is the ONE place the JPEG may reach the screen for RAW.
                    let need_fallback = matches!(
                        self.state.viewer.as_ref(),
                        Some(v) if v.image_id == image_id
                            && v.kind == ferrolite_image::FileKind::Raw
                            && !v.loaded
                    );
                    if need_fallback && self.reveal_srgb_preview(frame, image_id) {
                        eprintln!(
                            "ferrolite: full decode failed for #{image_id}; showing embedded JPEG fallback"
                        );
                    }
                    // Mark the viewer idle so the repaint loop can stop (the decode
                    // error was already logged on the job thread). On the fallback
                    // path `reveal_srgb_preview` already set `idle`; this covers the
                    // already-loaded / no-fallback cases too.
                    if let Some(v) = self.state.viewer.as_mut() {
                        if v.image_id == image_id {
                            v.idle = true;
                        }
                    }
                    self.state.dirty = true;
                    continue;
                }
                crate::events::AppEvent::PreviewCacheHit { image_id, linear } => {
                    self.apply_preview_cache_hit(frame, *image_id, linear);
                    self.state.dirty = true;
                    ctx.request_repaint();
                    continue;
                }
                crate::events::AppEvent::PreviewCacheMiss { image_id } => {
                    self.apply_preview_cache_miss(*image_id);
                    self.state.dirty = true;
                    ctx.request_repaint();
                    continue;
                }
                crate::events::AppEvent::OpsLoaded { image_id, stack } => {
                    if let Some(v) = self.state.viewer.as_mut() {
                        if v.image_id == *image_id && !v.ops_loaded {
                            v.ops_loaded = true;
                            if !stack.is_identity() {
                                v.history =
                                    crate::develop::history::History::new(stack.clone(), 100);
                                self.set_preview_and_full(frame, stack.clone());
                            }
                        }
                    }
                    self.state.dirty = true;
                    continue;
                }
                crate::events::AppEvent::IngestDone => {
                    ingest_done = true;
                }
                crate::events::AppEvent::HistogramReady { image_id, bins } => {
                    if let Some(v) = self.state.viewer.as_mut() {
                        if v.image_id == *image_id {
                            if !bins.is_empty() {
                                v.histogram.bins = Some(bins.clone());
                            }
                            v.histogram.inflight = false;
                        }
                    }
                    ctx.request_repaint();
                }
                crate::events::AppEvent::ExportProgress {
                    image_id,
                    done,
                    total,
                } => {
                    if self
                        .state
                        .viewer
                        .as_ref()
                        .is_some_and(|v| v.image_id == *image_id)
                    {
                        self.state.warning = Some(format!("Exporting… {done}/{total}"));
                    }
                    ctx.request_repaint();
                    continue;
                }
                crate::events::AppEvent::ExportFinished {
                    image_id: _,
                    ok,
                    message,
                } => {
                    // Surface success + warnings, or the failure, in the status bar.
                    let _ = ok;
                    self.state.warning = Some(message.clone());
                    ctx.request_repaint();
                    continue;
                }
                _ => {}
            }
            if let Some((id, rgba, w, h)) = self.state.apply(event) {
                if uploads_this_frame < MAX_THUMB_UPLOADS_PER_FRAME {
                    self.state.upload_thumbnail(ctx, id, rgba, w, h);
                    uploads_this_frame += 1;
                } else {
                    // Over budget this frame — stash for a subsequent frame and
                    // mark the id awaiting upload so a re-request while it waits
                    // does not re-submit/re-push (re-submit storm guard).
                    self.state.thumb_uploading.insert(id);
                    self.state.pending_uploads.push((id, rgba, w, h));
                }
            }
            self.state.dirty = true;
        }
        // If a texture-upload backlog remains, schedule another frame so it
        // flushes over subsequent frames (each capped) instead of all at once.
        if !self.state.pending_uploads.is_empty() {
            ctx.request_repaint();
        }
        let repaint_forced = !self.state.pending_uploads.is_empty();
        crate::diag::add_events(events_this_frame);
        crate::diag::add_uploads(uploads_this_frame);
        self.drain_thumb_regen_requests(ctx, frame);
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
                    "v0.0.1",
                    export_enabled,
                    viewer_open,
                    &self.state.settings.keymap,
                    can_undo,
                    can_redo,
                    self.state.settings.show_histogram,
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
                            self.state.warning = Some("Added to export queue.".to_string());
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
                        self.state.warning = Some("Preview cache purged.".to_string());
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
            // Esc closes the viewer. Cancel its in-flight decode + tile jobs first so a
            // closed image's work stops competing with whatever is opened next.
            if self
                .state
                .settings
                .keymap
                .pressed(ctx, crate::settings::keymap::Action::CloseViewer)
            {
                self.maybe_regen_on_leave(ctx, frame);
                if let Some(v) = self.state.viewer.take() {
                    v.cancel_loads();
                    self.cancel_viewer_tiles(frame, v.image_id);
                    self.module = crate::module::Module::Library;
                }
            }

            // Enter opens the selected image in the viewer (library grid only, no
            // viewer already open, exactly one image selected). Suppressed while a
            // modal is up or a text field holds focus (so a future search box's
            // Enter won't pop the viewer).
            if self.module.is_library()
                && self.state.viewer.is_none()
                && !ctx.wants_keyboard_input()
                && self
                    .state
                    .settings
                    .keymap
                    .pressed(ctx, crate::settings::keymap::Action::OpenImage)
            {
                if let Some(sel_id) = self.state.selected {
                    if let Some(rec) = self.state.images.iter().find(|r| r.id == sel_id).cloned() {
                        self.open_record(ctx, frame, &rec);
                    }
                }
            }

            // F1 opens the Help modal. Global: works regardless of module/viewer
            // state, but suppressed while a text field holds focus or another
            // modal is up (consistent with the neighboring shortcuts here).
            if !ctx.wants_keyboard_input()
                && self
                    .state
                    .settings
                    .keymap
                    .pressed(ctx, crate::settings::keymap::Action::OpenHelp)
            {
                self.show_help = true;
            }

            // Ctrl+, opens the Settings window. Global, same gating as Help
            // above. Since this whole region is gated on `!self.modal_active()`
            // (which now includes `show_settings`), the shortcut only opens
            // Settings when no modal is already up — acceptable, since a
            // modal already on screen has its own dismissal path.
            if !ctx.wants_keyboard_input()
                && self
                    .state
                    .settings
                    .keymap
                    .pressed(ctx, crate::settings::keymap::Action::OpenSettings)
            {
                self.show_settings = true;
            }

            // Ctrl/Cmd+A toggles select-all over the current (filtered) grid rows.
            // Library grid only (no viewer, no modal, no text field focused).
            if self.module.is_library()
                && self.state.viewer.is_none()
                && !ctx.wants_keyboard_input()
                && self
                    .state
                    .settings
                    .keymap
                    .pressed(ctx, crate::settings::keymap::Action::SelectAll)
            {
                self.state.toggle_select_all();
            }

            // Keyboard metadata commands: rating 0–5 (I = Pick, O = Reject), all as
            // toggles. In Library (no viewer) they apply to the grid selection; in
            // Develop or Library+viewer they apply to the open viewer image.
            if !ctx.wants_keyboard_input() {
                use ferrolite_image::{Flag, Rating};

                // --- 1. Read key intent ---
                enum KeyIntent {
                    Rating(u8),
                    Flag(Flag),
                }
                // Routed through the keymap (one lookup per Action, each its own
                // `ctx.input` call inside `Keymap::pressed`); priority order (ratings
                // 0..5, then Pick, then Reject) and "one intent per frame" preserved.
                use crate::settings::keymap::Action;
                let km = &self.state.settings.keymap;
                let rating_actions = [
                    Action::Rating0,
                    Action::Rating1,
                    Action::Rating2,
                    Action::Rating3,
                    Action::Rating4,
                    Action::Rating5,
                ];
                let mut intent = None;
                for (n, action) in rating_actions.into_iter().enumerate() {
                    if km.pressed(ctx, action) {
                        intent = Some(KeyIntent::Rating(n as u8));
                        break;
                    }
                }
                let intent = intent.or_else(|| {
                    if km.pressed(ctx, Action::FlagPick) {
                        Some(KeyIntent::Flag(Flag::Pick))
                    } else if km.pressed(ctx, Action::FlagReject) {
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
                            KeyIntent::Rating(n) => crate::metadata::MetaEdit::SetRating(
                                Rating::new(crate::metadata::toggle_rating(cur_rating, n)),
                            ),
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

                // Q toggles export-queue membership for the same target image used
                // by the rating/flag intents above (grid selection in Library-no-
                // viewer, else the open viewer image). Kept as a parallel check
                // rather than folded into `KeyIntent` so the rating/flag toggle
                // logic above is untouched.
                if self
                    .state
                    .settings
                    .keymap
                    .pressed(ctx, crate::settings::keymap::Action::AddToQueue)
                {
                    let target_id = if self.module.is_library() && self.state.viewer.is_none() {
                        self.state.selected
                    } else {
                        self.state.viewer.as_ref().map(|v| v.image_id)
                    };
                    if let Some(target_id) = target_id {
                        let was_queued = self.state.queue_contains(target_id);
                        self.state.queue_toggle(target_id);
                        self.state.warning = Some(if was_queued {
                            "Removed from export queue.".to_string()
                        } else {
                            "Added to export queue.".to_string()
                        });
                    }
                }
            }

            // Left/Right move between images while viewing (Develop), non-cyclic.
            if self.module == crate::module::Module::Develop
                && self.state.viewer.is_some()
                && !ctx.wants_keyboard_input()
            {
                let km = &self.state.settings.keymap;
                let dir = if km.pressed(ctx, crate::settings::keymap::Action::NextImage) {
                    Some(crate::viewer::nav::Step::Next)
                } else if km.pressed(ctx, crate::settings::keymap::Action::PrevImage) {
                    Some(crate::viewer::nav::Step::Prev)
                } else {
                    None
                };
                if let Some(dir) = dir {
                    self.navigate_step(ctx, frame, dir);
                }

                // Before/After: `\` shows the empty (before) stack while held, and
                // reverts to the live stack on release.
                //
                // NOTE (Task 2.3 keymap routing, deliberate behavior change): the
                // dispatch for this refactor explicitly routes `HoldBeforePeek`
                // through `Keymap::held` (level-triggered), matching the keymap's
                // own design — `Action::HoldBeforePeek` is documented as "Hold to
                // show original (before)" and `held()` exists specifically for this
                // action. The pre-refactor code actually toggled `before_after` on
                // each `key_pressed` (an edge-triggered latch), which contradicted
                // its own doc comment in `viewer/mod.rs` calling it "momentary".
                // This routes it to the momentary/hold behavior the naming always
                // implied: `before_after` now directly mirrors "is the chord held",
                // only re-evaluating the preview on an actual state transition
                // (press or release), not every frame it's held.
                let hold_before = self
                    .state
                    .settings
                    .keymap
                    .held(ctx, crate::settings::keymap::Action::HoldBeforePeek);
                let before_after_changed = self
                    .state
                    .viewer
                    .as_ref()
                    .is_some_and(|v| v.before_after != hold_before);
                if before_after_changed {
                    if let Some(v) = self.state.viewer.as_mut() {
                        v.before_after = hold_before;
                    }
                    let stack = self.state.viewer.as_ref().unwrap().op_stack.clone();
                    self.set_preview_and_full(frame, stack); // re-evaluates with before_after
                }

                // Undo / Redo. Redo also accepts the Ctrl+Y alias in addition to the
                // keymap's bound chord (defaults to Ctrl+Shift+Z) — kept for users
                // used to the common Ctrl+Y redo convention.
                let km = &self.state.settings.keymap;
                let undo = km.pressed(ctx, crate::settings::keymap::Action::Undo);
                let ctrl_y = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Y));
                let redo = km.pressed(ctx, crate::settings::keymap::Action::Redo) || ctrl_y;
                if undo || redo {
                    self.apply_undo_redo(ctx, frame, undo);
                }

                // Toggle before/after SPLIT-compare (draggable divider), mirroring
                // the `develop_filter_bar` toggle button's click handling exactly:
                // flips `split_compare` and, only when turning it on, resets
                // `split_pos` to center. (Auto-fit-at-1:1 is a later task — not
                // added here.)
                if self
                    .state
                    .settings
                    .keymap
                    .pressed(ctx, crate::settings::keymap::Action::ToggleSplitCompare)
                {
                    self.toggle_split_compare();
                }
            }
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
            if let Some(v) = self.state.viewer.as_mut() {
                v.crop_active = false; // re-armed by the open Geometry section
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
                            outcome = Some(crate::develop::adjustment_panel::show(
                                ui,
                                &mut self.state,
                                working_space,
                            ));
                        });
                });
            if let Some(outcome) = outcome {
                if let Some(ws) = outcome.working_space {
                    self.apply_working_space(ctx, frame, ws);
                }
                if let Some(o) = outcome.edit {
                    self.apply_edit(ctx, frame, o.kind, o.stack, o.commit);
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
                                if let Some(b) = self.state.batch.as_ref() {
                                    b.cancel_all();
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
                                self.set_preview_and_full(frame, stack);
                            }
                            self.crop_active_prev = crop_active;
                        }
                        self.drive_viewer(ui, frame);
                        if self.state.settings.show_histogram {
                            self.draw_histogram_overlay(ui);
                        }
                        // Crop overlay: shown while the Geometry section is open.
                        // Gather all viewer data into locals BEFORE calling apply_edit
                        // (which needs &mut self) — mirrors the panel-outcome pattern.
                        if self
                            .state
                            .viewer
                            .as_ref()
                            .map(|v| v.crop_active)
                            .unwrap_or(false)
                        {
                            let (stack, dims, view, viewport) = {
                                let v = self.state.viewer.as_ref().unwrap();
                                (
                                    v.op_stack.clone(),
                                    v.image_dims.unwrap_or((1, 1)),
                                    v.view,
                                    v.viewport,
                                )
                            };
                            let image_rect = crate::viewer::image_screen_rect(
                                ui.min_rect(),
                                dims,
                                view,
                                viewport,
                            );
                            if let Some(o) =
                                crate::develop::crop_overlay::show(ui, image_rect, &stack, dims)
                            {
                                self.apply_edit(ctx, frame, o.kind, o.stack, o.commit);
                            }
                        }
                        // Loupe context-menu widget covers the whole canvas; while
                        // cropping it must NOT be registered, or it competes with the
                        // crop overlay for input. Gate it on `!crop_active`.
                        let ctx_menu_id = self
                            .state
                            .viewer
                            .as_ref()
                            .filter(|v| !v.crop_active)
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
            if crate::settings::ui::show(ctx, &mut open, &mut self.state.settings) {
                self.mark_settings_dirty();
            }
            self.show_settings = open;
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
            egui::Stroke::new(1.0, theme::BORDER_STRONG),
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
            };
            let stats = self.state.jobs.stats();
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
            }
            if crate::diag::overlay_enabled() && self.diag.overlay_visible {
                if let Some(snap) = self.diag.last_snapshot() {
                    crate::diag::draw_overlay(ctx, snap);
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

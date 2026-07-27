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
    /// One-shot guard: set `true` the first frame that has a valid render state
    /// (pipelines pre-warmed), after kicking off the initial display-profile
    /// detect. Ensures the startup detect fires exactly once.
    did_display_detect: bool,
    /// The Develop tool/tab registry (design §4): base adjustment tabs + the
    /// ordered canvas tools shown in the palette. Built once here; read in
    /// Tasks 10-11 to render the palette/tab bar/canvas overlay.
    tool_registry: crate::develop::tool::DevelopToolRegistry,
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
        ctx: &egui::Context,
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
        // Invalidate the mask-overlay's bounded input cache: it was derived from
        // the previous preview source and no longer applies.
        v.mask_overlay_input = None;

        // RAW: do NOT reveal the embedded JPEG. Keep the spinner up until the
        // color-managed raw render is built at full-decode (`apply_full_decoded`),
        // so the reveal comes from the same pipeline as the sparse full — a
        // sharpness-only ramp with no color/tone shift.
        if is_raw {
            return;
        }

        // Standard: the preview IS the full-resolution image — reveal it now.
        // `full_res = true`: this is the genuine cold full-res JPEG decode, so
        // it is eligible to warm-cache.
        let revealed = self.reveal_srgb_preview(frame, image_id, true);
        if !revealed {
            return;
        }

        // Preview-cache write-back (Phase 3): on a qualifying Standard open, cache
        // the identity color-managed 2048px render so a later open of the same JPG
        // reveals instantly from disk (Task 6's read path). `preview_source` is
        // already display-linear sRGB, so the write-back matrix is identity (Task
        // 5). Gated on a default op stack + a genuine cache MISS
        // (`v.cache_write_back`, set by the preview-cache read) so an edited image
        // or a re-open with the entry already on disk never re-encodes.
        //
        // Reuse the retained `preview_source` Arc as the payload (no second
        // O(pixels) copy); the Background job does the key stat + encode + disk IO
        // off the UI thread (CLAUDE.md rule 1).
        let write_back = self.state.viewer.as_ref().and_then(|v| {
            if v.image_id != image_id {
                return None;
            }
            crate::develop::preview_cache::should_write_back(&v.op_stack, v.cache_write_back).then(
                || {
                    (
                        v.path.clone(),
                        v.op_stack.clone(),
                        v.color_profile.clone(),
                        v.preview_source.clone(),
                    )
                },
            )
        });
        if let Some((path, op_stack, color_profile, Some(render))) = write_back {
            crate::develop::preview_cache::spawn_cache_write(
                &self.state.jobs,
                std::sync::Arc::clone(&self.state.preview_store),
                &self.state.tx,
                ctx,
                path,
                op_stack,
                self.state.working_space,
                color_profile,
                render,
                crate::develop::preview_cache::standard_writeback_matrix(),
                ferrolite_previews::DEFAULT_CACHE_CAP_BYTES,
                image_id,
            );
        }
    }

    /// Build the rung-1 preview `VirtualTexture` from the retained sRGB
    /// `preview_source` via one `sRGB→working` color pass, install the holder,
    /// fit the view, and mark the viewer `loaded` + `idle`. Shared by the
    /// Standard preview reveal (`apply_preview_ready`), the Standard/RAW
    /// preview-cache-hit reveal (`apply_preview_cache_hit`), and the RAW
    /// full-decode-failure fallback (`FullFailed`). Returns `true` on success,
    /// `false` if a prerequisite (GPU / viewer / source) is missing.
    ///
    /// `full_res` tells `warm_insert_display` whether `preview_source` is
    /// genuinely full-resolution (a cold Standard decode) or a downscaled
    /// stand-in (the 2048px preview-cache render, or RAW's embedded-JPEG
    /// failure fallback) — only a full-resolution reveal may be warm-cached,
    /// otherwise a later warm hit could get stuck serving a low-res texture
    /// as if it were the sharp 1:1 tier (see `warm_insert_display`).
    fn reveal_srgb_preview(
        &mut self,
        frame: &eframe::Frame,
        image_id: i64,
        full_res: bool,
    ) -> bool {
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
            self.apply_display_tail(&gpu, vp);
            ferrolite_vt::VirtualTexture::single_from_texture(
                &gpu,
                converted.texture.clone(),
                (converted.width, converted.height),
                &vp.pipelines,
            )
        };

        if !self.install_preview_holder(frame, image_id, vt, dims) {
            return false;
        }
        self.warm_insert_display(frame, image_id, full_res);
        true
    }

    /// Shared holder-install tail for a rung-1 preview reveal: fit the view to
    /// `dims`, mark `image_dims`/`loaded`/`idle`, allocate fresh present
    /// buffers, and insert the `ViewerGpu` callback resource wrapping `vt`.
    /// Shared by `reveal_srgb_preview` (a freshly color-converted texture) and
    /// `try_warm_reveal` (a warm-cache-hit texture) so there is exactly one
    /// holder-install path (DRY). Returns `false` if the GPU render state is
    /// unavailable or the viewer has since navigated away from `image_id`.
    fn install_preview_holder(
        &mut self,
        frame: &eframe::Frame,
        image_id: i64,
        vt: ferrolite_vt::VirtualTexture,
        dims: (u32, u32),
    ) -> bool {
        let Some(rs) = frame.wgpu_render_state() else {
            return false;
        };
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
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
        // This tier has no tier-2 to wait for (Standard preview IS full-res, the
        // RAW fallback has given up on the full, and a warm-cache display hit
        // skips the decode ladder entirely for this open) — go idle so the
        // repaint loop does not spin.
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

    /// If the open viewer's `(image_id, op_stack_hash)` is warm in the cache,
    /// install its cached render immediately and return true. A `Display` hit
    /// installs the rung-1 preview VT (instant fit) and then lets the SHARP tier
    /// stream in behind it exactly as a normal open would — the instant-fit win
    /// without giving up 1:1 sharpness (the warm texture just replaces the cold
    /// preview→full crossfade's preview). A miss returns false and the normal
    /// ladder runs.
    ///
    /// The tail here (see the flag-setting after `install_preview_holder`) skips
    /// only the now-redundant tier-1 preview reveal while keeping the tier-2
    /// decode chain alive, and clears `idle` so the drive loop streams the full
    /// tier to convergence, mirroring `apply_preview_cache_hit`. (A future Task-6
    /// `Full` hit installs the 1:1 tier itself and would legitimately skip tier-2;
    /// that path is not reachable yet, so a `Full` hit is treated as its display.)
    fn try_warm_reveal(&mut self, frame: &eframe::Frame, image_id: i64) -> bool {
        let Some(rs) = frame.wgpu_render_state() else {
            return false;
        };
        let key = match self.state.viewer.as_ref() {
            Some(v) if v.image_id == image_id => crate::develop::cache::CacheKey {
                image_id,
                op_stack_hash: v.op_stack_hash(),
            },
            _ => return false,
        };
        let hit = self.state.warm_cache.get(key);
        let display = match hit {
            crate::develop::cache::WarmHit::Miss => return false,
            crate::develop::cache::WarmHit::Display(d) => d,
            // Full-tier install is Task 5/6; for Phase A treat Full as its display.
            crate::develop::cache::WarmHit::Full { display, .. } => display,
        };
        let Some(tex) = display.tex.clone() else {
            return false;
        };
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
        let vt = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            self.apply_display_tail(&gpu, vp);
            ferrolite_vt::VirtualTexture::single_from_texture(
                &gpu,
                tex,
                display.dims,
                &vp.pipelines,
            )
        };
        if !self.install_preview_holder(frame, image_id, vt, display.dims) {
            return false;
        }
        if let Some(v) = self.state.viewer.as_mut() {
            if v.image_id == image_id {
                // The cached display texture serves the instant fit/preview reveal.
                // For RAW the SHARP tier must still stream in behind it so 1:1 zoom
                // sharpens exactly as a normal open does (the sparse full pyramid) —
                // RAW has no separate full-res display tier. For Standard the cached
                // display texture IS already the full-resolution image (warm-cached
                // ONLY when full-res, see `warm_insert_display`'s `full_res` gate),
                // so there is no sharper tier left to stream. Therefore, mirroring
                // `apply_preview_cache_hit`:
                //  - `warm_revealed` skips the now-redundant tier-1 preview (the RAW
                //    embedded-JPEG reveal) AND the redundant Standard heavy JPEG
                //    re-decode in `drive_viewer`; RAW's tier-2 pyramid decode chain
                //    there still runs (ungated — it must, RAW has no other route to
                //    1:1 sharpness).
                //  - Mark the disk preview-cache read already-resolved so the
                //    debounced tier-2 step fires DIRECTLY, without the disk read
                //    re-installing a lower-res 2048px holder over the warm texture,
                //    and don't write back (a warm RAM hit says nothing new about the
                //    on-disk entry, which this session's prior reveal already
                //    settled).
                //  - Clear `idle` so the drive loop stays alive one more beat: RAW
                //    returns to idle on pyramid convergence (`apply_pyramid_ready`);
                //    Standard has nothing left to converge on, so `drive_viewer`'s
                //    gated branch sets `idle` back to `true` immediately once the
                //    debounce elapses — NOT a per-frame spin either way.
                // (A future Task-6 FULL hit installs the 1:1 tier itself and will
                // instead skip tier-2; that path is not reachable yet.)
                v.warm_revealed = true;
                v.idle = false;
                v.cache_read_requested = true;
                v.cache_resolved = true;
                v.cache_write_back = false;
            }
        }
        true
    }

    /// Record the just-installed rung-1 display texture into the warm cache so a
    /// later re-open of this `(image_id, op_stack_hash)` reveals instantly.
    ///
    /// `full_res` MUST be `true` only when the just-installed texture is the
    /// genuine full-resolution reveal (a cold Standard JPEG decode, or a RAW
    /// full decode). It is `false` for a downscaled stand-in (the 2048px
    /// preview-cache render, or RAW's embedded-JPEG failure fallback) — those
    /// are NOT cached, because a later warm Standard hit skips the redundant
    /// full-res re-decode entirely (see the `!v.warm_revealed` gate in
    /// `drive_viewer`) and would otherwise get stuck serving the downscaled
    /// texture as if it were the sharp 1:1 tier.
    fn warm_insert_display(&mut self, frame: &eframe::Frame, image_id: i64, full_res: bool) {
        if !full_res {
            return;
        }
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let key = match self.state.viewer.as_ref() {
            Some(v) if v.image_id == image_id => crate::develop::cache::CacheKey {
                image_id,
                op_stack_hash: v.op_stack_hash(),
            },
            _ => return,
        };
        let (tex, dims) = {
            let renderer = rs.renderer.read();
            let Some(g) = renderer.callback_resources.get::<viewer::ViewerGpu>() else {
                return;
            };
            if g.image_id != image_id {
                return;
            }
            match (g.preview.single_texture_arc(), g.preview.single_dims()) {
                (Some(t), Some(d)) => (t, d),
                _ => return,
            }
        };
        // Rgba16Float rung-1 texture = 8 B/px.
        let bytes = dims.0 as u64 * dims.1 as u64 * 8;
        self.state.warm_cache.set_open(Some(key));
        self.state.warm_cache.insert_display(
            key,
            crate::develop::cache::DisplayEntry {
                tex: Some(tex),
                dims,
                bytes,
            },
        );
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

    /// Normalized WhiteBalance temperature of the open viewer's current op stack
    /// (0.0 = as-shot/identity when there is no WB op or no viewer).
    fn current_wb_temp(&self) -> f32 {
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
    fn camera_to_working(&self, temp: f32) -> [[f32; 3]; 3] {
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
        let cam = self.camera_to_working(self.current_wb_temp());
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);

        // RAW rung-1 reveal render (Approach A): run the demosaiced camera-native
        // `image` through the op stack with the SAME camera→working matrix + op
        // stack as the sparse full below, so the preview→full swap is a
        // sharpness-only ramp with no color/tone shift. Build the preview
        // `EditPipeline` ONCE here and retain it (`v.preview_edit`) for reuse by
        // `set_preview_and_full` — never compiled per edit (CLAUDE.md rule 2).
        // Standard images never reach `apply_full_decoded`.
        // Build the camera-native reveal source ONCE for RAW. The full-res `Arc`
        // (`raw_preview_source`) is retained as `v.raw_preview_source` (consumed
        // by the split-compare "before" rebuild and the preview-cache
        // write-back below, which persists a NOT-viewport-bounded 2048px JPEG
        // to disk and must not be quality-degraded by the current window size)
        // AND reused as the write-back payload — the demosaiced buffer is never
        // memcpy'd a second time onto the UI thread for that purpose.
        //
        // Full-res reveal: the reveal `EditPipeline` runs the FULL-resolution
        // demosaiced image through the op chain, so the rung-1 preview is
        // dims-consistent with the full VT and with every preview-tier consumer
        // (display transform, GPU histogram, the before/after split compare, and
        // the retained `preview_edit` reused for live edits). A prior attempt to
        // render this at viewport resolution saved ~674 ms but made the preview
        // tier a low-res proxy whose logical size ≠ its texture size, which broke
        // the zoom/LOD transform, the split compare, the histogram, and edited-
        // preview sharpness — so it was reverted.
        // One owned copy of the full-res buffer, shared (by `Arc` refcount bump,
        // NOT a second O(pixels) memcpy) between the RAW reveal source here and
        // the pyramid job below — replacing what were two separate `image.clone()`s
        // (~400 MB each for a 24 MP frame). This was a major driver of the
        // develop-scroll RSS high-water mark (memory profiling, 2026-07).
        let image_arc = std::sync::Arc::new(image.clone());
        let raw_preview_source: Option<std::sync::Arc<ferrolite_image::LinearRgbaF32>> =
            is_raw.then(|| std::sync::Arc::clone(&image_arc));
        let raw_preview: Option<(std::sync::Arc<wgpu::Texture>, (u32, u32))> = if let Some(src) =
            raw_preview_source.as_ref()
        {
            match self.state.viewer.as_mut() {
                Some(v) if v.image_id == image_id => {
                    // The full-res reveal source is retained as `v.raw_preview_source`
                    // (read by `ensure_before_view`'s split-compare rebuild and reused
                    // as the preview-cache write-back payload below) AND fed to the
                    // reveal `EditPipeline` here — one full-res buffer, no second copy.
                    v.raw_preview_source = raw_preview_source.clone();
                    let ctx_arc =
                        std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
                    let mut ep = ferrolite_pipeline::EditPipeline::new(
                        ctx_arc.clone(),
                        src,
                        v.op_stack.clone(),
                        cam,
                    );
                    // Bind any lens bake already present (e.g. a re-open that
                    // baked before this decode landed) so the initial preview
                    // isn't uncorrected (I1). Usually None at fresh open; the
                    // `LensBaked` handler pushes the bake once it completes.
                    if let Some(w) = v.lens_warp.as_ref() {
                        ep.set_warp(ferrolite_pipeline::WarpGridTexture::upload(&ctx_arc, w));
                    }
                    ep.set_lens_uniform(ferrolite_pipeline::lens_uniform(
                        v.op_stack.lens_correction().as_ref(),
                        v.lens_warp.is_some(),
                    ));
                    if let Some(vg) = v.lens_vignette.as_ref() {
                        ep.set_vignette(ferrolite_pipeline::VignetteTexture::upload(&ctx_arc, vg));
                    }
                    // Mode-aware vignette (MV2): profile LUT lerp when a bake is
                    // bound, else the lens-free parametric manual gain — so a
                    // persisted manual-vignette op (lens_id=None, no bake) still
                    // applies on open. Both uniforms are pushed as a pair.
                    let (vig_amount, vig_manual) = crate::develop::vignette_mode::vig_pair(
                        v.op_stack.lens_correction().as_ref(),
                        v.lens_vignette.is_some(),
                    );
                    ep.set_vig_amount(vig_amount);
                    ep.set_vig_manual(vig_manual);
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

        // Build ONLY the rung-1 reveal preview VT here (cheap). The sparse full VT
        // (and the GPU edit pyramid) are built off the UI thread — both are CPU
        // box-downsample heavy (~1.2 s combined) and were the open freeze
        // (CLAUDE.md rule 1). They arrive later via `AppEvent::PyramidReady`, which
        // `apply_pyramid_ready` installs into the holder (see the Background job
        // submitted below). Until then the holder carries `full: None` and the
        // color-correct reveal is shown.
        let preview_vt = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            // Route through the current display state so an active monitor LUT
            // stays applied across image opens instead of reverting to sRGB.
            self.apply_display_tail(&gpu, vp);
            raw_preview.as_ref().map(|(tex, dims)| {
                ferrolite_vt::VirtualTexture::single_from_texture(
                    &gpu,
                    std::sync::Arc::clone(tex),
                    *dims,
                    &vp.pipelines,
                )
            })
        };

        // Install the reveal-preview holder with `full: None`. For RAW the rung-1
        // preview IS the reveal render, so install a fresh holder (there is no JPEG
        // holder — `apply_preview_ready` kept the spinner up). Replaces any stale
        // holder from a superseded image. The sparse full VT is installed later by
        // `apply_pyramid_ready` when the off-thread pyramid job completes; the full
        // VT MUST NOT produce raw-camera-native tiles until the edit producer
        // exists, so `set_producing(true)` is deferred to that handler.
        let mut preview_installed = false;
        {
            let mut renderer = rs.renderer.write();
            if let Some(preview) = preview_vt {
                let holder_gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
                // Placeholder (1,1) present-buffer size: `drive_viewer`'s per-frame
                // resize corrects it to the canvas's physical viewport before paint.
                let present =
                    ferrolite_vt::PresentBuffers::new(&holder_gpu, (1, 1), rs.target_format);
                let present_alpha = holder_gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vt-present-alpha"),
                    size: 32,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                renderer.callback_resources.insert(viewer::ViewerGpu {
                    ctx: holder_gpu,
                    preview,
                    full: None,
                    preview_before: None,
                    image_id,
                    present,
                    present_alpha,
                    blit_bind_front: None,
                });
                preview_installed = true;
            } else if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                // Non-RAW defensive path (Standard never submits a tier-2 decode);
                // reuse an existing matching holder. The pyramid job still runs and
                // `apply_pyramid_ready` installs the full VT into it.
                if g.image_id == image_id {
                    preview_installed = true;
                }
            }
        }

        if preview_installed {
            if let Some(v) = self.state.viewer.as_mut() {
                if v.image_id == image_id {
                    // Step 3 reveal: the rung-1 raw render is now on screen, so
                    // drop the spinner. (For RAW `loaded` was held false in
                    // `apply_preview_ready` until this color-correct reveal.)
                    v.loaded = true;
                    // NOTE: `full_ready` stays FALSE here. The sparse full VT and
                    // its edit producer are built off the UI thread and installed
                    // by `apply_pyramid_ready` on `AppEvent::PyramidReady`; only
                    // then is the full tier ready. Until the swap the color-correct
                    // reveal (installed above) is what the viewer shows.
                    //
                    // The full tier's dimensions (uprighted; full-res GPU RCD for
                    // RGGB, else QuadBin half-res) are the reveal render's dims too.
                    // Fit to them; fall back to the image's own size if the canvas
                    // has not painted yet (the user has not interacted at open time).
                    let full_dims = (image.width, image.height);
                    v.image_dims = Some(full_dims);
                    let viewport = if v.viewport.0 > 0.0 && v.viewport.1 > 0.0 {
                        v.viewport
                    } else {
                        (full_dims.0 as f32, full_dims.1 as f32)
                    };
                    v.view = ferrolite_vt::ViewTransform::fit(full_dims, viewport);
                }
            }

            // Both full-res pyramids (the sparse-VT CPU tile source and the
            // GPU-resident edit pyramid) are CPU box-downsample heavy (~1.2 s
            // combined) — build them on a `ferrolite-jobs` Background worker rather
            // than the UI thread (CLAUDE.md rule 1; this was the open freeze). They
            // need the FULL-res `image`; reuse the single shared `image_arc`
            // (an `Arc` refcount bump, NOT a second ~400 MB clone) built above.
            // `GpuContext` is `Send + Sync`
            // (Arc device/queue handles), as are `PyramidTileSource` and
            // `GpuPyramidSource`, so both build off-thread and are delivered over
            // the channel; `apply_pyramid_ready` installs them + starts producing.
            let image_full = std::sync::Arc::clone(&image_arc);
            let gpu_job = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
            let tx = self.state.tx.clone();
            let repaint = ctx.clone();
            let pyramid_handle =
                self.state
                    .jobs
                    .submit(ferrolite_jobs::Priority::Background, move |cancel| {
                        if cancel.is_cancelled() {
                            return;
                        }
                        // Attribute this job's large in-flight buffer (full-res linear f32) to
                        // the memory overlay for its lifetime. Gated: zero cost when off.
                        let _inflight = crate::diag::enabled().then(|| {
                            crate::diag_mem::track_inflight_pyramid(crate::diag_mem::linear_bytes(
                                image_full.width,
                                image_full.height,
                            ))
                        });
                        let tile_source: std::sync::Arc<
                            dyn ferrolite_vt::TileSource + Send + Sync,
                        > = std::sync::Arc::new(ferrolite_vt::PyramidTileSource::new(
                            (*image_full).clone(),
                        ));
                        if cancel.is_cancelled() {
                            return;
                        }
                        let gpu_pyramid = std::sync::Arc::new(
                            ferrolite_pipeline::GpuPyramidSource::new(&gpu_job, &image_full),
                        );
                        if cancel.is_cancelled() {
                            return;
                        }
                        let _ = tx.send(crate::events::AppEvent::PyramidReady {
                            image_id,
                            tile_source,
                            gpu_pyramid,
                        });
                        repaint.request_repaint();
                    });
            // Store the handle so a later navigation (`cancel_loads`) can cancel
            // this Background pyramid build. Guard on `image_id` matching in case
            // a newer image already superseded this one between submit and now.
            if let Some(v) = self.state.viewer.as_mut() {
                if v.image_id == image_id {
                    v.pyramid_handle = Some(pyramid_handle);
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
        if preview_installed {
            // Snapshot the viewer inputs, then release the borrow before the
            // job submit (which borrows other `self.state` fields).
            let write_back = self.state.viewer.as_ref().and_then(|v| {
                if v.image_id != image_id {
                    return None;
                }
                crate::develop::preview_cache::should_write_back(&v.op_stack, v.cache_write_back)
                    .then(|| (v.path.clone(), v.op_stack.clone()))
            });
            // `apply_full_decoded` only runs for RAW opens (see the `is_raw.then`
            // guard on `raw_preview_source` above), so `raw_preview_source` is
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

        if preview_installed {
            // `full_res = true`: `apply_full_decoded` only runs for RAW, and
            // its rung-1 reveal render is always the full-resolution
            // demosaiced image (see the comment above), so it is always
            // eligible to warm-cache.
            self.warm_insert_display(frame, image_id, true);
        }
        self.mark_histogram_dirty();
    }

    /// Both full-res pyramids finished building off the UI thread (delivered by
    /// the Background job submitted in `apply_full_decoded`): the sparse-VT CPU
    /// tile source and the GPU-resident edit pyramid. This is the UI-thread tail
    /// that the freeze fix moved off-thread. It builds the sparse full
    /// `VirtualTexture` (needs the render state) plus the `Rc`-based
    /// `TileEditPipeline` and `EditTileProducer` (both `!Send`, so UI-thread only),
    /// installs the full VT into the existing holder, and only NOW starts producing
    /// (the full VT must not emit raw camera-native tiles before the producer
    /// exists). A stale result (user navigated away while the pyramids built) is
    /// dropped.
    fn apply_pyramid_ready(
        &mut self,
        frame: &eframe::Frame,
        image_id: i64,
        tile_source: &std::sync::Arc<dyn ferrolite_vt::TileSource + Send + Sync>,
        gpu_pyramid: &std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>,
    ) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        // Stale guard: viewer closed or a different image is now open.
        if self.state.viewer.as_ref().map(|v| v.image_id) != Some(image_id) {
            return;
        }
        // Compute camera→working BEFORE any exclusive `viewer` borrow below:
        // `camera_to_working` itself borrows `self.state.viewer` immutably.
        let cam = self.camera_to_working(self.current_wb_temp());
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);

        // Build the sparse full VT from the off-thread tile source (needs the
        // pre-warmed `ViewerPipelines`; the read lock is released before the write
        // install below).
        let full = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            // Keep an active monitor LUT applied across the install.
            self.apply_display_tail(&gpu, vp);
            ferrolite_vt::VirtualTexture::sparse(
                &gpu,
                std::sync::Arc::clone(tile_source),
                std::sync::Arc::clone(&self.state.jobs),
                VIEWER_TILE_BUDGET,
                &vp.pipelines,
            )
        };

        // Build the full-res edit producer from the off-thread GPU pyramid and
        // flip `full_ready`. The full VT tiles ALWAYS pass through camera→working
        // (the raw camera-native CPU path must never reach the working→display
        // tail), so the producer is attached unconditionally — identity stack =
        // unedited-but-color-managed.
        let version;
        {
            let Some(v) = self.state.viewer.as_mut() else {
                return;
            };
            if v.image_id != image_id {
                return;
            }
            v.pyramid = Some(std::sync::Arc::clone(gpu_pyramid));
            let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
            // Mode-aware vignette pair (MV2) so a persisted manual-vignette op
            // (lens_id=None → no bake) applies on open; the current lens bake (if
            // any) is threaded in — `None` is byte-identical to no correction.
            let (vig_amount, vig_manual) = crate::develop::vignette_mode::vig_pair(
                v.op_stack.lens_correction().as_ref(),
                v.lens_vignette.is_some(),
            );
            let tep = ferrolite_pipeline::TileEditPipeline::new(
                ctx_arc,
                std::sync::Arc::clone(gpu_pyramid),
                v.op_stack.clone(),
                cam,
                v.lens_warp.as_ref(),
                v.lens_vignette.as_ref(),
            );
            let mut producer = viewer::EditTileProducer::new(tep);
            producer.set_vig_amount(vig_amount);
            producer.set_vig_manual(vig_manual);
            v.edit_producer = Some(producer);
            v.full_ready = true;
            version = v.opstack_version.max(1);
        }

        // Install the full VT into the existing holder + start producing. Guard on
        // `image_id` again: the holder could have been replaced by a newer open
        // between the reads above and this write lock.
        let mut renderer = rs.renderer.write();
        if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
            if g.image_id == image_id {
                g.full = Some(full);
                if let Some(full) = g.full.as_mut() {
                    full.set_producing(true);
                    full.set_opstack_version(&g.ctx, version);
                }
            }
        }
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
                // Invalidate the mask-overlay's bounded input cache: it was derived
                // from the previous preview source and no longer applies.
                v.mask_overlay_input = None;
            }
            _ => return, // stale: viewer closed or a different image is open
        }
        // `full_res = false`: this is the 2048px disk preview-cache render
        // (RAW or Standard), not the full-resolution image — must not be
        // warm-cached as if it were the sharp 1:1 tier.
        let revealed = self.reveal_srgb_preview(frame, image_id, false);
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

    /// An off-thread lens bake (`develop::lens_bake::spawn_lens_bake`) finished
    /// (Spec 4.4, U7). Stores the fresh warp grid / vignette map / resolved name
    /// on the viewer, then rebuilds the full-res tile producer so it picks up
    /// the new grid/LUT (a lens bake ALWAYS changes the baked content — the
    /// producer must be discarded and rebuilt, the same as a geometry/halo
    /// change; there is no in-place "new bake, same shapes" case to special-case).
    ///
    /// Guarded on `image_id == current`: a bake for an image the user has since
    /// navigated away from is dropped here even if it slipped past the
    /// `lens_bake_handle` cancellation checkpoint in the job itself.
    fn apply_lens_baked(
        &mut self,
        frame: &eframe::Frame,
        image_id: i64,
        result: &crate::develop::lens_bake::LensBakeResult,
    ) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let cam = self.camera_to_working(self.current_wb_temp());
        let Some(v) = self.state.viewer.as_mut() else {
            return;
        };
        if v.image_id != image_id {
            return; // superseded: navigated away before the bake finished
        }
        v.lens_warp = result.warp.clone();
        v.lens_vignette = result.vignette.clone();
        v.lens_resolved_name = result.resolved_name.clone();
        v.lens_bake_handle = None;

        // Rebuild the full-res producer (if the pyramid exists yet) so it binds
        // the fresh grid/LUT. Mirrors the rebuild branch in `set_preview_and_full`.
        let shown = if v.before_after {
            ferrolite_pipeline::OpStack::default()
        } else {
            v.op_stack.clone()
        };

        // Push the fresh bake to the PREVIEW/fit tier too, so toggling or
        // adjusting a correction updates the fit-zoom image live (I1). The
        // before-view (identity `shown`) carries no lens op, so bind identity
        // there. Bake products are cheap GPU uploads here (already baked
        // off-thread by the job that produced this `result`). Built once and
        // reused by the full-res producer rebuild below (CLAUDE.md GPU rule).
        let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let _ = &ctx_arc; // used by the preview and/or full-res branch below
        if let Some(ep) = v.preview_edit.as_mut() {
            let lc = shown.lens_correction();
            if let Some(w) = v.lens_warp.as_ref() {
                ep.set_warp(ferrolite_pipeline::WarpGridTexture::upload(&ctx_arc, w));
            }
            ep.set_lens_uniform(ferrolite_pipeline::lens_uniform(
                lc.as_ref(),
                v.lens_warp.is_some(),
            ));
            if let Some(vg) = v.lens_vignette.as_ref() {
                ep.set_vignette(ferrolite_pipeline::VignetteTexture::upload(&ctx_arc, vg));
            }
            // Mode-aware vignette pair (MV2): a bake just landed, so
            // `has_vignette_lut` reflects the fresh `v.lens_vignette`.
            let (vig_amount, vig_manual) =
                crate::develop::vignette_mode::vig_pair(lc.as_ref(), v.lens_vignette.is_some());
            ep.set_vig_amount(vig_amount);
            ep.set_vig_manual(vig_manual);
            let img = ep.evaluate();
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == image_id {
                    g.preview
                        .update_single_from_texture(img.texture.clone(), (img.width, img.height));
                }
            }
        }
        if let Some(pyr) = v.pyramid.clone() {
            // Mode-aware vignette pair for the rebuilt full-res producer (MV2).
            let (vig_amount, vig_manual) = crate::develop::vignette_mode::vig_pair(
                shown.lens_correction().as_ref(),
                v.lens_vignette.is_some(),
            );
            let tep = ferrolite_pipeline::TileEditPipeline::new(
                ctx_arc.clone(),
                pyr,
                shown,
                cam,
                v.lens_warp.as_ref(),
                v.lens_vignette.as_ref(),
            );
            let mut producer = viewer::EditTileProducer::new(tep);
            producer.set_vig_amount(vig_amount);
            producer.set_vig_manual(vig_manual);
            v.edit_producer = Some(producer);
            v.opstack_version = v.opstack_version.wrapping_add(1);
            let version = v.opstack_version;
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
        if let Some(v) = self.state.viewer.as_mut() {
            v.idle = false; // wake the drive loop so producer tiles re-render
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
        // WB temp of the INCOMING stack (v.op_stack is updated below), so a WB
        // temp edit re-interpolates the dual-illuminant matrix this same frame.
        let temp = stack.white_balance().map(|w| w.temp).unwrap_or(0.0);
        let cam = self.camera_to_working(temp);
        let pw = self.preview_to_working();
        let Some(v) = self.state.viewer.as_mut() else {
            return;
        };
        let old = v.op_stack.clone();
        v.op_stack = stack.clone();
        v.opstack_version = v.opstack_version.wrapping_add(1);

        // Preview-tier matrix (RAW = WB-driven camera→working `cam`; Standard =
        // sRGB `pw`). Recomputed each edit; `set_color_matrix` no-ops when
        // unchanged, so only a WB temp change actually dirties the head (P2 §5.1).
        let pv_matrix = v.preview_tier_source(cam, pw).1;

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
                let mut ep = ferrolite_pipeline::EditPipeline::new(
                    ctx_arc.clone(),
                    &src,
                    shown.clone(),
                    matrix,
                );
                // A rebuilt preview must re-bind the current lens bake so an
                // already-corrected image keeps its correction at fit zoom (I1).
                if let Some(w) = v.lens_warp.as_ref() {
                    ep.set_warp(ferrolite_pipeline::WarpGridTexture::upload(&ctx_arc, w));
                }
                if let Some(vg) = v.lens_vignette.as_ref() {
                    ep.set_vignette(ferrolite_pipeline::VignetteTexture::upload(&ctx_arc, vg));
                }
                v.preview_edit = Some(ep);
            }
        }
        if let Some(ep) = v.preview_edit.as_mut() {
            ep.set_stack(shown.clone());
            ep.set_color_matrix(pv_matrix);
            // Apply the current lens amounts + vig lerp to the preview too, so a
            // lens Amount-only drag (no bake, no rebuild) updates the fit-zoom
            // image live — mirroring the full-res producer's amount-only branch
            // below. `use_warp` follows whether a grid is currently bound.
            let lc = shown.lens_correction();
            ep.set_lens_uniform(ferrolite_pipeline::lens_uniform(
                lc.as_ref(),
                v.lens_warp.is_some(),
            ));
            // Mode-aware vignette pair (MV2): profile lerp when a LUT is bound,
            // else the lens-free parametric manual gain. This is the site that
            // makes a manual-vignette Amount drag update the fit-zoom preview
            // live with NO lens (uniform-only; no bake, no rebuild).
            let (vig_amount, vig_manual) =
                crate::develop::vignette_mode::vig_pair(lc.as_ref(), v.lens_vignette.is_some());
            ep.set_vig_amount(vig_amount);
            ep.set_vig_manual(vig_manual);
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
                    // Thread the current lens bake (U7); `needs_full_rebuild`
                    // already fires when the rebuild-relevant lens key changes,
                    // so the grid/LUT this producer is BUILT with must be the
                    // one matching `shown`'s lens_id/focal/aperture/crop/enabled
                    // flags — i.e. the bake already stored on `v` by the
                    // `LensBaked` handler for this same key.
                    // Mode-aware vignette pair (MV2) for the fresh producer, so a
                    // rebuild (e.g. geometry change) with a persisted manual or
                    // profile vignette keeps applying it — the constructor only
                    // seeds `vig_amount`, never the parametric `manual`.
                    let (vig_amount, vig_manual) = crate::develop::vignette_mode::vig_pair(
                        shown.lens_correction().as_ref(),
                        v.lens_vignette.is_some(),
                    );
                    let tep = ferrolite_pipeline::TileEditPipeline::new(
                        ctx_arc,
                        pyr,
                        shown.clone(),
                        cam,
                        v.lens_warp.as_ref(),
                        v.lens_vignette.as_ref(),
                    );
                    let mut producer = viewer::EditTileProducer::new(tep);
                    producer.set_vig_amount(vig_amount);
                    producer.set_vig_manual(vig_manual);
                    v.edit_producer = Some(producer);
                }
            } else if let Some(producer) = v.edit_producer.as_mut() {
                // Color-only change: update params in place. Also covers a lens
                // Amount-only change (no rebuild per `needs_full_rebuild`): the
                // grid/LUT are unchanged, only the uniform lerp amounts move.
                producer.set_stack(shown.clone());
                producer.set_color_matrix(cam);
                let lc = shown.lens_correction();
                producer.set_lens_uniform(ferrolite_pipeline::lens_uniform(
                    lc.as_ref(),
                    v.lens_warp.is_some(),
                ));
                // Mode-aware vignette pair (MV2): a manual Amount drag with no
                // lens reaches here (uniform-only, no rebuild) and updates the
                // full-res producer live.
                let (vig_amount, vig_manual) =
                    crate::develop::vignette_mode::vig_pair(lc.as_ref(), v.lens_vignette.is_some());
                producer.set_vig_amount(vig_amount);
                producer.set_vig_manual(vig_manual);
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
        // Snapshot the pre-edit stack BEFORE `set_preview_and_full` overwrites
        // `v.op_stack`, so a lens-key comparison below sees the real old/new.
        let old_stack = self.state.viewer.as_ref().map(|v| v.op_stack.clone());
        self.set_preview_and_full(frame, stack.clone());
        if !commit {
            return;
        }
        let Some(v) = self.state.viewer.as_mut() else {
            return;
        };
        v.edits_dirty = true;
        v.history.push(kind, stack.clone());
        // Mask edits all share OpKind::LocalAdjustments; seal so each committed
        // gesture (stroke, slider drag, discrete action) is its own undo step.
        if kind == ferrolite_pipeline::OpKind::LocalAdjustments {
            v.history.break_coalesce();
        }
        let image_id = v.image_id;
        let path = v.path.clone();
        let has_edits = !stack.is_identity();
        if let Some(rec) = self.state.images.iter_mut().find(|r| r.id == image_id) {
            rec.has_edits = has_edits; // optimistic cache update (filmstrip badge)
        }
        if kind == ferrolite_pipeline::OpKind::LensCorrection {
            if let Some(old) = old_stack {
                self.maybe_spawn_lens_bake(ctx, &old, &stack);
            }
        }
        self.persist_ops(ctx, image_id, path, stack);
    }

    /// Rebuild the mask-overlay GPU texture iff the selected mask definition or
    /// the preview generation changed. Bounded (≤ OVERLAY_MAX_EDGE composite +
    /// GPU-side red tint, NO GPU→CPU readback) + only-on-change, so it is safe on
    /// the UI thread even mid-stroke (CLAUDE.md §1). The `MaskOverlayCompositor`
    /// (incl. its tint pipeline) is built once and cached on `ViewerState`, never
    /// rebuilt per frame (CLAUDE.md §2).
    fn rebuild_mask_overlay_if_needed(&mut self, frame: &eframe::Frame) {
        use crate::develop::mask_edit;
        use crate::develop::mask_overlay_color::OVERLAY_MAX_EDGE;
        use std::hash::{Hash, Hasher};

        let Some(v) = self.state.viewer.as_mut() else {
            return;
        };
        let la = mask_edit::layers(&v.op_stack);
        let Some(sel) = v.mask.selected.filter(|&i| i < la.layers.len()) else {
            v.mask.overlay_key = None;
            return;
        };
        let committed_def = &la.layers[sel].mask;
        // While the Components window's Add section is tuning a Luma/Color
        // component (Task 6), composite the PROSPECTIVE def (committed + the
        // tentative component at its mode) instead of the committed one, so the
        // red overlay live-previews the in-progress add.
        let def = match v.mask.preview_component.clone() {
            Some((c, mode)) => mask_edit::prospective_def(committed_def, c, mode),
            None => committed_def.clone(),
        };
        // Key: which mask def + preview generation + any in-progress preview
        // component. serde-hash the def (small); fold the tentative component in
        // too so the overlay rebuilds live as the Add sliders move (its params
        // aren't reflected in `def`/`committed_def` otherwise since it's not
        // committed to the OpStack yet).
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // Committed mask changes are already captured by `opstack_version` (it
        // bumps on every edit, incl. undo/redo) plus the selected-mask index (a
        // mask switch changes the shown def without an edit). Hashing those two is
        // O(1). We deliberately do NOT serde-serialize `committed_def` here: that
        // is O(all-components · brush-nodes) on the UI thread EVERY frame during a
        // stroke, which made large masks (100+ components) lag. Only the
        // uncommitted `preview_component` (one tentative component, small) still
        // needs hashing, since it is not reflected in `opstack_version`.
        sel.hash(&mut h);
        v.opstack_version.hash(&mut h); // bumps on every committed edit / preview regen
        serde_json::to_string(&v.mask.preview_component)
            .unwrap_or_default()
            .hash(&mut h);
        // Fold the hovered component into the key so the highlight texture
        // rebuilds (via `MaskOverlayCompositor::highlight_texture`) whenever the
        // Components modal's hover changes, even though the def itself didn't.
        v.mask.highlight_component.hash(&mut h);
        let key = h.finish();
        // Dedup: `overlay_key` is the per-viewer correctness signal (it resets to
        // None on image switch / stack change, so a fresh viewer always rebuilds
        // before its first draw); `mask_overlay_native.is_some()` is only the
        // app-global first-registration guard. Both must hold to skip.
        if v.mask.overlay_key == Some(key) && self.state.mask_overlay_native.is_some() {
            return;
        }

        // GPU context from the eframe render state, NOT from `v.preview_edit`: the
        // mask overlay must be able to build on a freshly-(re)opened image BEFORE
        // any edit exists. `preview_edit` is created lazily — for Standard images
        // only on the first edit (`set_preview_and_full`), for RAW at full-decode —
        // so gating the overlay on it meant a just-opened mask showed no overlay
        // until the first component edit / invert toggle forced a rebuild. Sourcing
        // the context here (same wgpu device eframe uses; `from_render_state` is the
        // established ad-hoc-context pattern) fixes that. Compositor/input below are
        // still cached once and reused (CLAUDE.md GPU rule).
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let gpu_ctx = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        if v.mask_overlay.is_none() {
            v.mask_overlay = Some(ferrolite_pipeline::MaskOverlayCompositor::new(
                gpu_ctx.clone(),
            ));
        }
        if v.mask_overlay_input.is_none() {
            if let Some(src) = v.preview_source.as_ref() {
                let small = downscale_linear(src, OVERLAY_MAX_EDGE);
                v.mask_overlay_input = Some(ferrolite_pipeline::upload_source(&gpu_ctx, &small));
                // Bump the input generation so the compositor's per-component
                // cache re-samples range shapes against the fresh input.
                v.mask_overlay_input_gen = v.mask_overlay_input_gen.wrapping_add(1);
            }
        }
        let (overlay, highlight) = {
            let highlight_component = v.mask.highlight_component;
            // Monotonic input identity for the compositor's incremental cache:
            // changes whenever `mask_overlay_input` is re-uploaded, so range
            // shapes re-sample the fresh image. A counter (not the texture's
            // `Arc` pointer) avoids an ABA hazard where a freed texture's address
            // is reused by the new upload and collides.
            let input_id = v.mask_overlay_input_gen;
            let (Some(oc), Some(input)) = (v.mask_overlay.as_mut(), v.mask_overlay_input.as_ref())
            else {
                return;
            };
            let overlay = oc.overlay_texture(
                &def,
                input,
                input_id,
                crate::develop::mask_overlay_color::OVERLAY_STRENGTH,
            );
            // Highlight build MUST come after `overlay_texture` above: it reads
            // the per-component cache that call just populated for the CURRENT
            // def. `highlight_texture` is bounds-safe (returns `None` if the
            // hovered index isn't in the cache), so a stale/out-of-range index
            // just means no highlight this frame rather than a panic.
            let highlight = highlight_component.and_then(|idx| {
                oc.highlight_texture(idx, crate::develop::mask_overlay_color::HIGHLIGHT_STRENGTH)
            });
            v.mask.overlay_key = Some(key);
            (overlay, highlight)
        };
        // `v` borrow ends here; the renderer + app-global texture ids are disjoint.
        let view = overlay.srgb_view();
        {
            let mut renderer = rs.renderer.write();
            match self.state.mask_overlay_native {
                Some(id) => renderer.update_egui_texture_from_wgpu_texture(
                    &gpu_ctx.device,
                    &view,
                    wgpu::FilterMode::Linear,
                    id,
                ),
                None => {
                    let id = renderer.register_native_texture(
                        &gpu_ctx.device,
                        &view,
                        wgpu::FilterMode::Linear,
                    );
                    self.state.mask_overlay_native = Some(id);
                }
            }
            if let Some(highlight) = &highlight {
                let hview = highlight.srgb_view();
                match self.state.mask_overlay_highlight_native {
                    Some(id) => renderer.update_egui_texture_from_wgpu_texture(
                        &gpu_ctx.device,
                        &hview,
                        wgpu::FilterMode::Linear,
                        id,
                    ),
                    None => {
                        let id = renderer.register_native_texture(
                            &gpu_ctx.device,
                            &hview,
                            wgpu::FilterMode::Linear,
                        );
                        self.state.mask_overlay_highlight_native = Some(id);
                    }
                }
            }
        }
        self.state.mask_overlay_gpu = Some(overlay);
        // Only replace the cached highlight `OverlayTexture` when a fresh one was
        // built this frame; when `highlight_component` is `None` the stale GPU
        // texture is simply left un-drawn (draw-time gates on
        // `highlight_component.is_some()`), so there's no dangling-view risk from
        // dropping it here while the registered native id still references it.
        if let Some(highlight) = highlight {
            self.state.mask_overlay_highlight_gpu = Some(highlight);
        }
    }

    /// Spawn an off-thread lens bake (Spec 4.4, U7) iff the bake-relevant lens
    /// key (`ops_edit::lens_bake_key`: lens id, distortion/tca/vignetting
    /// enabled flags, focal/aperture/crop) changed between `old`/`new`. This is
    /// intentionally a DIFFERENT key from `needs_full_rebuild`'s
    /// `lens_rebuild_key`, which excludes `vignetting.enabled` (a vignette
    /// toggle has no halo/geometry impact, so it must not force an immediate
    /// `TileEditPipeline` rebuild) — but `bake_products` DOES bake the
    /// vignette LUT whenever `vignetting.enabled`, so the bake trigger must
    /// still fire on that toggle or the LUT is never produced. This
    /// deliberately excludes per-correction `amount`s: an Amount-only slider
    /// drag must NOT re-run the DB lookup + bake (it's a uniform-only update
    /// applied in `set_preview_and_full`'s non-rebuild branch), only a change
    /// that would actually produce a different grid/LUT re-bakes.
    fn maybe_spawn_lens_bake(
        &mut self,
        ctx: &egui::Context,
        old: &ferrolite_pipeline::OpStack,
        new: &ferrolite_pipeline::OpStack,
    ) {
        if crate::develop::ops_edit::lens_bake_key(old)
            == crate::develop::ops_edit::lens_bake_key(new)
        {
            return; // Amount-only (or no) lens change: no bake needed.
        }
        let Some(db) = self.state.lens_db.clone() else {
            return; // DB failed to load at startup: lens correction disabled.
        };
        let Some(lc) = new.lens_correction() else {
            return; // LensCorrection op was removed entirely: nothing to bake.
        };
        let Some(v) = self.state.viewer.as_mut() else {
            return;
        };
        // Cancel any in-flight bake for the previous key before superseding it.
        if let Some(h) = v.lens_bake_handle.take() {
            h.cancel();
        }
        let image_id = v.image_id;
        let handle = crate::develop::lens_bake::spawn_lens_bake(
            &self.state.jobs,
            &db,
            &self.state.tx,
            ctx,
            image_id,
            lc,
        );
        if let Some(v) = self.state.viewer.as_mut() {
            v.lens_bake_handle = Some(handle);
        }
    }

    /// Spec 4.4 (U9) Step 1: attempt the cheap in-memory lens auto-match for
    /// `image_id`'s viewer, once both its EXIF (`meta_loaded`) and its loaded
    /// op-stack (`ops_loaded`) are known. One-shot per open
    /// (`lens_auto_match_attempted`) and gated on `lens_match::should_auto_match`
    /// so an existing `LensCorrection` op (persisted, or added/cleared this
    /// session) is never second-guessed.
    ///
    /// Deliberately does NOT call `ops_edit::set_lens_correction` or spawn a
    /// bake: it only stores the `LensMatch` candidate on the viewer for the
    /// panel to read as its seed. No op is created, so `has_edits`/the
    /// catalog badge/the sidecar are all untouched until the user actually
    /// toggles a correction — opt-in is preserved.
    ///
    /// The DB lookup itself is a bundled-XML string/table search (no I/O, no
    /// GPU), the same cost class already run inline for the manual picker
    /// (`find_lenses`) and documented as UI-thread-safe in `lens_bake.rs`'s
    /// module doc — hence no job is spawned for this step.
    fn try_auto_match_lens(&mut self, image_id: i64) {
        let Some(v) = self.state.viewer.as_ref() else {
            return;
        };
        if v.image_id != image_id || v.lens_auto_match_attempted {
            return;
        }
        if !v.meta_loaded || !v.ops_loaded {
            return; // wait for both prerequisites before attempting/giving up
        }
        // Gather everything the match needs as owned values before taking the
        // exclusive borrow below (mirrors the borrow discipline used elsewhere
        // in this loop, e.g. `maybe_spawn_lens_bake`).
        let should_match = crate::develop::lens_match::should_auto_match(&v.op_stack);
        let query = v
            .meta
            .as_ref()
            .and_then(crate::develop::lens_match::query_from_metadata);

        let Some(v) = self.state.viewer.as_mut() else {
            return;
        };
        v.lens_auto_match_attempted = true; // one-shot regardless of outcome below
        if !should_match {
            return; // an explicit LensCorrection op already exists: don't guess
        }
        let Some(db) = self.state.lens_db.clone() else {
            return; // DB unavailable: lens correction section is disabled
        };
        let Some(query) = query else {
            return; // decode failed, no EXIF, or no focal length: can't query
        };
        let candidate = ferrolite_lens::LensDb::match_lens(db.as_ref(), &query);
        if let Some(v) = self.state.viewer.as_mut() {
            if v.image_id == image_id {
                v.lens_auto_match = candidate;
            }
        }
    }

    /// Re-apply the currently-resolved display tail to the viewer pipelines.
    /// LUT path when a monitor profile is active, else the analytic sRGB matrix.
    /// Synchronous — safe to call on every image reveal (no re-bake, no flash).
    /// Re-uploading the LUT on open also self-heals a device-loss.
    fn apply_display_tail(&self, gpu: &ferrolite_gpu::GpuContext, vp: &viewer::ViewerPipelines) {
        match &self.state.display_lut {
            Some(l) => vp.pipelines.set_display_lut(
                &gpu.queue,
                l.size,
                &l.rgba16f,
                ferrolite_color::DISPLAY_LUT_SHAPER_GAMMA,
            ),
            None => vp.pipelines.set_display_matrix(
                &gpu.queue,
                ferrolite_color::working_to_display(self.state.working_space),
            ),
        }
    }

    /// Detect the window's monitor profile (cheap UI-thread OS call), then
    /// parse + bake the display LUT off the UI thread on `ferrolite-jobs`.
    /// Bumps `display_detect_gen` so stale results from superseded re-detects
    /// are dropped when they arrive. The ONLY UI-thread work here is the
    /// `monitor_profile::detect` OS call (microseconds); the file read, ICC
    /// parse and LUT bake all run inside the Background job (CLAUDE.md §1).
    fn redetect_display_profile(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        use raw_window_handle::HasWindowHandle;

        self.state.display_detect_gen += 1;
        let generation = self.state.display_detect_gen;
        let mode = self.state.settings.display_profile.clone();
        let working = self.state.working_space;
        let tx = self.state.tx.clone();

        // UI-thread OS call only (cheap): get the ICC source + monitor key.
        let (detected, key) = match frame.window_handle() {
            Ok(h) => crate::monitor_profile::detect(h.as_raw()),
            Err(_) => (None, 0),
        };
        self.state.last_monitor_key = key;
        let source = crate::settings::dto::resolve(&mode, detected);

        // Off-thread: file read + ICC parse + LUT bake. Never on the UI thread.
        self.state
            .jobs
            .submit(ferrolite_jobs::Priority::Background, move |_cancel| {
                let (lut, name) = match source {
                    None => (None, "sRGB (default)".to_string()),
                    Some(src) => match crate::monitor_profile::source_to_bytes(src)
                        .ok()
                        .and_then(|b| ferrolite_color::DisplayProfile::parse(&b).ok())
                    {
                        Some(profile) => match ferrolite_color::bake_display_lut(
                            working,
                            &profile,
                            ferrolite_color::DISPLAY_LUT_SIZE,
                        ) {
                            Ok(lut) => {
                                let name = profile.name.clone();
                                (Some(lut), name)
                            }
                            Err(e) => {
                                eprintln!("ferrolite: display LUT bake failed: {e}");
                                (None, "Not detected — using sRGB".to_string())
                            }
                        },
                        None => (None, "Not detected — using sRGB".to_string()),
                    },
                };
                let _ = tx.send(crate::events::AppEvent::DisplayProfileResolved {
                    lut,
                    name,
                    generation,
                });
            });
        ctx.request_repaint();
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

        // Re-bake the display LUT for the new working space when a monitor
        // profile is active. In sRGB mode this resolves to `None` and the
        // event handler restores the analytic matrix above — a cheap no-op.
        self.redetect_display_profile(ctx, frame);

        let cam = self.camera_to_working(self.current_wb_temp());
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

        // Tier-0 placeholder: the resident grid thumbnail for this image (if any),
        // cloned out of the texture cache BEFORE the `viewer::paint` borrow of `v`
        // so the `self.state.textures` borrow is released first. A `TextureHandle`
        // clone is a cheap refcount bump (no pixel copy).
        let tier0_thumb = self.state.textures.get(image_id).cloned();

        // `paint` applies this frame's pan/zoom and clears `idle` when the view
        // moved, so read `idle` AFTER it to catch an interaction this frame. It
        // also folds this frame's `interacting` into the present source and returns
        // the chosen source so the repaint gate can keep the loop alive mid-fade.
        let (loading_preview, present_source) = viewer::paint(
            ui,
            v,
            full_ready,
            front_valid,
            factor,
            interactive,
            tier0_thumb.as_ref(),
        );
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
                || v.tier0_fading
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
    fn open_record(
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
        // Consult the warm cache immediately: a display-tier hit reveals sharp at
        // fit instantly from RAM, short-circuiting the Tier-0/1/decode ladder
        // below for this open (`v.warm_revealed`, checked in `drive_viewer`).
        self.try_warm_reveal(frame, rec.id);
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

/// Nearest-neighbor CPU downscale of a LinearRgbaF32 to ≤ max_edge longest side.
/// Used to bound the mask-overlay compositor's input (CLAUDE.md §1) so the
/// GPU composite + CPU readback stay cheap enough to rebuild on every mask/edit
/// change, even mid-stroke.
fn downscale_linear(
    src: &ferrolite_image::LinearRgbaF32,
    max_edge: u32,
) -> ferrolite_image::LinearRgbaF32 {
    let (sw, sh) = (src.width, src.height);
    let scale = (max_edge as f32 / sw.max(sh) as f32).min(1.0);
    let (dw, dh) = (
        ((sw as f32 * scale) as u32).max(1),
        ((sh as f32 * scale) as u32).max(1),
    );
    if (dw, dh) == (sw, sh) {
        return src.clone();
    }
    let mut px = Vec::with_capacity((dw * dh * 4) as usize);
    for y in 0..dh {
        let sy = (y as f32 / dh as f32 * sh as f32) as u32;
        for x in 0..dw {
            let sx = (x as f32 / dw as f32 * sw as f32) as u32;
            let i = ((sy * sw + sx) * 4) as usize;
            px.extend_from_slice(&src.pixels[i..i + 4]);
        }
    }
    ferrolite_image::LinearRgbaF32::new(dw, dh, px).expect("downscale length")
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
            self.redetect_display_profile(ctx, frame);
        } else if self.did_display_detect {
            // Multi-monitor follow: cheap per-frame monitor-key check. When the
            // window moves to a display with a different profile, re-detect+bake.
            use raw_window_handle::HasWindowHandle;
            if let Ok(h) = frame.window_handle() {
                let (_src, key) = crate::monitor_profile::detect(h.as_raw());
                if key != self.state.last_monitor_key {
                    self.redetect_display_profile(ctx, frame);
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
                    self.apply_preview_ready(frame, ctx, *image_id, linear);
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
                crate::events::AppEvent::PyramidReady {
                    image_id,
                    tile_source,
                    gpu_pyramid,
                } => {
                    self.apply_pyramid_ready(frame, *image_id, tile_source, gpu_pyramid);
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
                    // `full_res = false`: the embedded JPEG fallback is a
                    // downscaled stand-in, not the full RAW decode — must not
                    // be warm-cached as if it were the sharp 1:1 tier.
                    if need_fallback && self.reveal_srgb_preview(frame, image_id, false) {
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
                    let mut rebake: Option<ferrolite_pipeline::LensCorrection> = None;
                    let mut just_loaded = false;
                    if let Some(v) = self.state.viewer.as_mut() {
                        if v.image_id == *image_id && !v.ops_loaded {
                            v.ops_loaded = true;
                            just_loaded = true;
                            if !stack.is_identity() {
                                v.history =
                                    crate::develop::history::History::new(stack.clone(), 100);
                                self.set_preview_and_full(frame, stack.clone());
                            }
                            // Spec 4.4 (U9) Step 2: a persisted correction (any
                            // toggle enabled, with a resolvable lens key) needs
                            // its warp/vignette re-baked from scratch — the
                            // grids themselves are never persisted (spec §7.4).
                            // Until the bake returns, `LensUniform.use_warp`
                            // stays 0 (identity), so nothing renders wrong in
                            // the meantime.
                            if let Some(lc) = stack.lens_correction() {
                                if crate::develop::lens_bake::needs_rebake_on_load(&lc) {
                                    rebake = Some(lc);
                                }
                            }
                        }
                    }
                    if just_loaded {
                        // Session-wide tool/tab selection (Task 3) survives image
                        // switches, but a tab valid for the previous image may not
                        // exist for this one (e.g. its temp tab belonged to a tool
                        // disabled for the new image) — re-validate now that the new
                        // image's ops/registry context is current.
                        self.state.tool_state.ensure_valid_tab(&self.tool_registry);
                    }
                    if let Some(lc) = rebake {
                        if let Some(db) = self.state.lens_db.clone() {
                            let image_id = *image_id;
                            let handle = crate::develop::lens_bake::spawn_lens_bake(
                                &self.state.jobs,
                                &db,
                                &self.state.tx,
                                ctx,
                                image_id,
                                lc,
                            );
                            if let Some(v) = self.state.viewer.as_mut() {
                                if v.image_id == image_id {
                                    v.lens_bake_handle = Some(handle);
                                }
                            }
                        }
                    }
                    // Spec 4.4 (U9) Step 1: try the cheap in-memory auto-match
                    // now that the loaded stack is known (it may resolve before
                    // or after MetaLoaded — both event handlers attempt it, and
                    // `lens_auto_match_attempted` makes the attempt one-shot).
                    self.try_auto_match_lens(*image_id);
                    self.state.dirty = true;
                    continue;
                }
                crate::events::AppEvent::MetaLoaded { image_id, meta } => {
                    if let Some(v) = self.state.viewer.as_mut() {
                        if v.image_id == *image_id && !v.meta_loaded {
                            v.meta_loaded = true;
                            v.meta = meta.clone();
                        }
                    }
                    self.try_auto_match_lens(*image_id);
                    self.state.dirty = true;
                    ctx.request_repaint();
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
                    image_id: _,
                    done,
                    total,
                } => {
                    if let Some(a) = self.state.export_activity.as_mut() {
                        a.set_tiles(*done, *total);
                    }
                    ctx.request_repaint();
                    continue;
                }
                crate::events::AppEvent::ExportFinished {
                    image_id: _,
                    ok,
                    message,
                } => {
                    if let Some(a) = self.state.export_activity.as_mut() {
                        if a.kind == crate::export::ExportKind::Single {
                            a.item_finished(*ok, message.clone());
                        }
                    }
                    self.state.notify(
                        if *ok {
                            crate::notifications::Level::Info
                        } else {
                            crate::notifications::Level::Error
                        },
                        message.clone(),
                    );
                    ctx.request_repaint();
                    continue;
                }
                crate::events::AppEvent::ExportItemStarted { name } => {
                    if let Some(a) = self.state.export_activity.as_mut() {
                        a.start_item(Some(name.clone()));
                    }
                    ctx.request_repaint();
                    continue;
                }
                crate::events::AppEvent::DisplayProfileResolved {
                    lut,
                    name,
                    generation,
                } => {
                    // Drop results superseded by a newer re-detect.
                    if *generation == self.state.display_detect_gen {
                        self.state.display_profile_name = name.clone();
                        // Store the resolved tail so the handler and the image
                        // reveal sites share one source of truth (`apply_display_tail`).
                        self.state.display_lut = lut.clone();
                        if let Some(rs) = frame.wgpu_render_state() {
                            let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
                            let renderer = rs.renderer.read();
                            if let Some(vp) =
                                renderer.callback_resources.get::<viewer::ViewerPipelines>()
                            {
                                self.apply_display_tail(&gpu, vp);
                            }
                        }
                        ctx.request_repaint();
                    }
                    continue;
                }
                crate::events::AppEvent::LensBaked { image_id, result } => {
                    self.apply_lens_baked(frame, *image_id, result);
                    self.state.dirty = true;
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
        // Auto-dismiss the finished-export indicator a few seconds after it
        // completes so the status bar returns to normal on its own. `completed_at`
        // is only set once an export is done, so a running export is untouched.
        if let Some(done_at) = self
            .state
            .export_activity
            .as_ref()
            .and_then(|a| a.completed_at)
        {
            const EXPORT_DONE_LINGER: std::time::Duration = std::time::Duration::from_secs(4);
            let elapsed = done_at.elapsed();
            if elapsed >= EXPORT_DONE_LINGER {
                self.state.export_activity = None;
            } else {
                ctx.request_repaint_after(EXPORT_DONE_LINGER - elapsed);
            }
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
                        self.state.notify(
                            crate::notifications::Level::Info,
                            if was_queued {
                                "Removed from export queue."
                            } else {
                                "Added to export queue."
                            },
                        );
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

                // Tool-switch keybinds (A/C/M by default) and mask-overlay toggle.
                // Mirrors the tool palette's `SelectTool` handler's borrow
                // discipline: resolve `enabled` via a shared borrow first, then
                // take `&mut self.state.viewer` to apply it.
                let km = &self.state.settings.keymap;
                let tool = if km.pressed(ctx, crate::settings::keymap::Action::SwitchToolAdjust) {
                    Some(crate::develop::tool::ToolId::Adjust)
                } else if km.pressed(ctx, crate::settings::keymap::Action::SwitchToolCrop) {
                    Some(crate::develop::tool::ToolId::Crop)
                } else if km.pressed(ctx, crate::settings::keymap::Action::SwitchToolMask) {
                    Some(crate::develop::tool::ToolId::Mask)
                } else {
                    None
                };
                if let Some(id) = tool {
                    let enabled = self
                        .tool_registry
                        .get(id)
                        .map(|t| {
                            t.enabled(&crate::develop::tool::DevelopCtx { state: &self.state })
                        })
                        .unwrap_or(false);
                    if self.state.viewer.is_some() {
                        self.state
                            .tool_state
                            .select_tool(id, enabled, &self.tool_registry);
                    }
                }

                if self
                    .state
                    .settings
                    .keymap
                    .pressed(ctx, crate::settings::keymap::Action::ToggleMaskOverlay)
                {
                    if let Some(v) = self.state.viewer.as_mut() {
                        v.mask.overlay_on = !v.mask.overlay_on;
                    }
                }

                // New Brush Layer (default `B`): starts a fresh, separately-deletable
                // `Brush` component in the selected mask — the explicit "split" for the
                // merge-by-default brush model. Gated on the Mask tool being active
                // (mirrors `ToggleMaskOverlay` above) plus an actual mask selection.
                if self
                    .state
                    .settings
                    .keymap
                    .pressed(ctx, crate::settings::keymap::Action::NewBrushLayer)
                {
                    let stack_and_idx = self.state.viewer.as_ref().and_then(|v| {
                        (v.mask.active && v.mask.selected.is_some())
                            .then(|| (v.op_stack.clone(), v.mask.selected.unwrap()))
                    });
                    if let Some((stack, idx)) = stack_and_idx {
                        let new_stack = crate::develop::mask_edit::new_brush_layer(&stack, idx);
                        self.apply_edit(
                            ctx,
                            frame,
                            ferrolite_pipeline::OpKind::LocalAdjustments,
                            new_stack,
                            true,
                        );
                    }
                }

                // Zoom-to-fit (default `F`) and Zoom 1:1 (default `Z`): rebuild the
                // same transforms the canvas's double-click toggle already builds
                // (`viewer/mod.rs`'s `paint`), just from a keybind instead of a
                // double-click gesture.
                if self
                    .state
                    .settings
                    .keymap
                    .pressed(ctx, crate::settings::keymap::Action::ZoomFit)
                {
                    if let Some(v) = self.state.viewer.as_mut() {
                        if let Some(dims) = v.image_dims {
                            v.view = ferrolite_vt::ViewTransform::fit(dims, v.viewport);
                            v.idle = false;
                            // Snap cleanly to fit: drop any pending/residual scroll so
                            // trackpad momentum can't keep zooming past the fit this
                            // frame (drive_viewer reads these deltas later this frame).
                            ctx.input_mut(|i| {
                                i.raw_scroll_delta = egui::Vec2::ZERO;
                                i.smooth_scroll_delta = egui::Vec2::ZERO;
                            });
                            ctx.request_repaint();
                        }
                    }
                }
                if self
                    .state
                    .settings
                    .keymap
                    .pressed(ctx, crate::settings::keymap::Action::ZoomActual)
                {
                    if let Some(v) = self.state.viewer.as_mut() {
                        if v.image_dims.is_some() {
                            v.view = ferrolite_vt::ViewTransform {
                                zoom: 1.0,
                                pan: (0.0, 0.0),
                            };
                            v.idle = false;
                            // Same as fit: kill residual scroll velocity so the 1:1
                            // snap isn't immediately dragged off by trackpad momentum.
                            ctx.input_mut(|i| {
                                i.raw_scroll_delta = egui::Vec2::ZERO;
                                i.smooth_scroll_delta = egui::Vec2::ZERO;
                            });
                            ctx.request_repaint();
                        }
                    }
                }
            }
        }

        // Submit the tier-1 preview decode. RAW: the small EMBEDDED preview is
        // submitted immediately (keeps the tier-1 alive; cheap). Standard: the
        // tier-1 preview IS the full-res JPG decode (heavy), so it is NOT fired
        // here — it is gated behind the preview-cache read below (mirrors RAW's
        // full-decode gate) so a re-open reveals the cached 2048px entry first.
        if let Some(v) = self.state.viewer.as_mut() {
            // RAW embedded tier-1 preview: skip it on a warm DISPLAY hit
            // (`try_warm_reveal`, from `open_record`) — the cached display texture
            // already serves the fit/preview reveal, so this small embedded-JPEG
            // decode (only ever the full-decode-failure fallback source) would be
            // pure waste. The tier-2 decode chain BELOW is gated on `warm_revealed`
            // PER KIND: RAW's `spawn_full`/pyramid stays UNGATED — it must still run
            // so the sparse full SHARP tier streams in behind the warm texture
            // exactly as a normal open, and `idle` (cleared by the warm hit) returns
            // true only once it converges. Standard's heavy JPEG decode IS gated —
            // the warm display texture there is already the full-resolution image
            // (see `warm_insert_display`'s `full_res` gate), so re-decoding would
            // only reproduce what is already on screen; that branch settles `idle`
            // back to `true` itself once the debounce elapses. Neither case spins.
            if !v.warm_revealed && !v.preview_requested && v.kind == ferrolite_image::FileKind::Raw
            {
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
            // Tier-1 preview-cache read, then the heavy decode — for BOTH kinds.
            // Debounced (FULL_DECODE_DEBOUNCE) so fast arrow-nav doesn't submit a
            // read/decode per image flipped through — only the settled-on image
            // does, once `open_elapsed` crosses the threshold.
            //
            // Read-before-decode: consult the preview cache FIRST
            // (`spawn_cache_read`). The heavy decode is gated on the read having
            // resolved (`cache_resolved`), so a cache HIT reveals the 2048px entry
            // from disk (`apply_preview_cache_hit`) and the decode then streams in
            // the extra 1:1 detail — a MISS falls straight through to decode +
            // write-back. Phase 2's Tier-0 thumbnail covers the debounce window.
            //
            // A warm display hit pre-sets `cache_read_requested`/`cache_resolved`
            // (see `try_warm_reveal`), so this block skips the disk read and fires
            // the tier-2 decode directly once the debounce elapses — the sparse
            // full (RAW) / full-res (Standard) still streams in behind the warm
            // texture so 1:1 sharpens.
            let dt = ctx.input(|i| i.stable_dt);
            v.open_elapsed += dt;
            let heavy_pending = if v.kind == ferrolite_image::FileKind::Raw {
                !v.full_requested
            } else {
                !v.preview_requested
            };
            if !v.cache_read_requested || (heavy_pending && v.cache_resolved) {
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
                    } else if heavy_pending && v.cache_resolved {
                        if v.kind == ferrolite_image::FileKind::Raw {
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
                        } else if !v.warm_revealed {
                            // Standard: the heavy tier-1 IS the full-res JPG decode.
                            // This also runs after a cache HIT (`heavy_pending` is
                            // `!preview_requested`, still true post-hit) — the 2048px
                            // reveal from `apply_preview_cache_hit` shows first, then
                            // this streams in the full-res 1:1 detail.
                            //
                            // Gated on `!v.warm_revealed`: unlike RAW, Standard has no
                            // separate pyramid tier (`spawn_full`/the pyramid are
                            // RAW-only) — a Standard image's full-resolution display
                            // texture IS the sharp 1:1 artifact. A warm display hit
                            // (`try_warm_reveal`) already installed that full-res
                            // texture (warm-cached ONLY when full-res — see
                            // `warm_insert_display`'s `full_res` gate), so re-decoding
                            // here would just reproduce what is already on screen.
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
                        } else {
                            // Warm Standard hit: skip the redundant decode (see
                            // above). Nothing will call `reveal_srgb_preview` /
                            // `install_preview_holder` for this open, so there is no
                            // other path left to converge `idle` — settle it here,
                            // mirroring what `install_preview_holder` does for a tier-1
                            // reveal with no tier-2 to wait on. Also mark
                            // `preview_requested` so `heavy_pending` clears and this
                            // branch is not re-entered every frame.
                            v.preview_requested = true;
                            v.idle = true;
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
                                self.set_preview_and_full(frame, stack);
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
                            self.rebuild_mask_overlay_if_needed(frame);
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
                                        self.apply_edit(ctx, frame, o.kind, o.stack, o.commit);
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
                self.redetect_display_profile(ctx, frame);
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
                self.apply_edit(ctx, frame, o.kind, o.stack, o.commit);
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

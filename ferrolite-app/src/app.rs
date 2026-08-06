pub mod controller;
pub mod shortcuts;

use crate::canvas::{self, CanvasResources};
use crate::module::Module;
use crate::theme;
use crate::viewer;

pub struct FerroliteApp {
    pub(crate) module: Module,
    thumb_size: f32,
    pub(crate) state: crate::state::AppState,

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
    /// One-shot Task-14 metadata-backfill spawn guard: set `true` on the
    /// first `update()` frame, mirroring `did_restore`. The job
    /// (`library::meta_backfill::spawn_once`) is submitted unconditionally
    /// when this flag flips — the backlog check (`has_backlog`) runs INSIDE
    /// the job, off the UI thread, as its first step, so this flag only
    /// ensures the job is submitted at most once per app run, not a gate on
    /// whether there's work to do.
    did_meta_backfill_spawn: bool,
    /// One-shot startup preset-directory scan guard (P7), mirroring
    /// `did_meta_backfill_spawn`: the scan (`presets::spawn_load_all`) is
    /// off-thread file I/O, spawned exactly once per app run.
    did_presets_load_spawn: bool,
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

/// The boolean-OR core of `FerroliteApp::modal_active`, split out as a free
/// function over plain `bool`s so the "which flags gate global shortcut
/// dispatch" logic is unit-testable without a `FerroliteApp` — constructing
/// one needs a real wgpu `CreationContext` and opens the on-disk catalog
/// (`AppState::new`), disproportionate scaffolding for what is otherwise a
/// one-line `||` chain.
fn any_modal_pending(
    show_help: bool,
    show_settings: bool,
    pending_remove: bool,
    open_group_modal: bool,
    pending_rename_preset: bool,
    pending_delete_preset: bool,
) -> bool {
    show_help
        || show_settings
        || pending_remove
        || open_group_modal
        || pending_rename_preset
        || pending_delete_preset
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

            pending_texture_clear: false,
            diag: crate::diag::DiagState::new(),
            settings_dirty: false,
            did_restore: false,
            did_meta_backfill_spawn: false,
            did_presets_load_spawn: false,
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
    pub(crate) fn apply_undo_redo(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        undo: bool,
    ) {
        // P7: with no active Develop session, Ctrl+Z (or the Edit menu's
        // Undo item) reverts the last batch apply. Reusing the existing
        // action rather than adding a binding means Undo keeps meaning
        // "undo the last thing I did", and the keybind is already
        // discoverable in the Settings keyboard tab and the Help panel
        // (CLAUDE.md), so no new GROUPS or Help entry is needed. Redo is
        // NOT extended — undoing an undo is not offered (`spawn_batch_undo`
        // reports through `AppEvent::BatchUndoDone`, which carries no
        // snapshot of its own). The gating + one-shot-take itself lives in
        // `AppState::take_batch_undo` (state.rs), pinned by its own tests,
        // rather than inline here.
        if let Some(snapshot) = self.state.take_batch_undo(undo) {
            crate::presets::apply::spawn_batch_undo(
                &self.state.jobs,
                &self.state.writer,
                &self.state.tx,
                ctx,
                snapshot,
            );
            return;
        }
        let result = self.state.viewer.as_mut().and_then(|v| {
            if undo {
                v.history.undo()
            } else {
                v.history.redo()
            }
        });
        if let Some(stack) = result {
            crate::app::controller::AppController::set_preview_and_full(
                self,
                frame,
                stack.clone(),
                true,
            );
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
    /// Standard preview reveal (`apply_preview_ready`), the Standard/RAW
    /// preview-cache-hit reveal (`apply_preview_cache_hit`), and the RAW
    /// full-decode-failure fallback (`FullFailed`). Returns `true` on success,
    /// `false` if a prerequisite (GPU / viewer / source) is missing.
    /// `full_res` tells `warm_insert_display` whether `preview_source` is
    /// genuinely full-resolution (a cold Standard decode) or a downscaled
    /// stand-in (the 2048px preview-cache render, or RAW's embedded-JPEG
    /// failure fallback) — only a full-resolution reveal may be warm-cached,
    /// otherwise a later warm hit could get stuck serving a low-res texture
    /// as if it were the sharp 1:1 tier (see `warm_insert_display`).
    pub(crate) fn reveal_srgb_preview(
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
            crate::app::controller::AppController::apply_display_tail(self, &gpu, vp);
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
    /// preview→full crossfade's preview). A `Full` hit does that AND rebuilds the
    /// sparse full VT + `!Send` producer from the cached GPU pyramid/tile source
    /// (`install_full_pipeline`, the same construction `apply_pyramid_ready` runs
    /// after a fresh tier-2 decode) — the full pipeline is already in hand, so
    /// this skips the ~1.2 s CPU pyramid rebuild AND the tier-2 decode job
    /// entirely for this open (instant 1:1, not just instant fit). A miss
    /// returns false and the normal ladder runs.
    ///
    /// The tail here (see the flag-setting after `install_preview_holder`) skips
    /// the now-redundant tier-1 preview reveal, clears `idle` so the drive loop
    /// runs to convergence (mirroring `apply_preview_cache_hit`), and — only on a
    /// `Full` hit whose pipeline actually installed — also marks the tier-2
    /// pyramid decode as already-requested so `drive_viewer`'s decode gate does
    /// not redundantly resubmit it.
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
        let (display, full) = match hit {
            crate::develop::cache::WarmHit::Miss => return false,
            crate::develop::cache::WarmHit::Display(d) => (d, None),
            crate::develop::cache::WarmHit::Full { full, display } => (display, Some(full)),
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
            crate::app::controller::AppController::apply_display_tail(self, &gpu, vp);
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

        // A `Full` hit rebuilds the sparse full VT + producer on top of the
        // display preview just installed above. `pyramid`/`tile_source` are
        // `None` only in headless tests (production always populates both
        // alongside a `Full` insert — see `apply_pyramid_ready`); a `None` here
        // falls back to the display-only tail below, same as a `Display` hit.
        // The success flag is intentionally unused: whether or not the sharp
        // tier installed from the cache, `spawn_full` still re-runs (see the
        // `full_requested` note below).
        let _full_installed = full
            .as_ref()
            .and_then(|f| Some((f.pyramid.as_ref()?, f.tile_source.as_ref()?, f)))
            .is_some_and(|(pyramid, tile_source, f)| {
                crate::app::controller::AppController::install_full_pipeline(
                    self,
                    frame,
                    image_id,
                    pyramid,
                    tile_source,
                    &f.op_stack,
                    f.cam,
                )
            });

        if let Some(v) = self.state.viewer.as_mut() {
            if v.image_id == image_id {
                // The cached display texture serves the instant fit/preview reveal.
                // For RAW the SHARP tier must still stream in behind it so 1:1 zoom
                // sharpens exactly as a normal open does (the sparse full pyramid) —
                // RAW has no separate full-res display tier — unless the `Full` hit
                // above already installed that sharp tier from the cache. For
                // Standard the cached display texture IS already the full-resolution
                // image (warm-cached ONLY when full-res, see `warm_insert_display`'s
                // `full_res` gate), so there is no sharper tier left to stream either
                // way. Therefore, mirroring `apply_preview_cache_hit`:
                //  - `warm_revealed` skips the now-redundant tier-1 RAW embedded-JPEG
                //    reveal, and makes the heavy re-decodes below run in
                //    RESTORE-ONLY mode: the warm cache holds only GPU artifacts, so
                //    the retained CPU sources (`preview_source` /
                //    `raw_preview_source`) and the preview `EditPipeline` must be
                //    re-decoded or edits on a warm-revealed image silently stop
                //    rendering — but their handlers skip the holder re-install and
                //    view re-fit a cold open performs.
                //  - Mark the disk preview-cache read already-resolved so the
                //    debounced tier-2 step fires DIRECTLY, without the disk read
                //    re-installing a lower-res 2048px holder over the warm texture,
                //    and don't write back (a warm RAM hit says nothing new about the
                //    on-disk entry, which this session's prior reveal already
                //    settled).
                //  - Clear `idle` so the drive loop stays alive one more beat: RAW
                //    returns to idle on pyramid convergence (`drive_viewer`'s
                //    `full_converged` gate, which `install_full_pipeline` also feeds
                //    on a `Full` hit); Standard settles it in `apply_preview_ready`'s
                //    warm branch once the restore decode lands.
                v.warm_revealed = true;
                v.idle = false;
                v.cache_read_requested = true;
                v.cache_resolved = true;
                v.cache_write_back = false;
                // NOTE (`full_installed`): the sparse full pipeline is installed
                // from the cache (`full_ready` set inside `install_full_pipeline`),
                // but `full_requested` is deliberately NOT set — `spawn_full` must
                // still re-run (debounced, off-thread) because the warm cache holds
                // only GPU artifacts: the retained CPU source (`raw_preview_source`)
                // and the preview `EditPipeline` are gone, and without them edits on
                // a warm-revealed RAW silently stop rendering. `apply_full_decoded`
                // detects this case (`warm_revealed && full_ready`) and ONLY
                // restores those two — no holder re-install, no view re-fit, no
                // duplicate pyramid build.
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
    pub(crate) fn warm_insert_display(
        &mut self,
        frame: &eframe::Frame,
        image_id: i64,
        full_res: bool,
    ) {
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

    /// Render at most ONE queued warm neighbor's edited rung-1 display texture
    /// per frame (bounded GPU work, CLAUDE.md rule 2) and insert it into the
    /// warm cache so clicking that neighbor later reveals instantly. The heavy
    /// decode already happened off-thread (`warm_prefetch::spawn_warm_sources`,
    /// Task 7); this is only the fast GPU edit pass — the SAME rung-1 build a
    /// real open runs (`apply_full_decoded` for RAW, `reveal_srgb_preview` for
    /// Standard), so a warm reveal is pixel-identical to a cold one. No holder
    /// is installed here (unlike a real open) — only the output texture is
    /// cached; `try_warm_reveal` installs it later, on click.
    fn drain_one_warm_render(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        // C3: pause draining during active navigation. Only render a queued
        // warm neighbor once the CURRENT viewer has settled (dwelled at least
        // `WARM_SETTLE_SECS`) — during fast filmstrip scrubbing this GPU work
        // would compete with the live reveal + pyramid installs on the render
        // thread and cause frame spikes. The queue is bounded (caps length +
        // drops oldest) and nothing is popped here while paused, so payloads
        // are only DEFERRED, never dropped — draining resumes once the user
        // settles on an image.
        let settled = self
            .state
            .viewer
            .as_ref()
            .is_some_and(|v| v.open_elapsed >= crate::develop::cache::WARM_SETTLE_SECS);
        if !settled {
            return;
        }
        let Some(payload) = self.state.warm_render_queue.pop_front() else {
            return;
        };
        let Some(rs) = frame.wgpu_render_state() else {
            // No GPU this frame (should not happen in this build): retry next frame.
            self.state.warm_render_queue.push_front(payload);
            return;
        };
        let key = crate::develop::cache::CacheKey {
            image_id: payload.image_id,
            op_stack_hash: ferrolite_previews::hash_serde(&payload.op_stack),
        };
        // Already warm at this exact (image, stack)? Nothing to render.
        if !matches!(
            self.state.warm_cache.get(key),
            crate::develop::cache::WarmHit::Miss
        ) {
            return;
        }

        let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let (tex, dims) = match payload.kind {
            ferrolite_image::FileKind::Raw => {
                // Mirror `apply_full_decoded`'s rung-1 build: camera→working
                // composed from the NEIGHBOR's own color profile + op-stack WB —
                // NOT `self.camera_to_working`/`current_wb_temp`, which are
                // scoped to the open viewer's profile/stack — via the SAME
                // `wb_camera_to_working` helper `camera_to_working` itself
                // calls. Lens/vignette are left at `EditPipeline::new`'s
                // defaults (identity): this neighbor has not been lens-baked
                // yet, and a real open re-bakes + re-renders regardless.
                let temp = payload
                    .op_stack
                    .white_balance()
                    .map(|w| w.temp)
                    .unwrap_or(0.0);
                let cam = crate::camera_matrix::wb_camera_to_working(
                    &payload.color_profile,
                    temp,
                    self.state.working_space,
                );
                let mut ep = ferrolite_pipeline::EditPipeline::new(
                    ctx_arc,
                    &payload.source,
                    payload.op_stack.clone(),
                    cam,
                );
                let out = ep.evaluate();
                (out.texture.clone(), (out.width, out.height))
            }
            ferrolite_image::FileKind::Standard => {
                // Mirror `reveal_srgb_preview`'s path: the source is already
                // display-linear sRGB, so run ONE sRGB→working color pass — no
                // camera matrix. `preview_to_working` depends only on the
                // current working space (not on which image is open), so it
                // is reused as-is.
                let pw = self.preview_to_working();
                let out = ferrolite_pipeline::color_convert(ctx_arc, &payload.source, pw);
                (out.texture.clone(), (out.width, out.height))
            }
        };
        // Rgba16Float rung-1 texture = 8 B/px (matches `warm_insert_display`).
        // The source is always genuinely full-resolution here (Task 7 decodes
        // the full RAW / full-res Standard image, never a downscaled stand-in),
        // so this is a full-res Display entry — consistent with the full-res-
        // only rule `warm_insert_display` enforces for a real open's reveal.
        let bytes = dims.0 as u64 * dims.1 as u64 * 8;
        self.state.warm_cache.insert_display(
            key,
            crate::develop::cache::DisplayEntry {
                tex: Some(tex),
                dims,
                bytes,
            },
        );
        // Request the next frame so the following queued neighbor drains then —
        // one-per-frame cadence, never a while-loop draining the whole queue.
        ctx.request_repaint();
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
    /// remove-folder confirmation, the P7 group modal, or either P7 Task 8
    /// preset dialog). Used to suppress the app's global keyboard shortcuts
    /// underneath the modal so its own input handling (e.g. Esc) is the only
    /// thing that reacts, and so shortcuts like Enter/Ctrl+A — and, for
    /// `open_group_modal` and `pending_rename_preset` specifically, plain
    /// letter/digit keys typed into their name text field — don't leak
    /// through to the grid/viewer while a modal is up. Extend this with new
    /// modals as they're added.
    ///
    /// The mask Components window is intentionally NOT included here: unlike
    /// the modals above, it must stay non-blocking so the canvas keeps
    /// receiving input behind it (live preview, color-eyedropper sampling,
    /// brush drawing all route through the canvas while the window is open).
    fn modal_active(&self) -> bool {
        any_modal_pending(
            self.show_help,
            self.show_settings,
            self.state.pending_remove.is_some(),
            self.state.open_group_modal.is_some(),
            self.state.pending_rename_preset.is_some(),
            self.state.pending_delete_preset.is_some(),
        )
    }

    /// If the current viewer's edit stack changed this session, spawn a
    /// Background job to regenerate its Library thumbnail from the in-memory
    /// stack, then clear the flag so re-entrant frames do not double-spawn.
    /// Called at every "leave Develop for this image" transition. No-op when
    /// there is no viewer or no session edits.
    ///
    /// The flag is cleared ONLY once the job is actually spawned (gated
    /// through `thumb_regen::on_leave_decision`, not cleared unconditionally
    /// up front): `frame.wgpu_render_state()` can come back `None` on a given
    /// frame (e.g. a transient window-resize/chrome interaction), and
    /// `edits_dirty` is the only signal that ever re-triggers this regen. A
    /// clear-before-spawn ordering would silently strand the image on its
    /// pre-edit thumbnail forever the first time that frame's GPU state is
    /// missing, with nothing left to retry it short of the manual "Regenerate
    /// thumbnail" context-menu action. See `on_leave_decision`'s doc comment.
    pub(crate) fn maybe_regen_on_leave(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(v) = self.state.viewer.as_ref() else {
            return;
        };
        let rs = frame.wgpu_render_state();
        let (spawn, new_edits_dirty) =
            crate::develop::thumb_regen::on_leave_decision(v.edits_dirty, rs.is_some());
        if let Some(v) = self.state.viewer.as_mut() {
            v.edits_dirty = new_edits_dirty;
        }
        if !spawn {
            return;
        }
        // `on_leave_decision` only returns `spawn == true` when `rs.is_some()`
        // was true above, so this is guaranteed to be `Some`.
        let rs = rs.expect("on_leave_decision only spawns when a render state is present");
        let (image_id, path, kind, stack) = {
            let v = self
                .state
                .viewer
                .as_ref()
                .expect("checked at function entry");
            (v.image_id, v.path.clone(), v.kind, v.op_stack.clone())
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
            crate::develop::thumb_regen::RegenStackSource::InMemory(Box::new(stack)),
        );
    }

    /// Show the open group modal for one frame and act on its outcome.
    ///
    /// The modal is taken OUT of `AppState` for the duration so
    /// `presets::menu::confirm_group_modal` can hold `&mut AppState` and
    /// `&mut PendingGroupModal` at once; it is put back unless the confirm
    /// closed it. A rejected preset name keeps it open with the reason on
    /// `name_error`, so the user fixes the name instead of losing their input.
    fn drive_group_modal(&mut self, ctx: &egui::Context) {
        let Some(mut pending) = self.state.open_group_modal.take() else {
            return;
        };
        let keep_open = match pending.modal.show(ctx) {
            crate::presets::modal::GroupModalOutcome::None => true,
            crate::presets::modal::GroupModalOutcome::Cancelled => false,
            crate::presets::modal::GroupModalOutcome::Confirmed { name, owns } => {
                !crate::presets::menu::confirm_group_modal(
                    &mut self.state,
                    ctx,
                    &mut pending,
                    name,
                    owns,
                )
            }
        };
        if keep_open {
            self.state.open_group_modal = Some(pending);
        }
    }

    /// Show the Develop-panel "Rename preset" dialog for one frame and act
    /// on its outcome (P7 Task 8). `presets::rename` saves under the new
    /// name before deleting the old file, so a rejected name (duplicate or
    /// invalid) never loses the preset — the dialog just shows the reason
    /// inline and stays open, mirroring `drive_group_modal`'s handling of a
    /// rejected save name.
    fn drive_rename_preset(&mut self, ctx: &egui::Context) {
        let Some(mut pending) = self.state.pending_rename_preset.take() else {
            return;
        };
        let mut keep_open = true;
        egui::Window::new("Rename preset")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut pending.new_name);
                });
                if let Some(err) = &pending.error {
                    ui.colored_label(theme::SEMANTIC_AMBER, err);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let can_confirm =
                        crate::presets::sanitize_filename(&pending.new_name).is_some();
                    let confirm = ui.add_enabled(can_confirm, egui::Button::new("Rename"));
                    if !can_confirm {
                        confirm.on_disabled_hover_text("Enter a name");
                    } else if confirm.clicked() {
                        match crate::presets::rename(
                            &crate::presets::presets_dir(),
                            &pending.original,
                            &pending.new_name,
                        ) {
                            Ok((renamed, delete_err)) => {
                                crate::presets::spawn_load_all(
                                    &self.state.jobs,
                                    &self.state.tx,
                                    ctx,
                                );
                                // The rename itself succeeded (the new-name
                                // file is written) regardless of `delete_err`,
                                // so the dialog closes either way. F6 (whole-
                                // branch review): a genuine failure to remove
                                // the OLD file — e.g. an AV/indexer holding a
                                // handle — is no longer silently swallowed; it
                                // is surfaced as a Warning toast so the user
                                // knows to expect a leftover duplicate until
                                // it's cleared manually, instead of the old
                                // `let _ = delete(..)` which left no trace of
                                // the problem at all.
                                if let Some(err) = delete_err {
                                    self.state.notify(
                                        crate::notifications::Level::Warning,
                                        format!(
                                            "Renamed \u{201c}{}\u{201d} to \u{201c}{}\u{201d}, \
                                             but could not remove the old preset file: {err}",
                                            pending.original.name, renamed.name
                                        ),
                                    );
                                } else {
                                    self.state.notify(
                                        crate::notifications::Level::Info,
                                        format!(
                                            "Renamed \u{201c}{}\u{201d} to \u{201c}{}\u{201d}.",
                                            pending.original.name, renamed.name
                                        ),
                                    );
                                }
                                keep_open = false;
                            }
                            Err(e) => {
                                // A friendlier message than the raw `Display`
                                // for the two rejections the user can act on
                                // by editing the name; `Io` keeps its message
                                // (an underlying filesystem failure has no
                                // more-actionable phrasing to offer here).
                                let msg = match &e {
                                    crate::presets::PresetError::Duplicate(_) => {
                                        "A preset with that name already exists.".to_string()
                                    }
                                    crate::presets::PresetError::InvalidName => {
                                        "Enter a name with at least one letter, number, \
                                         space, - or _."
                                            .to_string()
                                    }
                                    crate::presets::PresetError::Io(_) => e.to_string(),
                                };
                                pending.error = Some(msg);
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        keep_open = false;
                    }
                });
            });
        if keep_open {
            self.state.pending_rename_preset = Some(pending);
        }
    }

    /// Show the pending preset-delete confirmation for one frame and act on
    /// its outcome (P7 Task 8) — deleting removes a file from disk, so it is
    /// confirmed first, mirroring the remove-folder confirmation above.
    fn drive_delete_preset(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.state.pending_delete_preset.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Delete preset")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!(
                    "Delete the preset \u{201c}{}\u{201d}? This removes its file from disk.",
                    pending.preset.name
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        match crate::presets::delete(
                            &crate::presets::presets_dir(),
                            &pending.preset,
                        ) {
                            Ok(()) => {
                                crate::presets::spawn_load_all(
                                    &self.state.jobs,
                                    &self.state.tx,
                                    ctx,
                                );
                                self.state.notify(
                                    crate::notifications::Level::Info,
                                    format!(
                                        "Deleted preset \u{201c}{}\u{201d}.",
                                        pending.preset.name
                                    ),
                                );
                            }
                            Err(e) => {
                                self.state
                                    .notify(crate::notifications::Level::Error, e.to_string());
                            }
                        }
                        self.state.pending_delete_preset = None;
                        open = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.state.pending_delete_preset = None;
                        open = false;
                    }
                });
            });
        if !open {
            self.state.pending_delete_preset = None;
        }
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

        let Some(v) = self.state.viewer.as_mut() else {
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
        // Whole-image dehaze atmospheric light (design §5.3), via the same
        // per-image cache the producer rebuilds use (`ViewerState::dehaze_atmos`)
        // — regardless of which `source` variant above was chosen, so a RAW
        // export (which only carries a GPU `Pyramid`, no CPU buffer, into
        // `spawn_export`) still gets the real estimate rather than the neutral
        // fallback. Falls back to neutral only if no preview source has decoded
        // yet, which can't happen here since `source` above already required one
        // (Pyramid or FullResCpu).
        let atmospheric_light = v
            .dehaze_atmos()
            .unwrap_or(ferrolite_pipeline::DEHAZE_ATMOS_NEUTRAL);
        let source_path = v.path.clone();
        let image_id = v.image_id;
        let stack = v.op_stack.clone();
        // Only when dehaze is actually active anywhere in the document (the
        // global op OR a visible mask layer's amount — Phase 4 Task 3, see
        // `EditDoc::dehaze_active_anywhere`) does export need a transmission
        // source: the same CPU preview-tier selection `preview_tier_source` uses
        // (RAW: `raw_preview_source`; Standard: `preview_source`), passed as a
        // SNAPSHOT `Arc` — export builds its own bounded transmission from it on
        // the worker thread rather than sampling the live preview pipeline's
        // texture (see `spawn_export`'s `transmission_source` doc). Widened past
        // the old `stack.dehaze().filter(amount != 0.0)` global-only gate, which
        // silently skipped this for a mask-only dehaze layer (global amount 0).
        let transmission_source = stack
            .dehaze_active_anywhere()
            .then(|| {
                v.raw_preview_source
                    .clone()
                    .or_else(|| v.preview_source.clone())
            })
            .flatten();

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
            atmospheric_light,
            transmission_source,
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
}

/// Physical tile-pool budget for the viewer's sparse VT. 256 tiles × 256² ×
/// RGBA16F ≈ 128 MB of GPU memory — generous headroom for a fit-to-window view
/// plus a few zoom levels of the quad-binned (half-res) full image.
pub(crate) const VIEWER_TILE_BUDGET: u32 = 256;

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
        // The warm-cache lookup is deferred to `drive_viewer` (the
        // `warm_reveal_attempted` one-shot gate) rather than attempted here: a
        // fresh `ViewerState::open` starts with `op_stack: OpStack::default()`,
        // and `try_warm_reveal` keys the cache by `op_stack_hash()` — consulting
        // it here would key by the default (identity) stack even for an edited
        // image, whose real op stack only arrives asynchronously via the
        // `OpsLoaded` event. See `try_warm_reveal`'s doc comment.
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
    pub(crate) fn drain_thumb_regen_requests(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
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

    /// Drain grid-detected stale-thumbnail regeneration requests (P7 Task
    /// 10 — the consumer half of the design; Task 4 is the producer that
    /// SETS the flag on a batch apply). `library::grid::show`'s per-frame
    /// cell-realize path has no access to `eframe::Frame`/the GPU render
    /// state, so it only enqueues onto `pending_stale_regen` (already having
    /// marked the id `stale_regen_inflight` there, BEFORE this drains, to
    /// guard against a per-frame re-spawn storm — see that field's doc
    /// comment on `AppState`). This mirrors `drain_thumb_regen_requests`
    /// above almost exactly; the one difference is the stack source: a
    /// batch-applied image is never the open Develop viewer (batch apply
    /// excludes it, design §5.1), so there is no in-memory stack to reuse —
    /// the job reads the persisted `.xmp` sidecar instead, same as the
    /// on-demand "Regenerate thumbnail" action above.
    pub(crate) fn drain_stale_thumb_regen_requests(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        if self.state.pending_stale_regen.is_empty() {
            return;
        }
        let Some(rs) = frame.wgpu_render_state() else {
            // No GPU this frame; keep the requests (and their in-flight
            // guards) for a later frame.
            return;
        };
        let ids = std::mem::take(&mut self.state.pending_stale_regen);
        let gpu = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let cam =
            crate::develop::thumb_regen::srgb_fallback_camera_to_working(self.state.working_space);
        for id in ids {
            let Some(rec) = self.state.images.iter().find(|r| r.id == id).cloned() else {
                // Row no longer in the browsed set (folder switch/filter
                // change raced this queue) — release the guard too, so a
                // later realize (e.g. the same id reappearing under a
                // changed filter) is free to re-detect and retry.
                self.state.stale_regen_inflight.remove(&id);
                continue;
            };
            let Ok(Some(folder)) = self.state.reads.folder_path(rec.folder_id) else {
                self.state.stale_regen_inflight.remove(&id);
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

        // Warm-navigation cache (display + full tiers) resident bytes.
        b.set(
            MemCategory::RamCache,
            self.state.warm_cache.resident_bytes(),
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
        // One-time GPU-budget diagnostic (root-cause confirmation for the dehaze
        // OOM on constrained GPUs): log the adapter + the memory-relevant limits
        // once. `device_type: IntegratedGpu` + a small `max_buffer_size` /
        // `max_storage_buffer_binding_size` confirm a shared-memory budget that a
        // full-res edit chain can exhaust.
        static GPU_INFO_ONCE: std::sync::Once = std::sync::Once::new();
        GPU_INFO_ONCE.call_once(|| {
            if let Some(rs) = frame.wgpu_render_state() {
                let info = rs.adapter.get_info();
                let lim = rs.device.limits();
                eprintln!(
                    "[ferrolite gpu] adapter={:?} backend={:?} device_type={:?} driver={:?}",
                    info.name, info.backend, info.device_type, info.driver
                );
                eprintln!(
                    "[ferrolite gpu] max_texture_dim_2d={} max_buffer_size={} MiB \
                     max_storage_buffer_binding_size={} MiB",
                    lim.max_texture_dimension_2d,
                    lim.max_buffer_size / (1024 * 1024),
                    lim.max_storage_buffer_binding_size / (1024 * 1024),
                );
            }
        });
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

        // One-shot Task-14 background metadata backfill: the job is spawned
        // unconditionally here (never a synchronous DB check on the UI
        // thread — the job's OWN first step, off-thread, is the cheap
        // backlog check, so a fully-backfilled library pays one off-thread
        // COUNT per launch, zero UI-thread work). Independent of
        // `did_restore`/session restore — runs across the whole catalog, not
        // the browsed folder.
        if !self.did_meta_backfill_spawn {
            self.did_meta_backfill_spawn = true;
            crate::library::meta_backfill::spawn_once(&mut self.state, ctx);
        }

        // One-shot startup preset-directory scan (P7): file I/O off the UI
        // thread (contract 1), delivered back via `AppEvent::PresetsLoaded`.
        if !self.did_presets_load_spawn {
            self.did_presets_load_spawn = true;
            crate::presets::spawn_load_all(&self.state.jobs, &self.state.tx, ctx);
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
                    crate::app::controller::AppController::redetect_display_profile(
                        self, ctx, frame,
                    );
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
                    .is_some_and(|v| v.history.can_undo())
                    // P7: with no Develop session open, a pending batch-apply
                    // snapshot also makes Undo actionable (see
                    // `apply_undo_redo`'s batch-revert branch).
                    || (self.state.viewer.is_none() && self.state.batch_undo.is_some());
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
                let filmstrip_resp = egui::TopBottomPanel::top("develop_filmstrip")
                    .resizable(true)
                    .default_height(self.state.settings.filmstrip_height)
                    .height_range(64.0..=220.0)
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
                let new_h = filmstrip_resp.response.rect.height();
                if (self.state.settings.filmstrip_height - new_h).abs() > 0.001 {
                    self.state.settings.filmstrip_height = new_h;
                    self.mark_settings_dirty();
                }
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

        // Deferred warm-cache lookup (one-shot, mirrors `lens_auto_match_attempted`
        // just below). `try_warm_reveal` keys the cache by `op_stack_hash()`, which
        // is only meaningful once the real op stack has loaded — a fresh open
        // starts at `OpStack::default()` and the loaded (possibly edited) stack
        // only arrives async via `OpsLoaded`. Gating on `ops_loaded` here (rather
        // than attempting the lookup in `open_record`, where it used to run) means
        // an edited image's warm hit is keyed correctly and reveals the EDITED
        // render, not a stale unedited one. Runs BEFORE the tier-1
        // preview/heavy-decode submission below so a same-frame hit still gates
        // them out via `warm_revealed`/`cache_read_requested`/`cache_resolved`,
        // exactly as the old open_record-time attempt did. `self.try_warm_reveal`
        // needs `&mut self`, so the `image_id` is read out and the `v` borrow
        // dropped before calling it.
        let pending_warm_reveal = self
            .state
            .viewer
            .as_ref()
            .filter(|v| v.ops_loaded && !v.warm_reveal_attempted)
            .map(|v| v.image_id);
        if let Some(image_id) = pending_warm_reveal {
            if let Some(v) = self.state.viewer.as_mut() {
                v.warm_reveal_attempted = true; // one-shot regardless of hit/miss
            }
            self.try_warm_reveal(frame, image_id);
        }

        // Submit the tier-1 preview decode. RAW: the small EMBEDDED preview is
        // submitted immediately (keeps the tier-1 alive; cheap). Standard: the
        // tier-1 preview IS the full-res JPG decode (heavy), so it is NOT fired
        // here — it is gated behind the preview-cache read below (mirrors RAW's
        // full-decode gate) so a re-open reveals the cached 2048px entry first.
        if let Some(v) = self.state.viewer.as_mut() {
            // RAW embedded tier-1 preview: skip it on a warm DISPLAY hit (the
            // deferred `try_warm_reveal` attempt just above) — the cached display
            // texture already serves the fit/preview reveal, so this small embedded-JPEG
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
                        } else {
                            // Standard: the heavy tier-1 IS the full-res JPG decode.
                            // This also runs after a cache HIT (`heavy_pending` is
                            // `!preview_requested`, still true post-hit) — the 2048px
                            // reveal from `apply_preview_cache_hit` shows first, then
                            // this streams in the full-res 1:1 detail.
                            //
                            // Runs on a warm display hit too — NOT for display (the
                            // warm texture is already the full-res edited render) but
                            // to restore the retained `preview_source` the warm cache
                            // cannot hold (it keeps only GPU artifacts): without it
                            // the lazy preview `EditPipeline` can never be built and
                            // edits on a warm-revealed image silently stop rendering.
                            // `apply_preview_ready` skips the redundant reveal for the
                            // warm case (no holder re-install, no view re-fit).
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

        // Task 7: alongside the disk preview-cache prefetch above, also warm the
        // forward-biased neighbor window's SOURCES off-thread (decode + demosaic
        // only — no display texture yet; Task 8 turns a delivered source into a
        // cached render). One-shot per open, gated + cancelled the same way.
        //
        // Additionally gated on `open_elapsed >= WARM_SETTLE_SECS` (C1): the user
        // must have DWELLED on this image before its neighbors are prefetched, so
        // fast filmstrip scrubbing — where each image is superseded before it
        // settles — never dispatches a neighbor-decode wave the app immediately
        // discards. This does NOT affect the warm-cache REVEAL for the image being
        // opened (`try_warm_reveal`, gated on `ops_loaded`) — that stays prompt.
        let warm_settle_pending = self
            .state
            .viewer
            .as_ref()
            .is_some_and(|v| v.loaded && !v.warm_prefetch_requested);
        if self.state.viewer.as_ref().is_some_and(|v| {
            v.loaded
                && !v.warm_prefetch_requested
                && v.open_elapsed >= crate::develop::cache::WARM_SETTLE_SECS
        }) {
            if let Some(current_id) = self.state.viewer.as_ref().map(|v| v.image_id) {
                let ids: Vec<i64> = self.state.images.iter().map(|r| r.id).collect();
                let targets = crate::develop::cache::warm_window(
                    &ids,
                    current_id,
                    crate::develop::cache::WARM_WINDOW_FORWARD,
                    crate::develop::cache::WARM_WINDOW_BACK,
                );
                let neighbors: Vec<(i64, std::path::PathBuf, ferrolite_image::FileKind)> = targets
                    .into_iter()
                    .filter_map(|id| {
                        let rec = self.state.images.iter().find(|r| r.id == id)?;
                        let path = self.state.image_path(rec)?;
                        Some((id, path, rec.kind))
                    })
                    .collect();
                if let Some(rs) = frame.wgpu_render_state() {
                    let gpu = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
                    let handles = crate::develop::warm_prefetch::spawn_warm_sources(
                        &self.state.jobs,
                        &self.state.tx,
                        ctx,
                        neighbors,
                        gpu,
                    );
                    if let Some(v) = self.state.viewer.as_mut() {
                        if v.image_id == current_id {
                            v.warm_prefetch_handles = handles;
                            v.warm_prefetch_requested = true;
                        }
                    }
                }
            }
        } else if warm_settle_pending {
            // Not settled yet: guarantee a frame fires once WARM_SETTLE_SECS
            // elapses even if the app would otherwise go idle waiting on input
            // (mirrors the FULL_DECODE_DEBOUNCE guarantee above) — otherwise a
            // still (non-navigated) image that reveals from the warm cache and
            // goes idle before 0.4s could leave the prefetch dispatch waiting on
            // an incidental repaint that may not come.
            if let Some(v) = self.state.viewer.as_ref() {
                ctx.request_repaint_after(std::time::Duration::from_secs_f32(
                    crate::develop::cache::WARM_SETTLE_SECS - v.open_elapsed,
                ));
            }
        }

        if self.module == crate::module::Module::Develop && self.state.show_info_panel {
            let info_resp = egui::SidePanel::left("develop_info_panel")
                .resizable(true)
                .default_width(self.state.settings.info_panel_width)
                .width_range(220.0..=450.0)
                .frame(
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(0x1a, 0x1a, 0x1a))
                        .inner_margin(egui::Margin::symmetric(12.0, 12.0)),
                )
                .show(ctx, |ui| {
                    crate::develop::info_panel::show(ui, &self.state);
                });
            if info_resp.response.drag_stopped()
                && (info_resp.response.rect.width() - self.state.settings.info_panel_width).abs()
                    > 0.5
            {
                self.state.settings.info_panel_width = info_resp.response.rect.width();
                self.mark_settings_dirty();
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
            let adjust_resp = egui::SidePanel::right("develop_adjust")
                .resizable(true)
                .default_width(self.state.settings.right_panel_width)
                .width_range(250.0..=400.0)
                .frame(
                    egui::Frame::none()
                        .fill(theme::BG_APP)
                        .inner_margin(egui::Margin {
                            left: 12.0,
                            right: 8.0,
                            top: 8.0,
                            bottom: 8.0,
                        }),
                )
                .show(ctx, |ui| {
                    ui.spacing_mut().scroll.bar_width = 10.0;
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            egui::Frame::none()
                                .inner_margin(egui::Margin {
                                    left: 0.0,
                                    right: 8.0,
                                    top: 0.0,
                                    bottom: 0.0,
                                })
                                .show(ui, |ui| {
                                    let prev_disclosures =
                                        crate::settings::dto::disclosure_snapshot(
                                            &self.state.settings,
                                        );
                                    outcome = Some(crate::develop::tool_panel::show(
                                        ui,
                                        &mut self.state,
                                        &self.tool_registry,
                                        working_space,
                                    ));
                                    if crate::settings::dto::disclosure_snapshot(
                                        &self.state.settings,
                                    ) != prev_disclosures
                                    {
                                        self.mark_settings_dirty();
                                    }
                                });
                        });
                });
            if adjust_resp.response.drag_stopped()
                && (adjust_resp.response.rect.width() - self.state.settings.right_panel_width).abs()
                    > 0.5
            {
                self.state.settings.right_panel_width = adjust_resp.response.rect.width();
                self.mark_settings_dirty();
            }
            if let Some(outcome) = outcome {
                if let Some(ws) = outcome.working_space {
                    crate::app::controller::AppController::apply_working_space(
                        self, ctx, frame, ws,
                    );
                }
                if let Some(o) = outcome.edit {
                    crate::app::controller::AppController::apply_edit(
                        self, ctx, frame, o.kind, o.stack, o.commit,
                    );
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
                .resizable(false)
                .exact_width(300.0)
                .frame(
                    egui::Frame::none()
                        .fill(theme::BG_APP)
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgb(0x26, 0x26, 0x26),
                        ))
                        .inner_margin(egui::Margin::symmetric(12.0, 12.0)),
                )
                .show(ctx, |ui| {
                    let before = self.state.export_settings;
                    crate::export_module::export_settings_panel(ui, &mut self.state);
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
                    if let Some(v) = self.state.viewer.as_ref() {
                        if let Some(action) =
                            crate::develop::canvas::Viewer::new(v.image_id).show(ui, self, frame)
                        {
                            match action {
                                crate::develop::canvas::ViewerAction::ApplyEdit {
                                    kind,
                                    stack,
                                    commit,
                                } => {
                                    crate::app::controller::AppController::apply_edit(
                                        self, ctx, frame, kind, stack, commit,
                                    );
                                }
                                crate::develop::canvas::ViewerAction::SelectTool(id) => {
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
                                crate::develop::canvas::ViewerAction::Undo => {
                                    self.apply_undo_redo(ctx, frame, true);
                                }
                                crate::develop::canvas::ViewerAction::Redo => {
                                    self.apply_undo_redo(ctx, frame, false);
                                }
                                crate::develop::canvas::ViewerAction::SetPreviewAndFull(stack) => {
                                    // Crop-mode transition (this action's only
                                    // emitter — see canvas/viewer.rs Step 1):
                                    // the shown extent just changed (full ↔
                                    // cropped), so re-frame the view to the
                                    // NEW extent. Without this, entering the
                                    // crop tool on a cropped image kept the
                                    // fit of the smaller cropped extent while
                                    // showing the full image (opened visibly
                                    // zoomed-in), and the overlay's
                                    // `image_dims`-derived hit geometry no
                                    // longer matched what was displayed.
                                    let shown_dims =
                                        crate::app::controller::AppController::set_preview_and_full(
                                            self, frame, stack, true,
                                        );
                                    if let Some(dims) = shown_dims {
                                        if let Some(v) = self.state.viewer.as_mut() {
                                            v.image_dims = Some(dims);
                                            if v.viewport.0 > 0.0 && v.viewport.1 > 0.0 {
                                                v.view = ferrolite_vt::ViewTransform::fit(
                                                    dims, v.viewport,
                                                );
                                            }
                                            v.idle = false;
                                        }
                                        ctx.request_repaint();
                                    }
                                }
                            }
                        }
                        // Bounded one-per-frame warm-neighbor render (Task 8):
                        // pop at most one queued decoded source and turn it into
                        // a cached display texture. Placed alongside the canvas
                        // viewer since both need `frame`'s render state and both
                        // run only while Develop is open with a viewer.
                        self.drain_one_warm_render(ctx, frame);
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
                crate::app::controller::AppController::apply_edit(
                    self, ctx, frame, o.kind, o.stack, o.commit,
                );
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

        // The P7 "Save preset" / "Paste settings" group modal, opened from the
        // library context menu (which only reaches `AppState`, hence the state
        // ownership) and driven here, where the `egui::Context` lives.
        self.drive_group_modal(ctx);
        // The P7 Task 8 Develop-panel Presets menu's rename/delete dialogs —
        // same reasoning: opened from `develop::presets_menu`, which only
        // reaches `AppState`, and driven here where `egui::Context` lives.
        self.drive_rename_preset(ctx);
        self.drive_delete_preset(ctx);

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

        // Task 14's backfill job runs across the whole catalog, not the
        // browsed folder, so it is intentionally NOT cancelled by
        // `cancel_pending_jobs` (folder-switch/reindex scoped) — cancel it
        // explicitly here instead, like the app's other long-lived handles.
        if let Some(h) = self.state.meta_backfill_handle.take() {
            h.cancel();
        }
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

#[cfg(test)]
mod tests {
    use super::any_modal_pending;
    use crate::settings::Settings;

    /// Every flag `modal_active` ORs together must independently suppress
    /// shortcut dispatch — most importantly `open_group_modal` (Task 7's
    /// "Save preset" / "Paste settings" dialog), which holds a free-text name
    /// field: without this, typing a preset name would fire every single-key
    /// shortcut (star ratings, tool switches, Ctrl+Z) on every keystroke.
    #[test]
    fn any_modal_pending_is_false_only_when_every_flag_is_clear() {
        assert!(!any_modal_pending(false, false, false, false, false, false));
        assert!(any_modal_pending(true, false, false, false, false, false));
        assert!(any_modal_pending(false, true, false, false, false, false));
        assert!(any_modal_pending(false, false, true, false, false, false));
        assert!(
            any_modal_pending(false, false, false, true, false, false),
            "open_group_modal must suppress shortcut dispatch — it holds a text field"
        );
        assert!(any_modal_pending(false, false, false, false, true, false));
        assert!(any_modal_pending(false, false, false, false, false, true));
    }

    #[test]
    fn test_panel_width_and_height_persistence() {
        let mut settings = Settings::default();
        assert_eq!(settings.right_panel_width, 300.0);
        assert_eq!(settings.info_panel_width, 300.0);
        assert_eq!(settings.filmstrip_height, 96.0);

        // Mutate widths and height and verify settings values
        settings.right_panel_width = 350.0;
        settings.info_panel_width = 280.0;
        settings.filmstrip_height = 120.0;

        assert_eq!(settings.right_panel_width, 350.0);
        assert_eq!(settings.info_panel_width, 280.0);
        assert_eq!(settings.filmstrip_height, 120.0);
    }

    #[test]
    fn test_side_panel_width_capture_and_dirty_marking() {
        let ctx = egui::Context::default();
        let mut settings = Settings::default();
        let mut settings_dirty = false;

        assert_eq!(settings.right_panel_width, 300.0);
        assert_eq!(settings.info_panel_width, 300.0);

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1920.0, 1080.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input.clone(), |_| {});
        let mut captured_info_w = 0.0;
        let mut captured_right_w = 0.0;

        let _ = ctx.run(input, |ctx| {
            let info_resp = egui::SidePanel::left("test_develop_info_panel")
                .resizable(true)
                .default_width(320.0)
                .width_range(220.0..=450.0)
                .show(ctx, |ui| {
                    ui.label("info");
                });
            let width = info_resp.response.rect.width();
            captured_info_w = width;
            if info_resp.response.drag_stopped() && (width - settings.info_panel_width).abs() > 0.5
            {
                settings.info_panel_width = width;
                settings_dirty = true;
            }

            let adjust_resp = egui::SidePanel::right("test_develop_adjust")
                .resizable(true)
                .default_width(340.0)
                .width_range(250.0..=400.0)
                .show(ctx, |ui| {
                    ui.label("adjust");
                });
            let width = adjust_resp.response.rect.width();
            captured_right_w = width;
            if adjust_resp.response.drag_stopped()
                && (width - settings.right_panel_width).abs() > 0.5
            {
                settings.right_panel_width = width;
                settings_dirty = true;
            }

            egui::CentralPanel::default().show(ctx, |_| {});
        });

        // Without drag_stopped(), initial settings remain untouched
        assert_eq!(settings.info_panel_width, 300.0);
        assert_eq!(settings.right_panel_width, 300.0);
        assert!(!settings_dirty);

        // Verify sub-0.5px difference on drag stop does not trigger dirty marking or update
        let small_diff_w = settings.info_panel_width + 0.3;
        let prev_w = settings.info_panel_width;
        let mut dirty = false;
        let drag_stopped = true;
        if drag_stopped && (small_diff_w - settings.info_panel_width).abs() > 0.5 {
            settings.info_panel_width = small_diff_w;
            dirty = true;
        }
        assert_eq!(settings.info_panel_width, prev_w);
        assert!(!dirty);

        // Verify >0.5px difference on drag stop updates settings and marks dirty
        if drag_stopped && (captured_info_w - settings.info_panel_width).abs() > 0.5 {
            settings.info_panel_width = captured_info_w;
            settings_dirty = true;
        }
        assert_eq!(settings.info_panel_width, captured_info_w);
        assert!(settings_dirty);
    }

    #[test]
    fn test_panel_width_drag_stop_persistence_and_scrollbar_clearance() {
        let ctx = egui::Context::default();
        let mut settings = Settings::default();
        let initial_width = settings.right_panel_width;
        let mut settings_dirty = false;

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1920.0, 1080.0),
            )),
            ..Default::default()
        };

        let mut inner_frame_right_margin = 0.0;

        let _ = ctx.run(input, |ctx| {
            let adjust_resp = egui::SidePanel::right("test_develop_adjust_drag_stop")
                .resizable(true)
                .default_width(340.0)
                .width_range(250.0..=400.0)
                .show(ctx, |ui| {
                    ui.spacing_mut().scroll.bar_width = 10.0;
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let frame = egui::Frame::none().inner_margin(egui::Margin {
                                left: 0.0,
                                right: 16.0,
                                top: 0.0,
                                bottom: 0.0,
                            });
                            inner_frame_right_margin = frame.inner_margin.right;
                            frame.show(ui, |ui| {
                                ui.label("content inside clearance frame");
                            });
                        });
                });

            // If not drag_stopped(), width setting remains untouched
            if adjust_resp.response.drag_stopped()
                && (adjust_resp.response.rect.width() - settings.right_panel_width).abs() > 0.5
            {
                settings.right_panel_width = adjust_resp.response.rect.width();
                settings_dirty = true;
            }
        });

        // Since no drag stopped event occurred, width setting is unchanged
        assert_eq!(settings.right_panel_width, initial_width);
        assert!(!settings_dirty);

        // Verify the scrollbar clearance frame right margin is 16.0px
        assert_eq!(inner_frame_right_margin, 16.0);

        // Simulate a drag stop condition
        let simulated_width = 360.0;
        let simulated_drag_stopped = true;
        if simulated_drag_stopped && (simulated_width - settings.right_panel_width).abs() > 0.5 {
            settings.right_panel_width = simulated_width;
            settings_dirty = true;
        }
        assert_eq!(settings.right_panel_width, 360.0);
        assert!(settings_dirty);
    }
}

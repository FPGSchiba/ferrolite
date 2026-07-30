use crate::app::FerroliteApp;
use crate::events::AppEvent;
use crate::viewer;

pub struct AppController;

impl AppController {
    pub fn handle_events(app: &mut FerroliteApp, ctx: &egui::Context, frame: &eframe::Frame) {
        // Drain the stashed backlog first (FIFO) up to the per-frame budget.
        let mut uploads_this_frame = 0usize;
        {
            let take = app
                .state
                .pending_uploads
                .len()
                .min(crate::app::MAX_THUMB_UPLOADS_PER_FRAME);
            if take > 0 {
                let backlog: Vec<(i64, Vec<u8>, u32, u32)> =
                    app.state.pending_uploads.drain(..take).collect();
                for (id, rgba, w, h) in backlog {
                    app.state.upload_thumbnail(ctx, id, rgba, w, h);
                    uploads_this_frame += 1;
                }
                app.state.dirty = true;
            }
        }

        let mut ingest_done = false;
        let mut events_this_frame = 0usize;
        while let Ok(event) = app.state.rx.try_recv() {
            events_this_frame += 1;
            let mut matched = true;
            match &event {
                AppEvent::PreviewReady { image_id, linear } => {
                    Self::apply_preview_ready(app, frame, ctx, *image_id, linear);
                }
                AppEvent::FullDecoded {
                    image_id,
                    image,
                    color_profile,
                } => {
                    Self::apply_full_decoded(app, frame, ctx, *image_id, image, color_profile);
                }
                AppEvent::PyramidReady {
                    image_id,
                    tile_source,
                    gpu_pyramid,
                } => {
                    Self::apply_pyramid_ready(app, frame, *image_id, tile_source, gpu_pyramid);
                }
                AppEvent::FullFailed { image_id } => {
                    let image_id = *image_id;
                    let need_fallback = matches!(
                        app.state.viewer.as_ref(),
                        Some(v) if v.image_id == image_id
                            && v.kind == ferrolite_image::FileKind::Raw
                            && !v.loaded
                    );
                    // `full_res = false`: the embedded JPEG fallback is a
                    // downscaled stand-in, not the full RAW decode — must not
                    // be warm-cached as if it were the sharp 1:1 tier.
                    if need_fallback && app.reveal_srgb_preview(frame, image_id, false) {
                        eprintln!(
                            "ferrolite: full decode failed for #{image_id}; showing embedded JPEG fallback"
                        );
                    }
                    if let Some(v) = app.state.viewer.as_mut() {
                        if v.image_id == image_id {
                            v.idle = true;
                        }
                    }
                }
                AppEvent::PreviewCacheHit { image_id, linear } => {
                    Self::apply_preview_cache_hit(app, frame, *image_id, linear);
                    ctx.request_repaint();
                }
                AppEvent::PreviewCacheMiss { image_id } => {
                    Self::apply_preview_cache_miss(app, *image_id);
                    ctx.request_repaint();
                }
                AppEvent::OpsLoaded { image_id, stack } => {
                    let mut rebake: Option<ferrolite_pipeline::LensCorrection> = None;
                    let mut just_loaded = false;
                    if let Some(v) = app.state.viewer.as_mut() {
                        if v.image_id == *image_id && !v.ops_loaded {
                            v.ops_loaded = true;
                            just_loaded = true;
                            if !stack.is_identity() {
                                v.history =
                                    crate::develop::history::History::new(stack.clone(), 100);
                                Self::set_preview_and_full(app, frame, stack.clone(), true);
                            }
                            if let Some(lc) = stack.lens_correction() {
                                if crate::develop::lens_bake::needs_rebake_on_load(&lc) {
                                    rebake = Some(lc);
                                }
                            }
                        }
                    }
                    if just_loaded {
                        app.state.tool_state.ensure_valid_tab(&app.tool_registry);
                    }
                    if let Some(lc) = rebake {
                        if let Some(db) = app.state.lens_db.clone() {
                            let image_id = *image_id;
                            let handle = crate::develop::lens_bake::spawn_lens_bake(
                                &app.state.jobs,
                                &db,
                                &app.state.tx,
                                ctx,
                                image_id,
                                lc,
                            );
                            if let Some(v) = app.state.viewer.as_mut() {
                                if v.image_id == image_id {
                                    v.lens_bake_handle = Some(handle);
                                }
                            }
                        }
                    }
                    Self::try_auto_match_lens(app, *image_id);
                }
                AppEvent::MetaLoaded { image_id, meta } => {
                    if let Some(v) = app.state.viewer.as_mut() {
                        if v.image_id == *image_id && !v.meta_loaded {
                            v.meta_loaded = true;
                            v.meta = meta.clone();
                        }
                    }
                    Self::try_auto_match_lens(app, *image_id);
                    ctx.request_repaint();
                }
                AppEvent::IngestDone => {
                    ingest_done = true;
                    matched = false;
                }
                AppEvent::HistogramReady { image_id, bins } => {
                    if let Some(v) = app.state.viewer.as_mut() {
                        if v.image_id == *image_id {
                            if !bins.is_empty() {
                                v.histogram.bins = Some(bins.clone());
                            }
                            v.histogram.inflight = false;
                        }
                    }
                    ctx.request_repaint();
                    matched = false;
                }
                AppEvent::ExportProgress {
                    image_id: _,
                    done,
                    total,
                } => {
                    if let Some(a) = app.state.export_activity.as_mut() {
                        a.set_tiles(*done, *total);
                    }
                    ctx.request_repaint();
                }
                AppEvent::ExportFinished {
                    image_id: _,
                    ok,
                    message,
                } => {
                    if let Some(a) = app.state.export_activity.as_mut() {
                        if a.kind == crate::export::ExportKind::Single {
                            a.item_finished(*ok, message.clone());
                        }
                    }
                    app.state.notify(
                        if *ok {
                            crate::notifications::Level::Info
                        } else {
                            crate::notifications::Level::Error
                        },
                        message.clone(),
                    );
                    ctx.request_repaint();
                }
                AppEvent::ExportItemStarted { name } => {
                    if let Some(a) = app.state.export_activity.as_mut() {
                        a.start_item(Some(name.clone()));
                    }
                    ctx.request_repaint();
                }
                AppEvent::DisplayProfileResolved {
                    lut,
                    name,
                    generation,
                } => {
                    if *generation == app.state.display_detect_gen {
                        app.state.display_profile_name = name.clone();
                        app.state.display_lut = lut.clone();
                        if let Some(rs) = frame.wgpu_render_state() {
                            let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
                            let renderer = rs.renderer.read();
                            if let Some(vp) =
                                renderer.callback_resources.get::<viewer::ViewerPipelines>()
                            {
                                Self::apply_display_tail(app, &gpu, vp);
                            }
                        }
                        ctx.request_repaint();
                    }
                }
                AppEvent::LensBaked { image_id, result } => {
                    Self::apply_lens_baked(app, frame, *image_id, result);
                    ctx.request_repaint();
                }
                _ => {
                    matched = false;
                }
            }
            if !matched {
                if let Some((id, rgba, w, h)) = app.state.apply(event) {
                    if uploads_this_frame < crate::app::MAX_THUMB_UPLOADS_PER_FRAME {
                        app.state.upload_thumbnail(ctx, id, rgba, w, h);
                        uploads_this_frame += 1;
                    } else {
                        app.state.thumb_uploading.insert(id);
                        app.state.pending_uploads.push((id, rgba, w, h));
                    }
                }
            }
            app.state.dirty = true;
        }

        if !app.state.pending_uploads.is_empty() {
            ctx.request_repaint();
        }
        if let Some(done_at) = app
            .state
            .export_activity
            .as_ref()
            .and_then(|a| a.completed_at)
        {
            const EXPORT_DONE_LINGER: std::time::Duration = std::time::Duration::from_secs(4);
            let elapsed = done_at.elapsed();
            if elapsed >= EXPORT_DONE_LINGER {
                app.state.export_activity = None;
            } else {
                ctx.request_repaint_after(EXPORT_DONE_LINGER - elapsed);
            }
        }
        crate::diag::add_events(events_this_frame);
        crate::diag::add_uploads(uploads_this_frame);
        app.drain_thumb_regen_requests(ctx, frame);
        if ingest_done {
            app.state.reload_vocab();
        }
    }

    /// Handle a tier-1 preview: run ONE sRGB→working color pass on the
    /// already-off-thread-converted linear buffer, build the rung-1
    /// `VirtualTexture` wrapping its output directly (no throwaway upload),
    /// stash it (+ GpuContext) in eframe's `callback_resources`, and fit the
    /// view. The full 9-node preview `EditPipeline` stays lazy — built on the
    /// first edit by `set_preview_and_full`, not here. Stale events (no open
    /// viewer, or a different image_id) are dropped — the user may have
    /// closed/switched the viewer mid-decode.
    pub fn apply_preview_ready(
        app: &mut FerroliteApp,
        frame: &eframe::Frame,
        ctx: &egui::Context,
        image_id: i64,
        linear: &ferrolite_image::LinearRgbaF32,
    ) {
        let Some(v) = app.state.viewer.as_mut() else {
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

        // Warm-revealed open: the cached (already-edited) display texture is on
        // screen — this decode ran ONLY to restore the retained source so the
        // lazy preview `EditPipeline` can be built on the first edit
        // (`set_preview_and_full`; without a source, edits on a warm-revealed
        // image silently stopped rendering). Re-running the reveal below would
        // re-install the holder and re-fit the view seconds after the instant
        // warm reveal. Standard has nothing left to converge on — settle idle.
        if v.warm_revealed {
            v.idle = true;
            return;
        }

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
        let revealed = app.reveal_srgb_preview(frame, image_id, true);
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
        let write_back = app.state.viewer.as_ref().and_then(|v| {
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
                &app.state.jobs,
                std::sync::Arc::clone(&app.state.preview_store),
                &app.state.tx,
                ctx,
                path,
                op_stack,
                app.state.working_space,
                color_profile,
                render,
                crate::develop::preview_cache::standard_writeback_matrix(),
                ferrolite_previews::DEFAULT_CACHE_CAP_BYTES,
                image_id,
            );
        }
    }

    /// Handle a tier-2 full decode: build a `PyramidTileSource` from the
    /// display-linear image, wrap it as a sparse (rung-4) `VirtualTexture`,
    /// store it alongside the preview in `ViewerGpu`, and begin the preview→full
    /// crossfade. Stale events (no open viewer / different image_id) are dropped.
    pub fn apply_full_decoded(
        app: &mut FerroliteApp,
        frame: &eframe::Frame,
        ctx: &egui::Context,
        image_id: i64,
        image: &ferrolite_image::LinearRgbaF32,
        color_profile: &ferrolite_decode::ColorProfile,
    ) {
        let Some(v) = app.state.viewer.as_mut() else {
            return; // viewer closed while decoding
        };
        if v.image_id != image_id {
            return; // stale: a different image is now open
        }
        v.color_profile = color_profile.clone();
        let is_raw = v.kind == ferrolite_image::FileKind::Raw;
        // Warm `Full` hit: the sparse pipeline was installed from the cache, but
        // the warm cache holds only GPU artifacts — the retained CPU source
        // (`raw_preview_source`) and the preview `EditPipeline` are gone, and
        // without them edits silently stop rendering. This decode re-ran ONLY to
        // restore those two; the tail below returns early before the cold-open
        // holder install / view re-fit / pyramid build / write-backs.
        let warm_refresh = v.warm_revealed && v.full_ready;
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };

        // `v` only guarded staleness above; release the borrow before taking the
        // renderer lock so we can re-borrow afterwards. (Both live on `self` but
        // do not alias.)
        let _ = v;

        // Compute camera→working BEFORE any exclusive `viewer` borrow below:
        // `camera_to_working` itself borrows `app.state.viewer` immutably.
        let cam = app.camera_to_working(app.current_wb_temp());
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
            match app.state.viewer.as_mut() {
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

        // Warm restore-only tail: hand the on-screen single texture to the
        // just-rebuilt live preview pipeline (the same edited render the warm
        // texture already shows) and stop — the warm reveal already installed
        // the holder/view, the cached sparse full pipeline is producing, and
        // the pyramid build / write-back / warm-insert tails below are for
        // cold opens only.
        if warm_refresh {
            if let Some((tex, dims)) = raw_preview.as_ref() {
                let mut renderer = rs.renderer.write();
                if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                    if g.image_id == image_id {
                        g.preview
                            .update_single_from_texture(std::sync::Arc::clone(tex), *dims);
                    }
                }
            }
            app.mark_histogram_dirty();
            return;
        }

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
            Self::apply_display_tail(app, &gpu, vp);
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
            if let Some(v) = app.state.viewer.as_mut() {
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
            //
            // Perf fix D: cancellation of a superseded build is cooperative and
            // only checked BETWEEN the two monolithic steps inside the job, so
            // fast filmstrip navigation could otherwise pile up remnant builds
            // that monopolize the worker pool and starve the settled image's
            // `Visible` full decode. Bound concurrency with a process-global
            // permit (`develop::cache::try_acquire_pyramid_permit`): if a slot is
            // free, submit now; otherwise defer via `needs_pyramid` and let
            // `drive_viewer` retry once a permit frees, for the CURRENT viewer
            // only (a superseded image's flag is simply never revisited — see
            // `ViewerState::needs_pyramid`).
            match crate::develop::cache::try_acquire_pyramid_permit() {
                Some(permit) => {
                    Self::submit_pyramid_build(
                        app,
                        frame,
                        ctx,
                        image_id,
                        std::sync::Arc::clone(&image_arc),
                        permit,
                    );
                }
                None => {
                    if let Some(v) = app.state.viewer.as_mut() {
                        if v.image_id == image_id {
                            v.needs_pyramid = true;
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
        if preview_installed {
            // Snapshot the viewer inputs, then release the borrow before the
            // job submit (which borrows other `app.state` fields).
            let write_back = app.state.viewer.as_ref().and_then(|v| {
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
                    &ferrolite_color::working_to_display(app.state.working_space),
                    &cam,
                );
                // Reuse the reveal `Arc` (no second full-buffer clone) and let
                // the job assemble the key off-thread (`key_for` does an
                // `fs::metadata` stat — never on the UI thread).
                crate::develop::preview_cache::spawn_cache_write(
                    &app.state.jobs,
                    std::sync::Arc::clone(&app.state.preview_store),
                    &app.state.tx,
                    ctx,
                    path,
                    op_stack,
                    app.state.working_space,
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
            app.warm_insert_display(frame, image_id, true);
        }
        app.mark_histogram_dirty();
    }

    /// Submit the off-thread build of both full-res pyramids (the sparse-VT
    /// CPU tile source and the GPU-resident edit pyramid) for `image_id` on a
    /// `ferrolite-jobs` Background worker — CPU box-downsample heavy (~1.2 s
    /// combined), so this must never run on the UI thread (CLAUDE.md rule 1;
    /// this was the original open freeze). `GpuContext` is `Send + Sync` (Arc
    /// device/queue handles), as are `PyramidTileSource` and
    /// `GpuPyramidSource`, so both build off-thread and are delivered over the
    /// channel as `AppEvent::PyramidReady`, which `apply_pyramid_ready`
    /// installs + starts producing.
    ///
    /// `permit` (perf fix D) is MOVED into the job closure so it is released,
    /// via its `Drop` impl, exactly when the closure ends — on normal
    /// completion, on an early return at a `cancel.is_cancelled()` checkpoint,
    /// or (in principle) on panic — so a `PYRAMID_BUILD_CONCURRENCY` slot can
    /// never leak. Two call sites feed this: the immediate submit in
    /// `apply_full_decoded` (when a permit is free at full-decode time), and
    /// `drive_viewer`'s per-frame retry (when it was deferred via
    /// `v.needs_pyramid` because none was).
    pub fn submit_pyramid_build(
        app: &mut FerroliteApp,
        frame: &eframe::Frame,
        ctx: &egui::Context,
        image_id: i64,
        image_full: std::sync::Arc<ferrolite_image::LinearRgbaF32>,
        permit: crate::develop::cache::PyramidPermit,
    ) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let gpu_job = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let tx = app.state.tx.clone();
        let repaint = ctx.clone();
        let pyramid_handle =
            app.state
                .jobs
                .submit(ferrolite_jobs::Priority::Background, move |cancel| {
                    // Held for the lifetime of this closure; dropped (releasing the
                    // permit) on every exit path below, including the early returns.
                    let _permit = permit;
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
                    let tile_source: std::sync::Arc<dyn ferrolite_vt::TileSource + Send + Sync> =
                        std::sync::Arc::new(ferrolite_vt::PyramidTileSource::new(
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
        if let Some(v) = app.state.viewer.as_mut() {
            if v.image_id == image_id {
                v.pyramid_handle = Some(pyramid_handle);
            }
        }
    }

    /// Build the sparse full `VirtualTexture` (needs the render state) plus the
    /// `Rc`-based `TileEditPipeline`/`EditTileProducer` (both `!Send`, so
    /// UI-thread/render-thread only) from a GPU pyramid, a tile source, an op
    /// stack, and a camera matrix; install the full VT into the existing
    /// `ViewerGpu` holder; and start producing (the full VT must not emit raw
    /// camera-native tiles before the producer exists). Returns `false`
    /// (nothing installed) if the render state is unavailable or the viewer has
    /// since navigated away from `image_id` — guarded both at the exclusive
    /// `viewer` borrow and again at the final write-lock install, since the
    /// holder can be replaced by a newer open between the two.
    ///
    /// Shared by `apply_pyramid_ready` (the normal tier-2 decode completion,
    /// which owns `cam`/`op_stack` from the just-decoded open) and
    /// `try_warm_reveal` (a `WarmHit::Full` — the pyramid `Arc`s and the
    /// `op_stack`/`cam` they were built with are already in hand from the warm
    /// cache, so this reconstructs the GPU-side producer without any decode).
    /// `v.lens_warp`/`v.lens_vignette` are read from the CURRENT viewer (not
    /// cached — they are lens-bake products keyed off the image, not the op
    /// stack, and `apply_lens_baked` rebuilds the producer again once a bake
    /// lands).
    pub fn install_full_pipeline(
        app: &mut FerroliteApp,
        frame: &eframe::Frame,
        image_id: i64,
        pyramid: &std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>,
        tile_source: &std::sync::Arc<dyn ferrolite_vt::TileSource + Send + Sync>,
        op_stack: &ferrolite_pipeline::OpStack,
        cam: [[f32; 3]; 3],
    ) -> bool {
        let Some(rs) = frame.wgpu_render_state() else {
            return false;
        };
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);

        // Build the sparse full VT from the tile source (needs the pre-warmed
        // `ViewerPipelines`; the read lock is released before the write install
        // below).
        let full = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            // Keep an active monitor LUT applied across the install.
            Self::apply_display_tail(app, &gpu, vp);
            ferrolite_vt::VirtualTexture::sparse(
                &gpu,
                std::sync::Arc::clone(tile_source),
                std::sync::Arc::clone(&app.state.jobs),
                crate::app::VIEWER_TILE_BUDGET,
                &vp.pipelines,
            )
        };

        // Build the full-res edit producer from the GPU pyramid and flip
        // `full_ready`. The full VT tiles ALWAYS pass through camera→working
        // (the raw camera-native CPU path must never reach the working→display
        // tail), so the producer is attached unconditionally — identity stack =
        // unedited-but-color-managed.
        let version;
        let out_dims;
        {
            let Some(v) = app.state.viewer.as_mut() else {
                return false;
            };
            if v.image_id != image_id {
                return false;
            }
            v.pyramid = Some(std::sync::Arc::clone(pyramid));
            let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
            // Mode-aware vignette pair (MV2) so a persisted manual-vignette op
            // (lens_id=None → no bake) applies on open; the current lens bake (if
            // any) is threaded in — `None` is byte-identical to no correction.
            let (vig_amount, vig_manual) = crate::develop::vignette_mode::vig_pair(
                op_stack.lens_correction().as_ref(),
                v.lens_vignette.is_some(),
            );
            let tep = ferrolite_pipeline::TileEditPipeline::new(
                ctx_arc,
                std::sync::Arc::clone(pyramid),
                op_stack.clone(),
                cam,
                v.lens_warp.as_ref(),
                v.lens_vignette.as_ref(),
            );
            let mut producer = viewer::EditTileProducer::new(tep);
            producer.set_vig_amount(vig_amount);
            producer.set_vig_manual(vig_manual);
            // Whole-image atmospheric light for dehaze (design §5.3): cached and
            // estimated at most once per image (`ViewerState::dehaze_atmos`), not
            // re-estimated on every producer rebuild — this rebuild also fires on
            // radius/geometry/lens drags, and `A` is image-invariant, so redoing
            // the O(n log n) sort here would be UI-thread work per drag tick
            // (CLAUDE.md responsiveness rule 1). Same fn + same source the preview
            // EditPipeline uses internally, so the two tiers agree.
            //
            // `if let` rather than `.unwrap_or(NEUTRAL)`: a decoded source is
            // guaranteed present here (this branch only runs once the full-res
            // pyramid + preview source exist), so `None` is a can't-happen guard,
            // not a silent-wrong fallback — leaving `producer` on its constructor
            // default (`DEHAZE_ATMOS_NEUTRAL`) would silently diverge from the
            // preview tier if it were ever hit.
            if let Some(a) = v.dehaze_atmos() {
                producer.set_dehaze_atmos(a);
            }
            // Seed the shared dehaze transmission (ST-Task 4) from the preview
            // `EditPipeline` if it already exists and has evaluated (it's built +
            // evaluated in `apply_full_decoded`, which runs before this pyramid-
            // ready handler) — so a producer built outside an edit still starts
            // with the current map instead of a stale passthrough. `producer` is a
            // local here (not yet stored on `v`), so this doesn't conflict with
            // the immutable `v.preview_edit` borrow.
            producer.set_shared_transmission(
                v.preview_edit
                    .as_ref()
                    .and_then(|ep| ep.transmission_texture()),
            );
            // The producer renders tiles in geometry-applied OUTPUT space (the
            // rounded crop extent) — the sparse VT's logical size below must be
            // re-pointed at these dims or a stack with a crop (e.g. a loaded
            // sidecar) presents the full tier at the pre-crop source extent.
            out_dims = producer.out_dims();
            v.edit_producer = Some(producer);
            // Baseline for deferred-full-res rebuild decisions (see `full_stack`):
            // this producer was built from `v.op_stack`.
            v.full_stack = v.op_stack.clone();
            v.full_ready = true;
            v.full_synced_version = v.opstack_version;
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
                    full.set_sparse_image_dims(out_dims);
                    full.set_producing(true);
                    full.set_opstack_version(&g.ctx, version);
                }
            }
        }
        true
    }

    /// Both full-res pyramids finished building off the UI thread (delivered by
    /// the Background job submitted in `apply_full_decoded`): the sparse-VT CPU
    /// tile source and the GPU-resident edit pyramid. This is the UI-thread tail
    /// that the freeze fix moved off-thread — `install_full_pipeline` does the
    /// actual VT/producer construction (shared with a `WarmHit::Full` reveal);
    /// this wrapper supplies `cam`/`op_stack` from the just-decoded open and, on
    /// success, records the full pipeline in the warm cache so an immediate
    /// back-navigation to this `(image_id, op_stack_hash)` reveals 1:1 instantly
    /// (`try_warm_reveal`) instead of paying the ~1.2 s rebuild again. A stale
    /// result (user navigated away while the pyramids built) is dropped.
    pub fn apply_pyramid_ready(
        app: &mut FerroliteApp,
        frame: &eframe::Frame,
        image_id: i64,
        tile_source: &std::sync::Arc<dyn ferrolite_vt::TileSource + Send + Sync>,
        gpu_pyramid: &std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>,
    ) {
        // Stale guard: viewer closed or a different image is now open. Snapshot
        // `op_stack` here (before any exclusive `viewer` borrow inside
        // `install_full_pipeline`) so it can be passed in by value.
        let Some(op_stack) = app
            .state
            .viewer
            .as_ref()
            .and_then(|v| (v.image_id == image_id).then(|| v.op_stack.clone()))
        else {
            return;
        };
        // Compute camera→working BEFORE any exclusive `viewer` borrow below:
        // `camera_to_working` itself borrows `app.state.viewer` immutably.
        let cam = app.camera_to_working(app.current_wb_temp());
        if !Self::install_full_pipeline(
            app,
            frame,
            image_id,
            gpu_pyramid,
            tile_source,
            &op_stack,
            cam,
        ) {
            return;
        }

        // Warm cache: retain this image's full pipeline (GPU pyramid + tile
        // source + stack/cam) so an immediate back-navigation reveals 1:1
        // instantly (`try_warm_reveal`) instead of repeating the ~1.2 s rebuild.
        let key = crate::develop::cache::CacheKey {
            image_id,
            op_stack_hash: app
                .state
                .viewer
                .as_ref()
                .map(|v| v.op_stack_hash())
                .unwrap_or(0),
        };
        // Estimate: full-res Rgba16Float resident bytes plus the mip tail
        // (matches the diag gather's per-image GPU-pyramid estimate).
        let bytes = app
            .state
            .viewer
            .as_ref()
            .and_then(|v| v.image_dims)
            .map(|(w, h)| w as u64 * h as u64 * 8 * 4 / 3)
            .unwrap_or(0);
        app.state.warm_cache.insert_full(
            key,
            crate::develop::cache::FullEntry {
                pyramid: Some(std::sync::Arc::clone(gpu_pyramid)),
                tile_source: Some(std::sync::Arc::clone(tile_source)),
                op_stack,
                cam,
                bytes,
            },
        );
    }

    /// A preview-cache READ resolved to a HIT (Task 6): the cached JPEG for
    /// `image_id` was decoded off-thread to `linear`. Reveal it via the same
    /// Improvement-1 sRGB path Standard images use (`reveal_srgb_preview`, which
    /// runs one bounded `sRGB→working` GPU pass, fits, and installs the VT), so a
    /// second visit to a RAW shows instantly WITHOUT the RAW pixel decode. Then
    /// mark `cache_resolved` so the sparse full decode still fires next frame for
    /// zoom/1:1 detail, and `cache_write_back = false` so that full decode does
    /// NOT re-encode an entry that already exists. Stale `image_id` is dropped.
    pub fn apply_preview_cache_hit(
        app: &mut FerroliteApp,
        frame: &eframe::Frame,
        image_id: i64,
        linear: &ferrolite_image::LinearRgbaF32,
    ) {
        match app.state.viewer.as_mut() {
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
        let revealed = app.reveal_srgb_preview(frame, image_id, false);
        if revealed {
            app.mark_histogram_dirty();
        }
        if let Some(v) = app.state.viewer.as_mut() {
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

    pub fn apply_preview_cache_miss(app: &mut FerroliteApp, image_id: i64) {
        if let Some(v) = app.state.viewer.as_mut() {
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
    /// `lens_bake_handle` cancellation checkpoint in the job itapp.
    pub fn apply_lens_baked(
        app: &mut FerroliteApp,
        frame: &eframe::Frame,
        image_id: i64,
        result: &crate::develop::lens_bake::LensBakeResult,
    ) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let cam = app.camera_to_working(app.current_wb_temp());
        let Some(v) = app.state.viewer.as_mut() else {
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
                shown.clone(),
                cam,
                v.lens_warp.as_ref(),
                v.lens_vignette.as_ref(),
            );
            let mut producer = viewer::EditTileProducer::new(tep);
            producer.set_vig_amount(vig_amount);
            producer.set_vig_manual(vig_manual);
            // Whole-image atmospheric light for dehaze (design §5.3): cached and
            // estimated at most once per image (`ViewerState::dehaze_atmos`) — see
            // the full rationale at the first `set_dehaze_atmos` call site in
            // `apply_pyramid_ready`. `if let` (not `.unwrap_or(NEUTRAL)`): a
            // decoded source is guaranteed present on this rebuild path, so
            // `None` is a can't-happen guard, not a silent-wrong fallback.
            if let Some(a) = v.dehaze_atmos() {
                producer.set_dehaze_atmos(a);
            }
            // Re-seed the shared dehaze transmission (ST-Task 4): this rebuild
            // discards the previous producer, so the fresh one needs the current
            // map from the preview `EditPipeline` (already built + evaluated by
            // now — see the preview-tier branch above in this same handler, or
            // `apply_full_decoded` at open). `producer` is a local (not yet on
            // `v`), so no borrow conflict with the immutable `v.preview_edit` read.
            producer.set_shared_transmission(
                v.preview_edit
                    .as_ref()
                    .and_then(|ep| ep.transmission_texture()),
            );
            // Sync the sparse VT's logical size to the rebuilt producer's
            // geometry-applied output dims — same invariant as the rebuild in
            // `set_preview_and_full` (a lens rebuild keeps the stack's crop, so
            // this preserves the cropped extent rather than resetting it).
            let out_dims = producer.out_dims();
            v.edit_producer = Some(producer);
            // Baseline for deferred-full-res rebuild decisions (see `full_stack`):
            // this producer was rebuilt from `shown`.
            v.full_stack = shown.clone();
            v.opstack_version = v.opstack_version.wrapping_add(1);
            v.full_synced_version = v.opstack_version;
            let version = v.opstack_version;
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == image_id {
                    if let Some(full) = g.full.as_mut() {
                        full.set_sparse_image_dims(out_dims);
                        full.set_producing(true);
                        full.set_opstack_version(&g.ctx, version);
                    }
                }
            }
        }
        if let Some(v) = app.state.viewer.as_mut() {
            v.idle = false; // wake the drive loop so producer tiles re-render
        }
    }

    /// Apply `stack` to both render tiers (GPU + memory only; no history/persist).
    /// Preview tier: build the EditPipeline once, reuse via set_stack; evaluate
    /// and swap the displayed single texture. Full-res tier: set_stack (color) or
    /// rebuild (geometry/halo), bump the opstack version to invalidate cached tiles.
    /// Update the render tiers for `stack`. The live PREVIEW tier is always
    /// updated (that is what the fit-zoom view shows). The full-res tiled tier is
    /// only (re)synced + re-produced when `produce_full` is true — passed as the
    /// edit's `commit` flag, so a slider DRAG updates only the cheap preview each
    /// frame and the expensive full-res producer refreshes once on release. This
    /// keeps the VT from re-running `produce_tile` for every tile every drag frame
    /// (which exhausted GPU memory on integrated GPUs once a heavy op like dehaze's
    /// multi-pass transmission was active). Non-drag callers pass `true`.
    /// Returns the evaluated PREVIEW-tier output dims (the extent of what is
    /// now on screen — geometry-applied, so the cropped extent at rest and the
    /// full extent while the crop tool forces `crop = full`), or `None` when
    /// no preview pipeline exists yet. The crop-mode transition handler in
    /// `app.rs` uses this to re-frame the view to the newly shown extent;
    /// every other caller ignores it.
    pub fn set_preview_and_full(
        app: &mut FerroliteApp,
        frame: &eframe::Frame,
        stack: ferrolite_pipeline::OpStack,
        produce_full: bool,
    ) -> Option<(u32, u32)> {
        let rs = frame.wgpu_render_state()?;
        // Compute before taking the exclusive `viewer` borrow below:
        // `camera_to_working`/`preview_to_working` themselves borrow
        // `app.state.viewer` immutably.
        // WB temp of the INCOMING stack (v.op_stack is updated below), so a WB
        // temp edit re-interpolates the dual-illuminant matrix this same frame.
        let temp = stack.white_balance().map(|w| w.temp).unwrap_or(0.0);
        let cam = app.camera_to_working(temp);
        let pw = app.preview_to_working();
        let v = app.state.viewer.as_mut()?;
        v.op_stack = stack.clone();
        v.opstack_version = v.opstack_version.wrapping_add(1);

        // Preview-tier matrix (RAW = WB-driven camera→working `cam`; Standard =
        // sRGB `pw`). Recomputed each edit; `set_color_matrix` no-ops when
        // unchanged, so only a WB temp change actually dirties the head (P2 §5.1).
        let pv_matrix = v.preview_tier_source(cam, pw).1;

        // What the preview should show: the live stack, the empty stack in
        // before/after mode, or (crop tool active) the stack with crop forced
        // full so the crop rectangle is represented by the overlay over the
        // full, rotated image. Pure helper (`develop::ops_edit::shown_stack`)
        // so the extent choice is unit-tested — it decides the dims this
        // method returns, which the crop-mode transition refit relies on.
        let shown = crate::develop::ops_edit::shown_stack(&stack, v.before_after, v.crop_active);

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
        let mut shown_dims: Option<(u32, u32)> = None;
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
            // (cheap Arc clone) into the write scope. (`ep` borrows `app.state`,
            // `renderer` borrows `frame` — disjoint, so they may coexist, but we
            // keep the evaluate out of the lock scope to stay close to the
            // apply_full_decoded discipline.)
            let img = ep.evaluate();
            shown_dims = Some((img.width, img.height));
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
        // Full-res tier: DEFERRED to commit (`produce_full`). Mid-drag
        // (`produce_full == false`) only the live preview above is refreshed each
        // frame; the producer is left untouched and NOT re-produced, so the VT does
        // not re-run `produce_tile` for every tile every frame — that per-frame
        // full-res churn is what exhausted GPU memory on integrated GPUs with a
        // heavy op (dehaze's multi-pass transmission) active. On commit the producer
        // syncs once and re-produces (the load the app already handled per edit).
        // `needs_full_rebuild` compares against `v.full_stack` — the stack the
        // producer ACTUALLY reflects — not the previous frame, so a dehaze on/off or
        // radius change made across the deferred drag still rebuilds here on release.
        if produce_full && v.full_ready {
            let rebuild = v.edit_producer.is_none()
                || crate::develop::ops_edit::needs_full_rebuild(&v.full_stack, &shown);
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
                    // Whole-image atmospheric light for dehaze (design §5.3): cached
                    // and estimated at most once per image
                    // (`ViewerState::dehaze_atmos`) — see the full rationale at the
                    // first `set_dehaze_atmos` call site in `apply_pyramid_ready`.
                    // This rebuild is exactly the radius/geometry/lens-drag path
                    // `needs_full_rebuild` fires on, so this is the call site the
                    // caching matters most for. `if let` (not `.unwrap_or(NEUTRAL)`):
                    // a decoded source is guaranteed present once `full_ready`, so
                    // `None` is a can't-happen guard, not a silent-wrong fallback.
                    if let Some(a) = v.dehaze_atmos() {
                        producer.set_dehaze_atmos(a);
                    }
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
            // Hand the tiled producer the shared dehaze transmission (ST-Task 4):
            // the preview `EditPipeline` above (`v.preview_edit`) is the SOLE place
            // the transmission is computed — evaluated earlier in this same call,
            // just above (line ~1569) — so it is current here. The tiled recovery
            // only samples it (no per-tile recompute). Fetch the `Arc` (cheap
            // clone) into a local BEFORE mutably borrowing `v.edit_producer`: both
            // are fields of `v`, so the immutable fetch must finish before the
            // mutable borrow starts, or this doesn't compile. Runs unconditionally
            // whenever a producer exists — both the just-rebuilt producer and the
            // updated-in-place (color-only / amount-only) producer above need the
            // current map; `None` (dehaze inactive) sets a passthrough.
            let shared_transmission = v
                .preview_edit
                .as_ref()
                .and_then(|ep| ep.transmission_texture());
            if let Some(producer) = v.edit_producer.as_mut() {
                producer.set_shared_transmission(shared_transmission);
            }
            // Record the stack the producer now reflects — the rebuild baseline for
            // the next commit (see the `full_stack` field doc + the block comment).
            v.full_stack = shown.clone();
            v.full_synced_version = v.opstack_version;
            let version = v.opstack_version;
            let image_id = v.image_id;
            // Keep the sparse VT's logical size in lockstep with the producer's
            // geometry-applied OUTPUT dims (the rounded crop extent, the same
            // dims the preview tier's texture just took above). Without this a
            // crop commit leaves the full tier at the pre-crop source extent:
            // wrongly cropped at rest, and pan/zoom flickers between the
            // cropped preview and the uncropped front (they place the image at
            // different centers/extents). Covers both branches: the rebuilt
            // producer (geometry change) and the in-place update (color-only —
            // a no-op, dims unchanged).
            let out_dims = v.edit_producer.as_ref().map(|p| p.out_dims());
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == image_id {
                    if let Some(full) = g.full.as_mut() {
                        if let Some(dims) = out_dims {
                            full.set_sparse_image_dims(dims);
                        }
                        full.set_producing(true);
                        full.set_opstack_version(&g.ctx, version);
                    }
                }
            }
        }
        v.idle = false; // wake the drive loop so producer tiles re-render
        app.mark_histogram_dirty();
        shown_dims
    }

    /// Apply a panel/widget edit: update both tiers immediately; on commit (drag
    /// release / discrete change) push undo history + persist off-thread.
    pub fn apply_edit(
        app: &mut FerroliteApp,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        kind: ferrolite_pipeline::OpKind,
        stack: ferrolite_pipeline::OpStack,
        commit: bool,
    ) {
        // Snapshot the pre-edit stack BEFORE `set_preview_and_full` overwrites
        // `v.op_stack`, so a lens-key comparison below sees the real old/new.
        let old_stack = app.state.viewer.as_ref().map(|v| v.op_stack.clone());
        // Mid-drag (`commit == false`): preview-only, defer the full-res tier to
        // release. On commit: sync + re-produce the full-res tier too. Also flag
        // the drag so `drive_viewer` PAUSES per-frame full-res tile production
        // while dragging (the OOM lever — the drive loop produces independently of
        // this method); production resumes on commit.
        Self::set_preview_and_full(app, frame, stack.clone(), commit);
        if let Some(v) = app.state.viewer.as_mut() {
            v.edit_in_progress = !commit;
        }
        if !commit {
            return;
        }
        let Some(v) = app.state.viewer.as_mut() else {
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
        if let Some(rec) = app.state.images.iter_mut().find(|r| r.id == image_id) {
            rec.has_edits = has_edits; // optimistic cache update (filmstrip badge)
        }
        if kind == ferrolite_pipeline::OpKind::LensCorrection {
            if let Some(old) = old_stack {
                Self::maybe_spawn_lens_bake(app, ctx, &old, &stack);
            }
        }
        app.persist_ops(ctx, image_id, path, stack);
    }

    pub fn maybe_spawn_lens_bake(
        app: &mut FerroliteApp,
        ctx: &egui::Context,
        old: &ferrolite_pipeline::OpStack,
        new: &ferrolite_pipeline::OpStack,
    ) {
        if crate::develop::ops_edit::lens_bake_key(old)
            == crate::develop::ops_edit::lens_bake_key(new)
        {
            return;
        }
        let Some(db) = app.state.lens_db.clone() else {
            return;
        };
        let Some(lc) = new.lens_correction() else {
            return;
        };
        let Some(v) = app.state.viewer.as_mut() else {
            return;
        };
        if let Some(h) = v.lens_bake_handle.take() {
            h.cancel();
        }
        let image_id = v.image_id;
        let handle = crate::develop::lens_bake::spawn_lens_bake(
            &app.state.jobs,
            &db,
            &app.state.tx,
            ctx,
            image_id,
            lc,
        );
        if let Some(v) = app.state.viewer.as_mut() {
            v.lens_bake_handle = Some(handle);
        }
    }

    pub fn try_auto_match_lens(app: &mut FerroliteApp, image_id: i64) {
        let Some(v) = app.state.viewer.as_ref() else {
            return;
        };
        if v.image_id != image_id || v.lens_auto_match_attempted {
            return;
        }
        if !v.meta_loaded || !v.ops_loaded {
            return;
        }
        let should_match = crate::develop::lens_match::should_auto_match(&v.op_stack);
        let query = v
            .meta
            .as_ref()
            .and_then(crate::develop::lens_match::query_from_metadata);

        let Some(v) = app.state.viewer.as_mut() else {
            return;
        };
        v.lens_auto_match_attempted = true;
        if !should_match {
            return;
        }
        let Some(db) = app.state.lens_db.clone() else {
            return;
        };
        let Some(query) = query else {
            return;
        };
        let candidate = ferrolite_lens::LensDb::match_lens(db.as_ref(), &query);
        if let Some(v) = app.state.viewer.as_mut() {
            if v.image_id == image_id {
                v.lens_auto_match = candidate;
            }
        }
    }

    pub fn apply_display_tail(
        app: &FerroliteApp,
        gpu: &ferrolite_gpu::GpuContext,
        vp: &viewer::ViewerPipelines,
    ) {
        match &app.state.display_lut {
            Some(l) => vp.pipelines.set_display_lut(
                &gpu.queue,
                l.size,
                &l.rgba16f,
                ferrolite_color::DISPLAY_LUT_SHAPER_GAMMA,
            ),
            None => vp.pipelines.set_display_matrix(
                &gpu.queue,
                ferrolite_color::working_to_display(app.state.working_space),
            ),
        }
    }

    pub fn redetect_display_profile(
        app: &mut FerroliteApp,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        use raw_window_handle::HasWindowHandle;

        app.state.display_detect_gen += 1;
        let generation = app.state.display_detect_gen;
        let mode = app.state.settings.display_profile.clone();
        let working = app.state.working_space;
        let tx = app.state.tx.clone();

        let (detected, key) = match frame.window_handle() {
            Ok(h) => crate::monitor_profile::detect(h.as_raw()),
            Err(_) => (None, 0),
        };
        app.state.last_monitor_key = key;
        let source = crate::settings::dto::resolve(&mode, detected);

        app.state
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

    pub fn apply_working_space(
        app: &mut FerroliteApp,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        ws: ferrolite_color::WorkingSpace,
    ) {
        if ws == app.state.working_space {
            return;
        }
        app.state.working_space = ws;
        app.state.settings.working_space = crate::settings::dto::PersistedWorkingSpace::from_ws(ws);
        app.mark_settings_dirty();
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);

        {
            let renderer = rs.renderer.read();
            if let Some(vp) = renderer.callback_resources.get::<viewer::ViewerPipelines>() {
                vp.pipelines
                    .set_display_matrix(&gpu.queue, ferrolite_color::working_to_display(ws));
            }
        }

        Self::redetect_display_profile(app, ctx, frame);

        let cam = app.camera_to_working(app.current_wb_temp());
        let pw = app.preview_to_working();
        let Some(v) = app.state.viewer.as_mut() else {
            ctx.request_repaint();
            return;
        };

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

        if let Some(producer) = v.edit_producer.as_mut() {
            producer.set_color_matrix(cam);
        }
        v.opstack_version = v.opstack_version.wrapping_add(1);
        v.full_synced_version = v.opstack_version;
        let version = v.opstack_version;
        let image_id = v.image_id;
        {
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == image_id {
                    g.preview_before = None;
                    if let Some(full) = g.full.as_mut() {
                        full.set_opstack_version(&g.ctx, version);
                    }
                }
            }
        }
        v.idle = false;
        app.mark_histogram_dirty();
        ctx.request_repaint();
    }
}

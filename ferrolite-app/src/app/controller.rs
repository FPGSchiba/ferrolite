use crate::app::FerroliteApp;
use crate::events::AppEvent;
use crate::viewer;
use std::hash::{Hash, Hasher};

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
                    Self::apply_preview_ready(app, frame, *image_id, linear);
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
                    if need_fallback && app.reveal_srgb_preview(frame, image_id) {
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
                                Self::set_preview_and_full(app, frame, stack.clone());
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

    pub fn apply_preview_ready(
        app: &mut FerroliteApp,
        frame: &eframe::Frame,
        image_id: i64,
        linear: &ferrolite_image::LinearRgbaF32,
    ) {
        let Some(v) = app.state.viewer.as_mut() else {
            return; // viewer closed while decoding
        };
        if v.image_id != image_id {
            return; // stale: a different image is now open
        }
        let is_raw = v.kind == ferrolite_image::FileKind::Raw;
        v.preview_source = Some(std::sync::Arc::new(linear.clone()));
        v.mask_overlay_input = None;

        if is_raw {
            return;
        }

        app.reveal_srgb_preview(frame, image_id);
    }

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
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };

        let _ = v;

        let cam = app.camera_to_working(app.current_wb_temp());
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);

        let image_arc = std::sync::Arc::new(image.clone());
        let raw_preview_source: Option<std::sync::Arc<ferrolite_image::LinearRgbaF32>> =
            is_raw.then(|| std::sync::Arc::clone(&image_arc));
        let raw_preview: Option<(std::sync::Arc<wgpu::Texture>, (u32, u32))> = if let Some(src) =
            raw_preview_source.as_ref()
        {
            match app.state.viewer.as_mut() {
                Some(v) if v.image_id == image_id => {
                    v.raw_preview_source = raw_preview_source.clone();
                    let ctx_arc =
                        std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
                    let mut ep = ferrolite_pipeline::EditPipeline::new(
                        ctx_arc.clone(),
                        src,
                        v.op_stack.clone(),
                        cam,
                    );
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

        let preview_vt = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
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

        let mut preview_installed = false;
        {
            let mut renderer = rs.renderer.write();
            if let Some(preview) = preview_vt {
                let holder_gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
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
                if g.image_id == image_id {
                    preview_installed = true;
                }
            }
        }

        if preview_installed {
            if let Some(v) = app.state.viewer.as_mut() {
                if v.image_id == image_id {
                    v.loaded = true;
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

            let image_full = std::sync::Arc::clone(&image_arc);
            let gpu_job = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
            let tx = app.state.tx.clone();
            let repaint = ctx.clone();
            let pyramid_handle =
                app.state
                    .jobs
                    .submit(ferrolite_jobs::Priority::Background, move |cancel| {
                        if cancel.is_cancelled() {
                            return;
                        }
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
            if let Some(v) = app.state.viewer.as_mut() {
                if v.image_id == image_id {
                    v.pyramid_handle = Some(pyramid_handle);
                }
            }
        }

        if preview_installed {
            let write_back = app.state.viewer.as_ref().and_then(|v| {
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
            if let (Some((path, op_stack)), Some(render)) =
                (write_back, raw_preview_source.as_ref())
            {
                let display_matrix = ferrolite_color::mul_mat3(
                    &ferrolite_color::working_to_display(app.state.working_space),
                    &cam,
                );
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

        app.mark_histogram_dirty();
    }

    pub fn apply_pyramid_ready(
        app: &mut FerroliteApp,
        frame: &eframe::Frame,
        image_id: i64,
        tile_source: &std::sync::Arc<dyn ferrolite_vt::TileSource + Send + Sync>,
        gpu_pyramid: &std::sync::Arc<ferrolite_pipeline::GpuPyramidSource>,
    ) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        if app.state.viewer.as_ref().map(|v| v.image_id) != Some(image_id) {
            return;
        }
        let cam = app.camera_to_working(app.current_wb_temp());
        let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);

        let full = {
            let renderer = rs.renderer.read();
            let vp = renderer
                .callback_resources
                .get::<viewer::ViewerPipelines>()
                .expect("ViewerPipelines pre-warmed at startup");
            Self::apply_display_tail(app, &gpu, vp);
            ferrolite_vt::VirtualTexture::sparse(
                &gpu,
                std::sync::Arc::clone(tile_source),
                std::sync::Arc::clone(&app.state.jobs),
                crate::app::VIEWER_TILE_BUDGET,
                &vp.pipelines,
            )
        };

        let version;
        {
            let Some(v) = app.state.viewer.as_mut() else {
                return;
            };
            if v.image_id != image_id {
                return;
            }
            v.pyramid = Some(std::sync::Arc::clone(gpu_pyramid));
            let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
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

    pub fn apply_preview_cache_hit(
        app: &mut FerroliteApp,
        frame: &eframe::Frame,
        image_id: i64,
        linear: &ferrolite_image::LinearRgbaF32,
    ) {
        match app.state.viewer.as_mut() {
            Some(v) if v.image_id == image_id => {
                v.preview_source = Some(std::sync::Arc::new(linear.clone()));
                v.mask_overlay_input = None;
            }
            _ => return,
        }
        let revealed = app.reveal_srgb_preview(frame, image_id);
        if revealed {
            app.mark_histogram_dirty();
        }
        if let Some(v) = app.state.viewer.as_mut() {
            if v.image_id == image_id {
                v.cache_write_back = false;
                v.cache_resolved = true;
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
            return;
        }
        v.lens_warp = result.warp.clone();
        v.lens_vignette = result.vignette.clone();
        v.lens_resolved_name = result.resolved_name.clone();
        v.lens_bake_handle = None;

        let shown = if v.before_after {
            ferrolite_pipeline::OpStack::default()
        } else {
            v.op_stack.clone()
        };

        let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let _ = &ctx_arc;
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
        if let Some(v) = app.state.viewer.as_mut() {
            v.idle = false;
        }
    }

    pub fn set_preview_and_full(
        app: &mut FerroliteApp,
        frame: &eframe::Frame,
        stack: ferrolite_pipeline::OpStack,
    ) {
        let Some(rs) = frame.wgpu_render_state() else {
            return;
        };
        let temp = stack.white_balance().map(|w| w.temp).unwrap_or(0.0);
        let cam = app.camera_to_working(temp);
        let pw = app.preview_to_working();
        let Some(v) = app.state.viewer.as_mut() else {
            return;
        };
        let old = v.op_stack.clone();
        v.op_stack = stack.clone();
        v.opstack_version = v.opstack_version.wrapping_add(1);

        let pv_matrix = v.preview_tier_source(cam, pw).1;

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
            let lc = shown.lens_correction();
            ep.set_lens_uniform(ferrolite_pipeline::lens_uniform(
                lc.as_ref(),
                v.lens_warp.is_some(),
            ));
            let (vig_amount, vig_manual) =
                crate::develop::vignette_mode::vig_pair(lc.as_ref(), v.lens_vignette.is_some());
            ep.set_vig_amount(vig_amount);
            ep.set_vig_manual(vig_manual);
            let img = ep.evaluate();
            let mut renderer = rs.renderer.write();
            if let Some(g) = renderer.callback_resources.get_mut::<viewer::ViewerGpu>() {
                if g.image_id == v.image_id {
                    g.preview
                        .update_single_from_texture(img.texture.clone(), (img.width, img.height));
                }
            }
        }

        if v.full_ready {
            let rebuild = v.edit_producer.is_none()
                || crate::develop::ops_edit::needs_full_rebuild(&old, &shown);
            if rebuild {
                if let Some(pyr) = v.pyramid.clone() {
                    let ctx_arc =
                        std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
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
                producer.set_stack(shown.clone());
                producer.set_color_matrix(cam);
                let lc = shown.lens_correction();
                producer.set_lens_uniform(ferrolite_pipeline::lens_uniform(
                    lc.as_ref(),
                    v.lens_warp.is_some(),
                ));
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
        v.idle = false;
        app.mark_histogram_dirty();
    }

    pub fn apply_edit(
        app: &mut FerroliteApp,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        kind: ferrolite_pipeline::OpKind,
        stack: ferrolite_pipeline::OpStack,
        commit: bool,
    ) {
        let old_stack = app.state.viewer.as_ref().map(|v| v.op_stack.clone());
        Self::set_preview_and_full(app, frame, stack.clone());
        if !commit {
            return;
        }
        let Some(v) = app.state.viewer.as_mut() else {
            return;
        };
        v.edits_dirty = true;
        v.history.push(kind, stack.clone());
        if kind == ferrolite_pipeline::OpKind::LocalAdjustments {
            v.history.break_coalesce();
        }
        let image_id = v.image_id;
        let path = v.path.clone();
        let has_edits = !stack.is_identity();
        if let Some(rec) = app.state.images.iter_mut().find(|r| r.id == image_id) {
            rec.has_edits = has_edits;
        }
        if kind == ferrolite_pipeline::OpKind::LensCorrection {
            if let Some(old) = old_stack {
                Self::maybe_spawn_lens_bake(app, ctx, &old, &stack);
            }
        }
        app.persist_ops(ctx, image_id, path, stack);
    }

    pub fn rebuild_mask_overlay_if_needed(app: &mut FerroliteApp, frame: &eframe::Frame) {
        use crate::develop::mask_edit;
        use crate::develop::mask_overlay_color::OVERLAY_MAX_EDGE;

        let Some(v) = app.state.viewer.as_mut() else {
            return;
        };
        let la = mask_edit::layers(&v.op_stack);
        let Some(sel) = v.mask.selected.filter(|&i| i < la.layers.len()) else {
            v.mask.overlay_key = None;
            return;
        };
        let committed_def = &la.layers[sel].mask;
        let def = match v.mask.preview_component.clone() {
            Some((c, mode)) => mask_edit::prospective_def(committed_def, c, mode),
            None => committed_def.clone(),
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        sel.hash(&mut h);
        v.opstack_version.hash(&mut h);
        serde_json::to_string(&v.mask.preview_component)
            .unwrap_or_default()
            .hash(&mut h);
        v.mask.highlight_component.hash(&mut h);
        let key = h.finish();
        if v.mask.overlay_key == Some(key) && app.state.mask_overlay_native.is_some() {
            return;
        }

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
                v.mask_overlay_input_gen = v.mask_overlay_input_gen.wrapping_add(1);
            }
        }
        let (overlay, highlight) = {
            let highlight_component = v.mask.highlight_component;
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
            let highlight = highlight_component.and_then(|idx| {
                oc.highlight_texture(idx, crate::develop::mask_overlay_color::HIGHLIGHT_STRENGTH)
            });
            v.mask.overlay_key = Some(key);
            (overlay, highlight)
        };
        let view = overlay.srgb_view();
        {
            let mut renderer = rs.renderer.write();
            match app.state.mask_overlay_native {
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
                    app.state.mask_overlay_native = Some(id);
                }
            }
            if let Some(highlight) = &highlight {
                let hview = highlight.srgb_view();
                match app.state.mask_overlay_highlight_native {
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
                        app.state.mask_overlay_highlight_native = Some(id);
                    }
                }
            }
        }
        app.state.mask_overlay_gpu = Some(overlay);
        if let Some(highlight) = highlight {
            app.state.mask_overlay_highlight_gpu = Some(highlight);
        }
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

    pub fn redetect_display_profile(app: &mut FerroliteApp, ctx: &egui::Context, frame: &eframe::Frame) {
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
        app.state.settings.working_space =
            crate::settings::dto::PersistedWorkingSpace::from_ws(ws);
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

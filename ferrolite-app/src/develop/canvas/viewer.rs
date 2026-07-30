//! Develop canvas viewer: interactive pan/zoom driving, split compares,
//! tool palette, histogram, EXIF overlays, and active tool canvas overlays.

use crate::develop::tool::DevelopToolRegistry;
use crate::state::AppState;

const MAX_PRODUCE_PER_FRAME: usize = 32;
const PREFETCH_RING: u32 = 1;

#[derive(Debug)]
pub enum ViewerAction {
    ApplyEdit {
        kind: ferrolite_pipeline::OpKind,
        stack: ferrolite_pipeline::OpStack,
        commit: bool,
    },
    SelectTool(crate::develop::tool::ToolId),
    Undo,
    Redo,
    SetPreviewAndFull(ferrolite_pipeline::OpStack),
}

pub struct Viewer {
    image_id: i64,
}

impl Viewer {
    pub fn new(image_id: i64) -> Self {
        Self { image_id }
    }

    pub fn show(
        self,
        ui: &mut egui::Ui,
        app: &mut crate::app::FerroliteApp,
        frame: &eframe::Frame,
    ) -> Option<ViewerAction> {
        let mut action_outcome = None;

        // Perf fix D: retry a pyramid build `apply_full_decoded` deferred
        // because no `PYRAMID_BUILD_CONCURRENCY` permit was free at the time.
        // `needs_pyramid` lives on the CURRENT viewer (reset to `false` by
        // every `ViewerState::open`), so this only ever retries for the image
        // the user is actually looking at now — a superseded image's deferred
        // pyramid is simply never revisited once navigation replaces the
        // viewer. Only RAW builds a pyramid at all, and `raw_preview_source`
        // (retained by `apply_full_decoded`) is the full-res `Arc` the build
        // needs, so gate on both being present.
        let deferred_pyramid = app.state.viewer.as_ref().and_then(|v| {
            (v.needs_pyramid && v.kind == ferrolite_image::FileKind::Raw)
                .then(|| v.raw_preview_source.clone())
                .flatten()
                .map(|image_full| (v.image_id, image_full))
        });
        if let Some((image_id, image_full)) = deferred_pyramid {
            match crate::develop::cache::try_acquire_pyramid_permit() {
                Some(permit) => {
                    crate::app::controller::AppController::submit_pyramid_build(
                        app,
                        frame,
                        ui.ctx(),
                        image_id,
                        image_full,
                        permit,
                    );
                    if let Some(v) = app.state.viewer.as_mut() {
                        if v.image_id == image_id {
                            v.needs_pyramid = false;
                        }
                    }
                }
                None => {
                    // Still no free permit: keep the drive loop alive so the
                    // retry runs again next frame instead of stalling until
                    // unrelated input requests a repaint.
                    ui.ctx().request_repaint();
                }
            }
        }

        // Verify that the viewer has the matching image open.
        let v = match app.state.viewer.as_mut() {
            Some(v) if v.image_id == self.image_id => v,
            _ => return None,
        };

        // Step 1: Detect crop_active mode transitions.
        let crop_active = v.crop_active;
        if crop_active != app.state.canvas.crop_active_prev {
            let stack = v.op_stack.clone();
            app.state.canvas.crop_active_prev = crop_active;
            action_outcome = Some(ViewerAction::SetPreviewAndFull(stack));
        }

        // Step 2: Brush gesture scroll listener (Ctrl+scroll brush-size).
        if v.mask.active
            && v.mask.selected.is_some()
            && v.mask.tool == crate::develop::mask_ui::MaskTool::Brush
        {
            let dims = v.image_dims.unwrap_or((1, 1));
            let image_rect =
                crate::viewer::image_screen_rect(ui.min_rect(), dims, v.view, v.viewport);
            let ctrl_scroll_over_image = ui.ctx().input(|i| {
                let ctrl = i.modifiers.command || i.modifiers.ctrl;
                let scroll_y = i.raw_scroll_delta.y;
                let over_image = i
                    .pointer
                    .hover_pos()
                    .is_some_and(|p| image_rect.contains(p));
                (ctrl && scroll_y.abs() > f32::EPSILON && over_image).then_some(scroll_y)
            });
            if let Some(scroll_y) = ctrl_scroll_over_image {
                v.mask.brush_radius = crate::develop::mask_overlay::brush_radius_from_scroll(
                    v.mask.brush_radius,
                    scroll_y,
                    crate::develop::mask_panel::BRUSH_RADIUS_MIN,
                    crate::develop::mask_panel::BRUSH_RADIUS_MAX,
                );
                // Consume: zero the scroll so drive_viewer/paint zoom doesn't fire.
                ui.ctx()
                    .input_mut(|i| i.raw_scroll_delta = egui::Vec2::ZERO);
            }
        }

        // Step 3: Drive the viewer (formerly drive_viewer method in app.rs).
        let dt = ui.ctx().input(|i| i.stable_dt);
        let open_id = Some(self.image_id);

        let mut tiles_pending: Option<usize> = None;
        let mut produce_pending: Option<usize> = None;
        let mut needed_established = false;
        let mut produced_this_frame = 0usize;
        let mut converged = false;
        let mut swapped_this_frame = false;
        let mut present_reallocated = false;

        if let Some(rs) = frame.wgpu_render_state() {
            let cur_view = v.view;
            let cur_viewport = v.viewport;
            let cur_version = v.opstack_version;
            let cur_synced = v.full_synced_version;
            let cur_present_key = v.present_key;
            let mut renderer = rs.renderer.write();

            if let Some(g) = renderer
                .callback_resources
                .get_mut::<crate::viewer::ViewerGpu>()
            {
                let ppp = ui.ctx().pixels_per_point();
                let phys = (
                    (v.viewport.0 * ppp).round().max(1.0) as u32,
                    (v.viewport.1 * ppp).round().max(1.0) as u32,
                );
                present_reallocated = g.present.resize(&g.ctx, phys);

                if Some(g.image_id) != open_id {
                    if let Some(full) = g.full.as_mut() {
                        full.cancel_sparse();
                    }
                } else if g.full.is_some() {
                    {
                        let full = g.full.as_mut().expect("checked is_some");
                        full.request_view_feedback(&g.ctx);
                        // PAUSE full-res production while a slider edit is being
                        // dragged: the fit view shows the live preview tier
                        // during the drag (the full tier is off-screen while the
                        // op version bumps), so re-producing the heavy dehaze
                        // full-res tiles every frame is pure waste — and on
                        // constrained/integrated GPUs that per-frame churn
                        // exhausts memory (OOM in `produce_tile`). Production
                        // resumes on commit (drag release), refreshing 1:1 once.
                        if !v.edit_in_progress {
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
                        converged = full.is_converged(&cur_view, cur_viewport);
                    }

                    // `swap_allowed` (present.rs): mid-drag the producer is
                    // deferred (`cur_synced` lags `cur_version`), and the pool's
                    // `converged` is checked against its own frozen version —
                    // composing then would stamp STALE pre-drag tiles valid over
                    // the live preview. Swap only when the producer is synced.
                    if crate::viewer::present::swap_allowed(
                        converged,
                        cur_version,
                        cur_synced,
                        cur_present_key == Some((cur_version, cur_view)),
                    ) {
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

        if crate::diag::enabled() {
            crate::diag::record_viewer_frame(dt, produced_this_frame);
        }

        if matches!(tiles_pending, Some(n) if n > 0) {
            v.idle = false;
        }

        let cur_version = v.opstack_version;
        let cur_view = v.view;
        if present_reallocated {
            v.present_key = None;
        }
        if swapped_this_frame {
            v.present_key = Some((cur_version, cur_view));
            v.begin_crossfade();
        }

        let factor = v.tick_crossfade(dt);
        let tiles_settled = matches!(tiles_pending, Some(0));
        let front_valid = v.present_key == Some((cur_version, cur_view));
        let show_full =
            v.full_ready && front_valid && factor >= 1.0 && tiles_settled && !v.split_compare;
        let full_ready = v.full_ready;
        v.showing_full = show_full;

        let full_converged = needed_established
            && matches!(produce_pending, Some(0) | None)
            && produced_this_frame == 0;

        if show_full && !v.crossfading && full_converged {
            v.idle = true;
        }

        let crossfading = v.crossfading;
        let interactive = !v.crop_active && (show_full || !v.split_compare);

        let canvas_rect = ui.available_rect_before_wrap();
        let split_active = v.split_compare && !show_full;
        let (image_id, view, viewport, split_pos) = (v.image_id, v.view, v.viewport, v.split_pos);

        // Tier-0 placeholder: the resident grid thumbnail for this image (if any),
        // cloned out of the texture cache BEFORE the `viewer::paint` borrow of `v`
        // so the `app.state.textures` borrow is released first. A `TextureHandle`
        // clone is a cheap refcount bump (no pixel copy).
        let tier0_thumb = app.state.textures.get(image_id).cloned();

        let (loading_preview, present_source) = crate::viewer::paint(
            ui,
            v,
            full_ready,
            front_valid,
            factor,
            interactive,
            tier0_thumb.as_ref(),
        );
        let idle = v.idle;
        let crossfading_present =
            matches!(present_source, crate::viewer::PresentSource::Crossfade(_));

        let tiles_loading = matches!(tiles_pending, Some(n) if n > 0);
        let full_warming = show_full && !full_converged;
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

        // Draw the split compare divider if active.
        if split_active {
            ensure_before_view(app, frame);
            let div_x = crate::develop::split::divider_x(
                canvas_rect.left(),
                canvas_rect.width(),
                split_pos,
            );
            let left_clip =
                egui::Rect::from_min_max(canvas_rect.min, egui::pos2(div_x, canvas_rect.max.y));
            ui.painter()
                .with_clip_rect(left_clip)
                .add(egui_wgpu::Callback::new_paint_callback(
                    canvas_rect,
                    crate::viewer::ViewerCallback {
                        image_id,
                        view,
                        viewport,
                        present_source: crate::viewer::PresentSource::Preview,
                        which: crate::viewer::PreviewWhich::Before,
                    },
                ));
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
                    if let Some(v) = app.state.viewer.as_mut() {
                        v.split_pos = new_pos;
                    }
                    ui.ctx().request_repaint();
                }
            }
        }

        // Draw standard overlays.
        if app.state.settings.show_histogram {
            crate::develop::canvas::overlays::draw_histogram(ui, &app.state);
        }
        let info_before = app.state.show_info_panel;
        crate::develop::canvas::overlays::draw_info(ui, &mut app.state);
        if app.state.show_info_panel != info_before {
            app.state.settings.show_info_panel = app.state.show_info_panel;
            app.mark_settings_dirty();
        }

        let tool_registry = DevelopToolRegistry::standard();
        let palette_action =
            crate::develop::canvas::overlays::draw_tool_palette(ui, &app.state, &tool_registry);
        if let Some(action) = palette_action {
            match action {
                crate::develop::tool_palette::PaletteAction::SelectTool(id) => {
                    action_outcome = Some(ViewerAction::SelectTool(id));
                }
                crate::develop::tool_palette::PaletteAction::Undo => {
                    action_outcome = Some(ViewerAction::Undo);
                }
                crate::develop::tool_palette::PaletteAction::Redo => {
                    action_outcome = Some(ViewerAction::Redo);
                }
            }
        }

        // Rebuild mask overlay if needed, and draw active tool canvas overlay.
        let active_tool = app
            .state
            .viewer
            .as_ref()
            .map(|_| app.state.tool_state.active);
        if active_tool == Some(crate::develop::tool::ToolId::Mask) {
            rebuild_mask_overlay_if_needed(&mut app.state, frame);
        }

        if let Some(id) = active_tool {
            if let Some((dims, view, viewport)) = app
                .state
                .viewer
                .as_ref()
                .map(|v| (v.image_dims.unwrap_or((1, 1)), v.view, v.viewport))
            {
                let image_rect =
                    crate::viewer::image_screen_rect(ui.min_rect(), dims, view, viewport);
                if let Some(tool) = tool_registry.get(id) {
                    if let Some(o) = tool.canvas(ui, image_rect, &mut app.state) {
                        action_outcome = Some(ViewerAction::ApplyEdit {
                            kind: o.kind,
                            stack: o.stack,
                            commit: o.commit,
                        });
                    }
                }
            }
        }

        // Loupe context-menu widget.
        let is_adjust_active = app.state.tool_state.active == crate::develop::tool::ToolId::Adjust;
        let ctx_menu_id = app
            .state
            .viewer
            .as_ref()
            .filter(|_| is_adjust_active)
            .map(|v| v.image_id);
        if let Some(image_id) = ctx_menu_id {
            let rect = ui.min_rect();
            let resp = ui.interact(rect, ui.id().with("loupe_ctx"), egui::Sense::click());
            resp.context_menu(|ui| {
                crate::library::image_context_menu::show(ui, &mut app.state, image_id, true);
            });
        }

        action_outcome
    }
}

// ── Math & Color Helpers ───────────────────────────────────────────────────

fn ensure_before_view(app: &crate::app::FerroliteApp, frame: &eframe::Frame) {
    let Some(rs) = frame.wgpu_render_state() else {
        return;
    };
    let (active, image_id, is_raw, srgb_src, raw_src) = match app.state.viewer.as_ref() {
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
    {
        let renderer = rs.renderer.read();
        if let Some(g) = renderer
            .callback_resources
            .get::<crate::viewer::ViewerGpu>()
        {
            if g.image_id == image_id && g.preview_before.is_some() {
                return;
            }
        }
    }
    let gpu = ferrolite_gpu::GpuContext::from_render_state(rs);
    let (tex, dims) = if is_raw {
        let temp = app.current_wb_temp();
        let cam = app.camera_to_working(temp);
        let Some(src) = raw_src else {
            return;
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
        let pw = app.preview_to_working();
        let Some(src) = srgb_src else {
            return;
        };
        let ctx_arc = std::sync::Arc::new(ferrolite_gpu::GpuContext::from_render_state(rs));
        let out = ferrolite_pipeline::color_convert(ctx_arc, &src, pw);
        (out.texture.clone(), (out.width, out.height))
    };
    let vt = {
        let renderer = rs.renderer.read();
        let Some(vp) = renderer
            .callback_resources
            .get::<crate::viewer::ViewerPipelines>()
        else {
            return;
        };
        ferrolite_vt::VirtualTexture::single_from_texture(&gpu, tex, dims, &vp.pipelines)
    };
    let mut renderer = rs.renderer.write();
    if let Some(g) = renderer
        .callback_resources
        .get_mut::<crate::viewer::ViewerGpu>()
    {
        if g.image_id == image_id {
            g.preview_before = Some(vt);
        }
    }
}

fn rebuild_mask_overlay_if_needed(state: &mut AppState, frame: &eframe::Frame) {
    use crate::develop::mask_edit;
    use crate::develop::mask_overlay_color::OVERLAY_MAX_EDGE;
    use std::hash::Hash;
    use std::hash::Hasher;

    let Some(v) = state.viewer.as_mut() else {
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
    if v.mask.overlay_key == Some(key) && state.mask_overlay_native.is_some() {
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
        match state.mask_overlay_native {
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
                state.mask_overlay_native = Some(id);
            }
        }
        if let Some(highlight) = &highlight {
            let hview = highlight.srgb_view();
            match state.mask_overlay_highlight_native {
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
                    state.mask_overlay_highlight_native = Some(id);
                }
            }
        }
    }
    state.mask_overlay_gpu = Some(overlay);
    if let Some(highlight) = highlight {
        state.mask_overlay_highlight_gpu = Some(highlight);
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

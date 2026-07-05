//! egui↔wgpu paint callback for the viewer's `VirtualTexture`s.
//!
//! The heavy GPU resources (the preview rung-1 VT, the optional tier-2 sparse
//! VT, and a borrowed `GpuContext`) live in eframe's `callback_resources` type-map
//! as a single [`ViewerGpu`] holder — only one viewer is open at a time. The egui
//! `Callback` carries only the small `Copy` per-frame data (`view` + `viewport` +
//! the `present_source` selector); the `prepare`/`paint` split lets us build the
//! per-frame uniform + bind group where the device/queue is available (`prepare`)
//! and merely bind+draw where it is not (`paint`).

use egui_wgpu::CallbackTrait;
use ferrolite_gpu::GpuContext;
use ferrolite_vt::{DisplayPipelines, DisplayVariant, ViewTransform, VirtualTexture};

use crate::viewer::PresentSource;

/// Holder stashed in `callback_resources` at startup: the pre-warmed display
/// pipelines (compiled once for the surface's `target_format`). Every image
/// open borrows from this so no per-open pipeline compilation occurs.
pub struct ViewerPipelines {
    pub pipelines: DisplayPipelines,
    /// Once-built histogram compute pipeline (pre-warmed at startup, reused).
    pub histogram: ferrolite_vt::HistogramPipeline,
}

/// Which of the two rung-1 previews a callback draws: the edited `After`
/// (the normal preview) or the unedited `Before` (identity stack). Used by the
/// before/after split — two callbacks with the same rect but different clip rects.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PreviewWhich {
    After,
    Before,
}

/// Holder stashed in `callback_resources`: the viewer's GPU context plus the
/// rung-1 preview texture and (once tier-2 finishes) the sparse full-res VT.
pub struct ViewerGpu {
    pub ctx: GpuContext,
    /// Rung-1 single-texture preview. Painted until the full VT is shown.
    pub preview: VirtualTexture,
    /// Rung-4 sparse full-res VT (tier-2). `None` until `FullDecoded` arrives.
    pub full: Option<VirtualTexture>,
    /// Rung-1 "before" (unedited, `sRGB→working`) preview for the split view.
    /// Built on demand while split-compare is active; `None` otherwise.
    pub preview_before: Option<VirtualTexture>,
    /// Image id whose textures these are — guards against painting a holder that
    /// belongs to a viewer that has since been closed/replaced.
    pub image_id: i64,
    /// Off-screen "swapchain": the sparse tier is composed into `back` when
    /// converged, then swapped to `front`; the callback blits `front`.
    pub present: ferrolite_vt::PresentBuffers,
    /// 32-byte `BlitParams { alpha: f32, _pad: vec3<f32> }` uniform for the
    /// crossfade blit (vec3 forces 16-byte alignment → 32-byte struct).
    pub present_alpha: wgpu::Buffer,
    /// Cached bind group for blitting `present`'s front buffer. Rebuilt in
    /// `prepare` when the present source is `Front`/`Crossfade` (front view can
    /// change on resize/swap); `None` until first built.
    pub blit_bind_front: Option<wgpu::BindGroup>,
}

/// Per-frame paint command: small `Copy` data only. The textures are fetched from
/// `callback_resources` in both phases. `image_id` guards against painting a
/// holder that belongs to a different (newer) viewer than this callback was
/// enqueued for.
///
/// For the `After` path, `present_source` (computed by `drive_viewer` via the
/// pure `viewer::present_source`) selects what this frame shows: the rung-1
/// `Preview`, a `Crossfade(f)` of the composed `front` over the preview, or the
/// converged `Front` buffer alone. The `Before` split path ignores it and always
/// draws the preview-tier `preview_before`.
#[derive(Clone, Copy)]
pub struct ViewerCallback {
    pub image_id: i64,
    pub view: ViewTransform,
    pub viewport: (f32, f32),
    pub present_source: PresentSource,
    pub which: PreviewWhich,
}

/// Alpha for the crossfade blit that `present_source` maps to: `Front` blits at
/// full opacity, `Crossfade(f)` at `f`. Returns `None` when no blit is needed
/// (the `Preview` source). Pure — used by `prepare` to decide whether to build a
/// blit bind group and what alpha to write.
fn blit_alpha(source: PresentSource) -> Option<f32> {
    match source {
        PresentSource::Front => Some(1.0),
        PresentSource::Crossfade(f) => Some(f),
        PresentSource::Preview => None,
    }
}

impl CallbackTrait for ViewerCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // Two-borrow hazard: `CallbackResources` is a type-map, so we cannot hold
        // `&ViewerPipelines` and `&mut ViewerGpu` at once. Clone the (cheap) blit
        // Arcs first and DROP the `ViewerPipelines` borrow before `get_mut`.
        let blit_resources = if matches!(self.which, PreviewWhich::After)
            && blit_alpha(self.present_source).is_some()
        {
            resources.get::<ViewerPipelines>().map(|vp| {
                (
                    vp.pipelines.blit_layout().clone(),
                    vp.pipelines.sampler().clone(),
                )
            })
        } else {
            None
        };

        if let Some(g) = resources.get_mut::<ViewerGpu>() {
            if g.image_id == self.image_id {
                match self.which {
                    PreviewWhich::After => match blit_alpha(self.present_source) {
                        None => {
                            // `Preview`: draw the rung-1 transform-aware preview.
                            g.preview.prepare_single(&g.ctx, &self.view, self.viewport);
                        }
                        Some(alpha) => {
                            // `Front`/`Crossfade(f)`: blit the composed front buffer.
                            // For a crossfade the preview is drawn underneath first,
                            // so also prepare it here.
                            if matches!(self.present_source, PresentSource::Crossfade(_)) {
                                g.preview.prepare_single(&g.ctx, &self.view, self.viewport);
                            }
                            // Write the blit alpha as the 32-byte `BlitParams`
                            // (`[alpha, 0,0,0, 0,0,0,0]` f32) and (re)build the
                            // front-buffer bind group.
                            let params: [f32; 8] = [alpha, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
                            queue.write_buffer(&g.present_alpha, 0, bytemuck::bytes_of(&params));
                            if let Some((layout, sampler)) = blit_resources.as_ref() {
                                let bind =
                                    g.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                                        label: Some("viewer-blit-front"),
                                        layout,
                                        entries: &[
                                            wgpu::BindGroupEntry {
                                                binding: 0,
                                                resource: wgpu::BindingResource::TextureView(
                                                    g.present.front_view(),
                                                ),
                                            },
                                            wgpu::BindGroupEntry {
                                                binding: 1,
                                                resource: wgpu::BindingResource::Sampler(sampler),
                                            },
                                            wgpu::BindGroupEntry {
                                                binding: 2,
                                                resource: g.present_alpha.as_entire_binding(),
                                            },
                                        ],
                                    });
                                g.blit_bind_front = Some(bind);
                            }
                        }
                    },
                    PreviewWhich::Before => {
                        // Split is preview-tier only: the before is always rung-1.
                        if let Some(pb) = g.preview_before.as_mut() {
                            pb.prepare_single(&g.ctx, &self.view, self.viewport);
                        }
                    }
                }
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(g) = resources.get::<ViewerGpu>() {
            if g.image_id == self.image_id {
                match self.which {
                    PreviewWhich::After => match self.present_source {
                        PresentSource::Preview => g.preview.draw_single(pass),
                        PresentSource::Front => draw_blit_front(g, resources, pass),
                        PresentSource::Crossfade(_) => {
                            // Draw the preview first, then alpha-blend the composed
                            // front buffer over it (the blit pipeline uses ALPHA_BLENDING).
                            g.preview.draw_single(pass);
                            draw_blit_front(g, resources, pass);
                        }
                    },
                    PreviewWhich::Before => {
                        if let Some(pb) = g.preview_before.as_ref() {
                            pb.draw_single(pass);
                        }
                    }
                }
            }
        }
    }
}

/// Bind the blit pipeline + the front-buffer bind group and draw the fullscreen
/// triangle. Both borrows here are shared (`get`), so holding `&ViewerPipelines`
/// and `&ViewerGpu` from the same type-map at once is sound. No-op if the blit
/// pipeline or the bind group (built in `prepare`) is missing.
fn draw_blit_front(
    g: &ViewerGpu,
    resources: &egui_wgpu::CallbackResources,
    pass: &mut wgpu::RenderPass<'static>,
) {
    if let (Some(vp), Some(bind)) = (
        resources.get::<ViewerPipelines>(),
        g.blit_bind_front.as_ref(),
    ) {
        pass.set_pipeline(vp.pipelines.pipeline(DisplayVariant::Blit));
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

//! egui↔wgpu paint callback for the viewer's `VirtualTexture`s.
//!
//! The heavy GPU resources (the preview rung-1 VT, the optional tier-2 sparse
//! VT, and a borrowed `GpuContext`) live in eframe's `callback_resources` type-map
//! as a single [`ViewerGpu`] holder — only one viewer is open at a time. The egui
//! `Callback` carries only the small `Copy` per-frame data (`view` + `viewport` +
//! the `show_full` selector); the `prepare`/`paint` split lets us build the
//! per-frame uniform + bind group where the device/queue is available (`prepare`)
//! and merely bind+draw where it is not (`paint`).

use egui_wgpu::CallbackTrait;
use ferrolite_gpu::GpuContext;
use ferrolite_vt::{DisplayPipelines, ViewTransform, VirtualTexture};

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
    #[allow(dead_code)] // constructed by the split-compare render path in Task 6
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
}

/// Per-frame paint command: small `Copy` data only. The textures are fetched from
/// `callback_resources` in both phases. `image_id` guards against painting a
/// holder that belongs to a different (newer) viewer than this callback was
/// enqueued for. `show_full` selects the sparse VT once the crossfade decides
/// it is the sharp image to show (swap-on-ready, see `viewer::paint`).
pub struct ViewerCallback {
    pub image_id: i64,
    pub view: ViewTransform,
    pub viewport: (f32, f32),
    pub show_full: bool,
    pub which: PreviewWhich,
}

impl CallbackTrait for ViewerCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(g) = resources.get_mut::<ViewerGpu>() {
            if g.image_id == self.image_id {
                match self.which {
                    PreviewWhich::After => {
                        if self.show_full {
                            if let Some(full) = g.full.as_mut() {
                                full.prepare_sparse(&g.ctx, &self.view, self.viewport);
                            } else {
                                g.preview.prepare_single(&g.ctx, &self.view, self.viewport);
                            }
                        } else {
                            g.preview.prepare_single(&g.ctx, &self.view, self.viewport);
                        }
                    }
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
                    PreviewWhich::After => {
                        if self.show_full {
                            if let Some(full) = g.full.as_ref() {
                                full.draw_sparse(pass);
                            } else {
                                g.preview.draw_single(pass);
                            }
                        } else {
                            g.preview.draw_single(pass);
                        }
                    }
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

//! egui↔wgpu paint callback for the viewer's rung-1 `VirtualTexture`.
//!
//! The heavy GPU resources (the `VirtualTexture` + a borrowed `GpuContext`) live
//! in eframe's `callback_resources` type-map as a single [`ViewerGpu`] holder —
//! only one viewer is open at a time. The egui `Callback` carries only the small
//! `Copy` per-frame data (`view` + `viewport`); the `prepare`/`paint` split lets
//! us build the per-frame uniform + bind group where the device/queue is
//! available (`prepare`) and merely bind+draw where it is not (`paint`).

use egui_wgpu::CallbackTrait;
use ferrolite_gpu::GpuContext;
use ferrolite_vt::{ViewTransform, VirtualTexture};

/// Holder stashed in `callback_resources`: the viewer's rung-1 texture plus the
/// GPU context needed to rebuild its per-frame uniform/bind group each frame.
pub struct ViewerGpu {
    pub ctx: GpuContext,
    pub vt: VirtualTexture,
    /// Image id whose preview this VT holds — guards against painting a texture
    /// that belongs to a viewer that has since been closed/replaced.
    pub image_id: i64,
}

/// Per-frame paint command: small `Copy` data only. The VT is fetched from
/// `callback_resources` in both phases. `image_id` guards against painting a
/// holder that belongs to a different (newer) viewer than this callback was
/// enqueued for.
pub struct ViewerCallback {
    pub image_id: i64,
    pub view: ViewTransform,
    pub viewport: (f32, f32),
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
                g.vt.prepare_single(&g.ctx, &self.view, self.viewport);
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
                g.vt.draw_single(pass);
            }
        }
    }
}

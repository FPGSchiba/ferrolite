//! Off-screen double-buffered presentation for the viewer "swapchain": the sparse
//! tier is composed into `back` and, once converged, `front`<->`back` swap so the
//! egui callback only ever blits a complete image. Generic, photo-agnostic. §4.
use ferrolite_gpu::GpuContext;

pub struct PresentBuffers {
    size: (u32, u32),
    format: wgpu::TextureFormat,
    front: (wgpu::Texture, wgpu::TextureView),
    back: (wgpu::Texture, wgpu::TextureView),
}

impl PresentBuffers {
    pub fn new(ctx: &GpuContext, size: (u32, u32), format: wgpu::TextureFormat) -> Self {
        let front = Self::alloc(ctx, size, format);
        let back = Self::alloc(ctx, size, format);
        Self {
            size,
            format,
            front,
            back,
        }
    }

    fn alloc(
        ctx: &GpuContext,
        size: (u32, u32),
        format: wgpu::TextureFormat,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vt-present"),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        (tex, view)
    }

    /// Resize both present buffers to `size`, reallocating fresh (blank) textures
    /// for `front` and `back` when `size` actually differs from the current size.
    /// No-ops (and returns `false`) when `size` is unchanged. Returns `true` when
    /// it reallocated — callers MUST treat that as "both buffers are now blank"
    /// and re-arm whatever one-shot compose+swap guard would otherwise skip
    /// recomposing (e.g. because the view is still `converged`), or the canvas
    /// will keep showing a blank/clear-color buffer until the next pan/zoom/edit.
    pub fn resize(&mut self, ctx: &GpuContext, size: (u32, u32)) -> bool {
        if size == self.size {
            return false;
        }
        self.size = size;
        self.front = Self::alloc(ctx, size, self.format);
        self.back = Self::alloc(ctx, size, self.format);
        true
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }
    pub fn back_view(&self) -> &wgpu::TextureView {
        &self.back.1
    }
    pub fn front_view(&self) -> &wgpu::TextureView {
        &self.front.1
    }
    pub fn swap(&mut self) {
        std::mem::swap(&mut self.front, &mut self.back);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_changes_reported_size() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let mut p = PresentBuffers::new(&ctx, (64, 64), wgpu::TextureFormat::Bgra8UnormSrgb);
        assert_eq!(p.size(), (64, 64));
        let resized = p.resize(&ctx, (128, 96));
        assert_eq!(p.size(), (128, 96));
        assert!(resized, "actual size change must report true (reallocated)");
        let resized_again = p.resize(&ctx, (128, 96));
        assert_eq!(p.size(), (128, 96));
        assert!(
            !resized_again,
            "no-op resize to the same size must report false"
        );
    }

    #[test]
    fn swap_exchanges_front_and_back_views() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let mut p = PresentBuffers::new(&ctx, (32, 32), wgpu::TextureFormat::Bgra8UnormSrgb);
        // Capture the texture IDs before swap to verify they exchange positions
        let front_tex_id = p.front.0.global_id();
        p.swap();
        // Verify that the old front texture is now in the back position
        assert_eq!(p.back.0.global_id(), front_tex_id, "old front is now back");
    }
}

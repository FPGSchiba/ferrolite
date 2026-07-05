//! Luma-range shape evaluator (smooth band over input luma).

use std::sync::Arc;

use ferrolite_gpu::GpuContext;

use crate::buffer::MaskBuffer;
use crate::pass::SampledPass;

/// Uniform for `luma_range.wgsl` — 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LumaRangeUniform {
    pub lo: f32,
    pub hi: f32,
    pub softness: f32,
    pub _pad: f32,
}

impl LumaRangeUniform {
    pub fn from_params(lo: f32, hi: f32, softness: f32) -> Self {
        Self {
            lo,
            hi,
            softness,
            _pad: 0.0,
        }
    }
}

/// Build-once luma-range pass.
pub struct LumaRangePass {
    inner: SampledPass<LumaRangeUniform>,
}

impl LumaRangePass {
    pub fn new(ctx: Arc<GpuContext>) -> Self {
        Self {
            inner: SampledPass::new(
                ctx,
                include_str!("../shaders/luma_range.wgsl"),
                "mask-luma-range",
            ),
        }
    }

    pub fn run(
        &self,
        lo: f32,
        hi: f32,
        softness: f32,
        input: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> MaskBuffer {
        self.inner.run(
            LumaRangeUniform::from_params(lo, hi, softness),
            input,
            width,
            height,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_maps_params() {
        let u = LumaRangeUniform::from_params(0.2, 0.8, 0.05);
        assert_eq!((u.lo, u.hi, u.softness), (0.2, 0.8, 0.05));
    }
}

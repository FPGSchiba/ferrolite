//! Linear-gradient shape evaluator.

use std::sync::Arc;

use ferrolite_gpu::GpuContext;

use crate::buffer::MaskBuffer;
use crate::pass::GenPass;
use crate::vec::Vec2;

/// Uniform for `linear_gradient.wgsl`. 16 bytes, no padding needed.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LinearGradientUniform {
    pub start: [f32; 2],
    pub end: [f32; 2],
}

impl LinearGradientUniform {
    pub fn from_params(start: Vec2, end: Vec2) -> Self {
        Self {
            start: [start.x, start.y],
            end: [end.x, end.y],
        }
    }
}

/// Build-once linear-gradient pass.
pub struct LinearGradientPass {
    inner: GenPass<LinearGradientUniform>,
}

impl LinearGradientPass {
    pub fn new(ctx: Arc<GpuContext>) -> Self {
        Self {
            inner: GenPass::new(
                ctx,
                include_str!("../shaders/linear_gradient.wgsl"),
                "mask-linear-gradient",
            ),
        }
    }

    pub fn run(&self, start: Vec2, end: Vec2, width: u32, height: u32) -> MaskBuffer {
        self.inner.run(
            LinearGradientUniform::from_params(start, end),
            width,
            height,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_maps_params_verbatim() {
        let u = LinearGradientUniform::from_params(Vec2::new(0.1, 0.2), Vec2::new(0.3, 0.4));
        assert_eq!(u.start, [0.1, 0.2]);
        assert_eq!(u.end, [0.3, 0.4]);
    }
}

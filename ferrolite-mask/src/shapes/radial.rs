//! Radial-gradient (ellipse) shape evaluator.

use std::sync::Arc;

use ferrolite_gpu::GpuContext;

use crate::buffer::MaskBuffer;
use crate::pass::GenPass;
use crate::vec::Vec2;

/// Uniform for `radial_gradient.wgsl` — 32 bytes (padded to a 16-byte multiple).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RadialGradientUniform {
    pub center: [f32; 2],
    pub radius: [f32; 2],
    pub rotation: f32,
    pub feather: f32,
    pub invert: f32,
    pub _pad: f32,
}

impl RadialGradientUniform {
    pub fn from_params(
        center: Vec2,
        radius: Vec2,
        rotation: f32,
        feather: f32,
        invert: bool,
    ) -> Self {
        Self {
            center: [center.x, center.y],
            radius: [radius.x, radius.y],
            rotation,
            feather,
            invert: if invert { 1.0 } else { 0.0 },
            _pad: 0.0,
        }
    }
}

pub struct RadialGradientPass {
    inner: GenPass<RadialGradientUniform>,
}

impl RadialGradientPass {
    pub fn new(ctx: Arc<GpuContext>) -> Self {
        Self {
            inner: GenPass::new(
                ctx,
                include_str!("../shaders/radial_gradient.wgsl"),
                "mask-radial-gradient",
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        center: Vec2,
        radius: Vec2,
        rotation: f32,
        feather: f32,
        invert: bool,
        width: u32,
        height: u32,
    ) -> MaskBuffer {
        self.inner.run(
            RadialGradientUniform::from_params(center, radius, rotation, feather, invert),
            width,
            height,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invert_flag_maps_to_float() {
        let a = RadialGradientUniform::from_params(
            Vec2::new(0.5, 0.5),
            Vec2::new(0.3, 0.2),
            0.0,
            0.1,
            false,
        );
        assert_eq!(a.invert, 0.0);
        let b = RadialGradientUniform::from_params(
            Vec2::new(0.5, 0.5),
            Vec2::new(0.3, 0.2),
            0.0,
            0.1,
            true,
        );
        assert_eq!(b.invert, 1.0);
    }
}

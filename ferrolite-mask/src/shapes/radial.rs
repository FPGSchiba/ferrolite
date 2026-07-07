//! Radial-gradient (ellipse) shape evaluator.

use std::sync::Arc;

use ferrolite_gpu::GpuContext;

use crate::buffer::MaskBuffer;
use crate::pass::GenPass;
use crate::vec::Vec2;

/// Uniform for `radial_gradient.wgsl` — 48 bytes (multiple of 16).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RadialGradientUniform {
    pub center: [f32; 2],
    pub radius: [f32; 2],
    pub rotation: f32,
    pub feather: f32,
    pub invert: f32,
    pub _pad: f32,
    pub uv_scale: [f32; 2],
    pub uv_offset: [f32; 2],
}

impl RadialGradientUniform {
    #[allow(clippy::too_many_arguments)]
    pub fn from_params(
        center: Vec2,
        radius: Vec2,
        rotation: f32,
        feather: f32,
        invert: bool,
        uv_scale: [f32; 2],
        uv_offset: [f32; 2],
    ) -> Self {
        Self {
            center: [center.x, center.y],
            radius: [radius.x, radius.y],
            rotation,
            feather,
            invert: if invert { 1.0 } else { 0.0 },
            _pad: 0.0,
            uv_scale,
            uv_offset,
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
        uv_scale: [f32; 2],
        uv_offset: [f32; 2],
        width: u32,
        height: u32,
    ) -> MaskBuffer {
        self.inner.run(
            RadialGradientUniform::from_params(
                center, radius, rotation, feather, invert, uv_scale, uv_offset,
            ),
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
            [1.0, 1.0],
            [0.0, 0.0],
        );
        assert_eq!(a.invert, 0.0);
        let b = RadialGradientUniform::from_params(
            Vec2::new(0.5, 0.5),
            Vec2::new(0.3, 0.2),
            0.0,
            0.1,
            true,
            [1.0, 1.0],
            [0.0, 0.0],
        );
        assert_eq!(b.invert, 1.0);
    }
}

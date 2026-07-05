//! Color-range shape evaluator (smooth color-distance selection).

use std::sync::Arc;

use ferrolite_gpu::GpuContext;

use crate::buffer::MaskBuffer;
use crate::pass::SampledPass;
use crate::vec::Rgb;

/// Maximum number of color samples packed into the uniform (mirrors the HSL
/// pass's fixed 8-slot array). Extra samples are ignored (documented).
pub const MAX_COLOR_SAMPLES: usize = 8;

/// Uniform for `color_range.wgsl` — 8 × vec4 samples (128B) + 16B tail = 144B.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ColorRangeUniform {
    pub samples: [[f32; 4]; MAX_COLOR_SAMPLES],
    pub count: f32,
    pub tolerance: f32,
    pub softness: f32,
    pub _pad: f32,
}

impl ColorRangeUniform {
    pub fn from_params(samples: &[Rgb], tolerance: f32, softness: f32) -> Self {
        let mut packed = [[0.0f32; 4]; MAX_COLOR_SAMPLES];
        let n = samples.len().min(MAX_COLOR_SAMPLES);
        for (slot, s) in packed.iter_mut().zip(samples.iter()).take(n) {
            *slot = [s.r, s.g, s.b, 0.0];
        }
        Self {
            samples: packed,
            count: n as f32,
            tolerance,
            softness,
            _pad: 0.0,
        }
    }
}

pub struct ColorRangePass {
    inner: SampledPass<ColorRangeUniform>,
}

impl ColorRangePass {
    pub fn new(ctx: Arc<GpuContext>) -> Self {
        Self {
            inner: SampledPass::new(
                ctx,
                include_str!("../shaders/color_range.wgsl"),
                "mask-color-range",
            ),
        }
    }

    pub fn run(
        &self,
        samples: &[Rgb],
        tolerance: f32,
        softness: f32,
        input: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) -> MaskBuffer {
        self.inner.run(
            ColorRangeUniform::from_params(samples, tolerance, softness),
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
    fn packs_samples_and_counts() {
        let u = ColorRangeUniform::from_params(
            &[Rgb::new(1.0, 0.0, 0.0), Rgb::new(0.0, 1.0, 0.0)],
            0.2,
            0.1,
        );
        assert_eq!(u.count, 2.0);
        assert_eq!(u.samples[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(u.samples[1], [0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn clamps_to_max_samples() {
        let many: Vec<Rgb> = (0..12).map(|_| Rgb::new(0.5, 0.5, 0.5)).collect();
        let u = ColorRangeUniform::from_params(&many, 0.1, 0.1);
        assert_eq!(u.count, MAX_COLOR_SAMPLES as f32);
    }
}

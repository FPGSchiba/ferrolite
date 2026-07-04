//! GPU upload wrappers for the lens-correction warp grid + vignette LUT baked
//! by `ferrolite-lens`. Photo tier — these are built-once resources re-created
//! only when a new bake arrives (U7 wires that); no per-frame allocation here.
//!
//! ## Warp texture format + sampling scheme (read this before writing U5's shader)
//!
//! The grid stores 6 floats/node: `[rU,rV, gU,gV, bU,bV]` (normalized [0,1]
//! source coords per channel, for TCA). It must be sampled **bilinearly** by
//! the geometry compute shader, and it must be precise near the image edges
//! (coords approach 1.0) even on a 45MP source, where a half-float's ~3
//! significant decimal digits already alias to multiple source pixels.
//!
//! `rgba16float` was rejected for exactly that reason: absolute normalized
//! coords near 1.0 lose too much precision in `f16`.
//!
//! The obvious fix — `rgba32float` — is filterable-if-and-only-if the device
//! enables `wgpu::Features::FLOAT32_FILTERABLE`. `ferrolite_gpu::GpuContext`
//! requests `wgpu::Features::empty()` (see `ferrolite-gpu/src/context.rs`), so
//! a `Filtering` sampler over an `rgba32float`/`rg32float` view would fail
//! bind-group creation at runtime. We do NOT enable that feature (it would
//! ripple through every `GpuContext` caller for one shader's sake).
//!
//! **Chosen scheme:** two full-precision `f32` textures, sampled by
//! `textureLoad` (never `textureSample`) with **manual bilinear interpolation
//! done in the compute shader** (4 texel fetches + a lerp) in U5:
//! - `rg_ba`: `n×n` `Rgba32Float` holding `[rU, rV, gU, gV]`.
//! - `b_uv`: `n×n` `Rg32Float` holding `[bU, bV]`.
//!
//! Both are created with `TEXTURE_BINDING | COPY_DST` usage — `STORAGE_BINDING`
//! is not needed (read-only `textureLoad`), and no sampler is created or bound:
//! `textureLoad` addresses texels directly and never needs one. This keeps full
//! `f32` precision end-to-end with zero new device features.

use ferrolite_gpu::GpuContext;
use ferrolite_lens::{VignetteMap, WarpGrid};
use wgpu::util::DeviceExt;

/// GPU-resident warp grid: `rg_ba` = `[rU,rV,gU,gV]`, `b_uv` = `[bU,bV]`, both
/// `n×n`. Sampled via `textureLoad` + manual bilinear in the geometry shader
/// (see module docs) — no sampler is needed or created.
pub struct WarpGridTexture {
    pub n: u32,
    rg_ba: wgpu::Texture,
    b_uv: wgpu::Texture,
}

impl WarpGridTexture {
    /// A `1×1` identity grid (source coords `[0,0]` for every channel) so a
    /// bind group referencing this texture is valid before any lens bake
    /// completes. Paired with `LensUniform.use_warp = 0` so the shader skips
    /// the grid sample entirely; the content here is never actually read.
    pub fn identity(ctx: &GpuContext) -> Self {
        let rg_ba = create_rgba32f(ctx, 1, 1, &[0.0, 0.0, 0.0, 0.0]);
        let b_uv = create_rg32f(ctx, 1, 1, &[0.0, 0.0]);
        Self { n: 1, rg_ba, b_uv }
    }

    /// Upload a freshly baked `WarpGrid`, replacing any previous content.
    /// `grid.coords[y*n + x] = [rU,rV,gU,gV,bU,bV]`; split into the two
    /// textures' texel layout (row-major, same `n`).
    pub fn upload(ctx: &GpuContext, grid: &WarpGrid) -> Self {
        let n = grid.n;
        let count = (n * n) as usize;
        debug_assert_eq!(grid.coords.len(), count, "WarpGrid coords must be n*n");

        let mut rg_ba_data = Vec::with_capacity(count * 4);
        let mut b_uv_data = Vec::with_capacity(count * 2);
        for c in &grid.coords {
            rg_ba_data.extend_from_slice(&[c[0], c[1], c[2], c[3]]);
            b_uv_data.extend_from_slice(&[c[4], c[5]]);
        }

        let rg_ba = create_rgba32f(ctx, n, n, &rg_ba_data);
        let b_uv = create_rg32f(ctx, n, n, &b_uv_data);
        Self { n, rg_ba, b_uv }
    }

    /// View over `[rU,rV,gU,gV]`.
    pub fn rg_ba_view(&self) -> wgpu::TextureView {
        self.rg_ba
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// View over `[bU,bV]`.
    pub fn b_uv_view(&self) -> wgpu::TextureView {
        self.b_uv
            .create_view(&wgpu::TextureViewDescriptor::default())
    }
}

/// GPU-resident radial vignette-gain LUT: a `len×1` `R32Float` texture (one
/// texel per LUT entry). Sampled via `textureLoad` in the shader for the same
/// precision/feature-availability reasons as the warp grid — see module docs.
pub struct VignetteTexture {
    pub len: u32,
    tex: wgpu::Texture,
}

impl VignetteTexture {
    /// A single-texel identity LUT (gain 1.0 everywhere it could be sampled).
    pub fn identity(ctx: &GpuContext) -> Self {
        let tex = create_r32f(ctx, 1, &[1.0]);
        Self { len: 1, tex }
    }

    /// Upload a freshly baked `VignetteMap`.
    pub fn upload(ctx: &GpuContext, map: &VignetteMap) -> Self {
        let len = map.radial.len() as u32;
        let tex = create_r32f(ctx, len, &map.radial);
        Self { len, tex }
    }

    pub fn view(&self) -> wgpu::TextureView {
        self.tex
            .create_view(&wgpu::TextureViewDescriptor::default())
    }
}

fn create_rgba32f(ctx: &GpuContext, w: u32, h: u32, data: &[f32]) -> wgpu::Texture {
    ctx.device.create_texture_with_data(
        &ctx.queue,
        &wgpu::TextureDescriptor {
            label: Some("lens-warp-rg-ba"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(data),
    )
}

fn create_rg32f(ctx: &GpuContext, w: u32, h: u32, data: &[f32]) -> wgpu::Texture {
    ctx.device.create_texture_with_data(
        &ctx.queue,
        &wgpu::TextureDescriptor {
            label: Some("lens-warp-b-uv"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(data),
    )
}

fn create_r32f(ctx: &GpuContext, len: u32, data: &[f32]) -> wgpu::Texture {
    ctx.device.create_texture_with_data(
        &ctx.queue,
        &wgpu::TextureDescriptor {
            label: Some("lens-vignette"),
            size: wgpu::Extent3d {
                width: len,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(data),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_upload_build_without_panicking() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (expected in headless CI)");
            return;
        };
        let warp_id = WarpGridTexture::identity(&ctx);
        assert_eq!(warp_id.n, 1);
        let _ = warp_id.rg_ba_view();
        let _ = warp_id.b_uv_view();

        let vig_id = VignetteTexture::identity(&ctx);
        assert_eq!(vig_id.len, 1);
        let _ = vig_id.view();

        let grid = WarpGrid {
            n: 2,
            coords: vec![
                [0.0, 0.0, 0.1, 0.1, 0.2, 0.2],
                [0.3, 0.3, 0.4, 0.4, 0.5, 0.5],
                [0.6, 0.6, 0.7, 0.7, 0.8, 0.8],
                [0.9, 0.9, 1.0, 1.0, 1.0, 1.0],
            ],
            max_disp: 12.0,
        };
        let warp = WarpGridTexture::upload(&ctx, &grid);
        assert_eq!(warp.n, 2);

        let vig = VignetteTexture::upload(
            &ctx,
            &VignetteMap {
                radial: vec![1.0, 0.9, 0.8, 0.7],
            },
        );
        assert_eq!(vig.len, 4);
    }
}

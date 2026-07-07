//! `MaskOverlayCompositor` — composites a `MaskDefinition` against a (bounded,
//! downscaled) input image and tints it into a GPU-native `OverlayTexture` (no
//! CPU readback) for the Develop canvas overlay. Reuses `ferrolite_mask::MaskCompositor`
//! (the same passes the edit DAG uses), so the overlay is faithful to the actual
//! mask. The app caches one instance (built once) and a bounded input; it calls
//! `overlay_texture` only when the mask/preview/toggle change (never
//! unconditionally per frame).

use std::sync::Arc;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{MaskCompositor, MaskDefinition, RasterStore};

use crate::image::PipelineImage;

/// Premultiplied **linear** red overlay tint for a coverage value, mirroring the
/// `mask_overlay_tint.wgsl` fragment shader exactly. Returns `[r, g, b, a]` with
/// `a = clamp(coverage) * clamp(strength)` and `r = a` (premultiplied red),
/// `g = b = 0`. The GPU pass stores these into a linear `Rgba8Unorm` target
/// (byte = value*255); an sRGB view is then handed to egui, so the on-screen
/// texel matches the former CPU overlay (`Color32::from_rgba_unmultiplied(255,0,0,a)`
/// premultiplies to the same `(a,0,0,a)`).
pub fn overlay_tint(coverage: f32, strength: f32) -> [f32; 4] {
    let a = coverage.clamp(0.0, 1.0) * strength.clamp(0.0, 1.0);
    [a, 0.0, 0.0, a]
}

/// The linear render/storage format of the overlay target. An `Rgba8UnormSrgb`
/// view (added via `view_formats`) is what egui samples, so bytes written here
/// linearly (value*255) are interpreted by egui as sRGB — matching the former
/// managed overlay texture texel-for-texel.
const OVERLAY_LINEAR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const OVERLAY_SRGB_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// A GPU overlay texture: premultiplied red tint of a composited mask. Format is
/// `Rgba8Unorm` with an `Rgba8UnormSrgb` view format so it can be handed to egui.
pub struct OverlayTexture {
    pub texture: Arc<wgpu::Texture>,
    pub width: u32,
    pub height: u32,
}

impl OverlayTexture {
    /// An `Rgba8UnormSrgb` view — pass this to `register_native_texture`.
    pub fn srgb_view(&self) -> wgpu::TextureView {
        self.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(OVERLAY_SRGB_FORMAT),
            ..Default::default()
        })
    }
}

pub struct MaskOverlayCompositor {
    compositor: MaskCompositor,
    ctx: Arc<GpuContext>,
    tint_pipeline: wgpu::RenderPipeline,
    tint_bgl: wgpu::BindGroupLayout,
}

impl MaskOverlayCompositor {
    pub fn new(ctx: Arc<GpuContext>) -> Self {
        let tint_bgl = ctx
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mask-overlay-tint-bgl"),
                entries: &[
                    // 0: coverage (R32Float, non-filterable, textureLoad — no sampler)
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    // 1: TintParams uniform
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let module = ctx.shader_module(
            "mask-overlay-tint",
            include_str!("shaders/mask_overlay_tint.wgsl"),
        );
        let layout = ctx
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mask-overlay-tint-pl"),
                bind_group_layouts: &[&tint_bgl],
                push_constant_ranges: &[],
            });
        let tint_pipeline = ctx
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("mask-overlay-tint-pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: "vs_main",
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: "fs_main",
                    targets: &[Some(OVERLAY_LINEAR_FORMAT.into())],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });
        Self {
            compositor: MaskCompositor::new(ctx.clone()),
            ctx,
            tint_pipeline,
            tint_bgl,
        }
    }

    /// Composite `def` against `input` (on the GPU) and tint it premultiplied red
    /// into a fresh `Rgba8Unorm` texture (dims = `input` dims). NO readback.
    pub fn overlay_texture(
        &self,
        def: &MaskDefinition,
        input: &PipelineImage,
        strength: f32,
    ) -> OverlayTexture {
        use wgpu::util::DeviceExt;
        let (w, h) = (input.width, input.height);
        let iv = input
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let coverage = self
            .compositor
            .composite(def, &iv, w, h, &RasterStore::default());

        let target = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mask-overlay-target"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OVERLAY_LINEAR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC, // COPY_SRC for the golden test readback
            view_formats: &[OVERLAY_SRGB_FORMAT],
        });
        let target = Arc::new(target);
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default()); // linear
        let cov_view = coverage
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let params: [f32; 4] = [strength.clamp(0.0, 1.0), 0.0, 0.0, 0.0];
        let ubuf = self
            .ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mask-overlay-tint-params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mask-overlay-tint-bind"),
                layout: &self.tint_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&cov_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: ubuf.as_entire_binding(),
                    },
                ],
            });

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mask-overlay-tint-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.tint_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.ctx.queue.submit([enc.finish()]);

        OverlayTexture {
            texture: target,
            width: w.max(1),
            height: h.max(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_tint_is_premultiplied_red_and_clamped() {
        assert_eq!(
            overlay_tint(0.0, 0.5),
            [0.0, 0.0, 0.0, 0.0],
            "zero coverage -> transparent"
        );
        assert_eq!(
            overlay_tint(1.0, 0.5),
            [0.5, 0.0, 0.0, 0.5],
            "full coverage -> premul red at strength"
        );
        // premultiplied: rgb.r always equals alpha
        let t = overlay_tint(0.4, 0.5);
        assert_eq!(t[0], t[3], "red channel is premultiplied by alpha");
        assert_eq!([t[1], t[2]], [0.0, 0.0], "green/blue are zero");
        // clamps coverage and strength into [0,1]
        assert_eq!(
            overlay_tint(-0.2, 0.5),
            [0.0, 0.0, 0.0, 0.0],
            "negative coverage clamps to 0"
        );
        assert_eq!(
            overlay_tint(1.5, 2.0),
            [1.0, 0.0, 0.0, 1.0],
            "over-range clamps to 1"
        );
    }

    #[test]
    fn overlay_texture_tints_premultiplied_red_ramp() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let oc = MaskOverlayCompositor::new(ctx.clone());
        // 8x1 mid-grey input; a left→right linear-gradient coverage.
        let src = ferrolite_image::LinearRgbaF32::new(8, 1, vec![0.5; 8 * 4]).unwrap();
        let img = crate::nodes::upload_source(&ctx, &src);
        let def = MaskDefinition {
            components: vec![(
                ferrolite_mask::MaskComponent::LinearGradient {
                    start: ferrolite_mask::Vec2::new(0.0, 0.5),
                    end: ferrolite_mask::Vec2::new(1.0, 0.5),
                },
                ferrolite_mask::CompositeMode::Add,
            )],
            invert: false,
        };
        let tex = oc.overlay_texture(&def, &img, 0.5);
        assert_eq!((tex.width, tex.height), (8, 1));
        // Read the LINEAR view bytes: byte == round(coverage*0.5*255), premultiplied.
        let bytes = ctx.read_rgba8(&tex.texture, 8, 1);
        // Every texel: green/blue zero, red == alpha (premultiplied).
        for x in 0..8usize {
            let px = &bytes[x * 4..x * 4 + 4];
            assert_eq!(px[1], 0, "green zero at {x}");
            assert_eq!(px[2], 0, "blue zero at {x}");
            assert_eq!(px[0], px[3], "red == alpha (premultiplied) at {x}");
        }
        // Alpha ramps left→right (coverage increases).
        assert!(
            bytes[3] < bytes[7 * 4 + 3],
            "alpha ramps L->R: {} !< {}",
            bytes[3],
            bytes[7 * 4 + 3]
        );
        // Full-ish coverage at the right edge is ~50% strength => ~128.
        assert!(
            bytes[7 * 4 + 3] > 96 && bytes[7 * 4 + 3] <= 130,
            "right edge ~50% alpha, got {}",
            bytes[7 * 4 + 3]
        );
    }
}

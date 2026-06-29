//! VirtualTexture rung 1: the whole image as one `Rgba16Float` texture, sampled
//! by the display shader with a zoom/pan transform. Also the fallback path.

use ferrolite_gpu::GpuContext;
use ferrolite_image::{tiles_per_level, LinearRgbaF32, TileCoord, TILE_SIZE};
use half::f16;
use wgpu::util::DeviceExt;

use crate::{TileSource, ViewTransform};

/// Max LOD levels the `TileMeta` uniform can carry (matches `array<vec4<u32>, 8>` in WGSL).
const MAX_LEVELS: usize = 8;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransformUniform {
    zoom: f32,
    _pad0: f32,
    pan: [f32; 2],
    viewport: [f32; 2],
    image: [f32; 2],
}

/// `TileMeta` uniform: must match the WGSL `struct TileMeta` layout exactly.
/// `levels[lod] = (cols, flat_offset, _, _)`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TileMetaUniform {
    level_count: u32,
    // WGSL `_pad: vec3<u32>` aligns to 16 and sits at offset 16, so `levels`
    // starts at offset 32. Use a full 32-byte header (7 padding u32s) to match.
    _pad: [u32; 7],
    levels: [[u32; 4]; MAX_LEVELS],
}

/// Rung-2 GPU resources: a 2-D-array texture holding every tile of every LOD,
/// a CPU-built `(lod,x,y) -> layer` slot table, the `TileMeta` uniform, plus the
/// bind-group layout and the `fs_tiled` pipeline.
pub struct TiledResources {
    array_view: wgpu::TextureView,
    slots_buf: wgpu::Buffer,
    meta_buf: wgpu::Buffer,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    image_dims: (u32, u32),
    // keep the texture alive (the view borrows from it conceptually)
    _array_tex: wgpu::Texture,
}

/// Rung-1 (single-texture) GPU resources.
struct SingleResources {
    texture: wgpu::Texture,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    image_dims: (u32, u32),
}

pub struct VirtualTexture {
    single: Option<SingleResources>,
    tiled: Option<TiledResources>,
}

impl VirtualTexture {
    pub fn single_texture(
        ctx: &GpuContext,
        image: &LinearRgbaF32,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let device = &ctx.device;
        // f32 -> f16 RGBA.
        let texels: Vec<f16> = image.pixels.iter().map(|&v| f16::from_f32(v)).collect();
        let texture = device.create_texture_with_data(
            &ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("vt-single"),
                size: wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&texels),
        );

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vt-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
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

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vt-display"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/display.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vt-pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vt-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(target_format.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vt-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            single: Some(SingleResources {
                texture,
                bind_group_layout: bgl,
                sampler,
                pipeline,
                image_dims: (image.width, image.height),
            }),
            tiled: None,
        }
    }

    pub fn render(
        &self,
        ctx: &GpuContext,
        pass: &mut wgpu::RenderPass<'_>,
        view: &ViewTransform,
        viewport: (f32, f32),
    ) {
        let single = self
            .single
            .as_ref()
            .expect("render called on a non-single VirtualTexture");
        let uniform = TransformUniform {
            zoom: view.zoom,
            _pad0: 0.0,
            pan: [view.pan.0, view.pan.1],
            viewport: [viewport.0, viewport.1],
            image: [single.image_dims.0 as f32, single.image_dims.1 as f32],
        };
        let ubuf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vt-xf"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let tview = single
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-bind"),
            layout: &single.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tview),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&single.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ubuf.as_entire_binding(),
                },
            ],
        });
        pass.set_pipeline(&single.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Offscreen render to an `Rgba8Unorm` image (golden tests).
    pub fn render_to_image(
        ctx: &GpuContext,
        image: &LinearRgbaF32,
        view: &ViewTransform,
        viewport: (f32, f32),
        out_w: u32,
        out_h: u32,
    ) -> Vec<u8> {
        let vt = Self::single_texture(ctx, image, wgpu::TextureFormat::Rgba8Unorm);
        let target = ctx.render_target(out_w, out_h, wgpu::TextureFormat::Rgba8Unorm);
        let tview = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vt-offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &tview,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            vt.render(ctx, &mut pass, view, viewport);
        }
        ctx.queue.submit([enc.finish()]);
        ctx.read_rgba8(&target, out_w, out_h)
    }

    /// Rung 2: upload ALL tiles of ALL LOD levels into a `texture_2d_array`
    /// (one layer per `(lod, x, y)` slot), build the slot table + `TileMeta`,
    /// and create the `fs_tiled` pipeline. Everything stays resident.
    pub fn tiled_resident(
        ctx: &GpuContext,
        source: &dyn TileSource,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let device = &ctx.device;
        let (img_w, img_h) = source.level_size(0);
        let level_count = source.level_count().min(MAX_LEVELS as u32);

        // Per-level tile grid + flat slot offsets. cols/rows are derived from the
        // source's own level size (ceil-div TILE_SIZE) rather than the image's
        // pyramid math, so a source whose levels diverge still lays out correctly.
        let mut cols_per_level = Vec::with_capacity(level_count as usize);
        let mut offsets = Vec::with_capacity(level_count as usize);
        let mut total_tiles: u32 = 0;
        for lod in 0..level_count {
            let (lw, lh) = source.level_size(lod);
            let cols = lw.div_ceil(TILE_SIZE);
            let rows = lh.div_ceil(TILE_SIZE);
            cols_per_level.push(cols);
            offsets.push(total_tiles);
            total_tiles += cols * rows;
            // tiles_per_level is referenced to keep the documented contract visible;
            // it agrees with the ceil-div above for PyramidTileSource.
            debug_assert_eq!(tiles_per_level(img_w, img_h, lod), (cols, rows));
        }
        let total_tiles = total_tiles.max(1);

        // Create the array texture (one layer per slot) and upload each tile.
        let array_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vt-tiled-array"),
            size: wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: total_tiles,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Build the slot table: slots[offset + y*cols + x] = layer_index.
        let mut slots: Vec<u32> = vec![0u32; total_tiles as usize];
        let mut layer: u32 = 0;
        for lod in 0..level_count {
            let (lw, lh) = source.level_size(lod);
            let cols = lw.div_ceil(TILE_SIZE);
            let rows = lh.div_ceil(TILE_SIZE);
            let offset = offsets[lod as usize];
            for y in 0..rows {
                for x in 0..cols {
                    let tile = source.tile(TileCoord { lod, x, y });
                    let texels: Vec<f16> = tile.pixels.iter().map(|&v| f16::from_f32(v)).collect();
                    ctx.queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture: &array_tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: 0,
                                z: layer,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        bytemuck::cast_slice(&texels),
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(TILE_SIZE * 4 * 2), // RGBA * f16
                            rows_per_image: Some(TILE_SIZE),
                        },
                        wgpu::Extent3d {
                            width: TILE_SIZE,
                            height: TILE_SIZE,
                            depth_or_array_layers: 1,
                        },
                    );
                    slots[(offset + y * cols + x) as usize] = layer;
                    layer += 1;
                }
            }
        }

        let array_view = array_tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("vt-tiled-array-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        // TileMeta uniform.
        let mut levels = [[0u32; 4]; MAX_LEVELS];
        for lod in 0..level_count as usize {
            levels[lod] = [cols_per_level[lod], offsets[lod], 0, 0];
        }
        let meta = TileMetaUniform {
            level_count,
            _pad: [0; 7],
            levels,
        };
        let meta_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vt-tile-meta"),
            contents: bytemuck::bytes_of(&meta),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let slots_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vt-slots"),
            contents: bytemuck::cast_slice(&slots),
            usage: wgpu::BufferUsages::STORAGE,
        });

        // Bind-group layout: 0-2 as rung 1, plus 3=array tex, 4=slots, 5=meta.
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vt-tiled-bgl"),
            entries: &[
                // binding 0 (`img_tex`) is declared in the shared module but not used
                // by `fs_tiled`, so it is intentionally omitted from this layout.
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
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

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vt-display"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/display.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vt-tiled-pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vt-tiled-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_tiled",
                targets: &[Some(target_format.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vt-tiled-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let tiled = TiledResources {
            array_view,
            slots_buf,
            meta_buf,
            bind_group_layout: bgl,
            sampler,
            pipeline,
            image_dims: (img_w, img_h),
            _array_tex: array_tex,
        };

        Self {
            single: None,
            tiled: Some(tiled),
        }
    }

    /// Rung 2: bind the tiled resources and draw the full-screen triangle.
    pub fn render_tiled(
        &self,
        ctx: &GpuContext,
        pass: &mut wgpu::RenderPass<'_>,
        view: &ViewTransform,
        viewport: (f32, f32),
    ) {
        let tiled = self
            .tiled
            .as_ref()
            .expect("render_tiled called on a non-tiled VirtualTexture");
        let uniform = TransformUniform {
            zoom: view.zoom,
            _pad0: 0.0,
            pan: [view.pan.0, view.pan.1],
            viewport: [viewport.0, viewport.1],
            image: [tiled.image_dims.0 as f32, tiled.image_dims.1 as f32],
        };
        let ubuf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vt-tiled-xf"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-tiled-bind"),
            layout: &tiled.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&tiled.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ubuf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&tiled.array_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: tiled.slots_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: tiled.meta_buf.as_entire_binding(),
                },
            ],
        });
        pass.set_pipeline(&tiled.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Rung 2 offscreen render to an `Rgba8Unorm` image (golden tests).
    pub fn render_tiled_to_image(
        ctx: &GpuContext,
        source: &dyn TileSource,
        view: &ViewTransform,
        viewport: (f32, f32),
        out_w: u32,
        out_h: u32,
    ) -> Vec<u8> {
        let vt = Self::tiled_resident(ctx, source, wgpu::TextureFormat::Rgba8Unorm);
        let target = ctx.render_target(out_w, out_h, wgpu::TextureFormat::Rgba8Unorm);
        let tview = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vt-tiled-offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &tview,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            vt.render_tiled(ctx, &mut pass, view, viewport);
        }
        ctx.queue.submit([enc.finish()]);
        ctx.read_rgba8(&target, out_w, out_h)
    }
}

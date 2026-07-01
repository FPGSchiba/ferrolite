//! VirtualTexture rung 1: the whole image as one `Rgba16Float` texture, sampled
//! by the display shader with a zoom/pan transform. Also the fallback path.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use ferrolite_gpu::GpuContext;
use ferrolite_image::{tiles_per_level, LinearRgbaF32, TileCoord, TILE_SIZE};
use ferrolite_jobs::{JobHandle, JobSystem, Priority};
use half::f16;
use wgpu::util::DeviceExt;

use crate::page_table::{FeedbackBuffer, LevelLayout as FlatLayout, PageTable};
use crate::pipelines::{DisplayPipelines, DisplayVariant};
use crate::pool::{SlotAllocator, TilePool, NOT_RESIDENT};
use crate::producer::TileProducer;
use crate::residency::{needed_tiles, ResidencySet, VersionedResidency};
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
    bind_group_layout: Arc<wgpu::BindGroupLayout>,
    sampler: Arc<wgpu::Sampler>,
    pipeline: Arc<wgpu::RenderPipeline>,
    image_dims: (u32, u32),
    // keep the texture alive (the view borrows from it conceptually)
    _array_tex: wgpu::Texture,
}

/// Rung-1 (single-texture) GPU resources.
struct SingleResources {
    texture: std::sync::Arc<wgpu::Texture>,
    texture_view: wgpu::TextureView,
    bind_group_layout: Arc<wgpu::BindGroupLayout>,
    sampler: Arc<wgpu::Sampler>,
    pipeline: Arc<wgpu::RenderPipeline>,
    image_dims: (u32, u32),
    /// Per-frame uniform buffer (transform), reused and rewritten via `prepare_single`.
    uniform_buf: wgpu::Buffer,
    /// Bind group built in `prepare_single`, consumed by `draw_single`.
    bind_group: Option<wgpu::BindGroup>,
}

/// Per-level slot-table geometry: tiles-per-row and flat-slot offset for each LOD.
struct LevelLayout {
    cols: Vec<u32>,
    offsets: Vec<u32>,
    total_tiles: u32,
    level_count: u32,
}

/// Rung-3 streaming resources: a budget-limited physical `TilePool`, the CPU
/// residency brain (`ResidencySet` + `SlotAllocator`), a job-driven load channel,
/// and the GPU bind resources (slots buffer mirrors the per-frame `(lod,tile)->slot`
/// indirection used by the shader's coarse-LOD fallback).
struct StreamingResources {
    pool: TilePool,
    allocator: SlotAllocator,
    residency: ResidencySet,
    layout: LevelLayout,
    source: Arc<dyn TileSource + Send + Sync>,
    jobs: Arc<JobSystem>,
    tx: Sender<(TileCoord, LinearRgbaF32)>,
    // `Mutex` so the enclosing `VirtualTexture` is `Sync` (required to live in
    // eframe's `callback_resources` for the rung-1 path). Drained single-threaded
    // on the render thread, so the lock is uncontended.
    rx: Mutex<Receiver<(TileCoord, LinearRgbaF32)>>,
    in_flight: HashMap<TileCoord, JobHandle>,
    // CPU mirror of the slot table (all NOT_RESIDENT until a tile is uploaded).
    slots: Vec<u32>,

    // GPU bind resources.
    array_view: wgpu::TextureView,
    slots_buf: wgpu::Buffer,
    meta_buf: wgpu::Buffer,
    bind_group_layout: Arc<wgpu::BindGroupLayout>,
    sampler: Arc<wgpu::Sampler>,
    pipeline: Arc<wgpu::RenderPipeline>,
    image_dims: (u32, u32),
    /// Per-frame transform uniform, rewritten by `prepare_streaming` (so the
    /// egui-callback paint split has no allocation in the render pass).
    uniform_buf: wgpu::Buffer,
    /// Bind group built in `prepare_streaming`, consumed by `draw_streaming`.
    bind_group: Option<wgpu::BindGroup>,
}

/// Rung-4 sparse resources: everything in `StreamingResources`, but the NEEDED
/// set is GPU-truth (read back from a `FeedbackBuffer` the display shader marks)
/// rather than a CPU rect estimate, and the shader resolves slots through a
/// `PageTable` indirection texture instead of a plain storage buffer.
struct SparseResources {
    pool: TilePool,
    allocator: SlotAllocator,
    residency: ResidencySet,
    layout: FlatLayout,
    source: Arc<dyn TileSource + Send + Sync>,
    jobs: Arc<JobSystem>,
    tx: Sender<(TileCoord, LinearRgbaF32)>,
    // See `StreamingResources::rx` — `Mutex` keeps `VirtualTexture: Sync`.
    rx: Mutex<Receiver<(TileCoord, LinearRgbaF32)>>,
    in_flight: HashMap<TileCoord, JobHandle>,
    // CPU mirror of the per-tile slot (all NOT_RESIDENT until uploaded).
    slots: Vec<u32>,

    // Plan 3: edited-tile version tracking + producer-drive bookkeeping. The
    // producer object itself is NOT stored here (it is !Send/!Sync); it is passed
    // to `produce_view` per call. `producing` suppresses CPU job submission in
    // `request_view_feedback` while the producer fills tiles instead.
    versions: VersionedResidency,
    producing: bool,
    last_needed: Vec<TileCoord>,

    // GPU bind resources.
    page_table: PageTable,
    feedback: FeedbackBuffer,
    array_view: wgpu::TextureView,
    meta_buf: wgpu::Buffer,
    bind_group_layout: Arc<wgpu::BindGroupLayout>,
    sampler: Arc<wgpu::Sampler>,
    pipeline: Arc<wgpu::RenderPipeline>,
    image_dims: (u32, u32),
    /// Per-frame transform uniform, rewritten by `prepare_sparse` (so the
    /// egui-callback paint split has no allocation in the render pass).
    uniform_buf: wgpu::Buffer,
    /// Bind group built in `prepare_sparse`, consumed by `draw_sparse`.
    bind_group: Option<wgpu::BindGroup>,
}

pub struct VirtualTexture {
    single: Option<SingleResources>,
    tiled: Option<TiledResources>,
    streaming: Option<StreamingResources>,
    sparse: Option<SparseResources>,
}

impl VirtualTexture {
    pub fn single_texture(
        ctx: &GpuContext,
        image: &LinearRgbaF32,
        pipelines: &DisplayPipelines,
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

        let bgl = pipelines.layout(DisplayVariant::Single).clone();
        let pipeline = pipelines.pipeline(DisplayVariant::Single).clone();
        let sampler = pipelines.sampler().clone();

        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Persistent uniform buffer rewritten each frame by `prepare_single`.
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-xf"),
            size: std::mem::size_of::<TransformUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            single: Some(SingleResources {
                texture: std::sync::Arc::new(texture),
                texture_view,
                bind_group_layout: bgl,
                sampler,
                pipeline,
                image_dims: (image.width, image.height),
                uniform_buf,
                bind_group: None,
            }),
            tiled: None,
            streaming: None,
            sparse: None,
        }
    }

    /// Image dimensions of the rung-1 texture, if this is a single-texture VT.
    pub fn single_dims(&self) -> Option<(u32, u32)> {
        self.single.as_ref().map(|s| s.image_dims)
    }

    /// Replace the rung-1 single texture with an externally-owned GPU texture
    /// (e.g. an edit-pipeline output). The texture must be `Rgba16Float` with
    /// `TEXTURE_BINDING` usage. The next `prepare_single` rebuilds the bind group
    /// from the new view; a no-op on a non-single VT.
    pub fn update_single_from_texture(
        &mut self,
        texture: std::sync::Arc<wgpu::Texture>,
        dims: (u32, u32),
    ) {
        if let Some(s) = self.single.as_mut() {
            s.texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            s.texture = texture;
            s.image_dims = dims;
            s.bind_group = None; // force rebuild in prepare_single
        }
    }

    /// Prepare-half of the rung-1 paint (egui_wgpu `CallbackTrait::prepare`):
    /// rewrite the per-frame transform uniform (via `queue.write_buffer`, no new
    /// allocation) and build the bind group, stashing it for `draw_single`.
    /// Call once per frame before `draw_single`.
    pub fn prepare_single(&mut self, ctx: &GpuContext, view: &ViewTransform, viewport: (f32, f32)) {
        let single = self
            .single
            .as_mut()
            .expect("prepare_single called on a non-single VirtualTexture");
        let uniform = TransformUniform {
            zoom: view.zoom,
            _pad0: 0.0,
            pan: [view.pan.0, view.pan.1],
            viewport: [viewport.0, viewport.1],
            image: [single.image_dims.0 as f32, single.image_dims.1 as f32],
        };
        ctx.queue
            .write_buffer(&single.uniform_buf, 0, bytemuck::bytes_of(&uniform));
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-bind"),
            layout: &single.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&single.texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&single.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: single.uniform_buf.as_entire_binding(),
                },
            ],
        });
        single.bind_group = Some(bind);
    }

    /// Paint-half of the rung-1 paint (egui_wgpu `CallbackTrait::paint`): set the
    /// pipeline + the bind group built by `prepare_single` and draw the
    /// full-screen triangle. No device/queue needed. A no-op if `prepare_single`
    /// has not run.
    pub fn draw_single(&self, pass: &mut wgpu::RenderPass<'_>) {
        let single = self
            .single
            .as_ref()
            .expect("draw_single called on a non-single VirtualTexture");
        let Some(bind) = single.bind_group.as_ref() else {
            return;
        };
        pass.set_pipeline(&single.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
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
        pipelines: &DisplayPipelines,
    ) -> Vec<u8> {
        debug_assert_eq!(pipelines.target_format(), wgpu::TextureFormat::Rgba8Unorm);
        let vt = Self::single_texture(ctx, image, pipelines);
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
        pipelines: &DisplayPipelines,
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

        // Bind-group layout + `fs_tiled` pipeline + sampler are cached in
        // DisplayPipelines (rung 2 and rung 3 share the Tiled bgl + `fs_tiled`).
        let bgl = pipelines.layout(DisplayVariant::Tiled).clone();
        let pipeline = pipelines.pipeline(DisplayVariant::Tiled).clone();
        let sampler = pipelines.sampler().clone();

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
            streaming: None,
            sparse: None,
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
        pipelines: &DisplayPipelines,
    ) -> Vec<u8> {
        debug_assert_eq!(pipelines.target_format(), wgpu::TextureFormat::Rgba8Unorm);
        let vt = Self::tiled_resident(ctx, source, pipelines);
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

    /// Rung 3: a budget-limited streaming virtual texture. Allocates a physical
    /// `TilePool` of `budget_tiles` slots (NO tiles uploaded up front), builds the
    /// `(lod,tile)->slot` indirection (all `NOT_RESIDENT`) and the `fs_tiled`
    /// pipeline. Tiles are loaded on demand by `request_view` + `drain_loaded`.
    pub fn streaming(
        ctx: &GpuContext,
        source: Arc<dyn TileSource + Send + Sync>,
        jobs: Arc<JobSystem>,
        budget_tiles: u32,
        pipelines: &DisplayPipelines,
    ) -> Self {
        let device = &ctx.device;
        let (img_w, img_h) = source.level_size(0);
        let level_count = source.level_count().min(MAX_LEVELS as u32);

        // Per-level slot-table geometry (drives both the shader's index math and
        // the CPU mirror). Derived from the source's own level sizes.
        let mut cols = Vec::with_capacity(level_count as usize);
        let mut offsets = Vec::with_capacity(level_count as usize);
        let mut total_tiles: u32 = 0;
        for lod in 0..level_count {
            let (lw, lh) = source.level_size(lod);
            let c = lw.div_ceil(TILE_SIZE);
            let r = lh.div_ceil(TILE_SIZE);
            cols.push(c);
            offsets.push(total_tiles);
            total_tiles += c * r;
        }
        let total_tiles = total_tiles.max(1);
        let layout = LevelLayout {
            cols,
            offsets,
            total_tiles,
            level_count,
        };

        let pool = TilePool::new(ctx, budget_tiles);
        let array_view = pool.texture().create_view(&wgpu::TextureViewDescriptor {
            label: Some("vt-stream-pool-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        // Slot table starts fully non-resident.
        let slots: Vec<u32> = vec![NOT_RESIDENT; total_tiles as usize];

        // TileMeta uniform (same layout as rung 2).
        let mut levels = [[0u32; 4]; MAX_LEVELS];
        for (lod, slot) in levels.iter_mut().enumerate().take(level_count as usize) {
            *slot = [layout.cols[lod], layout.offsets[lod], 0, 0];
        }
        let meta = TileMetaUniform {
            level_count,
            _pad: [0; 7],
            levels,
        };
        let meta_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vt-stream-meta"),
            contents: bytemuck::bytes_of(&meta),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        // Slots buffer needs COPY_DST so we can rewrite it each frame.
        let slots_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vt-stream-slots"),
            contents: bytemuck::cast_slice(&slots),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group_layout = pipelines.layout(DisplayVariant::Streaming).clone();
        let sampler = pipelines.sampler().clone();
        let pipeline = pipelines.pipeline(DisplayVariant::Streaming).clone();

        // Persistent transform uniform, rewritten each frame by `prepare_streaming`.
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-stream-xf"),
            size: std::mem::size_of::<TransformUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (tx, rx) = std::sync::mpsc::channel();

        let streaming = StreamingResources {
            pool,
            allocator: SlotAllocator::new(budget_tiles),
            residency: ResidencySet::new(budget_tiles as usize),
            layout,
            source,
            jobs,
            tx,
            rx: Mutex::new(rx),
            in_flight: HashMap::new(),
            slots,
            array_view,
            slots_buf,
            meta_buf,
            bind_group_layout,
            sampler,
            pipeline,
            image_dims: (img_w, img_h),
            uniform_buf,
            bind_group: None,
        };

        Self {
            single: None,
            tiled: None,
            streaming: Some(streaming),
            sparse: None,
        }
    }

    /// Rung 3: reconcile the resident set with the current view. Computes the
    /// needed tiles, diffs against residency, frees evicted slots, cancels loads
    /// no longer needed, submits `Visible` jobs for newly-needed tiles, and drains
    /// any tiles that finished loading (uploading them to the pool on this thread).
    /// GPU access is single-threaded — call this from the main/render thread.
    pub fn request_view(&mut self, ctx: &GpuContext, view: &ViewTransform, viewport: (f32, f32)) {
        // First absorb anything that finished since last frame.
        self.drain_loaded(ctx);

        let s = self
            .streaming
            .as_mut()
            .expect("request_view called on a non-streaming VirtualTexture");

        let needed = needed_tiles(s.image_dims, view, viewport, s.layout.level_count);
        let (to_load, to_evict) = s.residency.diff(&needed);

        // Evict: free physical slots + drop residency + clear the slot-table entry.
        for t in &to_evict {
            s.allocator.free(*t);
            s.residency.forget(*t);
            if let Some(idx) = slot_index(&s.layout, *t) {
                s.slots[idx] = NOT_RESIDENT;
            }
        }

        // Cancel in-flight loads for tiles no longer needed.
        let stale: Vec<TileCoord> = s
            .in_flight
            .keys()
            .copied()
            .filter(|t| !needed.contains(t))
            .collect();
        for t in stale {
            if let Some(h) = s.in_flight.remove(&t) {
                h.cancel();
            }
        }

        // Submit loads for newly-needed tiles not already resident or in flight.
        for t in to_load {
            if s.residency.contains(t) || s.in_flight.contains_key(&t) {
                continue;
            }
            let tx = s.tx.clone();
            let source = Arc::clone(&s.source);
            let coord = t;
            let handle = s.jobs.submit(Priority::Visible, move |token| {
                if token.is_cancelled() {
                    return;
                }
                let tile = source.tile(coord);
                // Receiver gone (VT dropped) => ignore.
                let _ = tx.send((coord, tile));
            });
            s.in_flight.insert(t, handle);
        }

        // Push the (possibly updated) slot table to the GPU for this frame.
        ctx.queue
            .write_buffer(&s.slots_buf, 0, bytemuck::cast_slice(&s.slots));
    }

    /// Drain tiles that finished loading and make them resident: allocate a slot,
    /// upload pixels into the pool, touch residency (LRU), update the slot table.
    /// Returns the number of tiles made resident. Runs on the main thread.
    pub fn drain_loaded(&mut self, ctx: &GpuContext) -> usize {
        let s = match self.streaming.as_mut() {
            Some(s) => s,
            None => return 0,
        };
        let mut made_resident = 0;
        // Collect first so we don't hold the receiver borrow across mutation.
        // Poison-tolerant: a panicked tile job poisons the mutex, but the
        // receiver itself is still valid, so recover the inner value.
        let rx = s.rx.get_mut().unwrap_or_else(|e| e.into_inner());
        let ready: Vec<(TileCoord, LinearRgbaF32)> = rx.try_iter().collect();
        for (coord, tile) in ready {
            s.in_flight.remove(&coord);
            // If a budget-driven eviction freed a slot, evict an LRU resident to
            // make room when the pool is full.
            let slot = match s.allocator.alloc(coord) {
                Some(slot) => slot,
                None => {
                    // Pool full: evict the LRU resident tile to free a slot.
                    if let Some(victim) = s.residency.lru() {
                        s.allocator.free(victim);
                        s.residency.forget(victim);
                        if let Some(idx) = slot_index(&s.layout, victim) {
                            s.slots[idx] = NOT_RESIDENT;
                        }
                    }
                    match s.allocator.alloc(coord) {
                        Some(slot) => slot,
                        None => continue, // still no room (capacity 0); drop tile
                    }
                }
            };
            s.pool.upload(ctx, slot, &tile);
            s.residency.touch(coord);
            if let Some(idx) = slot_index(&s.layout, coord) {
                s.slots[idx] = slot;
            }
            made_resident += 1;
        }
        if made_resident > 0 {
            ctx.queue
                .write_buffer(&s.slots_buf, 0, bytemuck::cast_slice(&s.slots));
        }
        made_resident
    }

    /// Rung 3: number of tiles still in flight (jobs submitted, not yet drained).
    /// `None` if this is not a streaming VT. Used by the viewer to decide whether
    /// to keep requesting repaints and whether the full image is "settled" enough
    /// to finish the crossfade. Note: 0 in-flight means everything *requested*
    /// has arrived; the next `request_view` may submit more if the view changed.
    pub fn streaming_pending(&self) -> Option<usize> {
        self.streaming.as_ref().map(|s| s.in_flight.len())
    }

    /// Rung 3: cancel every in-flight tile-load job. Called on navigation so a
    /// superseded image's tile jobs stop competing with the newly-opened one.
    /// Idempotent; a no-op on a non-streaming VT.
    pub fn cancel_streaming(&mut self) {
        if let Some(s) = self.streaming.as_mut() {
            for (_coord, handle) in s.in_flight.drain() {
                handle.cancel();
            }
        }
    }

    /// Prepare-half of the rung-3 paint (egui_wgpu `CallbackTrait::prepare`):
    /// rewrite the per-frame transform uniform and build the bind group, stashing
    /// it for `draw_streaming`. The slot table is uploaded separately by
    /// `request_view`/`drain_loaded`. Call once per frame before `draw_streaming`.
    pub fn prepare_streaming(
        &mut self,
        ctx: &GpuContext,
        view: &ViewTransform,
        viewport: (f32, f32),
    ) {
        let s = self
            .streaming
            .as_mut()
            .expect("prepare_streaming called on a non-streaming VirtualTexture");
        let uniform = TransformUniform {
            zoom: view.zoom,
            _pad0: 0.0,
            pan: [view.pan.0, view.pan.1],
            viewport: [viewport.0, viewport.1],
            image: [s.image_dims.0 as f32, s.image_dims.1 as f32],
        };
        ctx.queue
            .write_buffer(&s.uniform_buf, 0, bytemuck::bytes_of(&uniform));
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-stream-bind"),
            layout: &s.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&s.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: s.uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&s.array_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: s.slots_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: s.meta_buf.as_entire_binding(),
                },
            ],
        });
        s.bind_group = Some(bind);
    }

    /// Paint-half of the rung-3 paint (egui_wgpu `CallbackTrait::paint`): bind the
    /// pipeline + the bind group built by `prepare_streaming` and draw the
    /// full-screen triangle. No device/queue needed; a no-op if `prepare_streaming`
    /// has not run.
    pub fn draw_streaming(&self, pass: &mut wgpu::RenderPass<'_>) {
        let s = self
            .streaming
            .as_ref()
            .expect("draw_streaming called on a non-streaming VirtualTexture");
        let Some(bind) = s.bind_group.as_ref() else {
            return;
        };
        pass.set_pipeline(&s.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Rung 3: bind the streaming resources and draw the full-screen triangle.
    pub fn render_streaming(
        &self,
        ctx: &GpuContext,
        pass: &mut wgpu::RenderPass<'_>,
        view: &ViewTransform,
        viewport: (f32, f32),
    ) {
        let s = self
            .streaming
            .as_ref()
            .expect("render_streaming called on a non-streaming VirtualTexture");
        let uniform = TransformUniform {
            zoom: view.zoom,
            _pad0: 0.0,
            pan: [view.pan.0, view.pan.1],
            viewport: [viewport.0, viewport.1],
            image: [s.image_dims.0 as f32, s.image_dims.1 as f32],
        };
        let ubuf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vt-stream-xf"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-stream-bind"),
            layout: &s.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&s.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ubuf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&s.array_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: s.slots_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: s.meta_buf.as_entire_binding(),
                },
            ],
        });
        pass.set_pipeline(&s.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Rung 4: the full engine-style sparse virtual texture. Like `streaming`, but
    /// the display shader resolves slots through a `PageTable` indirection texture
    /// and marks the tiles it actually wanted into a `FeedbackBuffer`. The CPU reads
    /// that feedback back (one frame latent) to compute the GPU-truth needed set in
    /// `request_view_feedback`, replacing the rung-3 CPU rect estimate.
    pub fn sparse(
        ctx: &GpuContext,
        source: Arc<dyn TileSource + Send + Sync>,
        jobs: Arc<JobSystem>,
        budget_tiles: u32,
        pipelines: &DisplayPipelines,
    ) -> Self {
        let device = &ctx.device;
        let (img_w, img_h) = source.level_size(0);
        let level_count = source.level_count().min(MAX_LEVELS as u32);

        // Per-level (cols, rows) grid; drives both the flat layout and TileMeta.
        let mut dims = Vec::with_capacity(level_count as usize);
        for lod in 0..level_count {
            let (lw, lh) = source.level_size(lod);
            dims.push((lw.div_ceil(TILE_SIZE), lh.div_ceil(TILE_SIZE)));
        }
        let layout = FlatLayout::new(&dims);
        let total_tiles = layout.total().max(1);

        let pool = TilePool::new(ctx, budget_tiles);
        let array_view = pool.texture().create_view(&wgpu::TextureViewDescriptor {
            label: Some("vt-sparse-pool-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        // TileMeta uniform (cols + flat offset per level), same layout as rungs 2/3.
        let mut levels = [[0u32; 4]; MAX_LEVELS];
        for (lod, slot) in levels.iter_mut().enumerate().take(level_count as usize) {
            let (cols, _) = layout.dims(lod as u32);
            slot[0] = cols;
            slot[1] = layout.offsets()[lod];
        }
        let meta = TileMetaUniform {
            level_count,
            _pad: [0; 7],
            levels,
        };
        let meta_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vt-sparse-meta"),
            contents: bytemuck::bytes_of(&meta),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        // Page table starts fully non-resident; feedback starts clear.
        let slots: Vec<u32> = vec![NOT_RESIDENT; total_tiles as usize];
        let page_table = PageTable::new(ctx, total_tiles);
        page_table.update(ctx, &slots);
        let feedback = FeedbackBuffer::new(ctx, total_tiles);
        feedback.clear(ctx);

        let bind_group_layout = pipelines.layout(DisplayVariant::Sparse).clone();
        let sampler = pipelines.sampler().clone();
        let pipeline = pipelines.pipeline(DisplayVariant::Sparse).clone();

        // Persistent transform uniform, rewritten each frame by `prepare_sparse`.
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vt-sparse-xf"),
            size: std::mem::size_of::<TransformUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (tx, rx) = std::sync::mpsc::channel();

        let sparse = SparseResources {
            pool,
            allocator: SlotAllocator::new(budget_tiles),
            residency: ResidencySet::new(budget_tiles as usize),
            layout,
            source,
            jobs,
            tx,
            rx: Mutex::new(rx),
            in_flight: HashMap::new(),
            slots,
            versions: VersionedResidency::new(),
            producing: false,
            last_needed: Vec::new(),
            page_table,
            feedback,
            array_view,
            meta_buf,
            bind_group_layout,
            sampler,
            pipeline,
            image_dims: (img_w, img_h),
            uniform_buf,
            bind_group: None,
        };

        Self {
            single: None,
            tiled: None,
            streaming: None,
            sparse: Some(sparse),
        }
    }

    /// Rung 4: reconcile residency against GPU-truth visibility. Reads back the
    /// previous frame's feedback (the tiles the shader actually wanted), diffs vs
    /// residency, frees evicted slots, cancels stale loads, submits `Visible` jobs
    /// for missing tiles, drains any that finished (uploading + allocating), then
    /// pushes the updated page table and clears feedback for the next frame.
    /// GPU access is single-threaded — call from the main/render thread.
    ///
    /// Call exactly once per frame: it reads the PRIOR frame's feedback marks
    /// (visibility is one frame latent) and clears feedback before returning, so
    /// the current frame's `draw_sparse` paint starts from a clean slate.
    pub fn request_view_feedback(&mut self, ctx: &GpuContext) {
        // Absorb anything that finished loading since last frame.
        self.drain_loaded_sparse(ctx);

        let s = self
            .sparse
            .as_mut()
            .expect("request_view_feedback called on a non-sparse VirtualTexture");

        // GPU-truth needed set: the tiles the shader marked last frame.
        let needed = s.feedback.read_back(ctx, &s.layout);
        s.last_needed = needed.clone();
        let (to_load, to_evict) = s.residency.diff(&needed);

        // Evict: free physical slots + drop residency + clear the page-table entry.
        for t in &to_evict {
            s.allocator.free(*t);
            s.residency.forget(*t);
            s.versions.forget(*t);
            if let Some(idx) = flat_index(&s.layout, *t) {
                s.slots[idx] = NOT_RESIDENT;
            }
        }

        // Cancel in-flight loads for tiles no longer needed.
        let needed_set: std::collections::HashSet<TileCoord> = needed.iter().copied().collect();
        let stale: Vec<TileCoord> = s
            .in_flight
            .keys()
            .copied()
            .filter(|t| !needed_set.contains(t))
            .collect();
        for t in stale {
            if let Some(h) = s.in_flight.remove(&t) {
                h.cancel();
            }
        }

        // Submit loads for newly-needed tiles not already resident or in flight.
        // Skipped when producer-driven: the producer fills tiles instead of CPU jobs.
        if !s.producing {
            for t in to_load {
                if s.residency.contains(t) || s.in_flight.contains_key(&t) {
                    continue;
                }
                let tx = s.tx.clone();
                let source = Arc::clone(&s.source);
                let coord = t;
                let handle = s.jobs.submit(Priority::Visible, move |token| {
                    if token.is_cancelled() {
                        return;
                    }
                    let tile = source.tile(coord);
                    let _ = tx.send((coord, tile));
                });
                s.in_flight.insert(t, handle);
            }
        }

        // Push the (possibly updated) page table and clear feedback for next frame.
        s.page_table.update(ctx, &s.slots);
        s.feedback.clear(ctx);
    }

    /// Drain tiles that finished loading into the sparse pool: allocate a slot
    /// (evicting an LRU resident if the pool is full), upload pixels, touch
    /// residency, and update the CPU slot mirror. Returns the count made resident.
    pub fn drain_loaded_sparse(&mut self, ctx: &GpuContext) -> usize {
        let s = match self.sparse.as_mut() {
            Some(s) => s,
            None => return 0,
        };
        let mut made_resident = 0;
        // Poison-tolerant: recover the receiver even if a panicked tile job
        // poisoned the mutex (the channel itself remains valid).
        let rx = s.rx.get_mut().unwrap_or_else(|e| e.into_inner());
        let ready: Vec<(TileCoord, LinearRgbaF32)> = rx.try_iter().collect();
        for (coord, tile) in ready {
            s.in_flight.remove(&coord);
            let slot = match s.allocator.alloc(coord) {
                Some(slot) => slot,
                None => {
                    // Pool full: evict the LRU resident tile to free a slot.
                    if let Some(victim) = s.residency.lru() {
                        s.allocator.free(victim);
                        s.residency.forget(victim);
                        s.versions.forget(victim);
                        if let Some(idx) = flat_index(&s.layout, victim) {
                            s.slots[idx] = NOT_RESIDENT;
                        }
                    }
                    match s.allocator.alloc(coord) {
                        Some(slot) => slot,
                        None => continue, // still no room (capacity 0); drop tile
                    }
                }
            };
            s.pool.upload(ctx, slot, &tile);
            s.residency.touch(coord);
            if let Some(idx) = flat_index(&s.layout, coord) {
                s.slots[idx] = slot;
            }
            made_resident += 1;
        }
        if made_resident > 0 {
            s.page_table.update(ctx, &s.slots);
        }
        made_resident
    }

    /// Rung 4: bind the sparse resources (page table + feedback) and draw the
    /// full-screen triangle. The shader marks feedback as a side effect.
    pub fn render_sparse(
        &self,
        ctx: &GpuContext,
        pass: &mut wgpu::RenderPass<'_>,
        view: &ViewTransform,
        viewport: (f32, f32),
    ) {
        let s = self
            .sparse
            .as_ref()
            .expect("render_sparse called on a non-sparse VirtualTexture");
        let uniform = TransformUniform {
            zoom: view.zoom,
            _pad0: 0.0,
            pan: [view.pan.0, view.pan.1],
            viewport: [viewport.0, viewport.1],
            image: [s.image_dims.0 as f32, s.image_dims.1 as f32],
        };
        let ubuf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vt-sparse-xf"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-sparse-bind"),
            layout: &s.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&s.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ubuf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&s.array_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: s.meta_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(s.page_table.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: s.feedback.buffer().as_entire_binding(),
                },
            ],
        });
        pass.set_pipeline(&s.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Prepare-half of the rung-4 paint (egui_wgpu `CallbackTrait::prepare`):
    /// rewrite the per-frame transform uniform and build the bind group (page
    /// table + feedback + pool + sampler + uniform), stashing it for `draw_sparse`.
    /// The page table is uploaded separately by `request_view_feedback`. Call once
    /// per frame before `draw_sparse`.
    ///
    /// The feedback buffer is bound read-write here: `fs_sparse` does `atomicOr`
    /// into it during paint, and `request_view_feedback` reads/clears it next frame.
    pub fn prepare_sparse(&mut self, ctx: &GpuContext, view: &ViewTransform, viewport: (f32, f32)) {
        let s = self
            .sparse
            .as_mut()
            .expect("prepare_sparse called on a non-sparse VirtualTexture");
        let uniform = TransformUniform {
            zoom: view.zoom,
            _pad0: 0.0,
            pan: [view.pan.0, view.pan.1],
            viewport: [viewport.0, viewport.1],
            image: [s.image_dims.0 as f32, s.image_dims.1 as f32],
        };
        ctx.queue
            .write_buffer(&s.uniform_buf, 0, bytemuck::bytes_of(&uniform));
        let bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vt-sparse-bind"),
            layout: &s.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&s.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: s.uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&s.array_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: s.meta_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(s.page_table.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: s.feedback.buffer().as_entire_binding(),
                },
            ],
        });
        s.bind_group = Some(bind);
    }

    /// Paint-half of the rung-4 paint (egui_wgpu `CallbackTrait::paint`): bind the
    /// pipeline + the bind group built by `prepare_sparse` and draw the full-screen
    /// triangle (which marks feedback as a side effect). No device/queue needed; a
    /// no-op if `prepare_sparse` has not run.
    pub fn draw_sparse(&self, pass: &mut wgpu::RenderPass<'_>) {
        let s = self
            .sparse
            .as_ref()
            .expect("draw_sparse called on a non-sparse VirtualTexture");
        let Some(bind) = s.bind_group.as_ref() else {
            return;
        };
        pass.set_pipeline(&s.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Rung 4: number of tiles still in flight (jobs submitted, not yet drained).
    /// `None` if this is not a sparse VT. Mirrors `streaming_pending`: used by the
    /// viewer to gate the crossfade swap and terminate the repaint loop. Note: 0
    /// in-flight means everything requested has arrived; the next
    /// `request_view_feedback` may submit more once the feedback marks change.
    pub fn sparse_pending(&self) -> Option<usize> {
        self.sparse.as_ref().map(|s| s.in_flight.len())
    }

    /// Rung 4: cancel every in-flight tile-load job. Called on navigation so a
    /// superseded image's tile jobs stop competing with the newly-opened one.
    /// Idempotent; a no-op on a non-sparse VT. Mirrors `cancel_streaming`.
    pub fn cancel_sparse(&mut self) {
        if let Some(s) = self.sparse.as_mut() {
            for (_coord, handle) in s.in_flight.drain() {
                handle.cancel();
            }
        }
    }

    /// Plan 3: mark this sparse VT producer-driven. While `on`, `request_view_
    /// feedback` keeps reconciling residency + clearing feedback but skips CPU
    /// load-job submission (the producer fills tiles instead). No-op if non-sparse.
    pub fn set_producing(&mut self, on: bool) {
        if let Some(s) = self.sparse.as_mut() {
            s.producing = on;
        }
    }

    /// Plan 3: the needed set from the most recent `request_view_feedback`
    /// (GPU-truth). The producer-drive loop produces these. Empty if non-sparse
    /// or before the first reconcile.
    pub fn needed_now(&self) -> Vec<TileCoord> {
        self.sparse
            .as_ref()
            .map(|s| s.last_needed.clone())
            .unwrap_or_default()
    }

    /// Plan 3: set the active opstack version. On change, free the slots of every
    /// resident tile produced at an older version, clear their CPU slot-mirror
    /// entries, AND flush the GPU page table so the shader never samples a
    /// freed/aliased slot for a frame. No-op if unchanged or non-sparse.
    pub fn set_opstack_version(&mut self, ctx: &GpuContext, version: u64) {
        let Some(s) = self.sparse.as_mut() else {
            return;
        };
        let stale = s.versions.set_version(version);
        for t in &stale {
            s.allocator.free(*t);
            s.residency.forget(*t);
            if let Some(idx) = flat_index(&s.layout, *t) {
                s.slots[idx] = NOT_RESIDENT;
            }
        }
        if !stale.is_empty() {
            s.page_table.update(ctx, &s.slots);
        }
    }

    /// Plan 3: render up to `budget` not-current tiles from `needed` (in order)
    /// via the passed `producer`, copy each into its pool slot, update residency
    /// + page table. Returns the count produced.
    ///
    /// The producer is borrowed per call (it is !Send/!Sync, owned by
    /// `ViewerState`). Runs on the render thread (GPU work); bounded by `budget`
    /// per call. No-op (0) on a non-sparse VT.
    pub fn produce_view(
        &mut self,
        ctx: &GpuContext,
        producer: &mut dyn TileProducer,
        needed: &[TileCoord],
        budget: usize,
    ) -> usize {
        let Some(s) = self.sparse.as_mut() else {
            return 0;
        };

        let to_produce = s.versions.to_produce(needed);
        let mut produced = 0;
        for coord in to_produce.into_iter().take(budget) {
            // Allocate a slot, evicting an LRU resident if the pool is full.
            let slot = match s.allocator.alloc(coord) {
                Some(slot) => slot,
                None => {
                    if let Some(victim) = s.residency.lru() {
                        s.allocator.free(victim);
                        s.residency.forget(victim);
                        s.versions.forget(victim);
                        if let Some(idx) = flat_index(&s.layout, victim) {
                            s.slots[idx] = NOT_RESIDENT;
                        }
                    }
                    match s.allocator.alloc(coord) {
                        Some(slot) => slot,
                        None => continue, // capacity 0; nothing to do
                    }
                }
            };
            let tile_tex = producer.produce(ctx, coord);
            s.pool.copy_into(ctx, slot, &tile_tex);
            s.residency.touch(coord);
            s.versions.mark(coord);
            if let Some(idx) = flat_index(&s.layout, coord) {
                s.slots[idx] = slot;
            }
            produced += 1;
        }
        if produced > 0 {
            s.page_table.update(ctx, &s.slots);
        }
        produced
    }

    /// Test-introspection: is `t` currently resident (has a physical slot)?
    /// Works on the sparse path (rung 4) and the streaming path (rung 3).
    pub fn is_resident(&self, t: TileCoord) -> bool {
        if let Some(s) = self.sparse.as_ref() {
            return s.allocator.slot_of(t).is_some();
        }
        if let Some(s) = self.streaming.as_ref() {
            return s.allocator.slot_of(t).is_some();
        }
        false
    }
}

/// Flat index into the page table / slot mirror for `t`, or `None` if out of range.
fn flat_index(layout: &FlatLayout, t: TileCoord) -> Option<usize> {
    if t.lod >= layout.level_count() {
        return None;
    }
    let (cols, rows) = layout.dims(t.lod);
    if t.x >= cols || t.y >= rows {
        return None;
    }
    let idx = layout.flat_index(t.lod, t.x, t.y);
    if idx >= layout.total() {
        return None;
    }
    Some(idx as usize)
}

/// Flat index into the slot table for `t`, or `None` if out of range (e.g. an LOD
/// the source does not carry).
fn slot_index(layout: &LevelLayout, t: TileCoord) -> Option<usize> {
    if t.lod >= layout.level_count {
        return None;
    }
    let cols = layout.cols[t.lod as usize];
    if t.x >= cols {
        return None;
    }
    let idx = layout.offsets[t.lod as usize] + t.y * cols + t.x;
    if idx >= layout.total_tiles {
        return None;
    }
    Some(idx as usize)
}

#[cfg(test)]
mod single_update_tests {
    use super::*;
    use ferrolite_image::LinearRgbaF32;

    #[test]
    fn update_single_swaps_dims() {
        let Some(ctx) = GpuContext::headless() else {
            return; // CI headless: skip (spec §10 GPU-test convention)
        };
        let pipelines = DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
        let img = LinearRgbaF32::new(2, 2, vec![0.0; 2 * 2 * 4]).unwrap();
        let mut vt = VirtualTexture::single_texture(&ctx, &img, &pipelines);
        assert_eq!(vt.single_dims(), Some((2, 2)));

        // A 4x4 Rgba16Float texture with TEXTURE_BINDING (mirrors a pipeline output).
        let tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test-edit-out"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        vt.update_single_from_texture(std::sync::Arc::new(tex), (4, 4));
        assert_eq!(
            vt.single_dims(),
            Some((4, 4)),
            "dims reflect the swapped texture"
        );
    }
}

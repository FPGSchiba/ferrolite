mod common;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferrolite_gpu::GpuContext;
use ferrolite_image::{TileCoord, TILE_SIZE};
use ferrolite_jobs::JobSystem;
use ferrolite_vt::{PyramidTileSource, TileSource, ViewTransform, VirtualTexture};
use wgpu::util::DeviceExt;

const TOL: u8 = 4; // absorbs driver float differences

#[test]
fn rung1_fit_view_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping golden (expected in headless CI)");
        return;
    };
    let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    let img = common::split_image();
    let (w, h) = (64u32, 64u32);
    let view = ViewTransform::fit((img.width, img.height), (w as f32, h as f32));
    let pixels =
        VirtualTexture::render_to_image(&ctx, &img, &view, (w as f32, h as f32), w, h, &pipelines);

    let golden_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rung1_fit.png");
    if std::env::var("UPDATE_GOLDEN").is_ok() || !std::path::Path::new(golden_path).exists() {
        image::save_buffer(golden_path, &pixels, w, h, image::ColorType::Rgba8).unwrap();
        eprintln!("wrote golden {golden_path}");
        return;
    }
    let golden = image::open(golden_path).unwrap().to_rgba8();
    assert_eq!(golden.dimensions(), (w, h));
    assert!(
        common::max_abs_diff(&pixels, golden.as_raw()) <= TOL,
        "rendered output drifted from golden beyond tolerance"
    );
}

#[test]
fn rung2_tiled_matches_single_texture() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // A larger gradient so multiple tiles exist.
    let (iw, ih) = (300u32, 200u32);
    let mut px = Vec::new();
    for y in 0..ih {
        for x in 0..iw {
            px.extend_from_slice(&[x as f32 / iw as f32, y as f32 / ih as f32, 0.25, 1.0]);
        }
    }
    let img = ferrolite_image::LinearRgbaF32::new(iw, ih, px).unwrap();
    let (w, h) = (128u32, 128u32);
    let view = ViewTransform::fit((iw, ih), (w as f32, h as f32));

    let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    let single =
        VirtualTexture::render_to_image(&ctx, &img, &view, (w as f32, h as f32), w, h, &pipelines);
    let src = ferrolite_vt::PyramidTileSource::new(img);
    let tiled = VirtualTexture::render_tiled_to_image(
        &ctx,
        &src,
        &view,
        (w as f32, h as f32),
        w,
        h,
        &pipelines,
    );

    // At fit zoom the tiled path samples a coarse LOD; allow a generous tolerance
    // vs the single-texture reference (different filtering), but they must broadly agree.
    let diff = common::max_abs_diff(&single, &tiled);
    eprintln!("rung2 max_abs_diff = {diff}");
    assert!(diff <= 24, "tiled diverges from single-texture reference");
}

/// Rung 3: with a budget large enough to hold every needed tile, the streaming
/// path (after loads land) must broadly match the rung-2 resident render at the
/// same view. Exercises the live `request_view` + `drain_loaded` GPU path and the
/// coarse-LOD shader fallback (which returns the resolved tile once loaded).
#[test]
fn rung3_streaming_matches_resident_after_loads() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let (iw, ih) = (300u32, 200u32);
    let mut px = Vec::new();
    for y in 0..ih {
        for x in 0..iw {
            px.extend_from_slice(&[x as f32 / iw as f32, y as f32 / ih as f32, 0.25, 1.0]);
        }
    }
    let img = ferrolite_image::LinearRgbaF32::new(iw, ih, px).unwrap();
    let (w, h) = (128u32, 128u32);
    let view = ViewTransform::fit((iw, ih), (w as f32, h as f32));

    let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    // Reference: rung-2 fully-resident render.
    let src_ref = PyramidTileSource::new(img.clone());
    let resident = VirtualTexture::render_tiled_to_image(
        &ctx,
        &src_ref,
        &view,
        (w as f32, h as f32),
        w,
        h,
        &pipelines,
    );

    // Streaming: budget covers all tiles of all levels (generous).
    let src: Arc<dyn TileSource + Send + Sync> = Arc::new(PyramidTileSource::new(img));
    let total: u32 = (0..src.level_count())
        .map(|lod| {
            let (lw, lh) = src.level_size(lod);
            lw.div_ceil(256) * lh.div_ceil(256)
        })
        .sum();
    let jobs = Arc::new(JobSystem::new(2));
    let mut vt =
        VirtualTexture::streaming(&ctx, Arc::clone(&src), Arc::clone(&jobs), total, &pipelines);

    // Drive request_view + drain until tiles load (jobs run on worker threads).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        vt.request_view(&ctx, &view, (w as f32, h as f32));
        ctx.device.poll(wgpu::Maintain::Poll);
        let n = vt.drain_loaded(&ctx);
        if n == 0 && Instant::now() < deadline {
            // Give workers a moment to produce results, then re-drain.
            std::thread::sleep(Duration::from_millis(20));
            let m = vt.drain_loaded(&ctx);
            if m == 0 {
                // Nothing pending and nothing arrived: assume converged.
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    // Final reconcile so the slot table reflects all resident tiles.
    vt.request_view(&ctx, &view, (w as f32, h as f32));
    vt.drain_loaded(&ctx);

    // Render the streaming VT offscreen.
    let target = ctx.render_target(w, h, wgpu::TextureFormat::Rgba8Unorm);
    let tview = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vt-stream-offscreen"),
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
        vt.render_streaming(&ctx, &mut pass, &view, (w as f32, h as f32));
    }
    ctx.queue.submit([enc.finish()]);
    let streamed = ctx.read_rgba8(&target, w, h);

    let diff = common::max_abs_diff(&resident, &streamed);
    eprintln!("rung3 max_abs_diff vs resident = {diff}");
    // Once the needed tiles are resident the streaming render should closely
    // match the resident render (same pipeline, same tiles). Allow a small
    // tolerance for any not-yet-landed tiles served by the coarse-LOD fallback.
    assert!(
        diff <= 32,
        "streaming render diverges from resident reference (diff={diff})"
    );
}

/// Render the sparse VT offscreen one frame. The fragment shader marks the tiles
/// it wanted into the feedback buffer as a side effect of drawing.
fn render_sparse_frame(
    ctx: &GpuContext,
    vt: &VirtualTexture,
    view: &ViewTransform,
    w: u32,
    h: u32,
) {
    let target = ctx.render_target(w, h, wgpu::TextureFormat::Rgba8Unorm);
    let tview = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vt-sparse-offscreen"),
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
        vt.render_sparse(ctx, &mut pass, view, (w as f32, h as f32));
    }
    ctx.queue.submit([enc.finish()]);
    ctx.device.poll(wgpu::Maintain::Wait);
}

/// A trivial GPU producer for testing the VT produce path without any photo
/// dependency: uploads a solid-color `TILE_SIZE`² `Rgba16Float` tile whose color
/// encodes the coord, returning a `COPY_SRC` texture.
struct SolidProducer;
impl ferrolite_vt::TileProducer for SolidProducer {
    fn produce(
        &mut self,
        ctx: &ferrolite_gpu::GpuContext,
        coord: ferrolite_image::TileCoord,
    ) -> wgpu::Texture {
        use wgpu::util::DeviceExt;
        let n = (TILE_SIZE * TILE_SIZE) as usize;
        let r = half::f16::from_f32((coord.x as f32 + 1.0) / 16.0);
        let g = half::f16::from_f32((coord.y as f32 + 1.0) / 16.0);
        let b = half::f16::from_f32(0.5);
        let a = half::f16::from_f32(1.0);
        let mut texels = Vec::with_capacity(n * 4);
        for _ in 0..n {
            texels.extend_from_slice(&[r, g, b, a]);
        }
        ctx.device.create_texture_with_data(
            &ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("solid-producer-tile"),
                size: wgpu::Extent3d {
                    width: TILE_SIZE,
                    height: TILE_SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&texels),
        )
    }
}

#[test]
fn producer_fills_requested_tiles_and_version_bump_invalidates() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let (iw, ih) = (600u32, 500u32);
    let img = ferrolite_image::LinearRgbaF32::black(iw, ih);
    let src: Arc<dyn TileSource + Send + Sync> = Arc::new(PyramidTileSource::new(img));
    let total: u32 = (0..src.level_count())
        .map(|lod| {
            let (lw, lh) = src.level_size(lod);
            lw.div_ceil(TILE_SIZE) * lh.div_ceil(TILE_SIZE)
        })
        .sum();
    let jobs = Arc::new(JobSystem::new(1));
    let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    let mut vt =
        VirtualTexture::sparse(&ctx, Arc::clone(&src), Arc::clone(&jobs), total, &pipelines);
    let mut producer = SolidProducer;

    let needed = vec![
        TileCoord { lod: 0, x: 0, y: 0 },
        TileCoord { lod: 0, x: 1, y: 0 },
    ];
    let made = vt.produce_view(&ctx, &mut producer, &needed, 8);
    assert_eq!(made, 2, "both needed tiles produced");
    assert!(vt.is_resident(needed[0]) && vt.is_resident(needed[1]));

    // Re-producing the same view with no version change produces nothing more.
    assert_eq!(
        vt.produce_view(&ctx, &mut producer, &needed, 8),
        0,
        "already current"
    );

    // A version bump invalidates them; they must re-produce.
    vt.set_opstack_version(&ctx, 1);
    assert!(
        !vt.is_resident(needed[0]),
        "stale tile freed by version bump"
    );
    assert_eq!(
        vt.produce_view(&ctx, &mut producer, &needed, 8),
        2,
        "re-produced at new version"
    );
}

/// Rung 4 (the full engine-style sparse VT): the display shader marks the tiles
/// it actually sampled into a GPU feedback buffer; the CPU reads that back one
/// frame later and loads the missing tiles, updating the page table. After a few
/// render→feedback→process cycles the tile covering the viewport center — which
/// the shader demonstrably wanted — must become resident.
#[test]
fn rung4_feedback_makes_center_tile_resident() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };

    // A multi-tile gradient (>1 tile per side at LOD 0).
    let (iw, ih) = (600u32, 500u32);
    let mut px = Vec::new();
    for y in 0..ih {
        for x in 0..iw {
            px.extend_from_slice(&[x as f32 / iw as f32, y as f32 / ih as f32, 0.25, 1.0]);
        }
    }
    let img = ferrolite_image::LinearRgbaF32::new(iw, ih, px).unwrap();

    let (w, h) = (256u32, 256u32);
    // Zoom 1.0 so `pick_lod` resolves to LOD 0: the center pixel then maps to a
    // deterministic LOD-0 tile (the image-space center divided by TILE_SIZE).
    let view = ViewTransform {
        zoom: 1.0,
        pan: (0.0, 0.0),
    };
    // Center pixel -> image px = image center (pan 0). Tile that covers it:
    let center_x = (iw / 2) / TILE_SIZE;
    let center_y = (ih / 2) / TILE_SIZE;
    let center = TileCoord {
        lod: 0,
        x: center_x,
        y: center_y,
    };

    let src: Arc<dyn TileSource + Send + Sync> = Arc::new(PyramidTileSource::new(img));
    let total: u32 = (0..src.level_count())
        .map(|lod| {
            let (lw, lh) = src.level_size(lod);
            lw.div_ceil(TILE_SIZE) * lh.div_ceil(TILE_SIZE)
        })
        .sum();
    let jobs = Arc::new(JobSystem::new(2));
    let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    let mut vt =
        VirtualTexture::sparse(&ctx, Arc::clone(&src), Arc::clone(&jobs), total, &pipelines);

    // Feedback is one frame latent: render (marks feedback) -> process (reads it
    // back, submits loads, updates the page table) -> repeat until the worker jobs
    // land and the center tile resolves. Bounded by a wall-clock deadline.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        render_sparse_frame(&ctx, &vt, &view, w, h);
        vt.request_view_feedback(&ctx);
        ctx.device.poll(wgpu::Maintain::Poll);
        vt.drain_loaded_sparse(&ctx);
        if vt.is_resident(center) {
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    assert!(
        vt.is_resident(center),
        "feedback round-trip should make the center tile {center:?} resident"
    );
}

/// A `TileProducer` that writes a solid color keyed on the tile's LOD (not its
/// x/y), so adjacent LODs are visibly and numerically distinct. Used to force
/// specific LODs resident directly (bypassing the feedback round-trip, which
/// only ever marks the picked LOD, never the coarser blend partner) with colors
/// chosen so the trilinear blend between them is unambiguous in the golden.
struct LevelTintProducer;
impl ferrolite_vt::TileProducer for LevelTintProducer {
    fn produce(
        &mut self,
        ctx: &ferrolite_gpu::GpuContext,
        coord: ferrolite_image::TileCoord,
    ) -> wgpu::Texture {
        use wgpu::util::DeviceExt;
        // lod 2 -> pure red, lod 3 -> pure blue; any other lod (unused here) -> white.
        let (r, g, b) = match coord.lod {
            2 => (1.0, 0.0, 0.0),
            3 => (0.0, 0.0, 1.0),
            _ => (1.0, 1.0, 1.0),
        };
        let n = (TILE_SIZE * TILE_SIZE) as usize;
        let texel = [
            half::f16::from_f32(r),
            half::f16::from_f32(g),
            half::f16::from_f32(b),
            half::f16::from_f32(1.0),
        ];
        let mut texels = Vec::with_capacity(n * 4);
        for _ in 0..n {
            texels.extend_from_slice(&texel);
        }
        ctx.device.create_texture_with_data(
            &ctx.queue,
            &wgpu::TextureDescriptor {
                label: Some("level-tint-producer-tile"),
                size: wgpu::Extent3d {
                    width: TILE_SIZE,
                    height: TILE_SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&texels),
        )
    }
}

/// Trilinear LOD blend (Task 4): `fs_sparse` samples the picked LOD and the
/// next-coarser resident LOD and blends by `fract(log2 d)`, instead of hard
/// LOD selection. Renders at a fractional-LOD zoom (`lf ≈ 2.5`, so `lo=2`,
/// `hi=3`, blend factor `fract(2.5)=0.5`) with lod 2 forced pure red and lod 3
/// forced pure blue (via `LevelTintProducer`, bypassing the real pyramid
/// content so the two source levels are unambiguously distinct) — a correct
/// 50/50 blend renders magenta (`(0.5, 0, 0.5)`); hard LOD selection (the
/// pre-Task-4 behavior) would render solid red or solid blue instead. Compares
/// against a committed reference PNG within the existing tolerance.
///
/// Deliberately does NOT call `request_view_feedback` between the producer
/// pre-fill and the golden render: that reconcile evicts anything not in the
/// shader's feedback set, and `fs_sparse` only ever marks the *picked* (lo)
/// LOD — never the coarser blend partner (`hi`) — so reconciling here would
/// evict `hi` and collapse the test back to a single-level render. The
/// feedback-mark invariant itself (tiles the shader wants get marked and,
/// over repeated frames, loaded) is covered by `rung4_feedback_makes_center_tile_resident`.
#[test]
fn sparse_trilinear_blends_adjacent_levels() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping golden");
        return;
    };

    // 1200x900 -> pyramid levels: 1200x900, 600x450, 300x225, 150x112 (4 levels,
    // lod 3 is the last since both dims are <=256). lo=2 (300x225, still >256 on
    // width so 2x1 tiles) and hi=3 (150x112, single tile) are both valid indices
    // (< level_count), so the blend does not degenerate to a single level. The
    // base image content is irrelevant here (the producer overrides lod 2/3
    // tiles with solid tints below); a flat image keeps level dims deterministic.
    let (iw, ih) = (1200u32, 900u32);
    let img = ferrolite_image::LinearRgbaF32::black(iw, ih);
    let src = PyramidTileSource::new(img);
    assert_eq!(
        src.level_count(),
        4,
        "expected a 4-level pyramid at 1200x900"
    );

    let (w, h) = (128u32, 128u32);
    // zoom = 2^-2.5 so that d = 1/zoom = 2^2.5 and lf = log2(d) = 2.5 uniformly
    // across the frame (the screen->image mapping is affine, no perspective).
    let view = ViewTransform {
        zoom: 2f32.powf(-2.5),
        pan: (0.0, 0.0),
    };

    let total: u32 = (0..src.level_count())
        .map(|lod| {
            let (lw, lh) = src.level_size(lod);
            lw.div_ceil(TILE_SIZE) * lh.div_ceil(TILE_SIZE)
        })
        .sum();
    let jobs = Arc::new(JobSystem::new(1));
    let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    let src_arc: Arc<dyn TileSource + Send + Sync> = Arc::new(src);

    let mut vt = VirtualTexture::sparse(
        &ctx,
        Arc::clone(&src_arc),
        Arc::clone(&jobs),
        total,
        &pipelines,
    );
    let mut producer = LevelTintProducer;

    // Force BOTH the lo (lod 2) and hi (lod 3) tiles under the viewport center
    // resident directly via the producer path — the feedback loop alone only
    // ever marks the picked (lo) LOD, never the coarser blend partner, so it
    // cannot converge both levels on its own.
    let lo_tiles: Vec<TileCoord> = {
        let (cols, rows) = {
            let (lw, lh) = src_arc.level_size(2);
            (lw.div_ceil(TILE_SIZE), lh.div_ceil(TILE_SIZE))
        };
        (0..rows)
            .flat_map(|y| (0..cols).map(move |x| TileCoord { lod: 2, x, y }))
            .collect()
    };
    let hi_tiles: Vec<TileCoord> = {
        let (cols, rows) = {
            let (lw, lh) = src_arc.level_size(3);
            (lw.div_ceil(TILE_SIZE), lh.div_ceil(TILE_SIZE))
        };
        (0..rows)
            .flat_map(|y| (0..cols).map(move |x| TileCoord { lod: 3, x, y }))
            .collect()
    };
    let mut needed = lo_tiles.clone();
    needed.extend(hi_tiles.clone());
    let made = vt.produce_view(&ctx, &mut producer, &needed, needed.len());
    assert_eq!(made, needed.len(), "all lo/hi tiles produced");
    for t in &needed {
        assert!(vt.is_resident(*t), "tile {t:?} should be resident");
    }

    // Render the golden frame the same way `render_sparse_frame` does (marking
    // feedback as a side effect, which we deliberately do not reconcile — see
    // the doc comment above), then read back the pixels for comparison.
    let target = ctx.render_target(w, h, wgpu::TextureFormat::Rgba8Unorm);
    let tview = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vt-trilinear-golden"),
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
        vt.render_sparse(&ctx, &mut pass, &view, (w as f32, h as f32));
    }
    ctx.queue.submit([enc.finish()]);
    let pixels = ctx.read_rgba8(&target, w, h);

    let golden_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/refs/trilinear_sparse.png"
    );
    if std::env::var("UPDATE_GOLDEN").is_ok() || !std::path::Path::new(golden_path).exists() {
        std::fs::create_dir_all(std::path::Path::new(golden_path).parent().unwrap()).unwrap();
        image::save_buffer(golden_path, &pixels, w, h, image::ColorType::Rgba8).unwrap();
        eprintln!("wrote golden {golden_path}");
        return;
    }
    let golden = image::open(golden_path).unwrap().to_rgba8();
    assert_eq!(golden.dimensions(), (w, h));
    assert!(
        common::max_abs_diff(&pixels, golden.as_raw()) <= TOL,
        "trilinear-blended render drifted from golden beyond tolerance"
    );
}

/// Golden proving the off-screen "swapchain" indirection is a no-op when the
/// sparse pool is converged (Task 8, spec 4.5 §4.2): (A) `draw_sparse` directly
/// to an offscreen target, vs (B) `compose_sparse_into` a `PresentBuffers.back`,
/// `swap()`, then blit `front` (alpha 1.0, via the `DisplayVariant::Blit`
/// pipeline) into a second offscreen target of the same format. If A and B
/// match within tolerance, presenting through the buffered blit changes nothing
/// visually versus today's direct draw — it only defers *when* the pixels reach
/// the screen.
#[test]
fn blit_front_matches_direct_sparse_when_converged() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping golden (expected in headless CI)");
        return;
    };

    // Small multi-tile gradient so the sparse pool has more than one level/tile
    // to converge (mirrors the rung-4 / trilinear golden setup above).
    let (iw, ih) = (600u32, 500u32);
    let mut px = Vec::new();
    for y in 0..ih {
        for x in 0..iw {
            px.extend_from_slice(&[x as f32 / iw as f32, y as f32 / ih as f32, 0.25, 1.0]);
        }
    }
    let img = ferrolite_image::LinearRgbaF32::new(iw, ih, px).unwrap();
    let src = PyramidTileSource::new(img);
    let level_count = src.level_count();

    let target_format = wgpu::TextureFormat::Rgba8Unorm;
    let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, target_format);
    let src_arc: Arc<dyn TileSource + Send + Sync> = Arc::new(src);

    // Every tile of every level, so the pool is converged regardless of view.
    let total: u32 = (0..level_count)
        .map(|lod| {
            let (lw, lh) = src_arc.level_size(lod);
            lw.div_ceil(TILE_SIZE) * lh.div_ceil(TILE_SIZE)
        })
        .sum();
    let all_tiles: Vec<TileCoord> = (0..level_count)
        .flat_map(|lod| {
            let (lw, lh) = src_arc.level_size(lod);
            let (cols, rows) = (lw.div_ceil(TILE_SIZE), lh.div_ceil(TILE_SIZE));
            (0..rows).flat_map(move |y| (0..cols).map(move |x| TileCoord { lod, x, y }))
        })
        .collect();

    let jobs = Arc::new(JobSystem::new(1));
    let mut vt = VirtualTexture::sparse(
        &ctx,
        Arc::clone(&src_arc),
        Arc::clone(&jobs),
        total,
        &pipelines,
    );
    let mut producer = SolidProducer;
    let made = vt.produce_view(&ctx, &mut producer, &all_tiles, all_tiles.len());
    assert_eq!(made, all_tiles.len(), "every tile of every level produced");
    for t in &all_tiles {
        assert!(
            vt.is_resident(*t),
            "tile {t:?} should be resident (converged pool)"
        );
    }

    let (w, h) = (128u32, 128u32);
    let view = ViewTransform::fit((iw, ih), (w as f32, h as f32));

    // --- Path A: draw_sparse directly to an offscreen target. ---
    let target_a = ctx.render_target(w, h, target_format);
    let view_a = target_a.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc_a = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc_a.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vt-direct-sparse"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view_a,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.02,
                        g: 0.02,
                        b: 0.02,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        vt.render_sparse(&ctx, &mut pass, &view, (w as f32, h as f32));
    }
    ctx.queue.submit([enc_a.finish()]);
    let image_a = ctx.read_rgba8(&target_a, w, h);

    // --- Path B: compose_sparse_into PresentBuffers.back, swap, blit front. ---
    let mut present = ferrolite_vt::PresentBuffers::new(&ctx, (w, h), target_format);
    let mut enc_b = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    vt.compose_sparse_into(
        &ctx,
        &mut enc_b,
        present.back_view(),
        &view,
        (w as f32, h as f32),
    );
    ctx.queue.submit([enc_b.finish()]);
    present.swap();

    // Blit `front` (alpha 1.0) into a second offscreen target via the Blit
    // pipeline. Clear to opaque first: ALPHA_BLENDING with alpha=1.0 fully
    // overwrites, but clearing keeps the pass well-defined regardless.
    let target_b = ctx.render_target(w, h, target_format);
    let view_b = target_b.create_view(&wgpu::TextureViewDescriptor::default());

    let alpha_buf = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("blit-params"),
            contents: bytemuck::bytes_of(&BlitParams {
                alpha: 1.0,
                _pad0: [0.0; 3],
                _pad1: [0.0; 4],
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let blit_bind = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("blit-bind"),
        layout: pipelines.blit_layout(),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(present.front_view()),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(pipelines.sampler()),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: alpha_buf.as_entire_binding(),
            },
        ],
    });

    let mut enc_blit = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc_blit.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vt-blit-front"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view_b,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(pipelines.pipeline(ferrolite_vt::DisplayVariant::Blit));
        pass.set_bind_group(0, &blit_bind, &[]);
        pass.draw(0..3, 0..1);
    }
    ctx.queue.submit([enc_blit.finish()]);
    let image_b = ctx.read_rgba8(&target_b, w, h);

    let diff = common::max_abs_diff(&image_a, &image_b);
    eprintln!("blit_front_matches_direct_sparse max_abs_diff = {diff}");
    assert!(
        diff <= TOL,
        "blit(front) diverged from the direct sparse draw beyond tolerance (diff={diff})"
    );
}

/// Uniform matching `present.wgsl`'s `struct BlitParams { alpha: f32, _pad:
/// vec3<f32> }` layout, used to drive the blit pipeline directly from a golden
/// test (no app-side wrapper exists yet). WGSL aligns `vec3<f32>` to 16 bytes,
/// so the struct is 32 bytes total: `alpha` at offset 0 (padded to 16), then
/// `_pad` at offset 16..28 (padded to 32).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlitParams {
    alpha: f32,
    _pad0: [f32; 3],
    _pad1: [f32; 4],
}

/// The working→display matrix uniform is applied before the sRGB OETF. A
/// channel-swap matrix must visibly change the rendered output, proving the
/// tail is wired end-to-end (bind group + layout + shader).
#[test]
fn display_tail_applies_matrix() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    // Channel-swap matrix (row-major): display.r = g, .g = b, .b = r.
    pipelines.set_display_matrix(
        &ctx.queue,
        [[0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]],
    );
    let img = common::split_image();
    let (w, h) = (64u32, 64u32);
    let view = ViewTransform::fit((img.width, img.height), (w as f32, h as f32));
    let pixels =
        VirtualTexture::render_to_image(&ctx, &img, &view, (w as f32, h as f32), w, h, &pipelines);

    let golden_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/display_tail_swap.png"
    );
    if std::env::var("UPDATE_GOLDEN").is_ok() || !std::path::Path::new(golden_path).exists() {
        image::save_buffer(golden_path, &pixels, w, h, image::ColorType::Rgba8).unwrap();
        eprintln!("wrote golden {golden_path}");
        return;
    }
    let golden = image::open(golden_path).unwrap().to_rgba8();
    assert!(common::max_abs_diff(&pixels, golden.as_raw()) <= TOL);
}

/// The 3D-LUT display path (`use_lut == 1`): an "identity-shaper" LUT whose node
/// `(r,g,b)` stores exactly its own sample coordinate `(r/(n-1), g/(n-1), b/(n-1))`.
/// Since the stored function is the identity, trilinear interpolation reproduces
/// the sample coordinate exactly everywhere (not just at grid points), so
/// `tail(lin) = LUT[shaper_encode(lin)] = shaper_encode(lin)`. Rendering a known
/// image through this LUT and comparing against `shaper_encode(lin)` computed on
/// the CPU proves the LUT path samples and applies correctly end-to-end on the GPU.
#[test]
fn lut_path_samples_identity_shaper_lut() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping golden (expected in headless CI)");
        return;
    };

    let n = ferrolite_vt::LUT_SIZE as usize;
    let denom = (n - 1) as f32;
    let mut rgba16f = Vec::with_capacity(n * n * n * 4);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                // Output = the sample coordinate itself (idx), so tail(lin) = shaper_encode(lin).
                rgba16f.push(half::f16::from_f32(r as f32 / denom).to_bits());
                rgba16f.push(half::f16::from_f32(g as f32 / denom).to_bits());
                rgba16f.push(half::f16::from_f32(b as f32 / denom).to_bits());
                rgba16f.push(half::f16::from_f32(1.0).to_bits());
            }
        }
    }

    let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    let shaper_gamma = 2.2f32;
    pipelines.set_display_lut(&ctx.queue, ferrolite_vt::LUT_SIZE, &rgba16f, shaper_gamma);

    // A 4x4 image with a spread of known linear values per pixel, including 0.0,
    // mid-range values, and >1.0 (to exercise the LUT path's clamp in shaper_encode).
    let (iw, ih) = (4u32, 4u32);
    let known: [[f32; 3]; 16] = [
        [0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0],
        [0.5, 0.5, 0.5],
        [0.25, 0.75, 0.1],
        [0.9, 0.2, 0.6],
        [0.05, 0.95, 0.5],
        [1.5, 0.5, 0.5],
        [0.5, 1.2, 0.5],
        [0.5, 0.5, 2.0],
        [0.33, 0.66, 0.99],
        [0.01, 0.02, 0.03],
        [0.7, 0.7, 0.7],
        [0.2, 0.4, 0.8],
        [0.8, 0.4, 0.2],
        [0.15, 0.85, 0.45],
        [0.6, 0.3, 0.9],
    ];
    let mut px = Vec::with_capacity((iw * ih * 4) as usize);
    for [r, g, b] in known {
        px.extend_from_slice(&[r, g, b, 1.0]);
    }
    let img = ferrolite_image::LinearRgbaF32::new(iw, ih, px).unwrap();

    let (w, h) = (4u32, 4u32);
    let view = ViewTransform::fit((iw, ih), (w as f32, h as f32));
    let pixels =
        VirtualTexture::render_to_image(&ctx, &img, &view, (w as f32, h as f32), w, h, &pipelines);

    // Trilinear interpolation of a bilinear/identity-valued LUT is exact, but f16
    // storage + Rgba8Unorm output quantization still introduce a small rounding
    // error; a few /255 absorbs that without masking a real sampling bug.
    const LUT_TOL: f32 = 3.0 / 255.0;
    for (i, [r, g, b]) in known.iter().enumerate() {
        let expected = [
            r.clamp(0.0, 1.0).powf(1.0 / shaper_gamma),
            g.clamp(0.0, 1.0).powf(1.0 / shaper_gamma),
            b.clamp(0.0, 1.0).powf(1.0 / shaper_gamma),
        ];
        let base = i * 4;
        let actual = [
            pixels[base] as f32 / 255.0,
            pixels[base + 1] as f32 / 255.0,
            pixels[base + 2] as f32 / 255.0,
        ];
        for c in 0..3 {
            assert!(
                (actual[c] - expected[c]).abs() <= LUT_TOL,
                "pixel {i} channel {c}: expected {:.4}, got {:.4} (lin={:?})",
                expected[c],
                actual[c],
                known[i]
            );
        }
    }
}

/// Task 9: `is_converged` is the immediate CPU-rect convergence predicate — it
/// must report `false` for a freshly-built sparse VT (nothing produced yet) and
/// `true` once every tile `needed_tiles` reports for the fit view has been
/// produced via the stub producer. Mirrors the `producer_fills_requested_tiles`
/// setup above (no photo/pyramid dependency beyond the test-only pyramid source).
#[test]
fn fresh_sparse_is_not_converged_then_converges_after_producing() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };

    let (iw, ih) = (600u32, 500u32);
    let img = ferrolite_image::LinearRgbaF32::black(iw, ih);
    let src: Arc<dyn TileSource + Send + Sync> = Arc::new(PyramidTileSource::new(img));
    let total: u32 = (0..src.level_count())
        .map(|lod| {
            let (lw, lh) = src.level_size(lod);
            lw.div_ceil(TILE_SIZE) * lh.div_ceil(TILE_SIZE)
        })
        .sum();
    let jobs = Arc::new(JobSystem::new(1));
    let pipelines = ferrolite_vt::DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    let mut vt =
        VirtualTexture::sparse(&ctx, Arc::clone(&src), Arc::clone(&jobs), total, &pipelines);
    let mut producer = SolidProducer;

    let (w, h) = (128.0f32, 128.0f32);
    let view = ViewTransform::fit((iw, ih), (w, h));

    // Nothing produced yet: not converged.
    assert!(
        !vt.is_converged(&view, (w, h)),
        "a freshly built sparse VT must not be converged before anything is produced"
    );

    // Produce every tile the CPU rect estimate says this view needs, using the
    // same level_count the VT itself was built with (`sparse` caps at MAX_LEVELS,
    // which a 600x500 pyramid never reaches, so plain `level_count()` matches).
    let level_count = src.level_count();
    let needed = ferrolite_vt::needed_tiles((iw, ih), &view, (w, h), level_count);
    let made = vt.produce_view(&ctx, &mut producer, &needed, needed.len());
    assert_eq!(made, needed.len(), "every rect-needed tile produced");

    assert!(
        vt.is_converged(&view, (w, h)),
        "after producing every needed tile, the view must be converged"
    );
}

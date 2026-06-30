mod common;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferrolite_gpu::GpuContext;
use ferrolite_image::{TileCoord, TILE_SIZE};
use ferrolite_jobs::JobSystem;
use ferrolite_vt::{PyramidTileSource, TileSource, ViewTransform, VirtualTexture};

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

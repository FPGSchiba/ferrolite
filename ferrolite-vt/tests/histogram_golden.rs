//! GPU histogram compute vs a CPU reference over the same image. Auto-skips when
//! no GPU adapter is present (headless CI).

use ferrolite_gpu::GpuContext;
use ferrolite_image::LinearRgbaF32;
use ferrolite_vt::{bin_index, DisplayPipelines, HistogramPipeline, VirtualTexture, HIST_LEN};
use half::f16;

fn srgb_oetf(l: f32) -> f32 {
    if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// CPU reference: round through f16 (the texture is Rgba16Float), apply the sRGB
/// OETF (identity matrix), bin R,G,B,luma. Must mirror histogram.wgsl exactly.
fn cpu_histogram(img: &LinearRgbaF32) -> Vec<u32> {
    let mut bins = vec![0u32; HIST_LEN];
    let n = (img.width * img.height) as usize;
    for i in 0..n {
        let r = f16::from_f32(img.pixels[i * 4]).to_f32();
        let g = f16::from_f32(img.pixels[i * 4 + 1]).to_f32();
        let b = f16::from_f32(img.pixels[i * 4 + 2]).to_f32();
        let dr = srgb_oetf(r.clamp(0.0, 1.0));
        let dg = srgb_oetf(g.clamp(0.0, 1.0));
        let db = srgb_oetf(b.clamp(0.0, 1.0));
        let luma = 0.2126 * dr + 0.7152 * dg + 0.0722 * db;
        bins[bin_index(dr) as usize] += 1;
        bins[256 + bin_index(dg) as usize] += 1;
        bins[512 + bin_index(db) as usize] += 1;
        bins[768 + bin_index(luma) as usize] += 1;
    }
    bins
}

/// Values chosen to sit well away from bin midpoints so f16 rounding + the OETF
/// land GPU and CPU in the same bin (exact equality, no tolerance needed).
fn probe_image() -> LinearRgbaF32 {
    let px = vec![
        0.20, 0.40, 0.60, 1.0, //
        0.50, 0.10, 0.30, 1.0, //
        0.05, 0.25, 0.45, 1.0, //
        0.80, 0.55, 0.15, 1.0, //
    ];
    LinearRgbaF32::new(2, 2, px).unwrap()
}

#[test]
fn histogram_compute_matches_cpu_reference() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let img = probe_image();
    // Upload via the real rung-1 path (f16), then bin the resulting texture.
    let pipelines = DisplayPipelines::new(&ctx, wgpu::TextureFormat::Rgba8Unorm);
    let vt = VirtualTexture::single_texture(&ctx, &img, &pipelines);
    let tex = vt.single_texture_arc().expect("single texture");
    let hist = HistogramPipeline::new(&ctx);
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    hist.dispatch(&ctx, &tex, (img.width, img.height), identity);

    let (tx, rx) = std::sync::mpsc::channel();
    hist.read_async(move |maybe| {
        let _ = tx.send(maybe);
    });
    ctx.device.poll(wgpu::Maintain::Wait); // block in-test only; app uses Poll
    let gpu_bins = rx
        .recv()
        .expect("readback delivered")
        .expect("map succeeded");

    let cpu_bins = cpu_histogram(&img);
    assert_eq!(gpu_bins.len(), HIST_LEN);
    // Conservation: each channel counts every pixel exactly once.
    for ch in 0..4 {
        let sum: u32 = gpu_bins[ch * 256..ch * 256 + 256].iter().sum();
        assert_eq!(sum, 4, "channel {ch} must total the pixel count");
    }
    assert_eq!(
        gpu_bins, cpu_bins,
        "GPU histogram must match the CPU reference"
    );
}

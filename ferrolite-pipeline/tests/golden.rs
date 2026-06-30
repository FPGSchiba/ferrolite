mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_pipeline::{blit_to_rgba8, upload_source};

const W: u32 = 64;
const H: u32 = 48;

#[test]
fn source_upload_blit_matches_golden() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping golden (expected in headless CI)");
        return;
    };
    let src = common::gradient(W, H);
    let img = upload_source(&ctx, &src);
    let pixels = blit_to_rgba8(&ctx, &img);
    common::assert_golden(&pixels, W, H, "source.png");
}

mod common;

use ferrolite_gpu::GpuContext;
use ferrolite_mask::{MaskBuffer, MASK_FORMAT};

#[test]
fn alloc_produces_r32float_buffer_of_requested_size() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (expected in headless CI)");
        return;
    };
    let buf = MaskBuffer::alloc(&ctx, 16, 12);
    assert_eq!(buf.width, 16);
    assert_eq!(buf.height, 12);
    assert_eq!(buf.texture.format(), MASK_FORMAT);
}

#[test]
fn cleared_buffer_reads_back_zero() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let buf = MaskBuffer::alloc(&ctx, 8, 8);
    // Clear the R32Float texture to zero via a copy from a zeroed buffer.
    let view = buf
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let enc = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    // A render pass clear requires RENDER_ATTACHMENT usage the mask buffer lacks;
    // instead upload zeros through the queue.
    let _ = view; // no-op: keep the view creation smoke-tested
    drop(enc.finish());
    ctx.queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &buf.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&vec![0.0f32; 8 * 8]),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(8 * 4),
            rows_per_image: Some(8),
        },
        wgpu::Extent3d {
            width: 8,
            height: 8,
            depth_or_array_layers: 1,
        },
    );
    let values = common::read_r32f(&ctx, &buf);
    assert_eq!(values.len(), 64);
    assert!(values.iter().all(|&v| v == 0.0));
}

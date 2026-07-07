mod common;

use ferrolite_gpu::{GpuContext, Graph, Node};
use ferrolite_mask::{
    composite_scalar, CompositeMode, CompositeNode, CompositePass, LinearGradientPass, MaskBuffer,
    RadialGradientPass, Vec2,
};
use std::rc::Rc;
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 48;

/// A source node returning a pre-built MaskBuffer (graph root for the test).
struct BufSource(MaskBuffer);
impl Node<MaskBuffer> for BufSource {
    fn evaluate(&self, _inputs: &[&MaskBuffer]) -> MaskBuffer {
        self.0.clone()
    }
}

fn setup() -> Option<(
    Arc<GpuContext>,
    LinearGradientPass,
    RadialGradientPass,
    Rc<CompositePass>,
)> {
    let ctx = Arc::new(GpuContext::headless()?);
    let lin = LinearGradientPass::new(ctx.clone());
    let rad = RadialGradientPass::new(ctx.clone());
    let comp = Rc::new(CompositePass::new(ctx.clone()));
    Some((ctx, lin, rad, comp))
}

#[test]
fn add_composite_matches_golden() {
    let Some((ctx, lin, rad, comp)) = setup() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let a = lin.run(
        Vec2::new(0.1, 0.5),
        Vec2::new(0.5, 0.5),
        [1.0, 1.0],
        [0.0, 0.0],
        W,
        H,
    );
    let b = rad.run(
        Vec2::new(0.7, 0.5),
        Vec2::new(0.2, 0.3),
        0.0,
        0.2,
        false,
        [1.0, 1.0],
        [0.0, 0.0],
        W,
        H,
    );
    let out = comp.composite(&[(a, CompositeMode::Add), (b, CompositeMode::Add)], false);
    let values = common::read_r32f(&ctx, &out);
    common::assert_mask_golden(&values, W, H, "composite_add.png");
}

#[test]
fn subtract_composite_matches_golden() {
    let Some((ctx, lin, rad, comp)) = setup() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let a = lin.run(
        Vec2::new(0.1, 0.5),
        Vec2::new(0.9, 0.5),
        [1.0, 1.0],
        [0.0, 0.0],
        W,
        H,
    );
    let b = rad.run(
        Vec2::new(0.5, 0.5),
        Vec2::new(0.25, 0.35),
        0.0,
        0.2,
        false,
        [1.0, 1.0],
        [0.0, 0.0],
        W,
        H,
    );
    let out = comp.composite(
        &[(a, CompositeMode::Add), (b, CompositeMode::Subtract)],
        false,
    );
    let values = common::read_r32f(&ctx, &out);
    common::assert_mask_golden(&values, W, H, "composite_subtract.png");
}

#[test]
fn intersect_composite_matches_golden() {
    let Some((ctx, lin, rad, comp)) = setup() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let a = lin.run(
        Vec2::new(0.1, 0.5),
        Vec2::new(0.9, 0.5),
        [1.0, 1.0],
        [0.0, 0.0],
        W,
        H,
    );
    let b = rad.run(
        Vec2::new(0.5, 0.5),
        Vec2::new(0.4, 0.4),
        0.0,
        0.2,
        false,
        [1.0, 1.0],
        [0.0, 0.0],
        W,
        H,
    );
    let out = comp.composite(
        &[(a, CompositeMode::Add), (b, CompositeMode::Intersect)],
        false,
    );
    let values = common::read_r32f(&ctx, &out);
    common::assert_mask_golden(&values, W, H, "composite_intersect.png");
}

#[test]
fn invert_composite_matches_golden() {
    let Some((ctx, lin, _rad, comp)) = setup() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let a = lin.run(
        Vec2::new(0.1, 0.5),
        Vec2::new(0.9, 0.5),
        [1.0, 1.0],
        [0.0, 0.0],
        W,
        H,
    );
    let out = comp.composite(&[(a, CompositeMode::Add)], true);
    let values = common::read_r32f(&ctx, &out);
    common::assert_mask_golden(&values, W, H, "composite_invert.png");
}

/// GPU fold parity: a uniform-value fold matches the CPU `composite_scalar`.
#[test]
fn gpu_fold_matches_cpu_reference() {
    let Some((ctx, _lin, _rad, comp)) = setup() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // Constant masks via write_texture (a=0.8 everywhere, b=0.5 everywhere).
    let mk = |v: f32| -> MaskBuffer {
        let buf = MaskBuffer::alloc(&ctx, 8, 8);
        ctx.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &buf.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&vec![v; 64]),
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
        buf
    };
    let out = comp.composite(
        &[
            (mk(0.8), CompositeMode::Add),
            (mk(0.5), CompositeMode::Subtract),
        ],
        false,
    );
    let values = common::read_r32f(&ctx, &out);
    let expect = composite_scalar(
        &[(0.8, CompositeMode::Add), (0.5, CompositeMode::Subtract)],
        false,
    );
    assert!(
        (values[0] - expect).abs() < 1e-4,
        "GPU fold {} != CPU {}",
        values[0],
        expect
    );
    assert!((expect - 0.4).abs() < 1e-4);
}

/// Contract 4: the compositor runs as a generic node in an UNMODIFIED
/// `Graph<MaskBuffer>` and produces the same result as the direct call.
#[test]
fn composite_node_runs_in_generic_graph() {
    let Some((ctx, lin, rad, comp)) = setup() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    let a = lin.run(
        Vec2::new(0.1, 0.5),
        Vec2::new(0.9, 0.5),
        [1.0, 1.0],
        [0.0, 0.0],
        W,
        H,
    );
    let b = rad.run(
        Vec2::new(0.5, 0.5),
        Vec2::new(0.3, 0.3),
        0.0,
        0.2,
        false,
        [1.0, 1.0],
        [0.0, 0.0],
        W,
        H,
    );

    let direct = comp.composite(
        &[
            (a.clone(), CompositeMode::Add),
            (b.clone(), CompositeMode::Subtract),
        ],
        false,
    );
    let direct_values = common::read_r32f(&ctx, &direct);

    let mut g: Graph<MaskBuffer> = Graph::new();
    let na = g.add_node(Box::new(BufSource(a)), vec![]);
    let nb = g.add_node(Box::new(BufSource(b)), vec![]);
    let node = CompositeNode {
        pass: comp.clone(),
        modes: vec![CompositeMode::Add, CompositeMode::Subtract],
        invert: false,
    };
    let nc = g.add_node(Box::new(node), vec![na, nb]);
    let graph_out = g.evaluate(nc).clone();
    let graph_values = common::read_r32f(&ctx, &graph_out);

    let diff = direct_values
        .iter()
        .zip(graph_values.iter())
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(
        diff < 1e-5,
        "graph node result diverged from direct composite (diff {diff})"
    );
}

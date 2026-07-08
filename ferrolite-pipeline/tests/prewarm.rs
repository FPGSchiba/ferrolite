use ferrolite_gpu::GpuContext;

#[test]
fn prewarm_pipelines_runs_without_panicking() {
    let Some(ctx) = GpuContext::headless() else {
        eprintln!("no GPU adapter; skipping (headless CI)");
        return;
    };
    // Must not panic; warms driver pipeline compilation for the edit passes.
    ferrolite_pipeline::prewarm_pipelines(std::sync::Arc::new(ctx));
}

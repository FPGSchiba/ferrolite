//! `PipelineImage` — a GPU-resident `Rgba16Float` image (display-linear). Cheap
//! to clone (Arc handle); it is the node output type `O` of the edit DAG.

use std::sync::Arc;

/// The internal pipeline texture format (display-linear, f16).
pub const PIPELINE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[derive(Clone)]
pub struct PipelineImage {
    pub texture: Arc<wgpu::Texture>,
    pub width: u32,
    pub height: u32,
}

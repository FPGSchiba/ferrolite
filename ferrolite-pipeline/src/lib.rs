//! ferrolite-pipeline — the photo edit DAG (see crate docs in the final lib.rs).
mod image;
mod nodes;
mod op;
mod pipeline;
mod serialize;
mod uniforms;

pub use image::PipelineImage;
pub use nodes::upload_source;
pub use op::{Contrast, Exposure, Op, OpKind, OpStack, WhiteBalance, STACK_VERSION};
pub use pipeline::{blit_to_rgba8, EditPipeline};
pub use serialize::{deserialize, serialize};
pub use uniforms::{
    contrast_gain_pivot, contrast_uniform, exposure_gain, exposure_uniform, wb_multipliers,
    wb_uniform, ContrastUniform, ExposureUniform, WbUniform, CONTRAST_PIVOT,
};

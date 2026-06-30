//! ferrolite-pipeline — the photo edit DAG (see crate docs in the final lib.rs).
mod op;
mod serialize;
mod uniforms;

pub use op::{Contrast, Exposure, Op, OpKind, OpStack, WhiteBalance, STACK_VERSION};
pub use serialize::{deserialize, serialize};
pub use uniforms::{
    contrast_gain_pivot, contrast_uniform, exposure_gain, exposure_uniform, wb_multipliers,
    wb_uniform, ContrastUniform, ExposureUniform, WbUniform, CONTRAST_PIVOT,
};

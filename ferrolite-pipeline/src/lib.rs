//! ferrolite-pipeline — the photo edit DAG (see crate docs in the final lib.rs).
mod op;
mod serialize;

pub use op::{Contrast, Exposure, Op, OpKind, OpStack, WhiteBalance, STACK_VERSION};
pub use serialize::{deserialize, serialize};

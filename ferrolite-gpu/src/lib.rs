//! ferrolite-gpu — wgpu context + a generic, photo-agnostic retained-DAG
//! executor. Engine-transferable (permissive deps only).

mod context;
mod executor;

pub use context::GpuContext;
pub use executor::{Graph, Node, NodeId};

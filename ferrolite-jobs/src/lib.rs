//! ferrolite-jobs — a photo-agnostic threaded job scheduler with priorities,
//! cooperative cancellation, and panic isolation. Engine-transferable.

mod priority;
mod queue;
mod system;

pub use priority::{CancelToken, JobId, Priority};
pub use system::{JobHandle, JobSystem};

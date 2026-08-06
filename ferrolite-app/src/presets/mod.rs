//! Presets, copy/paste/sync and batch apply (P7).

pub mod apply;
pub mod menu;
pub mod modal;
pub mod store;

pub use store::{
    delete, presets_dir, rename, sanitize_filename, save, spawn_load_all, Preset, PresetError,
};
// `load_all` has no call site in the binary outside `spawn_load_all` itself
// (every consumer goes through the off-thread rescan, never the synchronous
// read) — still reserved, and used directly by the store's own tests.
// Scoped allow per the house pattern in `icons.rs`, never a blanket module allow.
#[allow(unused_imports)]
pub use store::load_all;

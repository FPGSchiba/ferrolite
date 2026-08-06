//! Presets, copy/paste/sync and batch apply (P7).

pub mod apply;
pub mod menu;
pub mod modal;
pub mod store;

pub use store::{presets_dir, save, spawn_load_all, Preset};
// No call site in the binary yet — reserved for the preset-management UI
// (rename/delete) and used by the store's own tests. Scoped allow per the
// house pattern in `icons.rs`, never a blanket module allow.
#[allow(unused_imports)]
pub use store::{delete, load_all, sanitize_filename, PresetError};

//! Presets, copy/paste/sync and batch apply (P7).

pub mod apply;
pub mod menu;
pub mod modal;
pub mod store;

pub use store::{
    delete, presets_dir, rename, sanitize_filename, save, spawn_load_all, Preset, PresetError,
};

//! Presets, copy/paste/sync and batch apply (P7).

pub mod store;

pub use store::{delete, load_all, presets_dir, sanitize_filename, save, Preset, PresetError};

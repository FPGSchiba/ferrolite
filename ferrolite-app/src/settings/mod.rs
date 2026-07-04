//! App settings: persisted user preferences (keybindings, export options,
//! Library filter, and app preferences). Stored as JSON in the OS data dir;
//! loaded at startup, saved off the UI thread. NOT part of the catalog (which
//! is a rebuildable cache) — this is genuine app state.

pub mod dto;
pub mod keymap;
pub mod persist;
pub mod ui;

use serde::{Deserialize, Serialize};

/// Root persisted settings document. Every field defaults so older/partial
/// files load cleanly (forward/backward tolerant).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub keymap: keymap::Keymap,
    pub export: dto::PersistedExport,
    pub filter: dto::PersistedFilter,
    pub working_space: dto::PersistedWorkingSpace,
    pub grid_size: f32,
    pub confirm_remove: bool,
    pub show_histogram: bool,
    pub restore_session: bool,
    pub last_module: dto::PersistedModule,
    pub last_folder: Option<std::path::PathBuf>,
    pub display_profile: dto::PersistedDisplayProfile,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            keymap: keymap::Keymap::defaults(),
            export: dto::PersistedExport::default(),
            filter: dto::PersistedFilter::default(),
            working_space: dto::PersistedWorkingSpace::default(),
            grid_size: 46.0,
            confirm_remove: true,
            show_histogram: true,
            restore_session: false,
            last_module: dto::PersistedModule::default(),
            last_folder: None,
            display_profile: dto::PersistedDisplayProfile::default(),
        }
    }
}

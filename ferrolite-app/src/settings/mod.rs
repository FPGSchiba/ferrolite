//! App settings: persisted user preferences (keybindings, export options,
//! Library filter, and app preferences). Stored as JSON in the OS data dir;
//! loaded at startup, saved off the UI thread. NOT part of the catalog (which
//! is a rebuildable cache) — this is genuine app state.

pub mod dto;
pub mod keymap;
pub mod persist;
pub mod ui;

pub use dto::Settings;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_palette_defaults_on() {
        assert!(Settings::default().show_tool_palette);
    }

    #[test]
    fn info_overlay_defaults_off() {
        assert!(!Settings::default().show_info_overlay);
    }
}

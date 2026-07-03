//! Settings JSON path resolution + (de)serialization. The write side runs on a
//! ferrolite-jobs job (see `save`), never on the UI thread.

use crate::settings::Settings;
use std::path::PathBuf;

/// `%LOCALAPPDATA%/ferrolite/settings.json`, falling back to the temp dir when
/// `LOCALAPPDATA` is unset (mirrors `state::default_db_path` / `diag`).
pub fn settings_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("ferrolite").join("settings.json")
}

pub fn to_json(s: &Settings) -> String {
    serde_json::to_string_pretty(s).unwrap_or_else(|_| "{}".to_string())
}

/// Parse settings JSON; on any error return defaults (never panics). Also
/// backfills any keymap action missing from the parsed document (e.g. an
/// explicit `{"keymap": {}}`) so every action always resolves to a real
/// chord rather than collapsing to the shared F1 fallback.
pub fn from_json(text: &str) -> Settings {
    let mut settings: Settings = serde_json::from_str(text).unwrap_or_default();
    settings.keymap.ensure_complete();
    settings
}

/// Load settings from disk; missing file or parse error → defaults.
pub fn load() -> Settings {
    match std::fs::read_to_string(settings_path()) {
        Ok(text) => from_json(&text),
        Err(_) => Settings::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn json_roundtrip_equals_original() {
        let s = Settings::default();
        let round = from_json(&to_json(&s));
        assert_eq!(round, s);
    }

    #[test]
    fn partial_json_fills_defaults() {
        // A file with only one field set must load, filling the rest from defaults.
        let json = r#"{ "confirm_remove": false }"#;
        let s = from_json(json);
        assert!(!s.confirm_remove);
        assert!(
            s.show_histogram,
            "unspecified field takes its default (true)"
        );
        assert_eq!(s.grid_size, Settings::default().grid_size);
    }

    #[test]
    fn garbage_json_falls_back_to_default() {
        assert_eq!(from_json("not json at all"), Settings::default());
    }

    #[test]
    fn empty_keymap_object_fills_defaults() {
        use crate::settings::keymap::{Action, Keymap};
        let s = from_json(r#"{ "keymap": {} }"#);
        let def = Keymap::defaults();
        for a in Action::ALL {
            assert_eq!(
                s.keymap.chord(a),
                def.chord(a),
                "action {:?} must fall back to default",
                a
            );
        }
    }
}

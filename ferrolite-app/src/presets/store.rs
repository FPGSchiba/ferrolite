//! Preset persistence: plain JSON files on disk, no catalog table.
//!
//! Contract 2 says the catalog is a cache rebuildable from disk. A user-authored
//! preset is NOT derivable from image files, so it cannot live only in the
//! catalog — it is a file. With the file as the source of truth a catalog index
//! buys nothing at realistic scale (tens to low hundreds of small JSON files
//! read once at startup), so no table is added: nothing cached means nothing
//! that can go stale. (P7 design §4.)

use std::path::{Path, PathBuf};

use ferrolite_pipeline::{EditDoc, EditPatch, GroupSet, PATCH_VERSION};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("preset name is empty or contains no usable characters")]
    InvalidName,
    #[error("a preset already exists with the filename \"{0}\"")]
    Duplicate(String),
}

/// One saved preset. The DISPLAY name lives inside the file; the filename is a
/// lossy sanitization of it (see `sanitize_filename`), so the sanitized form is
/// never shown to the user.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub version: u32,
    pub name: String,
    pub owns: GroupSet,
    pub doc: EditDoc,
}

impl Preset {
    pub fn to_patch(&self) -> EditPatch {
        EditPatch {
            version: self.version,
            owns: self.owns,
            doc: self.doc.clone(),
        }
    }
}

/// `<base>/ferrolite/presets`, resolved by the same logic as `catalog.db`
/// (`state::default_db_path`): LOCALAPPDATA, else XDG_DATA_HOME, else HOME,
/// else the current directory.
pub fn presets_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ferrolite").join("presets")
}

/// Derive a safe filename stem from a display name: every character outside
/// `[A-Za-z0-9 _-]` becomes `_`, runs of `_` collapse, the result is trimmed and
/// truncated to 64 chars. `None` when nothing usable remains.
///
/// Deliberately LOSSY — `Warm/Cool` and `Warm_Cool` collide — so uniqueness is
/// checked against this output, not the display name (see `save`).
pub fn sanitize_filename(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut last_underscore = false;
    for ch in name.trim().chars() {
        let ok = ch.is_ascii_alphanumeric() || ch == ' ' || ch == '_' || ch == '-';
        if ok {
            out.push(ch);
            last_underscore = ch == '_';
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    let trimmed = out.trim().trim_matches('_').trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(64).collect())
}

/// Read every `*.json` in `dir`. Unreadable, malformed, or wrong-version files
/// are SKIPPED (never a panic), mirroring `ferrolite_pipeline::deserialize`'s
/// contract. Returns presets sorted by display name, case-insensitively.
pub fn load_all(dir: &Path) -> Vec<Preset> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new(); // no dir yet == no presets
    };
    let mut out: Vec<Preset> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .filter_map(|text| serde_json::from_str::<Preset>(&text).ok())
        .filter(|p| p.version == PATCH_VERSION)
        .collect();
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Write `preset` as `<dir>/<sanitized>.json`, creating `dir` if needed.
/// Rejects a name that sanitizes to nothing, and rejects a filename collision
/// rather than silently overwriting.
pub fn save(dir: &Path, preset: &Preset) -> Result<PathBuf, PresetError> {
    let stem = sanitize_filename(&preset.name).ok_or(PresetError::InvalidName)?;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{stem}.json"));
    if path.exists() {
        return Err(PresetError::Duplicate(stem));
    }
    let json = serde_json::to_string_pretty(preset).expect("Preset is always serializable");
    std::fs::write(&path, json)?;
    Ok(path)
}

/// Scan the preset directory off the UI thread (contract 1 — this is file I/O,
/// however small) and deliver the list over the event channel.
pub fn spawn_load_all(
    jobs: &std::sync::Arc<ferrolite_jobs::JobSystem>,
    tx: &std::sync::mpsc::Sender<crate::events::AppEvent>,
    ctx: &egui::Context,
) {
    let tx = tx.clone();
    let ctx = ctx.clone();
    jobs.submit(ferrolite_jobs::Priority::Background, move |_cancel| {
        let presets = load_all(&presets_dir());
        let _ = tx.send(crate::events::AppEvent::PresetsLoaded { presets });
        ctx.request_repaint();
    });
}

/// Remove the file backing `preset`. A missing file is NOT an error — the
/// desired end state (no such preset) already holds.
pub fn delete(dir: &Path, preset: &Preset) -> Result<(), PresetError> {
    let Some(stem) = sanitize_filename(&preset.name) else {
        return Err(PresetError::InvalidName);
    };
    let path = dir.join(format!("{stem}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(PresetError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_invalid_chars_and_collapses_runs() {
        assert_eq!(
            sanitize_filename("Warm portrait").as_deref(),
            Some("Warm portrait")
        );
        assert_eq!(sanitize_filename("Warm/Cool").as_deref(), Some("Warm_Cool"));
        assert_eq!(
            sanitize_filename("a***b").as_deref(),
            Some("a_b"),
            "runs collapse"
        );
        assert_eq!(
            sanitize_filename("  padded  ").as_deref(),
            Some("padded"),
            "trimmed"
        );
    }

    #[test]
    fn sanitize_rejects_empty_and_all_invalid_names() {
        assert_eq!(sanitize_filename(""), None);
        assert_eq!(sanitize_filename("   "), None);
        assert_eq!(
            sanitize_filename("///"),
            None,
            "all-invalid collapses to nothing usable"
        );
    }

    #[test]
    fn sanitize_truncates_to_64_chars() {
        let long = "x".repeat(200);
        assert_eq!(sanitize_filename(&long).unwrap().len(), 64);
    }

    // `ThreadId`'s `Debug` format is not guaranteed to be filename-safe on all
    // platforms (Windows forbids several ASCII punctuation characters in
    // paths), so derive the uniqueness suffix from a hash of the thread id
    // plus a per-process atomic counter instead of `{:?}`-formatting it
    // directly. Tests must be able to run in parallel without colliding.
    fn tmp() -> std::path::PathBuf {
        use std::hash::{Hash, Hasher};
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::thread::current().id().hash(&mut hasher);
        let thread_hash = hasher.finish();
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "ferrolite-preset-test-{}-{thread_hash:x}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[allow(clippy::field_reassign_with_default)]
    // default-then-assign mirrors the plan's literal test spec; clearer than
    // struct-update for single fields.
    fn sample(name: &str) -> Preset {
        let mut doc = EditDoc::default();
        doc.global.exposure = 0.75;
        Preset {
            version: PATCH_VERSION,
            name: name.into(),
            owns: GroupSet::LIGHT,
            doc,
        }
    }

    #[test]
    fn save_then_load_all_round_trips() {
        let dir = tmp();
        let p = sample("Warm portrait");
        save(&dir, &p).expect("save");
        let loaded = load_all(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_rejects_a_filename_collision_rather_than_overwriting() {
        let dir = tmp();
        save(&dir, &sample("Warm/Cool")).expect("first save");
        // Sanitizes to the SAME stem "Warm_Cool" — must be refused.
        let err = save(&dir, &sample("Warm_Cool")).expect_err("collision must be refused");
        assert!(matches!(err, PresetError::Duplicate(_)), "got {err:?}");
        assert_eq!(load_all(&dir).len(), 1, "the original must survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_rejects_an_unusable_name() {
        let dir = tmp();
        let err = save(&dir, &sample("///")).expect_err("must reject");
        assert!(matches!(err, PresetError::InvalidName), "got {err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_all_skips_malformed_and_wrong_version_files_without_panicking() {
        let dir = tmp();
        save(&dir, &sample("Good")).expect("save");
        std::fs::write(dir.join("garbage.json"), "not json {{").unwrap();
        std::fs::write(
            dir.join("future.json"),
            r#"{"version":999,"name":"Future","owns":1,"doc":{"version":2}}"#,
        )
        .unwrap();
        let loaded = load_all(&dir);
        assert_eq!(loaded.len(), 1, "only the good preset loads");
        assert_eq!(loaded[0].name, "Good");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_the_file_and_is_idempotent() {
        let dir = tmp();
        let p = sample("Gone");
        save(&dir, &p).expect("save");
        delete(&dir, &p).expect("delete");
        assert!(load_all(&dir).is_empty());
        delete(&dir, &p).expect("second delete is a no-op, not an error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_all_on_a_missing_directory_is_empty_not_an_error() {
        assert!(load_all(std::path::Path::new("definitely/not/here")).is_empty());
    }
}

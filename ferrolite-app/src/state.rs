//! Application state: catalog handles, the job system, the event channel, and
//! the currently-browsed folder's rows + selection + progress counters.

use ferrolite_catalog::{Catalog, ImageRecord, ReadPool};
use ferrolite_jobs::{JobHandle, JobId, JobSystem};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::events::AppEvent;

/// A folder awaiting remove confirmation (shown in a modal).
#[derive(Debug, Clone)]
pub struct PendingRemove {
    pub id: i64,
    pub name: String,
    pub subtree_count: u64,
}

pub struct AppState {
    pub jobs: Arc<JobSystem>,
    pub writer: Arc<Mutex<Catalog>>,
    pub reads: Arc<ReadPool>,
    pub tx: Sender<AppEvent>,
    pub rx: Receiver<AppEvent>,

    pub current_folder: Option<i64>,
    pub images: Vec<ImageRecord>,
    pub selected: Option<i64>,

    pub indexed: u64,
    pub thumb_total: usize,
    pub thumb_done: usize,

    /// image_id → its pending/running thumbnail job (for reprioritization/cancel).
    pub thumb_jobs: HashMap<i64, JobId>,
    pub ingest_handle: Option<JobHandle>,

    /// LRU cache of decoded thumbnail textures (cap 512).
    pub textures: crate::library::texture_cache::TextureCache,
    /// IDs visible in the grid on the last frame (for delta reprioritization).
    pub last_visible: HashSet<i64>,

    /// Set to `true` whenever catalog-visible state changes (ingest events,
    /// folder switch). `app.rs` checks this flag before calling
    /// `refresh_images()` so idle frames issue zero SQL queries.
    pub dirty: bool,

    /// Recursive (subtree) vs direct folder view. Default true (on).
    pub include_subfolders: bool,
    /// Folder ids whose children are shown in the left-panel tree.
    pub expanded_folders: HashSet<i64>,
    /// A folder pending a remove-confirmation (set when it has subfolders).
    pub pending_remove: Option<PendingRemove>,
}

impl AppState {
    /// Open (or create) the catalog at the OS data dir and wire the job system.
    pub fn new() -> Result<Self, ferrolite_catalog::CatalogError> {
        let db_path = default_db_path();
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let writer = Catalog::open(&db_path)?;
        let reads = ReadPool::open(&db_path, 4)?;
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).max(1))
            .unwrap_or(3);
        let (tx, rx) = std::sync::mpsc::channel();
        Ok(Self {
            jobs: Arc::new(JobSystem::new(workers)),
            writer: Arc::new(Mutex::new(writer)),
            reads: Arc::new(reads),
            tx,
            rx,
            current_folder: None,
            images: Vec::new(),
            selected: None,
            indexed: 0,
            thumb_total: 0,
            thumb_done: 0,
            thumb_jobs: HashMap::new(),
            ingest_handle: None,
            textures: crate::library::texture_cache::TextureCache::new(512),
            last_visible: HashSet::new(),
            dirty: true,
            include_subfolders: true,
            expanded_folders: HashSet::new(),
            pending_remove: None,
        })
    }

    /// Decode a thumbnail JPEG and upload it as an egui texture into the cache.
    pub fn upload_thumbnail(&mut self, ctx: &egui::Context, image_id: i64, jpeg: Vec<u8>) {
        let Ok(img) = image::load_from_memory(&jpeg) else {
            return;
        };
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        let color = egui::ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw());
        let tex = ctx.load_texture(
            format!("thumb-{image_id}"),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.textures.insert(image_id, tex);
    }

    /// Reload the visible folder's rows from the read pool (called after ingest
    /// progress / folder switch). Cheap: indexed query, no filesystem walk.
    pub fn refresh_images(&mut self) {
        if let Some(folder_id) = self.current_folder {
            let rows = if self.include_subfolders {
                self.reads.list_images_recursive(folder_id)
            } else {
                self.reads.list_images(folder_id)
            };
            if let Ok(rows) = rows {
                self.images = rows;
            }
        }
    }

    /// Reset per-folder job + counter state when switching folders: cancel any
    /// pending thumbnail jobs, drop their handles, zero the progress counters,
    /// and mark the view dirty so the grid reloads.
    pub fn reset_for_new_folder(&mut self) {
        if let Some(h) = self.ingest_handle.take() {
            h.cancel();
        }
        for (_image_id, job_id) in self.thumb_jobs.drain() {
            self.jobs.cancel(job_id);
        }
        self.indexed = 0;
        self.thumb_total = 0;
        self.thumb_done = 0;
        self.images.clear();
        self.selected = None;
        self.dirty = true;
    }

    /// Switch the browsed folder (from the folder list) and reset state.
    pub fn select_folder(&mut self, folder_id: i64) {
        self.reset_for_new_folder();
        self.current_folder = Some(folder_id);
    }

    /// Remove a folder subtree from the catalog (cache only). If the current
    /// folder is inside the removed subtree, reset selection/jobs first.
    pub fn remove_folder_cascade(&mut self, folder_id: i64) {
        let removed_set = self.subtree_ids(folder_id);
        if self
            .current_folder
            .map(|c| removed_set.contains(&c))
            .unwrap_or(false)
        {
            self.reset_for_new_folder();
            self.current_folder = None;
        }
        if let Err(e) = self.writer.lock().expect("writer").remove_folder(folder_id) {
            eprintln!("ferrolite: remove_folder failed: {e}");
            return;
        }
        self.expanded_folders.remove(&folder_id);
        self.dirty = true;
    }

    /// Folder ids in the subtree rooted at `folder_id`, computed from the flat
    /// folder list (read pool).
    fn subtree_ids(&self, folder_id: i64) -> HashSet<i64> {
        let folders = self.reads.list_folders().unwrap_or_default();
        let mut out = HashSet::new();
        let mut stack = vec![folder_id];
        while let Some(id) = stack.pop() {
            if out.insert(id) {
                for f in &folders {
                    if f.parent_id == Some(id) {
                        stack.push(f.id);
                    }
                }
            }
        }
        out
    }

    #[cfg(test)]
    pub fn for_test() -> Self {
        // Use a unique ID per test (thread + process) to avoid concurrent collision.
        let tid = format!("{:?}", std::thread::current().id()).replace(['(', ')'], "");
        let path =
            std::env::temp_dir().join(format!("ferrolite-test-{}-{}.db", std::process::id(), tid));
        let _ = std::fs::remove_file(&path);
        let writer = Catalog::open(&path).unwrap();
        let reads = ReadPool::open(&path, 1).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            jobs: Arc::new(JobSystem::new(1)),
            writer: Arc::new(Mutex::new(writer)),
            reads: Arc::new(reads),
            tx,
            rx,
            current_folder: None,
            images: Vec::new(),
            selected: None,
            indexed: 0,
            thumb_total: 0,
            thumb_done: 0,
            thumb_jobs: HashMap::new(),
            ingest_handle: None,
            textures: crate::library::texture_cache::TextureCache::new(512),
            last_visible: HashSet::new(),
            dirty: true,
            include_subfolders: true,
            expanded_folders: HashSet::new(),
            pending_remove: None,
        }
    }
}

fn default_db_path() -> PathBuf {
    // Keep it simple + dependency-free: use the OS temp/home; a proper data-dir
    // crate can replace this later. Falls back to the current dir.
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ferrolite").join("catalog.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `reset_for_new_folder` must zero all per-folder counters, drain `thumb_jobs`,
    /// clear `images`, clear `selected`, and set the dirty flag.
    #[test]
    fn reset_for_new_folder_zeroes_counters_and_clears_jobs() {
        let mut s = AppState::for_test();
        // Seed some prior state.
        s.indexed = 42;
        s.thumb_total = 10;
        s.thumb_done = 7;
        s.thumb_jobs.insert(1, ferrolite_jobs::JobId(100));
        s.thumb_jobs.insert(2, ferrolite_jobs::JobId(101));
        s.selected = Some(1);
        s.dirty = false; // simulate an idle frame that already cleared the flag

        s.reset_for_new_folder();

        assert_eq!(s.thumb_total, 0, "thumb_total must be zeroed");
        assert_eq!(s.thumb_done, 0, "thumb_done must be zeroed");
        assert_eq!(s.indexed, 0, "indexed must be zeroed");
        assert!(s.thumb_jobs.is_empty(), "thumb_jobs must be drained");
        assert!(s.images.is_empty(), "images must be cleared");
        assert_eq!(s.selected, None, "selected must be cleared");
        assert!(s.dirty, "dirty flag must be set after reset");
    }

    /// `select_folder` must delegate to `reset_for_new_folder` and then set the
    /// new `current_folder`.
    #[test]
    fn select_folder_resets_and_sets_folder() {
        let mut s = AppState::for_test();
        s.current_folder = Some(99);
        s.thumb_total = 5;
        s.thumb_done = 3;
        s.thumb_jobs.insert(7, ferrolite_jobs::JobId(200));
        s.dirty = false;

        s.select_folder(42);

        assert_eq!(s.current_folder, Some(42));
        assert_eq!(s.thumb_total, 0);
        assert_eq!(s.thumb_done, 0);
        assert!(s.thumb_jobs.is_empty());
        assert!(s.dirty);
    }

    #[test]
    fn refresh_images_honors_include_subfolders() {
        use ferrolite_catalog::{FileKind, NewImage};
        let mut s = AppState::for_test();
        // Build root(parent None) with a child; one image in each.
        let (root, child) = {
            let w = s.writer.lock().unwrap();
            let root = w.upsert_folder(std::path::Path::new("/p"), None).unwrap();
            let child = w
                .upsert_folder(std::path::Path::new("/p/sub"), Some(root))
                .unwrap();
            w.upsert_image(&NewImage::failed(root, "a.nef".into(), 1, 1, FileKind::Raw))
                .unwrap();
            w.upsert_image(&NewImage::failed(
                child,
                "b.jpg".into(),
                1,
                1,
                FileKind::Standard,
            ))
            .unwrap();
            (root, child)
        };
        let _ = child;
        s.current_folder = Some(root);

        s.include_subfolders = false;
        s.refresh_images();
        assert_eq!(s.images.len(), 1, "direct view: only root's image");

        s.include_subfolders = true;
        s.refresh_images();
        assert_eq!(s.images.len(), 2, "recursive view: root + child images");
    }

    #[test]
    fn remove_folder_cascade_clears_current_when_inside_subtree() {
        use ferrolite_catalog::{FileKind, NewImage};
        let mut s = AppState::for_test();
        let (root, child) = {
            let w = s.writer.lock().unwrap();
            let root = w.upsert_folder(std::path::Path::new("/p"), None).unwrap();
            let child = w
                .upsert_folder(std::path::Path::new("/p/sub"), Some(root))
                .unwrap();
            w.upsert_image(&NewImage::failed(
                child,
                "b.jpg".into(),
                1,
                1,
                FileKind::Standard,
            ))
            .unwrap();
            (root, child)
        };
        s.current_folder = Some(child);
        s.remove_folder_cascade(root); // removing an ancestor of current
        assert_eq!(
            s.current_folder, None,
            "current cleared when in removed subtree"
        );
        assert!(s.reads.list_folders().unwrap().is_empty());
    }
}

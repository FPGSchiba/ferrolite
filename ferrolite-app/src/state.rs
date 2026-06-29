//! Application state: catalog handles, the job system, the event channel, and
//! the currently-browsed folder's rows + selection + progress counters.

use ferrolite_catalog::{Catalog, ImageRecord, ReadPool};
use ferrolite_jobs::{JobHandle, JobId, JobSystem};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::events::AppEvent;

pub struct AppState {
    pub jobs: Arc<JobSystem>,
    pub writer: Arc<Mutex<Catalog>>,
    pub reads: Arc<ReadPool>,
    pub db_path: PathBuf,
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
            db_path,
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
        let tex = ctx.load_texture(format!("thumb-{image_id}"), color, egui::TextureOptions::LINEAR);
        self.textures.insert(image_id, tex);
    }

    /// Reload the visible folder's rows from the read pool (called after ingest
    /// progress / folder switch). Cheap: indexed query, no filesystem walk.
    pub fn refresh_images(&mut self) {
        if let Some(folder_id) = self.current_folder {
            if let Ok(rows) = self.reads.list_images(folder_id) {
                self.images = rows;
            }
        }
    }

    #[cfg(test)]
    pub fn for_test() -> Self {
        // Use a unique ID per test (thread + process) to avoid concurrent collision.
        let tid = format!("{:?}", std::thread::current().id()).replace(['(', ')'], "");
        let path = std::env::temp_dir()
            .join(format!("ferrolite-test-{}-{}.db", std::process::id(), tid));
        let _ = std::fs::remove_file(&path);
        let writer = Catalog::open(&path).unwrap();
        let reads = ReadPool::open(&path, 1).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            jobs: Arc::new(JobSystem::new(1)),
            writer: Arc::new(Mutex::new(writer)),
            reads: Arc::new(reads),
            db_path: path,
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

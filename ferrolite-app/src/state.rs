//! Application state: catalog handles, the job system, the event channel, and
//! the currently-browsed folder's rows + selection + progress counters.

use crate::events::AppEvent;
use crate::library::filter::{FilterState, ViewSource};
use crate::metadata::MetaEdit;
use ferrolite_catalog::{
    Catalog, CollectionRecord, ImageRecord, LibraryQuery, ReadPool, TagRecord,
};
use ferrolite_image::TagId;
use ferrolite_jobs::{JobHandle, JobId, JobSystem};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

/// A folder awaiting remove confirmation (shown in a modal).
#[derive(Debug, Clone)]
pub struct PendingRemove {
    pub id: i64,
    pub name: String,
    pub subtree_count: u64,
}

/// Which kind of item is being renamed inline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameKind {
    Tag,
    Collection,
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

    /// Number of ingest jobs currently in flight (open/reindex/watcher/startup).
    /// The watcher fires only when this is 0. Incremented on spawn, decremented
    /// on `IngestDone`.
    pub active_ingests: usize,
    /// Wall-clock of the last watcher tick (for the periodic check).
    pub last_watch_check: Option<std::time::Instant>,
    /// One-time startup rescan guard (fires on the first update frame).
    pub startup_rescan_done: bool,

    /// Recursive (subtree) vs direct folder view. Default true (on).
    pub include_subfolders: bool,
    /// Folder ids whose children are shown in the left-panel tree.
    pub expanded_folders: HashSet<i64>,
    /// A folder pending a remove-confirmation (set when it has subfolders).
    pub pending_remove: Option<PendingRemove>,

    /// Non-None while the single-image viewer is open.
    pub viewer: Option<crate::viewer::ViewerState>,

    /// Active filter state (search text, rating, flags, tags, etc.).
    pub filter: FilterState,
    /// Which set of images is shown (folder, all, collection, recently added).
    pub source: ViewSource,
    /// Full tag vocabulary loaded from the catalog.
    pub tags: Vec<TagRecord>,
    /// Full collection vocabulary loaded from the catalog.
    pub collections: Vec<CollectionRecord>,
    /// Per-image tag associations cached for the currently visible grid cells.
    pub visible_tags: HashMap<i64, Vec<TagId>>,
    /// Selected image ids (multi-selection for batch ops).
    pub selection: HashSet<i64>,
    /// The anchor image id for shift-click range selection.
    pub selection_anchor: Option<i64>,
    /// Non-critical warning surfaced in the UI (e.g. query error).
    pub warning: Option<String>,

    /// Inline rename in progress: (kind, id, edit buffer).
    /// Set on double-click or "Rename" context-menu; cleared on Enter/blur.
    pub renaming: Option<(RenameKind, i64, String)>,

    // ── Cached toolbar metadata-filter aggregates (populated by reload_vocab) ──
    /// Distinct camera-model strings from the catalog.
    pub camera_options: Vec<String>,
    /// (min, max) ISO across the catalog, or None if no EXIF ISO is indexed.
    pub iso_range: Option<(u32, u32)>,
    /// (earliest, latest) capture-date strings from the catalog, or None.
    pub date_range: Option<(String, String)>,
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
            active_ingests: 0,
            last_watch_check: None,
            startup_rescan_done: false,
            include_subfolders: true,
            expanded_folders: HashSet::new(),
            pending_remove: None,
            viewer: None,
            filter: FilterState::default(),
            source: ViewSource::All,
            tags: Vec::new(),
            collections: Vec::new(),
            visible_tags: HashMap::new(),
            selection: HashSet::new(),
            selection_anchor: None,
            warning: None,
            camera_options: Vec::new(),
            iso_range: None,
            date_range: None,
            renaming: None,
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

    /// Build a `LibraryQuery` from the current source + filter state.
    pub fn build_query(&self) -> LibraryQuery {
        self.filter.to_query(self.source, self.include_subfolders)
    }

    /// Load the full tag and collection vocabularies, and refresh cached
    /// toolbar metadata-filter aggregates (camera list, ISO range, date range).
    /// Called at startup and after ingest completes.
    pub fn reload_vocab(&mut self) {
        if let Ok(t) = self.reads.list_tags() {
            self.tags = t;
        }
        if let Ok(c) = self.reads.list_collections() {
            self.collections = c;
        }
        self.camera_options = self.reads.distinct_cameras().unwrap_or_default();
        self.iso_range = self.reads.iso_bounds().unwrap_or_default();
        self.date_range = self.reads.date_bounds().unwrap_or_default();
    }

    /// Apply a metadata edit to the current selection (fallback to single
    /// `selected`): optimistic in-memory update of every affected grid row
    /// + `visible_tags`, then an off-thread persist (DB + xmp:Rating sidecar).
    pub fn apply_metadata_edit(&mut self, ctx: &egui::Context, edit: MetaEdit) {
        let mut targets: Vec<i64> = self.selection.iter().copied().collect();
        if targets.is_empty() {
            if let Some(id) = self.selected {
                targets.push(id);
            }
        }
        self.apply_metadata_edit_to_ids(ctx, &targets, edit);
    }

    /// Shared core: optimistically update each id's in-memory row + tag cache,
    /// then persist all of them in ONE off-thread job (DB + xmp:Rating).
    pub fn apply_metadata_edit_to_ids(&mut self, ctx: &egui::Context, ids: &[i64], edit: MetaEdit) {
        if ids.is_empty() {
            return;
        }
        // Collect (id, path) pairs for the persist job while borrowing reads.
        let mut image_paths: Vec<(i64, std::path::PathBuf)> = Vec::new();
        for id in ids {
            if let Some(rec) = self.images.iter().find(|r| r.id == *id).cloned() {
                if let Ok(Some(fp)) = self.reads.folder_path(rec.folder_id) {
                    image_paths.push((*id, std::path::PathBuf::from(fp).join(&rec.filename)));
                }
            }
        }
        // Optimistic in-memory update of grid rows + visible_tags cache.
        for id in ids {
            let mut tags = self.visible_tags.get(id).cloned().unwrap_or_default();
            if let Some(rec) = self.images.iter_mut().find(|r| r.id == *id) {
                crate::metadata::apply_edit_in_memory(rec, &mut tags, edit);
            }
            self.visible_tags.insert(*id, tags);
        }
        // ONE spawn for all images — batching is preserved.
        crate::metadata::spawn_metadata_write(
            &self.jobs,
            &self.writer,
            &self.tx,
            ctx,
            edit,
            image_paths,
        );
    }

    /// Apply an edit to a single explicit image (used by Develop: the open viewer image).
    /// Targets ONLY the given id — ignores grid selection.
    pub fn apply_metadata_edit_to_image(
        &mut self,
        ctx: &egui::Context,
        image_id: i64,
        edit: MetaEdit,
    ) {
        self.apply_metadata_edit_to_ids(ctx, &[image_id], edit);
    }

    /// Fetch tag associations for any visible image ids not yet cached (virtualised).
    pub fn ensure_tags_for(&mut self, ids: &HashSet<i64>) {
        let missing: Vec<i64> = ids
            .iter()
            .copied()
            .filter(|id| !self.visible_tags.contains_key(id))
            .collect();
        if missing.is_empty() {
            return;
        }
        if let Ok(map) = self.reads.tags_for_images(&missing) {
            for id in missing {
                self.visible_tags
                    .insert(id, map.get(&id).cloned().unwrap_or_default());
            }
        }
    }

    /// Reload the visible set of images from the read pool (called after ingest
    /// progress / folder switch / filter change). Cheap: indexed query, no
    /// filesystem walk.
    pub fn refresh_images(&mut self) {
        let q = self.build_query();
        if let Ok(rows) = self.reads.query_images(&q) {
            self.images = rows;
        }
        // Invalidate the per-cell tag cache so the grid re-fetches for the new set.
        self.visible_tags.clear();
    }

    /// Open `rec` in the viewer, cancelling any currently-open viewer first.
    /// Shared by the grid double-click (in `grid.rs`) and the Enter-key handler
    /// (in `app.rs`) so the two code paths stay in sync.
    pub fn open_image_in_viewer(&mut self, rec: &ferrolite_catalog::ImageRecord) {
        if let Ok(Some(folder_path)) = self.reads.folder_path(rec.folder_id) {
            let path = std::path::PathBuf::from(folder_path).join(&rec.filename);
            if let Some(old) = self.viewer.as_ref() {
                old.cancel_loads();
            }
            self.viewer = Some(crate::viewer::ViewerState::open(rec.id, path, rec.kind));
        }
    }

    /// Cancel any in-flight ingest + pending thumbnail jobs, without touching the
    /// view (images/current_folder/selection) or counters. Used by reindex.
    pub fn cancel_pending_jobs(&mut self) {
        if let Some(h) = self.ingest_handle.take() {
            h.cancel();
            // A queued-but-not-yet-dispatched job is skipped by the worker and
            // never emits IngestDone, so decrement here to keep the counter
            // balanced. If the job was already running it will still emit
            // IngestDone; the extra decrement is absorbed by saturating_sub.
            self.active_ingests = self.active_ingests.saturating_sub(1);
        }
        for (_image_id, job_id) in self.thumb_jobs.drain() {
            self.jobs.cancel(job_id);
        }
    }

    /// Reset per-folder job + counter state when switching folders.
    pub fn reset_for_new_folder(&mut self) {
        self.cancel_pending_jobs();
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
        self.source = ViewSource::Folder(folder_id);
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
        self.expanded_folders.retain(|id| !removed_set.contains(id));
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
            active_ingests: 0,
            last_watch_check: None,
            startup_rescan_done: false,
            include_subfolders: true,
            expanded_folders: HashSet::new(),
            pending_remove: None,
            viewer: None,
            filter: FilterState::default(),
            source: ViewSource::All,
            tags: Vec::new(),
            collections: Vec::new(),
            visible_tags: HashMap::new(),
            selection: HashSet::new(),
            selection_anchor: None,
            warning: None,
            camera_options: Vec::new(),
            iso_range: None,
            date_range: None,
            renaming: None,
        }
    }

    /// Add all selected images (or the single `selected` fallback) to a collection.
    pub fn add_selection_to_collection(&mut self, coll_id: i64) {
        let mut targets: Vec<i64> = self.selection.iter().copied().collect();
        if targets.is_empty() {
            if let Some(id) = self.selected {
                targets.push(id);
            }
        }
        self.add_images_to_collection(&targets, coll_id);
    }

    /// Shared core: write every id into the collection, then mark dirty if the
    /// current source is that collection.
    pub fn add_images_to_collection(&mut self, ids: &[i64], coll_id: i64) {
        if ids.is_empty() {
            return;
        }
        {
            let w = self.writer.lock().expect("writer");
            for id in ids {
                let _ = w.add_image_to_collection(coll_id, *id);
            }
        }
        if matches!(self.source, ViewSource::Collection(id) if id == coll_id) {
            self.dirty = true;
        }
    }

    /// Add a single explicit image to a collection (used by Develop/viewer).
    pub fn add_image_to_collection_now(&mut self, image_id: i64, coll_id: i64) {
        self.add_images_to_collection(&[image_id], coll_id);
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
            w.upsert_image(&NewImage::failed(
                root,
                "a.nef".into(),
                1,
                1,
                FileKind::Raw,
                0,
            ))
            .unwrap();
            w.upsert_image(&NewImage::failed(
                child,
                "b.jpg".into(),
                1,
                1,
                FileKind::Standard,
                0,
            ))
            .unwrap();
            (root, child)
        };
        let _ = child;
        s.current_folder = Some(root);
        s.source = ViewSource::Folder(root);

        s.include_subfolders = false;
        s.refresh_images();
        assert_eq!(s.images.len(), 1, "direct view: only root's image");

        s.include_subfolders = true;
        s.refresh_images();
        assert_eq!(s.images.len(), 2, "recursive view: root + child images");
    }

    #[test]
    fn remove_folder_cascade_preserves_current_when_outside_subtree() {
        use ferrolite_catalog::{FileKind, NewImage};
        let mut s = AppState::for_test();
        let (root, sibling, other) = {
            let w = s.writer.lock().unwrap();
            let root = w.upsert_folder(std::path::Path::new("/p"), None).unwrap();
            let sibling = w
                .upsert_folder(std::path::Path::new("/p/a"), Some(root))
                .unwrap();
            let other = w
                .upsert_folder(std::path::Path::new("/p/b"), Some(root))
                .unwrap();
            w.upsert_image(&NewImage::failed(
                sibling,
                "a.jpg".into(),
                1,
                1,
                FileKind::Standard,
                0,
            ))
            .unwrap();
            (root, sibling, other)
        };
        let _ = root;
        // current_folder is `other` (not under `sibling`)
        s.current_folder = Some(other);
        s.remove_folder_cascade(sibling); // remove a different branch
        assert_eq!(
            s.current_folder,
            Some(other),
            "current_folder must be unchanged when outside removed subtree"
        );
        // `sibling` should no longer appear in the folder list
        let remaining: Vec<i64> = s
            .reads
            .list_folders()
            .unwrap()
            .iter()
            .map(|f| f.id)
            .collect();
        assert!(
            !remaining.contains(&sibling),
            "removed folder must be absent from list"
        );
    }

    #[test]
    fn cancel_pending_jobs_keeps_view_but_drains_jobs() {
        let mut s = AppState::for_test();
        s.current_folder = Some(7);
        s.images = vec![]; // (kept as-is; view not cleared)
        s.selected = Some(3);
        s.indexed = 5;
        s.thumb_jobs.insert(1, ferrolite_jobs::JobId(100));
        s.thumb_jobs.insert(2, ferrolite_jobs::JobId(101));

        s.cancel_pending_jobs();

        assert!(s.thumb_jobs.is_empty(), "thumb jobs drained");
        assert_eq!(s.current_folder, Some(7), "current folder preserved");
        assert_eq!(s.selected, Some(3), "selection preserved");
        assert_eq!(s.indexed, 5, "counters not zeroed by cancel_pending_jobs");
    }

    #[test]
    fn cancel_pending_jobs_decrements_active_and_clears_handle() {
        let mut s = AppState::for_test();
        s.current_folder = Some(7);
        s.selected = Some(3);
        // Simulate one in-flight ingest with a real handle.
        let handle = s
            .jobs
            .submit(ferrolite_jobs::Priority::Background, |_cancel| {});
        s.ingest_handle = Some(handle);
        s.active_ingests = 1;

        s.cancel_pending_jobs();

        assert_eq!(
            s.active_ingests, 0,
            "active_ingests decremented when a handle was cancelled"
        );
        assert!(s.ingest_handle.is_none(), "ingest_handle cleared");
        assert_eq!(s.current_folder, Some(7), "view preserved");
        assert_eq!(s.selected, Some(3), "selection preserved");
    }

    #[test]
    fn refresh_images_uses_filter_query_across_source() {
        use ferrolite_catalog::{FileKind, NewImage};
        let mut s = AppState::for_test();
        let (f1, f2) = {
            let w = s.writer.lock().unwrap();
            let f1 = w.upsert_folder(std::path::Path::new("/a"), None).unwrap();
            let f2 = w.upsert_folder(std::path::Path::new("/b"), None).unwrap();
            w.upsert_image(&NewImage::failed(
                f1,
                "a.nef".into(),
                1,
                1,
                FileKind::Raw,
                0,
            ))
            .unwrap();
            w.upsert_image(&NewImage::failed(
                f2,
                "b.nef".into(),
                1,
                1,
                FileKind::Raw,
                0,
            ))
            .unwrap();
            (f1, f2)
        };
        let _ = (f1, f2);
        // AllPhotographs source returns images from both folders.
        s.source = ViewSource::All;
        s.refresh_images();
        assert_eq!(s.images.len(), 2);
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
                0,
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

    /// `apply_metadata_edit` with `ToggleTag` must optimistically update
    /// `visible_tags` for every image in `selection` (or the single `selected`).
    /// The in-memory update is unconditional; the persist job fires off-thread
    /// with an empty path list (no folder row in the test DB, so folder_path
    /// returns None) and completes without error.
    #[test]
    fn apply_metadata_edit_toggle_tag_updates_visible_tags() {
        use ferrolite_catalog::{DecodeStatus, FileKind};
        use ferrolite_image::{Flag, Orientation, Rating, TagId};

        let mut s = AppState::for_test();
        let ctx = egui::Context::default();

        // Seed two in-memory image rows (no DB folder row — folder_path returns
        // None so image_paths will be empty, but the optimistic update still runs).
        let mk_rec = |id: i64| ferrolite_catalog::ImageRecord {
            id,
            folder_id: 99,
            filename: format!("img{id}.nef"),
            width: None,
            height: None,
            orientation: Orientation::Normal,
            capture_time: None,
            iso: None,
            decode_status: DecodeStatus::Done,
            kind: FileKind::Raw,
            rating: Rating::default(),
            flag: Flag::None,
        };

        s.images = vec![mk_rec(1), mk_rec(2)];
        s.selection = [1, 2].into_iter().collect();

        let tag = TagId(42);

        // First toggle: tag should be added to both images.
        s.apply_metadata_edit(&ctx, crate::metadata::MetaEdit::ToggleTag(tag));
        assert_eq!(
            s.visible_tags.get(&1).cloned().unwrap_or_default(),
            vec![tag],
            "image 1: tag added"
        );
        assert_eq!(
            s.visible_tags.get(&2).cloned().unwrap_or_default(),
            vec![tag],
            "image 2: tag added"
        );

        // Second toggle: tag should be removed from both images.
        s.apply_metadata_edit(&ctx, crate::metadata::MetaEdit::ToggleTag(tag));
        assert!(
            s.visible_tags.get(&1).map(|v| v.is_empty()).unwrap_or(true),
            "image 1: tag removed"
        );
        assert!(
            s.visible_tags.get(&2).map(|v| v.is_empty()).unwrap_or(true),
            "image 2: tag removed"
        );

        // Fallback path: no selection, single selected.
        s.selection.clear();
        s.selected = Some(1);
        s.apply_metadata_edit(&ctx, crate::metadata::MetaEdit::ToggleTag(tag));
        assert_eq!(
            s.visible_tags.get(&1).cloned().unwrap_or_default(),
            vec![tag],
            "single-selected fallback: tag added to image 1"
        );
        assert!(
            s.visible_tags.get(&2).map(|v| v.is_empty()).unwrap_or(true),
            "image 2 unchanged when not selected"
        );
    }

    /// `add_selection_to_collection` adds each selected image to the collection and
    /// sets `dirty` only when the current source is that collection.
    #[test]
    fn add_selection_to_collection_adds_images_and_sets_dirty_when_viewing() {
        use ferrolite_catalog::{FileKind, NewImage};
        let mut s = AppState::for_test();

        // Create a folder, two images, and a collection.
        let (coll_id, img_a, img_b) = {
            let w = s.writer.lock().unwrap();
            let folder = w.upsert_folder(std::path::Path::new("/p"), None).unwrap();
            let a = w
                .upsert_image(&NewImage::failed(
                    folder,
                    "a.jpg".into(),
                    1,
                    1,
                    FileKind::Standard,
                    0,
                ))
                .unwrap();
            let b = w
                .upsert_image(&NewImage::failed(
                    folder,
                    "b.jpg".into(),
                    1,
                    1,
                    FileKind::Standard,
                    0,
                ))
                .unwrap();
            let c = w
                .create_collection("test-col", ferrolite_image::Color::default())
                .unwrap();
            (c, a, b)
        };

        // Select both images.
        s.selection = [img_a, img_b].into_iter().collect();
        s.dirty = false;
        // Not currently viewing the collection — dirty must stay false.
        s.source = ViewSource::All;
        s.add_selection_to_collection(coll_id);
        assert!(
            !s.dirty,
            "dirty stays false when not viewing the collection"
        );

        // Verify images are in the collection via the read pool.
        s.reload_vocab();
        s.source = ViewSource::Collection(coll_id);
        s.refresh_images();
        assert_eq!(s.images.len(), 2, "both images should be in the collection");

        // Re-run while viewing the collection: dirty must be set.
        s.dirty = false;
        s.source = ViewSource::Collection(coll_id);
        s.add_selection_to_collection(coll_id);
        assert!(
            s.dirty,
            "dirty set when currently viewing the target collection"
        );
    }

    /// `selection_anchor` is initialised to `None` in both constructors.
    #[test]
    fn selection_anchor_initialised_none() {
        let s = AppState::for_test();
        assert!(s.selection_anchor.is_none());
    }

    #[test]
    fn apply_metadata_edit_to_image_targets_only_that_image() {
        use ferrolite_catalog::{DecodeStatus, FileKind};
        use ferrolite_image::{Flag, Orientation, Rating};
        let mut s = AppState::for_test();
        let ctx = egui::Context::default();
        let mk = |id: i64| ferrolite_catalog::ImageRecord {
            id,
            folder_id: 99,
            filename: format!("img{id}.nef"),
            width: None,
            height: None,
            orientation: Orientation::Normal,
            capture_time: None,
            iso: None,
            decode_status: DecodeStatus::Done,
            kind: FileKind::Raw,
            rating: Rating::default(),
            flag: Flag::None,
        };
        s.images = vec![mk(1), mk(2)];
        // Selection is image 2, but we edit image 1 explicitly.
        s.selection = [2].into_iter().collect();
        s.selected = Some(2);

        s.apply_metadata_edit_to_image(
            &ctx,
            1,
            crate::metadata::MetaEdit::SetRating(Rating::new(4)),
        );

        let r1 = s.images.iter().find(|r| r.id == 1).unwrap().rating;
        let r2 = s.images.iter().find(|r| r.id == 2).unwrap().rating;
        assert_eq!(r1, Rating::new(4), "explicit target updated");
        assert_eq!(r2, Rating::default(), "selection NOT touched");
    }
}

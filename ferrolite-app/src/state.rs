//! Application state: catalog handles, the job system, the event channel, and
//! the currently-browsed folder's rows + selection + progress counters.

use crate::events::AppEvent;
use crate::library::filter::{FilterState, ViewSource};
use crate::metadata::MetaEdit;
use ferrolite_catalog::{
    Catalog, CollectionRecord, ImageRecord, LibraryQuery, ReadPool, SortKey, TagRecord,
};
use ferrolite_image::TagId;
use ferrolite_jobs::{JobHandle, JobSystem};
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

    /// Stat-only placeholder rows inserted by the instant index pass (Phase A).
    pub scanned: u64,
    pub indexed: u64,
    /// Total files the current/last ingest pass will process (set once per pass
    /// via `IngestPlanned`, after the needs-reingest filter). Denominator for the
    /// status-bar ingest-progress readout.
    pub ingest_total: usize,
    /// Files processed so far by the current/last ingest pass. Advanced per
    /// consumer row (`Indexed`), which in the inline-thumbnail model equals one
    /// decoded file + generated thumbnail.
    pub ingest_done: usize,

    pub ingest_handle: Option<JobHandle>,

    /// LRU cache of decoded thumbnail textures (cap 512).
    pub textures: crate::library::texture_cache::TextureCache,
    /// Session-only CPU cache of decoded thumbnail pixels so re-revealed cells
    /// re-upload without a new job / DB read / JPEG decode (Bug B).
    pub thumb_pixels: crate::library::thumb_pixel_cache::ThumbPixelCache,

    /// Image ids with an in-flight off-thread thumbnail decode (lazy-load path).
    /// Dedups repeated `request_thumbnail` calls while the job is running;
    /// cleared on `ThumbReady`/`ThumbFailed`/`ThumbMissing`.
    pub thumb_pending: HashSet<i64>,
    /// In-flight lazy-load fetch handles, keyed by image_id. Lets the grid
    /// cancel off-screen fetches each frame (`retain_visible_thumbnail_jobs`)
    /// and drain them at shutdown (`cancel_pending_jobs`) so a big scroll
    /// doesn't leave a stale backlog that blocks now-visible cells or stalls
    /// close.
    pub thumb_handles: HashMap<i64, JobHandle>,
    /// Image ids whose lazy-load job found no thumbnail blob yet (`Ok(None)`
    /// from `get_thumbnail`) — distinct from a hard decode failure. Sticky
    /// guard against a per-frame re-spawn storm: `request_thumbnail` skips ids
    /// in this set until ingest finishes (`IngestDone` clears it) or the
    /// thumbnail actually arrives (`ThumbReady` removes the id).
    pub thumb_missing: HashSet<i64>,
    /// Image ids whose decoded pixels are queued in `pending_uploads`, awaiting
    /// GPU upload — the lifecycle bridge between a finished job (`thumb_pending`
    /// cleared on `ThumbReady`) and a live texture (`textures`). Dedups
    /// `request_thumbnail` so a cell whose pixels are already queued does not
    /// re-submit a job or re-push another copy every frame. Invariant: this set
    /// equals the ids currently in `pending_uploads`.
    pub thumb_uploading: HashSet<i64>,
    /// Decoded thumbnails pulled from the event channel but not yet uploaded this
    /// frame (per-frame upload cap overflow). Drained first each frame.
    pub pending_uploads: Vec<(i64, Vec<u8>, u32, u32)>,

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

    /// App-global egui native texture id for the Develop mask overlay (GPU-tinted;
    /// no readback). Registered once, updated in place for whichever viewer is
    /// active — a single reused texture, so no per-image free is needed.
    pub mask_overlay_native: Option<egui::TextureId>,
    /// Keeps the current overlay `OverlayTexture` alive while egui's bind group
    /// references it. Replaced on each overlay rebuild.
    pub mask_overlay_gpu: Option<ferrolite_pipeline::OverlayTexture>,
    /// App-global egui native texture id for the white hover-highlight overlay
    /// (a single component's coverage, tinted white). Registered once, updated
    /// in place — mirrors `mask_overlay_native`. Stale (not drawn) whenever no
    /// component is hovered (`MaskUiState::highlight_component == None`).
    pub mask_overlay_highlight_native: Option<egui::TextureId>,
    /// Keeps the current highlight `OverlayTexture` alive while egui's bind
    /// group references it. Replaced on each highlight rebuild.
    pub mask_overlay_highlight_gpu: Option<ferrolite_pipeline::OverlayTexture>,

    /// Non-None while the single-image viewer is open.
    pub viewer: Option<crate::viewer::ViewerState>,

    /// Develop tool/tab selection state (design §5). Session-wide (unlike the
    /// per-image fields on `ViewerState`) so switching images keeps the same
    /// tool/tab active; `ensure_valid_tab` re-validates it against the new
    /// image's registry after each load.
    pub tool_state: crate::develop::tool_state::ToolState,

    /// The single-file export dialog, `Some` while the format+options popup is
    /// open (spec §8.3).
    pub export_dialog: Option<crate::export::ExportDialogState>,

    /// Live export activity (single or batch); `None` when no export has run this
    /// session. Drives the status-bar indicator and the Export module's batch UI.
    pub export_activity: Option<crate::export::ExportActivity>,

    /// Persisted export queue: ordered image_ids. Authoritative in-memory copy
    /// (the DB table is a cache — its loss never loses photos). Loaded at startup.
    pub export_queue: Vec<i64>,
    /// Shared batch export settings (spec §8.2).
    pub export_settings: ferrolite_export::ExportOptions,
    /// Batch destination folder (spec §8.4). `None` until picked.
    pub export_dest: Option<std::path::PathBuf>,
    /// Filename token template (spec §8.4). Default "{name}".
    pub export_template: String,
    /// Whether the filename-template token help modal is open.
    pub export_help_open: bool,

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
    /// Per-image collection membership cached for the currently visible grid cells.
    pub visible_collections: HashMap<i64, Vec<i64>>,
    /// Selected image ids (multi-selection for batch ops).
    pub selection: HashSet<i64>,
    /// The anchor image id for shift-click range selection.
    pub selection_anchor: Option<i64>,
    /// General-purpose in-app notifications (toasts). See `notifications` module.
    pub notifications: crate::notifications::Notifications,
    /// Image ids queued by the "Regenerate thumbnail" context-menu action,
    /// drained in `update()` where the GPU render state is available.
    pub pending_thumb_regen: Vec<i64>,

    /// Inline rename in progress: (kind, id, edit buffer).
    /// Set on double-click or "Rename" context-menu; cleared on Enter/blur.
    pub renaming: Option<(RenameKind, i64, String)>,

    /// Number of ops-persist jobs currently in flight (incremented before
    /// `spawn_ops_write`, decremented on `OpsSaved`). Drives the save-state indicator.
    pub ops_save_inflight: usize,
    /// Set to `true` when the most recent ops-persist completed with `ok=false`.
    /// Cleared on the next successful save. Drives the "Save failed" indicator.
    pub ops_save_failed: bool,

    // ── Cached toolbar metadata-filter aggregates (populated by reload_vocab) ──
    /// Distinct camera-model strings from the catalog.
    pub camera_options: Vec<String>,
    /// (min, max) ISO across the catalog, or None if no EXIF ISO is indexed.
    pub iso_range: Option<(u32, u32)>,
    /// (earliest, latest) capture-date strings from the catalog, or None.
    pub date_range: Option<(String, String)>,

    /// Bumped every time `images` is reassigned, so the grid's justified-layout
    /// cache knows when to rebuild (covers streaming ingest, filter, folder
    /// switch, and in-place edits — all funnel through `refresh_images`).
    pub images_rev: u64,
    /// Cached justified-rows layout, rebuilt only when its inputs change.
    pub grid_layout: Option<crate::library::grid_layout::CachedGridLayout>,

    /// Editing working space (spec §4.1, default Rec.2020). Global preference; the
    /// ColorMatrixNode + display tail are recomposed on change.
    pub working_space: ferrolite_color::WorkingSpace,

    /// On-disk cache of downscaled, color-managed RAW previews (sits next to
    /// `catalog.db`). Shared into `Background` write-back jobs via `Arc`.
    pub preview_store: Arc<ferrolite_previews::PreviewStore>,

    /// Persisted user preferences (keybindings, export options, filter,
    /// working space, etc.). Loaded at startup from `settings.json`; edits
    /// must call `FerroliteApp::mark_settings_dirty()` so they persist (see
    /// `crate::settings::persist`).
    pub settings: crate::settings::Settings,

    /// Resolved display-profile name for the Settings label ("sRGB (default)" when off).
    #[allow(dead_code)] // read by the Settings UI + display-profile detect flow (Unit 5)
    pub display_profile_name: String,
    /// Monotonic generation; each re-detect bumps it. Stale job results are dropped.
    #[allow(dead_code)] // guards stale detect-job results (Unit 5)
    pub display_detect_gen: u64,
    /// The last resolved monitor LUT (`None` = sRGB/fallback). Re-applied on every
    /// image reveal so opening an image never reverts the display to analytic sRGB.
    pub display_lut: Option<ferrolite_color::DisplayLut>,
    /// The monitor key the window was last seen on (0 = unknown / unsupported OS).
    #[allow(dead_code)] // compared on window-move to trigger re-detect (Unit 5)
    pub last_monitor_key: u64,

    /// The shared Lensfun DB handle (Spec 4.4), loaded ONCE at startup via
    /// `develop::lens_match::load_shared_db` — never per-image/per-frame
    /// (CLAUDE.md rule 1). `None` when the bundled DB failed to load; the
    /// lens-correction section (auto-match + manual picker + bake) is then
    /// disabled rather than retried on every open.
    pub lens_db: Option<Arc<ferrolite_lens::LensfunDb>>,
}

/// CPU thumbnail-pixel cache capacity. ≤256px RGBA8 ≈ 256 KB each → ~256 MB
/// worst case at this cap; covers many screens of scroll on large libraries.
const THUMB_PIXEL_CACHE_CAP: usize = 1024;

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
        let settings = crate::settings::persist::load();
        Ok(Self {
            jobs: Arc::new(JobSystem::new(workers)),
            writer: Arc::new(Mutex::new(writer)),
            reads: Arc::new(reads),
            tx,
            rx,
            current_folder: None,
            images: Vec::new(),
            selected: None,
            scanned: 0,
            indexed: 0,
            ingest_total: 0,
            ingest_done: 0,
            ingest_handle: None,
            textures: crate::library::texture_cache::TextureCache::new(512),
            thumb_pixels: crate::library::thumb_pixel_cache::ThumbPixelCache::new(
                THUMB_PIXEL_CACHE_CAP,
            ),
            thumb_pending: HashSet::new(),
            thumb_handles: HashMap::new(),
            thumb_missing: HashSet::new(),
            thumb_uploading: HashSet::new(),
            pending_uploads: Vec::new(),
            dirty: true,
            active_ingests: 0,
            last_watch_check: None,
            startup_rescan_done: false,
            include_subfolders: settings.filter.include_subfolders,
            expanded_folders: HashSet::new(),
            pending_remove: None,
            mask_overlay_native: None,
            mask_overlay_gpu: None,
            mask_overlay_highlight_native: None,
            mask_overlay_highlight_gpu: None,
            viewer: None,
            tool_state: Default::default(),
            export_dialog: None,
            export_activity: None,
            export_queue: Vec::new(),
            export_settings: settings.export.to_options(),
            export_dest: None,
            export_template: "{name}".to_string(),
            export_help_open: false,
            filter: settings.filter.apply_to(FilterState::default()),
            source: ViewSource::All,
            tags: Vec::new(),
            collections: Vec::new(),
            visible_tags: HashMap::new(),
            visible_collections: HashMap::new(),
            selection: HashSet::new(),
            selection_anchor: None,
            notifications: crate::notifications::Notifications::default(),
            pending_thumb_regen: Vec::new(),
            camera_options: Vec::new(),
            iso_range: None,
            date_range: None,
            renaming: None,
            ops_save_inflight: 0,
            ops_save_failed: false,
            images_rev: 0,
            grid_layout: None,
            working_space: settings.working_space.to_ws(),
            preview_store: Arc::new(open_preview_store(&default_previews_dir())),
            settings,
            display_profile_name: "sRGB (default)".to_string(),
            display_detect_gen: 0,
            display_lut: None,
            last_monitor_key: 0,
            lens_db: crate::develop::lens_match::load_shared_db(),
        })
    }

    /// Upload already-decoded RGBA8 pixels as an egui texture into the cache.
    /// NO JPEG decode happens here — the pixels arrive pre-decoded from a job
    /// thread (both the generation and lazy-load paths decode off the UI thread).
    pub fn upload_thumbnail(
        &mut self,
        ctx: &egui::Context,
        image_id: i64,
        rgba: Vec<u8>,
        w: u32,
        h: u32,
    ) {
        // The pixels are leaving the awaiting-upload stage regardless of whether
        // they upload successfully — always clear the guard so a malformed buffer
        // can never strand an id in `thumb_uploading` (which would stop its cell
        // from ever being re-requested). Harmless no-op for ids uploaded inline
        // that were never queued.
        self.thumb_uploading.remove(&image_id);
        // Guard against a malformed buffer so `from_rgba_unmultiplied` never
        // panics on a length mismatch.
        if rgba.len() != (w as usize) * (h as usize) * 4 {
            return;
        }
        self.thumb_pixels.insert(image_id, rgba.clone(), w, h);
        let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        let tex = ctx.load_texture(
            format!("thumb-{image_id}"),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.textures.insert(image_id, tex);
    }

    /// Request a thumbnail for a visible cell WITHOUT blocking the UI thread.
    /// Dedups against already-textured ids and in-flight requests. On a miss,
    /// spawns a `Visible`-priority job that reads the DB blob and decodes the
    /// JPEG → RGBA8 OFF the UI thread, then delivers `ThumbReady` (or
    /// `ThumbFailed`) over the app event channel.
    pub fn request_thumbnail(&mut self, ctx: &egui::Context, image_id: i64) {
        let textured = self.textures.contains(image_id);
        let pending = self.thumb_pending.contains(&image_id);
        let missing = self.thumb_missing.contains(&image_id);
        let uploading = self.thumb_uploading.contains(&image_id);
        if textured || pending || missing || uploading {
            crate::diag::record_request(crate::diag::classify_request(
                textured, pending, missing, uploading,
            ));
            return;
        }
        // Fast path: pixels already decoded this session → re-upload directly,
        // no job / DB read / JPEG decode (Bug B). Routed through the same
        // per-frame upload budget as ThumbReady via `pending_uploads`. Mark the
        // id `thumb_uploading` so a repeat request while it waits in the queue
        // does not push another copy (re-submit storm guard).
        if let Some((rgba, w, h)) = self.thumb_pixels.get(image_id) {
            crate::diag::record_request(crate::diag::ReqOutcome::FastPath);
            self.thumb_uploading.insert(image_id);
            self.pending_uploads.push((image_id, rgba, w, h));
            ctx.request_repaint();
            return;
        }
        crate::diag::record_request(crate::diag::ReqOutcome::NewSubmit);
        self.thumb_pending.insert(image_id);
        let reads = Arc::clone(&self.reads);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let handle = self
            .jobs
            .submit(ferrolite_jobs::Priority::Visible, move |cancel| {
                if cancel.is_cancelled() {
                    let _ = tx.send(AppEvent::ThumbFailed { image_id });
                    ctx.request_repaint();
                    return;
                }
                match reads.get_thumbnail(image_id) {
                    Ok(Some(thumb)) => match image::load_from_memory(&thumb.bytes) {
                        Ok(img) => {
                            let rgba = img.to_rgba8();
                            let (w, h) = (rgba.width(), rgba.height());
                            let _ = tx.send(AppEvent::ThumbReady {
                                image_id,
                                rgba: rgba.into_raw(),
                                w,
                                h,
                            });
                        }
                        Err(_) => {
                            let _ = tx.send(AppEvent::ThumbFailed { image_id });
                        }
                    },
                    // No blob yet (e.g. a status race) — distinct from a hard
                    // decode failure so the sticky `thumb_missing` guard (not
                    // `Failed`-cell UI) applies and this id stops re-spawning a
                    // job every frame.
                    Ok(None) => {
                        let _ = tx.send(AppEvent::ThumbMissing { image_id });
                    }
                    Err(_) => {
                        let _ = tx.send(AppEvent::ThumbFailed { image_id });
                    }
                }
                ctx.request_repaint();
            });
        self.thumb_handles.insert(image_id, handle);
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

    /// Add `tag_id` to every image in `ids` that doesn't already have it
    /// (add-only; reuses the toggle path so persistence is unchanged).
    pub fn add_tag_to_images(&mut self, ctx: &egui::Context, ids: &[i64], tag_id: TagId) {
        let missing = ids_missing_tag(ids, tag_id, &self.visible_tags);
        if missing.is_empty() {
            return;
        }
        self.apply_metadata_edit_to_ids(ctx, &missing, MetaEdit::ToggleTag(tag_id));
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

    /// Fetch collection membership for any visible image ids not yet cached
    /// (virtualised). Mirrors `ensure_tags_for`'s off-thread read-pool path so
    /// the UI thread never blocks on the catalog.
    pub fn ensure_collections_for(&mut self, ids: &HashSet<i64>) {
        let missing: Vec<i64> = ids
            .iter()
            .copied()
            .filter(|id| !self.visible_collections.contains_key(id))
            .collect();
        if missing.is_empty() {
            return;
        }
        if let Ok(map) = self.reads.collections_for_images(&missing) {
            for id in missing {
                self.visible_collections
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
        // Bump the layout revision so the grid rebuilds its justified layout for
        // the new set, and invalidate the per-cell tag cache so it re-fetches.
        self.images_rev = self.images_rev.wrapping_add(1);
        self.visible_tags.clear();
        self.visible_collections.clear();
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
            // Keep the current selection in sync with the viewed image so the
            // bottom status bar (filename · dims · ISO, driven by `selected`)
            // updates on Develop filmstrip navigation, not just library clicks.
            self.selected = Some(rec.id);
        }
    }

    /// Absolute path of an image: its folder path + filename. `None` if the
    /// folder can't be resolved. Mirrors `open_image_in_viewer`'s path build.
    pub fn image_path(&self, rec: &ferrolite_catalog::ImageRecord) -> Option<PathBuf> {
        self.reads
            .folder_path(rec.folder_id)
            .ok()
            .flatten()
            .map(|fp| PathBuf::from(fp).join(&rec.filename))
    }

    /// Cancel any in-flight ingest job, without touching the view
    /// (images/current_folder/selection) or counters. Used by reindex.
    /// Thumbnails are now generated inline within the ingest job (no separate
    /// per-image jobs to cancel), so cancelling the ingest handle is sufficient.
    pub fn cancel_pending_jobs(&mut self) {
        if let Some(h) = self.ingest_handle.take() {
            h.cancel();
            // A queued-but-not-yet-dispatched job is skipped by the worker and
            // never emits IngestDone, so decrement here to keep the counter
            // balanced. If the job was already running it will still emit
            // IngestDone; the extra decrement is absorbed by saturating_sub.
            self.active_ingests = self.active_ingests.saturating_sub(1);
        }
        // Drain and cancel every in-flight lazy-load thumbnail fetch too, so a
        // close right after a big scroll doesn't wait on a backlog of `Visible`
        // jobs (`on_exit` calls this fn).
        for (_id, handle) in self.thumb_handles.drain() {
            self.jobs.cancel(handle.id());
            handle.cancel();
        }
        self.thumb_pending.clear();
        // Drop any decoded-but-not-yet-uploaded thumbnails and their guard so a
        // folder switch / shutdown leaves no stale upload queue (and keeps the
        // `thumb_uploading == pending_uploads ids` invariant).
        self.pending_uploads.clear();
        self.thumb_uploading.clear();
    }

    /// Cancel and drop lazy-load thumbnail fetches whose cells are no longer
    /// visible, so a big scroll doesn't leave a stale backlog that blocks the
    /// now-visible cells (and saturates the UI at close). Cancelled ids are
    /// removed from the in-flight guards so they can be re-requested if scrolled
    /// back into view; they are NOT marked missing.
    pub fn retain_visible_thumbnail_jobs(&mut self, visible: &HashSet<i64>) {
        let offscreen: Vec<i64> = self
            .thumb_handles
            .keys()
            .copied()
            .filter(|id| !visible.contains(id))
            .collect();
        crate::diag::retain_cancels(offscreen.len());
        for id in offscreen {
            if let Some(handle) = self.thumb_handles.remove(&id) {
                self.jobs.cancel(handle.id()); // drop it from the queue if still pending
                handle.cancel(); // signal it if already running
            }
            self.thumb_pending.remove(&id);
        }
    }

    /// Zero the four scan/ingest progress counters (`scanned`, `indexed`,
    /// `ingest_total`, `ingest_done`). Shared by `reset_for_new_folder` (folder
    /// switch) and the start-of-wave reset in `submit_ingest` (active_ingests
    /// 0→1), so both call sites stay in sync as the counter set evolves.
    pub fn reset_ingest_counters(&mut self) {
        self.scanned = 0;
        self.indexed = 0;
        self.ingest_total = 0;
        self.ingest_done = 0;
    }

    /// Push a toast from UI-thread code. Job threads instead send
    /// `AppEvent::Notify` over the event channel.
    pub fn notify(&mut self, level: crate::notifications::Level, message: impl Into<String>) {
        self.notifications
            .push(level, message, std::time::Instant::now());
    }

    /// Reset per-folder job + counter state when switching folders.
    pub fn reset_for_new_folder(&mut self) {
        self.cancel_pending_jobs();
        self.reset_ingest_counters();
        self.thumb_missing.clear();
        self.images.clear();
        // Bump so the grid's layout cache rebuilds for the now-empty set instead
        // of indexing the previous folder's rows (stale-index panic otherwise).
        self.images_rev = self.images_rev.wrapping_add(1);
        self.selected = None;
        self.dirty = true;
    }

    /// Switch the browsed folder (from the folder list) and reset state.
    ///
    /// Sets the view sort to `added_at DESC` (newest-added first) so freshly
    /// ingested thumbnails appear at the top of the grid, where the user is
    /// watching, rather than wherever their `CaptureTime` happens to land.
    /// This is a deliberate view-state change on open, not a dynamic re-sort
    /// keyed on ingest activity — it persists until the user changes it, and
    /// leaves `FilterState::default()` (CaptureTime ASC) unchanged for every
    /// other entry point (`All`, `Collection`, `RecentlyAdded`, app startup).
    pub fn select_folder(&mut self, folder_id: i64) {
        self.reset_for_new_folder();
        self.current_folder = Some(folder_id);
        self.source = ViewSource::Folder(folder_id);
        self.filter.sort_key = SortKey::AddedAt;
        self.filter.sort_desc = true;
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

    /// Load the persisted export queue (spec §8.4). Cache contract: on DB error
    /// keep an empty in-memory queue and surface a warning; never panic.
    pub fn load_export_queue(&mut self) {
        match self.reads.list_export_queue() {
            Ok(ids) => self.export_queue = ids,
            Err(e) => {
                eprintln!("ferrolite: export queue load failed: {e}");
                self.export_queue = Vec::new();
                self.notify(
                    crate::notifications::Level::Warning,
                    "Could not load export queue.",
                );
            }
        }
    }

    fn now_unix() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// Persist a queue write; on error surface a warning but keep the in-memory
    /// queue authoritative (cache contract §5).
    fn persist_queue<F>(&mut self, op: F)
    where
        F: FnOnce(&ferrolite_catalog::Catalog) -> Result<(), ferrolite_catalog::CatalogError>,
    {
        let failed = if let Ok(cat) = self.writer.lock() {
            if let Err(e) = op(&cat) {
                eprintln!("ferrolite: export queue persist failed: {e}");
                true
            } else {
                false
            }
        } else {
            false
        };
        if failed {
            self.notify(
                crate::notifications::Level::Warning,
                "Export queue not saved (kept for this session).",
            );
        }
    }

    /// Add `image_id` to the export queue (dedup, in-memory authoritative), then
    /// persist to the catalog cache table.
    pub fn queue_add(&mut self, image_id: i64) {
        if self.export_queue.contains(&image_id) {
            return;
        }
        self.export_queue.push(image_id);
        let at = Self::now_unix();
        self.persist_queue(|cat| cat.add_to_export_queue(image_id, at));
    }

    /// Add several image ids to the export queue in order (dedup per-id).
    pub fn queue_add_many(&mut self, ids: &[i64]) {
        for &id in ids {
            self.queue_add(id);
        }
    }

    /// Remove `image_id` from the export queue and persist the change.
    pub fn queue_remove(&mut self, image_id: i64) {
        self.export_queue.retain(|&id| id != image_id);
        self.persist_queue(|cat| cat.remove_from_export_queue(image_id));
    }

    /// Whether `image_id` is currently in the export queue.
    pub fn queue_contains(&self, image_id: i64) -> bool {
        self.export_queue.contains(&image_id)
    }

    /// Toggle membership: remove if present, else add. Persists (cache-safe).
    pub fn queue_toggle(&mut self, image_id: i64) {
        if self.queue_contains(image_id) {
            self.queue_remove(image_id);
        } else {
            self.queue_add(image_id);
        }
    }

    /// Empty the export queue and persist the change.
    pub fn queue_clear(&mut self) {
        self.export_queue.clear();
        self.persist_queue(|cat| cat.clear_export_queue());
    }

    /// Move the queued item at `from` to insertion index `insert_at` (0..=len,
    /// as returned by the grid drop-index math), then persist the new order.
    /// Cache-safe (persist errors are swallowed to a warning by persist_queue).
    pub fn queue_reorder(&mut self, from: usize, insert_at: usize) {
        if from >= self.export_queue.len() {
            return;
        }
        let id = self.export_queue.remove(from);
        // After removing `from`, an insert index past it shifts left by one.
        let dest = if insert_at > from {
            insert_at - 1
        } else {
            insert_at
        };
        let dest = dest.min(self.export_queue.len());
        self.export_queue.insert(dest, id);
        let ordered = self.export_queue.clone();
        self.persist_queue(|cat| cat.reorder_export_queue(&ordered));
    }

    /// True while a BATCH export is running. Single export does not lock the
    /// Export-module queue, so queue gates check this, not merely "an export runs".
    pub fn batch_running(&self) -> bool {
        self.export_activity
            .as_ref()
            .is_some_and(|a| a.kind == crate::export::ExportKind::Batch && !a.is_done())
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
            scanned: 0,
            indexed: 0,
            ingest_total: 0,
            ingest_done: 0,
            ingest_handle: None,
            textures: crate::library::texture_cache::TextureCache::new(512),
            thumb_pixels: crate::library::thumb_pixel_cache::ThumbPixelCache::new(
                THUMB_PIXEL_CACHE_CAP,
            ),
            thumb_pending: HashSet::new(),
            thumb_handles: HashMap::new(),
            thumb_missing: HashSet::new(),
            thumb_uploading: HashSet::new(),
            pending_uploads: Vec::new(),
            dirty: true,
            active_ingests: 0,
            last_watch_check: None,
            startup_rescan_done: false,
            include_subfolders: true,
            expanded_folders: HashSet::new(),
            pending_remove: None,
            mask_overlay_native: None,
            mask_overlay_gpu: None,
            mask_overlay_highlight_native: None,
            mask_overlay_highlight_gpu: None,
            viewer: None,
            tool_state: Default::default(),
            export_dialog: None,
            export_activity: None,
            export_queue: Vec::new(),
            export_settings: ferrolite_export::ExportOptions::default(),
            export_dest: None,
            export_template: "{name}".to_string(),
            export_help_open: false,
            filter: FilterState::default(),
            source: ViewSource::All,
            tags: Vec::new(),
            collections: Vec::new(),
            visible_tags: HashMap::new(),
            visible_collections: HashMap::new(),
            selection: HashSet::new(),
            selection_anchor: None,
            notifications: crate::notifications::Notifications::default(),
            pending_thumb_regen: Vec::new(),
            camera_options: Vec::new(),
            iso_range: None,
            date_range: None,
            renaming: None,
            ops_save_inflight: 0,
            ops_save_failed: false,
            images_rev: 0,
            grid_layout: None,
            working_space: ferrolite_color::WorkingSpace::default(),
            preview_store: Arc::new(open_preview_store(&std::env::temp_dir().join(format!(
                "ferrolite-previews-test-{}-{}",
                std::process::id(),
                tid
            )))),
            settings: crate::settings::Settings::default(),
            display_profile_name: "sRGB (default)".to_string(),
            display_detect_gen: 0,
            display_lut: None,
            last_monitor_key: 0,
            // Skip the bundled-DB load in unit tests (unnecessary I/O per test;
            // no test in this module exercises lens matching/baking).
            lens_db: None,
        }
    }

    /// Toggle select-all over the current (already-filtered) grid rows.
    ///
    /// If `images` is non-empty and every row is already selected, clear the
    /// selection; otherwise select every row. Leaves the single `selected`
    /// cursor (status bar / Enter-to-open) untouched.
    pub fn toggle_select_all(&mut self) {
        let all_selected =
            !self.images.is_empty() && self.images.iter().all(|r| self.selection.contains(&r.id));
        if all_selected {
            self.selection.clear();
            self.selection_anchor = None;
        } else {
            self.selection = self.images.iter().map(|r| r.id).collect();
            self.selection_anchor = self.images.first().map(|r| r.id);
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
        // Optimistic cache update: a just-added collection immediately drops
        // out of the "Add" submenu and appears in the "Remove" submenu.
        for id in ids {
            let entry = self.visible_collections.entry(*id).or_default();
            if !entry.contains(&coll_id) {
                entry.push(coll_id);
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

    /// Remove all selected images (or the single `selected` fallback) from a collection.
    pub fn remove_selection_from_collection(&mut self, coll_id: i64) {
        let mut targets: Vec<i64> = self.selection.iter().copied().collect();
        if targets.is_empty() {
            if let Some(id) = self.selected {
                targets.push(id);
            }
        }
        self.remove_images_from_collection(&targets, coll_id);
    }

    /// Shared core: remove every id from the collection; refresh if viewing it.
    pub fn remove_images_from_collection(&mut self, ids: &[i64], coll_id: i64) {
        if ids.is_empty() {
            return;
        }
        {
            let w = self.writer.lock().expect("writer");
            for id in ids {
                let _ = w.remove_image_from_collection(coll_id, *id);
            }
        }
        for id in ids {
            if let Some(v) = self.visible_collections.get_mut(id) {
                v.retain(|c| *c != coll_id);
            }
        }
        if matches!(self.source, ViewSource::Collection(id) if id == coll_id) {
            self.dirty = true;
        }
    }

    /// Remove a single explicit image from a collection (used by Develop/viewer).
    pub fn remove_image_from_collection_now(&mut self, image_id: i64, coll_id: i64) {
        self.remove_images_from_collection(&[image_id], coll_id);
    }
}

/// Images (in input order) that do NOT already carry `tag_id`. Images absent
/// from `visible_tags` are treated as missing the tag (so they get it).
pub(crate) fn ids_missing_tag(
    ids: &[i64],
    tag_id: TagId,
    visible_tags: &HashMap<i64, Vec<TagId>>,
) -> Vec<i64> {
    ids.iter()
        .copied()
        .filter(|id| {
            visible_tags
                .get(id)
                .map(|tags| !tags.contains(&tag_id))
                .unwrap_or(true)
        })
        .collect()
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

/// Cache dir for downscaled RAW previews, next to `catalog.db` (same base
/// logic). `PreviewStore::new` creates it.
fn default_previews_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ferrolite").join("previews")
}

/// Open a `PreviewStore` at `dir`, never aborting startup. `PreviewStore::new`
/// only fails if `create_dir_all` fails; on that (rare) error, fall back to a
/// store rooted under the OS temp dir so the app still runs and the cache is
/// simply best-effort. The final `expect` is on the temp dir, which is
/// writable in every environment this app runs in.
fn open_preview_store(dir: &std::path::Path) -> ferrolite_previews::PreviewStore {
    ferrolite_previews::PreviewStore::new(dir).unwrap_or_else(|_| {
        ferrolite_previews::PreviewStore::new(&std::env::temp_dir().join("ferrolite-previews"))
            .expect("temp previews dir is creatable")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A freshly constructed state must have no monitor LUT resolved yet, so the
    /// display tail starts on the analytic sRGB path until a detect resolves.
    #[test]
    fn display_lut_defaults_to_none() {
        let s = AppState::for_test();
        assert!(s.display_lut.is_none());
    }

    /// `reset_ingest_counters` must zero exactly the four scan/ingest
    /// progress counters, independent of any other state. This is the shared
    /// helper both `reset_for_new_folder` (folder switch) and `submit_ingest`
    /// (start of a new ingest wave, `active_ingests` 0→1) call.
    #[test]
    fn reset_ingest_counters_zeroes_all_four_fields() {
        let mut s = AppState::for_test();
        s.scanned = 56440;
        s.indexed = 3320;
        s.ingest_total = 3320;
        s.ingest_done = 3320;

        s.reset_ingest_counters();

        assert_eq!(s.scanned, 0, "scanned must be zeroed");
        assert_eq!(s.indexed, 0, "indexed must be zeroed");
        assert_eq!(s.ingest_total, 0, "ingest_total must be zeroed");
        assert_eq!(s.ingest_done, 0, "ingest_done must be zeroed");
    }

    /// `reset_for_new_folder` must zero all per-folder counters, cancel the
    /// ingest handle, clear `images`, clear `selected`, and set the dirty flag.
    #[test]
    fn reset_for_new_folder_zeroes_counters_and_clears_jobs() {
        let mut s = AppState::for_test();
        // Seed some prior state.
        s.indexed = 42;
        s.ingest_total = 10;
        s.ingest_done = 7;
        s.selected = Some(1);
        s.dirty = false; // simulate an idle frame that already cleared the flag
        let rev_before = s.images_rev;

        s.reset_for_new_folder();

        assert_eq!(s.ingest_total, 0, "ingest_total must be zeroed");
        assert_eq!(s.ingest_done, 0, "ingest_done must be zeroed");
        assert_eq!(s.indexed, 0, "indexed must be zeroed");
        assert!(s.images.is_empty(), "images must be cleared");
        assert_eq!(s.selected, None, "selected must be cleared");
        assert!(s.dirty, "dirty flag must be set after reset");
        assert_ne!(
            s.images_rev, rev_before,
            "images_rev must bump so the grid layout cache rebuilds for the empty set"
        );
    }

    /// A folder switch must also clear `thumb_missing` — a sticky-missing id
    /// from the previous folder must not suppress a lazy-load request for the
    /// (unrelated) image with the same id under the new folder view.
    #[test]
    fn reset_for_new_folder_clears_thumb_missing() {
        let mut s = AppState::for_test();
        s.thumb_missing.insert(5);

        s.reset_for_new_folder();

        assert!(
            s.thumb_missing.is_empty(),
            "thumb_missing must be cleared on folder switch"
        );
    }

    /// `select_folder` must delegate to `reset_for_new_folder` and then set the
    /// new `current_folder`.
    #[test]
    fn select_folder_resets_and_sets_folder() {
        let mut s = AppState::for_test();
        s.current_folder = Some(99);
        s.ingest_total = 5;
        s.ingest_done = 3;
        s.dirty = false;

        s.select_folder(42);

        assert_eq!(s.current_folder, Some(42));
        assert_eq!(s.ingest_total, 0);
        assert_eq!(s.ingest_done, 0);
        assert!(s.dirty);
    }

    /// Opening a folder must set the view sort to `added_at DESC` (newest
    /// first) so freshly ingested thumbnails surface where the user is
    /// looking, without touching `FilterState::default()` for other views.
    #[test]
    fn select_folder_sorts_by_added_at_desc() {
        let mut s = AppState::for_test();
        assert_eq!(
            FilterState::default().sort_key,
            SortKey::CaptureTime,
            "global default sort must remain CaptureTime"
        );
        assert!(!FilterState::default().sort_desc);

        s.select_folder(42);

        assert_eq!(s.filter.sort_key, SortKey::AddedAt);
        assert!(s.filter.sort_desc);
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
    fn cancel_pending_jobs_keeps_view_and_counters() {
        let mut s = AppState::for_test();
        s.current_folder = Some(7);
        s.images = vec![]; // (kept as-is; view not cleared)
        s.selected = Some(3);
        s.indexed = 5;
        s.ingest_total = 8;
        s.ingest_done = 5;

        s.cancel_pending_jobs();

        assert_eq!(s.current_folder, Some(7), "current folder preserved");
        assert_eq!(s.selected, Some(3), "selection preserved");
        assert_eq!(s.indexed, 5, "counters not zeroed by cancel_pending_jobs");
        assert_eq!(s.ingest_total, 8, "ingest_total not zeroed");
        assert_eq!(s.ingest_done, 5, "ingest_done not zeroed");
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

    /// Round 4: `retain_visible_thumbnail_jobs` must cancel + drop tracked
    /// lazy-load fetches for ids that scrolled off-screen, while leaving
    /// still-visible ids' handles and pending markers untouched.
    #[test]
    fn retain_visible_thumbnail_jobs_cancels_offscreen_only() {
        let mut s = AppState::for_test();
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        // Occupy the single worker so subsequently submitted jobs stay queued
        // (so `pending_count` reflects the cancellation below).
        s.jobs
            .submit(ferrolite_jobs::Priority::Background, move |_| {
                let _ = gate_rx.recv();
            });

        for id in [1_i64, 2, 3] {
            s.thumb_pending.insert(id);
            let handle = s
                .jobs
                .submit(ferrolite_jobs::Priority::Visible, |_cancel| {});
            s.thumb_handles.insert(id, handle);
        }
        let before_pending = s.jobs.pending_count();

        let visible: HashSet<i64> = [2_i64].into_iter().collect();
        s.retain_visible_thumbnail_jobs(&visible);

        assert!(
            !s.thumb_handles.contains_key(&1) && !s.thumb_handles.contains_key(&3),
            "offscreen ids removed from thumb_handles"
        );
        assert!(
            !s.thumb_pending.contains(&1) && !s.thumb_pending.contains(&3),
            "offscreen ids removed from thumb_pending"
        );
        assert!(
            !s.thumb_missing.contains(&1) && !s.thumb_missing.contains(&3),
            "offscreen ids must NOT be marked missing (must be re-requestable)"
        );
        assert!(
            s.thumb_handles.contains_key(&2),
            "still-visible id keeps its handle"
        );
        assert!(
            s.thumb_pending.contains(&2),
            "still-visible id stays marked pending"
        );
        assert!(
            s.jobs.pending_count() < before_pending,
            "cancelled offscreen jobs must be dropped from the queue"
        );

        let _ = gate_tx.send(()); // release the occupying job
    }

    /// `cancel_pending_jobs` (called by `on_exit`) must also drain and cancel
    /// every in-flight lazy-load handle so a close right after a big scroll
    /// doesn't wait on a backlog of `Visible` fetches.
    #[test]
    fn cancel_pending_jobs_drains_thumb_handles() {
        let mut s = AppState::for_test();
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        s.jobs
            .submit(ferrolite_jobs::Priority::Background, move |_| {
                let _ = gate_rx.recv();
            });

        for id in [1_i64, 2] {
            s.thumb_pending.insert(id);
            let handle = s
                .jobs
                .submit(ferrolite_jobs::Priority::Visible, |_cancel| {});
            s.thumb_handles.insert(id, handle);
        }
        let before_pending = s.jobs.pending_count();

        s.cancel_pending_jobs();

        assert!(s.thumb_handles.is_empty(), "all thumb handles drained");
        assert!(s.thumb_pending.is_empty(), "thumb_pending cleared");
        assert!(
            s.jobs.pending_count() < before_pending,
            "queued lazy-load jobs must be dropped from the queue at shutdown"
        );

        let _ = gate_tx.send(());
    }

    /// A cell whose pixels are already queued for upload (`thumb_uploading`)
    /// must NOT submit a new job or push another copy to `pending_uploads`.
    #[test]
    fn request_thumbnail_dedups_ids_awaiting_upload() {
        let mut s = AppState::for_test();
        let ctx = egui::Context::default();
        s.thumb_uploading.insert(42);
        let jobs_before = s.jobs.pending_count();
        let uploads_before = s.pending_uploads.len();

        s.request_thumbnail(&ctx, 42);

        assert_eq!(
            s.jobs.pending_count(),
            jobs_before,
            "no job submitted for an id already awaiting upload"
        );
        assert_eq!(
            s.pending_uploads.len(),
            uploads_before,
            "no extra pending_uploads push for an id already awaiting upload"
        );
        assert!(
            !s.thumb_pending.contains(&42),
            "awaiting-upload id must not enter thumb_pending"
        );
    }

    /// FastPath (pixels cached, texture absent) queues the upload once and marks
    /// the id `thumb_uploading`; a repeated request is then a dedup no-op.
    #[test]
    fn request_thumbnail_fastpath_marks_uploading_once() {
        let mut s = AppState::for_test();
        let ctx = egui::Context::default();
        // Seed the CPU pixel cache (1x1 RGBA) without a live texture.
        s.thumb_pixels.insert(7, vec![1, 2, 3, 255], 1, 1);

        s.request_thumbnail(&ctx, 7);
        assert_eq!(s.pending_uploads.len(), 1, "fast path queued one upload");
        assert!(s.thumb_uploading.contains(&7), "id marked awaiting upload");

        // Second request while still awaiting upload: dedup no-op.
        s.request_thumbnail(&ctx, 7);
        assert_eq!(
            s.pending_uploads.len(),
            1,
            "no duplicate push while awaiting upload"
        );
    }

    /// Uploading an id clears its `thumb_uploading` marker (it is now textured).
    #[test]
    fn upload_thumbnail_clears_uploading_marker() {
        let mut s = AppState::for_test();
        let ctx = egui::Context::default();
        s.thumb_uploading.insert(9);

        s.upload_thumbnail(&ctx, 9, vec![1, 2, 3, 255], 1, 1);

        assert!(
            !s.thumb_uploading.contains(&9),
            "upload must clear the awaiting-upload marker"
        );
        assert!(s.textures.contains(9), "id is now textured");
    }

    /// Even a malformed (wrong-length) buffer must clear the `thumb_uploading`
    /// guard, so a bad upload can never strand an id (which would stop its cell
    /// from ever reloading). The texture is NOT created for the bad buffer.
    #[test]
    fn upload_thumbnail_malformed_buffer_still_clears_uploading() {
        let mut s = AppState::for_test();
        let ctx = egui::Context::default();
        s.thumb_uploading.insert(5);
        // Claims 2x2 (needs 16 bytes) but provides 4 → malformed.
        s.upload_thumbnail(&ctx, 5, vec![0u8; 4], 2, 2);
        assert!(
            !s.thumb_uploading.contains(&5),
            "malformed buffer must still clear the awaiting-upload guard"
        );
        assert!(
            !s.textures.contains(5),
            "no texture created for a malformed buffer"
        );
    }

    /// `cancel_pending_jobs` clears both the upload queue and its guard set.
    #[test]
    fn cancel_pending_jobs_clears_uploads_and_uploading() {
        let mut s = AppState::for_test();
        s.pending_uploads.push((1, vec![0; 4], 1, 1));
        s.thumb_uploading.insert(1);

        s.cancel_pending_jobs();

        assert!(s.pending_uploads.is_empty(), "pending_uploads cleared");
        assert!(s.thumb_uploading.is_empty(), "thumb_uploading cleared");
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
            has_edits: false,
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

    /// `ids_missing_tag` keeps ids that do NOT already carry the tag; an id
    /// absent from `visible_tags` is treated as missing it (so it's kept).
    #[test]
    fn ids_missing_tag_filters_those_already_tagged() {
        use ferrolite_image::TagId;
        let t = TagId(7);
        let other = TagId(9);
        let mut vt: std::collections::HashMap<i64, Vec<TagId>> = std::collections::HashMap::new();
        vt.insert(1, vec![t]); // already has t
        vt.insert(2, vec![other]); // has a different tag
        vt.insert(3, vec![]); // untagged
                              // id 4 absent from the map → treated as missing the tag
        let got = super::ids_missing_tag(&[1, 2, 3, 4], t, &vt);
        assert_eq!(got, vec![2, 3, 4]);
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

    /// `remove_selection_from_collection` removes each selected image from the
    /// collection and sets `dirty` only when the current source is that collection.
    #[test]
    fn remove_selection_from_collection_removes_images_and_sets_dirty_when_viewing() {
        use ferrolite_catalog::{FileKind, NewImage};
        let mut s = AppState::for_test();

        // Create a folder, two images, and a collection; add both images to it.
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
            w.add_image_to_collection(c, a).unwrap();
            w.add_image_to_collection(c, b).unwrap();
            (c, a, b)
        };

        // Select both images.
        s.selection = [img_a, img_b].into_iter().collect();
        s.dirty = false;
        // Not currently viewing the collection — dirty must stay false.
        s.source = ViewSource::All;
        s.remove_selection_from_collection(coll_id);
        assert!(
            !s.dirty,
            "dirty stays false when not viewing the collection"
        );

        // Verify images are no longer in the collection via the read pool.
        s.reload_vocab();
        s.source = ViewSource::Collection(coll_id);
        s.refresh_images();
        assert_eq!(
            s.images.len(),
            0,
            "both images should have been removed from the collection"
        );

        // Re-add both, then remove again while viewing the collection: dirty must be set.
        {
            let w = s.writer.lock().unwrap();
            w.add_image_to_collection(coll_id, img_a).unwrap();
            w.add_image_to_collection(coll_id, img_b).unwrap();
        }
        s.refresh_images();
        s.selection = [img_a, img_b].into_iter().collect();
        s.dirty = false;
        s.source = ViewSource::Collection(coll_id);
        s.remove_selection_from_collection(coll_id);
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
            has_edits: false,
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

    #[test]
    fn queue_add_dedups_and_preserves_order() {
        let mut s = AppState::for_test();
        s.queue_add(1);
        s.queue_add(2);
        s.queue_add(1); // dup ignored
        assert_eq!(s.export_queue, vec![1, 2]);
    }

    #[test]
    fn queue_remove_and_clear() {
        let mut s = AppState::for_test();
        s.export_queue = vec![1, 2, 3];
        s.queue_remove(2);
        assert_eq!(s.export_queue, vec![1, 3]);
        s.queue_clear();
        assert!(s.export_queue.is_empty());
    }

    #[test]
    fn queue_toggle_adds_then_removes() {
        let mut s = AppState::for_test();
        assert!(!s.queue_contains(7));
        s.queue_toggle(7);
        assert!(s.queue_contains(7));
        s.queue_toggle(7);
        assert!(!s.queue_contains(7));
    }

    #[test]
    fn queue_reorder_moves_item() {
        let mut s = AppState::for_test();
        s.export_queue = vec![10, 20, 30, 40];
        s.queue_reorder(0, 2); // move 10 into gap-before-index-2 → after 20
        assert_eq!(s.export_queue, vec![20, 10, 30, 40]);
        s.queue_reorder(3, 0); // move 40 to front
        assert_eq!(s.export_queue, vec![40, 20, 10, 30]);
        s.queue_reorder(1, 1); // no-op (same slot)
        assert_eq!(s.export_queue, vec![40, 20, 10, 30]);
    }

    /// Seed `n` grid rows (ids 1..=n) for select-all tests.
    #[cfg(test)]
    fn seed_grid_rows(s: &mut AppState, n: i64) {
        use ferrolite_catalog::{DecodeStatus, FileKind};
        use ferrolite_image::{Flag, Orientation, Rating};
        s.images = (1..=n)
            .map(|id| ferrolite_catalog::ImageRecord {
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
                has_edits: false,
            })
            .collect();
    }

    #[test]
    fn toggle_select_all_selects_every_row_from_empty() {
        let mut s = AppState::for_test();
        seed_grid_rows(&mut s, 3);

        s.toggle_select_all();

        assert_eq!(
            s.selection,
            [1, 2, 3].into_iter().collect(),
            "all rows selected"
        );
        assert_eq!(
            s.selection_anchor,
            Some(1),
            "anchor set to the first row on select-all"
        );
    }

    #[test]
    fn toggle_select_all_clears_when_all_already_selected() {
        let mut s = AppState::for_test();
        seed_grid_rows(&mut s, 3);
        s.selection = [1, 2, 3].into_iter().collect();
        s.selection_anchor = Some(2);

        s.toggle_select_all();

        assert!(s.selection.is_empty(), "selection cleared on second toggle");
        assert_eq!(s.selection_anchor, None, "anchor cleared on deselect");
    }

    #[test]
    fn toggle_select_all_reselects_from_partial_selection() {
        let mut s = AppState::for_test();
        seed_grid_rows(&mut s, 3);
        s.selection = [2].into_iter().collect();

        s.toggle_select_all();

        assert_eq!(
            s.selection,
            [1, 2, 3].into_iter().collect(),
            "partial selection expands to all rows, not cleared"
        );
    }

    #[test]
    fn toggle_select_all_noop_on_empty_grid() {
        let mut s = AppState::for_test();
        // No rows seeded.
        s.toggle_select_all();
        assert!(
            s.selection.is_empty(),
            "empty grid leaves selection empty (no all-selected clear path)"
        );
        assert_eq!(s.selection_anchor, None);
    }

    /// `batch_running` must be true only while a BATCH activity is in flight —
    /// a single export must never lock the Export-module queue.
    #[test]
    fn batch_running_true_only_for_inflight_batch() {
        let mut s = AppState::for_test();

        s.export_activity = Some(crate::export::ExportActivity::new_single(None));
        assert!(
            !s.batch_running(),
            "single export must not report batch_running"
        );

        s.export_activity = Some(crate::export::ExportActivity::new_batch(2));
        assert!(
            s.batch_running(),
            "an in-flight batch must report batch_running"
        );

        let a = s.export_activity.as_mut().unwrap();
        a.item_finished(true, "ok".into());
        a.item_finished(true, "ok".into());
        assert!(
            !s.batch_running(),
            "batch_running must go false once the batch is done"
        );
    }
}

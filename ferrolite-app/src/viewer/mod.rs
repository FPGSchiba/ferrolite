//! Single-image viewer state + pure pan/zoom input math, plus the tier-1
//! preview decode → upload → paint wiring (egui↔wgpu callback).

pub mod callback;
pub mod edit_producer;
pub mod load;
pub mod nav;
pub mod present;

pub use callback::{PreviewWhich, ViewerCallback, ViewerGpu, ViewerPipelines};
pub use edit_producer::EditTileProducer;
pub use present::{present_source, PresentSource};

use std::path::PathBuf;

use ferrolite_image::FileKind;
use ferrolite_jobs::JobHandle;
use ferrolite_pipeline::{EditPipeline, GpuPyramidSource, OpStack};
use ferrolite_vt::ViewTransform;

/// Preview→full crossfade duration (seconds). Short enough to read as instant,
/// long enough to avoid a hard pop.
pub const CROSSFADE_SECS: f32 = 0.15;

/// The Tier-0 grid-thumbnail placeholder is shown only once an open has been
/// unrevealed for this long — so fast/warm reveals show no placeholder at all
/// (no flash), while a genuinely slow cold decode still gets instant-ish
/// feedback. Tunable.
pub const TIER0_GRACE_SECS: f32 = 0.12;

/// Debounce (seconds) between a preview recompute and the histogram dispatch, so
/// a slider drag coalesces into one compute rather than one per frame.
pub const HIST_DEBOUNCE: f32 = 0.10;

/// Per-viewer histogram state: the last delivered bins (1024 = 256×{R,G,B,luma}),
/// plus debounce + in-flight bookkeeping. `bins` is drawn read-only in the panel.
pub struct HistogramState {
    pub bins: Option<Vec<u32>>,
    /// `pub(crate)` (rather than private) because `app.rs`'s
    /// `maybe_update_histogram`/event handler drive these directly across the
    /// module boundary; `mark_dirty`/`tick`/`should_dispatch` remain the normal
    /// entry points for everything else.
    pub(crate) dirty: bool,
    pub(crate) inflight: bool,
    since_dirty: f32,
}

impl HistogramState {
    pub fn new() -> Self {
        Self {
            bins: None,
            dirty: true, // compute once as soon as the preview is up
            inflight: false,
            since_dirty: 0.0,
        }
    }

    /// A preview recompute happened: recompute the histogram after the debounce.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.since_dirty = 0.0;
    }

    /// Advance the debounce timer (only while dirty).
    pub fn tick(&mut self, dt: f32) {
        if self.dirty {
            self.since_dirty += dt;
        }
    }

    /// True when a fresh compute should be dispatched now.
    pub fn should_dispatch(&self) -> bool {
        self.dirty && !self.inflight && self.since_dirty >= HIST_DEBOUNCE
    }
}

impl Default for HistogramState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ViewerState {
    pub image_id: i64,
    pub path: PathBuf,
    pub kind: FileKind,
    pub op_stack: OpStack,
    /// The op-stack the full-res `edit_producer` currently reflects. During a
    /// slider DRAG the full-res tier is deferred (only the live preview updates
    /// per frame), so `op_stack` moves ahead of what the producer was last synced
    /// to. This is that last-synced baseline: `needs_full_rebuild` on commit
    /// compares against THIS (not the previous frame) so a dehaze on/off or radius
    /// change across the drag still triggers the right rebuild on release.
    pub full_stack: OpStack,
    /// True while a slider edit is being DRAGGED (`commit == false`). While set,
    /// `drive_viewer` PAUSES full-res tile production: the fit view already shows
    /// the live preview tier during a drag (the full tier is off-screen while the
    /// version bumps), and re-producing the heavy full-res tiles (dehaze's
    /// multi-pass transmission per tile) every drag frame churned GPU memory to OOM
    /// on constrained/integrated GPUs. Cleared on commit (drag release), when
    /// production resumes and refreshes the 1:1 detail once.
    pub edit_in_progress: bool,
    /// Plan 3: the full-res edit producer (built on full-decode when the stack is
    /// non-identity). `!Send`/`!Sync`, so it lives here, never in callback_resources.
    pub edit_producer: Option<edit_producer::EditTileProducer>,
    pub view: ViewTransform,
    /// Last painted canvas size (image-space fit + zoom/pan math need it).
    pub viewport: (f32, f32),
    /// True once an `Interactive` preview decode has been submitted (one-shot).
    pub preview_requested: bool,
    /// Seconds elapsed since this viewer was opened. Used to debounce the
    /// tier-2 full-RAW decode submission (`FULL_DECODE_DEBOUNCE` in `app.rs`)
    /// so fast arrow-navigation doesn't queue a full decode per image —
    /// only the image the user settles on submits one.
    pub open_elapsed: f32,
    /// True once the rung-1 `VirtualTexture` is uploaded and the view fitted; the
    /// VT itself lives in eframe's `callback_resources` (paint reads it there).
    pub loaded: bool,

    /// True once the tier-2 full decode has been submitted (one-shot).
    pub full_requested: bool,
    /// True once the sparse (tier-2) `VirtualTexture` is built and stored in
    /// `ViewerGpu`; the paint then drives it per frame and crossfades toward it.
    pub full_ready: bool,
    /// True while the preview→full crossfade ramp is advancing.
    pub crossfading: bool,
    /// The `(opstack_version, view)` the CURRENT composed `front` buffer was
    /// composed at (spec 4.5 §4.2), or `None` when `front` is invalid / never
    /// composed (fresh open, or just reallocated by a canvas resize). Keys the
    /// off-screen compose+swap: the swap fires when the pool is converged AND
    /// this key does not match the current `(opstack_version, view)` (front is
    /// stale or missing), and `paint` presents `Front` only when this key
    /// matches the current state — otherwise `Preview`. This replaces the old
    /// transient `present_swapped` bool: an edit bumps `opstack_version` and a
    /// pan/zoom changes `view`, so the key mismatches immediately (no reliance on
    /// observing a `!converged` frame), which is why edits and the split now show
    /// without a zoom nudge.
    pub present_key: Option<(u64, ferrolite_vt::ViewTransform)>,
    /// Seconds elapsed into the active crossfade.
    pub crossfade_elapsed: f32,
    /// Terminal state: nothing more will load (preview failed AND/OR full failed,
    /// or full is ready and the crossfade is complete with no tiles pending). When
    /// set the paint loop stops requesting repaints to avoid a busy-loop.
    pub idle: bool,
    /// The real, per-frame-current `show_full` computed by `drive_viewer`
    /// (`full_ready && factor >= 1.0 && tiles_settled`) — i.e. whether the full
    /// (1:1 sparse) tier is ACTUALLY on screen this frame, as opposed to
    /// `full_ready` alone (which is true even while tiles are still streaming
    /// in after a pan/zoom). Consulted by `toggle_split_compare` to decide
    /// whether enabling the split would dead-end on the full tier and thus
    /// needs an auto-fit back to the preview tier.
    pub showing_full: bool,

    /// Image dimensions in pixels, stored once the preview arrives (needed for
    /// the fit↔1:1 double-click toggle and any future fit-on-resize logic).
    pub image_dims: Option<(u32, u32)>,

    /// In-flight decode jobs (preview + full). Cancelled on navigation so a
    /// superseded image's decode does not race the newly-opened one.
    pub preview_handle: Option<JobHandle>,
    pub full_handle: Option<JobHandle>,
    /// Handle for the off-thread full-res pyramid build (sparse-VT tile source
    /// and GPU edit pyramid, submitted from `apply_full_decoded`). Cancelled on
    /// navigation so a superseded image's ~1.2s CPU pyramid build and GPU
    /// upload doesn't keep running after its result would be discarded anyway
    /// by the `image_id` staleness guard in `apply_pyramid_ready`.
    pub pyramid_handle: Option<JobHandle>,
    /// One-shot deferral flag (perf fix D): set by `apply_full_decoded` when
    /// the tier-2 pyramid build was ready to submit but no
    /// `try_acquire_pyramid_permit` slot was free (`PYRAMID_BUILD_CONCURRENCY`
    /// concurrent builds already in flight). `drive_viewer` retries the submit
    /// each frame while this is `true`. Because the flag lives on THIS viewer
    /// (reset to `false` by every `ViewerState::open`), a superseded image's
    /// deferred pyramid is never retried after navigation — the new viewer has
    /// its own flag, starting `false`.
    pub needs_pyramid: bool,

    // ── Preview-cache read gating (Task 6) ─────────────────────────────────
    /// Handle for the in-flight preview-cache read job; cancelled on navigation
    /// so a scrubbed-past image's read coalesces with the rest.
    pub cache_read_handle: Option<JobHandle>,
    /// True once the debounced preview-cache read has been submitted (one-shot).
    pub cache_read_requested: bool,
    /// True once the cache read resolved (HIT or MISS). The RAW tier-2 full
    /// decode is gated on this: the full decode fires only after the read
    /// resolves, so a cache HIT reveals from disk and the full decode then
    /// streams in zoom/sparse detail.
    pub cache_resolved: bool,
    /// Whether the eventual full-decode render should be written back to the
    /// cache. Starts `true` (a fresh open is a miss until proven otherwise); set
    /// `false` on a cache HIT (the entry already exists) and `true` on a MISS.
    pub cache_write_back: bool,

    // ── Edit state (Task 8 / Plan 4) — read by Tasks 9+ ────────────────────
    /// The full-res linear source retained for re-evaluation when the op stack
    /// changes (built from the tier-2 full decode). For a Standard image this is
    /// the decoded sRGB image; for a RAW image this holds the embedded JPEG
    /// linear buffer, retained ONLY as the full-decode-failure fallback source
    /// (`FullFailed`) — never displayed on the happy path.
    pub preview_source: Option<std::sync::Arc<ferrolite_image::LinearRgbaF32>>,
    /// RAW preview-tier source: the demosaiced, camera-native, half-res
    /// `LinearRgbaF32` from the tier-2 full decode. For RAW everything displayed
    /// and measured on the preview tier (the rung-1 reveal render, the
    /// interactive `preview_edit`, the before-view, and thus the histogram) is
    /// sourced from THIS buffer through the camera→working matrix — the same
    /// color path as the sparse full — so the preview→full swap is a
    /// sharpness-only ramp with no color/tone shift. `None` for Standard images
    /// and for RAW before the full decode arrives.
    pub raw_preview_source: Option<std::sync::Arc<ferrolite_image::LinearRgbaF32>>,
    /// Whole-image dehaze atmospheric light `A` (design §5.3), estimated once
    /// per image and cached here. `A` is an image-invariant statistic, so it
    /// must NOT be re-estimated on every full-res producer rebuild (those also
    /// fire on radius/geometry/lens drags, on the UI thread) — see
    /// `dehaze_atmos()`. `None` until first requested; always `None` on a
    /// fresh `open()` (a new `ViewerState` is constructed per image, never
    /// reused in place, so no explicit invalidation on image switch is needed).
    dehaze_atmos: Option<[f32; 3]>,
    /// The retained GPU edit pipeline (`!Send`/`!Sync`, lives here like
    /// `edit_producer`). Rebuilt when geometry / halo radius changes.
    pub preview_edit: Option<EditPipeline>,
    /// The GPU pyramid retained so the full-res producer can be rebuilt on
    /// geometry or halo-radius changes.
    pub pyramid: Option<std::sync::Arc<GpuPyramidSource>>,
    /// Camera color profile from the tier-2 full decode; feeds the ColorMatrixNode
    /// via `ferrolite_color::camera_to_working`. sRGB fallback until full decode.
    pub color_profile: ferrolite_decode::ColorProfile,
    /// Monotonically-increasing counter; bumped on every op-stack mutation so
    /// GPU evaluation knows to re-run.
    pub opstack_version: u64,
    /// Bounded undo/redo ring for the current image's op stack.
    pub history: crate::develop::history::History,
    /// When `true`, the viewer renders the before/after split view.
    pub before_after: bool,
    /// When `true`, the viewer renders the before/after SPLIT (draggable divider);
    /// distinct from `before_after` (the `\` momentary full-before swap).
    pub split_compare: bool,
    /// Divider position as a fraction of the canvas width, in [MIN_POS, MAX_POS].
    pub split_pos: f32,
    /// When `true`, the crop overlay is active.
    pub crop_active: bool,
    /// Index of the currently-selected HSL band in the HSL panel (0–7).
    pub hsl_band: usize,
    /// Masking-tool UI state (design §9). Per-image, like `hsl_band`/`crop_active`.
    pub mask: crate::develop::mask_ui::MaskUiState,
    /// Overlay compositor for the canvas mask fill (Task 9), built once lazily on
    /// first overlay use and reused thereafter (CLAUDE.md §2: build GPU pipelines
    /// once, never per frame).
    pub mask_overlay: Option<ferrolite_pipeline::MaskOverlayCompositor>,
    /// Bounded (≤ `OVERLAY_MAX_EDGE`), GPU-uploaded downscale of `preview_source`
    /// used as the compositor's input for range shapes. Cached; rebuilt only when
    /// `preview_source` changes (cleared alongside it).
    pub mask_overlay_input: Option<ferrolite_pipeline::PipelineImage>,
    /// Monotonic generation of `mask_overlay_input`, bumped each time it is
    /// (re)uploaded. Passed to the overlay compositor as the input identity so its
    /// per-component cache invalidates range shapes when the input image changes.
    /// A counter (not the texture's `Arc` pointer) avoids an ABA hazard: a freed
    /// texture's address can be reused by the new upload, which would collide.
    pub mask_overlay_input_gen: u64,
    /// `true` once the `OpsLoaded` event for this image has been received (the
    /// op-stack read job finished and the stack has been applied).
    pub ops_loaded: bool,
    /// Handle for the in-flight op-stack read job; cancelled on navigation.
    pub ops_read_handle: Option<JobHandle>,
    /// Live GPU histogram of the on-screen preview (spec §7.1).
    pub histogram: HistogramState,

    // ── Neighbor prefetch (Task 7) ──────────────────────────────────────────
    /// True once the low-priority neighbor-prefetch pass has been submitted
    /// for this open (one-shot — fires only after `loaded`, never re-fires).
    pub prefetch_requested: bool,
    /// Handles for the in-flight prefetch jobs; cancelled on navigation
    /// alongside the other load handles so scrubbing past this image doesn't
    /// leave stale background decodes racing the newly-opened one.
    pub prefetch_handles: Vec<JobHandle>,

    // ── Warm-source neighbor prefetch (Task 7) ─────────────────────────────
    /// True once the forward-biased warm-source prefetch pass
    /// (`develop::warm_prefetch::spawn_warm_sources`) has been submitted for
    /// this open (one-shot — fires only after `loaded`, never re-fires).
    pub warm_prefetch_requested: bool,
    /// Handle for the single serialized warm-source prefetch job; cancelled on
    /// navigation alongside the other load handles for the same reason as
    /// `prefetch_handles`.
    pub warm_prefetch_handles: Vec<JobHandle>,

    /// True once any edit is applied to this image THIS session (including a
    /// reset-to-identity, which is also a change). Drives auto-regeneration of
    /// the Library thumbnail when leaving Develop. Set only in the app's
    /// `apply_edit`/undo/redo paths — NOT in the `OpsLoaded` load path.
    pub edits_dirty: bool,

    // ── Lens correction bake (Spec 4.4, U7) ────────────────────────────────
    /// The current lens bake products for this image's `LensCorrection`
    /// (`None`/`None` = no lens matched, or distortion+TCA/vignetting
    /// disabled — both render as identity). Threaded into `TileEditPipeline::
    /// new` at every rebuild so full-res tiles apply the live correction.
    pub lens_warp: Option<ferrolite_lens::WarpGrid>,
    pub lens_vignette: Option<ferrolite_lens::VignetteMap>,
    /// Human-readable resolved lens name for the panel label, or `None` when
    /// `lens_id` didn't resolve (stale/unknown persisted key).
    pub lens_resolved_name: Option<String>,
    /// Handle for the in-flight lens-bake job; cancelled on navigation (in
    /// `cancel_loads`) or when superseded by a newer bake request, so a stale
    /// result can never overwrite a fresher one.
    pub lens_bake_handle: Option<JobHandle>,
    /// True while the searchable camera+lens picker (Spec 4.4, U8 Task 13) is
    /// open. Persisted here (not egui memory) so it survives the panel's
    /// per-frame `Option` plumbing the same way `hsl_band` does.
    pub lens_picker_open: bool,
    /// The picker's search needle, kept across frames while open.
    pub lens_picker_query: String,

    // ── EXIF plumbing (Spec 4.4, U9) ───────────────────────────────────────
    /// This image's decoded EXIF (make/model/focal_length/aperture/lens),
    /// read off-thread by `develop::meta_read::spawn_meta_read` on open (the
    /// catalog only caches make/model, not focal/aperture/lens — see that
    /// module's doc comment). `None` until the read resolves, or forever on a
    /// decode error; consumers must fall back to the existing constant
    /// defaults in that case.
    pub meta: Option<ferrolite_decode::Metadata>,
    /// `true` once a `MetaLoaded` event for this image has been received
    /// (successful or not) — a one-shot guard mirroring `ops_loaded`.
    pub meta_loaded: bool,
    /// Handle for the in-flight metadata-read job; cancelled on navigation.
    pub meta_read_handle: Option<JobHandle>,
    /// The cheap in-memory auto-match candidate (Spec 4.4 Task 14 Step 1):
    /// computed once EXIF is available AND the loaded stack has no
    /// `LensCorrection` op yet. Purely a seed for the panel's "no lens picked
    /// yet" default (camera_hint + a pre-filled lens/crop) — storing it here
    /// does NOT create an op or enable any correction, so `has_edits` is
    /// unaffected (opt-in preserved). `None` when unmatched, DB-unavailable,
    /// or a persisted `LensCorrection` op already exists (auto-match only
    /// applies to a brand-new, never-edited stack).
    pub lens_auto_match: Option<ferrolite_lens::LensMatch>,
    /// `true` once the auto-match attempt has run for this image (whether or
    /// not it found a candidate) — a one-shot guard so it re-runs at most once
    /// per open, not every frame while `meta`/`ops_loaded` are both `Some`.
    pub lens_auto_match_attempted: bool,

    /// `true` once `try_warm_reveal` has installed a cached render for this
    /// open (a warm-cache hit). Gates the normal Tier-0/1/decode submission in
    /// the open-flow drive loop so a warm hit does not also kick off a
    /// redundant decode — the reveal is already on screen from RAM.
    pub warm_revealed: bool,
    /// `true` once the deferred warm-reveal attempt (`try_warm_reveal`) has run
    /// for this open — a one-shot guard mirroring `lens_auto_match_attempted`.
    /// The attempt is deferred until `ops_loaded` (the warm cache is keyed by
    /// `op_stack_hash()`, which is only correct once the real op stack has
    /// loaded — a fresh viewer starts at `OpStack::default()`), so this must
    /// stay `false` across the frames spent waiting on the ops-sidecar read,
    /// then flip `true` the first frame `ops_loaded` is `true` regardless of
    /// hit or miss (a miss must not be retried every frame).
    pub warm_reveal_attempted: bool,
}

impl ViewerState {
    /// Open the viewer for the given image. The viewport size is not yet known;
    /// `ViewTransform::fit` will be called when the preview arrives.
    pub fn open(image_id: i64, path: PathBuf, kind: FileKind) -> Self {
        Self {
            image_id,
            path,
            kind,
            op_stack: OpStack::default(),
            full_stack: OpStack::default(),
            edit_in_progress: false,
            edit_producer: None,
            view: ViewTransform {
                zoom: 1.0,
                pan: (0.0, 0.0),
            },
            viewport: (0.0, 0.0),
            preview_requested: false,
            open_elapsed: 0.0,
            loaded: false,
            full_requested: false,
            full_ready: false,
            crossfading: false,
            present_key: None,
            crossfade_elapsed: 0.0,
            idle: false,
            showing_full: false,
            image_dims: None,
            preview_handle: None,
            full_handle: None,
            pyramid_handle: None,
            needs_pyramid: false,
            cache_read_handle: None,
            cache_read_requested: false,
            cache_resolved: false,
            cache_write_back: true,
            preview_source: None,
            raw_preview_source: None,
            dehaze_atmos: None,
            preview_edit: None,
            pyramid: None,
            color_profile: ferrolite_decode::ColorProfile::srgb_fallback(),
            opstack_version: 0,
            history: crate::develop::history::History::new(OpStack::default(), 100),
            before_after: false,
            split_compare: false,
            split_pos: 0.5,
            crop_active: false,
            hsl_band: 0,
            mask: crate::develop::mask_ui::MaskUiState::default(),
            mask_overlay: None,
            mask_overlay_input: None,
            mask_overlay_input_gen: 0,
            ops_loaded: false,
            ops_read_handle: None,
            histogram: HistogramState::new(),
            prefetch_requested: false,
            prefetch_handles: Vec::new(),
            warm_prefetch_requested: false,
            warm_prefetch_handles: Vec::new(),
            edits_dirty: false,
            lens_warp: None,
            lens_vignette: None,
            lens_resolved_name: None,
            lens_bake_handle: None,
            lens_picker_open: false,
            lens_picker_query: String::new(),
            meta: None,
            meta_loaded: false,
            meta_read_handle: None,
            lens_auto_match: None,
            lens_auto_match_attempted: false,
            warm_revealed: false,
            warm_reveal_attempted: false,
        }
    }

    /// Begin the preview→full crossfade ramp (called when the full sparse VT
    /// becomes available).
    pub fn begin_crossfade(&mut self) {
        self.crossfading = true;
        self.crossfade_elapsed = 0.0;
    }

    /// Advance the crossfade by `dt` seconds and return the current blend factor
    /// in `[0, 1]` (0 = all preview, 1 = all full). Pure: clamps at 1.0 and, once
    /// not actively crossfading, reports 1.0 iff the full image is ready.
    pub fn tick_crossfade(&mut self, dt: f32) -> f32 {
        if !self.crossfading {
            return if self.full_ready { 1.0 } else { 0.0 };
        }
        self.crossfade_elapsed += dt;
        let factor = (self.crossfade_elapsed / CROSSFADE_SECS).clamp(0.0, 1.0);
        if factor >= 1.0 {
            self.crossfading = false;
        }
        factor
    }

    /// Select the `(source buffer, color matrix)` the PREVIEW tier must use for
    /// this image's kind. This is the single source of truth for the RAW-vs-Standard
    /// preview color path, shared by `apply_full_decoded`, `set_preview_and_full`,
    /// and `apply_working_space` so the three sites cannot drift apart.
    ///
    /// * RAW: the demosaiced, camera-native `raw_preview_source` through the
    ///   camera→working matrix (`cam`) — the SAME color path as the sparse full,
    ///   so the preview↔full swap is a sharpness-only ramp with no color/tone shift.
    /// * Standard: the sRGB `preview_source` through sRGB→working (`pw`).
    ///
    /// Sourcing RAW from the sRGB JPEG (with `pw`), or Standard from a RAW buffer,
    /// would reintroduce the exact color/tone shift the progressive-reveal path
    /// exists to eliminate. Pure — no GPU, no side effects.
    pub fn preview_tier_source(
        &self,
        cam: [[f32; 3]; 3],
        pw: [[f32; 3]; 3],
    ) -> (
        Option<std::sync::Arc<ferrolite_image::LinearRgbaF32>>,
        [[f32; 3]; 3],
    ) {
        match self.kind {
            FileKind::Raw => (self.raw_preview_source.clone(), cam),
            FileKind::Standard => (self.preview_source.clone(), pw),
        }
    }

    /// Whole-image dehaze atmospheric light (design §5.3), estimated once from
    /// the decoded preview source and cached — it is image-invariant, so
    /// producer rebuilds (radius/geometry/lens drags) must NOT re-estimate it
    /// on the UI thread (CLAUDE.md responsiveness rule 1). Returns `None` only
    /// before any preview source has decoded yet.
    pub fn dehaze_atmos(&mut self) -> Option<[f32; 3]> {
        if self.dehaze_atmos.is_none() {
            let src = self
                .raw_preview_source
                .as_ref()
                .or(self.preview_source.as_ref())?;
            self.dehaze_atmos = Some(ferrolite_pipeline::estimate_atmospheric_light(src));
        }
        self.dehaze_atmos
    }

    /// Cancel the in-flight decode jobs for this viewer. The sparse tile jobs
    /// are cancelled separately (they live in the `ViewerGpu` holder, owned by
    /// `callback_resources`) when that holder is dropped/replaced.
    pub fn cancel_loads(&self) {
        if let Some(h) = self.preview_handle.as_ref() {
            h.cancel();
        }
        if let Some(h) = self.full_handle.as_ref() {
            h.cancel();
        }
        if let Some(h) = self.pyramid_handle.as_ref() {
            h.cancel();
        }
        if let Some(h) = self.ops_read_handle.as_ref() {
            h.cancel();
        }
        if let Some(h) = self.cache_read_handle.as_ref() {
            h.cancel();
        }
        for h in &self.prefetch_handles {
            h.cancel();
        }
        for h in &self.warm_prefetch_handles {
            h.cancel();
        }
        if let Some(h) = self.lens_bake_handle.as_ref() {
            h.cancel();
        }
        if let Some(h) = self.meta_read_handle.as_ref() {
            h.cancel();
        }
    }

    /// Stable hash of this viewer's current op stack, for warm-cache keying.
    /// Uses the same `hash_serde` the disk preview cache keys with, so identical
    /// stacks collide intentionally and any edit changes the hash.
    pub fn op_stack_hash(&self) -> u64 {
        ferrolite_previews::hash_serde(&self.op_stack)
    }
}

/// Zoom about the cursor: keep the image point under the cursor fixed.
///
/// `pan` follows the `ViewTransform` convention: image-space px offset of the
/// viewport center (i.e. `cx = viewport_width/2 + pan.0` etc.).  The test
/// invariant is that the image point under the cursor does not move on screen.
pub fn apply_zoom(
    view: ViewTransform,
    scroll: f32,
    cursor: (f32, f32),
    viewport: (f32, f32),
) -> ViewTransform {
    let factor = (1.0 + scroll * 0.1).max(0.05);
    let new_zoom = (view.zoom * factor).clamp(0.01, 64.0);

    // The viewport-center in image space before zooming.
    let center = (view.pan.0 + viewport.0 * 0.5, view.pan.1 + viewport.1 * 0.5);
    // The image-space point currently under the cursor.
    let img_pt = (
        center.0 + (cursor.0 - viewport.0 * 0.5) / view.zoom,
        center.1 + (cursor.1 - viewport.1 * 0.5) / view.zoom,
    );
    // New viewport center so img_pt maps back to the same cursor position.
    let new_center = (
        img_pt.0 - (cursor.0 - viewport.0 * 0.5) / new_zoom,
        img_pt.1 - (cursor.1 - viewport.1 * 0.5) / new_zoom,
    );
    ViewTransform {
        zoom: new_zoom,
        // pan = new_center - viewport/2  (new_center is the new image-space viewport center)
        pan: (
            new_center.0 - viewport.0 * 0.5,
            new_center.1 - viewport.1 * 0.5,
        ),
    }
}

/// Pan by a screen-space drag delta.
///
/// Dragging the image to the right means the viewport center moves left in
/// image space, so pan decreases.  Dividing by zoom converts screen px to
/// image px.
pub fn apply_pan(view: ViewTransform, drag_delta: (f32, f32)) -> ViewTransform {
    ViewTransform {
        zoom: view.zoom,
        pan: (
            view.pan.0 - drag_delta.0 / view.zoom,
            view.pan.1 - drag_delta.1 / view.zoom,
        ),
    }
}

/// Paint the viewer's central canvas: fill black, read scroll/drag input into
/// the view transform, record the viewport size, and (once the rung-1 preview
/// texture is loaded) enqueue the egui↔wgpu paint callback.
///
/// Interaction (pan/zoom this frame) is detected HERE, so the present source is
/// computed here too: `present_source(interacting, split, full_ready,
/// front_valid, crossfade)` selects what the callback shows this frame — the
/// rung-1 preview (during interaction, while the before/after split is active,
/// before the `front` buffer is composed, or when `front` is stale/blank — see
/// `front_valid`), a crossfade toward the composed `front`, or the converged
/// `front` alone. `full_ready`, `front_valid`, and `crossfade` are computed by
/// `drive_viewer` and passed in; `split` is read from `state.split_compare` here.
///
/// Returns `(loading_preview, present_source)`: `loading_preview` is `true` while
/// the preview is still loading so the caller can `request_repaint` for a prompt
/// first pixel; `present_source` is returned so the caller can gate repaints on
/// an active crossfade.
///
/// When `interactive == false` (e.g. the crop tool is active) the canvas
/// pan/zoom/double-click interaction is SKIPPED entirely — the drag interaction
/// is not even registered — so the crop overlay is the sole input target over
/// this area. The image still renders (viewport recorded + paint callback added).
pub fn paint(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    full_ready: bool,
    front_valid: bool,
    crossfade: f32,
    interactive: bool,
    tier0_thumb: Option<&egui::TextureHandle>,
) -> (bool, PresentSource) {
    let rect = ui.available_rect_before_wrap();
    // Scope the painter so it drops before any `ui.put` / mutable-borrow calls
    // further down (the `!state.loaded` spinner branch needs `ui` mutably).
    {
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, egui::Color32::BLACK);
    }

    let viewport = (rect.width(), rect.height());
    state.viewport = viewport;

    // Pointer interaction over the canvas: drag pans, scroll zooms about cursor.
    // Skipped when non-interactive so the crop overlay owns input over this area
    // (no competing `viewer-canvas` drag registered, no scroll/drag read).
    // `interacting` records a pan/zoom/double-click THIS frame, which forces the
    // present source back to the smooth preview (the composed `front` is stale
    // for the new transform until the pool reconverges).
    let mut interacting = false;
    if interactive && state.loaded {
        let resp = ui.interact(
            rect,
            ui.id().with(("viewer-canvas", state.image_id)),
            egui::Sense::click_and_drag(),
        );
        if resp.dragged() {
            let d = resp.drag_delta();
            state.view = apply_pan(state.view, (d.x, d.y));
            // The view moved: new tiles may be needed, so wake the drive loop.
            state.idle = false;
            interacting = true;
        }
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > f32::EPSILON {
            if let Some(pos) = resp.hover_pos() {
                let cursor = (pos.x - rect.left(), pos.y - rect.top());
                // Normalize wheel notches (~50px) into the apply_zoom step scale.
                state.view = apply_zoom(state.view, scroll / 50.0, cursor, viewport);
                // Zoom changes the visible LOD/tiles: wake the drive loop.
                state.idle = false;
                interacting = true;
            }
        }
        // Double-click toggles between fit-to-screen and 1:1 (zoom = 1.0, centered).
        if resp.double_clicked() {
            if let Some(dims) = state.image_dims {
                let fit = ferrolite_vt::ViewTransform::fit(dims, viewport);
                // If already near the fit zoom, switch to 1:1; otherwise fit.
                if (state.view.zoom - fit.zoom).abs() < fit.zoom * 0.05 {
                    state.view = ferrolite_vt::ViewTransform {
                        zoom: 1.0,
                        pan: (0.0, 0.0),
                    };
                } else {
                    state.view = fit;
                }
                state.idle = false;
                interacting = true;
                ui.ctx().request_repaint();
            }
        }
    }

    // Now that this frame's interaction is known, select what the callback shows.
    // The before/after SPLIT is a preview-tier-only compare, so fold it in here:
    // while it is active the composed full `front` must never take over.
    let split = state.split_compare;
    let source = present_source(interacting, split, full_ready, front_valid, crossfade);

    if state.loaded {
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewerCallback {
                image_id: state.image_id,
                view: state.view,
                viewport,
                present_source: source,
                which: crate::viewer::PreviewWhich::After,
            },
        ));

        (false, source)
    } else if state.open_elapsed >= TIER0_GRACE_SECS {
        // Grace elapsed without a reveal: this is a genuinely slow cold decode,
        // so show the grid thumbnail (if decoded) upscaled-to-fit plus a spinner
        // so the wait reads as working. A fast/warm reveal never reaches this
        // branch — see the `state.open_elapsed < TIER0_GRACE_SECS` no-op case
        // below, which keeps the open silent (just the black background already
        // painted above) so there is no placeholder flash before a hard cut to
        // the real reveal.
        if let Some(thumb) = tier0_thumb {
            let sz = thumb.size();
            let dst = fit_rect(rect, (sz[0] as f32, sz[1] as f32));
            ui.painter().image(
                thumb.id(),
                dst,
                egui::Rect::from_min_max(
                    egui::pos2(0.0_f32, 0.0_f32),
                    egui::pos2(1.0_f32, 1.0_f32),
                ),
                egui::Color32::WHITE,
            );
        }
        // First pixel not ready yet: keep a spinner + "Loading…" so the wait reads
        // as working, and keep animating so we pick up the reveal as soon as it
        // arrives. Over a thumbnail the spinner is a subtle "sharpening" hint.
        let center = rect.center();
        let spinner_size = 32.0_f32;
        let spinner_rect = egui::Rect::from_center_size(
            center - egui::vec2(0.0_f32, 10.0_f32),
            egui::vec2(spinner_size, spinner_size),
        );
        ui.put(spinner_rect, egui::Spinner::new().size(spinner_size));
        ui.painter().text(
            center + egui::vec2(0.0_f32, 22.0_f32),
            egui::Align2::CENTER_CENTER,
            "Loading\u{2026}",
            egui::FontId::proportional(12.0),
            crate::theme::TEXT_DIM,
        );
        (true, source)
    } else {
        // Still within the grace window: show nothing extra beyond the black
        // background painted at the top of `paint` — no placeholder flash for
        // fast/warm reveals. `loading_preview` stays `true` so the caller keeps
        // requesting repaints and this branch re-evaluates every frame until
        // either the reveal lands (`state.loaded` flips true above) or the grace
        // elapses (the branch above takes over).
        (true, source)
    }
}

/// Map image bounds to the on-screen rect using the same zoom/pan convention
/// as the display shader (`display.wgsl`):
///
/// ```wgsl
/// let center = image * 0.5 + pan;
/// let img_px = center + (screen_px - viewport * 0.5) / zoom;
/// ```
///
/// Inverting: `screen_px = viewport/2 + (img_px − center) * zoom`.
/// So `center = (image_w/2 + pan_x, image_h/2 + pan_y)` is the image-space
/// point that maps to the viewport centre.
///
/// This is a pure helper — no GPU, no egui state. The fit case (pan = 0,
/// zoom = min(vw/iw, vh/ih)) returns the sub-rect of `canvas` that the image
/// occupies; the zoomed/panned case correctly maps to wherever the image sits.
pub fn image_screen_rect(
    canvas: egui::Rect,
    dims: (u32, u32),
    view: ferrolite_vt::ViewTransform,
    viewport: (f32, f32),
) -> egui::Rect {
    let (iw, ih) = (dims.0 as f32, dims.1 as f32);
    let (vw, vh) = viewport;
    let zoom = view.zoom;
    let (pan_x, pan_y) = view.pan;
    // Image-space point that maps to the viewport centre (matches the shader).
    let cx = iw * 0.5 + pan_x;
    let cy = ih * 0.5 + pan_y;
    // Canvas offset of the viewport centre.
    let ox = canvas.left() + vw * 0.5;
    let oy = canvas.top() + vh * 0.5;
    let left = ox + (0.0 - cx) * zoom;
    let top = oy + (0.0 - cy) * zoom;
    let right = ox + (iw - cx) * zoom;
    let bottom = oy + (ih - cy) * zoom;
    egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom))
}

/// Aspect-preserving, centered sub-rect of `canvas` that a `content`-sized image
/// occupies when fit (letterboxed) — the same framing `ViewTransform::fit`
/// produces, but computed directly from sizes for the Tier-0 placeholder shown
/// before the real `image_dims` are known. Pure (no GPU/egui state).
pub fn fit_rect(canvas: egui::Rect, content: (f32, f32)) -> egui::Rect {
    let (cw, ch) = (content.0.max(1.0), content.1.max(1.0));
    let scale = (canvas.width() / cw).min(canvas.height() / ch);
    let (w, h) = (cw * scale, ch * scale);
    egui::Rect::from_center_size(canvas.center(), egui::vec2(w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_vt::ViewTransform;

    #[test]
    fn new_viewer_has_no_raw_preview_source() {
        // The RAW preview-tier source only exists after the tier-2 full decode
        // (`apply_full_decoded`); a freshly-opened viewer must start without it so
        // the RAW path shows the spinner until the color-managed reveal is built.
        let v = ViewerState::open(1, std::path::PathBuf::from("x"), FileKind::Raw);
        assert!(v.raw_preview_source.is_none());
        assert!(
            !v.loaded,
            "RAW opens unrevealed (spinner) until full decode"
        );
    }

    #[test]
    fn new_viewer_is_not_edits_dirty() {
        let v = ViewerState::open(1, std::path::PathBuf::from("x.jpg"), FileKind::Standard);
        assert!(
            !v.edits_dirty,
            "a freshly opened viewer has no session edits"
        );
    }

    #[test]
    fn op_stack_hash_changes_with_edits() {
        let mut v = ViewerState::open(1, std::path::PathBuf::from("x.raw"), FileKind::Raw);
        let h0 = v.op_stack_hash();
        v.op_stack = v.op_stack.set_op(ferrolite_pipeline::Op::Exposure(
            ferrolite_pipeline::Exposure { ev: 1.0 },
        ));
        assert_ne!(
            h0,
            v.op_stack_hash(),
            "an edit must change the warm-cache hash"
        );
    }

    /// Two clearly-distinct sentinel matrices so the selector's matrix choice is
    /// unambiguous in assertions (identity-ish `cam` vs a scaled `pw`).
    const CAM: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    const PW: [[f32; 3]; 3] = [[2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 2.0]];

    fn dummy_linear() -> std::sync::Arc<ferrolite_image::LinearRgbaF32> {
        std::sync::Arc::new(ferrolite_image::LinearRgbaF32::new(1, 1, vec![0.0; 4]).unwrap())
    }

    #[test]
    fn preview_tier_source_raw_uses_raw_buffer_and_cam() {
        // RAW: the preview tier must be sourced from the demosaic camera-native
        // `raw_preview_source` through the camera→working matrix (`cam`) — NOT the
        // sRGB JPEG through `pw`, which would reintroduce the RAW color/tone shift.
        let mut v = ViewerState::open(1, std::path::PathBuf::from("x"), FileKind::Raw);
        let raw = dummy_linear();
        let srgb = dummy_linear();
        v.raw_preview_source = Some(std::sync::Arc::clone(&raw));
        v.preview_source = Some(std::sync::Arc::clone(&srgb));

        let (src, matrix) = v.preview_tier_source(CAM, PW);
        let src = src.expect("RAW selector returns the raw_preview_source");
        assert!(
            std::sync::Arc::ptr_eq(&src, &raw),
            "RAW must select raw_preview_source, not the sRGB JPEG"
        );
        assert_eq!(matrix, CAM, "RAW must use the camera→working matrix");
    }

    #[test]
    fn preview_tier_source_standard_uses_srgb_buffer_and_pw() {
        // Standard: the preview tier is the sRGB `preview_source` through
        // sRGB→working (`pw`). Byte-for-byte unchanged from prior behavior.
        let mut v = ViewerState::open(2, std::path::PathBuf::from("y"), FileKind::Standard);
        let raw = dummy_linear();
        let srgb = dummy_linear();
        v.raw_preview_source = Some(std::sync::Arc::clone(&raw));
        v.preview_source = Some(std::sync::Arc::clone(&srgb));

        let (src, matrix) = v.preview_tier_source(CAM, PW);
        let src = src.expect("Standard selector returns the preview_source");
        assert!(
            std::sync::Arc::ptr_eq(&src, &srgb),
            "Standard must select the sRGB preview_source"
        );
        assert_eq!(matrix, PW, "Standard must use the sRGB→working matrix");
    }

    #[test]
    fn crossfade_ramps_zero_to_one_then_clamps() {
        let mut v = ViewerState::open(1, std::path::PathBuf::from("x"), FileKind::Raw);
        v.begin_crossfade();
        assert_eq!(v.tick_crossfade(0.0), 0.0);
        let mid = v.tick_crossfade(0.075); // half of 150ms
        assert!(mid > 0.4 && mid < 0.6, "about halfway");
        let done = v.tick_crossfade(1.0); // way past
        assert_eq!(done, 1.0, "clamps at 1.0");
    }

    #[test]
    fn crossfade_idle_reports_full_readiness() {
        let mut v = ViewerState::open(2, std::path::PathBuf::from("y"), FileKind::Raw);
        // Not crossfading and full not ready => 0.0 (show preview).
        assert_eq!(v.tick_crossfade(0.5), 0.0);
        // Full ready but crossfade finished => 1.0 (show full).
        v.full_ready = true;
        assert_eq!(v.tick_crossfade(0.5), 1.0);
    }

    #[test]
    fn zoom_keeps_cursor_point_stationary() {
        let v = ViewTransform {
            zoom: 1.0,
            pan: (0.0, 0.0),
        };
        let viewport = (100.0, 100.0);
        let cursor = (75.0, 50.0); // right of center
        let z = apply_zoom(v, 1.0, cursor, viewport); // scroll up = zoom in
        assert!(z.zoom > 1.0, "scroll up zooms in");
        // Cursor is right of viewport center (75 > 50), so to keep the image
        // point under the cursor fixed, the viewport center must shift right in
        // image space (pan.0 increases).
        assert!(z.pan.0 > 0.0, "zoom about off-center cursor pans toward it");
    }

    #[test]
    fn image_screen_rect_fit_square_equals_canvas() {
        // Square image, square canvas, fit zoom → image fills the entire canvas.
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let dims = (100u32, 100u32);
        let viewport = (100.0f32, 100.0f32);
        let view = ViewTransform::fit(dims, viewport);
        let r = super::image_screen_rect(canvas, dims, view, viewport);
        assert!(
            (r.left() - canvas.left()).abs() < 1e-4,
            "left: {}",
            r.left()
        );
        assert!((r.top() - canvas.top()).abs() < 1e-4, "top: {}", r.top());
        assert!(
            (r.right() - canvas.right()).abs() < 1e-4,
            "right: {}",
            r.right()
        );
        assert!(
            (r.bottom() - canvas.bottom()).abs() < 1e-4,
            "bottom: {}",
            r.bottom()
        );
    }

    #[test]
    fn image_screen_rect_fit_landscape_letterboxed() {
        // 200×100 image in a 100×100 canvas: zoom = 0.5, image fills width, letterboxed top/bottom.
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let dims = (200u32, 100u32);
        let viewport = (100.0f32, 100.0f32);
        let view = ViewTransform::fit(dims, viewport);
        // zoom = min(100/200, 100/100) = 0.5; image screen size = 100×50
        let r = super::image_screen_rect(canvas, dims, view, viewport);
        assert!(
            (r.left() - 0.0).abs() < 1e-4,
            "left should be 0, got {}",
            r.left()
        );
        assert!(
            (r.right() - 100.0).abs() < 1e-4,
            "right should be 100, got {}",
            r.right()
        );
        // Vertical center: image height on screen = 50, centered in 100 → top=25, bottom=75
        assert!(
            (r.top() - 25.0).abs() < 1e-4,
            "top should be 25, got {}",
            r.top()
        );
        assert!(
            (r.bottom() - 75.0).abs() < 1e-4,
            "bottom should be 75, got {}",
            r.bottom()
        );
    }

    #[test]
    fn fit_rect_letterboxes_wide_image_in_tall_canvas() {
        // 200x100 canvas, 200x200 content -> width-limited, centered vertically.
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 100.0));
        let r = fit_rect(canvas, (200.0, 200.0));
        // Square content in a 2:1 canvas fits to height 100 -> 100x100, centered.
        assert!((r.width() - 100.0).abs() < 1e-3, "w={}", r.width());
        assert!((r.height() - 100.0).abs() < 1e-3, "h={}", r.height());
        assert!((r.center().x - 100.0).abs() < 1e-3, "centered x");
        assert!((r.center().y - 50.0).abs() < 1e-3, "centered y");
    }

    #[test]
    fn fit_rect_letterboxes_tall_image_in_wide_canvas() {
        let canvas = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 100.0));
        // 100x200 content in 200x100 canvas -> height-limited to 100 -> 50x100.
        let r = fit_rect(canvas, (100.0, 200.0));
        assert!((r.width() - 50.0).abs() < 1e-3, "w={}", r.width());
        assert!((r.height() - 100.0).abs() < 1e-3, "h={}", r.height());
        assert!((r.center().x - 100.0).abs() < 1e-3, "centered x");
    }

    #[test]
    fn pan_translates_by_delta_over_zoom() {
        let v = ViewTransform {
            zoom: 2.0,
            pan: (0.0, 0.0),
        };
        // Drag right 20 px, up 10 px in screen space.
        // pan.0 -= 20/2 = -10; pan.1 -= (-10)/2 = +5
        let p = apply_pan(v, (20.0, -10.0));
        assert!(
            (p.pan.0 + 10.0).abs() < 1e-6,
            "screen delta / zoom, inverted for pan"
        );
        assert!((p.pan.1 - 5.0).abs() < 1e-6);
    }
}

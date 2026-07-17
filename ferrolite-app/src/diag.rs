//! Env-flag-gated diagnostics dev-mode (`FERROLITE_DIAG`). Zero overhead when
//! unset: `enabled()` is a single cached bool check that short-circuits every
//! recorder, the per-frame tick, the overlay, and ingest/blocking generation
//! profiling.
//!
//! `FERROLITE_DIAG` = unset→off | `1`/`both`→log+overlay | `log` | `overlay`.
//! `FERROLITE_DIAG_FILE` overrides the session-log path.

use ferrolite_jobs::JobStats;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagMode {
    Off,
    Log,
    Overlay,
    Both,
}

/// Parse the raw `FERROLITE_DIAG` value into a mode (case/space-insensitive).
pub fn parse_mode(raw: Option<&str>) -> DiagMode {
    match raw.map(|s| s.trim().to_ascii_lowercase()) {
        None => DiagMode::Off,
        Some(v) => match v.as_str() {
            "" | "0" | "off" | "false" => DiagMode::Off,
            "log" => DiagMode::Log,
            "overlay" => DiagMode::Overlay,
            // "1", "both", "true", "on", or any other non-off value → both.
            _ => DiagMode::Both,
        },
    }
}

/// Cached mode, resolved once from the environment.
pub fn mode() -> DiagMode {
    static M: OnceLock<DiagMode> = OnceLock::new();
    *M.get_or_init(|| parse_mode(std::env::var("FERROLITE_DIAG").ok().as_deref()))
}

/// Check if a mode enables logging.
fn mode_logs(m: DiagMode) -> bool {
    matches!(m, DiagMode::Log | DiagMode::Both)
}

/// Check if a mode enables the overlay.
fn mode_overlays(m: DiagMode) -> bool {
    matches!(m, DiagMode::Overlay | DiagMode::Both)
}

pub fn enabled() -> bool {
    !matches!(mode(), DiagMode::Off)
}

/// Logging is enabled if the mode includes log output.
pub fn log_enabled() -> bool {
    mode_logs(mode())
}

/// Overlay is enabled if the mode includes on-screen display.
pub fn overlay_enabled() -> bool {
    mode_overlays(mode())
}

/// Per-second rate of a cumulative delta over `dt_secs` (guards dt→0).
pub fn compute_rate(delta: u64, dt_secs: f64) -> f64 {
    if dt_secs <= 0.0 {
        0.0
    } else {
        delta as f64 / dt_secs
    }
}

/// Lazily-opened session log file (only when logging is enabled). `None` if
/// logging is off or the file could not be opened (writes then go to stderr
/// only). Never panics — diagnostics must not take down the app.
fn log_file() -> &'static Option<Mutex<std::fs::File>> {
    static F: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    F.get_or_init(|| {
        if !log_enabled() {
            return None;
        }
        let path = session_log_path();
        match std::fs::File::create(&path) {
            Ok(f) => {
                eprintln!("[diag] logging to {}", path.display());
                Some(Mutex::new(f))
            }
            Err(e) => {
                eprintln!("[diag] could not open log file {}: {e}", path.display());
                None
            }
        }
    })
}

/// Session-log path: `FERROLITE_DIAG_FILE` if set, else
/// `%LOCALAPPDATA%/ferrolite/diag-<pid>.log` (falling back to the temp dir).
fn session_log_path() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("FERROLITE_DIAG_FILE") {
        return std::path::PathBuf::from(p);
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(std::path::PathBuf::from)
        .map(|b| b.join("ferrolite"))
        .unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::create_dir_all(&base);
    base.join(format!("diag-{}.log", std::process::id()))
}

/// Open the log file eagerly (so its path prints at startup) when logging is
/// enabled. No-op otherwise. Cheap and idempotent.
pub fn init() {
    if enabled() {
        let _ = log_file();
    }
}

/// Best-effort write of a (multi-line) diagnostic block to stderr and, if open,
/// the session file. Never blocks meaningfully and never propagates errors.
pub fn write_log(block: &str) {
    eprintln!("{block}");
    if let Some(lock) = log_file() {
        if let Ok(mut f) = lock.lock() {
            let _ = writeln!(f, "{block}");
            let _ = f.flush();
        }
    }
}

// ── App-side cumulative counters (process-global statics).
// UI-thread-written in practice; Relaxed atomics keep them cheap and sound.
static TEX_HIT: AtomicU64 = AtomicU64::new(0);
static TEX_MISS: AtomicU64 = AtomicU64::new(0);
static TEX_EVICT: AtomicU64 = AtomicU64::new(0);
static PIX_HIT: AtomicU64 = AtomicU64::new(0);
static PIX_MISS: AtomicU64 = AtomicU64::new(0);
static PIX_EVICT: AtomicU64 = AtomicU64::new(0);
static REQ_CALLS: AtomicU64 = AtomicU64::new(0);
static REQ_NEW: AtomicU64 = AtomicU64::new(0);
static REQ_FAST: AtomicU64 = AtomicU64::new(0);
static REQ_DEDUP_TEX: AtomicU64 = AtomicU64::new(0);
static REQ_DEDUP_PENDING: AtomicU64 = AtomicU64::new(0);
static REQ_DEDUP_MISSING: AtomicU64 = AtomicU64::new(0);
static REQ_DEDUP_UPLOADING: AtomicU64 = AtomicU64::new(0);
static RETAIN_CANCELS: AtomicU64 = AtomicU64::new(0);
static EVENTS_DRAINED: AtomicU64 = AtomicU64::new(0);
static UPLOADS_APPLIED: AtomicU64 = AtomicU64::new(0);

// Export (batch) diagnostics: an in-flight gauge plus cumulative done/failed and
// the last item's wall time (ms). The sequential batch job bounds in-flight to ≤1
// (see `export::batch`); the gauge exceeding 1 signals a concurrency regression —
// which is exactly the pathology (many concurrent GPU renders + rav1e encodes on
// the shared device) that froze the UI. Written on worker threads; Relaxed.
static EXPORT_ACTIVE: AtomicU64 = AtomicU64::new(0);
static EXPORT_DONE: AtomicU64 = AtomicU64::new(0);
static EXPORT_FAILED: AtomicU64 = AtomicU64::new(0);
static EXPORT_LAST_MS: AtomicU64 = AtomicU64::new(0);

// Task 17 (spec 4.5 §9): dev-only viewer frame-time profiling. `drive_viewer`
// records the last frame's `stable_dt` (µs, fixed-point) and the sparse
// producer's tiles-produced-this-frame count so the author can measure the
// ≤16.6 ms/frame pan/zoom budget on the dev GPU. Written on the UI thread only
// (Relaxed is a strength/ordering no-op there); read back into `Gauges` at the
// tick site. All writes are behind `diag::enabled()` at the call site in
// `app.rs`, so this is dead weight (a few Relaxed stores, no alloc/format) only
// while diagnostics are on — zero cost otherwise.
static VIEWER_FRAME_US: AtomicU64 = AtomicU64::new(0);
static VIEWER_FRAME_MAX_US: AtomicU64 = AtomicU64::new(0);
static VIEWER_TILES_PRODUCED: AtomicU64 = AtomicU64::new(0);

/// Record one `drive_viewer` frame's timing + producer output. Callers MUST
/// guard this with `diag::enabled()` themselves (mirrors every other recorder
/// in this module) so the caller's hot path never even computes `dt_ms`'s
/// microsecond conversion when diagnostics are off.
pub fn record_viewer_frame(dt_secs: f32, tiles_produced: usize) {
    let us = (dt_secs as f64 * 1_000_000.0).round().max(0.0) as u64;
    VIEWER_FRAME_US.store(us, Ordering::Relaxed);
    VIEWER_FRAME_MAX_US.fetch_max(us, Ordering::Relaxed);
    VIEWER_TILES_PRODUCED.store(tiles_produced as u64, Ordering::Relaxed);
}

/// Last recorded `drive_viewer` frame time, in ms. 0.0 if never recorded
/// (diag off, or no Develop viewer driven yet).
pub fn viewer_frame_ms() -> f64 {
    VIEWER_FRAME_US.load(Ordering::Relaxed) as f64 / 1000.0
}

/// Running max `drive_viewer` frame time (ms) since the last reset.
pub fn viewer_frame_max_ms() -> f64 {
    VIEWER_FRAME_MAX_US.load(Ordering::Relaxed) as f64 / 1000.0
}

/// Reset the running max after it has been read (mirrors `DiagState::tick`'s
/// own per-second `max_frame_ms` reset), so each ~1/sec snapshot shows the max
/// over that window rather than the lifetime max.
pub fn reset_viewer_frame_max() {
    VIEWER_FRAME_MAX_US.store(0, Ordering::Relaxed);
}

/// Tiles the sparse producer rendered on the most recent `drive_viewer` call.
pub fn viewer_tiles_produced() -> u64 {
    VIEWER_TILES_PRODUCED.load(Ordering::Relaxed)
}

/// Current ingest phase, for the live `ingest:` line. `Idle` hides the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IngestPhase {
    #[default]
    Idle,
    Scan,
    PhaseA,
    Filter,
    Decode,
}

impl IngestPhase {
    fn label(self) -> &'static str {
        match self {
            IngestPhase::Idle => "idle",
            IngestPhase::Scan => "scan",
            IngestPhase::PhaseA => "phaseA",
            IngestPhase::Filter => "filter",
            IngestPhase::Decode => "decode",
        }
    }
    fn from_u8(v: u8) -> IngestPhase {
        match v {
            1 => IngestPhase::Scan,
            2 => IngestPhase::PhaseA,
            3 => IngestPhase::Filter,
            4 => IngestPhase::Decode,
            _ => IngestPhase::Idle,
        }
    }
    fn as_u8(self) -> u8 {
        match self {
            IngestPhase::Idle => 0,
            IngestPhase::Scan => 1,
            IngestPhase::PhaseA => 2,
            IngestPhase::Filter => 3,
            IngestPhase::Decode => 4,
        }
    }
}

static INGEST_PHASE: AtomicU8 = AtomicU8::new(0);
static INGEST_CHAN: AtomicU64 = AtomicU64::new(0);

/// Best-effort live phase publish (last-writer-wins across concurrent jobs).
pub fn set_ingest_phase(phase: IngestPhase) {
    INGEST_PHASE.store(phase.as_u8(), Ordering::Relaxed);
}
pub fn note_ingest_chan(depth: u64) {
    INGEST_CHAN.store(depth, Ordering::Relaxed);
}
pub fn ingest_phase() -> IngestPhase {
    IngestPhase::from_u8(INGEST_PHASE.load(Ordering::Relaxed))
}
pub fn ingest_chan() -> u64 {
    INGEST_CHAN.load(Ordering::Relaxed)
}

#[inline]
fn add(c: &AtomicU64, n: u64) {
    if enabled() {
        c.fetch_add(n, Ordering::Relaxed);
    }
}

pub fn tex_hit() {
    add(&TEX_HIT, 1);
}
pub fn tex_miss() {
    add(&TEX_MISS, 1);
}
pub fn tex_evict(n: usize) {
    add(&TEX_EVICT, n as u64);
}
pub fn pix_hit() {
    add(&PIX_HIT, 1);
}
pub fn pix_miss() {
    add(&PIX_MISS, 1);
}
pub fn pix_evict(n: usize) {
    add(&PIX_EVICT, n as u64);
}
pub fn retain_cancels(n: usize) {
    add(&RETAIN_CANCELS, n as u64);
}
pub fn add_events(n: usize) {
    add(&EVENTS_DRAINED, n as u64);
}
pub fn add_uploads(n: usize) {
    add(&UPLOADS_APPLIED, n as u64);
}

/// One batch-export item started (bumps the in-flight gauge). Gated.
pub fn export_item_begin() {
    if enabled() {
        EXPORT_ACTIVE.fetch_add(1, Ordering::Relaxed);
    }
}

/// One batch-export item finished: drop the in-flight gauge (saturating, so it can
/// never underflow), bump done/failed, and record its wall time (ms). Gated.
pub fn export_item_end(ok: bool, ms: u64) {
    if !enabled() {
        return;
    }
    let _ = EXPORT_ACTIVE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
        Some(v.saturating_sub(1))
    });
    EXPORT_DONE.fetch_add(1, Ordering::Relaxed);
    if !ok {
        EXPORT_FAILED.fetch_add(1, Ordering::Relaxed);
    }
    EXPORT_LAST_MS.store(ms, Ordering::Relaxed);
}

pub fn export_active() -> u64 {
    EXPORT_ACTIVE.load(Ordering::Relaxed)
}
pub fn export_done() -> u64 {
    EXPORT_DONE.load(Ordering::Relaxed)
}
pub fn export_failed() -> u64 {
    EXPORT_FAILED.load(Ordering::Relaxed)
}
pub fn export_last_ms() -> u64 {
    EXPORT_LAST_MS.load(Ordering::Relaxed)
}

/// How a `request_thumbnail` call was resolved (see `state::request_thumbnail`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReqOutcome {
    NewSubmit,
    FastPath,
    DedupTextured,
    DedupPending,
    DedupMissing,
    DedupUploading,
}

/// Classify the outcome from the dedup guards, in `request_thumbnail`'s own
/// precedence order (textured > pending > missing > uploading). `NewSubmit` is
/// used when none of the guards hit and there is no pixel-cache fast path — the
/// caller records `FastPath` explicitly for the pixel-cache branch.
pub fn classify_request(
    textured: bool,
    pending: bool,
    missing: bool,
    uploading: bool,
) -> ReqOutcome {
    if textured {
        ReqOutcome::DedupTextured
    } else if pending {
        ReqOutcome::DedupPending
    } else if missing {
        ReqOutcome::DedupMissing
    } else if uploading {
        ReqOutcome::DedupUploading
    } else {
        ReqOutcome::NewSubmit
    }
}

/// Record one classified `request_thumbnail` call (bumps the call total plus
/// the per-outcome counter). Gated internally.
pub fn record_request(outcome: ReqOutcome) {
    if !enabled() {
        return;
    }
    REQ_CALLS.fetch_add(1, Ordering::Relaxed);
    let c = match outcome {
        ReqOutcome::NewSubmit => &REQ_NEW,
        ReqOutcome::FastPath => &REQ_FAST,
        ReqOutcome::DedupTextured => &REQ_DEDUP_TEX,
        ReqOutcome::DedupPending => &REQ_DEDUP_PENDING,
        ReqOutcome::DedupMissing => &REQ_DEDUP_MISSING,
        ReqOutcome::DedupUploading => &REQ_DEDUP_UPLOADING,
    };
    c.fetch_add(1, Ordering::Relaxed);
}

/// Immutable snapshot of the app-side cumulative counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppCounters {
    pub tex_hit: u64,
    pub tex_miss: u64,
    pub tex_evict: u64,
    pub pix_hit: u64,
    pub pix_miss: u64,
    pub pix_evict: u64,
    pub req_calls: u64,
    pub req_new: u64,
    pub req_fast: u64,
    pub req_dedup_tex: u64,
    pub req_dedup_pending: u64,
    pub req_dedup_missing: u64,
    pub req_dedup_uploading: u64,
    pub retain_cancels: u64,
    pub events_drained: u64,
    pub uploads_applied: u64,
}

pub fn app_counters() -> AppCounters {
    let l = |c: &AtomicU64| c.load(Ordering::Relaxed);
    AppCounters {
        tex_hit: l(&TEX_HIT),
        tex_miss: l(&TEX_MISS),
        tex_evict: l(&TEX_EVICT),
        pix_hit: l(&PIX_HIT),
        pix_miss: l(&PIX_MISS),
        pix_evict: l(&PIX_EVICT),
        req_calls: l(&REQ_CALLS),
        req_new: l(&REQ_NEW),
        req_fast: l(&REQ_FAST),
        req_dedup_tex: l(&REQ_DEDUP_TEX),
        req_dedup_pending: l(&REQ_DEDUP_PENDING),
        req_dedup_missing: l(&REQ_DEDUP_MISSING),
        req_dedup_uploading: l(&REQ_DEDUP_UPLOADING),
        retain_cancels: l(&RETAIN_CANCELS),
        events_drained: l(&EVENTS_DRAINED),
        uploads_applied: l(&UPLOADS_APPLIED),
    }
}

/// Live sizes read straight off `AppState` at tick time (not counters).
#[derive(Debug, Clone, Copy, Default)]
pub struct Gauges {
    pub thumb_pending: usize,
    pub thumb_missing: usize,
    pub thumb_handles: usize,
    pub thumb_uploading: usize,
    pub pending_uploads: usize,
    pub active_ingests: usize,
    pub ingest_done: usize,
    pub ingest_total: usize,
    pub uploads_cap: usize,
    pub ingest_phase: IngestPhase,
    pub ingest_chan: u64,
    /// Batch-export in-flight items (bounded to ≤1 by the sequential batch job).
    pub export_active: u64,
    pub export_done: u64,
    pub export_failed: u64,
    /// Wall time (ms) of the last completed export item.
    pub export_last_ms: u64,
    /// Task 17 (spec 4.5 §9): most recent Develop `drive_viewer` frame time
    /// (ms) and the sparse producer's tiles-produced count that frame, plus
    /// the running max frame time since the last tick. All three read as 0.0/0
    /// when diag is off or no viewer frame has been driven yet.
    pub viewer_frame_ms: f64,
    pub viewer_frame_max_ms: f64,
    pub viewer_tiles_produced: u64,
}

/// Everything the log/overlay render, precomputed once per tick.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub dt: f64,
    pub frame_ms: f64,
    pub max_frame_ms: f64,
    pub repaint_forced: bool,
    pub jobs: JobStats,
    pub g: Gauges,
    // Per-second rates.
    pub tex_hit_per_s: f64,
    pub tex_miss_per_s: f64,
    pub tex_evict_per_s: f64,
    pub pix_hit_per_s: f64,
    pub pix_miss_per_s: f64,
    pub pix_evict_per_s: f64,
    // Per-frame counts (last frame).
    pub ev_per_frame: u64,
    pub req_per_frame: u64,
    pub uploads_per_frame: u64,
    // Last-frame request breakdown (deltas vs previous frame).
    pub req_new_f: u64,
    pub req_fast_f: u64,
    pub req_dedup_tex_f: u64,
    pub req_dedup_pending_f: u64,
    pub req_dedup_missing_f: u64,
    pub req_dedup_uploading_f: u64,
    // Cache sizes (caps are fixed constants, shown for context).
    pub cur: AppCounters,
}

#[allow(clippy::too_many_arguments)]
pub fn build_snapshot(
    dt: f64,
    prev: &AppCounters,
    cur: &AppCounters,
    prev_frame: &AppCounters,
    jobs: JobStats,
    g: Gauges,
    frame_ms: f64,
    max_frame_ms: f64,
    repaint_forced: bool,
) -> Snapshot {
    let d = |a: u64, b: u64| a.saturating_sub(b);
    Snapshot {
        dt,
        frame_ms,
        max_frame_ms,
        repaint_forced,
        jobs,
        g,
        tex_hit_per_s: compute_rate(d(cur.tex_hit, prev.tex_hit), dt),
        tex_miss_per_s: compute_rate(d(cur.tex_miss, prev.tex_miss), dt),
        tex_evict_per_s: compute_rate(d(cur.tex_evict, prev.tex_evict), dt),
        pix_hit_per_s: compute_rate(d(cur.pix_hit, prev.pix_hit), dt),
        pix_miss_per_s: compute_rate(d(cur.pix_miss, prev.pix_miss), dt),
        pix_evict_per_s: compute_rate(d(cur.pix_evict, prev.pix_evict), dt),
        ev_per_frame: d(cur.events_drained, prev_frame.events_drained),
        req_per_frame: d(cur.req_calls, prev_frame.req_calls),
        uploads_per_frame: d(cur.uploads_applied, prev_frame.uploads_applied),
        req_new_f: d(cur.req_new, prev_frame.req_new),
        req_fast_f: d(cur.req_fast, prev_frame.req_fast),
        req_dedup_tex_f: d(cur.req_dedup_tex, prev_frame.req_dedup_tex),
        req_dedup_pending_f: d(cur.req_dedup_pending, prev_frame.req_dedup_pending),
        req_dedup_missing_f: d(cur.req_dedup_missing, prev_frame.req_dedup_missing),
        req_dedup_uploading_f: d(cur.req_dedup_uploading, prev_frame.req_dedup_uploading),
        cur: *cur,
    }
}

/// One-line batch-export status for the log/overlay. Empty until an export has run
/// (so it never clutters the common case). Shows the in-flight gauge — which the
/// sequential batch job bounds to 1; a value >1 means concurrent exports are back —
/// plus cumulative done/failed and the last item's wall time (ms).
pub fn format_export_line(active: u64, done: u64, failed: u64, last_ms: u64) -> String {
    if active == 0 && done == 0 {
        return String::new();
    }
    format!("\n export  active {active}  done {done} failed {failed}  last {last_ms}ms")
}

/// Task 17 (spec 4.5 §9): one-line Develop viewer frame-time readout for the
/// log/overlay. Empty until a viewer frame has actually been driven
/// (positive `frame_ms`), so it never clutters the Library-only /
/// no-image-open case. Shows the last frame's time against the 16.6 ms
/// (60 fps) budget, this tick's max, and the sparse producer's last
/// tiles-produced count.
pub fn format_viewer_line(frame_ms: f64, max_ms: f64, tiles_produced: u64) -> String {
    if frame_ms <= 0.0 {
        return String::new();
    }
    const BUDGET_MS: f64 = 16.6;
    let over = if frame_ms > BUDGET_MS { " OVER" } else { "" };
    format!(
        "\n viewer  frame {frame_ms:.2}ms(max {max_ms:.2}) budget {BUDGET_MS:.1}ms{over}  tiles/f {tiles_produced}"
    )
}

/// Render the multi-line ~1/sec log block (also reused, compacted, by the overlay).
pub fn format_log(s: &Snapshot) -> String {
    let j = &s.jobs;
    let g = &s.g;
    let dedup =
        s.req_dedup_tex_f + s.req_dedup_pending_f + s.req_dedup_missing_f + s.req_dedup_uploading_f;
    let export_line = format_export_line(
        g.export_active,
        g.export_done,
        g.export_failed,
        g.export_last_ms,
    );
    let viewer_line = format_viewer_line(
        g.viewer_frame_ms,
        g.viewer_frame_max_ms,
        g.viewer_tiles_produced,
    );
    let ingest_line = if g.ingest_phase != IngestPhase::Idle {
        format!(
            "\n ingest  phase {ph} {idn}/{itot}  chan {chan}",
            ph = g.ingest_phase.label(),
            idn = g.ingest_done,
            itot = g.ingest_total,
            chan = g.ingest_chan,
        )
    } else {
        String::new()
    };
    format!(
        "[diag +{dt:.1}s] frame {fms:.1}ms(max {mx:.1}) ev/f {ev} repaint {rp}\n\
         \x20jobs  sub I/V/B {si}/{sv}/{sb}  disp {disp}  done {done}  cxl(pre){cxp}  panic {pan}\n\
         \x20      active {act}  pending I/V/B {pi}/{pv}/{pb}  cancel removed {crem}/absent {cabs}\n\
         \x20thumb req/f {req} = new {rn} + fast {rf} + dedup {dd} (tex {rt}/pend {rpd}/miss {rms}/upl {rup})\n\
         \x20      pending {tp}  uploading {tu}  handles {th}  missing {tm}  retain req {rc}\n\
         \x20cache tex h/s {thh:.0} m/s {thm:.0} ev/s {the:.0} | pix h/s {pxh:.0} m/s {pxm:.0} ev/s {pxe:.0}\n\
         \x20uploads {up}/{cap} cap  backlog {bk}\n\
         \x20ingest active {ai}  done {idn}/{itot}{ingest_line}{export_line}{viewer_line}",
        dt = s.dt,
        fms = s.frame_ms,
        mx = s.max_frame_ms,
        ev = s.ev_per_frame,
        rp = if s.repaint_forced { "forced" } else { "no" },
        si = j.submitted[ferrolite_jobs::Priority::Interactive.index()],
        sv = j.submitted[ferrolite_jobs::Priority::Visible.index()],
        sb = j.submitted[ferrolite_jobs::Priority::Background.index()],
        disp = j.dispatched,
        done = j.completed,
        cxp = j.cancelled_before_dispatch,
        pan = j.panicked,
        act = j.active,
        pi = j.pending[ferrolite_jobs::Priority::Interactive.index()],
        pv = j.pending[ferrolite_jobs::Priority::Visible.index()],
        pb = j.pending[ferrolite_jobs::Priority::Background.index()],
        crem = j.cancel_removed,
        cabs = j.cancel_absent,
        req = s.req_per_frame,
        rn = s.req_new_f,
        rf = s.req_fast_f,
        dd = dedup,
        rt = s.req_dedup_tex_f,
        rpd = s.req_dedup_pending_f,
        rms = s.req_dedup_missing_f,
        rup = s.req_dedup_uploading_f,
        tp = g.thumb_pending,
        tu = g.thumb_uploading,
        th = g.thumb_handles,
        tm = g.thumb_missing,
        rc = s.cur.retain_cancels,
        thh = s.tex_hit_per_s,
        thm = s.tex_miss_per_s,
        the = s.tex_evict_per_s,
        pxh = s.pix_hit_per_s,
        pxm = s.pix_miss_per_s,
        pxe = s.pix_evict_per_s,
        up = s.uploads_per_frame,
        cap = g.uploads_cap,
        bk = g.pending_uploads,
        ai = g.active_ingests,
        idn = g.ingest_done,
        itot = g.ingest_total,
        ingest_line = ingest_line,
        export_line = export_line,
        viewer_line = viewer_line,
    )
}

/// Per-frame diagnostic state held on `FerroliteApp`. Drives the ~1/sec tick,
/// per-frame deltas, and the overlay toggle. Cheap to hold when diag is off
/// (it is simply never ticked).
pub struct DiagState {
    last_tick: Option<Instant>,
    prev_tick: AppCounters,
    prev_frame: AppCounters,
    max_frame_ms: f64,
    /// Wired to a toggle keybinding + the overlay panel.
    pub overlay_visible: bool,
    last_snapshot: Option<Snapshot>,
    /// Wired to F10; the dedicated memory overlay (separate from the text one).
    pub mem_overlay_visible: bool,
    /// Growth-graph ring buffer for the memory overlay (~5 min at 1/sec).
    pub mem_history: crate::diag_mem::MemHistory,
}

impl DiagState {
    pub fn new() -> Self {
        Self {
            last_tick: None,
            prev_tick: AppCounters::default(),
            prev_frame: AppCounters::default(),
            max_frame_ms: 0.0,
            overlay_visible: overlay_enabled(),
            last_snapshot: None,
            mem_overlay_visible: false,
            mem_history: crate::diag_mem::MemHistory::new(300),
        }
    }

    /// Wired to a toggle keybinding (F9 in `update`).
    pub fn toggle_overlay(&mut self) {
        self.overlay_visible = !self.overlay_visible;
    }

    /// Wired to a toggle keybinding (F10 in `update`).
    pub fn toggle_mem_overlay(&mut self) {
        self.mem_overlay_visible = !self.mem_overlay_visible;
    }

    /// Read by the overlay panel.
    pub fn last_snapshot(&self) -> Option<&Snapshot> {
        self.last_snapshot.as_ref()
    }

    /// Call once at the end of every `update()`. Tracks per-frame deltas and the
    /// running max frame time; emits (and caches) a `Snapshot` at most ~1×/sec.
    pub fn tick(
        &mut self,
        now: Instant,
        jobs: JobStats,
        g: Gauges,
        frame_ms: f64,
        repaint_forced: bool,
    ) -> Option<Snapshot> {
        if frame_ms > self.max_frame_ms {
            self.max_frame_ms = frame_ms;
        }
        let cur = app_counters();
        let out = match self.last_tick {
            None => {
                // Establish baselines; no emit on the very first frame.
                self.last_tick = Some(now);
                None
            }
            Some(last) => {
                let dt = now.duration_since(last).as_secs_f64();
                if dt < 1.0 {
                    None
                } else {
                    let snap = build_snapshot(
                        dt,
                        &self.prev_tick,
                        &cur,
                        &self.prev_frame,
                        jobs,
                        g,
                        frame_ms,
                        self.max_frame_ms,
                        repaint_forced,
                    );
                    self.last_tick = Some(now);
                    self.prev_tick = cur;
                    self.max_frame_ms = 0.0;
                    // Task 17: reset the viewer frame-time max alongside the
                    // general per-tick max, so each ~1/sec snapshot shows the
                    // max over that window (not the lifetime max).
                    reset_viewer_frame_max();
                    self.last_snapshot = Some(snap.clone());
                    Some(snap)
                }
            }
        };
        // Per-frame delta baseline advances every frame.
        self.prev_frame = cur;
        out
    }
}

impl Default for DiagState {
    fn default() -> Self {
        Self::new()
    }
}

/// Compact multi-line text for the on-screen overlay (a denser view of the same
/// snapshot the log formats).
pub fn format_overlay(s: &Snapshot) -> String {
    let j = &s.jobs;
    let g = &s.g;
    let export_line = format_export_line(
        g.export_active,
        g.export_done,
        g.export_failed,
        g.export_last_ms,
    );
    let viewer_line = format_viewer_line(
        g.viewer_frame_ms,
        g.viewer_frame_max_ms,
        g.viewer_tiles_produced,
    );
    let ingest_line = if g.ingest_phase != IngestPhase::Idle {
        format!(
            "\n ingest  phase {ph} {idn}/{itot}  chan {chan}",
            ph = g.ingest_phase.label(),
            idn = g.ingest_done,
            itot = g.ingest_total,
            chan = g.ingest_chan,
        )
    } else {
        String::new()
    };
    format!(
        "frame {fms:.1}ms (max {mx:.1})  ev/f {ev}\n\
         jobs sub I/V/B {si}/{sv}/{sb}  active {act}\n\
         pending I/V/B {pi}/{pv}/{pb}\n\
         cancel removed {crem}/absent {cabs}  cxl(pre) {cxp}  panic {pan}\n\
         req/f {req} new {rn} fast {rf} dedup {dd}\n\
         thumb pending {tp} uploading {tu} handles {th} missing {tm}\n\
         tex h/s {thh:.0} ev/s {the:.0} | pix h/s {pxh:.0} ev/s {pxe:.0}\n\
         uploads/f {up}/{cap}  backlog {bk}\n\
         ingest {idn}/{itot} active {ai}{ingest_line}{export_line}{viewer_line}",
        fms = s.frame_ms,
        mx = s.max_frame_ms,
        ev = s.ev_per_frame,
        si = j.submitted[ferrolite_jobs::Priority::Interactive.index()],
        sv = j.submitted[ferrolite_jobs::Priority::Visible.index()],
        sb = j.submitted[ferrolite_jobs::Priority::Background.index()],
        act = j.active,
        pi = j.pending[ferrolite_jobs::Priority::Interactive.index()],
        pv = j.pending[ferrolite_jobs::Priority::Visible.index()],
        pb = j.pending[ferrolite_jobs::Priority::Background.index()],
        crem = j.cancel_removed,
        cabs = j.cancel_absent,
        cxp = j.cancelled_before_dispatch,
        pan = j.panicked,
        req = s.req_per_frame,
        rn = s.req_new_f,
        rf = s.req_fast_f,
        dd = s.req_dedup_tex_f
            + s.req_dedup_pending_f
            + s.req_dedup_missing_f
            + s.req_dedup_uploading_f,
        tp = g.thumb_pending,
        tu = g.thumb_uploading,
        th = g.thumb_handles,
        tm = g.thumb_missing,
        thh = s.tex_hit_per_s,
        the = s.tex_evict_per_s,
        pxh = s.pix_hit_per_s,
        pxe = s.pix_evict_per_s,
        up = s.uploads_per_frame,
        cap = g.uploads_cap,
        bk = g.pending_uploads,
        idn = g.ingest_done,
        itot = g.ingest_total,
        ai = g.active_ingests,
        ingest_line = ingest_line,
        export_line = export_line,
        viewer_line = viewer_line,
    )
}

/// Paint the diagnostics overlay: a non-interactive, top-right, monospace panel
/// drawn on the tooltip layer so it floats above the app. Call at the end of
/// `update()` only when the overlay is enabled AND visible.
pub fn draw_overlay(ctx: &egui::Context, s: &Snapshot) {
    egui::Area::new(egui::Id::new("ferrolite-diag-overlay"))
        .order(egui::Order::Tooltip)
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_black_alpha(200))
                .inner_margin(egui::Margin::same(8.0))
                .rounding(egui::Rounding::same(4.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format_overlay(s))
                            .monospace()
                            .size(11.0)
                            .color(egui::Color32::from_rgb(120, 255, 120)),
                    );
                });
        });
}

/// One-line close-path report (emitted from `on_exit`, where the overlay is
/// already gone). Shows work in flight at close and the bounded-join outcome.
pub fn format_shutdown(before: JobStats, joined: bool, timeout_ms: u64, on_exit_ms: f64) -> String {
    let pending: u64 = before.pending.iter().sum();
    let detach = if joined {
        String::new()
    } else {
        format!("(detach@{timeout_ms}ms)")
    };
    format!(
        "[diag close] active {act} pending {pend}  joined={joined}{detach}  on_exit {ms:.0}ms",
        act = before.active,
        pend = pending,
        ms = on_exit_ms,
    )
}

/// Per-ingest-job generation profile. Created only when `diag::enabled()`,
/// threaded through `ingest_job` as `Option<Arc<IngestProfile>>`. All methods
/// are pure accumulators (no global gate) so they unit-test directly; the caller
/// gates creation/use behind `enabled()`. `Relaxed` throughout — diagnostics,
/// not synchronization.
#[derive(Default)]
pub struct IngestProfile {
    decode_sum_us: AtomicU64,
    decode_max_us: AtomicU64,
    encode_sum_us: AtomicU64,
    encode_count: AtomicU64,
    upsert_sum_us: AtomicU64,
    upsert_batches: AtomicU64,
    chan_inflight: AtomicU64,
    chan_depth_max: AtomicU64,
    // Per-file decode µs, split by kind (RAW vs Standard) for per-kind p50; the
    // overall distribution is the two merged. Touched only when profiling is on;
    // a brief push per file, negligible vs a ~200ms decode.
    raw_us: Mutex<Vec<u32>>,
    std_us: Mutex<Vec<u32>>,
    slow: Mutex<Vec<SlowSample>>,
    // RAW byte-source tier counts across ALL files (not just slow): how far the
    // incremental read had to grow. `directed`+`grown`+`full` are the files that
    // missed the 1 MiB prefix (the former slow tail); `directed` means
    // offset-parsing found the exact preview span in one extra read; `full`
    // needed the whole file.
    prefix_hits: AtomicU64,
    directed: AtomicU64,
    grown: AtomicU64,
    full: AtomicU64,
}

impl IngestProfile {
    pub fn record_decode(&self, us: u64, is_raw: bool) {
        self.decode_sum_us.fetch_add(us, Ordering::Relaxed);
        self.decode_max_us.fetch_max(us, Ordering::Relaxed);
        let bucket = if is_raw { &self.raw_us } else { &self.std_us };
        if let Ok(mut v) = bucket.lock() {
            v.push(us as u32);
        }
    }
    pub fn record_encode(&self, us: u64) {
        self.encode_sum_us.fetch_add(us, Ordering::Relaxed);
        self.encode_count.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_upsert(&self, us: u64) {
        self.upsert_sum_us.fetch_add(us, Ordering::Relaxed);
        self.upsert_batches.fetch_add(1, Ordering::Relaxed);
    }
    /// Producer sent a row into the channel: bump inflight and track the peak.
    pub fn on_send(&self) {
        let depth = self.chan_inflight.fetch_add(1, Ordering::Relaxed) + 1;
        self.chan_depth_max.fetch_max(depth, Ordering::Relaxed);
    }
    /// Consumer took a row off the channel.
    pub fn on_recv(&self) {
        self.chan_inflight.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn decode_sum_us(&self) -> u64 {
        self.decode_sum_us.load(Ordering::Relaxed)
    }
    pub fn decode_max_us(&self) -> u64 {
        self.decode_max_us.load(Ordering::Relaxed)
    }
    pub fn encode_sum_us(&self) -> u64 {
        self.encode_sum_us.load(Ordering::Relaxed)
    }
    pub fn upsert_sum_us(&self) -> u64 {
        self.upsert_sum_us.load(Ordering::Relaxed)
    }
    pub fn upsert_batches(&self) -> u64 {
        self.upsert_batches.load(Ordering::Relaxed)
    }
    pub fn chan_depth_max(&self) -> u64 {
        self.chan_depth_max.load(Ordering::Relaxed)
    }
    pub fn chan_inflight(&self) -> u64 {
        self.chan_inflight.load(Ordering::Relaxed)
    }
    /// All per-file decode samples (RAW ∪ Standard), for overall percentiles.
    pub fn decode_samples(&self) -> Vec<u32> {
        let mut out = self.raw_samples();
        out.extend(self.std_samples());
        out
    }
    pub fn raw_samples(&self) -> Vec<u32> {
        self.raw_us.lock().map(|v| v.clone()).unwrap_or_default()
    }
    pub fn std_samples(&self) -> Vec<u32> {
        self.std_us.lock().map(|v| v.clone()).unwrap_or_default()
    }
    pub fn record_slow(&self, s: SlowSample) {
        if let Ok(mut v) = self.slow.lock() {
            v.push(s);
        }
    }
    pub fn slow_samples(&self) -> Vec<SlowSample> {
        self.slow.lock().map(|v| v.clone()).unwrap_or_default()
    }
    /// Count one RAW file's byte-source tier (how far the incremental read grew).
    pub fn record_source(&self, kind: ferrolite_decode::SourceKind) {
        match kind {
            ferrolite_decode::SourceKind::Prefix => &self.prefix_hits,
            ferrolite_decode::SourceKind::Directed => &self.directed,
            ferrolite_decode::SourceKind::Grown => &self.grown,
            ferrolite_decode::SourceKind::Full => &self.full,
        }
        .fetch_add(1, Ordering::Relaxed);
    }
    pub fn prefix_hits(&self) -> u64 {
        self.prefix_hits.load(Ordering::Relaxed)
    }
    pub fn directed(&self) -> u64 {
        self.directed.load(Ordering::Relaxed)
    }
    pub fn grown(&self) -> u64 {
        self.grown.load(Ordering::Relaxed)
    }
    pub fn full(&self) -> u64 {
        self.full.load(Ordering::Relaxed)
    }
}

/// Nearest-rank percentile of `samples` (µs). Returns 0 for an empty slice.
/// `pct` in 0.0..=1.0. Clones + sorts, so callers pass a snapshot, not a hot Vec.
pub fn percentile(samples: &[u32], pct: f64) -> u32 {
    if samples.is_empty() {
        return 0;
    }
    let mut v = samples.to_vec();
    v.sort_unstable();
    let rank = (pct * v.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(v.len() - 1);
    v[idx]
}

/// Plain, `format`-ready snapshot of one ingest job's generation profile.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct IngestSummary {
    pub files: usize,
    pub wall_s: f64,
    pub scan_s: f64,
    pub phase_a_s: f64,
    pub filter_s: f64,
    pub decode_par_s: f64,
    pub decode_sum_s: f64,
    pub cores: usize,
    pub decode_p50_ms: f64,
    pub decode_p95_ms: f64,
    pub decode_max_ms: f64,
    pub encode_sum_s: f64,
    pub encode_avg_ms: f64,
    pub upsert_batches: u64,
    pub upsert_avg_ms: f64,
    pub upsert_sum_s: f64,
    pub chan_depth_max: u64,
    pub producer_done_s: f64,
    pub consumer_done_s: f64,
    pub raw_count: usize,
    pub raw_p50_ms: f64,
    pub std_count: usize,
    pub std_p50_ms: f64,
}

/// Render the one-shot per-ingest summary block.
pub fn format_ingest_summary(s: &IngestSummary) -> String {
    let speedup = if s.decode_par_s > 0.0 {
        s.decode_sum_s / s.decode_par_s
    } else {
        0.0
    };
    let tail = (s.consumer_done_s - s.producer_done_s).max(0.0);
    format!(
        "[ingest-summary] {files} files in {wall:.1}s\n\
         \x20phases  scan {scan:.1}s  phaseA {pa:.1}s  filter {flt:.1}s  decode(par) {dec:.1}s\n\
         \x20decode  sum {dsum:.0}s / {cores} cores -> {sp:.1}x | p50 {p50:.0}ms p95 {p95:.0}ms max {mx:.0}ms\n\
         \x20encode  sum {esum:.1}s  avg {eavg:.0}ms\n\
         \x20upsert  {ub} batches  avg {uavg:.0}ms (sum {usum:.1}s)\n\
         \x20channel max depth {chan}  | producer done@{pd:.1}s  consumer done@{cd:.1}s  (tail {tail:.1}s)\n\
         \x20by kind  RAW {rawn} (decode p50 {rawp50:.0}ms) | std {stdn} (decode p50 {stdp50:.0}ms)",
        files = s.files,
        wall = s.wall_s,
        scan = s.scan_s,
        pa = s.phase_a_s,
        flt = s.filter_s,
        dec = s.decode_par_s,
        dsum = s.decode_sum_s,
        cores = s.cores,
        sp = speedup,
        p50 = s.decode_p50_ms,
        p95 = s.decode_p95_ms,
        mx = s.decode_max_ms,
        esum = s.encode_sum_s,
        eavg = s.encode_avg_ms,
        ub = s.upsert_batches,
        uavg = s.upsert_avg_ms,
        usum = s.upsert_sum_s,
        chan = s.chan_depth_max,
        pd = s.producer_done_s,
        cd = s.consumer_done_s,
        tail = tail,
        rawn = s.raw_count,
        rawp50 = s.raw_p50_ms,
        stdn = s.std_count,
        stdp50 = s.std_p50_ms,
    )
}

/// Emit the ingest summary to the diag log sink (stderr + session file).
/// Gated: no-op when diag is off.
pub fn emit_ingest_summary(s: &IngestSummary) {
    if !enabled() {
        return;
    }
    write_log(&format_ingest_summary(s));
}

/// Max slowest files listed in the aggregate block.
const SLOW_TOP_N: usize = 10;

/// One slow-decode sample, recorded only when profiling is on and a file's total
/// decode time crossed `slow_threshold_ms()`. Constructed by `ingest.rs` via
/// `IngestProfile::record_slow`.
#[derive(Debug, Clone)]
pub struct SlowSample {
    pub decode_ms: f64,
    /// Which byte-source tier the file used (`None` for non-RAW).
    pub source_kind: Option<ferrolite_decode::SourceKind>,
    /// Bytes read to satisfy the decode (0 when unknown / non-RAW).
    pub source_bytes: u64,
    // Per-stage sub-timings (ms); together with the residual `rest` they split
    // `decode_ms`. 0.0 when the stage was not measured / not applicable.
    pub source_acquire_ms: f64,
    pub get_decoder_ms: f64,
    pub raw_metadata_ms: f64,
    pub raw_dims_ms: f64,
    pub extract_ms: f64,
    pub orient_ms: f64,
    pub is_raw: bool,
    pub src_w: u32,
    pub src_h: u32,
    pub source: ferrolite_decode::PreviewSource,
    pub model: String,
    pub path: String,
    /// Standard-file decode wall time (ms); 0.0 for RAW.
    pub std_decode_ms: f64,
    /// JPEG DCT scale (1/2/4/8); None for RAW / non-JPEG.
    pub dct_scale: Option<u8>,
}

impl SlowSample {
    fn megapixels(&self) -> f64 {
        (self.src_w as f64 * self.src_h as f64) / 1_000_000.0
    }
    /// Sum of the measured sub-stages (ms).
    fn measured_ms(&self) -> f64 {
        self.source_acquire_ms
            + self.get_decoder_ms
            + self.raw_metadata_ms
            + self.raw_dims_ms
            + self.extract_ms
            + self.orient_ms
            + self.std_decode_ms
    }
    /// Unattributed remainder of `decode_ms` after the measured stages.
    fn rest_ms(&self) -> f64 {
        (self.decode_ms - self.measured_ms()).max(0.0)
    }
}

/// Short ASCII label for a preview source (used in logs).
pub fn source_label(s: ferrolite_decode::PreviewSource) -> &'static str {
    use ferrolite_decode::PreviewSource::*;
    match s {
        EmbeddedPreview => "preview",
        FullImage => "full_image",
        EmbeddedThumbnail => "thumbnail",
    }
}

/// Short ASCII label for which byte-source tier a file used.
pub fn source_kind_label(k: Option<ferrolite_decode::SourceKind>) -> &'static str {
    match k {
        Some(ferrolite_decode::SourceKind::Prefix) => "prefix",
        Some(ferrolite_decode::SourceKind::Directed) => "directed",
        Some(ferrolite_decode::SourceKind::Grown) => "grown",
        Some(ferrolite_decode::SourceKind::Full) => "full",
        None => "n/a",
    }
}

/// Slow-file logging threshold in ms: `FERROLITE_DIAG_SLOW_MS` if set and valid,
/// else 500. Cached once.
pub fn slow_threshold_ms() -> f64 {
    static T: OnceLock<f64> = OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("FERROLITE_DIAG_SLOW_MS")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| *v >= 0.0)
            .unwrap_or(500.0)
    })
}

/// Quote `s` for diagnostic output while guaranteeing the result is ASCII-only.
/// `escape_default()` turns every non-ASCII/control char into a `\xNN`/`\u{...}`
/// escape, so free-form fields (camera model, filesystem path) can never inject
/// mojibake into the log even on non-ASCII Windows paths.
fn ascii_quote(s: &str) -> String {
    format!("\"{}\"", s.escape_default())
}

/// One-line slow-file record. ASCII-only. Splits `decode_ms` into its stages
/// (source-acquire / get_decoder / raw_metadata / dummy-geometry / embedded
/// extract / orient) plus the unattributed `rest`, and tags the byte-source tier.
pub fn format_slow_line(s: &SlowSample) -> String {
    format!(
        "[ingest-slow] {dec:.0}ms [{tier} {smb:.1}MB] (acquire {acq:.0} / decoder {gd:.0} / meta {rm:.0} / \
         dims {rd:.0} / extract {ex:.0} / orient {or:.0} / stddec {sd:.0} / rest {rest:.0}) \
         {kind} {w}x{h} {mp:.1}MP {dct} via {src} model={model} {path}",
        dec = s.decode_ms,
        tier = source_kind_label(s.source_kind),
        smb = s.source_bytes as f64 / 1_048_576.0,
        acq = s.source_acquire_ms,
        gd = s.get_decoder_ms,
        rm = s.raw_metadata_ms,
        rd = s.raw_dims_ms,
        ex = s.extract_ms,
        or = s.orient_ms,
        sd = s.std_decode_ms,
        rest = s.rest_ms(),
        kind = if s.is_raw { "RAW" } else { "std" },
        w = s.src_w,
        h = s.src_h,
        mp = s.megapixels(),
        dct = match s.dct_scale {
            Some(n) => format!("dct 1/{n}"),
            None => String::from("dct -"),
        },
        src = source_label(s.source),
        model = ascii_quote(&s.model),
        path = ascii_quote(&s.path),
    )
}

/// End-of-ingest aggregate over all slow samples. ASCII-only.
pub fn format_slow_aggregate(samples: &[SlowSample], total_files: usize) -> String {
    use ferrolite_decode::PreviewSource;
    if samples.is_empty() {
        return format!("[ingest-slow-summary] 0 slow files (of {total_files})");
    }
    let n = samples.len();
    let share = if total_files > 0 {
        100.0 * n as f64 / total_files as f64
    } else {
        0.0
    };
    // Per-stage totals (seconds) across the slow set, so the dominant stage is
    // obvious at a glance.
    let sum_s = |sel: fn(&SlowSample) -> f64| samples.iter().map(sel).sum::<f64>() / 1000.0;
    let acquire_s = sum_s(|s| s.source_acquire_ms);
    let decoder_s = sum_s(|s| s.get_decoder_ms);
    let meta_s = sum_s(|s| s.raw_metadata_ms);
    let dims_s = sum_s(|s| s.raw_dims_ms);
    let extract_sum_s = sum_s(|s| s.extract_ms);
    let orient_sum_s = sum_s(|s| s.orient_ms);
    let rest_s = sum_s(|s| s.rest_ms());

    // By byte-source tier: how far the read had to grow to satisfy each slow file.
    let count_kind = |k: ferrolite_decode::SourceKind| {
        samples.iter().filter(|s| s.source_kind == Some(k)).count()
    };
    let prefix_n = count_kind(ferrolite_decode::SourceKind::Prefix);
    let grown_n = count_kind(ferrolite_decode::SourceKind::Grown);
    let full_n = count_kind(ferrolite_decode::SourceKind::Full);

    let count_src = |src: PreviewSource| samples.iter().filter(|s| s.source == src).count();
    let by_source = format!(
        "full_image {f} | thumbnail {t} | preview {p}",
        f = count_src(PreviewSource::FullImage),
        t = count_src(PreviewSource::EmbeddedThumbnail),
        p = count_src(PreviewSource::EmbeddedPreview),
    );

    // by model: stable order by descending count, then name.
    let mut models: Vec<&str> = samples.iter().map(|s| s.model.as_str()).collect();
    models.sort_unstable();
    models.dedup();
    let mut model_rows: Vec<(usize, String)> = models
        .iter()
        .map(|m| {
            let ms: Vec<u32> = samples
                .iter()
                .filter(|s| s.model == *m)
                .map(|s| s.decode_ms as u32)
                .collect();
            (
                ms.len(),
                format!(
                    "{} {} (p50 {}ms)",
                    ascii_quote(m),
                    ms.len(),
                    percentile(&ms, 0.5)
                ),
            )
        })
        .collect();
    model_rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let by_model = model_rows
        .iter()
        .map(|(_, r)| r.as_str())
        .collect::<Vec<_>>()
        .join(" | ");

    // top-N slowest by decode_ms desc, tie-broken by path for determinism.
    let mut top: Vec<&SlowSample> = samples.iter().collect();
    top.sort_by(|a, b| {
        b.decode_ms
            .partial_cmp(&a.decode_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    let top_lines = top
        .iter()
        .take(SLOW_TOP_N)
        .map(|s| {
            format!(
                "  {dec:.0}ms {w}x{h} via {src} {path}",
                dec = s.decode_ms,
                w = s.src_w,
                h = s.src_h,
                src = source_label(s.source),
                path = ascii_quote(&s.path),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "[ingest-slow-summary] {n} slow files ({share:.1}% of {total_files})\n\
         \x20by path    prefix {pfx} | grown {grn} | full {full}\n\
         \x20stage sums acquire {acq:.1}s / decoder {gd:.1}s / meta {rm:.1}s / dims {rd:.1}s / \
         extract {es:.1}s / orient {os:.1}s / rest {rest:.1}s\n\
         \x20by source  {by_source}\n\
         \x20by model   {by_model}\n\
         \x20top {topn} slowest:\n{top_lines}",
        n = n,
        share = share,
        total_files = total_files,
        pfx = prefix_n,
        grn = grown_n,
        full = full_n,
        acq = acquire_s,
        gd = decoder_s,
        rm = meta_s,
        rd = dims_s,
        es = extract_sum_s,
        os = orient_sum_s,
        rest = rest_s,
        by_source = by_source,
        by_model = by_model,
        topn = top.len().min(SLOW_TOP_N),
        top_lines = top_lines,
    )
}

/// Emit the slow aggregate to the diag sink. No-op when diag is off.
pub fn emit_slow_aggregate(samples: &[SlowSample], total_files: usize) {
    if !enabled() {
        return;
    }
    write_log(&format_slow_aggregate(samples, total_files));
}

/// One-line RAW byte-source tier split across ALL files: how far the incremental
/// read had to grow. `prefix` = satisfied by the 1 MiB fast path; `directed` =
/// offset-parsing found the exact preview span in one extra read;
/// `grown`/`full` fell back to the tiered read (the former mmap-fallback tail);
/// `full` needed the whole file.
pub fn format_source_split(prefix: u64, directed: u64, grown: u64, full: u64) -> String {
    let total = prefix + directed + grown + full;
    let non_prefix = directed + grown + full;
    let pct = if total > 0 {
        100.0 * non_prefix as f64 / total as f64
    } else {
        0.0
    };
    format!(
        "[ingest-source] RAW byte-source: prefix {prefix} | directed {directed} | grown {grown} \
         | full {full} ({pct:.1}% needed >1MiB of {total})"
    )
}

/// Emit the RAW byte-source split to the diag sink. No-op when diag is off.
pub fn emit_source_split(prefix: u64, directed: u64, grown: u64, full: u64) {
    if !enabled() {
        return;
    }
    write_log(&format_source_split(prefix, directed, grown, full));
}

/// Point-in-time view of the ingest [`AdaptiveReadGate`](crate::read_gate::AdaptiveReadGate)
/// for the one-shot `[ingest-concurrency]` diag line. Mirrors
/// [`ControllerSnapshot`](crate::read_gate::ControllerSnapshot) plus the two
/// fields the gate itself doesn't track: a best-effort in-flight peak and
/// whether the gate is running pinned (adaptation disabled).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConcurrencySnapshot {
    pub limit: usize,
    pub rtt_min_us: u64,
    pub rtt_recent_us: u64,
    pub gradient: f64,
    pub inflight_peak: usize,
    pub pinned: bool,
}

/// Render the one-shot ingest read-concurrency summary line.
pub fn format_ingest_concurrency(s: &ConcurrencySnapshot) -> String {
    format!(
        "[ingest-concurrency] limit {limit}  rtt_min {rmin}us  rtt_recent {rrec}us  \
         gradient {grad:.2}  inflight_peak {peak}  pinned {pinned}",
        limit = s.limit,
        rmin = s.rtt_min_us,
        rrec = s.rtt_recent_us,
        grad = s.gradient,
        peak = s.inflight_peak,
        pinned = s.pinned,
    )
}

/// Emit the ingest read-concurrency summary to the diag sink. No-op when diag
/// is off.
pub fn emit_ingest_concurrency(s: &ConcurrencySnapshot) {
    if !enabled() {
        return;
    }
    write_log(&format_ingest_concurrency(s));
}

/// Bytes pre-read to force + time the disk IO a preview decode pages in (headless
/// `thumbnail_blocking` bench path). ~2 MiB covers the embedded preview prefix.
const PROBE_READ_BYTES: usize = 2 << 20;
/// Emit a running blocking-profile summary every this many profiled thumbnails.
const BLOCKING_SUMMARY_EVERY: u64 = 2;

static BLK_COUNT: AtomicU64 = AtomicU64::new(0);
static BLK_IO_US: AtomicU64 = AtomicU64::new(0);
static BLK_DECODE_US: AtomicU64 = AtomicU64::new(0);
static BLK_ENCODE_US: AtomicU64 = AtomicU64::new(0);
static BLK_WRITE_US: AtomicU64 = AtomicU64::new(0);
static BLK_READ_BYTES: AtomicU64 = AtomicU64::new(0);

/// Force + time the cold disk read for `path` (also warms the OS cache so the
/// decode timed next reflects CPU only). Returns the read duration in µs. Used
/// only by `ingest::thumbnail_blocking` (bench/test). Callers gate on `enabled()`.
pub fn measure_read(path: &Path) -> u64 {
    use std::io::Read;
    let t = Instant::now();
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut buf = vec![0u8; PROBE_READ_BYTES];
        if let Ok(n) = f.read(&mut buf) {
            BLK_READ_BYTES.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
    t.elapsed().as_micros() as u64
}

/// Record one headless blocking-thumbnail's phase timings (µs) and print a
/// cumulative `[thumb-blocking]` summary every `BLOCKING_SUMMARY_EVERY` files.
/// Gated: no-op when diag is off.
pub fn record_blocking(io_us: u64, decode_us: u64, encode_us: u64, write_us: u64) {
    if !enabled() {
        return;
    }
    let n = BLK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let io = BLK_IO_US.fetch_add(io_us, Ordering::Relaxed) + io_us;
    let dec = BLK_DECODE_US.fetch_add(decode_us, Ordering::Relaxed) + decode_us;
    let enc = BLK_ENCODE_US.fetch_add(encode_us, Ordering::Relaxed) + encode_us;
    let wr = BLK_WRITE_US.fetch_add(write_us, Ordering::Relaxed) + write_us;
    if !n.is_multiple_of(BLOCKING_SUMMARY_EVERY) {
        return;
    }
    let bytes = BLK_READ_BYTES.load(Ordering::Relaxed);
    let mbps = if io > 0 {
        bytes as f64 / io as f64
    } else {
        0.0
    };
    let nf = n as f64;
    write_log(&format!(
        "[thumb-blocking] n={n}  avg/file: io={:.1}ms decode={:.1}ms encode={:.1}ms write={:.1}ms \
         | read {:.0}MB @ {:.1}MB/s",
        io as f64 / 1000.0 / nf,
        dec as f64 / 1000.0 / nf,
        enc as f64 / 1000.0 / nf,
        wr as f64 / 1000.0 / nf,
        bytes as f64 / 1e6,
        mbps,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_maps_all_documented_values() {
        assert_eq!(parse_mode(None), DiagMode::Off);
        assert_eq!(parse_mode(Some("")), DiagMode::Off);
        assert_eq!(parse_mode(Some("0")), DiagMode::Off);
        assert_eq!(parse_mode(Some("off")), DiagMode::Off);
        assert_eq!(parse_mode(Some("log")), DiagMode::Log);
        assert_eq!(parse_mode(Some(" LOG ")), DiagMode::Log);
        assert_eq!(parse_mode(Some("overlay")), DiagMode::Overlay);
        assert_eq!(parse_mode(Some("1")), DiagMode::Both);
        assert_eq!(parse_mode(Some("both")), DiagMode::Both);
    }

    #[test]
    fn mode_flags_map_each_variant_correctly() {
        // Off: neither log nor overlay.
        assert!(!mode_logs(DiagMode::Off) && !mode_overlays(DiagMode::Off));
        // Log: log only.
        assert!(mode_logs(DiagMode::Log) && !mode_overlays(DiagMode::Log));
        // Overlay: overlay only.
        assert!(!mode_logs(DiagMode::Overlay) && mode_overlays(DiagMode::Overlay));
        // Both: log and overlay.
        assert!(mode_logs(DiagMode::Both) && mode_overlays(DiagMode::Both));
    }

    #[test]
    fn compute_rate_handles_zero_dt() {
        assert_eq!(compute_rate(100, 0.0), 0.0);
        assert_eq!(compute_rate(100, 2.0), 50.0);
        assert_eq!(compute_rate(0, 1.0), 0.0);
    }

    #[test]
    fn classify_request_prioritises_textured_then_pending_then_missing() {
        assert_eq!(
            classify_request(false, false, false, false),
            ReqOutcome::NewSubmit
        );
        assert_eq!(
            classify_request(true, false, false, false),
            ReqOutcome::DedupTextured
        );
        assert_eq!(
            classify_request(false, true, false, false),
            ReqOutcome::DedupPending
        );
        assert_eq!(
            classify_request(false, false, true, false),
            ReqOutcome::DedupMissing
        );
        // Textured wins when multiple guards are true (matches request_thumbnail
        // guard order: textures, then pending, then missing).
        assert_eq!(
            classify_request(true, true, true, false),
            ReqOutcome::DedupTextured
        );
    }

    #[test]
    fn classify_request_ranks_uploading_after_missing_before_new() {
        // uploading-only hit → DedupUploading
        assert_eq!(
            classify_request(false, false, false, true),
            ReqOutcome::DedupUploading
        );
        // none set → NewSubmit
        assert_eq!(
            classify_request(false, false, false, false),
            ReqOutcome::NewSubmit
        );
        // precedence: textured wins over uploading
        assert_eq!(
            classify_request(true, false, false, true),
            ReqOutcome::DedupTextured
        );
        // missing wins over uploading
        assert_eq!(
            classify_request(false, false, true, true),
            ReqOutcome::DedupMissing
        );
    }

    fn sample_gauges() -> Gauges {
        Gauges {
            thumb_pending: 640,
            thumb_missing: 0,
            thumb_handles: 640,
            thumb_uploading: 0,
            pending_uploads: 210,
            active_ingests: 0,
            ingest_done: 3320,
            ingest_total: 3320,
            uploads_cap: 16,
            ingest_phase: IngestPhase::Idle,
            ingest_chan: 0,
            export_active: 0,
            export_done: 0,
            export_failed: 0,
            export_last_ms: 0,
            viewer_frame_ms: 0.0,
            viewer_frame_max_ms: 0.0,
            viewer_tiles_produced: 0,
        }
    }

    #[test]
    fn build_snapshot_computes_per_second_rates() {
        let prev = AppCounters {
            tex_hit: 100,
            ..Default::default()
        };
        let cur = AppCounters {
            tex_hit: 140,
            ..Default::default()
        };
        let jobs = ferrolite_jobs::JobStats::default();
        let s = build_snapshot(
            2.0,
            &prev,
            &cur,
            &prev,
            jobs,
            sample_gauges(),
            6.2,
            11.0,
            true,
        );
        // 40 hits over 2s = 20/s.
        assert!((s.tex_hit_per_s - 20.0).abs() < 1e-6);
    }

    #[test]
    fn format_log_contains_key_fields() {
        let cur = AppCounters {
            req_calls: 44,
            req_new: 2,
            req_fast: 1,
            req_dedup_tex: 30,
            req_dedup_pending: 11,
            ..Default::default()
        };
        let mut jobs = ferrolite_jobs::JobStats::default();
        jobs.submitted[ferrolite_jobs::Priority::Visible.index()] = 812;
        jobs.active = 6;
        jobs.pending[ferrolite_jobs::Priority::Visible.index()] = 634;
        let s = build_snapshot(
            1.0,
            &AppCounters::default(),
            &cur,
            &cur,
            jobs,
            sample_gauges(),
            6.2,
            11.0,
            true,
        );
        let out = format_log(&s);
        assert!(out.contains("[diag"), "has the diag prefix");
        assert!(out.contains("frame 6.2ms"), "shows frame time");
        assert!(
            out.contains("sub I/V/B 0/812/0"),
            "shows per-priority submits"
        );
        assert!(out.contains("pending 640"), "shows lazy-load pending gauge");
        assert!(out.contains("uploads"), "shows uploads line");
    }

    #[test]
    fn format_overlay_contains_core_gauges() {
        let mut jobs = ferrolite_jobs::JobStats::default();
        jobs.pending[ferrolite_jobs::Priority::Visible.index()] = 634;
        jobs.active = 6;
        let s = build_snapshot(
            1.0,
            &AppCounters::default(),
            &AppCounters::default(),
            &AppCounters::default(),
            jobs,
            sample_gauges(),
            6.2,
            11.0,
            false,
        );
        let out = format_overlay(&s);
        assert!(out.contains("frame"), "overlay shows frame time");
        assert!(out.contains("active 6"), "overlay shows active jobs");
        assert!(out.contains("pending"), "overlay shows a pending gauge");
    }

    #[test]
    fn format_export_line_hidden_until_export_runs_then_shows_gauges() {
        // Common case: nothing exported yet → empty (no clutter in the overlay/log).
        assert_eq!(format_export_line(0, 0, 0, 0), "");
        // After exports run, show in-flight + cumulative counts + last item time.
        let line = format_export_line(1, 3, 1, 4200);
        assert!(line.contains("active 1"), "shows in-flight gauge");
        assert!(line.contains("done 3"), "shows completed count");
        assert!(line.contains("failed 1"), "shows failed count");
        assert!(line.contains("last 4200ms"), "shows last item wall time");
    }

    #[test]
    fn format_viewer_line_hidden_until_a_frame_is_recorded_then_shows_budget() {
        // Common case: no Develop viewer frame recorded yet → empty (diag off, or
        // still in Library) — no clutter.
        assert_eq!(format_viewer_line(0.0, 0.0, 0), "");
        // Under budget: no OVER marker.
        let under = format_viewer_line(10.0, 12.0, 5);
        assert!(under.contains("frame 10.00ms"), "shows last frame time");
        assert!(under.contains("max 12.00"), "shows this tick's max");
        assert!(
            under.contains("tiles/f 5"),
            "shows tiles produced this frame"
        );
        assert!(!under.contains("OVER"), "under budget is not flagged");
        // Over the ~16.6ms (60fps) budget: flagged so it's obvious at a glance.
        let over = format_viewer_line(20.0, 20.0, 32);
        assert!(over.contains("OVER"), "over-budget frame is flagged");
    }

    #[test]
    fn format_log_includes_export_line_when_active() {
        let g = Gauges {
            export_active: 1,
            export_done: 2,
            ..sample_gauges()
        };
        let s = build_snapshot(
            1.0,
            &AppCounters::default(),
            &AppCounters::default(),
            &AppCounters::default(),
            ferrolite_jobs::JobStats::default(),
            g,
            6.2,
            11.0,
            false,
        );
        assert!(format_log(&s).contains("export  active 1  done 2"));
    }

    #[test]
    fn format_shutdown_reports_join_result_and_counts() {
        let mut before = ferrolite_jobs::JobStats {
            active: 6,
            ..Default::default()
        };
        before.pending[ferrolite_jobs::Priority::Visible.index()] = 640;
        let detached = format_shutdown(before, false, 75, 78.0);
        assert!(detached.contains("[diag close]"));
        assert!(detached.contains("active 6"));
        assert!(detached.contains("pending 640"));
        assert!(detached.contains("joined=false"));
        assert!(detached.contains("detach@75ms"));
        assert!(detached.contains("on_exit 78"));

        let joined = format_shutdown(ferrolite_jobs::JobStats::default(), true, 75, 3.0);
        assert!(joined.contains("joined=true"));
        assert!(!joined.contains("detach@"), "no detach note when joined");
    }

    #[test]
    fn percentile_picks_expected_values() {
        let v: Vec<u32> = (1..=100).collect(); // 1..100
        assert_eq!(percentile(&v, 0.5), 50);
        assert_eq!(percentile(&v, 0.95), 95);
        assert_eq!(percentile(&v, 1.0), 100);
        assert_eq!(percentile(&[], 0.5), 0, "empty → 0");
        assert_eq!(percentile(&[7], 0.5), 7, "single element");
    }

    #[test]
    fn ingest_profile_accumulates_per_kind() {
        let p = IngestProfile::default();
        p.record_decode(100_000, true); // 100ms RAW
        p.record_decode(300_000, true); // 300ms RAW
        p.record_decode(10_000, false); // 10ms std
        p.record_encode(40_000);
        p.record_upsert(380_000);
        p.on_send();
        p.on_send();
        p.on_recv();
        assert_eq!(p.decode_samples().len(), 3);
        assert_eq!(p.raw_samples().len(), 2);
        assert_eq!(p.std_samples().len(), 1);
        assert_eq!(p.decode_sum_us(), 410_000);
        assert_eq!(p.decode_max_us(), 300_000);
        assert_eq!(p.encode_sum_us(), 40_000);
        assert_eq!(p.upsert_sum_us(), 380_000);
        assert_eq!(p.upsert_batches(), 1);
        assert_eq!(p.chan_depth_max(), 2, "peak inflight was 2 before the recv");
    }

    #[test]
    fn format_ingest_summary_is_ascii_only() {
        let s = IngestSummary {
            files: 3320,
            wall_s: 251.1,
            decode_sum_s: 2438.0,
            decode_par_s: 250.9,
            cores: 10,
            ..Default::default()
        };
        let out = format_ingest_summary(&s);
        assert!(
            out.is_ascii(),
            "ingest summary must be ASCII (no mojibake on Windows); got: {out}"
        );
    }

    #[test]
    fn format_ingest_summary_contains_all_sections() {
        let s = IngestSummary {
            files: 2730,
            wall_s: 412.3,
            scan_s: 3.1,
            phase_a_s: 45.2,
            filter_s: 38.4,
            decode_par_s: 310.7,
            decode_sum_s: 2100.0,
            cores: 10,
            decode_p50_ms: 210.0,
            decode_p95_ms: 800.0,
            decode_max_ms: 3100.0,
            encode_sum_s: 180.0,
            encode_avg_ms: 66.0,
            upsert_batches: 21,
            upsert_avg_ms: 380.0,
            upsert_sum_s: 8.0,
            chan_depth_max: 640,
            producer_done_s: 320.1,
            consumer_done_s: 412.3,
            raw_count: 2600,
            raw_p50_ms: 230.0,
            std_count: 130,
            std_p50_ms: 12.0,
        };
        let out = format_ingest_summary(&s);
        assert!(out.contains("[ingest-summary] 2730 files in 412.3s"));
        assert!(out.contains("scan 3.1s"));
        assert!(out.contains("phaseA 45.2s"));
        assert!(out.contains("filter 38.4s"));
        assert!(out.contains("decode(par) 310.7s"));
        assert!(out.contains("6.8x"), "speedup = 2100/310.7 ≈ 6.8");
        assert!(out.contains("p50 210ms p95 800ms max 3100ms"));
        assert!(out.contains("tail 92.2s"), "consumer_done - producer_done");
        assert!(out.contains("RAW 2600"));
        assert!(out.contains("std 130"));
    }

    #[test]
    fn format_log_shows_ingest_line_when_a_phase_is_active() {
        let mut g = sample_gauges();
        g.ingest_phase = IngestPhase::Decode;
        g.ingest_chan = 512;
        g.ingest_done = 1450;
        g.ingest_total = 2730;
        let s = build_snapshot(
            1.0,
            &AppCounters::default(),
            &AppCounters::default(),
            &AppCounters::default(),
            ferrolite_jobs::JobStats::default(),
            g,
            5.0,
            5.0,
            false,
        );
        let out = format_log(&s);
        assert!(
            out.contains("ingest  phase decode 1450/2730"),
            "shows phase + progress"
        );
        assert!(out.contains("chan 512"), "shows channel depth");
    }

    #[test]
    fn format_log_hides_ingest_line_when_idle() {
        let mut g = sample_gauges();
        g.ingest_phase = IngestPhase::Idle;
        let s = build_snapshot(
            1.0,
            &AppCounters::default(),
            &AppCounters::default(),
            &AppCounters::default(),
            ferrolite_jobs::JobStats::default(),
            g,
            5.0,
            5.0,
            false,
        );
        assert!(
            !format_log(&s).contains(" ingest  phase"),
            "no ingest line when idle"
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn slow_sample(
        decode_ms: f64,
        ex: f64,
        or_: f64,
        w: u32,
        h: u32,
        src: ferrolite_decode::PreviewSource,
        model: &str,
        path: &str,
    ) -> SlowSample {
        // Default the new stage timings to 0 and the tier to Grown, so the
        // legacy `extract`/`orient`/`rest` assertions stay meaningful (rest =
        // decode - extract - orient when the other stages are zero).
        SlowSample {
            decode_ms,
            source_kind: Some(ferrolite_decode::SourceKind::Grown),
            source_bytes: 8 << 20,
            source_acquire_ms: 0.0,
            get_decoder_ms: 0.0,
            raw_metadata_ms: 0.0,
            raw_dims_ms: 0.0,
            extract_ms: ex,
            orient_ms: or_,
            is_raw: true,
            src_w: w,
            src_h: h,
            source: src,
            model: model.to_string(),
            path: path.to_string(),
            std_decode_ms: 0.0,
            dct_scale: None,
        }
    }

    #[test]
    fn format_slow_line_has_all_fields_ascii() {
        use ferrolite_decode::PreviewSource;
        let s = slow_sample(
            5305.0,
            5100.0,
            190.0,
            6048,
            4032,
            PreviewSource::FullImage,
            "ILCE-7M4",
            "C:/x/DSC1234.ARW",
        );
        let out = format_slow_line(&s);
        assert!(out.starts_with("[ingest-slow]"));
        assert!(out.contains("5305ms"));
        assert!(out.contains("[grown 8.0MB]"), "shows tier tag + bytes read");
        assert!(out.contains("acquire 0"));
        assert!(out.contains("extract 5100"));
        assert!(out.contains("orient 190"));
        assert!(out.contains("rest 15")); // 5305 - 5100 - 190 (other stages 0)
        assert!(out.contains("6048x4032"));
        assert!(out.contains("24.4MP"));
        assert!(out.contains("via full_image"));
        assert!(out.contains("ILCE-7M4"));
        assert!(out.is_ascii());
    }

    #[test]
    fn format_slow_line_shows_std_decode_and_dct() {
        use ferrolite_decode::PreviewSource;
        let mut s = slow_sample(
            5305.0,
            5100.0,
            190.0,
            6048,
            4032,
            PreviewSource::FullImage,
            "ILCE-7M4",
            "C:/x/DSC1234.ARW",
        );
        s.is_raw = false;
        s.std_decode_ms = 48.0;
        s.dct_scale = Some(8);
        let out = format_slow_line(&s);
        assert!(out.contains("stddec 48"));
        assert!(out.contains("dct 1/8"));
    }

    #[test]
    fn format_source_split_reports_counts_and_pct() {
        let out = format_source_split(2900, 60, 350, 36);
        assert!(out.starts_with("[ingest-source]"));
        assert!(out.contains("prefix 2900"));
        assert!(out.contains("directed 60"));
        assert!(out.contains("grown 350"));
        assert!(out.contains("full 36"));
        assert!(out.contains("of 3346")); // 2900 + 60 + 350 + 36
        assert!(out.is_ascii());
    }

    #[test]
    fn format_ingest_concurrency_line() {
        let out = format_ingest_concurrency(&ConcurrencySnapshot {
            limit: 5,
            rtt_min_us: 1200,
            rtt_recent_us: 3400,
            gradient: 0.35,
            inflight_peak: 6,
            pinned: false,
        });
        assert!(out.starts_with("[ingest-concurrency]"));
        assert!(out.contains("limit 5"));
        assert!(out.contains("gradient 0.35"));
    }

    #[test]
    fn ingest_profile_counts_source_tiers() {
        use ferrolite_decode::SourceKind;
        let p = IngestProfile::default();
        p.record_source(SourceKind::Prefix);
        p.record_source(SourceKind::Prefix);
        p.record_source(SourceKind::Directed);
        p.record_source(SourceKind::Grown);
        p.record_source(SourceKind::Full);
        assert_eq!(p.prefix_hits(), 2);
        assert_eq!(p.directed(), 1);
        assert_eq!(p.grown(), 1);
        assert_eq!(p.full(), 1);
    }

    #[test]
    fn source_kind_label_covers_directed() {
        use ferrolite_decode::SourceKind;
        assert_eq!(source_kind_label(Some(SourceKind::Directed)), "directed");
        assert_eq!(source_kind_label(None), "n/a");
    }

    #[test]
    fn slow_formatters_are_ascii_even_with_non_ascii_model_and_path() {
        use ferrolite_decode::PreviewSource;
        let s = slow_sample(
            600.0,
            400.0,
            50.0,
            6048,
            4032,
            PreviewSource::FullImage,
            "Canikón",
            "C:/\u{dc}nsern/DSC.ARW",
        );
        assert!(
            format_slow_line(&s).is_ascii(),
            "format_slow_line must escape non-ASCII model/path"
        );
        assert!(
            format_slow_aggregate(std::slice::from_ref(&s), 1).is_ascii(),
            "format_slow_aggregate must escape non-ASCII model/path"
        );
    }

    #[test]
    fn format_slow_aggregate_reports_counts_sources_models_and_top() {
        use ferrolite_decode::PreviewSource;
        let samples = vec![
            slow_sample(
                6000.0,
                5800.0,
                150.0,
                6048,
                4032,
                PreviewSource::FullImage,
                "ILCE-7M4",
                "a.ARW",
            ),
            slow_sample(
                5000.0,
                4800.0,
                150.0,
                6048,
                4032,
                PreviewSource::FullImage,
                "ILCE-7M4",
                "b.ARW",
            ),
            slow_sample(
                700.0,
                120.0,
                520.0,
                8256,
                5504,
                PreviewSource::EmbeddedThumbnail,
                "NIKON Z 7",
                "c.NEF",
            ),
        ];
        let out = format_slow_aggregate(&samples, 3320);
        assert!(out.contains("[ingest-slow-summary] 3 slow files"));
        assert!(out.contains("full_image 2"));
        assert!(out.contains("thumbnail 1"));
        assert!(out.contains("ILCE-7M4"));
        assert!(out.contains("NIKON Z 7"));
        // by-path tier counts (all three samples default to Grown).
        assert!(out.contains("prefix 0 | grown 3 | full 0"));
        // stage-sum rollup line present.
        assert!(out.contains("stage sums"));
        // top-slowest section lists the 6000ms file first.
        let top_idx = out.find("top ").expect("has a top section");
        assert!(out[top_idx..].contains("6000ms"));
        assert!(out.is_ascii());
    }

    #[test]
    fn slow_threshold_defaults_to_500_when_unset() {
        // Note: reads the process env once (cached). In CI the var is unset.
        assert_eq!(slow_threshold_ms(), 500.0);
    }

    #[test]
    fn ingest_profile_records_slow_samples() {
        use ferrolite_decode::PreviewSource;
        let p = IngestProfile::default();
        p.record_slow(slow_sample(
            5305.0,
            5100.0,
            190.0,
            6048,
            4032,
            PreviewSource::FullImage,
            "m",
            "p",
        ));
        assert_eq!(p.slow_samples().len(), 1);
    }

    #[test]
    fn tick_emits_at_most_once_per_second() {
        use std::time::{Duration, Instant};
        let mut d = DiagState::new();
        let t0 = Instant::now();
        let jobs = ferrolite_jobs::JobStats::default();
        // First tick establishes the baseline (no emit).
        assert!(d.tick(t0, jobs, sample_gauges(), 5.0, false).is_none());
        // 100ms later: below the 1s threshold → no emit.
        assert!(d
            .tick(
                t0 + Duration::from_millis(100),
                jobs,
                sample_gauges(),
                5.0,
                false
            )
            .is_none());
        // 1.1s after baseline → emit.
        assert!(d
            .tick(
                t0 + Duration::from_millis(1100),
                jobs,
                sample_gauges(),
                5.0,
                false
            )
            .is_some());
    }
}

//! Env-flag-gated diagnostics dev-mode (`FERROLITE_DIAG`). Zero overhead when
//! unset: `enabled()` is a single cached bool check that short-circuits every
//! recorder, the per-frame tick, and the overlay. Sibling to `thumb_profile.rs`
//! (the narrow ingest profiler), which this does not touch.
//!
//! `FERROLITE_DIAG` = unset→off | `1`/`both`→log+overlay | `log` | `overlay`.
//! `FERROLITE_DIAG_FILE` overrides the session-log path.

use ferrolite_jobs::JobStats;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
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

// ── App-side cumulative counters (process-global, like thumb_profile's statics).
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

/// Render the multi-line ~1/sec log block (also reused, compacted, by the overlay).
pub fn format_log(s: &Snapshot) -> String {
    let j = &s.jobs;
    let g = &s.g;
    let dedup =
        s.req_dedup_tex_f + s.req_dedup_pending_f + s.req_dedup_missing_f + s.req_dedup_uploading_f;
    format!(
        "[diag +{dt:.1}s] frame {fms:.1}ms(max {mx:.1}) ev/f {ev} repaint {rp}\n\
         \x20jobs  sub I/V/B {si}/{sv}/{sb}  disp {disp}  done {done}  cxl(pre){cxp}  panic {pan}\n\
         \x20      active {act}  pending I/V/B {pi}/{pv}/{pb}  cancel removed {crem}/absent {cabs}\n\
         \x20thumb req/f {req} = new {rn} + fast {rf} + dedup {dd} (tex {rt}/pend {rpd}/miss {rms}/upl {rup})\n\
         \x20      pending {tp}  uploading {tu}  handles {th}  missing {tm}  retain req {rc}\n\
         \x20cache tex h/s {thh:.0} m/s {thm:.0} ev/s {the:.0} | pix h/s {pxh:.0} m/s {pxm:.0} ev/s {pxe:.0}\n\
         \x20uploads {up}/{cap} cap  backlog {bk}\n\
         \x20ingest active {ai}  done {idn}/{itot}",
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
        }
    }

    /// Wired to a toggle keybinding (F9 in `update`).
    pub fn toggle_overlay(&mut self) {
        self.overlay_visible = !self.overlay_visible;
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
    format!(
        "frame {fms:.1}ms (max {mx:.1})  ev/f {ev}\n\
         jobs sub I/V/B {si}/{sv}/{sb}  active {act}\n\
         pending I/V/B {pi}/{pv}/{pb}\n\
         cancel removed {crem}/absent {cabs}  cxl(pre) {cxp}  panic {pan}\n\
         req/f {req} new {rn} fast {rf} dedup {dd}\n\
         thumb pending {tp} uploading {tu} handles {th} missing {tm}\n\
         tex h/s {thh:.0} ev/s {the:.0} | pix h/s {pxh:.0} ev/s {pxe:.0}\n\
         uploads/f {up}/{cap}  backlog {bk}\n\
         ingest {idn}/{itot} active {ai}",
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

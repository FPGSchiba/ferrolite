//! Env-flag-gated diagnostics dev-mode (`FERROLITE_DIAG`). Zero overhead when
//! unset: `enabled()` is a single cached bool check that short-circuits every
//! recorder, the per-frame tick, and the overlay. Sibling to `thumb_profile.rs`
//! (the narrow ingest profiler), which this does not touch.
//!
//! `FERROLITE_DIAG` = unset→off | `1`/`both`→log+overlay | `log` | `overlay`.
//! `FERROLITE_DIAG_FILE` overrides the session-log path.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

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

pub fn enabled() -> bool {
    !matches!(mode(), DiagMode::Off)
}
pub fn log_enabled() -> bool {
    matches!(mode(), DiagMode::Log | DiagMode::Both)
}

#[allow(dead_code)]
pub fn overlay_enabled() -> bool {
    matches!(mode(), DiagMode::Overlay | DiagMode::Both)
}

/// Per-second rate of a cumulative delta over `dt_secs` (guards dt→0).
#[allow(dead_code)]
pub fn compute_rate(delta: u64, dt_secs: f64) -> f64 {
    if dt_secs <= 0.0 {
        0.0
    } else {
        delta as f64 / dt_secs
    }
}

use std::io::Write;
use std::sync::Mutex;

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
#[allow(dead_code)]
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
/// Wired in Task 4 (event-loop drain instrumentation).
#[allow(dead_code)]
pub fn add_events(n: usize) {
    add(&EVENTS_DRAINED, n as u64);
}
/// Wired in Task 4 (per-frame upload-budget instrumentation).
#[allow(dead_code)]
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
}

/// Classify the outcome from the three dedup guards, in `request_thumbnail`'s
/// own precedence order (textured > pending > missing). `NewSubmit` is used
/// when none of the guards hit and there is no pixel-cache fast path — the
/// caller records `FastPath` explicitly for the pixel-cache branch.
pub fn classify_request(textured: bool, pending: bool, missing: bool) -> ReqOutcome {
    if textured {
        ReqOutcome::DedupTextured
    } else if pending {
        ReqOutcome::DedupPending
    } else if missing {
        ReqOutcome::DedupMissing
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
    };
    c.fetch_add(1, Ordering::Relaxed);
}

/// Immutable snapshot of the app-side cumulative counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(dead_code)] // Consumed by the diag overlay/log, wired in Task 4.
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
    pub retain_cancels: u64,
    pub events_drained: u64,
    pub uploads_applied: u64,
}

/// Wired in Task 4 (diag overlay/log snapshot source).
#[allow(dead_code)]
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
        retain_cancels: l(&RETAIN_CANCELS),
        events_drained: l(&EVENTS_DRAINED),
        uploads_applied: l(&UPLOADS_APPLIED),
    }
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
    fn mode_flags_are_consistent() {
        assert!(!DiagMode::Off.eq(&DiagMode::Both));
        assert_eq!(parse_mode(Some("log")), DiagMode::Log);
        assert!(matches!(DiagMode::Log, DiagMode::Log));
    }

    #[test]
    fn compute_rate_handles_zero_dt() {
        assert_eq!(compute_rate(100, 0.0), 0.0);
        assert_eq!(compute_rate(100, 2.0), 50.0);
        assert_eq!(compute_rate(0, 1.0), 0.0);
    }

    #[test]
    fn classify_request_prioritises_textured_then_pending_then_missing() {
        assert_eq!(classify_request(false, false, false), ReqOutcome::NewSubmit);
        assert_eq!(
            classify_request(true, false, false),
            ReqOutcome::DedupTextured
        );
        assert_eq!(
            classify_request(false, true, false),
            ReqOutcome::DedupPending
        );
        assert_eq!(
            classify_request(false, false, true),
            ReqOutcome::DedupMissing
        );
        // Textured wins when multiple guards are true (matches request_thumbnail
        // guard order: textures, then pending, then missing).
        assert_eq!(
            classify_request(true, true, true),
            ReqOutcome::DedupTextured
        );
    }
}

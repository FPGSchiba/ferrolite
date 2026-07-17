//! Memory-diagnostics domain for the Develop memory overlay: category model,
//! pure breakdown math (including the `unattributed` residual), adaptive budget,
//! and byte formatting. Pure and unit-tested; the impure gather (reading live
//! `AppState`) lives in `app.rs`, the egui shell in `draw_mem_overlay`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

/// One attributable slice of memory. CPU-resident categories count toward RSS
/// (so `unattributed` = rss − Σ(cpu categories)); VRAM/disk categories are shown
/// for context but excluded from that residual.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemCategory {
    ViewerFullLinear,
    ViewerPreviewSrc,
    CpuPyramid,
    GpuPyramid,
    VtPools,
    PresentBuffers,
    RamCache,
    DiskPreview,
    ThumbTex,
    ThumbPix,
    InflightDecode,
    InflightPyramid,
}

#[allow(dead_code)] // gathered/rendered by app.rs + draw_mem_overlay (later tasks), not wired in yet
impl MemCategory {
    pub const COUNT: usize = 12;
    pub const ALL: [MemCategory; Self::COUNT] = [
        MemCategory::ViewerFullLinear,
        MemCategory::ViewerPreviewSrc,
        MemCategory::CpuPyramid,
        MemCategory::GpuPyramid,
        MemCategory::VtPools,
        MemCategory::PresentBuffers,
        MemCategory::RamCache,
        MemCategory::DiskPreview,
        MemCategory::ThumbTex,
        MemCategory::ThumbPix,
        MemCategory::InflightDecode,
        MemCategory::InflightPyramid,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            MemCategory::ViewerFullLinear => "viewer_full_linear",
            MemCategory::ViewerPreviewSrc => "viewer_preview_src",
            MemCategory::CpuPyramid => "cpu_pyramid",
            MemCategory::GpuPyramid => "gpu_pyramid",
            MemCategory::VtPools => "vt_pools",
            MemCategory::PresentBuffers => "present_buffers",
            MemCategory::RamCache => "ram_cache",
            MemCategory::DiskPreview => "disk_preview",
            MemCategory::ThumbTex => "thumb_tex",
            MemCategory::ThumbPix => "thumb_pix",
            MemCategory::InflightDecode => "inflight_decode",
            MemCategory::InflightPyramid => "inflight_pyramid",
        }
    }

    /// True for categories that live in process RAM (count toward RSS). VRAM
    /// (`GpuPyramid`, `VtPools`, `PresentBuffers`, `ThumbTex`) and disk
    /// (`DiskPreview`) are excluded from the `unattributed` residual.
    pub fn is_cpu_resident(self) -> bool {
        matches!(
            self,
            MemCategory::ViewerFullLinear
                | MemCategory::ViewerPreviewSrc
                | MemCategory::CpuPyramid
                | MemCategory::RamCache
                | MemCategory::ThumbPix
                | MemCategory::InflightDecode
                | MemCategory::InflightPyramid
        )
    }
}

/// A point-in-time memory attribution. `bytes` is indexed by `MemCategory::index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // gathered/rendered by app.rs + draw_mem_overlay (later tasks), not wired in yet
pub struct MemBreakdown {
    pub bytes: [u64; MemCategory::COUNT],
    pub rss: u64,
    pub budget: u64,
}

#[allow(dead_code)] // gathered/rendered by app.rs + draw_mem_overlay (later tasks), not wired in yet
impl MemBreakdown {
    pub fn empty() -> Self {
        Self {
            bytes: [0; MemCategory::COUNT],
            rss: 0,
            budget: 0,
        }
    }

    pub fn set(&mut self, cat: MemCategory, v: u64) {
        self.bytes[cat.index()] = v;
    }

    pub fn get(&self, cat: MemCategory) -> u64 {
        self.bytes[cat.index()]
    }

    /// Sum of the CPU-resident categories (the part of RSS we can attribute).
    pub fn known_cpu_sum(&self) -> u64 {
        MemCategory::ALL
            .iter()
            .filter(|c| c.is_cpu_resident())
            .map(|c| self.bytes[c.index()])
            .sum()
    }

    /// The part of RSS we could NOT attribute. Climbing here = unmodeled growth
    /// (the leak signal). Saturates at 0 (over-attribution never underflows).
    pub fn unattributed(&self) -> u64 {
        self.rss.saturating_sub(self.known_cpu_sum())
    }
}

/// Bytes of a `LinearRgbaF32` of the given dimensions (RGBA f32 = 16 B/px).
#[allow(dead_code)] // called by app.rs's gather_mem_breakdown (later task), not wired in yet
pub fn linear_bytes(width: u32, height: u32) -> u64 {
    width as u64 * height as u64 * 16
}

/// Adaptive RAM-cache budget = clamp(15% of total RAM, 512 MiB, 4 GiB).
#[allow(dead_code)] // gathered by app.rs (later task), not wired in yet
pub fn adaptive_budget(total_ram: u64) -> u64 {
    const FLOOR: u64 = 512 * 1024 * 1024;
    const CEILING: u64 = 4 * 1024 * 1024 * 1024;
    (total_ram / 100 * 15).clamp(FLOOR, CEILING)
}

/// Human-readable bytes: `0B`, `512B`, `1.5K`, `2.0M`, `3.0G` (1024-based).
#[allow(dead_code)] // rendered by draw_mem_overlay (later task), not wired in yet
pub fn fmt_bytes(n: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = K * 1024;
    const G: u64 = M * 1024;
    if n >= G {
        format!("{:.1}G", n as f64 / G as f64)
    } else if n >= M {
        format!("{:.1}M", n as f64 / M as f64)
    } else if n >= K {
        format!("{:.1}K", n as f64 / K as f64)
    } else {
        format!("{n}B")
    }
}

static INFLIGHT_DECODE: AtomicU64 = AtomicU64::new(0);
static INFLIGHT_PYRAMID: AtomicU64 = AtomicU64::new(0);

#[allow(dead_code)] // read by app.rs / draw_mem_overlay (later task), not wired in yet
pub fn inflight_decode_bytes() -> u64 {
    INFLIGHT_DECODE.load(Ordering::Relaxed)
}
#[allow(dead_code)] // read by app.rs / draw_mem_overlay (later task), not wired in yet
pub fn inflight_pyramid_bytes() -> u64 {
    INFLIGHT_PYRAMID.load(Ordering::Relaxed)
}

/// RAII gauge: adds `bytes` to a global in-flight counter on construction and
/// subtracts (saturating) on drop. Held by a decode/pyramid job for its lifetime
/// so the memory overlay attributes buffers that are alive but not yet installed.
#[allow(dead_code)] // handed to the pyramid decode job by a later task, not wired in yet
pub struct InflightGuard {
    counter: &'static AtomicU64,
    bytes: u64,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let _ = self
            .counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(self.bytes))
            });
    }
}

#[allow(dead_code)] // called by the pyramid decode job (later task), not wired in yet
pub fn track_inflight_decode(bytes: u64) -> InflightGuard {
    INFLIGHT_DECODE.fetch_add(bytes, Ordering::Relaxed);
    InflightGuard {
        counter: &INFLIGHT_DECODE,
        bytes,
    }
}

#[allow(dead_code)] // called by the pyramid decode job (later task), not wired in yet
pub fn track_inflight_pyramid(bytes: u64) -> InflightGuard {
    INFLIGHT_PYRAMID.fetch_add(bytes, Ordering::Relaxed);
    InflightGuard {
        counter: &INFLIGHT_PYRAMID,
        bytes,
    }
}

/// One time-series sample for the growth graph.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)] // constructed/read by draw_mem_overlay (later task), not wired in yet
pub struct MemSample {
    pub t_secs: f32,
    pub rss: u64,
    pub cpu_known: u64,
    pub cache: u64,
}

/// Bounded ring buffer of memory samples for the overlay's growth graph.
#[allow(dead_code)] // owned/updated by app.rs + draw_mem_overlay (later task), not wired in yet
pub struct MemHistory {
    cap: usize,
    samples: VecDeque<MemSample>,
}

impl MemHistory {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            samples: VecDeque::with_capacity(cap.max(1)),
        }
    }

    pub fn push(&mut self, s: MemSample) {
        if self.samples.len() == self.cap {
            self.samples.pop_front();
        }
        self.samples.push_back(s);
    }

    pub fn samples(&self) -> &VecDeque<MemSample> {
        &self.samples
    }

    pub fn max_rss(&self) -> u64 {
        self.samples.iter().map(|s| s.rss).max().unwrap_or(0)
    }
}

/// Signed byte delta as a human string, e.g. `+384.0M` / `-12.0K` / `+0B`.
fn fmt_delta(prev: u64, cur: u64) -> String {
    if cur >= prev {
        format!("+{}", fmt_bytes(cur - prev))
    } else {
        format!("-{}", fmt_bytes(prev - cur))
    }
}

/// ~1/sec structured memory line for the diag log sink.
#[allow(dead_code)] // wired into the diag log sink by a later task, not wired in yet
pub fn format_mem_log_line(t_secs: f64, b: &MemBreakdown) -> String {
    format!(
        "[mem] t+{t:.1}s rss={rss} live={live} inflight={inf} gpu={gpu} cache={cache} unattrib={un} budget={bud}",
        t = t_secs,
        rss = fmt_bytes(b.rss),
        live = fmt_bytes(b.get(MemCategory::ViewerFullLinear) + b.get(MemCategory::ViewerPreviewSrc)),
        inf = fmt_bytes(b.get(MemCategory::InflightDecode) + b.get(MemCategory::InflightPyramid)),
        gpu = fmt_bytes(b.get(MemCategory::GpuPyramid)),
        cache = fmt_bytes(b.get(MemCategory::RamCache)),
        un = fmt_bytes(b.unattributed()),
        bud = fmt_bytes(b.budget),
    )
}

/// Event-anchored line (open/close/nav): every changed category as a signed
/// delta, plus the new RSS. Categories with no change are omitted to keep it
/// scannable.
#[allow(dead_code)] // wired into open/close/nav sites by a later task, not wired in yet
pub fn format_mem_event_line(label: &str, prev: &MemBreakdown, cur: &MemBreakdown) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in MemCategory::ALL {
        let (p, q) = (prev.get(c), cur.get(c));
        if p != q {
            parts.push(format!("{} {}", c.label(), fmt_delta(p, q)));
        }
    }
    if parts.is_empty() {
        parts.push("no category change".to_string());
    }
    format!(
        "[mem] {label}: {} rss={}",
        parts.join(" "),
        fmt_bytes(cur.rss)
    )
}

/// Shared body of the category table: one line per `MemCategory`, followed by
/// the `rss` / `unattributed` / `budget` totals. Used by both `format_mem_dump`
/// and `draw_mem_overlay` so the two stay byte-identical apart from their own
/// caller-specific header/prefix.
fn mem_table_lines(b: &MemBreakdown) -> String {
    let mut out = String::new();
    for c in MemCategory::ALL {
        out.push_str(&format!("  {:<18} {}\n", c.label(), fmt_bytes(b.get(c))));
    }
    out.push_str(&format!(
        "  {:<18} {}\n  {:<18} {}\n  {:<18} {}\n",
        "rss",
        fmt_bytes(b.rss),
        "unattributed",
        fmt_bytes(b.unattributed()),
        "budget",
        fmt_bytes(b.budget),
    ));
    out
}

/// Full categorized snapshot for the Shift+F10 dump: one line per category.
pub fn format_mem_dump(b: &MemBreakdown) -> String {
    format!("[mem-dump]\n{}", mem_table_lines(b))
}

/// Paint the dedicated memory overlay: a category table + an RSS growth
/// sparkline, top-LEFT (so it does not overlap the top-right text overlay).
/// Non-interactive, monospace, on the tooltip layer. Call only when the mem
/// overlay is enabled AND visible.
pub fn draw_mem_overlay(ctx: &egui::Context, b: &MemBreakdown, history: &MemHistory) {
    egui::Area::new(egui::Id::new("ferrolite-mem-overlay"))
        .order(egui::Order::Tooltip)
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(8.0, 8.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_black_alpha(210))
                .inner_margin(egui::Margin::same(8.0))
                .rounding(egui::Rounding::same(4.0))
                .show(ui, |ui| {
                    let text = format!(
                        "MEMORY  category            current\n{}",
                        mem_table_lines(b)
                    );
                    ui.label(
                        egui::RichText::new(text)
                            .monospace()
                            .size(11.0)
                            .color(egui::Color32::from_rgb(120, 220, 255)),
                    );
                    draw_growth_sparkline(ui, history);
                });
        });
}

/// A simple RSS-over-time line graph, drawn with `Painter` (data-viz, not an
/// icon). Fixed 220×48 box; scales y to the max RSS in the window.
fn draw_growth_sparkline(ui: &mut egui::Ui, history: &MemHistory) {
    let (w, h) = (220.0_f32, 48.0_f32);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, egui::Color32::from_black_alpha(120));
    let samples = history.samples();
    let max = history.max_rss().max(1) as f32;
    if samples.len() >= 2 {
        let n = samples.len();
        let pts: Vec<egui::Pos2> = samples
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let x = rect.left() + w * (i as f32 / (n - 1) as f32);
                let y = rect.bottom() - h * (s.rss as f32 / max);
                egui::pos2(x, y)
            })
            .collect();
        painter.add(egui::Shape::line(
            pts,
            egui::Stroke::new(1.5, egui::Color32::from_rgb(120, 220, 255)),
        ));
    }
    painter.text(
        rect.left_top() + egui::vec2(3.0, 1.0),
        egui::Align2::LEFT_TOP,
        format!("rss max {}", fmt_bytes(history.max_rss())),
        egui::FontId::monospace(9.0),
        egui::Color32::from_gray(200),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_count_matches_all_array() {
        assert_eq!(MemCategory::ALL.len(), MemCategory::COUNT);
        for (i, c) in MemCategory::ALL.iter().enumerate() {
            assert_eq!(c.index(), i, "index must match position in ALL");
        }
    }

    #[test]
    fn unattributed_is_rss_minus_known_cpu() {
        let mut b = MemBreakdown::empty();
        b.rss = 1000;
        b.set(MemCategory::ViewerFullLinear, 400); // cpu-resident
        b.set(MemCategory::GpuPyramid, 900); // VRAM, NOT counted vs RSS
        assert_eq!(b.known_cpu_sum(), 400);
        assert_eq!(b.unattributed(), 600);
    }

    #[test]
    fn unattributed_saturates_when_known_exceeds_rss() {
        let mut b = MemBreakdown::empty();
        b.rss = 100;
        b.set(MemCategory::ViewerFullLinear, 500);
        assert_eq!(b.unattributed(), 0, "must saturate, never underflow");
    }

    #[test]
    fn adaptive_budget_clamps_to_floor_and_ceiling() {
        const FLOOR: u64 = 512 * 1024 * 1024;
        const CEIL: u64 = 4 * 1024 * 1024 * 1024;
        // Tiny RAM -> floor.
        assert_eq!(adaptive_budget(1024 * 1024 * 1024), FLOOR);
        // Huge RAM -> ceiling.
        assert_eq!(adaptive_budget(128 * 1024 * 1024 * 1024), CEIL);
        // Mid RAM -> 15% of it.
        let mid = 16u64 * 1024 * 1024 * 1024;
        assert_eq!(adaptive_budget(mid), mid / 100 * 15);
    }

    #[test]
    fn linear_bytes_is_16_per_pixel() {
        assert_eq!(linear_bytes(1000, 1000), 16_000_000);
        assert_eq!(linear_bytes(0, 0), 0);
    }

    #[test]
    fn fmt_bytes_is_human_readable() {
        assert_eq!(fmt_bytes(0), "0B");
        assert_eq!(fmt_bytes(512), "512B");
        assert_eq!(fmt_bytes(1024), "1.0K");
        assert_eq!(fmt_bytes(1536), "1.5K");
        assert_eq!(fmt_bytes(2 * 1024 * 1024), "2.0M");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.0G");
    }

    #[test]
    fn inflight_guard_adds_then_subtracts_on_drop() {
        let base = inflight_decode_bytes();
        {
            let _g = track_inflight_decode(1000);
            assert_eq!(inflight_decode_bytes(), base + 1000);
            let _g2 = track_inflight_decode(500);
            assert_eq!(inflight_decode_bytes(), base + 1500);
        }
        assert_eq!(
            inflight_decode_bytes(),
            base,
            "both guards subtracted on drop"
        );
    }

    #[test]
    fn inflight_pyramid_is_independent() {
        let d0 = inflight_decode_bytes();
        let p0 = inflight_pyramid_bytes();
        let _g = track_inflight_pyramid(2048);
        assert_eq!(inflight_pyramid_bytes(), p0 + 2048);
        assert_eq!(inflight_decode_bytes(), d0, "pyramid gauge is separate");
    }

    #[test]
    fn history_is_bounded_and_tracks_max() {
        let mut h = MemHistory::new(3);
        for i in 0..5u64 {
            h.push(MemSample {
                t_secs: i as f32,
                rss: i * 100,
                cpu_known: i * 10,
                cache: 0,
            });
        }
        assert_eq!(h.samples().len(), 3, "ring buffer caps at capacity");
        // Oldest two (rss 0, 100) evicted; newest three are 200,300,400.
        assert_eq!(h.samples().front().unwrap().rss, 200);
        assert_eq!(h.samples().back().unwrap().rss, 400);
        assert_eq!(h.max_rss(), 400);
    }

    #[test]
    fn history_max_of_empty_is_zero() {
        assert_eq!(MemHistory::new(8).max_rss(), 0);
    }

    fn sample_breakdown() -> MemBreakdown {
        let mut b = MemBreakdown::empty();
        b.rss = 2_100 * 1024 * 1024;
        b.budget = 3_400 * 1024 * 1024;
        b.set(MemCategory::ViewerFullLinear, 380 * 1024 * 1024);
        b.set(MemCategory::InflightDecode, 760 * 1024 * 1024);
        b.set(MemCategory::GpuPyramid, 512 * 1024 * 1024);
        b
    }

    #[test]
    fn log_line_has_rss_unattrib_and_budget() {
        let line = format_mem_log_line(12.0, &sample_breakdown());
        assert!(line.starts_with("[mem] t+12.0s"), "got: {line}");
        assert!(line.contains("rss="), "got: {line}");
        assert!(line.contains("unattrib="), "got: {line}");
        assert!(line.contains("budget="), "got: {line}");
    }

    #[test]
    fn event_line_shows_signed_deltas() {
        let mut prev = MemBreakdown::empty();
        prev.rss = 1_000 * 1024 * 1024;
        prev.set(MemCategory::ViewerFullLinear, 0);
        let cur = sample_breakdown();
        let line = format_mem_event_line("open #123 RAW", &prev, &cur);
        assert!(line.starts_with("[mem] open #123 RAW:"), "got: {line}");
        assert!(
            line.contains("viewer_full_linear +"),
            "growth shown with +, got: {line}"
        );
        assert!(line.contains("rss="), "got: {line}");
    }

    #[test]
    fn mem_dump_lists_every_category_and_totals() {
        let b = sample_breakdown();
        let d = format_mem_dump(&b);
        for c in MemCategory::ALL {
            assert!(d.contains(c.label()), "missing {} in dump", c.label());
        }
        assert!(d.contains("unattributed"));
        assert!(d.contains("budget"));
    }
}

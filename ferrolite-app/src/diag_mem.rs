//! Memory-diagnostics domain for the Develop memory overlay: category model,
//! pure breakdown math (including the `unattributed` residual), adaptive budget,
//! and byte formatting. Pure and unit-tested; the impure gather (reading live
//! `AppState`) lives in `app.rs`, the egui shell in `draw_mem_overlay`.

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
}

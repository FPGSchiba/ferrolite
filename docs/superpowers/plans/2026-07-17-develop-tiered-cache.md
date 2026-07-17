# Develop Tiered Cache — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a permanent memory-profiling overlay in the diag stack (Phase 0) to precisely attribute and confirm the Develop-view memory growth, then bound memory and speed up RAW/JPG opens with a tiered, byte-budgeted cache (Phases 1–3).

**Architecture:** Phase 0 adds a dedicated memory overlay + structured logging to the existing env-gated `diag` subsystem, with per-category byte attribution and an `unattributed = rss − Σ(known CPU categories)` line that pinpoints unmodeled growth. All *logic* (categories, breakdown math, ring buffer, budget clamp, formatters) is pure and unit-tested; impure gathering (reading live `AppState`) and the egui overlay draw are thin shells. Phases 1–3 are expanded into a follow-up plan after Phase 0's measurement gate.

**Tech Stack:** Rust, egui/eframe 0.29.1, `sysinfo` (new dev-only dep for RSS + total RAM), existing `ferrolite-app/src/diag.rs` diagnostics stack.

## Global Constraints

- **Toolchain:** run `rustup update stable` before the repo gate; fix code forward-compatibly, never pin to dodge a newer lint. (CLAUDE.md "Toolchain".)
- **Scoped gate for each task** (`ferrolite-app` is the only crate touched in Phase 0): `cargo fmt -p ferrolite-app -- --check` · `cargo clippy -p ferrolite-app --all-targets -- -D warnings` · `cargo test -p ferrolite-app`. The coordinator runs the repo gate once at end of branch.
- **Diagnostics are zero-cost when off:** every new recorder/gather/draw path MUST be gated behind `crate::diag::enabled()` (or a mode check) at the call site, mirroring existing recorders. No allocation or formatting when `FERROLITE_DIAG` is unset.
- **Icons:** any glyph comes from the `icons` module (Phosphor). The memory overlay uses text + a `Painter` line graph only (a data graph is not an icon); no raw emoji/symbols in text, no hand-drawn icons.
- **Immutability / small files:** prefer new focused modules over growing `diag.rs` (already 2125 lines). New logic lands in `diag_mem.rs` + `mem_probe.rs`, not appended to `diag.rs`.
- **Line width 100, rustfmt defaults, 4-space indent.** `-D warnings` clippy.

---

## File Structure (Phase 0)

- **Create `ferrolite-app/src/mem_probe.rs`** — platform layer: process RSS + total system RAM, via `sysinfo`, cached handle. One responsibility: turn OS memory facts into `u64` bytes.
- **Create `ferrolite-app/src/diag_mem.rs`** — the memory-diagnostics domain: `MemCategory`, `MemBreakdown` (pure math incl. `unattributed`), `adaptive_budget` (pure clamp), in-flight push atomics + `InflightGuard`, `MemHistory` ring buffer, pure formatters (`fmt_bytes`, `format_mem_log_line`, `format_mem_event_line`), and the `draw_mem_overlay` egui shell.
- **Modify `ferrolite-app/src/lib.rs` + `main.rs`** — declare the two new modules.
- **Modify `ferrolite-app/src/library/thumb_pixel_cache.rs`** — add `resident_bytes()` accessor.
- **Modify `ferrolite-app/src/diag.rs`** — extend `DiagState` with `mem_overlay_visible: bool` + `mem_history: MemHistory`; no memory *logic* added here (it delegates to `diag_mem`).
- **Modify `ferrolite-app/src/app.rs`** — impure `gather_mem_bytes()` reading live `AppState`/`ViewerState`; wire the per-tick gather+push+log+draw next to the existing diag tick (`app.rs:4443–4487`); F10 toggle + Shift+F10 dump next to the F9 toggle (`app.rs:2830`); event-anchored logging in `open_record` (`app.rs:2602`).
- **Modify `ferrolite-app/Cargo.toml`** — add `sysinfo`.

---

## Task 1: `mem_probe` — platform RSS + total RAM

**Files:**
- Create: `ferrolite-app/src/mem_probe.rs`
- Modify: `ferrolite-app/Cargo.toml` (add `sysinfo`)
- Modify: `ferrolite-app/src/lib.rs`, `ferrolite-app/src/main.rs` (declare module)

**Interfaces:**
- Produces: `mem_probe::total_ram_bytes() -> u64`, `mem_probe::process_rss_bytes() -> u64`. Both return `0` on failure (never panic — diagnostics must not take down the app).

- [ ] **Step 1: Add the dependency**

In `ferrolite-app/Cargo.toml`, under `[dependencies]`, add (default features; we only use process + memory refresh):

```toml
sysinfo = "0.32"
```

Run: `cargo tree -p ferrolite-app -i sysinfo` → Expected: resolves to a 0.32.x version.

- [ ] **Step 2: Write the failing test**

Create `ferrolite-app/src/mem_probe.rs`:

```rust
//! Platform memory probe: process RSS + total system RAM, via `sysinfo`.
//! Dev-diagnostics only (behind `diag::enabled()` at call sites). Never panics:
//! any failure returns 0 so the memory overlay simply shows 0 rather than
//! crashing the app.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_ram_is_positive_on_this_host() {
        assert!(total_ram_bytes() > 0, "a real host reports nonzero total RAM");
    }

    #[test]
    fn process_rss_is_positive_for_this_process() {
        assert!(process_rss_bytes() > 0, "this process has a nonzero RSS");
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p ferrolite-app mem_probe:: 2>&1 | tail -20`
Expected: FAIL — `cannot find function total_ram_bytes` (not yet defined) or module not declared.

- [ ] **Step 4: Declare the module**

In `ferrolite-app/src/lib.rs` add (alphabetical among the `pub mod` lines, near `pub mod diag;`):

```rust
pub mod diag_mem;
pub mod mem_probe;
```

In `ferrolite-app/src/main.rs` add (near `mod diag;`):

```rust
mod diag_mem;
mod mem_probe;
```

(`diag_mem` is created in Task 2; declaring it now is harmless only if the file exists — so create an empty placeholder to keep the tree compiling: `printf '' > ferrolite-app/src/diag_mem.rs` is NOT allowed as a plan placeholder; instead declare `mem_probe` only in this task and add the `diag_mem` declarations in Task 2.)

Correction — in THIS task add only:

```rust
// lib.rs
pub mod mem_probe;
// main.rs
mod mem_probe;
```

- [ ] **Step 5: Write minimal implementation**

Append to `ferrolite-app/src/mem_probe.rs` (above the `#[cfg(test)]` block):

```rust
use std::sync::Mutex;
use std::sync::OnceLock;
use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};

/// Total physical RAM in bytes (queried once; it does not change at runtime).
pub fn total_ram_bytes() -> u64 {
    static TOTAL: OnceLock<u64> = OnceLock::new();
    *TOTAL.get_or_init(|| {
        let sys = System::new_with_specifics(RefreshKind::new().with_memory(sysinfo::MemoryRefreshKind::everything()));
        sys.total_memory()
    })
}

/// Resident set size (bytes) of the current process. Refreshes a cached
/// single-process `System` each call; cheap enough at the ~1/sec diag cadence.
/// Returns 0 if the process cannot be read.
pub fn process_rss_bytes() -> u64 {
    static SYS: OnceLock<Mutex<System>> = OnceLock::new();
    let pid = Pid::from_u32(std::process::id());
    let lock = SYS.get_or_init(|| Mutex::new(System::new()));
    let Ok(mut sys) = lock.lock() else {
        return 0;
    };
    sys.refresh_process_specifics(pid, ProcessRefreshKind::new().with_memory());
    sys.process(pid).map(|p| p.memory()).unwrap_or(0)
}
```

> Note for the implementer: `sysinfo` 0.32 reports `Process::memory()` in **bytes** and `System::total_memory()` in **bytes**. If the pinned minor version differs and returns KiB, multiply by 1024 and add a comment. Verify with the assertion magnitudes (RSS for this test binary should be > 1 MiB).

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p ferrolite-app mem_probe:: 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 7: Scoped gate + commit**

Run: `cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app mem_probe::`
Expected: clean.

```bash
git add ferrolite-app/Cargo.toml Cargo.lock ferrolite-app/src/mem_probe.rs ferrolite-app/src/lib.rs ferrolite-app/src/main.rs
git commit -m "feat(diag): platform RSS + total-RAM probe (sysinfo)"
```

---

## Task 2: `diag_mem` core — categories, breakdown math, budget, byte formatting

**Files:**
- Create: `ferrolite-app/src/diag_mem.rs`
- Modify: `ferrolite-app/src/lib.rs`, `ferrolite-app/src/main.rs` (declare module)

**Interfaces:**
- Consumes: `mem_probe::total_ram_bytes` (Task 1).
- Produces:
  - `enum MemCategory` with `const COUNT: usize`, `fn index(self) -> usize`, `fn label(self) -> &'static str`, `fn is_cpu_resident(self) -> bool`, `const ALL: [MemCategory; COUNT]`.
  - `struct MemBreakdown { bytes: [u64; MemCategory::COUNT], rss: u64, budget: u64 }` with `known_cpu_sum() -> u64`, `unattributed() -> u64`, `get(MemCategory) -> u64`.
  - `fn adaptive_budget(total_ram: u64) -> u64`.
  - `fn fmt_bytes(u64) -> String`.

- [ ] **Step 1: Declare the module**

In `ferrolite-app/src/lib.rs`: `pub mod diag_mem;` (near `pub mod diag;`).
In `ferrolite-app/src/main.rs`: `mod diag_mem;` (near `mod diag;`).

- [ ] **Step 2: Write the failing tests**

Create `ferrolite-app/src/diag_mem.rs`:

```rust
//! Memory-diagnostics domain for the Develop memory overlay: category model,
//! pure breakdown math (including the `unattributed` residual), adaptive budget,
//! and byte formatting. Pure and unit-tested; the impure gather (reading live
//! `AppState`) lives in `app.rs`, the egui shell in `draw_mem_overlay`.

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
        b.set(MemCategory::GpuPyramid, 900);        // VRAM, NOT counted vs RSS
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
        assert_eq!(adaptive_budget(1 * 1024 * 1024 * 1024), FLOOR);
        // Huge RAM -> ceiling.
        assert_eq!(adaptive_budget(128 * 1024 * 1024 * 1024), CEIL);
        // Mid RAM -> 15% of it.
        let mid = 16u64 * 1024 * 1024 * 1024;
        assert_eq!(adaptive_budget(mid), mid * 15 / 100);
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
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ferrolite-app diag_mem::tests 2>&1 | tail -20`
Expected: FAIL — types/functions not defined.

- [ ] **Step 4: Write minimal implementation**

Prepend to `ferrolite-app/src/diag_mem.rs` (above the test module):

```rust
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
pub struct MemBreakdown {
    pub bytes: [u64; MemCategory::COUNT],
    pub rss: u64,
    pub budget: u64,
}

impl MemBreakdown {
    pub fn empty() -> Self {
        Self { bytes: [0; MemCategory::COUNT], rss: 0, budget: 0 }
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
pub fn adaptive_budget(total_ram: u64) -> u64 {
    const FLOOR: u64 = 512 * 1024 * 1024;
    const CEILING: u64 = 4 * 1024 * 1024 * 1024;
    (total_ram / 100 * 15).clamp(FLOOR, CEILING)
}

/// Human-readable bytes: `0B`, `512B`, `1.5K`, `2.0M`, `3.0G` (1024-based).
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
```

> Implementer note: `adaptive_budget` computes `total_ram / 100 * 15` (divide-then-multiply) to avoid `u64` overflow on large-RAM hosts. The mid-RAM test expects exactly `mid * 15 / 100`; for `mid = 16 GiB` both orders are equal (16 GiB is divisible by 100? No). Use `total_ram / 100 * 15` in impl **and** in the test's expected value so they match. Update the test's expected to `mid / 100 * 15`.

- [ ] **Step 5: Fix the mid-RAM test expectation to match the overflow-safe formula**

In the test `adaptive_budget_clamps_to_floor_and_ceiling`, change the mid assertion to:

```rust
let mid = 16u64 * 1024 * 1024 * 1024;
assert_eq!(adaptive_budget(mid), mid / 100 * 15);
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p ferrolite-app diag_mem::tests 2>&1 | tail -20`
Expected: PASS (5 tests).

- [ ] **Step 7: Scoped gate + commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app diag_mem::
git add ferrolite-app/src/diag_mem.rs ferrolite-app/src/lib.rs ferrolite-app/src/main.rs
git commit -m "feat(diag): memory category model, breakdown math, adaptive budget, fmt_bytes"
```

---

## Task 3: In-flight byte accounting (push atomics + drop guard)

**Files:**
- Modify: `ferrolite-app/src/diag_mem.rs`

**Interfaces:**
- Produces: `fn inflight_decode_bytes() -> u64`, `fn inflight_pyramid_bytes() -> u64`, `struct InflightGuard` (RAII; subtracts on drop, saturating), `fn track_inflight_decode(bytes: u64) -> InflightGuard`, `fn track_inflight_pyramid(bytes: u64) -> InflightGuard`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `diag_mem.rs`:

```rust
#[test]
fn inflight_guard_adds_then_subtracts_on_drop() {
    let base = inflight_decode_bytes();
    {
        let _g = track_inflight_decode(1000);
        assert_eq!(inflight_decode_bytes(), base + 1000);
        let _g2 = track_inflight_decode(500);
        assert_eq!(inflight_decode_bytes(), base + 1500);
    }
    assert_eq!(inflight_decode_bytes(), base, "both guards subtracted on drop");
}

#[test]
fn inflight_pyramid_is_independent() {
    let d0 = inflight_decode_bytes();
    let p0 = inflight_pyramid_bytes();
    let _g = track_inflight_pyramid(2048);
    assert_eq!(inflight_pyramid_bytes(), p0 + 2048);
    assert_eq!(inflight_decode_bytes(), d0, "pyramid gauge is separate");
}
```

> These two tests mutate process-global atomics; keep them reading a `base` first so they are order-independent under the test harness's threads.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app diag_mem::tests::inflight 2>&1 | tail -20`
Expected: FAIL — `track_inflight_decode` not found.

- [ ] **Step 3: Write minimal implementation**

Add to `diag_mem.rs` (above the tests):

```rust
use std::sync::atomic::{AtomicU64, Ordering};

static INFLIGHT_DECODE: AtomicU64 = AtomicU64::new(0);
static INFLIGHT_PYRAMID: AtomicU64 = AtomicU64::new(0);

pub fn inflight_decode_bytes() -> u64 {
    INFLIGHT_DECODE.load(Ordering::Relaxed)
}
pub fn inflight_pyramid_bytes() -> u64 {
    INFLIGHT_PYRAMID.load(Ordering::Relaxed)
}

/// RAII gauge: adds `bytes` to a global in-flight counter on construction and
/// subtracts (saturating) on drop. Held by a decode/pyramid job for its lifetime
/// so the memory overlay attributes buffers that are alive but not yet installed.
pub struct InflightGuard {
    counter: &'static AtomicU64,
    bytes: u64,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let _ = self.counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
            Some(v.saturating_sub(self.bytes))
        });
    }
}

pub fn track_inflight_decode(bytes: u64) -> InflightGuard {
    INFLIGHT_DECODE.fetch_add(bytes, Ordering::Relaxed);
    InflightGuard { counter: &INFLIGHT_DECODE, bytes }
}

pub fn track_inflight_pyramid(bytes: u64) -> InflightGuard {
    INFLIGHT_PYRAMID.fetch_add(bytes, Ordering::Relaxed);
    InflightGuard { counter: &INFLIGHT_PYRAMID, bytes }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app diag_mem::tests::inflight 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app diag_mem::
git add ferrolite-app/src/diag_mem.rs
git commit -m "feat(diag): in-flight byte gauges with RAII drop guard"
```

---

## Task 4: Growth ring buffer (`MemHistory`)

**Files:**
- Modify: `ferrolite-app/src/diag_mem.rs`

**Interfaces:**
- Produces: `struct MemSample { t_secs: f32, rss: u64, cpu_known: u64, cache: u64 }`, `struct MemHistory` with `fn new(cap: usize) -> Self`, `fn push(&mut self, MemSample)`, `fn samples(&self) -> &VecDeque<MemSample>`, `fn max_rss(&self) -> u64`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
#[test]
fn history_is_bounded_and_tracks_max() {
    let mut h = MemHistory::new(3);
    for i in 0..5u64 {
        h.push(MemSample { t_secs: i as f32, rss: i * 100, cpu_known: i * 10, cache: 0 });
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app diag_mem::tests::history 2>&1 | tail -20`
Expected: FAIL — `MemHistory` not found.

- [ ] **Step 3: Write minimal implementation**

Add to `diag_mem.rs`:

```rust
use std::collections::VecDeque;

/// One time-series sample for the growth graph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemSample {
    pub t_secs: f32,
    pub rss: u64,
    pub cpu_known: u64,
    pub cache: u64,
}

/// Bounded ring buffer of memory samples for the overlay's growth graph.
pub struct MemHistory {
    cap: usize,
    samples: VecDeque<MemSample>,
}

impl MemHistory {
    pub fn new(cap: usize) -> Self {
        Self { cap: cap.max(1), samples: VecDeque::with_capacity(cap.max(1)) }
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app diag_mem::tests::history 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app diag_mem::
git add ferrolite-app/src/diag_mem.rs
git commit -m "feat(diag): bounded MemHistory ring buffer for growth graph"
```

---

## Task 5: Pure formatters — per-tick log line + event-anchored delta line

**Files:**
- Modify: `ferrolite-app/src/diag_mem.rs`

**Interfaces:**
- Produces: `fn format_mem_log_line(t_secs: f64, b: &MemBreakdown) -> String`, `fn format_mem_event_line(label: &str, prev: &MemBreakdown, cur: &MemBreakdown) -> String`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
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
    assert!(line.contains("viewer_full_linear +"), "growth shown with +, got: {line}");
    assert!(line.contains("rss="), "got: {line}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app diag_mem::tests 2>&1 | tail -20`
Expected: FAIL — formatters not found.

- [ ] **Step 3: Write minimal implementation**

Add to `diag_mem.rs`:

```rust
/// Signed byte delta as a human string, e.g. `+384.0M` / `-12.0K` / `0B`.
fn fmt_delta(prev: u64, cur: u64) -> String {
    if cur >= prev {
        format!("+{}", fmt_bytes(cur - prev))
    } else {
        format!("-{}", fmt_bytes(prev - cur))
    }
}

/// ~1/sec structured memory line for the diag log sink.
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
    format!("[mem] {label}: {} rss={}", parts.join(" "), fmt_bytes(cur.rss))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app diag_mem::tests 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app diag_mem::
git add ferrolite-app/src/diag_mem.rs
git commit -m "feat(diag): memory log-line + event-anchored delta formatters"
```

---

## Task 6: `ThumbPixelCache::resident_bytes` accessor

**Files:**
- Modify: `ferrolite-app/src/library/thumb_pixel_cache.rs`

**Interfaces:**
- Produces: `ThumbPixelCache::resident_bytes(&self) -> u64` (sum of entry `rgba.len()`).

- [ ] **Step 1: Write the failing test**

In `ferrolite-app/src/library/thumb_pixel_cache.rs`, inside its existing `#[cfg(test)] mod tests` (or add one if absent), add:

```rust
#[test]
fn resident_bytes_sums_entry_pixels() {
    let mut c = ThumbPixelCache::new(4);
    assert_eq!(c.resident_bytes(), 0);
    c.insert(1, vec![0u8; 100], 5, 5);
    c.insert(2, vec![0u8; 200], 10, 5);
    assert_eq!(c.resident_bytes(), 300);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ferrolite-app thumb_pixel_cache 2>&1 | tail -20`
Expected: FAIL — `resident_bytes` not found.

- [ ] **Step 3: Write minimal implementation**

Add to `impl ThumbPixelCache`:

```rust
/// Total bytes of decoded RGBA held (sum of entry pixel buffers). Diagnostics.
pub fn resident_bytes(&self) -> u64 {
    self.map.values().map(|e| e.rgba.len() as u64).sum()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ferrolite-app thumb_pixel_cache 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app thumb_pixel_cache
git add ferrolite-app/src/library/thumb_pixel_cache.rs
git commit -m "feat(diag): ThumbPixelCache::resident_bytes for memory attribution"
```

---

## Task 7: Impure gather — `gather_mem_bytes` reading live `AppState`

**Files:**
- Modify: `ferrolite-app/src/app.rs` (new method on `FerroliteApp`)
- Modify: `ferrolite-app/src/diag_mem.rs` (add a small pure helper `linear_bytes`)

**Interfaces:**
- Consumes: `MemCategory`, `MemBreakdown`, `mem_probe`, `adaptive_budget`, `inflight_*_bytes`, `ThumbPixelCache::resident_bytes`.
- Produces: `FerroliteApp::gather_mem_breakdown(&self) -> diag_mem::MemBreakdown` (impure; reads live state; only called behind `diag::enabled()`).

- [ ] **Step 1: Add a pure byte helper + test in `diag_mem.rs`**

```rust
/// Bytes of a `LinearRgbaF32` of the given dimensions (RGBA f32 = 16 B/px).
pub fn linear_bytes(width: u32, height: u32) -> u64 {
    width as u64 * height as u64 * 16
}
```

Test (add to `tests`):

```rust
#[test]
fn linear_bytes_is_16_per_pixel() {
    assert_eq!(linear_bytes(1000, 1000), 16_000_000);
    assert_eq!(linear_bytes(0, 0), 0);
}
```

Run: `cargo test -p ferrolite-app diag_mem::tests::linear 2>&1 | tail -5` → FAIL then, after adding the fn, PASS.

- [ ] **Step 2: Write `gather_mem_breakdown`**

Locate the `impl FerroliteApp` block containing `drive_viewer`/diag code in `app.rs`. Add this method (adjust field access to the real `ViewerState` fields: `preview_source`, `raw_preview_source` are `Option<Arc<LinearRgbaF32>>`; `pyramid` is `Option<Arc<GpuPyramidSource>>`; `image_dims` is `Option<(u32,u32)>`):

```rust
/// Build a point-in-time memory attribution from live app state. Impure
/// (reads `ViewerState`, caches, in-flight gauges, and the OS RSS). Only call
/// behind `diag::enabled()`. GPU/VRAM figures are documented estimates.
fn gather_mem_breakdown(&self) -> crate::diag_mem::MemBreakdown {
    use crate::diag_mem::{linear_bytes, MemBreakdown, MemCategory};
    let mut b = MemBreakdown::empty();
    b.rss = crate::mem_probe::process_rss_bytes();
    b.budget = crate::diag_mem::adaptive_budget(crate::mem_probe::total_ram_bytes());

    if let Some(v) = self.state.viewer.as_ref() {
        let preview_src = [v.preview_source.as_ref(), v.raw_preview_source.as_ref()]
            .into_iter()
            .flatten()
            .map(|a| linear_bytes(a.width, a.height))
            .sum::<u64>();
        b.set(MemCategory::ViewerPreviewSrc, preview_src);

        // GPU pyramid VRAM estimate: full-res f32 + mip tail (~4/3). Present only
        // once the pyramid has been installed.
        if v.pyramid.is_some() {
            if let Some((w, h)) = v.image_dims {
                b.set(MemCategory::GpuPyramid, linear_bytes(w, h) * 4 / 3);
            }
        }
    }

    // In-flight buffers (decode + pyramid jobs holding large Arcs).
    b.set(MemCategory::InflightDecode, crate::diag_mem::inflight_decode_bytes());
    b.set(MemCategory::InflightPyramid, crate::diag_mem::inflight_pyramid_bytes());

    // Thumb pixel cache (real bytes) and texture cache (VRAM estimate: entries ×
    // 256×256 RGBA8).
    b.set(MemCategory::ThumbPix, self.state.thumb_pixels.resident_bytes());
    b.set(
        MemCategory::ThumbTex,
        self.state.textures.len() as u64 * 256 * 256 * 4,
    );

    b
}
```

> Implementer notes:
> - `viewer_full_linear` stays 0 for now: the full linear buffer is moved into the pyramid job (see `app.rs:1046`), so at rest the viewer does not retain it — it is captured by `inflight_pyramid` instead once Task 8 instruments the job. Leave the category defined (Phase 1's clone-removal will make the viewer retain it as an `Arc`, at which point this gather sets it).
> - `ram_cache`, `disk_preview`, `cpu_pyramid`, `vt_pools`, `present_buffers` remain 0 in Phase 0 (RAM cache does not exist yet; the rest need accessors added in Phase 1). They are shown as 0 so the table shape is stable.
> - If `ViewerState` field names differ from the above, grep `struct ViewerState` in `ferrolite-app/src/viewer/mod.rs` and adjust; do NOT invent fields.

- [ ] **Step 3: Compile check (no behavior wired yet)**

Run: `cargo build -p ferrolite-app 2>&1 | tail -20`
Expected: compiles (method is currently unused → allow with a temporary `#[allow(dead_code)]` on the method; remove it in Task 9 when it is called).

- [ ] **Step 4: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/app.rs ferrolite-app/src/diag_mem.rs
git commit -m "feat(diag): gather memory breakdown from live app state"
```

---

## Task 8: Instrument the pyramid job's in-flight bytes

**Files:**
- Modify: `ferrolite-app/src/app.rs` (the pyramid `jobs.submit` closure at `app.rs:1046–1086`)

**Interfaces:**
- Consumes: `diag_mem::track_inflight_pyramid`.

- [ ] **Step 1: Wrap the pyramid job body with an in-flight guard**

In the `Priority::Background` closure that builds `PyramidTileSource` + `GpuPyramidSource` (search `GpuPyramidSource::new` in `app.rs`), at the very top of the closure body, after the first `cancel.is_cancelled()` check, add:

```rust
// Attribute this job's large in-flight buffer (full-res linear f32) to the
// memory overlay for its lifetime. Gated: zero cost when diagnostics are off.
let _inflight = crate::diag::enabled()
    .then(|| crate::diag_mem::track_inflight_pyramid(
        crate::diag_mem::linear_bytes(image_full.width, image_full.height),
    ));
```

Place it so `_inflight` lives until the closure returns (it will drop at end of scope, i.e. when the job completes or cancels). `image_full` is the `Arc<LinearRgbaF32>` already captured by the closure.

- [ ] **Step 2: Compile + manual reasoning check**

Run: `cargo build -p ferrolite-app 2>&1 | tail -20`
Expected: compiles. (No unit test — this is a threading side-effect; verified live in the visual test plan. The pure gauge math is already covered by Task 3.)

- [ ] **Step 3: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings
git add ferrolite-app/src/app.rs
git commit -m "feat(diag): attribute pyramid-job buffer to inflight_pyramid gauge"
```

---

## Task 9: Wire the per-tick gather, F10 toggle, Shift+F10 dump, and log

**Files:**
- Modify: `ferrolite-app/src/diag.rs` (`DiagState` fields)
- Modify: `ferrolite-app/src/app.rs` (tick site `4443–4487`; key handling near `2830`)

**Interfaces:**
- Consumes: `gather_mem_breakdown`, `MemHistory`, `MemSample`, `format_mem_log_line`.
- Produces: `DiagState.mem_overlay_visible: bool`, `DiagState.mem_history: MemHistory`, `DiagState::toggle_mem_overlay(&mut self)`.

- [ ] **Step 1: Extend `DiagState`**

In `diag.rs`, add fields to `struct DiagState`:

```rust
    /// Wired to F10; the dedicated memory overlay (separate from the text one).
    pub mem_overlay_visible: bool,
    /// Growth-graph ring buffer for the memory overlay (~5 min at 1/sec).
    pub mem_history: crate::diag_mem::MemHistory,
```

In `DiagState::new()` initialize them:

```rust
            mem_overlay_visible: false,
            mem_history: crate::diag_mem::MemHistory::new(300),
```

Add the toggle method to `impl DiagState`:

```rust
    /// Wired to a toggle keybinding (F10 in `update`).
    pub fn toggle_mem_overlay(&mut self) {
        self.mem_overlay_visible = !self.mem_overlay_visible;
    }
```

- [ ] **Step 2: Add F10 toggle + Shift+F10 dump next to the F9 handler**

In `app.rs` near line 2830 (the existing F9 block), add:

```rust
        if crate::diag::enabled() && ctx.input(|i| i.key_pressed(egui::Key::F10)) {
            if ctx.input(|i| i.modifiers.shift) {
                // Shift+F10: dump a full categorized snapshot to the diag log.
                let b = self.gather_mem_breakdown();
                crate::diag::write_log(&crate::diag_mem::format_mem_dump(&b));
            } else {
                self.diag.toggle_mem_overlay();
            }
        }
```

- [ ] **Step 3: Add the `format_mem_dump` full-snapshot formatter (pure) + test in `diag_mem.rs`**

```rust
/// Full categorized snapshot for the Shift+F10 dump: one line per category.
pub fn format_mem_dump(b: &MemBreakdown) -> String {
    let mut out = String::from("[mem-dump]\n");
    for c in MemCategory::ALL {
        out.push_str(&format!("  {:<18} {}\n", c.label(), fmt_bytes(b.get(c))));
    }
    out.push_str(&format!(
        "  {:<18} {}\n  {:<18} {}\n  {:<18} {}\n",
        "rss", fmt_bytes(b.rss),
        "unattributed", fmt_bytes(b.unattributed()),
        "budget", fmt_bytes(b.budget),
    ));
    out
}
```

Test:

```rust
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
```

Run: `cargo test -p ferrolite-app diag_mem::tests::mem_dump 2>&1 | tail -5` → FAIL then PASS.

- [ ] **Step 4: Gather + push + log at the diag tick site**

In `app.rs`, inside the `if let Some(t0) = diag_t0 {` block (around 4443), AFTER the existing `self.diag.tick(...)` call that yields `snap`, add a memory gather that runs on the same ~1/sec cadence. Replace the existing `if let Some(snap) = self.diag.tick(...) { ... }` tail with a version that also handles memory:

```rust
            if let Some(snap) = self.diag.tick(
                std::time::Instant::now(),
                stats,
                gauges,
                frame_ms,
                repaint_forced,
            ) {
                if crate::diag::log_enabled() {
                    crate::diag::write_log(&crate::diag::format_log(&snap));
                }
                // Memory: gather once per diag tick, push to the growth ring,
                // and log the structured line.
                let mem = self.gather_mem_breakdown();
                self.diag.mem_history.push(crate::diag_mem::MemSample {
                    t_secs: snap.dt as f32,
                    rss: mem.rss,
                    cpu_known: mem.known_cpu_sum(),
                    cache: mem.get(crate::diag_mem::MemCategory::RamCache),
                });
                if crate::diag::log_enabled() {
                    crate::diag::write_log(&crate::diag_mem::format_mem_log_line(snap.dt, &mem));
                }
            }
```

Remove the temporary `#[allow(dead_code)]` from `gather_mem_breakdown` (it is now called).

- [ ] **Step 5: Build + run the full crate tests**

Run: `cargo test -p ferrolite-app 2>&1 | tail -20`
Expected: PASS (all existing + new diag_mem tests).

- [ ] **Step 6: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/diag.rs ferrolite-app/src/app.rs ferrolite-app/src/diag_mem.rs
git commit -m "feat(diag): wire memory gather/log + F10 overlay toggle + Shift+F10 dump"
```

---

## Task 10: Draw the memory overlay (table + growth sparkline)

**Files:**
- Modify: `ferrolite-app/src/diag_mem.rs` (add `draw_mem_overlay`)
- Modify: `ferrolite-app/src/app.rs` (call it at the overlay-draw site, ~4482)

**Interfaces:**
- Consumes: `MemBreakdown`, `MemHistory`, `fmt_bytes`.
- Produces: `diag_mem::draw_mem_overlay(ctx: &egui::Context, b: &MemBreakdown, history: &MemHistory)`.

- [ ] **Step 1: Implement the overlay draw (egui shell — no unit test; visual)**

Add to `diag_mem.rs`:

```rust
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
                    let mut text = String::from("MEMORY  category            current\n");
                    for c in MemCategory::ALL {
                        text.push_str(&format!("  {:<18} {}\n", c.label(), fmt_bytes(b.get(c))));
                    }
                    text.push_str(&format!(
                        "  {:<18} {}\n  {:<18} {}\n  {:<18} {}\n",
                        "rss", fmt_bytes(b.rss),
                        "unattributed", fmt_bytes(b.unattributed()),
                        "budget", fmt_bytes(b.budget),
                    ));
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
```

> Implementer note: verify the egui 0.29 API names (`Shape::line`, `painter_at`, `allocate_exact_size`, `FontId::monospace`) against the version in `Cargo.lock`; they are stable in 0.29 but adjust if clippy/compile flags a signature.

- [ ] **Step 2: Call the overlay at the draw site**

In `app.rs`, in the `if let Some(t0) = diag_t0 {` block, next to the existing text-overlay draw (`draw_overlay`, ~4482), add:

```rust
            if crate::diag::enabled() && self.diag.mem_overlay_visible {
                let mem = self.gather_mem_breakdown();
                crate::diag_mem::draw_mem_overlay(ctx, &mem, &self.diag.mem_history);
            }
```

- [ ] **Step 3: Build + full crate test**

Run: `cargo build -p ferrolite-app 2>&1 | tail -20 && cargo test -p ferrolite-app 2>&1 | tail -10`
Expected: compiles; all tests pass.

- [ ] **Step 4: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/diag_mem.rs ferrolite-app/src/app.rs
git commit -m "feat(diag): dedicated memory overlay with category table + growth sparkline"
```

---

## Task 11: Event-anchored memory logging on open/navigate

**Files:**
- Modify: `ferrolite-app/src/app.rs` (`open_record`, `app.rs:2602`)

**Interfaces:**
- Consumes: `gather_mem_breakdown`, `format_mem_event_line`.

- [ ] **Step 1: Capture before/after breakdown around the open**

In `open_record` (`app.rs:2602`), wrap the open so a before/after delta is logged. Because the new image's buffers load asynchronously, capture `before` synchronously here and log an immediate event line describing the *transition intent* (the async arrivals are captured by the ~1/sec tick line):

```rust
    fn open_record(
        &mut self,
        ctx: &egui::Context,
        frame: &mut eframe::Frame,
        rec: &ferrolite_catalog::ImageRecord,
    ) {
        let mem_before = crate::diag::enabled().then(|| self.gather_mem_breakdown());
        self.maybe_regen_on_leave(ctx, frame);
        if let Some(old) = self.state.viewer.as_ref() {
            let old_id = old.image_id;
            old.cancel_loads();
            self.cancel_viewer_tiles(frame, old_id);
        }
        self.state.open_image_in_viewer(rec);
        self.module = crate::module::Module::Develop;
        if let Some(before) = mem_before {
            let after = self.gather_mem_breakdown();
            let kind = if rec.kind == ferrolite_image::FileKind::Raw { "RAW" } else { "JPG" };
            crate::diag::write_log(&crate::diag_mem::format_mem_event_line(
                &format!("open #{} {}", rec.id, kind),
                &before,
                &after,
            ));
        }
        ctx.request_repaint();
    }
```

- [ ] **Step 2: Build + test**

Run: `cargo build -p ferrolite-app 2>&1 | tail -20 && cargo test -p ferrolite-app 2>&1 | tail -10`
Expected: compiles; tests pass.

- [ ] **Step 3: Commit**

```bash
cargo fmt -p ferrolite-app -- --check && cargo clippy -p ferrolite-app --all-targets -- -D warnings && cargo test -p ferrolite-app
git add ferrolite-app/src/app.rs
git commit -m "feat(diag): event-anchored memory delta log on image open"
```

---

## Phase 0 — Coordinator wrap-up (not a subagent task)

After Task 11, the coordinator:

1. Runs `rustup update stable`, then the **repo gate**:
   `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo build --all-targets && cargo test --workspace`.
2. Hands the author this **visual/measurement test plan**:
   - Launch with `FERROLITE_DIAG=1`, open Develop, press **F10** → memory overlay (top-left) shows the category table + growth sparkline; text overlay (top-right, F9) unaffected.
   - Scroll a RAW folder fast. Watch the sparkline and `inflight_pyramid`. **Expected (policy):** RSS rises while scrolling, then *recedes* within a second or two of stopping; `unattributed` stays small/flat. **Leak signal:** `unattributed` climbs monotonically and never recedes → capture the log (`[mem]` lines + Shift+F10 dump) for follow-up.
   - Confirm `[mem]` lines appear ~1/sec in the diag log and `open #… RAW/JPG` event lines appear on each navigation.
3. Records the verdict (policy vs leak) in the spec/investigation notes. **This verdict gates Phases 1–3.**

---

## Subsequent phases (expanded into their own plan AFTER the Phase 0 measurement gate)

Per the spec's "measure first, then build" decision, the following are captured as a task-level roadmap now and turned into a detailed TDD plan once Phase 0's measurement confirms the growth mechanism (so the cache is built against evidence, not a guess).

**Phase 1 — Bound memory (`develop::cache` + budget + clone removal).**
- Create `ferrolite-app/src/develop/cache.rs`: byte-accounted LRU keyed by `(image_id, Tier)`; `adaptive_budget` reused from `diag_mem`; `insert`/`get`/`evict_to`/`resident_bytes`; never-evict-open-image rule. Pure logic, headless-tested (mirrors `ResidencySet`).
- Cap concurrent heavy decodes via an in-flight permit count so transient memory ≤ budget.
- Remove the redundant `Arc::new(image.clone())` at `app.rs:1047` (share the reveal's `Arc`); set `viewer_full_linear` in the gather once the viewer retains it.
- Wire `ram_cache`/`cpu_pyramid`/`vt_pools`/`present_buffers`/`disk_preview` real byte accessors into `gather_mem_breakdown`.
- Register the module in `develop/mod.rs`.

**Phase 2 — Instant open (Tier-0 thumbnail placeholder).**
- On open, reveal the already-decoded grid/filmstrip thumbnail upscaled as the initial preview, crossfading to Tier 1/2 (reuse `crossfading`/`crossfade_elapsed`).

**Phase 3 — Fast re-opens (JPG Tier-1 write-back + warm RAM reuse).**
- Relax `preview_cache::should_write_back`'s `is_raw` gate to include `FileKind::Standard`; confirm the Standard color path yields an equivalent `display_matrix`.
- Serve warm neighbors from the `develop::cache` RAM tier under budget; extend prefetch to JPG.

Each subsequent phase ends with its own scoped gate, repo gate, and author visual test.

---

## Self-Review

- **Spec coverage (Phase 0):** memory overlay ✓ (Tasks 9–10), per-category attribution incl. `unattributed` ✓ (Task 2, gather Task 7), growth-over-time graph ✓ (Tasks 4, 10), structured ~1/sec log ✓ (Task 9), event-anchored deltas ✓ (Tasks 5, 11), dump hotkey ✓ (Task 9), RSS/total-RAM platform layer ✓ (Task 1), zero-cost-when-off ✓ (gated at every call site). Phases 1–3 spec items are rostered in the roadmap for their follow-up plan (deliberate, per the measurement gate).
- **Placeholder scan:** no "TBD/TODO/handle edge cases"; every code step shows complete code. Task 1 Step 4 flags-and-corrects a module-declaration ordering trap explicitly rather than leaving it ambiguous.
- **Type consistency:** `MemCategory`/`MemBreakdown`/`MemHistory`/`MemSample`/`InflightGuard` names and signatures are used identically across Tasks 2–11; `gather_mem_breakdown`, `format_mem_log_line`, `format_mem_event_line`, `format_mem_dump`, `draw_mem_overlay`, `adaptive_budget`, `fmt_bytes`, `linear_bytes` all match their defining tasks.
- **Known implementer verifications flagged inline:** `sysinfo` byte-vs-KiB unit, `ViewerState` field names, egui 0.29 painter API names — each has a note telling the implementer to verify against the real source rather than assume.

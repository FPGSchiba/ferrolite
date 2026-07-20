//! Develop warm-navigation cache: a two-level in-RAM cache of recently-shown
//! render state, keyed by `(image_id, op_stack_hash)`, so filmstrip navigation
//! reveals instantly. All LRU / budget / eviction logic is pure and headless-
//! tested; GPU `Arc` handles are opaque payload the cache never inspects. See
//! docs/superpowers/specs/2026-07-20-develop-warm-navigation-cache-design.md.

use std::collections::HashMap;
use std::sync::Arc;

/// Neighbors warmed AHEAD of the current image (the culling direction).
#[allow(dead_code)] // wired by Task 7 (forward-biased prefetch window)
pub const WARM_WINDOW_FORWARD: usize = 4;
/// Neighbors warmed BEHIND the current image.
#[allow(dead_code)] // wired by Task 7 (forward-biased prefetch window)
pub const WARM_WINDOW_BACK: usize = 2;
/// How many most-recent images also retain the full pipeline (instant 1:1).
#[allow(dead_code)] // wired by Task 5 (full-tier count cap)
pub const WARM_FULL_COUNT: usize = 2;

/// Identity of a cached render: an edit (new op stack) yields a new hash, so the
/// stale entry is a natural miss and ages out by LRU — no manual invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub image_id: i64,
    pub op_stack_hash: u64,
}

/// Display-tier payload: the edited rung-1 display texture (fit-view sharp).
/// `tex` is `None` only in headless tests; production always stores `Some`.
#[derive(Clone)]
pub struct DisplayEntry {
    #[allow(dead_code)] // read by try_warm_reveal (wired by Task 4)
    pub tex: Option<Arc<wgpu::Texture>>,
    #[allow(dead_code)] // read by try_warm_reveal (wired by Task 4)
    pub dims: (u32, u32),
    pub bytes: u64,
}

/// Full-pipeline payload (Task 5 populates the GPU `Arc`s). Kept minimal here so
/// the `WarmHit::Full` variant and the `full` map type-check from Task 2.
#[allow(dead_code)] // wired by Task 5/6
#[derive(Clone)]
pub struct FullEntry {
    pub bytes: u64,
}

/// Result of consulting the cache for a key. `Full` is populated by Task 5.
pub enum WarmHit {
    #[allow(dead_code)] // wired by Task 5/6
    Full {
        full: FullEntry,
        display: DisplayEntry,
    },
    #[allow(dead_code)] // matched by try_warm_reveal (wired by Task 4)
    Display(DisplayEntry),
    Miss,
}

/// A monotonically increasing tick used as the LRU last-touch stamp.
type Touch = u64;

// `Slot`/`WarmCache` are not yet constructed from production code — `AppState`
// gains a `warm_cache: WarmCache` field in Task 4, which is the first production
// call site. Until then the bin's private `mod develop;` tree (see main.rs) has
// no reachable caller, so `-D warnings` needs these narrow allows (cf. the same
// situation documented in icons.rs). Tests already exercise the pure logic.
#[allow(dead_code)] // constructed by WarmCache::insert_display (wired by Task 4)
struct Slot<E> {
    entry: E,
    touched: Touch,
}

#[allow(dead_code)] // constructed by AppState::new (wired by Task 4)
pub struct WarmCache {
    #[allow(dead_code)] // read by evict_to_budget's real impl (wired by Task 3)
    budget: u64,
    clock: Touch,
    #[allow(dead_code)] // read by evict_to_budget's real impl (wired by Task 3)
    open: Option<CacheKey>,
    display: HashMap<CacheKey, Slot<DisplayEntry>>,
    // Full tier added in Task 5.
    #[allow(dead_code)] // wired by Task 5/6
    full: HashMap<CacheKey, Slot<FullEntry>>,
}

impl WarmCache {
    #[allow(dead_code)] // wired by Task 4 (AppState::new)
    pub fn new(budget: u64) -> Self {
        Self {
            budget,
            clock: 0,
            open: None,
            display: HashMap::new(),
            full: HashMap::new(),
        }
    }

    #[allow(dead_code)] // called by get/insert_display (wired by Task 4)
    fn tick(&mut self) -> Touch {
        self.clock += 1;
        self.clock
    }

    /// Not yet called by any task in this plan; kept for a future live-budget
    /// settings knob (mirrors `set_open`'s shape).
    #[allow(dead_code)]
    pub fn set_budget(&mut self, budget: u64) {
        self.budget = budget;
        self.evict_to_budget();
    }

    /// Mark which key is the currently-open image — it is never evicted.
    #[allow(dead_code)] // wired by Task 4 (warm_insert_display)
    pub fn set_open(&mut self, open: Option<CacheKey>) {
        self.open = open;
    }

    #[allow(dead_code)] // wired by Task 4 (try_warm_reveal)
    pub fn get(&mut self, key: CacheKey) -> WarmHit {
        let now = self.tick();
        if let Some(f) = self.full.get_mut(&key) {
            f.touched = now;
            let full = f.entry.clone();
            if let Some(d) = self.display.get_mut(&key) {
                d.touched = now;
                return WarmHit::Full {
                    full,
                    display: d.entry.clone(),
                };
            }
        }
        if let Some(d) = self.display.get_mut(&key) {
            d.touched = now;
            return WarmHit::Display(d.entry.clone());
        }
        WarmHit::Miss
    }

    #[allow(dead_code)] // wired by Task 4 (warm_insert_display)
    pub fn insert_display(&mut self, key: CacheKey, entry: DisplayEntry) {
        let now = self.tick();
        self.display.insert(
            key,
            Slot {
                entry,
                touched: now,
            },
        );
        self.evict_to_budget();
    }

    /// Total resident bytes across both tiers (feeds the F10 `ram_cache` gauge).
    #[allow(dead_code)] // wired by Task 9 (diagnostics)
    pub fn resident_bytes(&self) -> u64 {
        let d: u64 = self.display.values().map(|s| s.entry.bytes).sum();
        let f: u64 = self.full.values().map(|s| s.entry.bytes).sum();
        d + f
    }

    /// Not yet called by any task in this plan; kept for API completeness
    /// alongside `resident_bytes`.
    #[allow(dead_code)]
    pub fn len_display(&self) -> usize {
        self.display.len()
    }

    #[allow(dead_code)] // called by set_budget/insert_display (wired by Task 4)
    fn evict_to_budget(&mut self) {
        // Implemented in Task 3.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disp(bytes: u64) -> DisplayEntry {
        DisplayEntry {
            tex: None,
            dims: (0, 0),
            bytes,
        }
    }
    fn key(id: i64, hash: u64) -> CacheKey {
        CacheKey {
            image_id: id,
            op_stack_hash: hash,
        }
    }

    #[test]
    fn display_hit_after_insert_and_miss_otherwise() {
        let mut c = WarmCache::new(1 << 30);
        assert!(matches!(c.get(key(1, 0)), WarmHit::Miss));
        c.insert_display(key(1, 0), disp(100));
        assert!(matches!(c.get(key(1, 0)), WarmHit::Display(_)));
        // Different op_stack_hash for the same image is a distinct key (edit miss).
        assert!(matches!(c.get(key(1, 9)), WarmHit::Miss));
    }

    #[test]
    fn resident_bytes_sums_display_entries() {
        let mut c = WarmCache::new(1 << 30);
        c.insert_display(key(1, 0), disp(100));
        c.insert_display(key(2, 0), disp(250));
        assert_eq!(c.resident_bytes(), 350);
        // Re-inserting the same key replaces (does not double-count).
        c.insert_display(key(1, 0), disp(400));
        assert_eq!(c.resident_bytes(), 650);
    }
}

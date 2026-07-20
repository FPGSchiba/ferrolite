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
    pub tex: Option<Arc<wgpu::Texture>>,
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
    Display(DisplayEntry),
    Miss,
}

/// A monotonically increasing tick used as the LRU last-touch stamp.
type Touch = u64;

// `AppState` holds a `warm_cache: WarmCache` field (Task 4), the production
// construction site; `try_warm_reveal`/`warm_insert_display` (`app.rs`) are the
// production `get`/`insert_display` call sites. Tests exercise the pure logic
// directly, below.
struct Slot<E> {
    entry: E,
    touched: Touch,
}

pub struct WarmCache {
    budget: u64,
    clock: Touch,
    open: Option<CacheKey>,
    display: HashMap<CacheKey, Slot<DisplayEntry>>,
    // Full tier added in Task 5.
    #[allow(dead_code)] // wired by Task 5/6
    full: HashMap<CacheKey, Slot<FullEntry>>,
}

impl WarmCache {
    pub fn new(budget: u64) -> Self {
        Self {
            budget,
            clock: 0,
            open: None,
            display: HashMap::new(),
            full: HashMap::new(),
        }
    }

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
    pub fn set_open(&mut self, open: Option<CacheKey>) {
        self.open = open;
    }

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

    /// Evict least-recently-touched DISPLAY entries until within budget. The open
    /// image is never evicted (skipped as a candidate). The full tier is bounded
    /// by count (Task 5), not evicted here. If the only remaining candidates are
    /// protected, the cache may briefly exceed budget rather than drop the open
    /// image — a bounded overshoot of at most the open image's own bytes.
    fn evict_to_budget(&mut self) {
        while self.resident_bytes() > self.budget {
            // Pick the LRU display entry that is not the open image.
            let victim = self
                .display
                .iter()
                .filter(|(k, _)| Some(**k) != self.open)
                .min_by_key(|(_, s)| s.touched)
                .map(|(k, _)| *k);
            match victim {
                Some(k) => {
                    self.display.remove(&k);
                }
                None => break, // only protected entries remain
            }
        }
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

    #[test]
    fn evicts_least_recently_touched_over_budget() {
        let mut c = WarmCache::new(300);
        c.insert_display(key(1, 0), disp(100)); // touch 1
        c.insert_display(key(2, 0), disp(100)); // touch 2
        c.insert_display(key(3, 0), disp(100)); // touch 3 -> 300, at budget
        assert_eq!(c.resident_bytes(), 300);
        // Touch #1 so it is now most-recent.
        assert!(matches!(c.get(key(1, 0)), WarmHit::Display(_)));
        // Insert #4 -> over budget by 100 -> evict the LRU, which is now #2.
        c.insert_display(key(4, 0), disp(100));
        assert_eq!(c.resident_bytes(), 300);
        assert!(matches!(c.get(key(2, 0)), WarmHit::Miss), "LRU #2 evicted");
        assert!(
            matches!(c.get(key(1, 0)), WarmHit::Display(_)),
            "#1 kept (touched)"
        );
    }

    #[test]
    fn never_evicts_the_open_image() {
        let mut c = WarmCache::new(150);
        c.insert_display(key(1, 0), disp(100)); // oldest
        c.set_open(Some(key(1, 0)));
        // Insert #2 -> 200 > 150; #1 is oldest but open, so #2... must free space
        // WITHOUT touching #1. With only #1 and #2 present and #1 protected, the
        // cache evicts #2's competitors — here nothing else — so it may exceed
        // budget rather than drop the open image. Assert #1 survives.
        c.insert_display(key(2, 0), disp(100));
        assert!(
            matches!(c.get(key(1, 0)), WarmHit::Display(_)),
            "open image never evicted"
        );
    }
}

//! Develop warm-navigation cache: a two-level in-RAM cache of recently-shown
//! render state, keyed by `(image_id, op_stack_hash)`, so filmstrip navigation
//! reveals instantly. All LRU / budget / eviction logic is pure and headless-
//! tested; GPU `Arc` handles are opaque payload the cache never inspects. See
//! docs/superpowers/specs/2026-07-20-develop-warm-navigation-cache-design.md.

use std::collections::HashMap;
use std::sync::Arc;

use ferrolite_pipeline::GpuPyramidSource;
use ferrolite_pipeline::OpStack;
use ferrolite_vt::TileSource;

/// Neighbors warmed AHEAD of the current image (the culling direction).
pub const WARM_WINDOW_FORWARD: usize = 4;
/// Neighbors warmed BEHIND the current image.
pub const WARM_WINDOW_BACK: usize = 2;
/// How many most-recent images also retain the full pipeline (instant 1:1).
pub const WARM_FULL_COUNT: usize = 2;
/// How long the user must dwell on an image before its neighbors are warm-
/// prefetched — so fast filmstrip scrubbing doesn't flood the job system with
/// neighbor decodes it will immediately supersede. Tunable.
pub const WARM_SETTLE_SECS: f32 = 0.4;

/// Forward-biased neighbor selection for warm prefetch: up to `forward` ids ahead
/// of `current` (the culling direction, listed first) then up to `back` behind,
/// clamped to the list, excluding `current`. Empty if `current` is absent.
pub fn warm_window(ids: &[i64], current: i64, forward: usize, back: usize) -> Vec<i64> {
    let Some(pos) = ids.iter().position(|id| *id == current) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 1..=forward {
        if let Some(id) = ids.get(pos + i) {
            out.push(*id);
        }
    }
    for i in 1..=back {
        if pos >= i {
            out.push(ids[pos - i]);
        }
    }
    out
}

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

/// Full-pipeline payload for the 1–2 most-recent images: the GPU pyramid + tile
/// source + the op stack/cam needed to rebuild the (`!Send`) producer + sparse
/// VT on the render thread. GPU handles are `Option` only so headless tests can
/// fabricate the struct; production always stores `Some`.
#[derive(Clone)]
pub struct FullEntry {
    pub pyramid: Option<Arc<GpuPyramidSource>>,
    pub tile_source: Option<Arc<dyn TileSource + Send + Sync>>,
    pub op_stack: OpStack,
    pub cam: [[f32; 3]; 3],
    pub bytes: u64,
}

/// Result of consulting the cache for a key.
pub enum WarmHit {
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

    /// Insert a full-pipeline entry, bounding the tier to `WARM_FULL_COUNT` by
    /// evicting the least-recently-touched full entry that is not the open image.
    pub fn insert_full(&mut self, key: CacheKey, entry: FullEntry) {
        let now = self.tick();
        self.full.insert(
            key,
            Slot {
                entry,
                touched: now,
            },
        );
        while self.full.len() > WARM_FULL_COUNT {
            let victim = self
                .full
                .iter()
                .filter(|(k, _)| Some(**k) != self.open)
                .min_by_key(|(_, s)| s.touched)
                .map(|(k, _)| *k);
            match victim {
                Some(k) => {
                    self.full.remove(&k);
                }
                None => break,
            }
        }
        self.evict_to_budget();
    }

    /// Not yet called by any task in this plan; kept for API completeness
    /// alongside `resident_bytes`.
    #[allow(dead_code)]
    pub fn len_full(&self) -> usize {
        self.full.len()
    }

    /// Total resident bytes across both tiers (feeds the F10 `ram_cache` gauge).
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

    const IDENTITY_CAM: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    fn full_e(bytes: u64) -> FullEntry {
        FullEntry {
            pyramid: None,
            tile_source: None,
            op_stack: Default::default(),
            cam: IDENTITY_CAM,
            bytes,
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

    #[test]
    fn open_image_survives_even_when_it_alone_exceeds_budget() {
        let mut c = WarmCache::new(50);
        // Production always calls `set_open` before `insert_display` (see
        // `app.rs::warm_insert_display`) so the just-opened image is already
        // protected by the time its own insert runs its eviction pass.
        c.set_open(Some(key(1, 0)));
        c.insert_display(key(1, 0), disp(100)); // 100 > budget 50, but open
        assert_eq!(
            c.resident_bytes(),
            100,
            "open image kept despite exceeding budget"
        );
        // Re-run eviction via a no-op budget set; open entry must still not be dropped.
        c.set_budget(50);
        assert_eq!(
            c.resident_bytes(),
            100,
            "open image kept despite exceeding budget"
        );
        assert!(matches!(c.get(key(1, 0)), WarmHit::Display(_)));
    }

    #[test]
    fn full_tier_caps_at_warm_full_count() {
        let mut c = WarmCache::new(1 << 30);
        for i in 0..(WARM_FULL_COUNT as i64 + 2) {
            c.insert_display(key(i, 0), disp(10));
            c.insert_full(key(i, 0), full_e(10));
        }
        assert_eq!(c.len_full(), WARM_FULL_COUNT, "full tier bounded by count");
    }

    #[test]
    fn warm_window_is_forward_biased_and_clamped() {
        let ids = [10, 20, 30, 40, 50, 60, 70];
        // current=30 (pos 2), forward 3 back 1 -> [40,50,60] forward + [20] back,
        // forward first (culling direction), current excluded.
        assert_eq!(warm_window(&ids, 30, 3, 1), vec![40, 50, 60, 20]);
        // Clamp at the end.
        assert_eq!(warm_window(&ids, 70, 3, 1), vec![60]);
        // current absent -> empty.
        assert!(warm_window(&ids, 999, 3, 1).is_empty());
    }

    #[test]
    fn get_returns_full_when_both_tiers_present() {
        let mut c = WarmCache::new(1 << 30);
        c.insert_display(key(1, 0), disp(10));
        c.insert_full(key(1, 0), full_e(10));
        assert!(matches!(c.get(key(1, 0)), WarmHit::Full { .. }));
        // Full without display degrades to nothing (full needs its display too).
        c.insert_full(key(2, 0), full_e(10));
        assert!(matches!(c.get(key(2, 0)), WarmHit::Miss));
    }
}

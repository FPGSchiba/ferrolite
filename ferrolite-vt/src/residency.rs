//! Pure CPU residency bookkeeping: which tiles a view needs, and an LRU set with
//! a tile-count budget. No GPU — the streaming brain, fully testable headless.

use std::collections::HashMap;

use ferrolite_image::{tiles_per_level, TileCoord, TILE_SIZE};

use crate::transform::ViewTransform;

/// Virtual tiles the current view needs at its chosen LOD (visible rect only).
pub fn needed_tiles(
    image: (u32, u32),
    view: &ViewTransform,
    viewport: (f32, f32),
    level_count: u32,
) -> Vec<TileCoord> {
    let lod = view.lod_for(image, level_count);
    let (cols, rows) = tiles_per_level(image.0, image.1, lod);
    // Visible image-space rect (centered pan). Half-viewport in image px = (vp/2)/zoom.
    let half_w = (viewport.0 * 0.5) / view.zoom;
    let half_h = (viewport.1 * 0.5) / view.zoom;
    let cx = image.0 as f32 * 0.5 + view.pan.0;
    let cy = image.1 as f32 * 0.5 + view.pan.1;
    let lod_scale = (1u32 << lod) as f32; // image px per lod px
    let tile_px = TILE_SIZE as f32 * lod_scale; // image px covered by one tile at this lod
    let x0 = (((cx - half_w).max(0.0)) / tile_px).floor() as u32;
    let x1 = (((cx + half_w).max(0.0)) / tile_px).floor() as u32;
    let y0 = (((cy - half_h).max(0.0)) / tile_px).floor() as u32;
    let y1 = (((cy + half_h).max(0.0)) / tile_px).floor() as u32;
    let mut out = Vec::new();
    for y in y0..=y1.min(rows.saturating_sub(1)) {
        for x in x0..=x1.min(cols.saturating_sub(1)) {
            out.push(TileCoord { lod, x, y });
        }
    }
    out
}

/// LRU set of resident tiles under a fixed tile-count budget.
pub struct ResidencySet {
    capacity: usize,
    order: Vec<TileCoord>, // front = LRU
}

impl ResidencySet {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            order: Vec::new(),
        }
    }
    pub fn contains(&self, t: TileCoord) -> bool {
        self.order.contains(&t)
    }
    /// Remove `t` from the resident set entirely (used when a slot is freed).
    pub fn forget(&mut self, t: TileCoord) {
        if let Some(p) = self.order.iter().position(|&x| x == t) {
            self.order.remove(p);
        }
    }
    /// The least-recently-used resident tile, if any (front of the order).
    pub fn lru(&self) -> Option<TileCoord> {
        self.order.first().copied()
    }
    pub fn touch(&mut self, t: TileCoord) {
        if let Some(p) = self.order.iter().position(|&x| x == t) {
            self.order.remove(p);
        }
        self.order.push(t);
    }
    /// Insert `t` as MRU; return an evicted tile if over capacity.
    pub fn insert(&mut self, t: TileCoord) -> Option<TileCoord> {
        self.touch(t);
        if self.order.len() > self.capacity {
            Some(self.order.remove(0))
        } else {
            None
        }
    }
    /// Given the needed set, return (to_load = needed∖resident, to_evict =
    /// resident∖needed). Does not mutate; caller drives load/evict via jobs.
    pub fn diff(&self, needed: &[TileCoord]) -> (Vec<TileCoord>, Vec<TileCoord>) {
        let to_load = needed
            .iter()
            .copied()
            .filter(|t| !self.contains(*t))
            .collect();
        let to_evict = self
            .order
            .iter()
            .copied()
            .filter(|t| !needed.contains(t))
            .collect();
        (to_load, to_evict)
    }
}

/// Tracks the opstack version each *produced* (edited) tile was rendered at.
/// Pure bookkeeping — no GPU. An edit bumps the version; tiles produced at an
/// older version are stale and must be re-produced lazily for the current view.
pub struct VersionedResidency {
    current: u64,
    /// coord -> the version it was last produced at.
    at: HashMap<TileCoord, u64>,
}

impl VersionedResidency {
    pub fn new() -> Self {
        Self {
            current: 0,
            at: HashMap::new(),
        }
    }

    pub fn current(&self) -> u64 {
        self.current
    }

    /// Set the active version. If it changed, returns every coord whose produced
    /// version is now stale (≠ the new version) so the caller can free those slots.
    pub fn set_version(&mut self, v: u64) -> Vec<TileCoord> {
        if v == self.current {
            return Vec::new();
        }
        self.current = v;
        let stale: Vec<TileCoord> = self
            .at
            .iter()
            .filter(|&(_, &ver)| ver != v)
            .map(|(&c, _)| c)
            .collect();
        for c in &stale {
            self.at.remove(c);
        }
        stale
    }

    /// Record that `t` was produced at the current version (resident + fresh).
    pub fn mark(&mut self, t: TileCoord) {
        self.at.insert(t, self.current);
    }

    /// Is `t` resident AND produced at the current version?
    pub fn is_current(&self, t: TileCoord) -> bool {
        self.at.get(&t) == Some(&self.current)
    }

    /// Drop `t` entirely (slot freed).
    pub fn forget(&mut self, t: TileCoord) {
        self.at.remove(&t);
    }

    /// Of `needed`, those not resident at the current version (must (re)produce),
    /// preserving the needed order (visibility priority).
    pub fn to_produce(&self, needed: &[TileCoord]) -> Vec<TileCoord> {
        needed.iter().copied().filter(|t| !self.is_current(*t)).collect()
    }
}

impl Default for VersionedResidency {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_image::TileCoord;

    fn tc(lod: u32, x: u32, y: u32) -> TileCoord {
        TileCoord { lod, x, y }
    }

    #[test]
    fn insert_evicts_least_recently_used_over_capacity() {
        let mut r = ResidencySet::new(2);
        assert_eq!(r.insert(tc(0, 0, 0)), None);
        assert_eq!(r.insert(tc(0, 1, 0)), None);
        r.touch(tc(0, 0, 0)); // 0,0,0 now MRU
        assert_eq!(r.insert(tc(0, 2, 0)), Some(tc(0, 1, 0))); // evict LRU
        assert!(r.contains(tc(0, 0, 0)));
        assert!(!r.contains(tc(0, 1, 0)));
    }

    #[test]
    fn diff_reports_missing_and_unneeded() {
        let mut r = ResidencySet::new(8);
        r.insert(tc(0, 0, 0));
        r.insert(tc(0, 9, 9)); // resident but not needed
        let needed = vec![tc(0, 0, 0), tc(0, 1, 0)];
        let (to_load, to_evict) = r.diff(&needed);
        assert_eq!(to_load, vec![tc(0, 1, 0)]);
        assert_eq!(to_evict, vec![tc(0, 9, 9)]);
    }

    #[test]
    fn panning_evicts_offscreen_and_loads_newly_visible() {
        use crate::pool::SlotAllocator;
        let image = (2048u32, 2048u32);
        let vp = (256.0f32, 256.0f32);
        let mut res = ResidencySet::new(64);
        let mut alloc = SlotAllocator::new(64);
        // View A: top-left.
        let a = ViewTransform {
            zoom: 1.0,
            pan: (-800.0, -800.0),
        };
        for t in needed_tiles(image, &a, vp, 4) {
            res.insert(t);
            alloc.alloc(t);
        }
        // View B: bottom-right (disjoint).
        let b = ViewTransform {
            zoom: 1.0,
            pan: (800.0, 800.0),
        };
        let needed_b = needed_tiles(image, &b, vp, 4);
        let (to_load, to_evict) = res.diff(&needed_b);
        assert!(!to_load.is_empty(), "new tiles needed");
        assert!(!to_evict.is_empty(), "old tiles evicted");
        for t in &to_evict {
            alloc.free(*t);
        }
        for t in &to_load {
            assert!(alloc.alloc(*t).is_some(), "freed slots make room");
        }
    }

    #[test]
    fn version_bump_invalidates_stale_tiles_only() {
        let mut vr = VersionedResidency::new();
        vr.mark(tc(0, 0, 0));
        vr.mark(tc(0, 1, 0));
        assert!(vr.is_current(tc(0, 0, 0)));
        // Bump: every previously-marked tile is now stale and returned to invalidate.
        let stale = vr.set_version(1);
        assert_eq!(stale.len(), 2);
        assert!(!vr.is_current(tc(0, 0, 0)));
        // A no-op bump to the same version invalidates nothing.
        vr.mark(tc(0, 0, 0));
        assert!(vr.set_version(1).is_empty());
    }

    #[test]
    fn to_produce_is_needed_minus_current_resident() {
        let mut vr = VersionedResidency::new();
        vr.mark(tc(0, 0, 0)); // resident at current version
        let needed = vec![tc(0, 0, 0), tc(0, 1, 0)];
        // (0,0) is current; only (1,0) must be produced.
        assert_eq!(vr.to_produce(&needed), vec![tc(0, 1, 0)]);
        // After a version bump, (0,0) is stale -> both must be (re)produced.
        vr.set_version(2);
        assert_eq!(vr.to_produce(&needed), vec![tc(0, 0, 0), tc(0, 1, 0)]);
    }

    #[test]
    fn forget_drops_a_tile() {
        let mut vr = VersionedResidency::new();
        vr.mark(tc(0, 0, 0));
        vr.forget(tc(0, 0, 0));
        assert!(!vr.is_current(tc(0, 0, 0)));
    }
}

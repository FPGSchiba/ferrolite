//! Session-only CPU-side LRU of decoded thumbnail pixels, keyed by image id.
//! Lets a grid cell re-revealed after GPU-texture eviction re-upload its
//! texture directly — no re-submitted job, no DB read, no JPEG re-decode
//! (see docs/superpowers/investigations/2026-07-02-thumbnail-and-shutdown-bugs.md,
//! Bug B). Bounded so memory stays in check on large libraries.

use std::collections::HashMap;

struct Entry {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
}

pub struct ThumbPixelCache {
    capacity: usize,
    order: Vec<i64>, // front = least recently used
    map: HashMap<i64, Entry>,
}

impl ThumbPixelCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            order: Vec::new(),
            map: HashMap::new(),
        }
    }

    fn touch(&mut self, id: i64) {
        if let Some(pos) = self.order.iter().position(|&x| x == id) {
            self.order.remove(pos);
        }
        self.order.push(id);
    }

    /// Not called from production paths today (the fast path in
    /// `request_thumbnail` uses `get` directly) but kept as public API per the
    /// cache's interface contract and exercised by the unit tests below.
    #[allow(dead_code)]
    pub fn contains(&self, id: i64) -> bool {
        self.map.contains_key(&id)
    }

    pub fn get(&mut self, id: i64) -> Option<(Vec<u8>, u32, u32)> {
        if self.map.contains_key(&id) {
            crate::diag::pix_hit();
            self.touch(id);
            let e = self.map.get(&id)?;
            Some((e.rgba.clone(), e.w, e.h))
        } else {
            crate::diag::pix_miss();
            None
        }
    }

    pub fn insert(&mut self, id: i64, rgba: Vec<u8>, w: u32, h: u32) {
        self.touch(id);
        self.map.insert(id, Entry { rgba, w, h });
        while self.order.len() > self.capacity {
            let evict = self.order.remove(0);
            self.map.remove(&evict);
            crate::diag::pix_evict(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_miss_then_hit_returns_pixels() {
        let mut c = ThumbPixelCache::new(4);
        assert!(c.get(1).is_none());
        c.insert(1, vec![1, 2, 3, 4], 1, 1);
        assert_eq!(c.get(1), Some((vec![1, 2, 3, 4], 1, 1)));
        assert!(c.contains(1));
    }

    #[test]
    fn evicts_least_recently_used_over_capacity() {
        let mut c = ThumbPixelCache::new(2);
        c.insert(1, vec![0; 4], 1, 1);
        c.insert(2, vec![0; 4], 1, 1);
        let _ = c.get(1); // 1 now most-recent, 2 is LRU
        c.insert(3, vec![0; 4], 1, 1); // over cap → evict 2
        assert!(c.contains(1));
        assert!(!c.contains(2));
        assert!(c.contains(3));
    }

    #[test]
    fn reinsert_same_id_updates_without_growing() {
        let mut c = ThumbPixelCache::new(2);
        c.insert(5, vec![9; 4], 1, 1);
        c.insert(5, vec![7; 4], 1, 1);
        assert_eq!(c.get(5), Some((vec![7; 4], 1, 1)));
        // capacity still allows one more distinct id without evicting 5
        c.insert(6, vec![0; 4], 1, 1);
        assert!(c.contains(5));
        assert!(c.contains(6));
    }
}

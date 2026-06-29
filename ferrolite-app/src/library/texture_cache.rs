//! LRU cache of decoded thumbnail textures keyed by image id. The ordering
//! bookkeeping is a small generic `Lru` so it can be tested without a GPU.

use std::collections::HashMap;

struct Lru {
    capacity: usize,
    order: Vec<i64>, // front = least recently used
}

impl Lru {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            order: Vec::new(),
        }
    }
    fn touch(&mut self, id: i64) {
        if let Some(pos) = self.order.iter().position(|&x| x == id) {
            self.order.remove(pos);
        }
        self.order.push(id);
    }
    /// Record an insertion; return an id to evict if over capacity.
    fn insert(&mut self, id: i64) -> Option<i64> {
        self.touch(id);
        if self.order.len() > self.capacity {
            Some(self.order.remove(0))
        } else {
            None
        }
    }
}

pub struct TextureCache {
    lru: Lru,
    textures: HashMap<i64, egui::TextureHandle>,
}

impl TextureCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            lru: Lru::new(capacity),
            textures: HashMap::new(),
        }
    }
    pub fn contains(&self, id: i64) -> bool {
        self.textures.contains_key(&id)
    }
    pub fn get(&mut self, id: i64) -> Option<&egui::TextureHandle> {
        if self.textures.contains_key(&id) {
            self.lru.touch(id);
            self.textures.get(&id)
        } else {
            None
        }
    }
    pub fn insert(&mut self, id: i64, tex: egui::TextureHandle) {
        if let Some(evict) = self.lru.insert(id) {
            self.textures.remove(&evict);
        }
        self.textures.insert(id, tex);
    }
    /// Number of cached textures. Not called in the current UI but kept as a
    /// public API for future diagnostics / Plan 4 memory-pressure logic.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.textures.len()
    }
    /// Returns true when no textures are cached. Companion to `len`; kept for
    /// the same future Plan 4 diagnostics use.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::Lru;

    #[test]
    fn evicts_least_recently_used() {
        let mut lru = Lru::new(2);
        assert_eq!(lru.insert(1), None);
        assert_eq!(lru.insert(2), None);
        lru.touch(1); // 1 now most-recent
        assert_eq!(lru.insert(3), Some(2)); // 2 was LRU → evicted
    }

    #[test]
    fn touch_moves_to_most_recent() {
        let mut lru = Lru::new(3);
        lru.insert(1);
        lru.insert(2);
        lru.insert(3);
        lru.touch(1);
        assert_eq!(lru.insert(4), Some(2));
    }
}

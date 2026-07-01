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
    fn clear(&mut self) {
        self.order.clear();
    }
}

pub struct TextureCache {
    lru: Lru,
    textures: HashMap<i64, egui::TextureHandle>,
    /// Handles retired this frame (evicted, replaced, or cleared) but not yet
    /// dropped. Dropping a `TextureHandle` queues its texture in egui's
    /// `TexturesDelta.free`, which egui_wgpu destroys BETWEEN encoding the
    /// render pass and `queue.submit`. If the same texture is still referenced
    /// by a mesh painted THIS frame, dropping it this frame destroys it before
    /// the submit that uses it → wgpu validation panic. Holding retired
    /// handles here and dropping them at the top of the NEXT frame (see
    /// `begin_frame`) guarantees a texture is never destroyed in a frame that
    /// still paints it.
    retiring: Vec<egui::TextureHandle>,
}

impl TextureCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            lru: Lru::new(capacity),
            textures: HashMap::new(),
            retiring: Vec::new(),
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
            if let Some(old) = self.textures.remove(&evict) {
                self.retiring.push(old);
            }
        }
        if let Some(old) = self.textures.insert(id, tex) {
            self.retiring.push(old); // replacing same id: retire old handle, don't drop mid-frame
        }
    }
    /// Retire all cached textures (and LRU order) instead of dropping them now.
    /// Used when returning from the GPU-heavy Develop viewer, whose full-res
    /// `VirtualTexture` work can leave the shared wgpu textures stale —
    /// forcing the grid to re-upload fresh. The retired handles are dropped
    /// on the following frame's `begin_frame` call, same as evicted/replaced
    /// handles, so they are never freed in a frame that still paints them.
    pub fn clear(&mut self) {
        self.retiring.extend(self.textures.drain().map(|(_, h)| h));
        self.lru.clear();
    }
    /// Drop the handles retired during the PREVIOUS frame. MUST be called once
    /// at the very top of each frame, before anything paints. Deferring frees
    /// by one frame guarantees a texture painted this frame is never destroyed
    /// before this frame's `queue.submit` (egui_wgpu destroys `delta.free`
    /// textures between the render-pass encode and submit). The retired ids
    /// were already removed from the map when retired, so they are not
    /// painted in the frame they are finally freed.
    pub fn begin_frame(&mut self) {
        self.retiring.clear();
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

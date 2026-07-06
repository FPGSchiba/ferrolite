//! `RasterStore` — a runtime registry mapping `RasterHandle → MaskBuffer` for the
//! AI/imported-mask seam (design §8). It is the resolution point the `Imported`
//! component reads during compositing. It is a **re-derivable cache** (contract 2):
//! never serialized — only the parametric `MaskProvenance` (the prompt) persists in
//! the sidecar; A2's `ferrolite-ai::segment` job rebuilds the raster from that prompt
//! and populates this store. In P1 there is no producer, so the store is empty and
//! every `Imported` component composites as identity/zero (inert).
//!
//! Engine tier, weight-free: this is a plain map of GPU handles — no `ort`, no model
//! weights, no `ferrolite-ai` dependency (map D6).

use std::collections::HashMap;

use crate::buffer::MaskBuffer;
use crate::model::RasterHandle;

/// Runtime registry of externally-produced raster masks, keyed by `RasterHandle`.
/// Not serialized (the raster is a cache; the prompt is the source of truth).
#[derive(Clone, Default)]
pub struct RasterStore {
    rasters: HashMap<RasterHandle, MaskBuffer>,
}

impl RasterStore {
    /// Immutable-builder insert: returns a new store with `buffer` bound to `handle`.
    pub fn with_raster(mut self, handle: RasterHandle, buffer: MaskBuffer) -> Self {
        self.rasters.insert(handle, buffer);
        self
    }

    /// Bind (or replace) the raster for `handle`.
    pub fn insert(&mut self, handle: RasterHandle, buffer: MaskBuffer) {
        self.rasters.insert(handle, buffer);
    }

    /// Resolve `handle` to its raster buffer, if present.
    pub fn get(&self, handle: RasterHandle) -> Option<&MaskBuffer> {
        self.rasters.get(&handle)
    }

    /// True when no raster is registered (the P1 no-producer default).
    pub fn is_empty(&self) -> bool {
        self.rasters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_gpu::GpuContext;
    use std::sync::Arc;

    #[test]
    fn default_store_is_empty_and_resolves_nothing() {
        let store = RasterStore::default();
        assert!(store.is_empty());
        assert!(store.get(RasterHandle(0)).is_none());
        assert!(store.get(RasterHandle(42)).is_none());
    }

    #[test]
    fn with_raster_binds_and_resolves_by_handle() {
        let Some(ctx) = GpuContext::headless() else {
            eprintln!("no GPU adapter; skipping (headless CI)");
            return;
        };
        let ctx = Arc::new(ctx);
        let buf = MaskBuffer::alloc_zeroed(&ctx, 4, 4);
        let store = RasterStore::default().with_raster(RasterHandle(7), buf);
        assert!(!store.is_empty());
        let got = store.get(RasterHandle(7)).expect("handle 7 resolves");
        assert_eq!((got.width, got.height), (4, 4));
        assert!(
            store.get(RasterHandle(8)).is_none(),
            "unbound handle is None"
        );
    }
}

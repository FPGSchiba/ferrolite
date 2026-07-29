//! Pure present-source selection for the viewer "swapchain". Decides whether a
//! frame shows the smooth preview, a crossfade, or the composed sharp `front`
//! buffer. No egui, no GPU — unit-testable. See spec 4.5 §4.3.

/// What the viewer presents this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PresentSource {
    /// Rung-1 preview (transform-aware, smooth). Shown during interaction and
    /// while the full-res `back` buffer is still composing.
    Preview,
    /// Blend preview -> front by this factor in [0,1) (the swap crossfade).
    Crossfade(f32),
    /// The composed, converged sharp full-res `front` buffer, 1:1.
    Front,
}

/// Select the present source. `interacting` = pan/zoom/slider this frame;
/// `split` = the before/after SPLIT is active (a preview-tier-only compare, so
/// the composed full `front` must never take over while it is shown — always
/// `Preview`); `full_ready` = the sparse tier exists; `front_valid` = the
/// off-screen compose+swap has populated `front` for the CURRENT
/// `(opstack_version, view)` — it subsumes the old `converged` + `present_swapped`
/// pair: it is `false` during edits (version changed → key mismatch), motion
/// (view changed each frame), and right after a canvas resize reallocates the
/// present buffers, staying false until the pool reconverges AND the compose+swap
/// runs again for the exact current state; `crossfade` = the ramp [0,1].
///
/// `Front`/`Crossfade` require `front_valid == true` — otherwise `front` may be
/// blank (just reallocated) or stale (an edit bumped the version, or the view
/// moved), so this falls back to `Preview` even though a frame might look
/// "settled". This keys the swap on exactly what `front` was composed at,
/// fixing edits/split not showing until a zoom nudge and the one-frame blank
/// flash on canvas resize (spec 4.5 final review, I1).
pub fn present_source(
    interacting: bool,
    split: bool,
    full_ready: bool,
    front_valid: bool,
    crossfade: f32,
) -> PresentSource {
    if interacting || split || !full_ready || !front_valid {
        return PresentSource::Preview;
    }
    if crossfade >= 1.0 {
        PresentSource::Front
    } else {
        PresentSource::Crossfade(crossfade)
    }
}

/// Whether the off-screen compose+swap may run this frame.
///
/// `converged` = the sparse pool reports every needed tile resident;
/// `opstack_version` = the app-side edit counter (bumped on EVERY
/// `set_preview_and_full`, including each mid-drag frame);
/// `full_synced_version` = the value of `opstack_version` at the last point the
/// full-res producer was actually (re)synced to the stack (deferred to commit
/// while a slider drag is in progress — the OOM lever);
/// `key_matches_current` = `present_key` already equals the current
/// `(opstack_version, view)` (front is already composed for this exact state).
///
/// The `opstack_version == full_synced_version` term is the mid-drag freeze
/// guard: while a drag defers the full-res tier, the pool's residency is
/// checked against its own FROZEN version, so `converged` stays true for
/// pre-drag tiles. Composing then would stamp those STALE tiles "valid" for
/// the new version and mask the live preview — the "no live edits until
/// release" / pan-zoom raw-flash regression. Blocking the swap keeps
/// `present_key` at the pre-drag version, so `front_valid` goes false and the
/// presenter falls back to the live preview for the whole drag.
pub fn swap_allowed(
    converged: bool,
    opstack_version: u64,
    full_synced_version: u64,
    key_matches_current: bool,
) -> bool {
    converged && opstack_version == full_synced_version && !key_matches_current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_always_shows_preview() {
        // interacting=true, split=false, full_ready=true, front_valid=true
        assert_eq!(
            present_source(true, false, true, true, 1.0),
            PresentSource::Preview
        );
    }

    #[test]
    fn not_ready_shows_preview() {
        assert_eq!(
            present_source(false, false, false, false, 0.0),
            PresentSource::Preview
        );
    }

    #[test]
    fn ready_full_ramp_shows_front() {
        // split=false, full_ready=true, front_valid=true, ramp complete.
        assert_eq!(
            present_source(false, false, true, true, 1.0),
            PresentSource::Front
        );
    }

    #[test]
    fn ready_mid_ramp_crossfades() {
        // split=false, full_ready=true, front_valid=true, ramp mid.
        assert_eq!(
            present_source(false, false, true, true, 0.4),
            PresentSource::Crossfade(0.4)
        );
    }

    /// Split fix: the before/after SPLIT is a preview-tier-only compare, so
    /// even with everything else "ready" (full_ready + front_valid + full ramp)
    /// the composed full `front` must NOT take over — it always shows `Preview`
    /// so the split renders at the settled fit view.
    #[test]
    fn split_active_shows_preview_even_when_ready() {
        assert_eq!(
            present_source(false, true, true, true, 1.0),
            PresentSource::Preview
        );
    }

    /// Stale-front guard: the frame can look "settled" (full_ready + full ramp)
    /// but if `front` was composed for a different `(opstack_version, view)` — an
    /// edit bumped the version, the view moved, or the canvas resized and
    /// reallocated — `front_valid` is false and presenting `Front` would blit a
    /// stale/blank buffer. Must fall back to `Preview` instead. This is the fix
    /// for edits/split not showing until a zoom nudge and the resize blank flash.
    #[test]
    fn front_invalid_shows_preview() {
        assert_eq!(
            present_source(false, false, true, false, 1.0),
            PresentSource::Preview
        );
    }

    /// The no-live-edits regression: mid-drag the deferral leaves the producer
    /// at the last committed version while `opstack_version` bumps every moved
    /// frame — the pool's `converged` is checked against its own frozen version
    /// and lies. The swap MUST stay blocked for the whole drag (else stale
    /// tiles get stamped valid and mask the live preview).
    #[test]
    fn unsynced_producer_blocks_swap_even_when_converged() {
        assert!(!swap_allowed(true, 6, 5, false));
    }

    /// Commit re-syncs the producer (`full_synced_version == opstack_version`);
    /// a stale key (edit or view change) then swaps exactly once.
    #[test]
    fn synced_producer_with_stale_key_swaps() {
        assert!(swap_allowed(true, 6, 6, false));
    }

    /// No re-swap churn when `front` is already composed for the current state.
    #[test]
    fn matching_key_never_reswaps() {
        assert!(!swap_allowed(true, 6, 6, true));
    }

    /// An unconverged pool never swaps, synced or not.
    #[test]
    fn unconverged_never_swaps() {
        assert!(!swap_allowed(false, 6, 6, false));
    }
}

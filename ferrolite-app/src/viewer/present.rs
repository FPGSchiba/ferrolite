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
}

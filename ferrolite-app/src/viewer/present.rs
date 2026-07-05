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
/// `full_ready` = the sparse tier exists; `converged` = the pool is complete for
/// the current transform+version (CPU-rect predicate); `present_swapped` = the
/// off-screen compose+swap has populated `front` for the CURRENT converged state
/// (false right after a canvas resize reallocates the present buffers, even if
/// `converged` itself stayed true — see the resize re-arm in `drive_viewer`);
/// `crossfade` = the ramp [0,1].
///
/// `Front`/`Crossfade` require `present_swapped == true` — otherwise `front`
/// may be blank (just reallocated) or stale, so this falls back to `Preview`
/// even though `converged` and `crossfade` look ready. This is the fix for the
/// one-frame blank flash on canvas resize (spec 4.5 final review, I1).
pub fn present_source(
    interacting: bool,
    full_ready: bool,
    converged: bool,
    present_swapped: bool,
    crossfade: f32,
) -> PresentSource {
    if interacting || !full_ready || !converged || !present_swapped {
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
        assert_eq!(
            present_source(true, true, true, true, 1.0),
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
    fn settled_not_converged_holds_preview() {
        assert_eq!(
            present_source(false, true, false, true, 1.0),
            PresentSource::Preview
        );
    }

    #[test]
    fn converged_mid_ramp_crossfades() {
        assert_eq!(
            present_source(false, true, true, true, 0.4),
            PresentSource::Crossfade(0.4)
        );
    }

    #[test]
    fn converged_full_ramp_shows_front() {
        assert_eq!(
            present_source(false, true, true, true, 1.0),
            PresentSource::Front
        );
    }

    /// Resize-blank guard (spec 4.5 final review, I1): converged with a
    /// full crossfade ramp would normally show `Front`, but if the compose+swap
    /// has not (yet) populated `front` for this converged state — e.g. the
    /// canvas just resized and reallocated the present buffers, re-arming
    /// `present_swapped = false` — presenting `Front` would blit the blanked
    /// buffer for one frame. Must fall back to `Preview` instead.
    #[test]
    fn converged_full_ramp_but_not_swapped_shows_preview() {
        assert_eq!(
            present_source(false, true, true, false, 1.0),
            PresentSource::Preview
        );
    }
}

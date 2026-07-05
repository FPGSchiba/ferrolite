//! Pure present-source selection for the viewer "swapchain". Decides whether a
//! frame shows the smooth preview, a crossfade, or the composed sharp `front`
//! buffer. No egui, no GPU — unit-testable. See spec 4.5 §4.3.

/// What the viewer presents this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
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
/// the current transform+version (CPU-rect predicate); `crossfade` = the ramp [0,1].
#[allow(dead_code)]
pub fn present_source(
    interacting: bool,
    full_ready: bool,
    converged: bool,
    crossfade: f32,
) -> PresentSource {
    if interacting || !full_ready || !converged {
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
            present_source(true, true, true, 1.0),
            PresentSource::Preview
        );
    }

    #[test]
    fn not_ready_shows_preview() {
        assert_eq!(
            present_source(false, false, false, 0.0),
            PresentSource::Preview
        );
    }

    #[test]
    fn settled_not_converged_holds_preview() {
        assert_eq!(
            present_source(false, true, false, 1.0),
            PresentSource::Preview
        );
    }

    #[test]
    fn converged_mid_ramp_crossfades() {
        assert_eq!(
            present_source(false, true, true, 0.4),
            PresentSource::Crossfade(0.4)
        );
    }

    #[test]
    fn converged_full_ramp_shows_front() {
        assert_eq!(present_source(false, true, true, 1.0), PresentSource::Front);
    }
}

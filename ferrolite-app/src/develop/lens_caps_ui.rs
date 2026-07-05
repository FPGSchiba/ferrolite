//! Pure mapping from per-lens data availability (`ferrolite_lens::LensCaps`,
//! FB1) to the Distortion/TCA row's enabled state + disabled-hover-text in the
//! Lens Corrections panel (Spec 4.4, FB2). A matched lens can still lack
//! calibration for a given correction (e.g. many primes have distortion + TCA
//! data but no vignetting profile, or vice versa), so "a lens is matched"
//! alone isn't enough to enable a row — the panel needs to know which
//! corrections that specific lens/focal/aperture combo actually has data for.
//!
//! Vignetting is deliberately NOT modeled here: its row is unconditionally
//! enabled (manual gain works with no lens/profile at all), so it has no
//! enable/disable decision — only a label choice, decided separately by
//! [`vignette_row_label`].

use ferrolite_lens::LensCaps;

/// Which correction row this decision is for. Distortion and TCA both gate on
/// `has_lens` + the corresponding `LensCaps` flag; they differ only in which
/// flag and which "lens matched but no data" message to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatedCorrection {
    Distortion,
    Tca,
}

/// Enabled state + optional disabled-hover-text for a Distortion/TCA row.
/// `hover_text` is `None` when the row is enabled (nothing to explain).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowGate {
    pub enabled: bool,
    pub hover_text: Option<&'static str>,
}

/// Decide whether a Distortion/TCA row should be enabled, and what
/// disabled-hover-text to show when it isn't.
///
/// - No lens matched at all (`has_lens == false`): disabled, "Needs a matched
///   lens" — same message the row already showed pre-FB2, so a user who has
///   never picked a lens sees the identical hint.
/// - A lens is matched but `caps` is `None` (the persisted `lens_id` no longer
///   resolves — e.g. stale profile after a DB update) or came back without
///   this correction's data: disabled, correction-specific "No … data for
///   this lens".
/// - A lens is matched and has the data: enabled, no hover text needed.
pub fn correction_row_gate(
    has_lens: bool,
    caps: Option<LensCaps>,
    which: GatedCorrection,
) -> RowGate {
    let Some(caps) = (if has_lens { caps } else { None }) else {
        // Either no lens matched yet, or one is matched but `lens_caps`
        // couldn't resolve it (stale persisted `lens_id`) — in both cases we
        // have no confirmed data to point to, so use the generic hint rather
        // than claiming a specific correction is missing.
        return RowGate {
            enabled: false,
            hover_text: Some("Needs a matched lens"),
        };
    };
    let has_data = match which {
        GatedCorrection::Distortion => caps.distortion,
        GatedCorrection::Tca => caps.tca,
    };
    if has_data {
        RowGate {
            enabled: true,
            hover_text: None,
        }
    } else {
        RowGate {
            enabled: false,
            hover_text: Some(match which {
                GatedCorrection::Distortion => "No distortion data for this lens",
                GatedCorrection::Tca => "No TCA data for this lens",
            }),
        }
    }
}

/// The Vignette row's label: mode-aware on whether the matched lens has a
/// vignetting profile. The row itself is ALWAYS enabled (manual gain needs no
/// lens), so this only changes the text, signalling to the user which mode
/// (profile vs. manual fallback) is currently driving the slider.
pub fn vignette_row_label(caps: Option<LensCaps>) -> &'static str {
    if caps.map(|c| c.vignetting).unwrap_or(false) {
        "Vignette"
    } else {
        "Vignette (manual)"
    }
}

/// Build the title-line text for a correction group (widget v2, FB "author
/// visual-test round 5"): the plain correction name when available, or the
/// name with the gate's disabled reason appended inline (" — reason") when
/// it isn't. This is the ONLY thing that changed about availability
/// surfacing — the gate truth table itself (`correction_row_gate`) is
/// unchanged; this just renders its `hover_text` inline instead of (or in
/// addition to) on hover, since a hover-only reason was easy to miss.
///
/// `name` is the plain correction name ("Distortion", "TCA", or the
/// `vignette_row_label` result for Vignette). `enabled` + `hover_text` come
/// straight from a `RowGate` (or `true`/`None` for Vignette, which has no
/// gate). Pass the pieces rather than a `RowGate` so Vignette — which has no
/// `RowGate` at all — can reuse the same title builder.
pub fn correction_title(name: &str, enabled: bool, hover_text: Option<&str>) -> String {
    match (enabled, hover_text) {
        (false, Some(reason)) => format!("{name} \u{2014} {reason}"),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(distortion: bool, tca: bool, vignetting: bool) -> LensCaps {
        LensCaps {
            distortion,
            tca,
            vignetting,
        }
    }

    #[test]
    fn no_lens_disables_distortion_with_needs_a_lens_hint() {
        let gate = correction_row_gate(false, None, GatedCorrection::Distortion);
        assert!(!gate.enabled);
        assert_eq!(gate.hover_text, Some("Needs a matched lens"));
    }

    #[test]
    fn no_lens_disables_tca_with_needs_a_lens_hint() {
        let gate = correction_row_gate(false, None, GatedCorrection::Tca);
        assert!(!gate.enabled);
        assert_eq!(gate.hover_text, Some("Needs a matched lens"));
    }

    #[test]
    fn lens_matched_but_caps_none_disables_with_needs_a_lens_hint() {
        // Persisted lens_id no longer resolves (e.g. stale after a DB
        // update): `has_lens` is true (we have a name/id) but `lens_caps`
        // returned `None`. Treat like "no lens" rather than claiming
        // specific missing data we can't actually confirm.
        let gate = correction_row_gate(true, None, GatedCorrection::Distortion);
        assert!(!gate.enabled);
        assert_eq!(gate.hover_text, Some("Needs a matched lens"));
    }

    #[test]
    fn fe_24mm_has_distortion_and_tca_but_no_vignetting() {
        // Mirrors ferrolite-lens's own fixture test naming/scenario.
        let c = Some(caps(true, true, false));
        let distortion = correction_row_gate(true, c, GatedCorrection::Distortion);
        assert!(distortion.enabled);
        assert_eq!(distortion.hover_text, None);

        let tca = correction_row_gate(true, c, GatedCorrection::Tca);
        assert!(tca.enabled);
        assert_eq!(tca.hover_text, None);
    }

    #[test]
    fn lens_matched_without_distortion_data_disables_with_specific_hint() {
        let c = Some(caps(false, true, true));
        let gate = correction_row_gate(true, c, GatedCorrection::Distortion);
        assert!(!gate.enabled);
        assert_eq!(gate.hover_text, Some("No distortion data for this lens"));
    }

    #[test]
    fn lens_matched_without_tca_data_disables_with_specific_hint() {
        let c = Some(caps(true, false, true));
        let gate = correction_row_gate(true, c, GatedCorrection::Tca);
        assert!(!gate.enabled);
        assert_eq!(gate.hover_text, Some("No TCA data for this lens"));
    }

    #[test]
    fn vignette_label_is_plain_when_profile_data_present() {
        assert_eq!(vignette_row_label(Some(caps(true, true, true))), "Vignette");
    }

    #[test]
    fn vignette_label_is_manual_when_no_vignetting_profile() {
        assert_eq!(
            vignette_row_label(Some(caps(true, true, false))),
            "Vignette (manual)"
        );
    }

    #[test]
    fn vignette_label_is_manual_when_caps_is_none() {
        assert_eq!(vignette_row_label(None), "Vignette (manual)");
    }

    #[test]
    fn correction_title_is_plain_name_when_enabled() {
        assert_eq!(correction_title("Distortion", true, None), "Distortion");
    }

    #[test]
    fn correction_title_appends_reason_when_disabled_with_hover_text() {
        assert_eq!(
            correction_title("Distortion", false, Some("Needs a matched lens")),
            "Distortion \u{2014} Needs a matched lens"
        );
        assert_eq!(
            correction_title("Transverse CA", false, Some("No TCA data for this lens")),
            "Transverse CA \u{2014} No TCA data for this lens"
        );
    }

    #[test]
    fn correction_title_ignores_hover_text_when_enabled() {
        // Defensive: enabled rows never carry hover_text in practice
        // (`correction_row_gate` only sets it alongside `enabled: false`),
        // but the title builder should still prefer the plain name if ever
        // called with both set, rather than surface a stale/contradictory reason.
        assert_eq!(
            correction_title("Distortion", true, Some("stale reason")),
            "Distortion"
        );
    }

    #[test]
    fn correction_title_is_plain_name_when_disabled_without_hover_text() {
        // Vignette-shaped inputs: always enabled in practice, but if ever
        // called disabled with no reason, fall back to the plain name rather
        // than panicking or emitting a dangling " — ".
        assert_eq!(correction_title("Vignette", false, None), "Vignette");
    }
}

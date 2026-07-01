//! Read-only four-channel (R,G,B,luma) histogram drawn in the Develop adjustment
//! panel (design-system §6). The pixel counts come from the GPU compute pass
//! (`ferrolite_vt::HistogramPipeline`) via `AppEvent::HistogramReady`; this widget
//! only normalizes + paints them. Not an editable op → no per-control reset.

use ferrolite_vt::{HIST_BINS, HIST_LEN};

/// The largest bin count across all channels (min 1 to avoid divide-by-zero),
/// used as the common vertical scale so channels are comparable.
pub fn peak_bin(bins: &[u32]) -> u32 {
    bins.iter().copied().max().unwrap_or(1).max(1)
}

/// Normalize one channel's 256 bins to `[0,1]` heights against `peak`.
pub fn channel_norm(bins: &[u32], channel: usize, peak: u32) -> Vec<f32> {
    let base = channel * HIST_BINS;
    (0..HIST_BINS)
        .map(|i| bins[base + i] as f32 / peak as f32)
        .collect()
}

const HIST_H: f32 = 96.0;

/// Draw the four-channel histogram into a fixed-height area. `None` (no data yet)
/// paints just the framed background.
pub fn show(ui: &mut egui::Ui, bins: Option<&[u32]>) {
    let (rect, _resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), HIST_H),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, crate::theme::BG_CANVAS);

    let Some(bins) = bins.filter(|b| b.len() == HIST_LEN) else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Histogram\u{2026}",
            egui::FontId::proportional(11.0),
            crate::theme::TEXT_FAINT,
        );
        return;
    };

    let peak = peak_bin(bins);
    // R, G, B, luma — additive-ish translucent fills so overlaps read naturally.
    let channels = [
        (
            0usize,
            egui::Color32::from_rgba_unmultiplied(230, 70, 70, 150),
        ),
        (1, egui::Color32::from_rgba_unmultiplied(70, 200, 90, 150)),
        (2, egui::Color32::from_rgba_unmultiplied(80, 130, 235, 150)),
        (3, egui::Color32::from_rgba_unmultiplied(200, 200, 200, 90)),
    ];
    for (ch, color) in channels {
        let heights = channel_norm(bins, ch, peak);
        let mut pts: Vec<egui::Pos2> = Vec::with_capacity(HIST_BINS + 2);
        pts.push(egui::pos2(rect.left(), rect.bottom()));
        for (i, h) in heights.iter().enumerate() {
            let x = rect.left() + (i as f32 / (HIST_BINS - 1) as f32) * rect.width();
            let y = rect.bottom() - h * (rect.height() - 2.0);
            pts.push(egui::pos2(x, y));
        }
        pts.push(egui::pos2(rect.right(), rect.bottom()));
        painter.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
    }
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, crate::theme::BORDER_STRONG),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_is_max_bin_min_one() {
        assert_eq!(peak_bin(&[0, 0, 0]), 1, "empty peak floors at 1");
        assert_eq!(peak_bin(&[3, 9, 4]), 9);
    }

    #[test]
    fn channel_norm_scales_against_peak() {
        let mut bins = vec![0u32; HIST_LEN];
        bins[0] = 5; // R bin 0
        bins[HIST_BINS] = 10; // G bin 0
        let peak = peak_bin(&bins);
        assert_eq!(peak, 10);
        let r = channel_norm(&bins, 0, peak);
        assert_eq!(r.len(), HIST_BINS);
        assert!((r[0] - 0.5).abs() < 1e-6, "R bin 0 is half the peak");
        let g = channel_norm(&bins, 1, peak);
        assert!((g[0] - 1.0).abs() < 1e-6, "G bin 0 is the peak");
    }
}

//! Pure grid geometry: how many columns fit, and which item indices are visible
//! for a given scroll offset. No egui — unit-testable.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridMetrics {
    pub columns: usize,
    pub cell: f32,
    pub row_height: f32,
}

/// Columns that fit in `available_width` for square-ish `cell` cells separated
/// by `gap`. Always ≥1.
pub fn metrics(available_width: f32, cell: f32, gap: f32) -> GridMetrics {
    let step = cell + gap;
    let columns = (((available_width + gap) / step).floor() as usize).max(1);
    GridMetrics { columns, cell, row_height: cell + gap }
}

/// Inclusive-exclusive range of item indices intersecting the viewport, padded
/// by one row above/below to avoid pop-in at the edges.
pub fn visible_items(
    scroll_top: f32,
    viewport_h: f32,
    m: &GridMetrics,
    item_count: usize,
) -> std::ops::Range<usize> {
    if item_count == 0 || m.row_height <= 0.0 {
        return 0..0;
    }
    let first_row = (scroll_top / m.row_height).floor() as isize - 1;
    let last_row = ((scroll_top + viewport_h) / m.row_height).ceil() as isize + 1;
    let first_row = first_row.max(0) as usize;
    let last_row = last_row.max(0) as usize;
    let start = (first_row * m.columns).min(item_count);
    let end = (last_row * m.columns).min(item_count);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_fits_expected_columns() {
        let m = metrics(820.0, 100.0, 10.0);
        // (820+10)/110 = 7.5 → 7 columns
        assert_eq!(m.columns, 7);
        assert_eq!(m.row_height, 110.0);
    }

    #[test]
    fn metrics_never_zero_columns() {
        assert_eq!(metrics(10.0, 100.0, 10.0).columns, 1);
    }

    #[test]
    fn visible_items_windows_around_scroll() {
        let m = GridMetrics { columns: 5, cell: 100.0, row_height: 110.0 };
        // scrolled to row ~9 (990px), 600px tall viewport.
        let r = visible_items(990.0, 600.0, &m, 1000);
        // first_row = 9-1=8 → start=40; last_row=ceil(1590/110)+1=16 → end=80
        assert_eq!(r.start, 40);
        assert_eq!(r.end, 80);
    }

    #[test]
    fn visible_items_empty_when_no_items() {
        let m = GridMetrics { columns: 5, cell: 100.0, row_height: 110.0 };
        assert_eq!(visible_items(0.0, 600.0, &m, 0), 0..0);
    }

    #[test]
    fn visible_items_clamps_to_item_count() {
        let m = GridMetrics { columns: 5, cell: 100.0, row_height: 110.0 };
        let r = visible_items(0.0, 10_000.0, &m, 12);
        assert_eq!(r.end, 12);
    }
}

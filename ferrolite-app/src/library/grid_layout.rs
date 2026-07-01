//! Pure justified-rows grid geometry: pack thumbnails at their natural aspect
//! ratio into rows that grow to fill the full available width (Flickr/Google
//! Photos style), so cells adapt to each image's form factor and the grid never
//! overflows or leaves a ragged right edge. No egui — unit-testable.

use std::ops::Range;

/// One thumbnail placed within a row. `width` is the cell footprint (which may
/// be wider than the image to fit a longer filename label); `img_width` is the
/// image itself, centered within the footprint. The row's `img_height` gives the
/// height. `x` is the row-relative left of the footprint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowItem {
    pub index: usize,
    pub x: f32,
    pub width: f32,
    pub img_width: f32,
}

/// A justified row of thumbnails sharing one `img_height`, stacked at vertical
/// offset `y`. A meta-label band of `GridLayout::label_h` sits below the images.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub y: f32,
    pub img_height: f32,
    pub items: Vec<RowItem>,
}

/// The full virtualizable layout: every row's geometry plus the total content
/// height (for the scroll area) and the per-cell label band height.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GridLayout {
    pub rows: Vec<Row>,
    pub total_height: f32,
    pub label_h: f32,
}

/// Total width of items `i..j` at row height `h`: each cell is the image width
/// (`aspect * h`) floored to its `min_width` (so a long filename label is never
/// clipped), plus the inter-cell gaps.
fn row_width(aspects: &[f32], min_w: &[f32], i: usize, j: usize, h: f32, gap: f32) -> f32 {
    let mut w = 0.0_f32;
    for k in i..j {
        w += (aspects[k].clamp(0.05, 20.0) * h).max(min_w[k]);
    }
    w + (j - i).saturating_sub(1) as f32 * gap
}

/// Justify images into rows filling `avail_w`, where each cell is at least
/// `min_widths[k]` wide (its label width) so filenames are never clipped.
///
/// A row collects images at `target_h` until they fill `avail_w`, then the row
/// height is solved (binary search — exact even with per-cell min-width floors)
/// so the cells + gaps span the width. A trailing under-full row keeps
/// `target_h`. `label_h` reserves space under every row for the meta text.
pub fn layout(
    aspects: &[f32],
    min_widths: &[f32],
    avail_w: f32,
    target_h: f32,
    gap: f32,
    label_h: f32,
) -> GridLayout {
    let avail_w = avail_w.max(1.0);
    let target_h = target_h.max(1.0);
    let mut rows: Vec<Row> = Vec::new();
    let mut y = 0.0_f32;
    let mut i = 0usize;
    let n = aspects.len();

    while i < n {
        // Greedily grow the row until its width (at target_h) reaches avail_w.
        let mut j = i + 1;
        while j < n && row_width(aspects, min_widths, i, j, target_h, gap) < avail_w {
            j += 1;
        }
        let is_last = j >= n;

        // Solve the row height that fills the width, except a trailing short row.
        let row_h = if is_last && row_width(aspects, min_widths, i, j, target_h, gap) < avail_w {
            target_h
        } else {
            solve_row_height(aspects, min_widths, i, j, avail_w, gap, target_h)
        };

        let mut x = 0.0_f32;
        let mut items = Vec::with_capacity(j - i);
        for k in i..j {
            let img_w = aspects[k].clamp(0.05, 20.0) * row_h;
            let cell_w = img_w.max(min_widths[k]);
            items.push(RowItem {
                index: k,
                x,
                width: cell_w,
                img_width: img_w,
            });
            x += cell_w + gap;
        }
        rows.push(Row {
            y,
            img_height: row_h,
            items,
        });
        y += row_h + label_h + gap;
        i = j;
    }

    GridLayout {
        rows,
        total_height: y,
        label_h,
    }
}

/// Binary-search the row height in `[0.4·target, 3·target]` so items `i..j` plus
/// gaps span `avail_w`. `row_width` is monotonic in `h`, so this converges.
fn solve_row_height(
    aspects: &[f32],
    min_w: &[f32],
    i: usize,
    j: usize,
    avail_w: f32,
    gap: f32,
    target_h: f32,
) -> f32 {
    let (mut lo, mut hi) = (target_h * 0.4, target_h * 3.0);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if row_width(aspects, min_w, i, j, mid, gap) < avail_w {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

impl GridLayout {
    /// Inclusive-exclusive range of row indices intersecting the viewport,
    /// padded by one row above/below to avoid pop-in at the edges.
    pub fn visible_rows(&self, scroll_top: f32, viewport_h: f32) -> Range<usize> {
        if self.rows.is_empty() {
            return 0..0;
        }
        let top = scroll_top;
        let bottom = scroll_top + viewport_h;
        // First row whose bottom edge is at/under the viewport top (binary search).
        let start = self
            .rows
            .partition_point(|r| r.y + r.img_height + self.label_h < top);
        let mut end = start;
        while end < self.rows.len() && self.rows[end].y <= bottom {
            end += 1;
        }
        let start = start.saturating_sub(1);
        let end = (end + 1).min(self.rows.len());
        start..end
    }
}

/// Cache key: the layout is rebuilt only when the image set, available width, or
/// target row height changes (not every frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutSig {
    pub images_rev: u64,
    /// Image count — a defensive guard: even if a mutation forgets to bump
    /// `images_rev`, a length change still invalidates the cache so the render
    /// pass can never index a row item past the current image list.
    pub item_count: usize,
    pub avail_w: u32,
    pub target_h: u32,
}

/// A computed layout tagged with the inputs it was built from.
#[derive(Debug, Clone)]
pub struct CachedGridLayout {
    pub sig: LayoutSig,
    pub layout: GridLayout,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lay out with no label-width floors (the common case in these tests).
    fn lay(aspects: &[f32], avail_w: f32, target_h: f32, gap: f32, label_h: f32) -> GridLayout {
        let zeros = vec![0.0_f32; aspects.len()];
        layout(aspects, &zeros, avail_w, target_h, gap, label_h)
    }

    #[test]
    fn empty_input_yields_no_rows() {
        let l = lay(&[], 800.0, 100.0, 8.0, 30.0);
        assert!(l.rows.is_empty());
        assert_eq!(l.total_height, 0.0);
    }

    #[test]
    fn full_rows_fill_available_width() {
        // Many landscape (3:2) thumbs → several justified rows.
        let aspects = vec![1.5_f32; 20];
        let avail = 800.0;
        let gap = 8.0;
        let l = lay(&aspects, avail, 100.0, gap, 30.0);
        // Every row except possibly the last must span the full width.
        for (ri, row) in l.rows.iter().enumerate() {
            let is_last = ri == l.rows.len() - 1;
            let right = row.items.last().map(|it| it.x + it.width).unwrap_or(0.0);
            if !is_last {
                assert!(
                    (right - avail).abs() < 1.0,
                    "row {ri} right={right} should fill avail={avail}"
                );
            }
        }
    }

    #[test]
    fn item_widths_follow_aspect_ratio() {
        // One row: a 2:1 wide image must be twice as wide as a 1:1 square at the
        // same row height.
        let aspects = vec![2.0_f32, 1.0];
        let l = lay(&aspects, 10_000.0, 100.0, 0.0, 0.0);
        // avail huge → single trailing row at target_h=100.
        assert_eq!(l.rows.len(), 1);
        let items = &l.rows[0].items;
        assert!((items[0].width - 200.0).abs() < 0.5, "2:1 → 200px wide");
        assert!((items[1].width - 100.0).abs() < 0.5, "1:1 → 100px wide");
    }

    #[test]
    fn trailing_row_keeps_target_height_not_ballooned() {
        // A single square on a wide canvas must not be blown up to fill width.
        let l = lay(&[1.0_f32], 2000.0, 120.0, 8.0, 30.0);
        assert_eq!(l.rows.len(), 1);
        assert!((l.rows[0].img_height - 120.0).abs() < 0.5);
    }

    #[test]
    fn min_width_widens_cell_and_centers_image() {
        // A narrow portrait (0.5 aspect) at target_h=100 → image 50px wide, but a
        // 90px label floor must widen the cell to 90 and center the 50px image.
        let l = layout(&[0.5_f32], &[90.0], 2000.0, 100.0, 8.0, 30.0);
        let it = l.rows[0].items[0];
        assert!((it.width - 90.0).abs() < 0.5, "cell floored to label width");
        assert!(
            (it.img_width - 50.0).abs() < 0.5,
            "image keeps aspect width"
        );
        assert!(
            it.img_width < it.width,
            "image narrower than cell → centerable"
        );
    }

    #[test]
    fn total_height_accounts_for_labels_and_gaps() {
        let l = lay(&[1.0_f32, 1.0, 1.0], 100.0, 50.0, 4.0, 20.0);
        // Sum of each row's (img_height + label_h + gap).
        let expected: f32 = l.rows.iter().map(|r| r.img_height + 20.0 + 4.0).sum();
        assert!((l.total_height - expected).abs() < 0.5);
    }

    #[test]
    fn visible_rows_windows_around_scroll() {
        let aspects = vec![1.5_f32; 60];
        let l = lay(&aspects, 800.0, 100.0, 8.0, 30.0);
        let r = l.visible_rows(0.0, 300.0);
        // Starts at row 0 (clamped) and covers the viewport plus padding.
        assert_eq!(r.start, 0);
        assert!(r.end >= 2 && r.end <= l.rows.len());
    }

    #[test]
    fn visible_rows_empty_when_no_rows() {
        let l = GridLayout::default();
        assert_eq!(l.visible_rows(0.0, 600.0), 0..0);
    }
}

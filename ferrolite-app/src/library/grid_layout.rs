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

/// Guard rails on a cell aspect ratio, so a corrupt catalog row cannot produce
/// a degenerate or enormous cell. Wider than `cell_aspect`'s own `[0.1, 10.0]`
/// clamp on purpose: this is the last line of defence, not the policy.
const ASPECT_MIN: f32 = 0.05;
const ASPECT_MAX: f32 = 20.0;

/// Total width of items `i..j` at row height `h`: each cell is the image width
/// (`aspect * h`) floored to its `min_width` (so a long filename label is never
/// clipped), plus the inter-cell gaps.
fn row_width(aspects: &[f32], min_w: &[f32], i: usize, j: usize, h: f32, gap: f32) -> f32 {
    let mut w = 0.0_f32;
    for k in i..j {
        w += (aspects[k].clamp(ASPECT_MIN, ASPECT_MAX) * h).max(min_w[k]);
    }
    w + (j - i).saturating_sub(1) as f32 * gap
}

/// The largest `aspect`-preserving size that fits inside a `cell_w x cell_h`
/// box — a letterbox fit. The caller centers it in the box.
///
/// Needed wherever a thumbnail is painted into a cell whose aspect is NOT
/// already the image's own. `egui::Image::paint_at` maps the whole texture onto
/// the rect it is handed with no aspect handling at all, and
/// `Image::fit_to_exact_size` does NOT change that — `fit` is only consulted by
/// `Image::ui`/`calc_size`, so on the `paint_at` path it is a silent no-op.
/// Handing `paint_at` a fixed-aspect box therefore STRETCHES the image (the
/// export-queue grid squashed every portrait into its 3:2 cell this way). Fit
/// the rect first, then paint into the fitted rect.
///
/// Pure and egui-free like the rest of this module, so it is unit-testable; it
/// returns a size rather than a `Rect` to keep it that way.
pub fn fit_size(cell_w: f32, cell_h: f32, aspect: f32) -> (f32, f32) {
    let cell_w = cell_w.max(1.0);
    let cell_h = cell_h.max(1.0);
    // A non-finite or non-positive aspect (an absent/corrupt thumbnail row)
    // falls back to the cell's own aspect, i.e. the image fills the box —
    // never a zero, negative, or NaN extent.
    let a = if aspect.is_finite() && aspect > 0.0 {
        aspect.clamp(ASPECT_MIN, ASPECT_MAX)
    } else {
        cell_w / cell_h
    };
    let w = cell_w.min(a * cell_h);
    let h = cell_h.min(cell_w / a);
    (w.max(1.0), h.max(1.0))
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

    // --- fit_size (letterbox fit) ---------------------------------------

    /// Cell is 3:2 like the export queue's 132x88 thumbnail box.
    const CELL: (f32, f32) = (132.0, 88.0);

    fn fit(aspect: f32) -> (f32, f32) {
        fit_size(CELL.0, CELL.1, aspect)
    }

    #[test]
    fn fit_size_matching_aspect_fills_the_box_exactly() {
        let (w, h) = fit(CELL.0 / CELL.1);
        assert!((w - CELL.0).abs() < 0.01 && (h - CELL.1).abs() < 0.01);
    }

    #[test]
    fn fit_size_portrait_is_height_bound_and_keeps_its_aspect() {
        // 2:3 portrait in a 3:2 box: height-bound, much narrower than the cell.
        let a = 2.0 / 3.0;
        let (w, h) = fit(a);
        assert!((h - CELL.1).abs() < 0.01, "portrait fills the height");
        assert!(
            w < CELL.0,
            "portrait must not fill the width ({w} vs {})",
            CELL.0
        );
        assert!(
            (w / h - a).abs() < 1e-3,
            "aspect must survive the fit: got {}, want {a}",
            w / h
        );
    }

    #[test]
    fn fit_size_panorama_is_width_bound_and_keeps_its_aspect() {
        let a = 4.0;
        let (w, h) = fit(a);
        assert!((w - CELL.0).abs() < 0.01, "panorama fills the width");
        assert!(h < CELL.1, "panorama must not fill the height");
        assert!((w / h - a).abs() < 1e-3, "aspect must survive the fit");
    }

    /// The regression for the export-queue distortion: whatever the source
    /// aspect, the fitted size is CONTAINED in the cell and never stretched.
    #[test]
    fn fit_size_is_always_contained_and_never_distorts() {
        for a in [0.1_f32, 0.5, 0.6667, 1.0, 1.5, 1.8, 2.5, 4.0, 10.0] {
            let (w, h) = fit(a);
            assert!(
                w <= CELL.0 + 0.01 && h <= CELL.1 + 0.01,
                "aspect {a}: {w}x{h} escapes the {}x{} cell",
                CELL.0,
                CELL.1
            );
            assert!(
                (w / h - a).abs() < 1e-3,
                "aspect {a}: fitted aspect {} differs — the image would be                  stretched",
                w / h
            );
            // And it must touch at least one edge, i.e. it is the LARGEST fit.
            assert!(
                (w - CELL.0).abs() < 0.01 || (h - CELL.1).abs() < 0.01,
                "aspect {a}: {w}x{h} is smaller than the largest fit"
            );
        }
    }

    #[test]
    fn fit_size_degenerate_inputs_stay_positive() {
        for a in [0.0_f32, -3.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let (w, h) = fit(a);
            assert!(
                w >= 1.0 && h >= 1.0 && w.is_finite() && h.is_finite(),
                "aspect {a:?} yielded {w}x{h}"
            );
            assert!(w <= CELL.0 + 0.01 && h <= CELL.1 + 0.01);
        }
    }

    #[test]
    fn fit_size_tolerates_a_degenerate_cell() {
        let (w, h) = fit_size(0.0, -5.0, 1.5);
        assert!(w >= 1.0 && h >= 1.0 && w.is_finite() && h.is_finite());
    }

    #[test]
    fn visible_rows_empty_when_no_rows() {
        let l = GridLayout::default();
        assert_eq!(l.visible_rows(0.0, 600.0), 0..0);
    }
}

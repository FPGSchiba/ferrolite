//! Virtualized thumbnail grid. Realizes only the visible window of cells and
//! pulls ready thumbnails from the read pool on demand (lazy-load). Ingest
//! thumbnails are generated inline during ingest, so there are no separate
//! per-image thumbnail jobs for the grid to reprioritize by visibility.

use crate::library::cell_state::{cell_state, CellState};
use crate::library::grid_layout::{uniform_layout, CachedUniformLayout, UniformLayoutSig};
use crate::library::icons;
use crate::state::AppState;
use crate::theme;
use ferrolite_catalog::ImageRecord;
use ferrolite_image::Flag;
use std::collections::HashSet;

const GAP: f32 = 8.0;
const SEL_ROUND: f32 = 6.0;
/// Height of the meta-label band (filename + capture date) under each cell.
const LABEL_H: f32 = 30.0;
/// Gap between the thumbnail and its label band.
const LABEL_PAD: f32 = 3.0;
/// Spec §4.3's text inset: horizontal breathing room between the label band's
/// elided text and the cell's own left/right edges, so the ellipsis does not
/// land flush on the cell boundary.
const LABEL_INSET: f32 = 3.0;
/// Outer padding around the grid (left, right, top, bottom) so cells don't hug
/// the panel edges.
const MARGIN: f32 = 14.0;

/// Size-slider → cell-width mapping from `docs/design/V2/README.md:42`
/// (`118 + sizePct * 1.7`), spec decision D4. The slider is a 0..=100 percentage
/// (`settings.grid_size`, default 46), so this yields 118..288 px, default ~196.
///
/// Replaced `thumb_size + 60` (60..160). The old 60 px floor made a cell narrower
/// than a typical filename at small sizes, which is how the label-width floor
/// came to dominate the layout in the first place.
const CELL_W_BASE: f32 = 118.0;
const CELL_W_PER_PCT: f32 = 1.7;

/// Cell width for a `0..=100` Size-slider percentage. Clamps out-of-range input
/// so a hand-edited or future-versioned settings file cannot produce a
/// degenerate cell.
pub(crate) fn cell_width_for_size(size_pct: f32) -> f32 {
    CELL_W_BASE + size_pct.clamp(0.0, 100.0) * CELL_W_PER_PCT
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState, cell: f32) -> Option<i64> {
    let avail_w = (ui.available_width() - 2.0 * MARGIN).max(1.0);
    // `cell` is the cell WIDTH (spec D4's slider mapping); height follows from
    // `CELL_ASPECT` inside `uniform_layout`.
    let cell_w = cell.max(1.0);

    // Rebuild only when the image set, width, or cell size changed. Taken out of
    // `state` for the render pass so `paint_cell` can borrow `state` mutably
    // without aliasing; restored at the end.
    let sig = UniformLayoutSig {
        images_rev: state.images_rev,
        item_count: state.images.len(),
        avail_w: avail_w.round() as u32,
        cell_w: cell_w.round() as u32,
    };
    let mut cache = state.grid_layout.take();
    if cache.as_ref().map(|c| c.sig) != Some(sig) {
        cache = Some(CachedUniformLayout {
            sig,
            layout: uniform_layout(state.images.len(), avail_w, cell_w, GAP, LABEL_PAD, LABEL_H),
        });
    }
    let cache = cache.expect("layout built above");

    // Built once per frame (not per cell) so membership checks stay O(1) instead
    // of O(queue length) per cell.
    let queued: HashSet<i64> = state.export_queue.iter().copied().collect();
    // Computed once per frame, not per cell — cells with no texture render a
    // "generating" affordance instead of a flat placeholder while an ingest is
    // active (see `cell_state::CellState::Generating`).
    let is_ingesting = state.active_ingests > 0;

    let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
    let mut opened: Option<i64> = None;
    scroll.show_viewport(ui, |ui, viewport| {
        ui.set_height(cache.layout.total_height + 2.0 * MARGIN);
        // Content is offset down/right by MARGIN, so map the viewport into the
        // layout's own (0-based) coordinate space before picking visible rows.
        let scroll_top = (viewport.min.y - MARGIN).max(0.0);
        let vh = viewport.height() + MARGIN;
        let rows = cache.layout.visible_rows(scroll_top, vh);
        // Bounded by the viewport, never by the item count (CLAUDE.md §1) — see
        // `uniform_indices_for_rows_is_bounded_by_the_viewport_not_the_item_count`.
        let indices = cache.layout.indices_for_rows(rows);

        // Compute the visible id set (used to fetch tag associations for the
        // window). Ingest thumbnails are now generated inline within the ingest
        // job — there are no separate per-image thumbnail jobs to reprioritize by
        // visibility, so the old promote/demote pass is gone.
        let mut now_visible: HashSet<i64> = HashSet::new();
        for i in indices.clone() {
            now_visible.insert(state.images[i].id);
        }
        // Fetch tag associations for the visible window (only missing ids queried).
        state.ensure_tags_for(&now_visible);
        // Fetch collection membership for the same visible window, so the
        // "Add/Remove to collection" submenus can decide addable/removable
        // without a synchronous DB call.
        state.ensure_collections_for(&now_visible);
        // Cancel any lazy-load thumbnail fetches for cells scrolled out of view
        // this frame, so a big scroll doesn't leave a stale backlog blocking the
        // now-visible cells (Round 4 fix).
        state.retain_visible_thumbnail_jobs(&now_visible);

        let origin = ui.min_rect().left_top() + egui::vec2(MARGIN, MARGIN);
        for i in indices {
            let rec = state.images[i].clone();
            let (cx, cy) = cache.layout.cell_offset(i);
            let cell_rect = egui::Rect::from_min_size(
                origin + egui::vec2(cx, cy),
                egui::vec2(cache.layout.cell_w, cache.layout.cell_h),
            );
            if let Some(id) = paint_cell(
                ui,
                state,
                &rec,
                cell_rect,
                queued.contains(&rec.id),
                is_ingesting,
            ) {
                opened = Some(id);
            }
            let label_rect = egui::Rect::from_min_size(
                egui::pos2(cell_rect.left(), cell_rect.bottom() + LABEL_PAD),
                egui::vec2(cell_rect.width(), LABEL_H - LABEL_PAD),
            );
            paint_meta(ui, &rec, label_rect);
        }
    });
    state.grid_layout = Some(cache);

    if let Some(p) = egui::DragAndDrop::payload::<crate::library::drag::DraggedImages>(ui.ctx()) {
        crate::library::drag::draw_drag_chip(ui.ctx(), p.0.len());
    }

    opened
}

/// Upright aspect ratio (width / height) of an image, matching what the
/// thumbnail actually shows.
///
/// Prefers the persisted thumbnail's OWN dimensions (`thumb_w`/`thumb_h`,
/// joined from the `thumbnails` table) when present: those are already
/// display-upright and reflect any crop/geometry edit baked in by a
/// thumbnail regen (`develop::thumb_regen`), whereas `width`/`height` are the
/// ingest-time SENSOR-space dims and never change after a crop (see
/// `ImageRecord::thumb_w` doc comment) — using them directly would show the
/// pre-crop aspect ratio forever. Falls back to `width`/`height` +
/// orientation swap (and ultimately square 1.0) only when no thumbnail row
/// exists yet, e.g. a freshly-scanned `Pending` row.
pub(crate) fn cell_aspect(rec: &ImageRecord) -> f32 {
    if let (Some(tw), Some(th)) = (rec.thumb_w, rec.thumb_h) {
        return (tw.max(1) as f32 / th.max(1) as f32).clamp(0.1, 10.0);
    }
    let w = rec.width.unwrap_or(0).max(1) as f32;
    let h = rec.height.unwrap_or(0).max(1) as f32;
    let (w, h) = if rec.orientation.swaps_dimensions() {
        (h, w)
    } else {
        (w, h)
    };
    (w / h).clamp(0.1, 10.0)
}

/// Whether `paint_cell` should submit its ordinary lazy-load thumbnail fetch
/// (`AppState::request_thumbnail`) for a visible cell this frame.
///
/// `false` whenever: the cell already has a texture (nothing to fetch);
/// there is no thumbnail row yet (`decode_done == false`, i.e. a `Pending`
/// row — see the call site's comment on why fetching would just re-spawn
/// every frame); or a stale-thumbnail regen already owns this id
/// (`stale_regen_inflight == true`, P7 Task 10's storm-fix). That last case
/// is why this is a combined predicate rather than three separate `if`s at
/// the call site: a stale regen SUPERSEDES the lazy load — it is about to
/// replace the very blob the lazy load would fetch — so the two decisions
/// must be evaluated together, with the staleness check settled first.
pub(crate) fn should_request_lazy_thumbnail(
    has_texture: bool,
    decode_done: bool,
    stale_regen_inflight: bool,
) -> bool {
    !has_texture && decode_done && !stale_regen_inflight
}

/// Draw the per-cell meta label under the thumbnail: filename on top, capture
/// date below, both centered and both ELIDED to the cell width.
///
/// Eliding (not measuring) is the rule: `Label::truncate()` lays out one galley
/// for one visible cell inside the already-virtualized render pass, whereas the
/// `label_width` floor this replaces measured every filename in the catalog on
/// each layout rebuild. The full name on hover comes free from `egui::Label`
/// itself: it checks the galley's own `elided` flag and attaches the hover
/// tooltip ONLY when the text was actually truncated (`egui-0.29.1/src/widgets/
/// label.rs`) — so nothing more needs doing here, and nothing should be added:
/// an explicit `.on_hover_text(...)` on this response would stack a SECOND
/// tooltip bubble on a long name (each call adds a bubble, it does not replace
/// one) and would wrongly attach a tooltip to a short, non-elided name.
///
/// `selectable(false)` keeps the label inert so it never steals the drag or
/// click that the cell's own `interact` owns.
fn paint_meta(ui: &mut egui::Ui, rec: &ImageRecord, rect: egui::Rect) {
    let rect = rect.shrink2(egui::vec2(LABEL_INSET, 0.0));
    let name_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 14.0));
    ui.put(
        name_rect,
        egui::Label::new(
            egui::RichText::new(&rec.filename)
                .size(11.0)
                .color(theme::TEXT_PRIMARY),
        )
        .truncate()
        .selectable(false),
    );

    if let Some(date) = format_capture_date(rec.capture_time.as_deref()) {
        let date_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.top() + 14.0),
            egui::vec2(rect.width(), (rect.height() - 14.0).max(1.0)),
        );
        ui.put(
            date_rect,
            egui::Label::new(egui::RichText::new(date).size(10.0).color(theme::TEXT_DIM))
                .truncate()
                .selectable(false),
        );
    }
}

/// Format an EXIF `DateTimeOriginal` ("YYYY:MM:DD HH:MM:SS") as "YYYY-MM-DD
/// HH:MM" for display. Returns `None` for missing/empty values; passes through
/// unexpected formats (first 16 chars) rather than failing.
fn format_capture_date(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim();
    if s.is_empty() {
        return None;
    }
    let out: String = s
        .chars()
        .take(16)
        .enumerate()
        .map(|(i, c)| {
            if (i == 4 || i == 7) && c == ':' {
                '-'
            } else {
                c
            }
        })
        .collect();
    Some(out)
}

/// Compute the inclusive range of image indices between `anchor_idx` and
/// `target_idx` (order-independent). Returns both endpoints and all in between.
pub fn range_indices(anchor_idx: usize, target_idx: usize) -> std::ops::RangeInclusive<usize> {
    anchor_idx.min(target_idx)..=anchor_idx.max(target_idx)
}

fn paint_cell(
    ui: &mut egui::Ui,
    state: &mut AppState,
    rec: &ferrolite_catalog::ImageRecord,
    rect: egui::Rect,
    queued: bool,
    is_ingesting: bool,
) -> Option<i64> {
    // Determine selection state early so we can adjust the thumbnail rect.
    let selected = state.selection.contains(&rec.id) || state.selected == Some(rec.id);

    // P7 §5.2 lazy-refresh consumer (Task 10): a batch preset apply flagged
    // this thumbnail stale (Task 4) — regenerate it now that the cell is
    // actually on screen, instead of the moment it was applied. The grid is
    // already virtualized, so this only ever pays the decode + GPU render +
    // encode cost for cells the user actually looks at.
    //
    // This MUST run before the lazy-load request below, not after: a cell
    // that is both un-textured AND stale is the feature's primary scenario
    // (the first browse of a folder right after a batch apply), and if the
    // ordinary lazy load already fired this frame, it would be fetching the
    // very stale blob this regen is about to replace — a second job, in the
    // hot path this feature exists to make cheap, doing knowably wrong work.
    // See `should_request_lazy_thumbnail` below for how the two decisions
    // combine.
    //
    // Gated on `Done`: no thumbnail row exists yet for a `Pending` image, so
    // there is nothing to check. `stale_regen_inflight`/`stale_checked_fresh`
    // short-circuit the indexed `is_thumbnail_stale` read-pool lookup for an
    // id already known this "epoch" (in flight, or freshly confirmed
    // not-stale) — see their doc comments on `AppState` for why this is
    // bounded, not unbounded: a cell that stays on screen across many frames
    // (e.g. a held scroll drag) must not re-query the catalog every single
    // frame. This cannot spawn the regen job directly (no access to
    // `eframe::Frame`/the GPU render state here) — it only enqueues;
    // `FerroliteApp::drain_stale_thumb_regen_requests` (app.rs), called once
    // per frame, does the actual spawn.
    let decode_done = rec.decode_status == ferrolite_catalog::DecodeStatus::Done;
    let mut stale_regen_inflight = state.stale_regen_inflight.contains(&rec.id);
    if decode_done && !stale_regen_inflight && !state.stale_checked_fresh.contains(&rec.id) {
        let stale = state.reads.is_thumbnail_stale(rec.id).unwrap_or(false);
        if crate::develop::thumb_regen::should_regen_stale(stale, stale_regen_inflight) {
            state.stale_regen_inflight.insert(rec.id);
            state.pending_stale_regen.push(rec.id);
            stale_regen_inflight = true;
        } else {
            state.stale_checked_fresh.insert(rec.id);
        }
    }

    // Request a thumbnail off-thread if not yet cached (visible cell only). The
    // DB read + JPEG decode happen in a `Visible`-priority job; the decoded
    // pixels arrive over the event channel and are uploaded there. NO UI-thread
    // decode here.
    //
    // Gated on `Done` (not just `!= Failed`): a `Done` row's thumbnail blob is
    // written in the same atomic batch as its row, so `Done` implies the blob
    // is present. A `Pending` row (not yet reached by ingest) has no blob yet —
    // requesting one would submit a `Visible` job that immediately finds
    // nothing and re-spawns every frame (the lazy-load re-spawn storm). A
    // `Pending` cell instead shows the `Generating` spinner while ingesting and
    // gets its texture from the ingest `ThumbReady` path once generation
    // reaches it (unchanged).
    //
    // Also skipped whenever a stale regen owns this id (just enqueued above,
    // or already in flight from an earlier frame): that job will deliver the
    // correct, post-edit thumbnail, so a lazy load here would only be a
    // second job racing to fetch the stale blob the regen is replacing. Until
    // the regen lands the cell just keeps showing whatever it already has
    // (or the usual placeholder) — the honest state, not a stale render.
    if should_request_lazy_thumbnail(
        state.textures.contains(rec.id),
        decode_done,
        stale_regen_inflight,
    ) {
        state.request_thumbnail(ui.ctx(), rec.id);
    }

    let has_tex = state.textures.contains(rec.id);
    let painter = ui.painter_at(rect);

    // The image is letterboxed inside the cell box, not stretched to it.
    // `egui::Image::paint_at` maps the whole texture onto whatever rect it is
    // given with no aspect handling, and `Image::fit_to_exact_size` does NOT
    // change that (`fit` is only read by `Image::ui`/`calc_size`, so on this
    // path it is a silent no-op) — so the fit has to be computed here. The
    // aspect comes from the RECORD via the shared `cell_aspect`, not from the
    // texture, so the cell does not reflow when the thumbnail finishes loading.
    // Every overlay below (rating, flag, queue badge, tag dots, selection ring)
    // anchors to `img_rect`, so they hug the thumbnail rather than floating over
    // a letterbox bar (spec D5).
    let (img_w, img_h) =
        crate::library::grid_layout::fit_size(rect.width(), rect.height(), cell_aspect(rec));
    let img_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(img_w, img_h));

    // Round the thumbnail corners to match the selection border so a square
    // corner never pokes outside the rounded border. Unselected cells stay square.
    let img_round = if selected { SEL_ROUND } else { 0.0 };

    // Cell ground, always painted across the full CELL box (not just
    // `img_rect`): the hit area below (`ui.interact(rect, ...)`) is the whole
    // cell, so for a non-3:2 image the letterbox bars are otherwise bare
    // canvas and the clickable region extends past anything visible. Uses the
    // cell's own rounding so a selected cell's rounded corners still line up.
    // Mirrors the export queue's identical fill (`export_module::queue_list`).
    // Fully covered (no visual change) when the image fills the cell.
    painter.rect_filled(rect, img_round, theme::BG_PANEL);

    match cell_state(rec, has_tex, is_ingesting) {
        CellState::Ready => {
            if let Some(tex) = state.textures.get(rec.id) {
                let img = egui::Image::new(tex)
                    .fit_to_exact_size(img_rect.size())
                    .rounding(img_round);
                img.paint_at(ui, img_rect);
            }
        }
        CellState::Placeholder => {
            painter.rect_filled(img_rect, img_round.max(2.0), theme::BG_PANEL);
        }
        CellState::Generating => {
            // Same base fill as the idle placeholder, plus a small centered
            // spinner so a not-yet-generated cell reads as "working" instead
            // of "untouched" during ingest. `egui::Spinner` paints one arc
            // (O(1) geometry, same widget used by the viewer's loading state)
            // and animates via the shared egui animation clock, so this stays
            // O(visible cells) per frame — no per-cell timers or extra state.
            painter.rect_filled(img_rect, img_round.max(2.0), theme::BG_PANEL);
            let spinner_size = 16.0_f32
                .min(img_rect.width() * 0.5)
                .min(img_rect.height() * 0.5);
            if spinner_size >= 4.0 {
                let spinner_rect = egui::Rect::from_center_size(
                    img_rect.center(),
                    egui::Vec2::splat(spinner_size),
                );
                ui.put(spinner_rect, egui::Spinner::new().size(spinner_size));
            }
            // Keep the spinner animating only while there's actually an
            // ingest running — an idle grid must not burn CPU on continuous
            // repaint (Placeholder cells never reach this branch when idle,
            // so this call site is gated by construction, not just by `if`).
            ui.ctx().request_repaint();
        }
        CellState::Failed => {
            painter.rect_filled(img_rect, img_round.max(2.0), theme::BG_PANEL);
            painter.text(
                img_rect.center(),
                egui::Align2::CENTER_CENTER,
                "broken",
                egui::FontId::proportional(11.0),
                theme::SEMANTIC_RED,
            );
        }
    }

    // #8 — Rating stars (bottom-left): drawn shapes instead of glyphs.
    // Overlays are anchored to img_rect so they hug the thumbnail in both states.
    if rec.rating.get() > 0 {
        // origin = left-centre of the star row, sitting 8px above the bottom edge.
        let r = 4.0_f32;
        let gap = 2.0_f32;
        let row_y = img_rect.bottom() - 8.0;
        let origin = egui::pos2(img_rect.left() + 4.0 + r, row_y);
        // Show only the filled stars (no empty outlines): the grid overlay is a
        // status indicator, not an editable control — empties would imply clicks
        // that the grid doesn't handle. Matches the filmstrip.
        icons::rating_stars(
            &painter,
            origin,
            r,
            gap,
            rec.rating.get(),
            rec.rating.get(),
            theme::STAR,
            true,
        );
    }

    // #8 — Flag icon (top-left): icon-font glyph, not a hand-drawn shape.
    match rec.flag {
        Flag::Pick => {
            icons::flag(
                &painter,
                egui::pos2(img_rect.left() + 6.0, img_rect.top() + 12.0),
                10.0,
                true,
                theme::SEMANTIC_GREEN,
                true,
                false,
            );
        }
        Flag::Reject => {
            icons::flag(
                &painter,
                egui::pos2(img_rect.left() + 6.0, img_rect.top() + 12.0),
                10.0,
                true,
                theme::SEMANTIC_RED,
                true,
                true,
            );
        }
        Flag::None => {}
    }

    // Export-queue badge (top-right): small accent square with a "Q" glyph.
    // Top-right is otherwise unused by the flag (top-left) and rating/tag
    // (bottom) overlays.
    if queued {
        icons::queued_badge(
            &painter,
            egui::pos2(img_rect.right() - 4.0, img_rect.top() + 4.0),
            14.0,
            theme::TEXT_PRIMARY,
            theme::ACCENT,
        );
    }

    // Tag colour dots (bottom-right), looked up from the loaded vocabulary.
    if let Some(tag_ids) = state.visible_tags.get(&rec.id) {
        let mut x = img_rect.right() - 8.0;
        for tid in tag_ids.iter().take(5) {
            if let Some(t) = state.tags.iter().find(|t| t.id == *tid) {
                let c = egui::Color32::from_rgb(t.color.r, t.color.g, t.color.b);
                painter.circle_filled(egui::pos2(x, img_rect.bottom() - 8.0), 4.0, c);
                x -= 11.0;
            }
        }
    }

    // Selection: ctrl/cmd-click toggles; shift-click range-select; plain click replaces.
    // Context menu on right-click.
    // Hit area is the full CELL box, not the letterboxed image: a uniform,
    // gap-free target means a portrait is no harder to click than a panorama.
    let resp = ui.interact(
        rect,
        ui.id().with(("cell", rec.id)),
        egui::Sense::click_and_drag(),
    );

    // Begin a drag carrying the selection (or just this image).
    if resp.drag_started() {
        let ids = crate::library::drag::ids_for_drag(rec.id, &state.selection);
        egui::DragAndDrop::set_payload(ui.ctx(), crate::library::drag::DraggedImages(ids));
    }

    if resp.clicked() {
        let (shift, multi) =
            ui.input(|i| (i.modifiers.shift, i.modifiers.command || i.modifiers.ctrl));
        if shift {
            // Range select: find anchor index (anchor → selected → this image).
            let anchor_id = state.selection_anchor.or(state.selected).unwrap_or(rec.id);
            let anchor_idx = state
                .images
                .iter()
                .position(|r| r.id == anchor_id)
                .unwrap_or(0);
            let target_idx = state
                .images
                .iter()
                .position(|r| r.id == rec.id)
                .unwrap_or(anchor_idx);
            state.selection = range_indices(anchor_idx, target_idx)
                .map(|i| state.images[i].id)
                .collect();
            // Anchor does not move on shift-click.
            state.selected = Some(rec.id);
        } else if multi {
            if !state.selection.remove(&rec.id) {
                state.selection.insert(rec.id);
            }
            state.selection_anchor = Some(rec.id);
            state.selected = Some(rec.id);
        } else {
            state.selection.clear();
            state.selection.insert(rec.id);
            state.selection_anchor = Some(rec.id);
            state.selected = Some(rec.id);
        }
    }
    let mut opened = None;
    if resp.double_clicked() {
        opened = Some(rec.id);
    }

    // Selection border: a bright-blue rounded ring with a ~1px dark keyline on
    // each side, so it stays distinct on both dark and light/bluish thumbnails.
    // The whole 4px band is inset 2px so it sits fully inside the cell — the
    // painter is clipped to `rect`, so a band centered nearer the edge would have
    // its outer half clipped away (which hid the halo before).
    if selected {
        let path = img_rect.shrink(2.0);
        painter.rect_stroke(
            path,
            SEL_ROUND,
            egui::Stroke::new(4.0_f32, egui::Color32::from_black_alpha(200)),
        );
        painter.rect_stroke(
            path,
            SEL_ROUND,
            egui::Stroke::new(2.0_f32, theme::ACCENT_BRIGHT),
        );
    }

    // #5 — Right-click context menu (shared helper).
    let rec_id = rec.id;
    resp.context_menu(|ui| {
        crate::library::image_context_menu::show(ui, state, rec_id, false);
    });

    opened
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_catalog::{DecodeStatus, FileKind};
    use ferrolite_image::{Flag, Orientation, Rating};

    /// Minimal `ImageRecord` fixture for `cell_aspect` tests. `width`/`height`
    /// stand in for the ingest-time sensor dims; `thumb_w`/`thumb_h` stand in
    /// for the persisted thumbnail's own (possibly cropped) dims.
    fn rec(
        width: Option<u32>,
        height: Option<u32>,
        orientation: Orientation,
        thumb_w: Option<u32>,
        thumb_h: Option<u32>,
    ) -> ImageRecord {
        ImageRecord {
            id: 1,
            folder_id: 1,
            filename: "x.nef".into(),
            width,
            height,
            orientation,
            capture_time: None,
            iso: None,
            decode_status: DecodeStatus::Done,
            kind: FileKind::Raw,
            rating: Rating::default(),
            flag: Flag::None,
            has_edits: false,
            thumb_w,
            thumb_h,
        }
    }

    /// P7 Task 10 fix: a cell that is both un-textured AND stale must spawn
    /// ONLY the regen, never both jobs for the same `image_id` — the regen
    /// supersedes the lazy load.
    #[test]
    fn stale_regen_supersedes_the_lazy_load_for_untextured_cells() {
        assert!(
            !should_request_lazy_thumbnail(false, true, true),
            "untextured + stale-regen-inflight must NOT also fetch the (about-to-be-replaced) stale blob"
        );
    }

    #[test]
    fn untextured_fresh_cell_still_requests_the_lazy_load() {
        assert!(
            should_request_lazy_thumbnail(false, true, false),
            "the ordinary case (no regen in play) must be unaffected"
        );
    }

    #[test]
    fn textured_cell_never_requests_a_lazy_load_regardless_of_staleness() {
        assert!(!should_request_lazy_thumbnail(true, true, false));
        assert!(!should_request_lazy_thumbnail(true, true, true));
    }

    #[test]
    fn pending_row_never_requests_a_lazy_load() {
        // decode_done == false (no thumbnail row yet) must short-circuit
        // regardless of texture/stale-regen state, matching the existing
        // Pending-row rationale at the call site.
        assert!(!should_request_lazy_thumbnail(false, false, false));
        assert!(!should_request_lazy_thumbnail(false, false, true));
    }

    #[test]
    fn cell_aspect_prefers_thumbnail_dims_when_present() {
        // Sensor dims say landscape 4:3, but the persisted thumbnail (after a
        // crop-driven regen) is portrait — the cell must follow the thumbnail,
        // not the stale ingest-time width/height.
        let r = rec(
            Some(4000),
            Some(3000),
            Orientation::Normal,
            Some(120),
            Some(200),
        );
        assert!(
            (cell_aspect(&r) - 120.0 / 200.0).abs() < 1e-6,
            "aspect must come from thumb_w/thumb_h, not width/height"
        );
    }

    #[test]
    fn cell_aspect_falls_back_to_width_height_when_no_thumbnail_yet() {
        // A freshly-scanned Pending row has no thumbnails row yet.
        let r = rec(Some(4000), Some(3000), Orientation::Normal, None, None);
        assert!((cell_aspect(&r) - 4000.0 / 3000.0).abs() < 1e-6);
    }

    #[test]
    fn cell_aspect_fallback_still_applies_orientation_swap() {
        // No thumbnail yet + a 90°-rotated EXIF orientation: the fallback path
        // must still swap width/height so the cell isn't the wrong way round.
        let r = rec(Some(4000), Some(3000), Orientation::Rotate90, None, None);
        assert!((cell_aspect(&r) - 3000.0 / 4000.0).abs() < 1e-6);
    }

    #[test]
    fn cell_aspect_uncropped_reedit_restores_original_aspect() {
        // A crop regen (portrait thumb) followed by an un-cropped re-edit
        // regen: the thumbnail dims are back to the upright original aspect,
        // and the cell must follow — no stale cropped aspect lingers.
        let cropped = rec(
            Some(4000),
            Some(3000),
            Orientation::Normal,
            Some(120),
            Some(200),
        );
        assert!((cell_aspect(&cropped) - 120.0 / 200.0).abs() < 1e-6);

        let uncropped = rec(
            Some(4000),
            Some(3000),
            Orientation::Normal,
            Some(400),
            Some(300),
        );
        assert!((cell_aspect(&uncropped) - 4000.0 / 3000.0).abs() < 1e-6);
    }

    #[test]
    fn range_indices_low_to_high() {
        let r: Vec<usize> = range_indices(2, 5).collect();
        assert_eq!(r, vec![2, 3, 4, 5]);
    }

    #[test]
    fn range_indices_high_to_low() {
        let r: Vec<usize> = range_indices(5, 2).collect();
        assert_eq!(r, vec![2, 3, 4, 5]);
    }

    #[test]
    fn range_indices_same_point() {
        let r: Vec<usize> = range_indices(3, 3).collect();
        assert_eq!(r, vec![3]);
    }

    #[test]
    fn format_capture_date_converts_exif_to_display() {
        assert_eq!(
            format_capture_date(Some("2023:05:14 18:32:07")).as_deref(),
            Some("2023-05-14 18:32")
        );
    }

    #[test]
    fn format_capture_date_handles_missing_and_empty() {
        assert_eq!(format_capture_date(None), None);
        assert_eq!(format_capture_date(Some("   ")), None);
    }

    #[test]
    fn format_capture_date_passes_through_unexpected() {
        // Not the EXIF shape → first 16 chars, no colon swaps at non-date spots.
        assert_eq!(
            format_capture_date(Some("sometime")).as_deref(),
            Some("sometime")
        );
    }

    /// The image must be letterboxed inside its cell, never stretched to it.
    /// `paint_cell` cannot be unit-tested (it needs an egui `Ui`), so this pins
    /// the composition it performs: `fit_size` of `cell_aspect`.
    #[test]
    fn cell_image_is_letterboxed_to_its_own_aspect() {
        use crate::library::grid_layout::fit_size;
        // Portrait thumbnail (2:3) in a landscape 3:2 cell.
        let r = rec(
            Some(4000),
            Some(6000),
            Orientation::Normal,
            Some(200),
            Some(300),
        );
        let a = cell_aspect(&r);
        let (w, h) = fit_size(150.0, 100.0, a);
        assert!(
            w < 150.0,
            "a portrait must not fill a landscape cell's width"
        );
        assert!((h - 100.0).abs() < 0.01, "it should fill the height");
        assert!(
            (w / h - a).abs() < 1e-3,
            "the fitted rect must keep the image's aspect, else it is stretched"
        );
    }

    /// Guard against the O(all-items) text measurement returning. The deleted
    /// `label_width` called egui's no-wrap text layout on EVERY image on each
    /// layout rebuild (any panel resize or Size-slider change) — the class of
    /// work CLAUDE.md's virtualization rule forbids. Eliding to a known cell
    /// width replaces it.
    ///
    /// Each needle is ASSEMBLED AT RUNTIME from two fragments so it cannot match
    /// this test's own source (`include_str!` pulls in the test module too, so a
    /// plain literal would make the test red even on correct code). Same
    /// convention as `settings::dto`'s `disclosure_snapshot_covers_every_open_field`,
    /// which crafts its needle so it cannot self-match. `\r` is stripped so a
    /// CRLF checkout does not change the result.
    #[test]
    fn the_grid_never_measures_filenames_to_size_cells() {
        let src = include_str!("grid.rs").replace('\r', "");
        let measure_call = ["layout_no", "_wrap"].concat();
        assert!(
            !src.contains(&measure_call),
            "grid.rs measures text again — sizing cells from filename widths \
             reintroduces the O(all-items) work and the row collapse"
        );
        let label_cap = ["MAX_LABEL", "_W"].concat();
        assert!(
            !src.contains(&label_cap),
            "the label-width floor's cap is back; a filename-derived minimum \
             cell width is what made the row solver unsolvable"
        );
    }

    /// The justified solver must be GONE, not merely bypassed. Task 2's zeroed
    /// `min_widths` argument removed the symptom; leaving `solve_row_height` in
    /// the tree leaves the 0.4x clamp one call site away from returning.
    ///
    /// Needles are assembled at runtime so they cannot match this test's own
    /// source. They grep a different file (`grid_layout.rs`) today, so a plain
    /// literal would work — but assembling them means the test keeps meaning the
    /// same thing if it is ever moved into that file. Same convention as
    /// `settings::dto`'s `disclosure_snapshot_covers_every_open_field`.
    ///
    /// There is deliberately NO positive "grid.rs calls uniform_layout"
    /// assertion: greping this file for that name would match this test's own
    /// text and pass vacuously, and the compiler is the stronger check anyway —
    /// once the solver is deleted, `grid.rs` cannot build a layout without it.
    #[test]
    fn the_justified_row_solver_is_gone() {
        let layout_src = include_str!("grid_layout.rs").replace('\r', "");
        let gone = [
            ["solve_row", "_height"].concat(),
            ["fn row", "_width"].concat(),
            ["struct Row", "Item"].concat(),
        ];
        for needle in &gone {
            assert!(
                !layout_src.contains(needle),
                "grid_layout.rs still defines `{needle}` — the uniform grid \
                 replaced the justified solver, so it must not linger"
            );
        }
    }

    /// Spec D4: the Size slider maps to cell WIDTH by the documented V2 formula
    /// `118 + sizePct * 1.7` (docs/design/V2/README.md:42), replacing the old
    /// `thumb_size + 60`. Pinned because the old 60px floor is what let a
    /// filename dominate a cell.
    #[test]
    fn cell_width_follows_the_documented_v2_size_range() {
        assert!(
            (cell_width_for_size(0.0) - 118.0).abs() < 0.01,
            "slider min"
        );
        assert!(
            (cell_width_for_size(100.0) - 288.0).abs() < 0.01,
            "slider max"
        );
        // The persisted default (settings::dto grid_size = 46.0).
        assert!((cell_width_for_size(46.0) - 196.2).abs() < 0.01, "default");
    }

    #[test]
    fn cell_width_is_monotonic_and_clamps_out_of_range_input() {
        let mut prev = 0.0_f32;
        for pct in [0.0_f32, 10.0, 46.0, 90.0, 100.0] {
            let w = cell_width_for_size(pct);
            assert!(w > prev, "must grow with the slider");
            prev = w;
        }
        // A persisted setting outside 0..=100 must not produce a degenerate cell.
        assert!((cell_width_for_size(-50.0) - 118.0).abs() < 0.01);
        assert!((cell_width_for_size(999.0) - 288.0).abs() < 0.01);
    }
}

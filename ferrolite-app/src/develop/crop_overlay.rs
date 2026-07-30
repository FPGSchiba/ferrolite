//! Canvas crop overlay. Pure geometry in `crop_math`; this layer paints handles +
//! a rule-of-thirds grid and routes pointer events. Shown only while the Geometry
//! section is active (viewer.crop_active). Visual-tested.
//!
//! **Escape precedence (spec C5 "leaving crop feels safe"):** `Action::CloseViewer`
//! is bound to bare Escape (`settings/keymap.rs`) and is dispatched in
//! `app/shortcuts.rs`, which runs BEFORE this module's `show()` each frame (see
//! `app.rs`'s `update()`: `shortcuts::dispatch` at the top, canvas/tool
//! rendering in the `CentralPanel` below it). Left alone, a same-frame Escape
//! would let `CloseViewer` close the whole viewer (mid-drag) before this
//! module ever got a chance to just cancel the drag. `shortcuts::dispatch`
//! therefore checks [`drag_in_progress`] and skips `CloseViewer` while a crop
//! handle drag is live, so Escape's FIRST job — while dragging — is "cancel
//! this drag", and only once no drag is in progress does Escape fall through
//! to its normal "close the viewer" behavior.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::crop_math::{self, Handle};
use crate::develop::tools::crop::geometry_edit;
use crate::theme;
use ferrolite_pipeline::{CropRect, Geometry, OpStack};

const HANDLE_R: f32 = 0.03; // normalized hit radius
const HANDLE_DOT_R: f32 = 4.0; // idle handle-dot radius
const HANDLE_DOT_R_HOT: f32 = 6.0; // enlarged radius for the hovered/dragged handle

/// What the crop overlay wants done this frame: a geometry edit (handle
/// resize / body move / Escape cancel), or a canvas PAN — a drag that started
/// inside the image but OUTSIDE the crop rect and any handle. Pan/zoom must
/// keep working while the crop tool is active (QoL): the overlay owns the
/// pointer over the image, so it is the one that has to hand non-crop drags
/// back to the caller (`tools/crop.rs` applies them via `viewer::apply_pan`).
// Boxed: an `EditOutcome` carries a whole `OpStack` (~540 bytes) while `Pan`
// is 8 — clippy::large_enum_variant. One box per emitted edit (a handful per
// gesture), not per frame.
pub enum CropOverlayAction {
    Edit(Box<EditOutcome>),
    Pan(egui::Vec2),
}

/// Pure routing decision for a drag, given the press-position hit-test:
/// a handle resizes, the body moves the rect, anywhere else pans the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragRoute {
    Resize(Handle),
    MoveBody,
    Pan,
}

fn route_drag(hit: Option<Handle>) -> DragRoute {
    match hit {
        Some(Handle::Body) => DragRoute::MoveBody,
        Some(h) => DragRoute::Resize(h),
        None => DragRoute::Pan,
    }
}

/// The hover/drag cursor for each crop target (spec: hover affordance like
/// the Tone Curve's points — the cursor names the gesture).
fn cursor_for(h: Handle) -> egui::CursorIcon {
    match h {
        Handle::TopLeft | Handle::BottomRight => egui::CursorIcon::ResizeNwSe,
        Handle::TopRight | Handle::BottomLeft => egui::CursorIcon::ResizeNeSw,
        Handle::Top | Handle::Bottom => egui::CursorIcon::ResizeVertical,
        Handle::Left | Handle::Right => egui::CursorIcon::ResizeHorizontal,
        Handle::Body => egui::CursorIcon::Move,
    }
}

/// Fixed (NOT `ui.id()`-derived) egui data key for the in-progress handle
/// drag: which handle, and the crop rect exactly as it was BEFORE the drag
/// began (what Escape restores). Fixed so [`drag_in_progress`] can be
/// queried from `app/shortcuts.rs`, which has no `Ui` — only the
/// `egui::Context` — and runs before this module's `show()` this frame; see
/// the module doc comment for why that ordering matters.
fn drag_state_id() -> egui::Id {
    egui::Id::new("ferrolite_crop_overlay_drag")
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DragState {
    handle: Handle,
    /// The crop rect exactly as it was before this drag began.
    origin: CropRect,
}

/// Whether a crop handle drag is currently recorded as in progress. Read by
/// `app::shortcuts::dispatch` to give an in-progress drag precedence over
/// `Action::CloseViewer`'s Escape binding (module doc comment above).
pub fn drag_in_progress(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<DragState>(drag_state_id()))
        .is_some()
}

/// Build the Escape-cancel outcome: restore `origin` into `geo.crop`, written
/// through the same identity-normalizing [`geometry_edit`] every other Crop
/// control uses, so cancelling back to the identity crop leaves the doc with
/// NO geometry op (`EditDoc::is_identity()`), not a dangling
/// `Some(Geometry::default())` (Task 4 review finding — this overlay's emit
/// path had the same bug via a raw `stack.set_op` call; fixed here too).
///
/// `commit` is always `false`. That still needs justifying: EVERY drag frame
/// (not only the commit frame) already writes the dragged-to geometry into
/// the LIVE `viewer.op_stack` — see `apply_edit`/`set_preview_and_full` in
/// `app/controller.rs`, which assign `v.op_stack = stack` even when
/// `commit == false`. So simply emitting nothing on Escape would leave the
/// dragged-to (uncommitted) rect live in the stack. A non-committed restore
/// undoes that live mutation without pushing anything onto the undo history
/// or the persisted sidecar — exactly "no accidental commit".
fn cancel_drag_outcome(stack: &OpStack, geo: Geometry, origin: CropRect) -> EditOutcome {
    let restored = Geometry {
        crop: origin,
        ..geo
    };
    geometry_edit(stack, restored, false)
}

/// Pure decision: given whether Escape was pressed this frame and the
/// recorded mid-drag state (if any), should the drag be cancelled — and with
/// what outcome? Deliberately free of `egui::Ui`/`Response` so it is
/// unit-testable without spinning up a live egui frame.
fn cancel_if_escaped(
    escape_pressed: bool,
    drag: Option<DragState>,
    stack: &OpStack,
    geo: Geometry,
) -> Option<EditOutcome> {
    if !escape_pressed {
        return None;
    }
    let ds = drag?;
    Some(cancel_drag_outcome(stack, geo, ds.origin))
}

pub fn show(
    ui: &mut egui::Ui,
    image_rect: egui::Rect,
    stack: &OpStack,
    aspect_dims: (u32, u32),
) -> Option<CropOverlayAction> {
    let geo = stack.geometry().unwrap_or_default();
    let crop = geo.crop;
    let to_screen = |nx: f32, ny: f32| {
        egui::pos2(
            image_rect.left() + nx * image_rect.width(),
            image_rect.top() + ny * image_rect.height(),
        )
    };
    let to_norm = |p: egui::Pos2| {
        (
            ((p.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0),
            ((p.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0),
        )
    };

    // Interact BEFORE painting so this frame's hover/drag state can drive the
    // hover affordance (hovered handle grows, cursor names the gesture) —
    // mirrors `widgets/curve.rs`, where the hovered point enlarges.
    let resp = ui.interact(
        image_rect,
        ui.id().with("crop_overlay"),
        egui::Sense::click_and_drag(),
    );

    // The handle (or body) the pointer is over, plus the one an in-progress
    // drag is holding — the "hot" target that gets the enlarged dot + cursor.
    let hover_hit = resp
        .hover_pos()
        .and_then(|p| crop_math::hit_test(crop, to_norm(p), HANDLE_R));
    let drag_hit: Option<Handle> = ui
        .data(|d| d.get_temp::<DragState>(drag_state_id()))
        .map(|ds| ds.handle);
    let hot = drag_hit.or(hover_hit);
    if let Some(h) = hot {
        ui.ctx().set_cursor_icon(cursor_for(h));
    }

    // Crop rect + rule-of-thirds. Body hover/drag brightens + thickens the
    // rect edges (the body's equivalent of a handle growing).
    let r = egui::Rect::from_min_max(
        to_screen(crop.x, crop.y),
        to_screen(crop.x + crop.w, crop.y + crop.h),
    );
    let body_hot = hot == Some(Handle::Body);
    let rect_stroke = if body_hot {
        egui::Stroke::new(2.0_f32, theme::ACCENT_TEXT)
    } else {
        egui::Stroke::new(1.5_f32, theme::ACCENT_BRIGHT)
    };
    let painter = ui.painter();
    painter.rect_stroke(r, 0.0, rect_stroke);
    for i in 1..3 {
        let f = i as f32 / 3.0;
        painter.line_segment(
            [
                egui::pos2(r.left() + f * r.width(), r.top()),
                egui::pos2(r.left() + f * r.width(), r.bottom()),
            ],
            egui::Stroke::new(1.0_f32, theme::ACCENT),
        );
        painter.line_segment(
            [
                egui::pos2(r.left(), r.top() + f * r.height()),
                egui::pos2(r.right(), r.top() + f * r.height()),
            ],
            egui::Stroke::new(1.0_f32, theme::ACCENT),
        );
    }
    // All 8 handles (spec §8.4): 4 corners + 4 edge midpoints. `hit_test`
    // recognizes all 8, so paint all 8 so they're discoverable, not just
    // corners. The hovered/dragged handle renders enlarged + brightened
    // (Tone Curve parity — see `DOT_R`/`DOT_R_HOVER` in `widgets/curve.rs`).
    for (handle, nx, ny) in [
        // Corners.
        (Handle::TopLeft, crop.x, crop.y),
        (Handle::TopRight, crop.x + crop.w, crop.y),
        (Handle::BottomLeft, crop.x, crop.y + crop.h),
        (Handle::BottomRight, crop.x + crop.w, crop.y + crop.h),
        // Edge midpoints.
        (Handle::Top, crop.x + crop.w * 0.5, crop.y),
        (Handle::Right, crop.x + crop.w, crop.y + crop.h * 0.5),
        (Handle::Bottom, crop.x + crop.w * 0.5, crop.y + crop.h),
        (Handle::Left, crop.x, crop.y + crop.h * 0.5),
    ] {
        let is_hot = hot == Some(handle);
        painter.circle(
            to_screen(nx, ny),
            if is_hot {
                HANDLE_DOT_R_HOT
            } else {
                HANDLE_DOT_R
            },
            if is_hot {
                theme::ACCENT_TEXT
            } else {
                theme::ACCENT_BRIGHT
            },
            egui::Stroke::new(1.0_f32, theme::BG_BASE),
        );
    }

    // Reconcile stale drag state: if egui itself no longer considers this
    // widget's id to be mid-drag (e.g. the user switched tools via keybind
    // while still holding the mouse down, so this `interact()` call went
    // unpolled for a while and egui's own drag tracking for this id already
    // ended), drop our record so a later Escape can't "cancel" a drag that
    // isn't actually happening. `drag_stopped()` is deliberately included in
    // the "still relevant" set alongside `dragged()`/`drag_started()`: egui
    // reports `dragged() == false` on the very frame `drag_stopped() ==
    // true` (release forces `dragged` false — see `Context::get_response`),
    // so excluding it here would wipe the drag's recorded handle/origin
    // before the release-frame commit below gets to read it.
    if !resp.dragged() && !resp.drag_started() && !resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<DragState>(drag_state_id()));
    }

    // Escape cancels an in-progress handle drag (spec C5): restore the
    // pre-drag rect and swallow this frame's drag processing below so it
    // can't immediately re-apply the still-held pointer's delta. See the
    // module doc comment for why `app/shortcuts.rs` must not also treat this
    // Escape as `Action::CloseViewer`.
    let escape_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let stored_drag: Option<DragState> = ui.data(|d| d.get_temp(drag_state_id()));
    if let Some(outcome) = cancel_if_escaped(escape_pressed, stored_drag, stack, geo) {
        ui.data_mut(|d| d.remove::<DragState>(drag_state_id()));
        return Some(CropOverlayAction::Edit(Box::new(outcome)));
    }

    // NORMALIZED-space ratio, NOT `aspect_ratio`'s image-space one: `resize`
    // works in [0,1]² where a pixel ratio must be scaled by the source's own
    // shape (3:2 on a 6000×4000 source is 1.0 here, not 1.5). Passing the
    // image-space value straight in — as this line did from the first overlay
    // commit (a563e7f) until now — made every aspect-locked drag hold the
    // wrong ratio on non-square sources, which read as "aspect not held".
    let aspect = crop_math::normalized_aspect(geo.aspect, aspect_dims.0, aspect_dims.1);
    let mut new_crop = crop;
    let mut changed = false;
    let mut pan: Option<egui::Vec2> = None;
    if resp.drag_started() {
        // Hit-test at the PRESS ORIGIN, not the current pointer position.
        // `drag_started()` only fires once egui considers the press
        // "decidedly dragging" — i.e. the pointer has already travelled past
        // the ~6px click threshold (or dwelled) — so by this frame
        // `interact_pointer_pos()` can be well past the handle (a quick flick
        // easily clears a 0.03-normalized hit radius) or even OUTSIDE the
        // rect body the user grabbed. Hit-testing the moved position instead
        // of the pressed one silently dropped those drags — the "can't move
        // the crop" regression (Task-7 rework kept the old position choice
        // while making a missed hit terminal for the whole drag).
        let press = ui
            .input(|i| i.pointer.press_origin())
            .or_else(|| resp.interact_pointer_pos());
        if let Some(p) = press {
            let hit = crop_math::hit_test(crop, to_norm(p), HANDLE_R);
            ui.data_mut(|d| match hit {
                Some(handle) => d.insert_temp(
                    drag_state_id(),
                    DragState {
                        handle,
                        origin: crop,
                    },
                ),
                // No handle and not the body: this drag PANS the canvas (the
                // routing below reads the absent DragState as `DragRoute::Pan`).
                None => d.remove::<DragState>(drag_state_id()),
            });
        }
    }
    // `|| resp.drag_stopped()`: without it the release frame never commits.
    // egui forces `dragged() == false` on the exact frame `drag_stopped() ==
    // true` (the same mechanism noted above), so gating this block on
    // `dragged()` alone means `changed` is never `true` when `drag_stopped()`
    // also is — the two are mutually exclusive per frame for one widget id —
    // and the `if changed { ...commit: resp.drag_stopped()... }` below could
    // then never actually fire with `commit == true`. `interact_pointer_pos()`
    // is documented to stay valid through the release frame for exactly this
    // reason (mirrors the equivalent `mask_overlay.rs` drag-handle pattern).
    if resp.dragged() || resp.drag_stopped() {
        let active: Option<Handle> = ui
            .data(|d| d.get_temp::<DragState>(drag_state_id()))
            .map(|ds| ds.handle);
        if let Some(p) = resp.interact_pointer_pos() {
            let norm = to_norm(p);
            match route_drag(active) {
                DragRoute::MoveBody => {
                    let d = (
                        resp.drag_delta().x / image_rect.width(),
                        resp.drag_delta().y / image_rect.height(),
                    );
                    new_crop = crop_math::move_body(crop, d);
                    changed = true;
                }
                DragRoute::Resize(handle) => {
                    new_crop = crop_math::resize(crop, handle, norm, aspect);
                    changed = true;
                }
                // A drag that started outside the crop rect pans the canvas
                // (QoL: pan keeps working in crop mode). Only per-frame
                // deltas while actively dragging — the release frame's delta
                // is zero and pan has no commit semantics.
                DragRoute::Pan => {
                    let d = resp.drag_delta();
                    if resp.dragged() && d != egui::Vec2::ZERO {
                        pan = Some(d);
                    }
                }
            }
        }
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<DragState>(drag_state_id()));
    }
    if changed {
        let new_geo = Geometry {
            crop: new_crop,
            angle_deg: geo.angle_deg,
            aspect: geo.aspect,
            keystone_v: geo.keystone_v,
            keystone_h: geo.keystone_h,
        };
        return Some(CropOverlayAction::Edit(Box::new(geometry_edit(
            stack,
            new_geo,
            resp.drag_stopped(),
        ))));
    }
    pan.map(CropOverlayAction::Pan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::Op;

    /// Drive `show()` through a synthetic pointer drag, one egui frame per
    /// event, feeding each frame's emitted stack back in (exactly what
    /// `apply_edit`/`set_preview_and_full` do with the live `viewer.op_stack`).
    /// Returns the final stack after the release frame plus the accumulated
    /// pan delta the overlay reported.
    fn drive_pointer_drag(
        start: egui::Pos2,
        moves: &[egui::Pos2],
        mut stack: OpStack,
    ) -> (OpStack, egui::Vec2) {
        let ctx = egui::Context::default();
        let image_rect =
            egui::Rect::from_min_max(egui::pos2(100.0_f32, 100.0_f32), egui::pos2(500.0, 500.0));
        let mut pan_total = egui::Vec2::ZERO;
        let frame = |events: Vec<egui::Event>, stack: &OpStack| -> Option<CropOverlayAction> {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_max(
                    egui::pos2(0.0_f32, 0.0_f32),
                    egui::pos2(800.0, 600.0),
                )),
                ..Default::default()
            };
            input.events = events;
            let mut out = None;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    out = show(ui, image_rect, stack, (1000, 1000));
                });
            });
            out
        };
        let apply =
            |action: Option<CropOverlayAction>, stack: &mut OpStack, pan_total: &mut egui::Vec2| {
                match action {
                    Some(CropOverlayAction::Edit(o)) => *stack = o.stack,
                    Some(CropOverlayAction::Pan(d)) => *pan_total += d,
                    None => {}
                }
            };
        // Hover, then press.
        for ev in [
            egui::Event::PointerMoved(start),
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
        ] {
            let out = frame(vec![ev], &stack);
            apply(out, &mut stack, &mut pan_total);
        }
        // Drag moves (each far past egui's 6px decidedly-dragging threshold).
        for &p in moves {
            let out = frame(vec![egui::Event::PointerMoved(p)], &stack);
            apply(out, &mut stack, &mut pan_total);
        }
        // Release.
        let last = *moves.last().unwrap_or(&start);
        let out = frame(
            vec![egui::Event::PointerButton {
                pos: last,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            }],
            &stack,
        );
        apply(out, &mut stack, &mut pan_total);
        (stack, pan_total)
    }

    /// EMPIRICAL repro for "cannot move the crop overlay": drag the BODY of a
    /// centered half-size crop 40px right+down (image_rect is 400px wide, so
    /// +0.1 normalized) and expect the rect to move.
    #[test]
    fn body_drag_moves_the_crop_rect() {
        let start_crop = CropRect {
            x: 0.25,
            y: 0.25,
            w: 0.5,
            h: 0.5,
        };
        let stack = stack_with_crop(start_crop);
        // Body center: (100 + 0.5*400, 100 + 0.5*400) = (300, 300).
        let (end, pan) = drive_pointer_drag(
            egui::pos2(300.0_f32, 300.0),
            &[egui::pos2(320.0_f32, 320.0), egui::pos2(340.0_f32, 340.0)],
            stack,
        );
        let crop = end.geometry().expect("geometry op present").crop;
        assert!(
            (crop.x - 0.35).abs() < 0.02 && (crop.y - 0.35).abs() < 0.02,
            "body drag must move the rect by ~+0.1, got {crop:?}"
        );
        assert_eq!(pan, egui::Vec2::ZERO, "a body move never pans");
    }

    /// The press-origin regression: the handle grab is decided at the PRESS
    /// position. By the time egui reports `drag_started()` the pointer has
    /// already moved past the ~6px decidedly-dragging threshold — here it is
    /// 20px past the handle, well outside the 0.03-normalized hit radius —
    /// so hit-testing the current position (the pre-fix behavior) dropped
    /// the whole drag.
    #[test]
    fn handle_drag_resizes_the_crop_rect_even_when_the_pointer_outruns_the_handle() {
        let start_crop = CropRect {
            x: 0.25,
            y: 0.25,
            w: 0.5,
            h: 0.5,
        };
        let stack = stack_with_crop(start_crop);
        // BottomRight handle: (100 + 0.75*400, 100 + 0.75*400) = (400, 400).
        let (end, pan) = drive_pointer_drag(
            egui::pos2(400.0_f32, 400.0),
            &[egui::pos2(420.0_f32, 420.0), egui::pos2(440.0_f32, 440.0)],
            stack,
        );
        let crop = end.geometry().expect("geometry op present").crop;
        assert!(
            (crop.w - 0.6).abs() < 0.02 && (crop.h - 0.6).abs() < 0.02,
            "handle drag must grow the rect to ~0.6, got {crop:?}"
        );
        assert_eq!(pan, egui::Vec2::ZERO, "a handle resize never pans");
    }

    /// QoL: a drag that STARTS outside the crop rect (but on the image) pans
    /// the canvas instead of editing the crop.
    #[test]
    fn drag_outside_the_crop_rect_pans_and_leaves_the_crop_alone() {
        let start_crop = CropRect {
            x: 0.4,
            y: 0.4,
            w: 0.3,
            h: 0.3,
        };
        let stack = stack_with_crop(start_crop);
        // (140, 140) is norm (0.1, 0.1): inside the image, far outside the
        // crop rect and any handle.
        let (end, pan) = drive_pointer_drag(
            egui::pos2(140.0_f32, 140.0),
            &[egui::pos2(170.0_f32, 160.0), egui::pos2(200.0_f32, 180.0)],
            stack,
        );
        let crop = end.geometry().expect("geometry op present").crop;
        assert_eq!(
            (crop.x, crop.y, crop.w, crop.h),
            (0.4, 0.4, 0.3, 0.3),
            "an outside drag must not touch the crop"
        );
        assert!(
            (pan.x - 60.0).abs() < 1e-3 && (pan.y - 40.0).abs() < 1e-3,
            "the full drag delta pans the canvas, got {pan:?}"
        );
    }

    // ── Pure routing decision (spec: hit-test routing tests) ───────────────

    #[test]
    fn route_drag_sends_body_to_move_handles_to_resize_and_misses_to_pan() {
        assert_eq!(route_drag(Some(Handle::Body)), DragRoute::MoveBody);
        for h in [
            Handle::TopLeft,
            Handle::Top,
            Handle::TopRight,
            Handle::Right,
            Handle::BottomRight,
            Handle::Bottom,
            Handle::BottomLeft,
            Handle::Left,
        ] {
            assert_eq!(route_drag(Some(h)), DragRoute::Resize(h));
        }
        assert_eq!(route_drag(None), DragRoute::Pan);
    }

    #[test]
    fn cursor_for_names_the_gesture_per_handle() {
        assert_eq!(cursor_for(Handle::Body), egui::CursorIcon::Move);
        assert_eq!(
            cursor_for(Handle::TopLeft),
            egui::CursorIcon::ResizeNwSe,
            "diagonal corners"
        );
        assert_eq!(cursor_for(Handle::BottomLeft), egui::CursorIcon::ResizeNeSw);
        assert_eq!(cursor_for(Handle::Top), egui::CursorIcon::ResizeVertical);
        assert_eq!(
            cursor_for(Handle::Right),
            egui::CursorIcon::ResizeHorizontal
        );
    }

    fn stack_with_crop(crop: CropRect) -> OpStack {
        OpStack::default().set_op(Op::Geometry(Geometry {
            crop,
            ..Geometry::default()
        }))
    }

    fn moved_crop() -> CropRect {
        CropRect {
            x: 0.3,
            y: 0.3,
            w: 0.4,
            h: 0.4,
        }
    }

    #[test]
    fn cancel_if_escaped_is_none_when_escape_not_pressed() {
        let stack = stack_with_crop(moved_crop());
        let geo = stack.geometry().unwrap();
        let drag = Some(DragState {
            handle: Handle::BottomRight,
            origin: CropRect::full(),
        });
        assert!(cancel_if_escaped(false, drag, &stack, geo).is_none());
    }

    #[test]
    fn cancel_if_escaped_is_none_without_a_recorded_drag() {
        // Escape pressed but no drag was ever recorded (e.g. a stray Escape
        // while just hovering the canvas, or one already reconciled as
        // stale) — nothing to cancel.
        let stack = stack_with_crop(moved_crop());
        let geo = stack.geometry().unwrap();
        assert!(cancel_if_escaped(true, None, &stack, geo).is_none());
    }

    /// Step 1 (failing-test-first): mid-drag state + Escape restores the
    /// exact pre-drag rect as a non-committed outcome. `commit` must be
    /// `false` — see `cancel_drag_outcome`'s doc comment for why a restore
    /// still has to be a real (uncommitted) `EditOutcome` rather than "emit
    /// nothing": every drag frame already mutates the LIVE `viewer.op_stack`
    /// (mid-drag or not), so undoing that live mutation requires writing the
    /// restored geometry back, just without committing it to undo
    /// history/disk.
    #[test]
    fn escape_mid_drag_restores_the_pre_drag_rect_uncommitted() {
        let origin = CropRect {
            x: 0.1,
            y: 0.1,
            w: 0.5,
            h: 0.5,
        };
        let dragged_to = moved_crop();
        let stack = stack_with_crop(dragged_to); // as if a drag already moved it here
        let geo = stack.geometry().unwrap();
        let drag = Some(DragState {
            handle: Handle::BottomRight,
            origin,
        });

        let outcome = cancel_if_escaped(true, drag, &stack, geo).expect("cancels the drag");
        assert!(!outcome.commit, "cancel must never commit");
        assert_eq!(
            outcome.stack.geometry().unwrap().crop,
            origin,
            "restores the exact pre-drag rect, not the dragged-to one"
        );
    }

    /// Coordinator-carried finding (Task 4 review): a drag that lands (or, as
    /// here, is cancelled) back to the exact identity geometry must leave the
    /// doc with NO geometry op at all, not `Some(Geometry::default())` —
    /// otherwise `EditDoc::is_identity()` desyncs from what's actually on
    /// disk/in the doc. Escape-cancel goes through `geometry_edit`, the same
    /// identity-normalizing path `CropTab` uses, so this holds here too.
    #[test]
    fn escape_cancel_to_identity_leaves_no_geometry_op() {
        let stack = stack_with_crop(moved_crop());
        let geo = stack.geometry().unwrap();
        let drag = Some(DragState {
            handle: Handle::BottomRight,
            origin: CropRect::full(), // identity crop
        });

        let outcome = cancel_if_escaped(true, drag, &stack, geo).expect("cancels the drag");
        assert!(!outcome.commit);
        assert!(
            outcome.stack.geometry().is_none(),
            "identity restore normalizes to no geometry op, not Some(default)"
        );
        assert!(outcome.stack.is_identity());
    }

    /// Step 2 regression (spec C5): exiting the Crop tool commits nothing by
    /// itself. `crop_overlay::show` (this module) is the ONLY place a crop
    /// `EditOutcome` can be produced — `develop/canvas/viewer.rs`'s dispatch
    /// calls `tool.canvas()` only for the currently ACTIVE tool
    /// (`if let Some(id) = active_tool { ... tool.canvas(ui, ...) }`), so the
    /// instant `ToolState::select_tool` moves `active` away from
    /// `ToolId::Crop` (see `develop/tool_state.rs`'s own regression test for
    /// that half of the guarantee), this function is simply never called
    /// again — mid-drag or not. There is no separate "on tool exit, flush
    /// the pending edit" path to accidentally commit through. This test
    /// pins that down concretely: a recorded mid-drag `DragState` sitting in
    /// egui data is inert on its own — nothing except a LATER call to
    /// `show()` (via `cancel_if_escaped` or the `dragged()`/`drag_stopped()`
    /// branches) can ever turn it into an `EditOutcome`.
    #[test]
    fn a_recorded_drag_produces_no_outcome_without_show_running_again() {
        let stack = stack_with_crop(moved_crop());
        let geo = stack.geometry().unwrap();
        let drag = Some(DragState {
            handle: Handle::BottomRight,
            origin: CropRect::full(),
        });
        // Escape NOT pressed this "frame" (the tool-switch keybind, not
        // Escape) — the only other input `show()` reacts to is drag
        // continuation/release, neither of which happens if `show()` isn't
        // called at all. `cancel_if_escaped` models exactly the reachable
        // "is there an outcome" surface without needing a live `egui::Ui`.
        assert!(cancel_if_escaped(false, drag, &stack, geo).is_none());
    }
}

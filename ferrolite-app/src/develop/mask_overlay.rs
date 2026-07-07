//! Canvas mask overlay: paints the composited coverage as a red tint over the
//! displayed image, then routes tool affordances (Tasks 10–12). Pure math lives
//! in `mask_affordance`; this layer only paints + routes pointer events (same
//! discipline as `crop_overlay`). Visual-tested.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::mask_affordance::{self, BrushParams, LinHandle, RadHandle};
use crate::develop::mask_edit;
use crate::develop::mask_ui::{MaskGesture, MaskTool, MaskUiState};
use crate::theme;
use ferrolite_image::LinearRgbaF32;
use ferrolite_mask::{MaskComponent, Stroke, Vec2};
use ferrolite_pipeline::{display_to_source, source_to_display, Geometry, OpKind, OpStack};
use std::sync::Arc;

const HANDLE_R: f32 = 0.04; // normalized (source-space) hit radius, matches brief tests

/// New brush radius from a scroll delta, applied MULTIPLICATIVELY so each scroll
/// tick is a constant percentage change — fine-grained at small radii, coarser at
/// large radii (smooth size ramp). Clamped to [min, max]. `scroll_y == 0` is a no-op.
/// `scroll_y > 0` (wheel up) grows the brush; negative shrinks it. Callers pass the same
/// bounds as the panel's brush-radius slider (`mask_panel::BRUSH_RADIUS_MIN/MAX`)
/// so the Ctrl+scroll canvas gesture and the slider agree on range. Pure/egui-free
/// so it's unit-testable without a UI context.
///
/// Only called from `app.rs`'s Ctrl+scroll dispatch, which — like the rest of
/// the `app`/`chrome`/`canvas` module tree — is compiled into the `ferrolite-app`
/// *binary* target only, not the library target (`lib.rs` intentionally omits
/// `app`; see its module comment). The `--lib` build/test therefore has no
/// caller for this function even though the real app does; hence the explicit
/// allow rather than a false "dead code" failure under `-D warnings`.
#[allow(dead_code)]
pub(crate) fn brush_radius_from_scroll(current: f32, scroll_y: f32, min: f32, max: f32) -> f32 {
    // Per-"tick" growth factor; egui scroll deltas are ~pixels, so scale down.
    const PER_UNIT: f32 = 0.0012; // ln-space rate; tuned in the visual test
    (current * (scroll_y * PER_UNIT).exp()).clamp(min, max)
}

/// Tag bits packed into `MaskGesture::DragHandle.handle` so a single `u32` field
/// can carry either a `LinHandle` or a `RadHandle`. Decoding uses `mask.tool` to
/// know which enum applies (a mask's active gesture is always for its current
/// tool), so the raw discriminant (0/1/2) is stored directly with no extra tag —
/// simplest choice consistent with the existing `DragHandle { handle: u32 }` shape.
fn lin_handle_to_u32(h: LinHandle) -> u32 {
    match h {
        LinHandle::Start => 0,
        LinHandle::End => 1,
        LinHandle::Body => 2,
    }
}

fn u32_to_lin_handle(v: u32) -> LinHandle {
    match v {
        0 => LinHandle::Start,
        1 => LinHandle::End,
        _ => LinHandle::Body,
    }
}

fn rad_handle_to_u32(h: RadHandle) -> u32 {
    match h {
        RadHandle::Center => 0,
        RadHandle::RadiusX => 1,
        RadHandle::RadiusY => 2,
        RadHandle::Both => 3,
    }
}

fn u32_to_rad_handle(v: u32) -> RadHandle {
    match v {
        0 => RadHandle::Center,
        1 => RadHandle::RadiusX,
        3 => RadHandle::Both,
        _ => RadHandle::RadiusY,
    }
}

/// Paint the coverage fill (if a texture is ready + overlay is on) and route tool
/// affordances. `overlay_tex` is the app-global native texture id for the
/// GPU-tinted overlay (None until first built / when no mask is selected).
/// `src_dims` is the source image's (w, h), needed for the display↔source
/// coordinate mapping.
pub fn show(
    ui: &mut egui::Ui,
    image_rect: egui::Rect,
    stack: &OpStack,
    mask: &mut MaskUiState,
    overlay_tex: Option<egui::TextureId>,
    src_dims: (u32, u32),
    preview_source: Option<&Arc<LinearRgbaF32>>,
) -> Option<EditOutcome> {
    // Fill: stretch the coverage texture over the image rect with alpha blend.
    // Suppressed while `adjusting` (a Light+Color slider of this mask is being
    // dragged) so the user sees the actual effect instead of the red tint.
    if mask.overlay_on && !mask.adjusting {
        if let Some(tex_id) = overlay_tex {
            ui.painter().image(
                tex_id,
                image_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }

    // Color eyedropper is armed-mode and stages samples in `mask.color_samples`
    // (UI state only — not tied to a selected mask until "Add Color range" is
    // clicked in the panel), so route it whenever the Mask tool is active,
    // BEFORE the selection gate below that brush/linear/radial need. Without
    // this, arming "Pick color" with no mask selected made the eyedropper's
    // `ui.interact`, cursor, loupe, and sampling a silent no-op — the click
    // fell through to `drive_viewer`'s canvas interact instead.
    if mask.active && mask.tool == MaskTool::ColorRange && mask.picking_color {
        route_color_eyedropper(ui, image_rect, mask, src_dims, stack, preview_source);
        return None;
    }

    let (Some(idx), true) = (mask.selected, mask.active) else {
        return None;
    };
    let tool = mask.tool;
    if tool == MaskTool::Brush {
        return route_brush(ui, image_rect, stack, mask, idx, src_dims);
    }
    if tool == MaskTool::ColorRange {
        route_color_eyedropper(ui, image_rect, mask, src_dims, stack, preview_source);
        return None;
    }
    if tool != MaskTool::Linear && tool != MaskTool::Radial {
        return None;
    }

    let geo = stack.geometry();
    let (src_w, src_h) = src_dims;
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
    let screen_to_src = |p: egui::Pos2| display_to_source(geo, src_w, src_h, to_norm(p));
    let src_to_screen =
        |p: (f32, f32)| -> egui::Pos2 { to_screen_from_src(geo, src_w, src_h, p, &to_screen) };

    // Find the first component matching the active tool in the selected mask.
    let la = mask_edit::layers(stack);
    let existing = la.layers.get(idx).and_then(|l| {
        l.mask
            .components
            .iter()
            .enumerate()
            .find_map(|(i, (c, _))| match (tool, c) {
                (MaskTool::Linear, MaskComponent::LinearGradient { start, end }) => {
                    Some((i, (start.x, start.y), (end.x, end.y)))
                }
                _ => None,
            })
    });
    let existing_radial = la.layers.get(idx).and_then(|l| {
        l.mask
            .components
            .iter()
            .enumerate()
            .find_map(|(i, (c, _))| match (tool, c) {
                (MaskTool::Radial, MaskComponent::RadialGradient { center, radius, .. }) => {
                    Some((i, (center.x, center.y), (radius.x, radius.y)))
                }
                _ => None,
            })
    });

    let resp = ui.interact(
        image_rect,
        ui.id().with("mask_overlay_affordance"),
        egui::Sense::click_and_drag(),
    );

    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            let src = screen_to_src(p);
            match tool {
                MaskTool::Linear => {
                    if let Some((comp_idx, start, end)) = existing {
                        let handle = mask_affordance::linear_hit_test(start, end, src, HANDLE_R);
                        if let Some(h) = handle {
                            mask.gesture = Some(MaskGesture::DragHandle {
                                component: comp_idx,
                                handle: lin_handle_to_u32(h),
                                origin_src: src,
                            });
                        }
                    } else {
                        mask.gesture = Some(MaskGesture::DragHandle {
                            component: usize::MAX, // sentinel: created on first drag update
                            handle: lin_handle_to_u32(LinHandle::End),
                            origin_src: src,
                        });
                    }
                }
                MaskTool::Radial => {
                    if let Some((comp_idx, center, radius)) = existing_radial {
                        let handle =
                            mask_affordance::radial_hit_test(center, radius, 0.0, src, HANDLE_R);
                        if let Some(h) = handle {
                            mask.gesture = Some(MaskGesture::DragHandle {
                                component: comp_idx,
                                handle: rad_handle_to_u32(h),
                                origin_src: src,
                            });
                        }
                    } else {
                        mask.gesture = Some(MaskGesture::DragHandle {
                            component: usize::MAX,
                            handle: rad_handle_to_u32(RadHandle::Both),
                            origin_src: src,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    let mut outcome: Option<EditOutcome> = None;
    if resp.dragged() || resp.drag_stopped() {
        if let (
            Some(MaskGesture::DragHandle {
                component,
                handle,
                origin_src,
            }),
            Some(p),
        ) = (&mask.gesture, resp.interact_pointer_pos())
        {
            let src = screen_to_src(p);
            let component = *component;
            let handle = *handle;
            let origin_src = *origin_src;
            let new_stack = if component == usize::MAX {
                // Drag-to-create: origin_src is the pointer-down point.
                match tool {
                    MaskTool::Linear => {
                        let comp = MaskComponent::LinearGradient {
                            start: Vec2::new(origin_src.0, origin_src.1),
                            end: Vec2::new(src.0, src.1),
                        };
                        let added = mask_edit::add_component(stack, idx, comp, mask.next_mode);
                        let new_idx =
                            mask_edit::layers(&added).layers[idx].mask.components.len() - 1;
                        mask.gesture = Some(MaskGesture::DragHandle {
                            component: new_idx,
                            handle: lin_handle_to_u32(LinHandle::End),
                            origin_src,
                        });
                        Some(added)
                    }
                    MaskTool::Radial => {
                        let comp = MaskComponent::RadialGradient {
                            center: Vec2::new(origin_src.0, origin_src.1),
                            radius: Vec2::new(1e-3, 1e-3),
                            rotation: 0.0,
                            feather: 0.3,
                            invert: false,
                        };
                        let added = mask_edit::add_component(stack, idx, comp, mask.next_mode);
                        let new_idx =
                            mask_edit::layers(&added).layers[idx].mask.components.len() - 1;
                        mask.gesture = Some(MaskGesture::DragHandle {
                            component: new_idx,
                            handle: rad_handle_to_u32(RadHandle::Both),
                            origin_src,
                        });
                        Some(added)
                    }
                    _ => None,
                }
            } else {
                match tool {
                    MaskTool::Linear => existing.and_then(|(comp_idx, start, end)| {
                        if comp_idx != component {
                            return None;
                        }
                        let h = u32_to_lin_handle(handle);
                        let (ns, ne) = if h == LinHandle::Body {
                            // Body drag translates by a source-space delta: re-derive
                            // it from consecutive source-space pointer positions
                            // (screen-space `drag_delta()` isn't valid under
                            // rotation/crop, since the mapping isn't a pure scale).
                            let d = (src.0 - origin_src.0, src.1 - origin_src.1);
                            mask_affordance::linear_drag_body(start, end, d)
                        } else {
                            mask_affordance::linear_drag(start, end, h, src)
                        };
                        // Body drag is delta-based, so advance the drag origin to
                        // the current point for the next frame's delta.
                        if h == LinHandle::Body {
                            mask.gesture = Some(MaskGesture::DragHandle {
                                component: comp_idx,
                                handle,
                                origin_src: src,
                            });
                        }
                        Some(mask_edit::set_component(
                            stack,
                            idx,
                            comp_idx,
                            MaskComponent::LinearGradient {
                                start: Vec2::new(ns.0, ns.1),
                                end: Vec2::new(ne.0, ne.1),
                            },
                        ))
                    }),
                    MaskTool::Radial => existing_radial.and_then(|(comp_idx, center, radius)| {
                        if comp_idx != component {
                            return None;
                        }
                        let h = u32_to_rad_handle(handle);
                        let (nc, nr) = mask_affordance::radial_drag(center, radius, 0.0, h, src);
                        let feather = la.layers[idx]
                            .mask
                            .components
                            .get(comp_idx)
                            .and_then(|(c, _)| match c {
                                MaskComponent::RadialGradient { feather, .. } => Some(*feather),
                                _ => None,
                            })
                            .unwrap_or(0.3);
                        Some(mask_edit::set_component(
                            stack,
                            idx,
                            comp_idx,
                            MaskComponent::RadialGradient {
                                center: Vec2::new(nc.0, nc.1),
                                radius: Vec2::new(nr.0, nr.1),
                                rotation: 0.0,
                                feather,
                                invert: false,
                            },
                        ))
                    }),
                    _ => None,
                }
            };
            if let Some(new_stack) = new_stack {
                outcome = Some(EditOutcome {
                    stack: new_stack,
                    kind: OpKind::LocalAdjustments,
                    commit: resp.drag_stopped(),
                });
            }
        }
    }
    if resp.drag_stopped() {
        mask.gesture = None;
    }

    // Paint handles for the currently-existing component (post-drag state is
    // reflected via `stack` on the next frame once the outcome is applied).
    let painter = ui.painter();
    match tool {
        MaskTool::Linear => {
            if let Some((_, start, end)) = existing {
                let p0 = src_to_screen(start);
                let p1 = src_to_screen(end);
                painter.line_segment([p0, p1], egui::Stroke::new(1.5, theme::ACCENT_BRIGHT));
                for p in [p0, p1] {
                    painter.circle(
                        p,
                        4.0,
                        theme::ACCENT_BRIGHT,
                        egui::Stroke::new(1.0, theme::BG_BASE),
                    );
                }
            }
        }
        MaskTool::Radial => {
            if let Some((_, center, radius)) = existing_radial {
                const SEGMENTS: usize = 48;
                let pts: Vec<egui::Pos2> = (0..=SEGMENTS)
                    .map(|i| {
                        let t = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
                        let sx = center.0 + radius.0 * t.cos();
                        let sy = center.1 + radius.1 * t.sin();
                        src_to_screen((sx, sy))
                    })
                    .collect();
                painter.add(egui::Shape::line(
                    pts,
                    egui::Stroke::new(1.5, theme::ACCENT_BRIGHT),
                ));
                painter.circle(
                    src_to_screen(center),
                    4.0,
                    theme::ACCENT_BRIGHT,
                    egui::Stroke::new(1.0, theme::BG_BASE),
                );
            }
        }
        _ => {}
    }

    outcome
}

fn to_screen_from_src(
    geo: Option<Geometry>,
    src_w: u32,
    src_h: u32,
    src: (f32, f32),
    to_screen: &impl Fn(f32, f32) -> egui::Pos2,
) -> egui::Pos2 {
    let disp = source_to_display(geo, src_w, src_h, src);
    to_screen(disp.0, disp.1)
}

/// Route the Brush tool: draw the cursor ring, capture the in-progress stroke
/// via pure `mask_affordance::append_brush_node`, and emit a live (commit=false)
/// preview while dragging or a committed `MaskComponent::Brush` on release. Kept
/// as its own function so `show` doesn't grow another large inline branch.
fn route_brush(
    ui: &mut egui::Ui,
    image_rect: egui::Rect,
    stack: &OpStack,
    mask: &mut MaskUiState,
    idx: usize,
    src_dims: (u32, u32),
) -> Option<EditOutcome> {
    let geo = stack.geometry();
    let (src_w, src_h) = src_dims;
    let params = BrushParams {
        radius: mask.brush_radius,
        hardness: mask.brush_hardness,
        flow: mask.brush_flow,
    };

    let resp = ui.interact(
        image_rect,
        ui.id().with("mask_overlay_affordance"),
        egui::Sense::click_and_drag(),
    );

    // Cursor ring at the pointer: outer at the brush radius, a fainter inner
    // ring scaled by hardness (harder = the falloff starts closer to the edge).
    if let Some(p) = resp.hover_pos().or_else(|| resp.interact_pointer_pos()) {
        let screen_r = mask.brush_radius * image_rect.width();
        ui.painter()
            .circle_stroke(p, screen_r, egui::Stroke::new(1.5, theme::ACCENT_BRIGHT));
        ui.painter().circle_stroke(
            p,
            screen_r * mask.brush_hardness,
            egui::Stroke::new(1.0, theme::TEXT_FAINT),
        );
    }

    if resp.drag_started() {
        mask.gesture = Some(MaskGesture::Stroke(vec![], None));
    }

    let mut outcome: Option<EditOutcome> = None;
    if resp.dragged() || resp.drag_stopped() {
        if let (Some(MaskGesture::Stroke(nodes, comp_idx)), Some(p)) =
            (&mut mask.gesture, resp.interact_pointer_pos())
        {
            let norm = (
                ((p.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0),
                ((p.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0),
            );
            let src = display_to_source(geo, src_w, src_h, norm);
            mask_affordance::append_brush_node(nodes, src, params);

            let strokes = vec![Stroke {
                nodes: nodes.clone(),
                erase: mask.brush_erase,
            }];
            let comp = MaskComponent::Brush { strokes };
            // Two-phase, mirroring the Linear/Radial create-then-replace: the
            // first node append CREATES the component; every later frame
            // REPLACES it in place so a growing stroke doesn't append a new
            // component per dragged frame (the base `stack` passed in each
            // frame is the previous frame's preview stack, which already
            // carries the in-progress component).
            let new_stack = match *comp_idx {
                Some(ci) => mask_edit::set_component(stack, idx, ci, comp),
                None => {
                    let added = mask_edit::add_component(stack, idx, comp, mask.next_mode);
                    let new_idx = mask_edit::layers(&added).layers[idx].mask.components.len() - 1;
                    *comp_idx = Some(new_idx);
                    added
                }
            };
            outcome = Some(EditOutcome {
                stack: new_stack,
                kind: OpKind::LocalAdjustments,
                commit: resp.drag_stopped(),
            });
        }
    }
    if resp.drag_stopped() {
        mask.gesture = None;
    }

    outcome
}

/// Route the Color-range eyedropper: a click on the image inverse-maps to source
/// coords, samples `preview_source` via the pure `mask_affordance::sample_source`,
/// and pushes the result into `mask.color_samples` (UI state only — no OpStack
/// edit; the "Add Color range" button in the panel is what commits the
/// component). Also draws small swatches for the collected samples so the user
/// can see what's queued. Kept as its own function so `show` stays a thin
/// dispatcher (mirrors `route_brush`).
fn route_color_eyedropper(
    ui: &mut egui::Ui,
    image_rect: egui::Rect,
    mask: &mut MaskUiState,
    src_dims: (u32, u32),
    stack: &OpStack,
    preview_source: Option<&Arc<LinearRgbaF32>>,
) {
    if !mask.picking_color {
        return; // sampling (and the loupe) only while the "Pick color" toggle is armed
    }

    let resp = ui.interact(
        image_rect,
        ui.id().with("mask_overlay_affordance"),
        egui::Sense::click(),
    );
    let hover = resp.hover_pos().or_else(|| resp.interact_pointer_pos());

    if resp.clicked() {
        if let (Some(p), Some(src_img)) = (resp.interact_pointer_pos(), preview_source) {
            let geo = stack.geometry();
            let (src_w, src_h) = src_dims;
            let norm = (
                ((p.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0),
                ((p.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0),
            );
            let src_norm = display_to_source(geo, src_w, src_h, norm);
            let rgb = mask_affordance::sample_source(src_img, src_norm);
            mask.color_samples.push(rgb);
        }
    }

    // Picker cursor + zoom loupe while armed: a small grid of source pixels
    // around the pointer, magnified, with a crosshair on the exact sampled
    // pixel (bounded egui vector drawing — mirrors the brush cursor ring in
    // `route_brush`; no texture/sub-Ui allocation, no per-frame heavy work).
    if let (Some(p), Some(src_img)) = (hover, preview_source) {
        let painter = ui.painter();
        const LOUPE_R: f32 = 44.0;
        const ZOOM: f32 = 8.0; // source px per loupe px
        let center = p - egui::vec2(0.0, LOUPE_R + 16.0); // float above the pointer
        let geo = stack.geometry();
        let (src_w, src_h) = src_dims;
        let span = (LOUPE_R / ZOOM) as i32; // half-width in source px
        for dy in -span..=span {
            for dx in -span..=span {
                let n = (
                    ((p.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0)
                        + dx as f32 / src_w as f32,
                    ((p.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0)
                        + dy as f32 / src_h as f32,
                );
                let sn = display_to_source(
                    geo,
                    src_w,
                    src_h,
                    (n.0.clamp(0.0, 1.0), n.1.clamp(0.0, 1.0)),
                );
                let rgb = mask_affordance::sample_source(src_img, sn);
                let cell = egui::Rect::from_center_size(
                    center + egui::vec2(dx as f32 * ZOOM, dy as f32 * ZOOM),
                    egui::vec2(ZOOM, ZOOM),
                );
                painter.rect_filled(
                    cell,
                    0.0,
                    egui::Color32::from_rgb(
                        (rgb.r.clamp(0.0, 1.0) * 255.0) as u8,
                        (rgb.g.clamp(0.0, 1.0) * 255.0) as u8,
                        (rgb.b.clamp(0.0, 1.0) * 255.0) as u8,
                    ),
                );
            }
        }
        painter.circle_stroke(
            center,
            LOUPE_R,
            egui::Stroke::new(1.5, theme::ACCENT_BRIGHT),
        );
        // Crosshair on the exact sampled (center) pixel.
        painter.line_segment(
            [center - egui::vec2(6.0, 0.0), center + egui::vec2(6.0, 0.0)],
            egui::Stroke::new(1.0, theme::BG_BASE),
        );
        painter.line_segment(
            [center - egui::vec2(0.0, 6.0), center + egui::vec2(0.0, 6.0)],
            egui::Stroke::new(1.0, theme::BG_BASE),
        );
        // Picker dot at the pointer itself.
        painter.circle_stroke(p, 3.0, egui::Stroke::new(1.5, theme::ACCENT_BRIGHT));
    }

    // Small swatches for the collected samples, stacked along the top-left of
    // the image rect so they don't obscure the pick point.
    let painter = ui.painter();
    const SWATCH_R: f32 = 6.0;
    const SWATCH_SPACING: f32 = 16.0;
    for (i, s) in mask.color_samples.iter().enumerate() {
        let center = egui::pos2(
            image_rect.left() + 12.0 + i as f32 * SWATCH_SPACING,
            image_rect.top() + 12.0,
        );
        let color = egui::Color32::from_rgb(
            (s.r.clamp(0.0, 1.0) * 255.0) as u8,
            (s.g.clamp(0.0, 1.0) * 255.0) as u8,
            (s.b.clamp(0.0, 1.0) * 255.0) as u8,
        );
        painter.circle(
            center,
            SWATCH_R,
            color,
            egui::Stroke::new(1.0, theme::BG_BASE),
        );
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;
    #[test]
    fn scroll_is_multiplicative_and_clamped() {
        let (min, max) = (0.005f32, 0.5f32);
        // up grows, down shrinks
        assert!(
            brush_radius_from_scroll(0.1, 120.0, min, max) > 0.1,
            "up grows"
        );
        assert!(
            brush_radius_from_scroll(0.1, -120.0, min, max) < 0.1,
            "down shrinks"
        );
        // clamps
        assert_eq!(
            brush_radius_from_scroll(0.49, 100_000.0, min, max),
            max,
            "clamp hi"
        );
        assert_eq!(
            brush_radius_from_scroll(0.01, -100_000.0, min, max),
            min,
            "clamp lo"
        );
        // exponential: same scroll delta => larger ABSOLUTE change at a larger radius
        let small_delta = brush_radius_from_scroll(0.02, 120.0, min, max) - 0.02;
        let large_delta = brush_radius_from_scroll(0.20, 120.0, min, max) - 0.20;
        assert!(
            large_delta > small_delta,
            "bigger absolute step when larger (exponential feel)"
        );
        // and the RATIO is ~constant (multiplicative)
        let r_small = brush_radius_from_scroll(0.02, 120.0, min, max) / 0.02;
        let r_large = brush_radius_from_scroll(0.20, 120.0, min, max) / 0.20;
        assert!(
            (r_small - r_large).abs() < 1e-3,
            "constant multiplicative ratio"
        );
    }
    #[test]
    fn zero_scroll_is_noop() {
        assert_eq!(brush_radius_from_scroll(0.1, 0.0, 0.005, 0.5), 0.1);
    }
}

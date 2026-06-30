//! Canvas crop overlay. Pure geometry in `crop_math`; this layer paints handles +
//! a rule-of-thirds grid and routes pointer events. Shown only while the Geometry
//! section is active (viewer.crop_active). Visual-tested.

use crate::develop::adjustment_panel::EditOutcome;
use crate::develop::crop_math::{self, Handle};
use crate::theme;
use ferrolite_pipeline::{Aspect, CropRect, Geometry, Op, OpKind, OpStack};

const HANDLE_R: f32 = 0.03; // normalized hit radius

pub fn show(
    ui: &mut egui::Ui,
    image_rect: egui::Rect,
    stack: &OpStack,
    aspect_dims: (u32, u32),
) -> Option<EditOutcome> {
    let geo = stack.geometry().unwrap_or(Geometry {
        crop: CropRect::full(),
        angle_deg: 0.0,
        aspect: Aspect::Original,
    });
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

    // Crop rect + rule-of-thirds.
    let r = egui::Rect::from_min_max(
        to_screen(crop.x, crop.y),
        to_screen(crop.x + crop.w, crop.y + crop.h),
    );
    let painter = ui.painter();
    painter.rect_stroke(r, 0.0, egui::Stroke::new(1.5, theme::ACCENT_BRIGHT));
    for i in 1..3 {
        let f = i as f32 / 3.0;
        painter.line_segment(
            [
                egui::pos2(r.left() + f * r.width(), r.top()),
                egui::pos2(r.left() + f * r.width(), r.bottom()),
            ],
            egui::Stroke::new(1.0, theme::ACCENT),
        );
        painter.line_segment(
            [
                egui::pos2(r.left(), r.top() + f * r.height()),
                egui::pos2(r.right(), r.top() + f * r.height()),
            ],
            egui::Stroke::new(1.0, theme::ACCENT),
        );
    }
    for (nx, ny) in [
        (crop.x, crop.y),
        (crop.x + crop.w, crop.y),
        (crop.x, crop.y + crop.h),
        (crop.x + crop.w, crop.y + crop.h),
    ] {
        painter.circle(
            to_screen(nx, ny),
            4.0,
            theme::ACCENT_BRIGHT,
            egui::Stroke::new(1.0, theme::BG_BASE),
        );
    }

    let resp = ui.interact(
        image_rect,
        ui.id().with("crop_overlay"),
        egui::Sense::click_and_drag(),
    );
    let aspect = crop_math::aspect_ratio(geo.aspect, aspect_dims.0, aspect_dims.1);
    let mut new_crop = crop;
    let mut changed = false;
    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            let h = crop_math::hit_test(crop, to_norm(p), HANDLE_R);
            ui.memory_mut(|m| m.data.insert_temp(resp.id, h.map(|h| h as u8).unwrap_or(255)));
        }
    }
    if resp.dragged() {
        let active: u8 = ui.memory(|m| m.data.get_temp(resp.id)).unwrap_or(255);
        if let Some(p) = resp.interact_pointer_pos() {
            let norm = to_norm(p);
            match active {
                x if x == Handle::Body as u8 => {
                    let d = (
                        resp.drag_delta().x / image_rect.width(),
                        resp.drag_delta().y / image_rect.height(),
                    );
                    new_crop = crop_math::move_body(crop, d);
                    changed = true;
                }
                255 => {}
                _ => {
                    let handle = HANDLES[active as usize];
                    new_crop = crop_math::resize(crop, handle, norm, aspect);
                    changed = true;
                }
            }
        }
    }
    if changed {
        let new_geo = Geometry {
            crop: new_crop,
            angle_deg: geo.angle_deg,
            aspect: geo.aspect,
        };
        return Some(EditOutcome {
            stack: stack.set_op(Op::Geometry(new_geo)),
            kind: OpKind::Geometry,
            commit: resp.drag_stopped(),
        });
    }
    None
}

// Index map matching `Handle as u8` for the resize handles (Body handled separately).
// MUST stay in the same order as the `Handle` enum declaration so that
// `HANDLES[h as usize] == h` for every variant. A debug_assert in tests enforces this.
const HANDLES: [Handle; 9] = [
    Handle::TopLeft,
    Handle::Top,
    Handle::TopRight,
    Handle::Right,
    Handle::BottomRight,
    Handle::Bottom,
    Handle::BottomLeft,
    Handle::Left,
    Handle::Body,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `HANDLES[h as usize] == h` for every variant — if the enum
    /// order and the array ever drift, this test catches it at compile time (the
    /// `#[repr(u8)]` guarantee ensures `as u8` is well-defined).
    #[test]
    fn handles_array_matches_enum_discriminants() {
        for (i, &h) in HANDLES.iter().enumerate() {
            assert_eq!(
                h as usize,
                i,
                "HANDLES[{i}] discriminant mismatch: array has {h:?} but expected discriminant {i}"
            );
        }
    }
}

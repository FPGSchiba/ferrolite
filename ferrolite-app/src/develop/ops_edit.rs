//! Pure helpers: map a UI value to a new immutable `OpStack`. A value at its
//! identity default REMOVES the op so `is_identity()`/`has_edits` stay correct.
#![allow(dead_code)] // adjustment panel not yet wired (Task 10)

use ferrolite_pipeline::{
    sharpen_halo, Contrast, Exposure, Op, OpStack, Sharpen, WhiteBalance,
};

pub fn set_exposure(s: &OpStack, ev: f32) -> OpStack {
    if ev == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::Exposure)
    } else {
        s.set_op(Op::Exposure(Exposure { ev }))
    }
}

pub fn set_white_balance(s: &OpStack, temp: f32, tint: f32) -> OpStack {
    if temp == 0.0 && tint == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::WhiteBalance)
    } else {
        s.set_op(Op::WhiteBalance(WhiteBalance { temp, tint }))
    }
}

pub fn set_contrast(s: &OpStack, amount: f32) -> OpStack {
    if amount == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::Contrast)
    } else {
        s.set_op(Op::Contrast(Contrast { amount }))
    }
}

pub fn set_sharpen(s: &OpStack, amount: f32, radius: u32) -> OpStack {
    if amount == 0.0 {
        s.reset(ferrolite_pipeline::OpKind::Sharpen)
    } else {
        s.set_op(Op::Sharpen(Sharpen { amount, radius }))
    }
}

/// The full-res `TileEditPipeline` bakes geometry + the sharpen halo at
/// construction; only a change to either requires discarding + rebuilding it.
/// Color-only changes are applied via `TileEditPipeline::set_stack`.
pub fn needs_full_rebuild(old: &OpStack, new: &OpStack) -> bool {
    old.geometry() != new.geometry()
        || sharpen_halo(old.sharpen()) != sharpen_halo(new.sharpen())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{Op, OpStack};

    #[test]
    fn set_exposure_adds_then_identity_removes() {
        let s = set_exposure(&OpStack::default(), 0.5);
        assert_eq!(s.exposure().unwrap().ev, 0.5);
        let s2 = set_exposure(&s, 0.0);
        assert!(s2.exposure().is_none(), "identity ev removes the op");
        assert!(s2.is_identity());
    }

    #[test]
    fn set_white_balance_identity_when_both_zero() {
        let s = set_white_balance(&OpStack::default(), 0.0, 0.0);
        assert!(s.white_balance().is_none());
    }

    #[test]
    fn set_sharpen_identity_when_amount_zero() {
        let s = set_sharpen(&OpStack::default(), 0.0, 3);
        assert!(s.sharpen().is_none(), "zero amount = no sharpen");
        let s = set_sharpen(&OpStack::default(), 0.4, 2);
        assert_eq!(s.sharpen(), Some(ferrolite_pipeline::Sharpen { amount: 0.4, radius: 2 }));
    }

    #[test]
    fn needs_full_rebuild_on_geometry_and_halo_only() {
        let base = set_exposure(&OpStack::default(), 0.5);
        let color_only = set_contrast(&base, 0.3);
        assert!(!needs_full_rebuild(&base, &color_only), "color ops: no rebuild");
        let sharper = set_sharpen(&base, 0.5, 5);
        assert!(needs_full_rebuild(&base, &sharper), "halo change: rebuild");
        let geo = base.set_op(Op::Geometry(ferrolite_pipeline::Geometry {
            crop: ferrolite_pipeline::CropRect::full(),
            angle_deg: 5.0,
            aspect: ferrolite_pipeline::Aspect::Free,
        }));
        assert!(needs_full_rebuild(&base, &geo), "geometry change: rebuild");
    }
}

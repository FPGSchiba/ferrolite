//! Bounded undo/redo ring of `OpStack` snapshots with same-kind coalescing.
//! Per-open-image; not persisted (only the resulting `OpStack` persists).

use ferrolite_pipeline::{OpKind, OpStack};

pub struct History {
    entries: Vec<OpStack>,
    cursor: usize,
    cap: usize,
    last_kind: Option<OpKind>,
}

impl History {
    pub fn new(initial: OpStack, cap: usize) -> Self {
        Self {
            entries: vec![initial],
            cursor: 0,
            cap: cap.max(1),
            last_kind: None,
        }
    }

    /// Returns the current op-stack at the cursor position.
    // public API: reading the live stack; not yet wired to callers outside tests
    #[allow(dead_code)]
    pub fn current(&self) -> &OpStack {
        &self.entries[self.cursor]
    }

    /// Returns `true` if there is at least one undo step available. Drives
    /// the Edit menu's Undo item enable-state.
    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    /// Returns `true` if there is at least one redo step available. Drives
    /// the Edit menu's Redo item enable-state.
    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    pub fn push(&mut self, kind: OpKind, stack: OpStack) {
        // Drop any redo tail.
        self.entries.truncate(self.cursor + 1);
        if self.last_kind == Some(kind) && self.cursor > 0 {
            // Coalesce: replace the tip rather than append a new step.
            self.entries[self.cursor] = stack;
        } else {
            self.entries.push(stack);
            self.cursor += 1;
            self.last_kind = Some(kind);
        }
        // Enforce the bound (drop oldest).
        while self.entries.len() > self.cap {
            self.entries.remove(0);
            self.cursor -= 1;
        }
    }

    pub fn undo(&mut self) -> Option<OpStack> {
        if !self.can_undo() {
            return None;
        }
        self.cursor -= 1;
        self.last_kind = None; // next edit starts a fresh step
        Some(self.entries[self.cursor].clone())
    }

    pub fn redo(&mut self) -> Option<OpStack> {
        if !self.can_redo() {
            return None;
        }
        self.cursor += 1;
        self.last_kind = None;
        Some(self.entries[self.cursor].clone())
    }

    /// Force the NEXT `push` to start a fresh step instead of coalescing into the
    /// current tip. Called after a committed mask gesture (all mask edits share
    /// `OpKind::LocalAdjustments`, so kind-based coalescing would otherwise merge
    /// separate gestures — a stroke and a later slider edit — into one undo step).
    pub fn break_coalesce(&mut self) {
        self.last_kind = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrolite_pipeline::{Exposure, Op, OpKind, OpStack};

    fn ev(stack: &OpStack, v: f32) -> OpStack {
        stack.set_op(Op::Exposure(Exposure { ev: v }))
    }

    #[test]
    fn coalesces_same_kind_into_one_step() {
        let mut h = History::new(OpStack::default(), 50);
        let s1 = ev(&OpStack::default(), 0.1);
        let s2 = ev(&OpStack::default(), 0.2);
        let s3 = ev(&OpStack::default(), 0.3);
        h.push(OpKind::Exposure, s1);
        h.push(OpKind::Exposure, s2);
        h.push(OpKind::Exposure, s3.clone());
        assert_eq!(h.current(), &s3);
        // One undo returns to the pre-drag state (identity), not 0.2/0.1.
        assert_eq!(h.undo(), Some(OpStack::default()));
        assert!(!h.can_undo());
    }

    #[test]
    fn different_kind_starts_new_step() {
        let mut h = History::new(OpStack::default(), 50);
        let s1 = ev(&OpStack::default(), 0.5);
        let s2 = s1.set_op(Op::Contrast(ferrolite_pipeline::Contrast { amount: 0.3 }));
        h.push(OpKind::Exposure, s1.clone());
        h.push(OpKind::Contrast, s2.clone());
        assert_eq!(h.undo(), Some(s1));
        assert_eq!(h.undo(), Some(OpStack::default()));
    }

    #[test]
    fn redo_after_undo_then_push_truncates() {
        let mut h = History::new(OpStack::default(), 50);
        let a = ev(&OpStack::default(), 0.5);
        h.push(OpKind::Exposure, a.clone());
        assert_eq!(h.undo(), Some(OpStack::default()));
        assert_eq!(h.redo(), Some(a));
        assert_eq!(h.undo(), Some(OpStack::default()));
        // A new push after an undo drops the redo tail.
        let b =
            OpStack::default().set_op(Op::Contrast(ferrolite_pipeline::Contrast { amount: 0.2 }));
        h.push(OpKind::Contrast, b);
        assert!(!h.can_redo(), "redo tail dropped after a fresh push");
    }

    #[test]
    fn cap_drops_oldest() {
        let mut h = History::new(OpStack::default(), 2); // initial + 1 more
        h.push(OpKind::Exposure, ev(&OpStack::default(), 0.1));
        h.push(
            OpKind::Contrast,
            OpStack::default().set_op(Op::Contrast(ferrolite_pipeline::Contrast { amount: 0.2 })),
        );
        // Capacity 2 means at most 2 entries; the oldest (identity) was dropped.
        let mut steps = 0;
        while h.undo().is_some() {
            steps += 1;
        }
        assert_eq!(
            steps, 1,
            "cap=2: exactly one undo step survives after eviction"
        );
    }

    #[test]
    fn break_coalesce_separates_same_kind_gestures() {
        use ferrolite_pipeline::{LocalAdjustments, Op};
        let la = |n: usize| {
            let mut d = LocalAdjustments::default();
            for i in 0..n {
                d.layers.push(ferrolite_pipeline::MaskLayer {
                    name: format!("m{i}"),
                    visible: true,
                    mask: Default::default(),
                    adjustments: Default::default(),
                });
            }
            OpStack::default().set_op(Op::LocalAdjustments(d))
        };
        let mut h = History::new(OpStack::default(), 50);
        // Gesture 1: one mask (a "stroke" commit).
        h.push(OpKind::LocalAdjustments, la(1));
        h.break_coalesce();
        // Gesture 2: two masks (a distinct "slider" commit) — must NOT coalesce into gesture 1.
        h.push(OpKind::LocalAdjustments, la(2));
        // Two distinct undo steps back to identity.
        assert_eq!(h.undo(), Some(la(1)), "undo returns to after gesture 1");
        assert_eq!(
            h.undo(),
            Some(OpStack::default()),
            "undo returns to identity"
        );
        assert!(!h.can_undo());
    }

    #[test]
    fn no_coalesce_after_undo() {
        let ev = |v: f32| OpStack::default().set_op(Op::Exposure(Exposure { ev: v }));
        let mut h = History::new(OpStack::default(), 50);
        h.push(OpKind::Exposure, ev(0.1));
        h.push(OpKind::Exposure, ev(0.2)); // coalesces -> one step at tip
        assert_eq!(h.undo(), Some(OpStack::default()), "back to pre-drag state");
        // After undo, last_kind is reset, so this same-kind push must NOT coalesce
        // into the pre-undo step — it starts a fresh step.
        h.push(OpKind::Exposure, ev(0.5));
        // Now exactly one undo step should be reachable (back to identity), and the
        // push must have truncated any redo tail.
        assert!(!h.can_redo(), "push after undo dropped the redo tail");
        assert_eq!(
            h.undo(),
            Some(OpStack::default()),
            "fresh step undoes to identity"
        );
        assert!(!h.can_undo(), "only one step existed after the undo+push");
    }
}

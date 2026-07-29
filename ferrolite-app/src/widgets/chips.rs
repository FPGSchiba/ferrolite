//! SegmentedControl widget — contiguous or wrapping horizontal rounded buttons.
//! Also home to `multi_select_chips`, the independent-toggle sibling used by
//! the Library toolbar's file-type filter (`library::toolbar`), which shares
//! the exact same chip visuals via `chip_button` below.

use crate::theme;
use std::collections::BTreeSet;

/// A builder for rendering a segmented control widget.
#[allow(dead_code)]
pub struct SegmentedControl<'a, T> {
    current: &'a mut T,
    options: &'a [(T, &'a str)],
}

impl<'a, T: PartialEq + Clone> SegmentedControl<'a, T> {
    /// Create a new `SegmentedControl` widget builder.
    #[allow(dead_code)]
    pub fn new(current: &'a mut T, options: &'a [(T, &'a str)]) -> Self {
        Self { current, options }
    }

    /// Render the segmented control into the provided `egui::Ui`.
    #[allow(dead_code)]
    pub fn ui(self, ui: &mut egui::Ui, id_source: impl std::hash::Hash) -> egui::Response {
        segmented_control(ui, id_source, self.current, self.options)
    }
}

/// Draw one chip button (rounded rect + centered label) and return its click
/// response. Shared paint code for `segmented_control` (single-select,
/// mutually exclusive) and `multi_select_chips` (independent per-chip
/// toggles) so both stay visually identical:
///
/// - 3px border radius (`Rounding::same(3.0_f32)`).
/// - Active: `theme::ACCENT_FILL` (`#232b30`), 1px `theme::ACCENT_BORDER` (`#34464f`), `theme::ACCENT_TEXT` (`#cfe0ec`).
/// - Inactive: `theme::BG_BASE` (`#141414`), 1px `theme::BORDER_STRONG` (`#2a2a2a`), `theme::TEXT_DIM` (`#8a8a8a`).
fn chip_button(ui: &mut egui::Ui, label: &str, is_active: bool) -> egui::Response {
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let temp_color = if is_active {
        theme::ACCENT_TEXT
    } else {
        theme::TEXT_DIM
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, temp_color);

    let button_size = egui::vec2(galley.rect.width() + 16.0_f32, 24.0_f32);
    let (rect, resp) = ui.allocate_exact_size(button_size, egui::Sense::click());

    if resp.hovered() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
    }

    let fill = if is_active {
        theme::ACCENT_FILL
    } else {
        theme::BG_BASE
    };

    let stroke = if is_active {
        egui::Stroke::new(1.0_f32, theme::ACCENT_BORDER)
    } else {
        egui::Stroke::new(1.0_f32, theme::BORDER_STRONG)
    };

    let text_color = if is_active {
        theme::ACCENT_TEXT
    } else if resp.hovered() {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_DIM
    };

    let painter = ui.painter();
    painter.rect(rect, egui::Rounding::same(3.0_f32), fill, stroke);

    let text_galley = painter.layout_no_wrap(
        label.to_string(),
        egui::TextStyle::Button.resolve(ui.style()),
        text_color,
    );
    let text_pos = egui::pos2(
        rect.center().x - text_galley.rect.width() / 2.0_f32,
        rect.center().y - text_galley.rect.height() / 2.0_f32,
    );
    painter.galley(text_pos, text_galley, text_color);

    resp
}

/// Render a segmented control with horizontal option buttons.
///
/// - 3px border radius (`Rounding::same(3.0_f32)`).
/// - Active: `theme::ACCENT_FILL` (`#232b30`), 1px `theme::ACCENT_BORDER` (`#34464f`), `theme::ACCENT_TEXT` (`#cfe0ec`).
/// - Inactive: `theme::BG_BASE` (`#141414`), 1px `theme::BORDER_STRONG` (`#2a2a2a`), `theme::TEXT_DIM` (`#8a8a8a`).
#[allow(dead_code)]
pub fn segmented_control<T: PartialEq + Clone>(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    current: &mut T,
    options: &[(T, &str)],
) -> egui::Response {
    let mut changed = false;
    let mut response: Option<egui::Response> = None;

    ui.push_id(id_source, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0_f32;
            for (value, label) in options {
                let is_active = *current == *value;
                let resp = chip_button(ui, label, is_active);

                if resp.clicked() && !is_active {
                    *current = value.clone();
                    changed = true;
                }

                if let Some(r) = response.as_mut() {
                    *r = r.union(resp);
                } else {
                    response = Some(resp);
                }
            }
        });
    });

    let mut resp = response.unwrap_or_else(|| {
        let (_rect, r) = ui.allocate_exact_size(egui::vec2(0.0_f32, 0.0_f32), egui::Sense::hover());
        r
    });
    if changed {
        resp.mark_changed();
    }
    resp
}

/// Render each `options` entry as an independently toggleable chip over set
/// membership: clicking a chip adds it to `selected` if absent, or removes it
/// if present. Any number of chips may be active at once (unlike
/// `segmented_control`, which is mutually exclusive). An empty `selected` is
/// a valid, common state — callers treat it as "no filter"/"all" — so this
/// widget itself never forces a selection.
///
/// Used by the Library toolbar's file-type filter (`library::toolbar`),
/// where an empty set means "all file types".
pub fn multi_select_chips<T: Ord + Copy>(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    selected: &mut BTreeSet<T>,
    options: &[(T, &str)],
) -> egui::Response {
    let mut changed = false;
    let mut response: Option<egui::Response> = None;

    ui.push_id(id_source, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0_f32;
            for (value, label) in options {
                let is_active = selected.contains(value);
                let resp = chip_button(ui, label, is_active);

                if resp.clicked() {
                    if is_active {
                        selected.remove(value);
                    } else {
                        selected.insert(*value);
                    }
                    changed = true;
                }

                if let Some(r) = response.as_mut() {
                    *r = r.union(resp);
                } else {
                    response = Some(resp);
                }
            }
        });
    });

    let mut resp = response.unwrap_or_else(|| {
        let (_rect, r) = ui.allocate_exact_size(egui::vec2(0.0_f32, 0.0_f32), egui::Sense::hover());
        r
    });
    if changed {
        resp.mark_changed();
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Context, RawInput};

    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
    enum TestOption {
        OptA,
        OptB,
        OptC,
    }

    #[test]
    fn segmented_control_initial_state() {
        let ctx = Context::default();
        let mut current = TestOption::OptA;
        let options = [
            (TestOption::OptA, "Option A"),
            (TestOption::OptB, "Option B"),
            (TestOption::OptC, "Option C"),
        ];

        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = segmented_control(ui, "test_ctrl", &mut current, &options);
                assert!(!resp.changed());
            });
        });

        assert_eq!(current, TestOption::OptA);
    }

    #[test]
    fn segmented_control_builder_ui() {
        let ctx = Context::default();
        let mut current = TestOption::OptA;
        let options = [
            (TestOption::OptA, "Option A"),
            (TestOption::OptB, "Option B"),
        ];

        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = SegmentedControl::new(&mut current, &options).ui(ui, "builder_ctrl");
                assert!(!resp.changed());
            });
        });

        assert_eq!(current, TestOption::OptA);
    }

    #[test]
    fn segmented_control_selection_change() {
        let ctx = Context::default();
        let mut current = TestOption::OptA;
        let options = [
            (TestOption::OptA, "Option A"),
            (TestOption::OptB, "Option B"),
        ];

        // Render pass 1 to set layout bounds
        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = segmented_control(ui, "test_ctrl", &mut current, &options);
            });
        });

        // Pass 2 with pointer click on the second option region
        let mut input = RawInput::default();
        let click_pos = egui::pos2(80.0_f32, 12.0_f32);
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });

        let mut changed = false;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = segmented_control(ui, "test_ctrl", &mut current, &options);
                changed = resp.changed();
            });
        });

        assert_eq!(current, TestOption::OptB);
        assert!(changed);
    }

    #[test]
    fn segmented_control_empty() {
        let ctx = Context::default();
        let mut current = TestOption::OptA;
        let options: [(TestOption, &str); 0] = [];

        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = segmented_control(ui, "empty_ctrl", &mut current, &options);
                assert!(!resp.changed());
            });
        });
        assert_eq!(current, TestOption::OptA);
    }

    /// Step 1 (Task 8): toggling a chip must behave as a set, not a
    /// single-choice replacement — clicking an unselected chip inserts it
    /// WITHOUT disturbing any other already-selected chip.
    #[test]
    fn multi_select_chips_click_adds_without_clearing_others() {
        let ctx = Context::default();
        let mut selected: BTreeSet<TestOption> = BTreeSet::new();
        selected.insert(TestOption::OptA);
        let options = [
            (TestOption::OptA, "Option A"),
            (TestOption::OptB, "Option B"),
        ];

        // Layout pass.
        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = multi_select_chips(ui, "test_multi", &mut selected, &options);
            });
        });

        // Click the second chip ("Option B").
        let mut input = RawInput::default();
        let click_pos = egui::pos2(80.0_f32, 12.0_f32);
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });

        let mut changed = false;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = multi_select_chips(ui, "test_multi", &mut selected, &options);
                changed = resp.changed();
            });
        });

        assert!(changed);
        assert!(selected.contains(&TestOption::OptA));
        assert!(selected.contains(&TestOption::OptB));
        assert_eq!(selected.len(), 2);
    }

    /// Step 1 (Task 8): clicking an already-selected chip removes just that
    /// chip from the set (a re-click toggles it back out), and an emptied set
    /// is a perfectly valid end state (the "all types" reset state).
    #[test]
    fn multi_select_chips_click_removes_when_already_selected() {
        let ctx = Context::default();
        let mut selected: BTreeSet<TestOption> = BTreeSet::new();
        selected.insert(TestOption::OptA);
        let options = [(TestOption::OptA, "Option A")];

        // Layout pass.
        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = multi_select_chips(ui, "test_multi_remove", &mut selected, &options);
            });
        });

        // Click the only chip again to deselect it.
        let mut input = RawInput::default();
        let click_pos = egui::pos2(30.0_f32, 12.0_f32);
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos: click_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });

        let mut changed = false;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = multi_select_chips(ui, "test_multi_remove", &mut selected, &options);
                changed = resp.changed();
            });
        });

        assert!(changed);
        assert!(selected.is_empty());
    }

    #[test]
    fn multi_select_chips_empty_options_is_a_no_op() {
        let ctx = Context::default();
        let mut selected: BTreeSet<TestOption> = BTreeSet::new();
        let options: [(TestOption, &str); 0] = [];

        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = multi_select_chips(ui, "empty_multi", &mut selected, &options);
                assert!(!resp.changed());
            });
        });
        assert!(selected.is_empty());
    }
}

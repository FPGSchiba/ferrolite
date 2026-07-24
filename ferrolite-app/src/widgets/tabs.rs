//! TabRow widget — horizontal bar of selectable tabs with active accent bottom stroke.

use crate::theme;

/// A builder for rendering a row of selectable tabs.
#[allow(dead_code)]
pub struct TabRow<'a, T> {
    current: &'a mut T,
    tabs: &'a [(T, &'a str)],
}

impl<'a, T: PartialEq + Clone> TabRow<'a, T> {
    /// Create a new `TabRow` widget builder.
    #[allow(dead_code)]
    pub fn new(current: &'a mut T, tabs: &'a [(T, &'a str)]) -> Self {
        Self { current, tabs }
    }

    /// Render the tab row into the provided `egui::Ui`.
    #[allow(dead_code)]
    pub fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        tab_row(ui, self.current, self.tabs)
    }
}

/// Render a horizontal row of tab labels.
///
/// - Active tab: `theme::TEXT_ACTIVE` (`#eaf1f6`) text + 2px bottom stroke with `theme::ACCENT` (`#6d97b5`).
/// - Inactive tab: `theme::TEXT_INACTIVE` (`#9a9a9a`) text + transparent/no bottom stroke.
#[allow(dead_code)]
pub fn tab_row<T: PartialEq + Clone>(
    ui: &mut egui::Ui,
    current: &mut T,
    tabs: &[(T, &str)],
) -> egui::Response {
    let mut changed = false;
    let mut response: Option<egui::Response> = None;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0_f32;
        for (value, label) in tabs {
            let is_active = *current == *value;

            let font_id = egui::TextStyle::Button.resolve(ui.style());
            let temp_color = if is_active {
                theme::TEXT_ACTIVE
            } else {
                theme::TEXT_INACTIVE
            };
            let galley = ui
                .painter()
                .layout_no_wrap((*label).to_string(), font_id, temp_color);

            let tab_size = egui::vec2(galley.rect.width() + 16.0_f32, 28.0_f32);
            let (rect, resp) = ui.allocate_exact_size(tab_size, egui::Sense::click());

            if resp.hovered() {
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
            }

            if resp.clicked() && !is_active {
                *current = value.clone();
                changed = true;
            }

            let text_color = if is_active {
                theme::TEXT_ACTIVE
            } else if resp.hovered() {
                theme::TEXT_PRIMARY
            } else {
                theme::TEXT_INACTIVE
            };

            let painter = ui.painter();
            let text_galley = painter.layout_no_wrap(
                (*label).to_string(),
                egui::TextStyle::Button.resolve(ui.style()),
                text_color,
            );
            let text_pos = egui::pos2(
                rect.center().x - text_galley.rect.width() / 2.0_f32,
                rect.center().y - text_galley.rect.height() / 2.0_f32,
            );
            painter.galley(text_pos, text_galley, text_color);

            if is_active {
                painter.line_segment(
                    [
                        egui::pos2(rect.left(), rect.bottom() - 1.0),
                        egui::pos2(rect.right(), rect.bottom() - 1.0),
                    ],
                    egui::Stroke::new(2.0_f32, theme::ACCENT),
                );
            }

            if let Some(r) = response.as_mut() {
                *r = r.union(resp);
            } else {
                response = Some(resp);
            }
        }
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

    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    enum TestTab {
        First,
        Second,
        Third,
    }

    #[test]
    fn tab_row_initial_state() {
        let ctx = Context::default();
        let mut current = TestTab::First;
        let tabs = [
            (TestTab::First, "First"),
            (TestTab::Second, "Second"),
            (TestTab::Third, "Third"),
        ];

        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = tab_row(ui, &mut current, &tabs);
                assert!(!resp.changed());
            });
        });

        assert_eq!(current, TestTab::First);
    }

    #[test]
    fn tab_row_builder_ui() {
        let ctx = Context::default();
        let mut current = TestTab::First;
        let tabs = [(TestTab::First, "First"), (TestTab::Second, "Second")];

        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = TabRow::new(&mut current, &tabs).ui(ui);
                assert!(!resp.changed());
            });
        });

        assert_eq!(current, TestTab::First);
    }

    #[test]
    fn tab_row_selection_change() {
        let ctx = Context::default();
        let mut current = TestTab::First;
        let tabs = [(TestTab::First, "First"), (TestTab::Second, "Second")];

        // Render pass 1 to set layout bounds
        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = tab_row(ui, &mut current, &tabs);
            });
        });

        // Pass 2 with pointer click on the second tab region
        let mut input = RawInput::default();
        let click_pos = egui::pos2(80.0_f32, 14.0_f32);
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
                let resp = tab_row(ui, &mut current, &tabs);
                changed = resp.changed();
            });
        });

        assert_eq!(current, TestTab::Second);
        assert!(changed);
    }

    #[test]
    fn tab_row_empty() {
        let ctx = Context::default();
        let mut current = TestTab::First;
        let tabs: [(TestTab, &str); 0] = [];

        let _ = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = tab_row(ui, &mut current, &tabs);
                assert!(!resp.changed());
            });
        });
        assert_eq!(current, TestTab::First);
    }

    #[test]
    fn tab_row_active_underline_stroke() {
        let ctx = Context::default();
        let mut current = TestTab::First;
        let tabs = [(TestTab::First, "First"), (TestTab::Second, "Second")];

        let output = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = tab_row(ui, &mut current, &tabs);
            });
        });

        fn has_accent_underline(shape: &egui::Shape) -> bool {
            match shape {
                egui::Shape::LineSegment { points, stroke } => {
                    (stroke.width - 2.0_f32).abs() < 1e-4
                        && stroke.color == egui::epaint::ColorMode::Solid(theme::ACCENT)
                        && (points[0].y - points[1].y).abs() < 1e-4
                }
                egui::Shape::Vec(shapes) => shapes.iter().any(has_accent_underline),
                _ => false,
            }
        }

        let found = output
            .shapes
            .iter()
            .any(|clipped| has_accent_underline(&clipped.shape));
        assert!(
            found,
            "Active tab underline stroke (2px theme::ACCENT line segment) should be painted"
        );
    }
}

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

/// Fixed height of a tab row (each tab's clickable rect).
///
/// Allocated upfront (see `tab_row`) so a vertically-centering parent (the 30px
/// titlebar) centers the row using its TRUE height. `ui.horizontal()` must NOT be
/// used here: it sizes its child row to the default `interact_size.y` (18px), the
/// `Align::Center` titlebar centers that 18px child (top = 6), and the 28px tabs
/// then grow DOWNWARD to y = 34 — past the 30px panel clip rect — so anything
/// painted near `rect.bottom()` (the active-tab underline) is clipped invisible.
pub const TAB_ROW_HEIGHT: f32 = 28.0_f32;

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

    // Allocate the row at its real height so the parent's cross-axis centering is
    // exact (see `TAB_ROW_HEIGHT` — `ui.horizontal()` would overflow the titlebar).
    let row_size = egui::vec2(ui.available_width(), TAB_ROW_HEIGHT);
    ui.allocate_ui_with_layout(
        row_size,
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
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

                let tab_size = egui::vec2(galley.rect.width() + 16.0_f32, TAB_ROW_HEIGHT);
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
                    // 2px accent underline under the active label. `rect` is guaranteed to
                    // sit inside the parent (the row is allocated at its true height above),
                    // so `rect.bottom() - 2.0` keeps the full 2px stroke (y in
                    // [bottom-3, bottom-1]) inside the titlebar's clip rect. The two failed
                    // pre-fix placements died because the row overflowed the 30px bar
                    // (rect.bottom() was 34) and the stroke landed below the panel clip.
                    painter.line_segment(
                        [
                            egui::pos2(rect.left() + 6.0_f32, rect.bottom() - 2.0_f32),
                            egui::pos2(rect.right() - 6.0_f32, rect.bottom() - 2.0_f32),
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
        },
    );

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

        let mut row_rect = egui::Rect::NOTHING;
        let output = ctx.run(RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = tab_row(ui, &mut current, &tabs);
                // All tabs share the same vertical extent, so the union response's
                // bottom edge equals the active tab's own rect bottom.
                row_rect = resp.rect;
            });
        });

        fn find_accent_underline_y(shape: &egui::Shape) -> Option<f32> {
            match shape {
                egui::Shape::LineSegment { points, stroke } => {
                    let is_accent_underline = (stroke.width - 2.0_f32).abs() < 1e-4
                        && stroke.color == egui::epaint::ColorMode::Solid(theme::ACCENT)
                        && (points[0].y - points[1].y).abs() < 1e-4;
                    is_accent_underline.then(|| points[0].y)
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(find_accent_underline_y),
                _ => None,
            }
        }

        let underline_y = output
            .shapes
            .iter()
            .find_map(|clipped| find_accent_underline_y(&clipped.shape));

        assert!(
            underline_y.is_some(),
            "Active tab underline stroke (2px theme::ACCENT line segment) should be painted"
        );

        // Placement: fully inside the tab rect — see the comment in `tab_row`
        // above the stroke. The chrome regression test
        // (`chrome::tests::titlebar_active_tab_underline_is_visible`) additionally
        // proves the stroke lands inside the real 30px titlebar's clip rect.
        let expected_y = row_rect.bottom() - 2.0_f32;
        assert!(
            (underline_y.unwrap() - expected_y).abs() < 1e-3,
            "underline should sit at rect.bottom() - 2.0 (got {}, expected {})",
            underline_y.unwrap(),
            expected_y
        );
    }
}

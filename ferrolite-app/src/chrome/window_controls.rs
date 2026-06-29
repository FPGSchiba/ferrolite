//! Window control buttons (minimize / maximize-restore / close) for the
//! borderless title bar, plus the pure action->command mapping.

use crate::theme;
use egui::{pos2, Color32, Painter, Pos2, Rect, Sense, Stroke, Ui, Vec2, ViewportCommand};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    Minimize,
    ToggleMaximize,
    Close,
}

/// Map a control action to the egui viewport command to send.
pub fn command(action: WindowAction, is_maximized: bool) -> ViewportCommand {
    match action {
        WindowAction::Minimize => ViewportCommand::Minimized(true),
        WindowAction::ToggleMaximize => ViewportCommand::Maximized(!is_maximized),
        WindowAction::Close => ViewportCommand::Close,
    }
}

pub const BTN_W: f32 = 44.0;

/// Render the three window-control buttons right-to-left (close is rightmost).
/// Icons are drawn as vector shapes (not font glyphs) so they render identically
/// regardless of font coverage; the maximize button shows a single square when
/// the window is restored and an offset double-square ("restore") when maximized.
/// Returns the action whose button was clicked this frame, if any.
pub fn controls_ui(ui: &mut Ui, is_maximized: bool) -> Option<WindowAction> {
    let mut clicked = None;
    // Order matters in a right_to_left layout: first added sits rightmost.
    for action in [
        WindowAction::Close,
        WindowAction::ToggleMaximize,
        WindowAction::Minimize,
    ] {
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(BTN_W, ui.available_height()), Sense::click());
        let hover = resp.hovered();
        if hover {
            let bg = if action == WindowAction::Close {
                theme::SEMANTIC_RED
            } else {
                theme::BG_TOOLBAR
            };
            ui.painter().rect_filled(rect, 0.0, bg);
        }
        let fg = if hover && action == WindowAction::Close {
            Color32::WHITE
        } else {
            theme::TEXT_DIM
        };
        paint_icon(ui.painter(), rect.center(), action, is_maximized, fg);
        if resp.clicked() {
            clicked = Some(action);
        }
    }
    clicked
}

/// Draw a control's icon centred at `c` with the given colour.
fn paint_icon(
    painter: &Painter,
    c: Pos2,
    action: WindowAction,
    is_maximized: bool,
    color: Color32,
) {
    let s = Stroke::new(1.2, color);
    let r = 5.0; // half-extent → ~10px icon box
    let square = |min: Pos2| Rect::from_min_size(min, Vec2::splat(2.0 * r - 2.0));
    match action {
        WindowAction::Minimize => {
            painter.line_segment([pos2(c.x - r, c.y), pos2(c.x + r, c.y)], s);
        }
        WindowAction::ToggleMaximize => {
            if is_maximized {
                // "restore": a back square (up-right) and a front square (down-left).
                painter.rect_stroke(square(pos2(c.x - r + 2.0, c.y - r)), 0.0, s);
                painter.rect_stroke(square(pos2(c.x - r, c.y - r + 2.0)), 0.0, s);
            } else {
                painter.rect_stroke(
                    Rect::from_min_size(pos2(c.x - r, c.y - r), Vec2::splat(2.0 * r)),
                    0.0,
                    s,
                );
            }
        }
        WindowAction::Close => {
            painter.line_segment([pos2(c.x - r, c.y - r), pos2(c.x + r, c.y + r)], s);
            painter.line_segment([pos2(c.x - r, c.y + r), pos2(c.x + r, c.y - r)], s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimize_maps_to_minimized_true() {
        assert!(matches!(
            command(WindowAction::Minimize, false),
            ViewportCommand::Minimized(true)
        ));
    }

    #[test]
    fn close_maps_to_close() {
        assert!(matches!(
            command(WindowAction::Close, true),
            ViewportCommand::Close
        ));
    }

    #[test]
    fn toggle_maximize_flips_both_states() {
        assert!(matches!(
            command(WindowAction::ToggleMaximize, false),
            ViewportCommand::Maximized(true)
        ));
        assert!(matches!(
            command(WindowAction::ToggleMaximize, true),
            ViewportCommand::Maximized(false)
        ));
    }
}

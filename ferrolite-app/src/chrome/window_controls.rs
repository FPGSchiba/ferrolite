//! Window control buttons (minimize / maximize-restore / close) for the
//! borderless title bar, plus the pure action->command mapping.

use crate::theme;
use egui::{Align2, Color32, FontId, Sense, Ui, Vec2, ViewportCommand};

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
/// Returns the action whose button was clicked this frame, if any.
pub fn controls_ui(ui: &mut Ui) -> Option<WindowAction> {
    let mut clicked = None;
    // Order matters in a right_to_left layout: first added sits rightmost.
    for (action, glyph) in [
        (WindowAction::Close, "\u{2715}"),          // ✕
        (WindowAction::ToggleMaximize, "\u{25A1}"), // □
        (WindowAction::Minimize, "\u{2013}"),       // –
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
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(13.0),
            fg,
        );
        if resp.clicked() {
            clicked = Some(action);
        }
    }
    clicked
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

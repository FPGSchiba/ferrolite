//! The Settings window's Keyboard tab. Stub for the 6.1 shell commit —
//! filled in by the following commit (keyboard rebinding tab).

use super::Settings;

/// Draw the Keyboard tab. Returns `true` if any binding changed this frame.
pub(super) fn draw(ui: &mut egui::Ui, _settings: &mut Settings) -> bool {
    ui.heading("Keyboard");
    ui.label("Keyboard rebinding coming up.");
    false
}

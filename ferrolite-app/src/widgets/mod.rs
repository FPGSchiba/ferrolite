pub mod chips;
pub mod color_wheel;
pub mod curve;
pub mod slider;
pub mod tabs;
pub mod tool_button;
#[allow(unused_imports)]
pub use chips::{segmented_control, SegmentedControl};
pub use color_wheel::{color_wheel, WheelEdit};
pub use slider::EguiSlider;
#[allow(unused_imports)]
pub use tabs::{tab_row, TabRow};
pub(crate) use tool_button::tool_button;

use egui::Color32;

/// Draw the per-control "reset" glyph (`icons::RESET`, a counter-clockwise
/// arrow) centered at `center`, sized to visually match the radius `r` the
/// old hand-built arc+arrowhead used.
///
/// Shared by `EguiSlider` and any other editable control that needs a
/// per-control reset affordance (see the design rule in the root `CLAUDE.md`).
pub(crate) fn draw_reset_arrow(
    painter: &egui::Painter,
    center: egui::Pos2,
    r: f32,
    color: Color32,
) {
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        crate::icons::RESET,
        crate::icons::font(r * 2.2), // tuned to match the reset column's prior visual size
        color,
    );
}

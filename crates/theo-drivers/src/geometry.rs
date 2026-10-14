//! Pixel sizes and positions shared by the panel, touch and view code.
//!
//! - [`PanelSize`]: width and height in pixels, of the panel or of a view
//!   drawn on it.
//! - [`TouchPoint`]: a pixel position from the top-left corner, in the
//!   frame of a panel or of a view.

/// Width and height in pixels, of the panel or of a view drawn on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelSize {
    /// Columns; positions in this frame have X below it.
    pub width: u16,
    /// Rows; positions in this frame have Y below it.
    pub height: u16,
}

/// Pixel position from the top-left corner of a panel or view.
///
/// The touch driver reports panel positions;
/// [`crate::Rotation::panel_to_view`] maps them into the view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchPoint {
    /// Column, below the frame's width.
    pub x: u16,
    /// Row, below the frame's height.
    pub y: u16,
}

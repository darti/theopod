//! Screen orientation from gravity, and the matching view transform.
//!
//! - [`AxisMap`], [`Axis`]: how the accelerometer axes lie on the panel;
//!   a board constant, checked by [`AxisMap::is_rotation`].
//! - [`PanelAcceleration`]: an acceleration on the panel's axes.
//! - [`OrientationDetector`]: picks the [`Rotation`] that keeps the view
//!   upright, with a dead band and a stability count against flicker.
//! - [`Rotation`]: quarter turns of the view on the panel, with the view
//!   size and the panel-to-view mapping of touch points.
//!
//! Rotations follow Slint's `RenderingRotation`: [`Rotation::Deg90`] turns
//! the view a quarter turn clockwise on the panel, so the view's top edge
//! runs along the panel's right edge.

use crate::{
    geometry::{PanelSize, TouchPoint},
    qmi8658::Acceleration,
};

/// One accelerometer axis, possibly reversed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Sensor +X.
    X,
    /// Sensor +Y.
    Y,
    /// Sensor +Z.
    Z,
    /// Sensor -X.
    NegX,
    /// Sensor -Y.
    NegY,
    /// Sensor -Z.
    NegZ,
}

impl Axis {
    /// Unit vector of this axis in sensor coordinates.
    const fn vector(self) -> [i32; 3] {
        match self {
            Self::X => [1, 0, 0],
            Self::Y => [0, 1, 0],
            Self::Z => [0, 0, 1],
            Self::NegX => [-1, 0, 0],
            Self::NegY => [0, -1, 0],
            Self::NegZ => [0, 0, -1],
        }
    }

    fn read(self, acceleration: Acceleration) -> i16 {
        match self {
            Self::X => acceleration.x,
            Self::Y => acceleration.y,
            Self::Z => acceleration.z,
            Self::NegX => acceleration.x.saturating_neg(),
            Self::NegY => acceleration.y.saturating_neg(),
            Self::NegZ => acceleration.z.saturating_neg(),
        }
    }
}

/// Sensor axis pointing along each panel axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisMap {
    /// Toward the panel's right edge.
    pub right: Axis,
    /// Toward the panel's bottom edge.
    pub down: Axis,
    /// Out of the screen, toward the viewer.
    pub out: Axis,
}

impl AxisMap {
    /// Tells whether a sensor can be mounted this way: a proper rotation.
    ///
    /// Each sensor axis must be used once, without mirroring space. Right,
    /// down and out form a left-handed frame while the sensor's X, Y, Z are
    /// right-handed, so the matrix of the map has determinant -1. A typo in
    /// a measured map (a repeated axis, one sign flipped) fails this check;
    /// a wrong but physical map does not.
    #[must_use]
    pub const fn is_rotation(&self) -> bool {
        let [a, b, c] = self.right.vector();
        let [d, e, f] = self.down.vector();
        let [g, h, i] = self.out.vector();
        a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g) == -1
    }

    /// Expresses a sensor sample on the panel's axes.
    #[must_use]
    pub fn apply(&self, acceleration: Acceleration) -> PanelAcceleration {
        PanelAcceleration {
            right: self.right.read(acceleration),
            down: self.down.read(acceleration),
            out: self.out.read(acceleration),
        }
    }
}

/// Acceleration on the panel's axes, in [`Acceleration::ONE_G`] units.
///
/// At rest an accelerometer reads the reaction to gravity: this vector
/// points up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelAcceleration {
    /// Toward the panel's right edge.
    pub right: i16,
    /// Toward the panel's bottom edge.
    pub down: i16,
    /// Out of the screen.
    pub out: i16,
}

/// Quarter turn of the view on the panel, clockwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rotation {
    /// View upright on the panel (portrait).
    #[default]
    Deg0,
    /// View top along the panel's right edge (landscape).
    Deg90,
    /// View upside down (portrait).
    Deg180,
    /// View top along the panel's left edge (landscape).
    Deg270,
}

impl Rotation {
    /// Size of the view on a `panel`: swapped for quarter turns.
    #[must_use]
    pub fn view_size(self, panel: PanelSize) -> PanelSize {
        match self {
            Self::Deg0 | Self::Deg180 => panel,
            Self::Deg90 | Self::Deg270 => PanelSize {
                width: panel.height,
                height: panel.width,
            },
        }
    }

    /// Maps a point on the `panel` to the view drawn on it.
    ///
    /// `point` must lie on the panel; the result then lies in
    /// [`Self::view_size`].
    #[must_use]
    pub fn panel_to_view(self, point: TouchPoint, panel: PanelSize) -> TouchPoint {
        let flip_x = |x: u16| panel.width.saturating_sub(1).saturating_sub(x);
        let flip_y = |y: u16| panel.height.saturating_sub(1).saturating_sub(y);
        match self {
            Self::Deg0 => point,
            Self::Deg90 => TouchPoint {
                x: point.y,
                y: flip_x(point.x),
            },
            Self::Deg180 => TouchPoint {
                x: flip_x(point.x),
                y: flip_y(point.y),
            },
            Self::Deg270 => TouchPoint {
                x: flip_y(point.y),
                y: point.x,
            },
        }
    }

    /// Rotation that keeps the view upright when `up` points that way.
    fn for_up(up: PanelAcceleration) -> Option<Self> {
        let right = i64::from(up.right);
        let down = i64::from(up.down);
        let out = i64::from(up.out);
        let in_plane = right * right + down * down;
        let total = in_plane + out * out;
        let half_g = i64::from(Acceleration::ONE_G / 2);
        // Free fall: under half of gravity, no usable direction.
        if total < half_g * half_g {
            return None;
        }
        // Lying flat: less than half of gravity in the screen plane (tilted
        // under 30°), so no edge is clearly up.
        if in_plane * 4 < total {
            return None;
        }
        // Dead band: one axis must exceed the other by 3:2 (about 56°).
        let (abs_right, abs_down) = (right.abs(), down.abs());
        if abs_right * 2 >= abs_down * 3 {
            Some(if right > 0 { Self::Deg90 } else { Self::Deg270 })
        } else if abs_down * 2 >= abs_right * 3 {
            Some(if down < 0 { Self::Deg0 } else { Self::Deg180 })
        } else {
            None
        }
    }
}

/// Tracks the view rotation from successive gravity samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrientationDetector {
    rotation: Rotation,
    /// Rotation seen in the last samples, and how many in a row.
    candidate: Option<(Rotation, u8)>,
}

impl OrientationDetector {
    /// Consecutive samples a new rotation needs before it is adopted.
    pub const STABLE_SAMPLES: u8 = 3;

    /// Starts at `rotation`.
    #[must_use]
    pub fn new(rotation: Rotation) -> Self {
        Self {
            rotation,
            candidate: None,
        }
    }

    /// Current rotation.
    #[must_use]
    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    /// Feeds one sample; returns the new rotation when it changes.
    ///
    /// Samples lying flat, in free fall or near a diagonal keep the current
    /// rotation and reset the stability count.
    pub fn update(&mut self, up: PanelAcceleration) -> Option<Rotation> {
        let Some(seen) = Rotation::for_up(up).filter(|&seen| seen != self.rotation) else {
            self.candidate = None;
            return None;
        };
        let count = match self.candidate {
            Some((candidate, count)) if candidate == seen => count.saturating_add(1),
            _ => 1,
        };
        if count >= Self::STABLE_SAMPLES {
            self.rotation = seen;
            self.candidate = None;
            Some(seen)
        } else {
            self.candidate = Some((seen, count));
            None
        }
    }
}

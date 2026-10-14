use speculoos::prelude::*;

use crate::{
    geometry::{PanelSize, TouchPoint},
    orientation::{Axis, AxisMap, OrientationDetector, PanelAcceleration, Rotation},
    qmi8658::Acceleration,
};

const G: i16 = Acceleration::ONE_G;
const PANEL: PanelSize = PanelSize {
    width: 368,
    height: 448,
};

/// Up vector (reaction to gravity) in panel axes.
fn up(right: i16, down: i16, out: i16) -> PanelAcceleration {
    PanelAcceleration { right, down, out }
}

/// Upright portrait: the panel's top edge points up.
const TOP_UP: PanelAcceleration = PanelAcceleration {
    right: 0,
    down: -G,
    out: 0,
};
const RIGHT_UP: PanelAcceleration = PanelAcceleration {
    right: G,
    down: 0,
    out: 0,
};
const BOTTOM_UP: PanelAcceleration = PanelAcceleration {
    right: 0,
    down: G,
    out: 0,
};
const LEFT_UP: PanelAcceleration = PanelAcceleration {
    right: -G,
    down: 0,
    out: 0,
};

/// Feeds `sample` until the detector's stability count is reached.
fn settle(detector: &mut OrientationDetector, sample: PanelAcceleration) -> Option<Rotation> {
    let mut changed = None;
    for _ in 0..OrientationDetector::STABLE_SAMPLES {
        changed = detector.update(sample).or(changed);
    }
    changed
}

#[test]
fn axis_map_identity_keeps_axes() {
    let map = AxisMap {
        right: Axis::X,
        down: Axis::Y,
        out: Axis::Z,
    };

    let mapped = map.apply(Acceleration { x: 1, y: 2, z: 3 });

    assert_that!(mapped).is_equal_to(up(1, 2, 3));
}

#[test]
fn axis_map_swaps_and_negates_axes() {
    let map = AxisMap {
        right: Axis::NegY,
        down: Axis::X,
        out: Axis::NegZ,
    };

    let mapped = map.apply(Acceleration { x: 1, y: 2, z: 3 });

    assert_that!(mapped).is_equal_to(up(-2, 1, -3));
}

#[test]
fn axis_map_negating_most_negative_value_saturates() {
    let map = AxisMap {
        right: Axis::NegX,
        down: Axis::Y,
        out: Axis::Z,
    };

    let mapped = map.apply(Acceleration {
        x: i16::MIN,
        y: 0,
        z: 0,
    });

    assert_that!(mapped.right).is_equal_to(i16::MAX);
}

#[test]
fn update_each_edge_up_selects_matching_rotation() {
    for (sample, rotation) in [
        (RIGHT_UP, Rotation::Deg90),
        (BOTTOM_UP, Rotation::Deg180),
        (LEFT_UP, Rotation::Deg270),
    ] {
        let mut detector = OrientationDetector::new(Rotation::Deg0);

        let changed = settle(&mut detector, sample);

        assert_that!(changed).is_equal_to(Some(rotation));
        assert_that!(detector.rotation()).is_equal_to(rotation);
    }
}

#[test]
fn update_back_to_top_up_selects_deg0() {
    let mut detector = OrientationDetector::new(Rotation::Deg90);

    let changed = settle(&mut detector, TOP_UP);

    assert_that!(changed).is_equal_to(Some(Rotation::Deg0));
}

#[test]
fn update_current_orientation_reports_no_change() {
    let mut detector = OrientationDetector::new(Rotation::Deg0);

    let changed = settle(&mut detector, TOP_UP);

    assert_that!(changed).is_none();
}

#[test]
fn update_needs_stable_samples_before_switching() {
    let mut detector = OrientationDetector::new(Rotation::Deg0);

    for _ in 1..OrientationDetector::STABLE_SAMPLES {
        assert_that!(detector.update(RIGHT_UP)).is_none();
    }

    assert_that!(detector.update(RIGHT_UP)).is_equal_to(Some(Rotation::Deg90));
}

#[test]
fn update_interrupted_candidate_restarts_count() {
    let mut detector = OrientationDetector::new(Rotation::Deg0);
    for _ in 1..OrientationDetector::STABLE_SAMPLES {
        detector.update(RIGHT_UP);
    }

    detector.update(TOP_UP);
    let changed = detector.update(RIGHT_UP);

    assert_that!(changed).is_none();
    assert_that!(detector.rotation()).is_equal_to(Rotation::Deg0);
}

#[test]
fn update_different_candidate_restarts_count() {
    let mut detector = OrientationDetector::new(Rotation::Deg0);
    for _ in 1..OrientationDetector::STABLE_SAMPLES {
        detector.update(RIGHT_UP);
    }

    let changed = detector.update(LEFT_UP);

    assert_that!(changed).is_none();
    assert_that!(detector.rotation()).is_equal_to(Rotation::Deg0);
}

#[test]
fn update_near_diagonal_keeps_current_rotation() {
    // 50° from upright toward the right edge: inside the dead band.
    let sample = up(12_551, -10_532, 0);
    let mut detector = OrientationDetector::new(Rotation::Deg0);

    let changed = settle(&mut detector, sample);

    assert_that!(changed).is_none();
}

#[test]
fn update_past_dead_band_switches() {
    // 60° from upright toward the right edge.
    let sample = up(14_189, -8_192, 0);
    let mut detector = OrientationDetector::new(Rotation::Deg0);

    let changed = settle(&mut detector, sample);

    assert_that!(changed).is_equal_to(Some(Rotation::Deg90));
}

#[test]
fn update_exactly_three_to_two_switches() {
    let mut detector = OrientationDetector::new(Rotation::Deg0);

    let changed = settle(&mut detector, up(12_288, -8_192, 0));

    assert_that!(changed).is_equal_to(Some(Rotation::Deg90));
}

#[test]
fn update_just_under_three_to_two_keeps_current_rotation() {
    let mut detector = OrientationDetector::new(Rotation::Deg0);

    let changed = settle(&mut detector, up(12_287, -8_192, 0));

    assert_that!(changed).is_none();
}

#[test]
fn update_tilted_35_degrees_from_flat_switches() {
    // Right edge raised 35° from lying flat, screen up.
    let mut detector = OrientationDetector::new(Rotation::Deg0);

    let changed = settle(&mut detector, up(9_397, 0, 13_421));

    assert_that!(changed).is_equal_to(Some(Rotation::Deg90));
}

#[test]
fn update_tilted_25_degrees_from_flat_keeps_current_rotation() {
    let mut detector = OrientationDetector::new(Rotation::Deg0);

    let changed = settle(&mut detector, up(6_924, 0, 14_849));

    assert_that!(changed).is_none();
}

#[test]
fn update_more_than_half_gravity_switches() {
    // Upright at 0.55 g.
    let mut detector = OrientationDetector::new(Rotation::Deg90);

    let changed = settle(&mut detector, up(0, -9_011, 0));

    assert_that!(changed).is_equal_to(Some(Rotation::Deg0));
}

#[test]
fn update_less_than_half_gravity_keeps_current_rotation() {
    // Upright at 0.45 g.
    let mut detector = OrientationDetector::new(Rotation::Deg90);

    let changed = settle(&mut detector, up(0, -7_373, 0));

    assert_that!(changed).is_none();
}

#[test]
fn update_lying_flat_keeps_current_rotation() {
    // Screen facing up, slightly tilted toward the right edge.
    let sample = up(4_000, 0, G);
    let mut detector = OrientationDetector::new(Rotation::Deg0);

    let changed = settle(&mut detector, sample);

    assert_that!(changed).is_none();
}

#[test]
fn update_free_fall_keeps_current_rotation() {
    let sample = up(2_000, 0, 0);
    let mut detector = OrientationDetector::new(Rotation::Deg0);

    let changed = settle(&mut detector, sample);

    assert_that!(changed).is_none();
}

#[test]
fn update_extreme_values_do_not_overflow() {
    let diagonal = up(i16::MIN, i16::MIN, i16::MIN);
    let mut detector = OrientationDetector::new(Rotation::Deg0);

    settle(&mut detector, diagonal);
    let left = settle(&mut detector, up(i16::MIN, 0, 0));
    let right = settle(&mut detector, up(i16::MAX, 0, i16::MIN));

    assert_that!(left).is_equal_to(Some(Rotation::Deg270));
    assert_that!(right).is_equal_to(Some(Rotation::Deg90));
}

#[test]
fn view_size_quarter_turns_swap_panel_size() {
    let landscape = PanelSize {
        width: 448,
        height: 368,
    };

    assert_that!(Rotation::Deg0.view_size(PANEL)).is_equal_to(PANEL);
    assert_that!(Rotation::Deg90.view_size(PANEL)).is_equal_to(landscape);
    assert_that!(Rotation::Deg180.view_size(PANEL)).is_equal_to(PANEL);
    assert_that!(Rotation::Deg270.view_size(PANEL)).is_equal_to(landscape);
}

#[test]
fn panel_to_view_maps_panel_corners_to_view_corners() {
    let top_left = TouchPoint { x: 0, y: 0 };
    let bottom_right = TouchPoint { x: 367, y: 447 };
    let point = |x, y| TouchPoint { x, y };

    // Deg90 draws the view's top edge along the panel's right edge.
    assert_that!(Rotation::Deg0.panel_to_view(top_left, PANEL)).is_equal_to(point(0, 0));
    assert_that!(Rotation::Deg90.panel_to_view(top_left, PANEL)).is_equal_to(point(0, 367));
    assert_that!(Rotation::Deg90.panel_to_view(bottom_right, PANEL)).is_equal_to(point(447, 0));
    assert_that!(Rotation::Deg180.panel_to_view(top_left, PANEL)).is_equal_to(point(367, 447));
    assert_that!(Rotation::Deg270.panel_to_view(top_left, PANEL)).is_equal_to(point(447, 0));
    assert_that!(Rotation::Deg270.panel_to_view(bottom_right, PANEL)).is_equal_to(point(0, 367));
}

/// Panel pixel where Slint 1.18 draws view pixel `view` with `rotation`.
///
/// Transcribes `Transform for Point2D` in `i-slint-renderer-software`
/// (mirror, then transpose, in the view's frame); checked against Slint
/// on the host and on the board.
fn slint_view_to_panel(rotation: Rotation, view: TouchPoint) -> TouchPoint {
    let size = rotation.view_size(PANEL);
    let (mirror_width, mirror_height, transpose) = match rotation {
        Rotation::Deg0 => (false, false, false),
        Rotation::Deg90 => (false, true, true),
        Rotation::Deg180 => (true, true, false),
        Rotation::Deg270 => (true, false, true),
    };
    let x = if mirror_width {
        size.width - 1 - view.x
    } else {
        view.x
    };
    let y = if mirror_height {
        size.height - 1 - view.y
    } else {
        view.y
    };
    if transpose {
        TouchPoint { x: y, y: x }
    } else {
        TouchPoint { x, y }
    }
}

#[test]
fn panel_to_view_inverts_slint_transform_for_every_pixel() {
    for rotation in [
        Rotation::Deg0,
        Rotation::Deg90,
        Rotation::Deg180,
        Rotation::Deg270,
    ] {
        let view = rotation.view_size(PANEL);
        for y in 0..view.height {
            for x in 0..view.width {
                let point = TouchPoint { x, y };
                let panel = slint_view_to_panel(rotation, point);

                assert!(panel.x < PANEL.width && panel.y < PANEL.height);
                assert_eq!(rotation.panel_to_view(panel, PANEL), point, "{rotation:?}");
            }
        }
    }
}

/// Every signed axis.
const AXES: [Axis; 6] = [
    Axis::X,
    Axis::Y,
    Axis::Z,
    Axis::NegX,
    Axis::NegY,
    Axis::NegZ,
];

#[test]
fn axis_map_is_rotation_accepts_board_map() {
    // `IMU_AXES` measured on the board (`src/device/mod.rs`).
    let map = AxisMap {
        right: Axis::Y,
        down: Axis::NegX,
        out: Axis::NegZ,
    };

    assert_that!(map.is_rotation()).is_true();
}

#[test]
fn axis_map_is_rotation_rejects_mirror() {
    // Right, down and out form a left-handed frame: mapping them onto the
    // sensor's right-handed X, Y, Z mirrors the space.
    let map = AxisMap {
        right: Axis::X,
        down: Axis::Y,
        out: Axis::Z,
    };

    assert_that!(map.is_rotation()).is_false();
}

#[test]
fn axis_map_is_rotation_rejects_repeated_axis() {
    let map = AxisMap {
        right: Axis::Y,
        down: Axis::NegY,
        out: Axis::Z,
    };

    assert_that!(map.is_rotation()).is_false();
}

#[test]
fn axis_map_is_rotation_accepts_exactly_the_24_mountings() {
    let mut rotations = 0;
    for right in AXES {
        for down in AXES {
            for out in AXES {
                let map = AxisMap { right, down, out };
                if map.is_rotation() {
                    rotations += 1;
                }
            }
        }
    }

    // A cube has 24 orientations; mirrors and repeated axes are rejected.
    assert_that!(rotations).is_equal_to(24);
}

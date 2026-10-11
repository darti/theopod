use speculoos::prelude::*;

use crate::{
    co5300::{Co5300, Error, PanelGeometry},
    panel::Panel,
    qspi::Lanes,
    test_support::{BusFault, NoDelay, RecordingBus, Transfer, block_on},
};

const GEOMETRY: PanelGeometry = PanelGeometry {
    width: 368,
    height: 448,
    column_offset: 16,
};

fn panel() -> Co5300<RecordingBus> {
    Co5300::new(RecordingBus::default(), GEOMETRY).unwrap()
}

fn assert_invalid_geometry(geometry: PanelGeometry) {
    let result = Co5300::new(RecordingBus::default(), geometry);

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidGeometry { geometry }));
}

#[test]
fn new_odd_column_offset_returns_invalid_geometry() {
    assert_invalid_geometry(PanelGeometry {
        column_offset: 15,
        ..GEOMETRY
    });
}

#[test]
fn new_odd_width_returns_invalid_geometry() {
    assert_invalid_geometry(PanelGeometry {
        width: 367,
        ..GEOMETRY
    });
}

#[test]
fn new_odd_height_returns_invalid_geometry() {
    assert_invalid_geometry(PanelGeometry {
        height: 447,
        ..GEOMETRY
    });
}

#[test]
fn new_zero_width_returns_invalid_geometry() {
    assert_invalid_geometry(PanelGeometry {
        width: 0,
        ..GEOMETRY
    });
}

#[test]
fn new_zero_height_returns_invalid_geometry() {
    assert_invalid_geometry(PanelGeometry {
        height: 0,
        ..GEOMETRY
    });
}

#[test]
fn new_offset_overflowing_u16_returns_invalid_geometry() {
    assert_invalid_geometry(PanelGeometry {
        column_offset: u16::MAX - 1,
        ..GEOMETRY
    });
}

#[test]
fn new_frame_bytes_overflowing_u32_returns_invalid_geometry() {
    assert_invalid_geometry(PanelGeometry {
        width: 65_534,
        height: 65_534,
        column_offset: 0,
    });
}

#[test]
fn size_returns_visible_geometry() {
    assert_that!(panel().size()).is_equal_to((368, 448));
}

// Regression (driver half): register writes must use opcode 0x02 with
// single-lane data. Sending them on quad lanes kept the panel black. The
// single-line command/address phases are the `QspiBus` implementation's job
// (src/device/esp_qspi.rs) and are only verified on hardware.
#[test]
fn init_sends_every_register_single_line_with_opcode_0x02() {
    let mut panel = panel();

    block_on(panel.init(&mut NoDelay)).unwrap();

    let transfers = &panel.release().transfers;
    assert_that!(transfers.is_empty()).is_false();
    for transfer in transfers {
        assert_that!(transfer.opcode).is_equal_to(0x02);
        assert_that!(transfer.lanes).is_equal_to(Lanes::Single);
        assert_that!(transfer.address & 0xFF00_00FF).is_equal_to(0);
    }
}

#[test]
fn init_sends_waveshare_v2_sequence() {
    let mut panel = panel();

    block_on(panel.init(&mut NoDelay)).unwrap();

    assert_that!(panel.release().transfers).is_equal_to(vec![
        Transfer::register(0x01, &[]),
        Transfer::register(0x11, &[]),
        Transfer::register(0xFE, &[0x00]),
        Transfer::register(0xC4, &[0x80]),
        Transfer::register(0x3A, &[0x55]),
        Transfer::register(0x53, &[0x20]),
        Transfer::register(0x63, &[0xFF]),
        Transfer::register(0x29, &[]),
        Transfer::register(0x51, &[0xD0]),
        Transfer::register(0x58, &[0x00]),
        Transfer::register(0x36, &[0x00]),
        Transfer::register(0x20, &[]),
    ]);
}

#[test]
fn init_bus_failure_returns_bus_error_and_stops() {
    let mut panel = Co5300::new(RecordingBus::failing(2), GEOMETRY).unwrap();

    let result = block_on(panel.init(&mut NoDelay));

    assert_that!(result.err()).is_equal_to(Some(Error::Bus(BusFault)));
    assert_that!(panel.release().transfers).has_length(2);
}

#[test]
fn begin_area_applies_column_offset_and_inclusive_ends() {
    let mut panel = panel();

    panel.begin_area(0..2, 4..6).unwrap();

    assert_that!(panel.release().transfers).is_equal_to(vec![
        Transfer::register(0x2A, &[0x00, 0x10, 0x00, 0x11]),
        Transfer::register(0x2B, &[0x00, 0x04, 0x00, 0x05]),
        Transfer::register(0x2C, &[]),
    ]);
}

#[test]
fn begin_area_full_panel_spans_offset_columns() {
    let mut panel = panel();

    panel.begin_area(0..368, 0..448).unwrap();

    let transfers = panel.release().transfers;
    // Columns 16..=383 and rows 0..=447.
    assert_that!(transfers[0]).is_equal_to(Transfer::register(0x2A, &[0x00, 0x10, 0x01, 0x7F]));
    assert_that!(transfers[1]).is_equal_to(Transfer::register(0x2B, &[0x00, 0x00, 0x01, 0xBF]));
}

// Regression: the CO5300 silently ignores areas not aligned on 2x2 blocks,
// which left the screen black when lines were sent one row at a time.
#[test]
fn begin_area_single_row_returns_invalid_area() {
    let mut panel = panel();

    let result = panel.begin_area(0..368, 4..5);

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidArea { x: 0..368, y: 4..5 }));
    assert_that!(panel.release().transfers.is_empty()).is_true();
}

#[test]
fn begin_area_odd_start_returns_invalid_area() {
    let mut panel = panel();

    let result = panel.begin_area(1..3, 0..2);

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidArea { x: 1..3, y: 0..2 }));
}

#[test]
fn begin_area_odd_column_count_returns_invalid_area() {
    let mut panel = panel();

    let result = panel.begin_area(0..3, 0..2);

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidArea { x: 0..3, y: 0..2 }));
}

#[test]
fn begin_area_empty_returns_invalid_area() {
    let mut panel = panel();

    let result = panel.begin_area(0..0, 0..2);

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidArea { x: 0..0, y: 0..2 }));
}

#[test]
fn begin_area_beyond_panel_returns_invalid_area() {
    let mut panel = panel();

    let result = panel.begin_area(0..370, 0..2);

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidArea { x: 0..370, y: 0..2 }));
}

#[test]
fn begin_area_bus_failure_closes_previous_area() {
    let mut panel = Co5300::new(RecordingBus::failing(3), GEOMETRY).unwrap();
    panel.begin_area(0..2, 0..2).unwrap();

    let reopened = panel.begin_area(0..2, 0..2);
    let result = panel.write_pixels(&[0; 2]);

    assert_that!(reopened.err()).is_equal_to(Some(Error::Bus(BusFault)));
    assert_that!(result.err()).is_equal_to(Some(Error::PixelOverflow {
        remaining: 0,
        submitted: 2,
    }));
}

// Regression (driver half): pixel data must use opcode 0x32 with
// memory-write-continue and quad-lane data; see the register test above for
// the command/address phases.
#[test]
fn write_pixels_sends_quad_data_with_memory_write_continue() {
    let mut panel = panel();
    panel.begin_area(0..2, 0..2).unwrap();

    panel.write_pixels(&[0xF8, 0x00, 0x07, 0xE0]).unwrap();

    let transfers = panel.release().transfers;
    assert_that!(transfers.last().cloned())
        .is_some()
        .is_equal_to(Transfer::pixels(&[0xF8, 0x00, 0x07, 0xE0]));
}

#[test]
fn write_pixels_after_area_filled_returns_pixel_overflow() {
    let mut panel = panel();
    panel.begin_area(0..2, 0..2).unwrap();
    panel.write_pixels(&[0; 4]).unwrap();
    panel.write_pixels(&[0; 4]).unwrap();

    let result = panel.write_pixels(&[0; 2]);

    assert_that!(result.err()).is_equal_to(Some(Error::PixelOverflow {
        remaining: 0,
        submitted: 2,
    }));
}

#[test]
fn write_pixels_bus_failure_keeps_remaining_budget() {
    let mut panel = Co5300::new(RecordingBus::failing(3), GEOMETRY).unwrap();
    panel.begin_area(0..2, 0..2).unwrap();

    let failed = panel.write_pixels(&[0; 8]);
    let retried = panel.write_pixels(&[0; 8]);

    assert_that!(failed.err()).is_equal_to(Some(Error::Bus(BusFault)));
    assert_that!(retried).is_ok();
}

#[test]
fn write_pixels_past_area_end_returns_pixel_overflow() {
    let mut panel = panel();
    panel.begin_area(0..2, 0..2).unwrap();

    let result = panel.write_pixels(&[0; 10]);

    assert_that!(result.err()).is_equal_to(Some(Error::PixelOverflow {
        remaining: 8,
        submitted: 10,
    }));
}

#[test]
fn write_pixels_without_open_area_returns_pixel_overflow() {
    let mut panel = panel();

    let result = panel.write_pixels(&[0; 2]);

    assert_that!(result.err()).is_equal_to(Some(Error::PixelOverflow {
        remaining: 0,
        submitted: 2,
    }));
}

#[test]
fn write_pixels_odd_byte_count_returns_pixel_overflow() {
    let mut panel = panel();
    panel.begin_area(0..2, 0..2).unwrap();

    let result = panel.write_pixels(&[0; 3]);

    assert_that!(result.err()).is_equal_to(Some(Error::PixelOverflow {
        remaining: 8,
        submitted: 3,
    }));
}

#[test]
fn error_display_describes_invalid_area() {
    let message = Error::<BusFault>::InvalidArea { x: 1..3, y: 0..2 }.to_string();

    assert_that!(message.as_str()).is_equal_to("invalid area columns 1..3 rows 0..2");
}

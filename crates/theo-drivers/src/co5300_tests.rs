use speculoos::prelude::*;

use crate::{
    co5300::{Co5300, Error, PanelGeometry},
    panel::Panel,
    qspi::Lanes,
    test_support::{
        BusFault, NoDelay, RecordingBus, RecordingDelay, Transfer, block_on, counting_frame,
        poll_once,
    },
};

const GEOMETRY: PanelGeometry = PanelGeometry {
    width: 368,
    height: 448,
    column_offset: 16,
};

/// Bytes of a full [`GEOMETRY`] frame of RGB565.
const FRAME_LEN: usize = GEOMETRY.width as usize * GEOMETRY.height as usize * 2;

/// A panel whose bus frame counts up ([`counting_frame`]).
fn panel() -> Co5300<RecordingBus> {
    panel_on(RecordingBus::default())
}

/// A panel on `bus`, given a full [`GEOMETRY`] frame that counts up.
fn panel_on(bus: RecordingBus) -> Co5300<RecordingBus> {
    Co5300::new(bus.with_frame(counting_frame(FRAME_LEN)), GEOMETRY).unwrap()
}

/// A recording bus with a full [`GEOMETRY`] frame that counts up.
fn bus() -> RecordingBus {
    RecordingBus::default().with_frame(counting_frame(FRAME_LEN))
}

/// The bus holds a full [`GEOMETRY`] frame, which none of the invalid
/// geometries matches: the geometry check must come first.
fn assert_invalid_geometry(geometry: PanelGeometry) {
    let result = Co5300::new(bus(), geometry);

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidGeometry { geometry }));
}

#[test]
fn new_frame_shorter_than_geometry_returns_frame_size_mismatch() {
    let bus = RecordingBus::default().with_frame(counting_frame(FRAME_LEN - 2));

    let result = Co5300::new(bus, GEOMETRY);

    assert_that!(result.err()).is_equal_to(Some(Error::FrameSizeMismatch {
        expected: FRAME_LEN,
        actual: FRAME_LEN - 2,
    }));
}

#[test]
fn new_frame_longer_than_geometry_returns_frame_size_mismatch() {
    let bus = RecordingBus::default().with_frame(counting_frame(FRAME_LEN + 2));

    let result = Co5300::new(bus, GEOMETRY);

    assert_that!(result.err()).is_equal_to(Some(Error::FrameSizeMismatch {
        expected: FRAME_LEN,
        actual: FRAME_LEN + 2,
    }));
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

// Regression (f954dfb, driver half): register writes must use opcode 0x02 with
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
    let mut panel = panel_on(RecordingBus::failing(2));

    let result = block_on(panel.init(&mut NoDelay));

    assert_that!(result.err()).is_equal_to(Some(Error::Bus(BusFault)));
    assert_that!(panel.release().transfers).has_length(2);
}

#[test]
fn sleep_turns_display_off_then_enters_sleep() {
    let mut panel = panel();

    block_on(panel.sleep(&mut NoDelay)).unwrap();

    assert_that!(panel.release().transfers).is_equal_to(vec![
        Transfer::register(0x28, &[]),
        Transfer::register(0x10, &[]),
    ]);
}

#[test]
fn sleep_waits_for_controller_to_settle() {
    let mut panel = panel();
    let mut delay = RecordingDelay::default();

    block_on(panel.sleep(&mut delay)).unwrap();

    assert_that!(delay.delays_ms).is_equal_to(vec![120]);
}

#[test]
fn sleep_in_failure_returns_bus_error_without_settling() {
    let mut panel = panel_on(RecordingBus::failing(1));
    let mut delay = RecordingDelay::default();

    let result = block_on(panel.sleep(&mut delay));

    assert_that!(result.err()).is_equal_to(Some(Error::Bus(BusFault)));
    assert_that!(panel.release().transfers).is_equal_to(vec![Transfer::register(0x28, &[])]);
    assert_that!(delay.delays_ms).is_empty();
}

#[test]
fn sleep_bus_failure_returns_bus_error_and_stops() {
    let mut panel = panel_on(RecordingBus::failing(0));

    let result = block_on(panel.sleep(&mut NoDelay));

    assert_that!(result.err()).is_equal_to(Some(Error::Bus(BusFault)));
    assert_that!(panel.release().transfers).is_empty();
}

#[test]
fn begin_area_applies_column_offset_and_inclusive_ends() {
    let mut panel = panel();

    block_on(panel.begin_area(0..2, 4..6)).unwrap();

    assert_that!(panel.release().transfers).is_equal_to(vec![
        Transfer::register(0x2A, &[0x00, 0x10, 0x00, 0x11]),
        Transfer::register(0x2B, &[0x00, 0x04, 0x00, 0x05]),
        Transfer::register(0x2C, &[]),
    ]);
}

#[test]
fn begin_area_full_panel_spans_offset_columns() {
    let mut panel = panel();

    block_on(panel.begin_area(0..368, 0..448)).unwrap();

    let transfers = panel.release().transfers;
    // Columns 16..=383 and rows 0..=447.
    assert_that!(transfers[0]).is_equal_to(Transfer::register(0x2A, &[0x00, 0x10, 0x01, 0x7F]));
    assert_that!(transfers[1]).is_equal_to(Transfer::register(0x2B, &[0x00, 0x00, 0x01, 0xBF]));
}

// Regression (f954dfb): the CO5300 silently ignores areas not aligned on 2x2 blocks,
// which left the screen black when lines were sent one row at a time.
#[test]
fn begin_area_single_row_returns_invalid_area() {
    let mut panel = panel();

    let result = block_on(panel.begin_area(0..368, 4..5));

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidArea { x: 0..368, y: 4..5 }));
    assert_that!(panel.release().transfers.is_empty()).is_true();
}

#[test]
fn begin_area_odd_start_returns_invalid_area() {
    let mut panel = panel();

    let result = block_on(panel.begin_area(1..3, 0..2));

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidArea { x: 1..3, y: 0..2 }));
}

#[test]
fn begin_area_odd_column_count_returns_invalid_area() {
    let mut panel = panel();

    let result = block_on(panel.begin_area(0..3, 0..2));

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidArea { x: 0..3, y: 0..2 }));
}

#[test]
fn begin_area_empty_returns_invalid_area() {
    let mut panel = panel();

    let result = block_on(panel.begin_area(0..0, 0..2));

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidArea { x: 0..0, y: 0..2 }));
}

#[test]
fn begin_area_beyond_panel_returns_invalid_area() {
    let mut panel = panel();

    let result = block_on(panel.begin_area(0..370, 0..2));

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidArea { x: 0..370, y: 0..2 }));
}

#[test]
fn begin_area_bus_failure_closes_previous_area() {
    let mut panel = panel_on(RecordingBus::failing(3));
    block_on(panel.begin_area(0..2, 0..2)).unwrap();

    let reopened = block_on(panel.begin_area(0..2, 0..2));
    let result = block_on(panel.write_frame(0..2));

    assert_that!(reopened.err()).is_equal_to(Some(Error::Bus(BusFault)));
    assert_that!(result.err()).is_equal_to(Some(Error::PixelOverflow {
        remaining: 0,
        submitted: 2,
    }));
}

// Regression (f954dfb, driver half): pixel data must use opcode 0x32 with
// memory-write-continue and quad-lane data; see the register test above for
// the command/address phases.
#[test]
fn write_frame_sends_quad_data_with_memory_write_continue() {
    let mut frame = counting_frame(FRAME_LEN);
    frame[..4].copy_from_slice(&[0xF8, 0x00, 0x07, 0xE0]);
    let mut panel = Co5300::new(RecordingBus::default().with_frame(frame), GEOMETRY).unwrap();
    block_on(panel.begin_area(0..2, 0..2)).unwrap();

    block_on(panel.write_frame(0..4)).unwrap();

    let transfers = panel.release().transfers;
    assert_that!(transfers.last().cloned())
        .is_some()
        .is_equal_to(Transfer::pixels(&[0xF8, 0x00, 0x07, 0xE0]));
}

// `RecordingBus` accepts at most 16 bytes per transfer; its frame counts up.
#[test]
fn write_frame_longer_than_bus_limit_splits_into_transfers() {
    let mut panel = panel();
    block_on(panel.begin_area(0..4, 0..4)).unwrap();
    let pixels: Vec<u8> = (0..32).collect();

    block_on(panel.write_frame(0..pixels.len())).unwrap();

    let transfers = panel.release().transfers;
    assert_that!(transfers[3..].to_vec()).is_equal_to(vec![
        Transfer::pixels(&pixels[..16]),
        Transfer::pixels(&pixels[16..]),
    ]);
}

#[test]
fn write_frame_non_multiple_of_bus_limit_sends_remainder_last() {
    let mut panel = panel();
    block_on(panel.begin_area(0..4, 0..4)).unwrap();
    let pixels: Vec<u8> = (0..18).collect();

    block_on(panel.write_frame(0..pixels.len())).unwrap();

    let transfers = panel.release().transfers;
    assert_that!(transfers[3..].to_vec()).is_equal_to(vec![
        Transfer::pixels(&pixels[..16]),
        Transfer::pixels(&pixels[16..]),
    ]);
}

// Regression (t-3 review): a failed transfer may have reached the panel (a
// DMA underrun is only seen once every byte is out), so the area's budget
// is unknown, as after a cancellation.
#[test]
fn write_frame_failure_after_first_chunk_closes_the_area() {
    let mut panel = panel_on(RecordingBus::failing(4));
    block_on(panel.begin_area(0..4, 0..4)).unwrap();

    let failed = block_on(panel.write_frame(0..32));
    let result = block_on(panel.write_frame(0..2));

    assert_that!(failed.err()).is_equal_to(Some(Error::Bus(BusFault)));
    assert_that!(result.err()).is_equal_to(Some(Error::PixelOverflow {
        remaining: 0,
        submitted: 2,
    }));
}

#[test]
fn write_frame_after_area_filled_returns_pixel_overflow() {
    let mut panel = panel();
    block_on(panel.begin_area(0..2, 0..2)).unwrap();
    block_on(panel.write_frame(0..4)).unwrap();
    block_on(panel.write_frame(4..8)).unwrap();

    let result = block_on(panel.write_frame(0..2));

    assert_that!(result.err()).is_equal_to(Some(Error::PixelOverflow {
        remaining: 0,
        submitted: 2,
    }));
}

#[test]
fn write_frame_bus_failure_requires_new_area() {
    let mut panel = panel_on(RecordingBus::failing(3));
    block_on(panel.begin_area(0..2, 0..2)).unwrap();

    let failed = block_on(panel.write_frame(0..8));
    block_on(panel.begin_area(0..2, 0..2)).unwrap();
    let retried = block_on(panel.write_frame(0..8));

    assert_that!(failed.err()).is_equal_to(Some(Error::Bus(BusFault)));
    assert_that!(retried).is_ok();
}

// Regression (t-3): a cancelled pixel write may have sent part of its
// transfer, so the area's remaining budget is unknown.
#[test]
fn write_frame_cancelled_mid_transfer_requires_new_area() {
    let mut panel = panel_on(RecordingBus::pending(3));
    block_on(panel.begin_area(0..2, 0..2)).unwrap();

    let completed = poll_once(panel.write_frame(0..8));
    let continued = block_on(panel.write_frame(0..8));
    block_on(panel.begin_area(0..2, 0..2)).unwrap();
    let reopened = block_on(panel.write_frame(0..8));

    assert_that!(completed).is_false();
    assert_that!(continued.err()).is_equal_to(Some(Error::PixelOverflow {
        remaining: 0,
        submitted: 8,
    }));
    assert_that!(reopened).is_ok();
    // The refused write sent nothing: two areas of 3 registers, one pixel
    // write (the cancelled one was never recorded).
    assert_that!(panel.release().transfers).has_length(7);
}

#[test]
fn write_frame_past_area_end_returns_pixel_overflow() {
    let mut panel = panel();
    block_on(panel.begin_area(0..2, 0..2)).unwrap();

    let result = block_on(panel.write_frame(0..10));

    assert_that!(result.err()).is_equal_to(Some(Error::PixelOverflow {
        remaining: 8,
        submitted: 10,
    }));
}

#[test]
fn write_frame_without_open_area_returns_pixel_overflow() {
    let mut panel = panel();

    let result = block_on(panel.write_frame(0..2));

    assert_that!(result.err()).is_equal_to(Some(Error::PixelOverflow {
        remaining: 0,
        submitted: 2,
    }));
}

#[test]
fn write_frame_odd_byte_count_returns_pixel_overflow() {
    let mut panel = panel();
    block_on(panel.begin_area(0..2, 0..2)).unwrap();

    let result = block_on(panel.write_frame(0..3));

    assert_that!(result.err()).is_equal_to(Some(Error::PixelOverflow {
        remaining: 8,
        submitted: 3,
    }));
}

#[test]
fn error_display_describes_frame_size_mismatch() {
    let message = Error::<BusFault>::FrameSizeMismatch {
        expected: 8,
        actual: 6,
    }
    .to_string();

    assert_that!(message.as_str()).is_equal_to("frame holds 6 bytes, panel needs 8");
}

#[test]
fn write_frame_range_ending_at_the_frame_end_sends_the_last_bytes() {
    let mut panel = panel();
    block_on(panel.begin_area(0..2, 0..2)).unwrap();

    let result = block_on(panel.write_frame(FRAME_LEN - 8..FRAME_LEN));

    assert_that!(result).is_ok();
    let last = counting_frame(FRAME_LEN)[FRAME_LEN - 8..].to_vec();
    assert_that!(panel.release().transfers.last().cloned())
        .is_some()
        .is_equal_to(Transfer::pixels(&last));
}

#[test]
fn error_display_describes_invalid_area() {
    let message = Error::<BusFault>::InvalidArea { x: 1..3, y: 0..2 }.to_string();

    assert_that!(message.as_str()).is_equal_to("invalid area columns 1..3 rows 0..2");
}

// Regression (t-46 review): a reversed range read as empty, so the write
// silently succeeded without sending anything.
#[test]
#[expect(clippy::reversed_empty_ranges, reason = "the invalid input under test")]
fn write_frame_reversed_range_returns_invalid_range() {
    let mut panel = panel();
    block_on(panel.begin_area(0..2, 0..2)).unwrap();

    let result = block_on(panel.write_frame(8..4));

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidRange { bytes: 8..4 }));
}

#[test]
fn write_frame_past_the_frame_returns_invalid_range_before_sending() {
    let mut panel = panel();
    block_on(panel.begin_area(0..4, 0..4)).unwrap();

    let past_end = FRAME_LEN - 16..FRAME_LEN + 16;

    let result = block_on(panel.write_frame(past_end.clone()));

    assert_that!(result.err()).is_equal_to(Some(Error::InvalidRange { bytes: past_end }));
    // Only the three area registers: no partial pixel transfer.
    assert_that!(panel.release().transfers).has_length(3);
}

#[test]
fn write_frame_empty_range_sends_nothing() {
    let mut panel = panel();
    block_on(panel.begin_area(0..2, 0..2)).unwrap();

    let result = block_on(panel.write_frame(4..4));

    assert_that!(result).is_ok();
    assert_that!(panel.release().transfers).has_length(3);
}

#[test]
fn write_frame_sends_the_requested_bytes_of_the_frame() {
    let mut panel = panel();
    block_on(panel.begin_area(0..2, 0..2)).unwrap();

    block_on(panel.write_frame(4..8)).unwrap();

    let transfers = panel.release().transfers;
    assert_that!(transfers.last().cloned()).is_equal_to(Some(Transfer::pixels(&[4, 5, 6, 7])));
}

use embedded_hal::digital::{ErrorKind, ErrorType, OutputPin, PinState};
use speculoos::prelude::*;

use crate::{
    chip_select::ChipSelect,
    test_support::{YieldOnce, poll_once},
};

/// Output pin recording every level it drives; writes fail once
/// `fail_after` levels are recorded.
#[derive(Debug, Default)]
struct RecordingPin {
    levels: Vec<PinState>,
    fail_after: Option<usize>,
}

impl ErrorType for RecordingPin {
    type Error = ErrorKind;
}

impl OutputPin for RecordingPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.set_state(PinState::Low)
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.set_state(PinState::High)
    }

    fn set_state(&mut self, state: PinState) -> Result<(), Self::Error> {
        if self.fail_after == Some(self.levels.len()) {
            return Err(ErrorKind::Other);
        }
        self.levels.push(state);
        Ok(())
    }
}

#[test]
fn select_drives_pin_low() {
    let mut pin = RecordingPin::default();

    // Forgotten on purpose: dropping the guard would release the pin.
    let selected = ChipSelect::select(&mut pin).map(core::mem::forget);

    assert_that!(selected).is_ok();
    assert_that!(pin.levels).is_equal_to(vec![PinState::Low]);
}

#[test]
fn release_drives_pin_high_once() {
    let mut pin = RecordingPin::default();

    let released = ChipSelect::select(&mut pin).unwrap().release();

    assert_that!(released).is_ok();
    assert_that!(pin.levels).is_equal_to(vec![PinState::Low, PinState::High]);
}

// Regression (t-3): a cancelled transfer left chip select asserted.
#[test]
fn drop_without_release_drives_pin_high() {
    let mut pin = RecordingPin::default();

    drop(ChipSelect::select(&mut pin).unwrap());

    assert_that!(pin.levels).is_equal_to(vec![PinState::Low, PinState::High]);
}

#[test]
fn drop_mid_transfer_releases_chip_select() {
    let mut pin = RecordingPin::default();
    let transfer = async {
        let selected = ChipSelect::select(&mut pin).unwrap();
        // The bus transfer, still running when the future is dropped.
        YieldOnce(false).await;
        selected.release()
    };

    let completed = poll_once(transfer);

    assert_that!(completed).is_false();
    assert_that!(pin.levels).is_equal_to(vec![PinState::Low, PinState::High]);
}

#[test]
fn select_failure_returns_pin_error_without_release() {
    let mut pin = RecordingPin {
        fail_after: Some(0),
        ..RecordingPin::default()
    };

    let selected = ChipSelect::select(&mut pin).map(drop);

    assert_that!(selected).is_equal_to(Err(ErrorKind::Other));
    assert_that!(pin.levels).is_empty();
}

#[test]
fn release_failure_returns_pin_error() {
    let mut pin = RecordingPin {
        fail_after: Some(1),
        ..RecordingPin::default()
    };

    let released = ChipSelect::select(&mut pin).unwrap().release();

    assert_that!(released).is_equal_to(Err(ErrorKind::Other));
    // Not retried on drop.
    assert_that!(pin.levels).is_equal_to(vec![PinState::Low]);
}

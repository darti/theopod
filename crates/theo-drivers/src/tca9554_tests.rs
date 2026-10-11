use embedded_hal::digital::PinState;
use embedded_hal_async::i2c::{ErrorKind, NoAcknowledgeSource};
use speculoos::prelude::*;

use crate::{
    tca9554::{Pin, Tca9554},
    test_support::{FakeTca9554, TCA_CONFIG, TCA_OUTPUT, block_on, poll_once},
};

fn expander() -> Tca9554<FakeTca9554> {
    Tca9554::new(FakeTca9554::new(0x20), 0x20)
}

#[test]
fn pin_mask_selects_one_bit() {
    assert_that!(Pin::P0.mask()).is_equal_to(0x01);
    assert_that!(Pin::P7.mask()).is_equal_to(0x80);
}

#[test]
fn write_levels_changes_only_masked_pins() {
    let mut expander = expander();

    block_on(expander.write_levels(0x06, 0x02)).unwrap();

    // Power-on output register is 0xFF: only bit 2 drops.
    assert_that!(expander.release().writes).is_equal_to(vec![(TCA_OUTPUT, 0xFB)]);
}

#[test]
fn configure_outputs_latches_levels_before_direction() {
    let mut expander = expander();

    block_on(expander.configure_outputs(0x07, 0x05)).unwrap();

    assert_that!(expander.release().writes)
        .is_equal_to(vec![(TCA_OUTPUT, 0xFD), (TCA_CONFIG, 0xF8)]);
}

#[test]
fn configure_outputs_keeps_other_pin_directions() {
    let mut fake = FakeTca9554::new(0x20);
    // P4/P5 start as inputs, so the update must clear exactly those bits.
    fake.registers[usize::from(TCA_CONFIG)] = 0x3F;
    let mut expander = Tca9554::new(fake, 0x20);

    block_on(expander.configure_outputs(0x30, 0x30)).unwrap();

    assert_that!(expander.release().registers[usize::from(TCA_CONFIG)]).is_equal_to(0x0F);
}

#[test]
fn configure_inputs_sets_config_bits_of_masked_pins() {
    let mut fake = FakeTca9554::new(0x20);
    fake.registers[usize::from(TCA_CONFIG)] = 0x00;
    let mut expander = Tca9554::new(fake, 0x20);

    block_on(expander.configure_inputs(0x81)).unwrap();

    assert_that!(expander.release().writes).is_equal_to(vec![(TCA_CONFIG, 0x81)]);
}

#[test]
fn configure_output_drives_single_pin_level_first() {
    let mut expander = expander();

    block_on(expander.configure_output(Pin::P3, PinState::Low)).unwrap();

    assert_that!(expander.release().writes)
        .is_equal_to(vec![(TCA_OUTPUT, 0xF7), (TCA_CONFIG, 0xF7)]);
}

#[test]
fn configure_input_releases_single_pin() {
    let mut fake = FakeTca9554::new(0x20);
    fake.registers[usize::from(TCA_CONFIG)] = 0x00;
    let mut expander = Tca9554::new(fake, 0x20);

    block_on(expander.configure_input(Pin::P5)).unwrap();

    assert_that!(expander.release().writes).is_equal_to(vec![(TCA_CONFIG, 0x20)]);
}

#[test]
fn set_level_changes_single_pin() {
    let mut expander = expander();

    block_on(expander.set_level(Pin::P0, PinState::Low)).unwrap();
    block_on(expander.set_level(Pin::P0, PinState::High)).unwrap();

    assert_that!(expander.release().writes)
        .is_equal_to(vec![(TCA_OUTPUT, 0xFE), (TCA_OUTPUT, 0xFF)]);
}

#[test]
fn read_inputs_returns_input_port() {
    let mut fake = FakeTca9554::new(0x20);
    fake.inputs = 0xA5;
    let mut expander = Tca9554::new(fake, 0x20);

    assert_that!(block_on(expander.read_inputs()))
        .is_ok()
        .is_equal_to(0xA5);
}

#[test]
fn level_reads_single_input_pin() {
    let mut fake = FakeTca9554::new(0x20);
    fake.inputs = 0x10;
    let mut expander = Tca9554::new(fake, 0x20);

    assert_that!(block_on(expander.level(Pin::P4)))
        .is_ok()
        .is_equal_to(PinState::High);
    assert_that!(block_on(expander.level(Pin::P3)))
        .is_ok()
        .is_equal_to(PinState::Low);
}

#[test]
fn write_levels_bus_failure_returns_i2c_error() {
    let mut expander = Tca9554::new(FakeTca9554::new(0x20).failing(0), 0x20);

    let result = block_on(expander.write_levels(0x01, 0x00));

    assert_that!(result.err()).is_equal_to(Some(ErrorKind::Bus));
}

#[test]
fn write_levels_wrong_address_returns_nack() {
    let mut expander = Tca9554::new(FakeTca9554::new(0x21), 0x20);

    let result = block_on(expander.write_levels(0x01, 0x00));

    assert_that!(result.err())
        .is_equal_to(Some(ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address)));
}

#[test]
fn new_borrowed_bus_drives_caller_bus() {
    let mut fake = FakeTca9554::new(0x20);

    block_on(Tca9554::new(&mut fake, 0x20).write_levels(0x07, 0x07)).unwrap();

    assert_that!(fake.writes).is_equal_to(vec![(TCA_OUTPUT, 0xFF)]);
}

#[test]
fn write_levels_write_failure_after_read_returns_i2c_error() {
    let mut expander = Tca9554::new(FakeTca9554::new(0x20).failing_writes(0), 0x20);

    let result = block_on(expander.write_levels(0x01, 0x00));

    assert_that!(result.err()).is_equal_to(Some(ErrorKind::Bus));
}

#[test]
fn configure_outputs_on_suspending_bus_yields_to_executor() {
    let mut expander = Tca9554::new(FakeTca9554::new(0x20).yielding(), 0x20);

    let completed = poll_once(expander.configure_outputs(0x07, 0x05));

    assert_that!(completed).is_false();
}

#[test]
fn configure_outputs_on_suspending_bus_latches_levels_before_direction() {
    let mut expander = Tca9554::new(FakeTca9554::new(0x20).yielding(), 0x20);

    block_on(expander.configure_outputs(0x07, 0x05)).unwrap();

    assert_that!(expander.release().writes)
        .is_equal_to(vec![(TCA_OUTPUT, 0xFD), (TCA_CONFIG, 0xF8)]);
}

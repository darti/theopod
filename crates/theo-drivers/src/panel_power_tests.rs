use embedded_hal::i2c::ErrorKind;
use speculoos::prelude::*;

use crate::{
    panel_power::PanelPower,
    tca9554::Tca9554,
    test_support::{FakeTca9554, NoDelay, RecordingDelay, TCA_CONFIG, TCA_OUTPUT, block_on},
};

const POWER: PanelPower = PanelPower {
    pins: 0x07,
    settle_ms: 100,
    reset_ms: 300,
    power_up_ms: 300,
};

fn power_up(fake: FakeTca9554) -> (Result<(), ErrorKind>, FakeTca9554) {
    let mut expander = Tca9554::new(fake, 0x20);
    let result = block_on(POWER.power_up(&mut expander, &mut NoDelay));
    (result, expander.release())
}

#[test]
fn power_up_pulses_panel_pins_low_once() {
    let (result, fake) = power_up(FakeTca9554::new(0x20));

    assert_that!(result).is_ok();
    assert_that!(fake.writes).is_equal_to(vec![
        (TCA_OUTPUT, 0xFF),
        (TCA_CONFIG, 0xF8),
        (TCA_OUTPUT, 0xF8),
        (TCA_OUTPUT, 0xFF),
    ]);
}

// Regression: the output register may hold low levels (for example after a
// firmware reset during a previous pulse). Switching the pins to outputs
// before latching high levels would glitch the panel rails low.
#[test]
fn power_up_latches_levels_before_outputs_so_rails_never_glitch() {
    let mut fake = FakeTca9554::new(0x20);
    fake.registers[usize::from(TCA_OUTPUT)] = 0x00;
    let initial = fake.registers;

    let (result, fake) = power_up(fake);

    let panel_low: Vec<bool> = fake
        .driven_low_history(initial)
        .iter()
        .map(|low| low & POWER.pins != 0)
        .collect();
    assert_that!(result).is_ok();
    // Only the deliberate reset pulse (third write) drives the rails low.
    assert_that!(panel_low).is_equal_to(vec![false, false, true, false]);
}

#[test]
fn power_up_leaves_other_expander_pins_untouched() {
    let mut fake = FakeTca9554::new(0x20);
    fake.registers[usize::from(TCA_OUTPUT)] = 0x50;
    fake.registers[usize::from(TCA_CONFIG)] = 0x7F;

    let (_, fake) = power_up(fake);

    assert_that!(fake.registers[usize::from(TCA_OUTPUT)]).is_equal_to(0x57);
    assert_that!(fake.registers[usize::from(TCA_CONFIG)]).is_equal_to(0x78);
}

#[test]
fn power_up_bus_failure_returns_i2c_error_and_stops() {
    let (result, fake) = power_up(FakeTca9554::new(0x20).failing(2));

    assert_that!(result.err()).is_equal_to(Some(ErrorKind::Bus));
    assert_that!(fake.writes).has_length(2);
}

#[test]
fn power_up_waits_settle_then_reset_pulse_then_power_up() {
    // Distinct durations, so a swapped or dropped delay shows.
    let power = PanelPower {
        settle_ms: 1,
        reset_ms: 2,
        power_up_ms: 3,
        ..POWER
    };
    let mut expander = Tca9554::new(FakeTca9554::new(0x20), 0x20);
    let mut delay = RecordingDelay::default();

    block_on(power.power_up(&mut expander, &mut delay)).unwrap();

    assert_that!(delay.delays_ms).is_equal_to(vec![1, 2, 3]);
}

use embedded_hal_async::i2c::ErrorKind;
use speculoos::prelude::*;

use crate::{
    panel_power::PanelPower,
    tca9554::{Pin, Tca9554},
    test_support::{FakeTca9554, NoDelay, RecordingDelay, TCA_CONFIG, TCA_OUTPUT, block_on},
};

// The board's panel pins (EXIO0/1); EXIO2 is the touch reset.
const POWER: PanelPower = PanelPower {
    pins: Pin::P0.mask() | Pin::P1.mask(),
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
        (TCA_CONFIG, 0xFC),
        (TCA_OUTPUT, 0xFC),
        (TCA_OUTPUT, 0xFF),
    ]);
}

// Regression (t-8): the output register may hold low levels (for example after a
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

    let (result, fake) = power_up(fake);

    assert_that!(result).is_ok();
    assert_that!(fake.registers[usize::from(TCA_OUTPUT)]).is_equal_to(0x53);
    assert_that!(fake.registers[usize::from(TCA_CONFIG)]).is_equal_to(0x7C);
}

// The touch controller's reset (EXIO2) shares the expander: the panel pulse
// must never drive it low, even when it is already an output.
#[test]
fn power_up_never_drives_touch_reset_low() {
    let mut fake = FakeTca9554::new(0x20);
    fake.registers[usize::from(TCA_CONFIG)] = !Pin::P2.mask();
    let initial = fake.registers;

    let (_, fake) = power_up(fake);

    let touch_low: Vec<bool> = fake
        .driven_low_history(initial)
        .iter()
        .map(|low| low & Pin::P2.mask() != 0)
        .collect();
    assert_that!(touch_low).is_equal_to(vec![false; 4]);
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

fn power_down(fake: FakeTca9554) -> (Result<(), ErrorKind>, FakeTca9554) {
    let mut expander = Tca9554::new(fake, 0x20);
    let result = block_on(POWER.power_down(&mut expander));
    (result, expander.release())
}

// Regression guard (t-8): as at power-up, levels are latched before the
// pins become outputs, so no pin drives a stale level.
#[test]
fn power_down_drives_panel_pins_low_as_outputs() {
    let (result, fake) = power_down(FakeTca9554::new(0x20));

    assert_that!(result).is_ok();
    // Levels latched low before the pins become outputs.
    assert_that!(fake.writes).is_equal_to(vec![(TCA_OUTPUT, 0xFC), (TCA_CONFIG, 0xFC)]);
}

#[test]
fn power_down_after_power_up_cuts_rails_and_reset() {
    let mut expander = Tca9554::new(FakeTca9554::new(0x20), 0x20);
    block_on(POWER.power_up(&mut expander, &mut NoDelay)).unwrap();

    block_on(POWER.power_down(&mut expander)).unwrap();

    let fake = expander.release();
    assert_that!(fake.registers[usize::from(TCA_OUTPUT)] & POWER.pins).is_equal_to(0);
    assert_that!(fake.registers[usize::from(TCA_CONFIG)] & POWER.pins).is_equal_to(0);
}

// EXIO2 (touch reset) is parked by the touch driver, not by the panel.
#[test]
fn power_down_leaves_other_expander_pins_untouched() {
    let mut fake = FakeTca9554::new(0x20);
    fake.registers[usize::from(TCA_OUTPUT)] = 0x57;
    fake.registers[usize::from(TCA_CONFIG)] = 0x7F;

    let (result, fake) = power_down(fake);

    assert_that!(result).is_ok();
    assert_that!(fake.registers[usize::from(TCA_OUTPUT)]).is_equal_to(0x54);
    assert_that!(fake.registers[usize::from(TCA_CONFIG)]).is_equal_to(0x7C);
}

#[test]
fn power_down_bus_failure_returns_i2c_error() {
    let (result, _) = power_down(FakeTca9554::new(0x20).failing(0));

    assert_that!(result.err()).is_equal_to(Some(ErrorKind::Bus));
}

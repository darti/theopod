use embedded_hal_async::i2c::ErrorKind;
use speculoos::prelude::*;

use crate::{
    qmi8658::{Acceleration, Error, Qmi8658},
    test_support::{FakeQmi8658, RecordingDelay, block_on},
};

const ADDRESS: u8 = 0x6B;

fn imu(fake: FakeQmi8658) -> Qmi8658<FakeQmi8658> {
    Qmi8658::new(fake, ADDRESS)
}

#[test]
fn init_resets_then_enables_accelerometer_only() {
    let mut imu = imu(FakeQmi8658::new(ADDRESS));

    let result = block_on(imu.init(&mut RecordingDelay::default()));

    assert_that!(result).is_ok();
    // Oscillator on; reset; address auto-increment, little-endian; ±2 g in
    // low-power mode at 21 Hz (t-41); accelerometer on, gyroscope off.
    assert_that!(imu.release().writes).is_equal_to(vec![
        (0x02, 0x40),
        (0x60, 0xB0),
        (0x02, 0x40),
        (0x03, 0x0D),
        (0x08, 0x01),
    ]);
}

#[test]
fn init_polls_reset_result_until_done() {
    let mut fake = FakeQmi8658::new(ADDRESS);
    fake.reset_polls = Some(3);
    let mut imu = imu(fake);
    let mut delay = RecordingDelay::default();

    let result = block_on(imu.init(&mut delay));

    assert_that!(result).is_ok();
    assert_that!(delay.delays_ms).is_equal_to(vec![5, 5, 5]);
}

#[test]
fn init_reset_never_done_gives_up_after_budget_without_configuring() {
    let mut fake = FakeQmi8658::new(ADDRESS);
    fake.reset_polls = None;
    let mut imu = imu(fake);
    let mut delay = RecordingDelay::default();

    let result = block_on(imu.init(&mut delay));

    assert_that!(result).is_equal_to(Err(Error::ResetTimeout));
    assert_that!(delay.delays_ms).is_equal_to(vec![5; 10]);
    assert_that!(imu.release().writes).is_equal_to(vec![(0x02, 0x40), (0x60, 0xB0)]);
}

#[test]
fn init_other_chip_returns_wrong_chip_without_any_write() {
    let mut fake = FakeQmi8658::new(ADDRESS);
    fake.registers[0] = 0x42;
    let mut imu = imu(fake);

    let result = block_on(imu.init(&mut RecordingDelay::default()));

    assert_that!(result).is_equal_to(Err(Error::WrongChip { id: 0x42 }));
    assert_that!(imu.release().writes).is_empty();
}

#[test]
fn init_failed_accelerometer_enable_returns_bus_error() {
    let mut fake = FakeQmi8658::new(ADDRESS);
    fake.failing_register = Some(0x08);
    let mut imu = imu(fake);

    let result = block_on(imu.init(&mut RecordingDelay::default()));

    assert_that!(result).is_equal_to(Err(Error::Bus(ErrorKind::Bus)));
}

#[test]
fn init_bus_failure_returns_bus_error() {
    let mut fake = FakeQmi8658::new(ADDRESS);
    fake.failing = true;
    let mut imu = imu(fake);

    let result = block_on(imu.init(&mut RecordingDelay::default()));

    assert_that!(result).is_equal_to(Err(Error::Bus(ErrorKind::Bus)));
}

#[test]
fn acceleration_reads_little_endian_axes() {
    let mut fake = FakeQmi8658::new(ADDRESS);
    fake.set_acceleration(16_384, -16_384, 0x1234);
    let mut imu = imu(fake);
    // `init` enables the auto-increment the 6-byte burst relies on.
    block_on(imu.init(&mut RecordingDelay::default())).unwrap();

    let acceleration = block_on(imu.acceleration());

    assert_that!(acceleration).is_equal_to(Ok(Acceleration {
        x: 16_384,
        y: -16_384,
        z: 0x1234,
    }));
}

#[test]
fn acceleration_bus_failure_returns_bus_error() {
    let mut fake = FakeQmi8658::new(ADDRESS);
    fake.failing = true;
    let mut imu = imu(fake);

    let acceleration = block_on(imu.acceleration());

    assert_that!(acceleration).is_equal_to(Err(Error::Bus(ErrorKind::Bus)));
}

#[test]
fn power_down_disables_sensors_then_oscillator() {
    let mut imu = imu(FakeQmi8658::new(ADDRESS));

    let result = block_on(imu.power_down());

    assert_that!(result).is_ok();
    // Accelerometer and gyroscope off, then CTRL1 sensorDisable (bit 0)
    // stops the 2 MHz oscillator; auto-increment stays on.
    assert_that!(imu.release().writes).is_equal_to(vec![(0x08, 0x00), (0x02, 0x41)]);
}

#[test]
fn power_down_failed_sensor_disable_still_stops_oscillator() {
    let mut fake = FakeQmi8658::new(ADDRESS);
    fake.failing_register = Some(0x08);
    let mut imu = imu(fake);

    let result = block_on(imu.power_down());

    assert_that!(result).is_equal_to(Err(Error::Bus(ErrorKind::Bus)));
    assert_that!(imu.release().writes).is_equal_to(vec![(0x02, 0x41)]);
}

// Regression (t-50): after a deep-sleep stop the sensor keeps its
// power-down state (oscillator off) into the next boot; a soft reset sent
// in that state may never complete, losing orientation until power-cycled.
#[test]
fn init_after_power_down_starts_oscillator_before_reset() {
    let mut fake = FakeQmi8658::new(ADDRESS);
    fake.registers[0x02] = 0x41;
    let mut imu = imu(fake);

    let result = block_on(imu.init(&mut RecordingDelay::default()));

    assert_that!(result).is_ok();
    assert_that!(imu.release().writes.first()).is_equal_to(Some(&(0x02, 0x40)));
}

#[test]
fn power_down_bus_failure_returns_bus_error() {
    let mut fake = FakeQmi8658::new(ADDRESS);
    fake.failing = true;
    let mut imu = imu(fake);

    let result = block_on(imu.power_down());

    assert_that!(result).is_equal_to(Err(Error::Bus(ErrorKind::Bus)));
}

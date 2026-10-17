use std::{cell::RefCell, rc::Rc};

use embedded_hal::{
    delay::DelayNs,
    digital::{ErrorKind, ErrorType, InputPin, OutputPin},
};
use speculoos::prelude::*;

use crate::bus_clear::{BusClear, BusClearError, MAX_CLOCK_PULSES, clear_i2c_bus};

/// What the master did on the bus, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    /// SCL driven (`false`) or released (`true`).
    Scl(bool),
    /// SDA driven (`false`) or released (`true`).
    Sda(bool),
    /// A delay, in ns.
    Wait(u32),
}

/// Two open-drain lines and a slave the last reset left mid-transfer.
///
/// The slave puts `bits[n]` on SDA after the n-th falling SCL edge (as a
/// transmitter shifts its next bit while SCL is low), `true` releasing it;
/// past the end it has let go. It may also hold SCL low from the
/// `scl_held_from`-th falling edge on.
#[derive(Debug, Default)]
struct Bus {
    scl_out: bool,
    sda_out: bool,
    bits: Vec<bool>,
    falling_edges: usize,
    scl_held_from: Option<usize>,
    events: Vec<Event>,
    /// Pin access (counting from 0) that fails, once.
    fail_at: Option<usize>,
    accesses: usize,
}

impl Bus {
    /// A slave holding SDA low until the `n`-th falling edge.
    fn holding_sda_for(n: usize) -> Self {
        Self {
            bits: vec![false; n],
            ..Self::default()
        }
    }

    fn scl(&self) -> bool {
        self.scl_out
            && self
                .scl_held_from
                .is_none_or(|from| self.falling_edges < from)
    }

    fn sda(&self) -> bool {
        self.sda_out && self.bits.get(self.falling_edges).copied().unwrap_or(true)
    }

    fn access(&mut self) -> Result<(), ErrorKind> {
        let n = self.accesses;
        self.accesses += 1;
        if self.fail_at == Some(n) {
            Err(ErrorKind::Other)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
struct Line {
    bus: Rc<RefCell<Bus>>,
    clock: bool,
}

impl ErrorType for Line {
    type Error = ErrorKind;
}

impl OutputPin for Line {
    fn set_low(&mut self) -> Result<(), ErrorKind> {
        self.drive(false)
    }

    fn set_high(&mut self) -> Result<(), ErrorKind> {
        self.drive(true)
    }
}

impl InputPin for Line {
    fn is_high(&mut self) -> Result<bool, ErrorKind> {
        let mut bus = self.bus.borrow_mut();
        bus.access()?;
        Ok(if self.clock { bus.scl() } else { bus.sda() })
    }

    fn is_low(&mut self) -> Result<bool, ErrorKind> {
        self.is_high().map(|high| !high)
    }
}

impl Line {
    fn drive(&mut self, level: bool) -> Result<(), ErrorKind> {
        let mut bus = self.bus.borrow_mut();
        bus.access()?;
        if self.clock {
            if bus.scl_out && !level {
                bus.falling_edges += 1;
            }
            bus.scl_out = level;
            bus.events.push(Event::Scl(level));
        } else {
            bus.sda_out = level;
            bus.events.push(Event::Sda(level));
        }
        Ok(())
    }
}

/// Records delays among the bus events.
struct Clock(Rc<RefCell<Bus>>);

impl DelayNs for Clock {
    fn delay_ns(&mut self, ns: u32) {
        self.0.borrow_mut().events.push(Event::Wait(ns));
    }
}

fn run(bus: Bus) -> (Rc<RefCell<Bus>>, Result<BusClear, BusClearError<ErrorKind>>) {
    let bus = Rc::new(RefCell::new(bus));
    let mut scl = Line {
        bus: bus.clone(),
        clock: true,
    };
    let mut sda = Line {
        bus: bus.clone(),
        clock: false,
    };
    let result = clear_i2c_bus(&mut scl, &mut sda, &mut Clock(bus.clone()));
    (bus, result)
}

/// The line changes the master made, without the delays.
fn changes(bus: &Bus) -> Vec<Event> {
    bus.events
        .iter()
        .copied()
        .filter(|event| !matches!(event, Event::Wait(_)))
        .collect()
}

#[test]
fn clear_idle_bus_only_releases_the_lines() {
    let (bus, result) = run(Bus::default());

    assert_that!(result).is_equal_to(Ok(BusClear::Idle));
    assert_that!(changes(&bus.borrow())).is_equal_to(vec![Event::Scl(true), Event::Sda(true)]);
}

// Regression guard (t-84): a slave left mid-transfer by a watchdog reset
// kept the bus, so the early stop could not park the board over it.
#[test]
fn clear_sda_held_clocks_until_released() {
    let (_, result) = run(Bus::holding_sda_for(3));

    assert_that!(result).is_equal_to(Ok(BusClear::Cleared { pulses: 3 }));
}

#[test]
fn clear_sda_held_ends_with_a_stop() {
    let (bus, result) = run(Bus::holding_sda_for(1));

    assert_that!(result).is_equal_to(Ok(BusClear::Cleared { pulses: 1 }));
    // STOP: SDA driven low while SCL is low, then released while SCL is high.
    let changes = changes(&bus.borrow());
    let stop = changes[changes.len() - 4..].to_vec();
    assert_that!(stop).is_equal_to(vec![
        Event::Scl(false),
        Event::Sda(false),
        Event::Scl(true),
        Event::Sda(true),
    ]);
}

#[test]
fn clear_waits_half_a_period_after_every_line_change() {
    let (bus, _) = run(Bus::holding_sda_for(2));

    // After the initial release of both lines and its wait, each change is
    // followed by a 5 µs wait: the lines rise through pull-ups.
    let events = &bus.borrow().events;
    let mut rest = events[3..].chunks(2);
    assert_that!(
        rest.all(|pair| matches!(pair, [Event::Scl(_) | Event::Sda(_), Event::Wait(5_000)]))
    )
    .is_true();
}

#[test]
fn clear_sda_released_on_the_last_pulse_clears() {
    let (_, result) = run(Bus::holding_sda_for(usize::from(MAX_CLOCK_PULSES)));

    assert_that!(result).is_equal_to(Ok(BusClear::Cleared {
        pulses: MAX_CLOCK_PULSES,
    }));
}

#[test]
fn clear_sda_released_after_the_last_pulse_reports_stuck() {
    let (_, result) = run(Bus::holding_sda_for(usize::from(MAX_CLOCK_PULSES) + 1));

    assert_that!(result).is_equal_to(Err(BusClearError::Stuck));
}

// Regression guard (t-84): a slave sending a byte puts a 1 on SDA, then a 0
// on the STOP's falling edge; the bus is still held after that STOP.
#[test]
fn clear_slave_driving_low_again_after_the_stop_clocks_on() {
    let (bus, result) = run(Bus {
        bits: vec![false, true, false],
        ..Bus::default()
    });

    assert_that!(result).is_equal_to(Ok(BusClear::Cleared { pulses: 2 }));
    let bus = bus.borrow();
    assert_that!((bus.scl(), bus.sda())).is_equal_to((true, true));
}

#[test]
fn clear_sda_never_released_reports_stuck_and_releases_the_lines() {
    let (bus, result) = run(Bus::holding_sda_for(64));

    assert_that!(result).is_equal_to(Err(BusClearError::Stuck));
    let bus = bus.borrow();
    assert_that!(bus.falling_edges).is_equal_to(usize::from(MAX_CLOCK_PULSES));
    assert_that!((bus.scl_out, bus.sda_out)).is_equal_to((true, true));
}

#[test]
fn clear_scl_held_low_reports_it_without_clocking() {
    let (bus, result) = run(Bus {
        scl_held_from: Some(0),
        ..Bus::default()
    });

    assert_that!(result).is_equal_to(Err(BusClearError::ClockHeld));
    assert_that!(bus.borrow().falling_edges).is_equal_to(0);
}

#[test]
fn clear_scl_held_during_the_pulses_reports_it_and_releases_the_lines() {
    let (bus, result) = run(Bus {
        scl_held_from: Some(2),
        ..Bus::holding_sda_for(5)
    });

    assert_that!(result).is_equal_to(Err(BusClearError::ClockHeld));
    let bus = bus.borrow();
    assert_that!(bus.falling_edges).is_equal_to(2);
    assert_that!((bus.scl_out, bus.sda_out)).is_equal_to((true, true));
}

#[test]
fn clear_pin_failure_returns_pin_error() {
    let (_, result) = run(Bus {
        fail_at: Some(0),
        ..Bus::default()
    });

    assert_that!(result).is_equal_to(Err(BusClearError::Pin(ErrorKind::Other)));
}

#[test]
fn clear_pin_failure_mid_pulse_releases_the_lines() {
    // Accesses: release SCL, release SDA, read SCL, read SDA, drive SCL
    // low, then the failing release of SCL.
    let (bus, result) = run(Bus {
        fail_at: Some(5),
        ..Bus::holding_sda_for(3)
    });

    assert_that!(result).is_equal_to(Err(BusClearError::Pin(ErrorKind::Other)));
    let bus = bus.borrow();
    assert_that!((bus.scl_out, bus.sda_out)).is_equal_to((true, true));
}

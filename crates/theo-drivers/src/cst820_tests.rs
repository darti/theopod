use std::{cell::RefCell, rc::Rc};

use embedded_hal::digital::{ErrorKind as PinErrorKind, ErrorType as PinErrorType};
use embedded_hal_async::{
    delay::DelayNs,
    digital::Wait,
    i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation},
};
use speculoos::prelude::*;

use crate::{
    cst820::{Cst820, Error, IDLE_PROBE_MS, InitError, POLL_MS, TouchEvent},
    geometry::{PanelSize, TouchPoint},
    tca9554::{Pin, Tca9554},
    test_support::{FakeCst820, FakeTca9554, RecordingDelay, TCA_CONFIG, block_on},
};

const ADDRESS: u8 = 0x15;
const EXPANDER_ADDRESS: u8 = 0x20;
const SIZE: PanelSize = PanelSize {
    width: 368,
    height: 448,
};

/// Interrupt pin whose falling edges arrive at once; counts the waits.
/// A `silent` pin never sees an edge.
#[derive(Debug, Default)]
struct EdgePin {
    falling_edges: usize,
    fail: bool,
    silent: bool,
}

impl PinErrorType for EdgePin {
    type Error = PinErrorKind;
}

impl Wait for EdgePin {
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        unimplemented!("the touch driver waits for falling edges")
    }

    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        unimplemented!("the touch driver waits for falling edges")
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        unimplemented!("the touch driver waits for falling edges")
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        if self.fail {
            return Err(PinErrorKind::Other);
        }
        if self.silent {
            core::future::pending::<()>().await;
        }
        self.falling_edges += 1;
        Ok(())
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        unimplemented!("the touch driver waits for falling edges")
    }
}

/// One step of `init`, as recorded by [`Logged`] fakes sharing a log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Expander,
    Delay(u32),
    Controller,
}

type Log = Rc<RefCell<Vec<Step>>>;

/// Wraps a fake so each access appends to a log shared with other fakes.
///
/// Back-to-back accesses to one device log once, so a read-modify-write is
/// one step.
struct Logged<T> {
    inner: T,
    log: Log,
    device: Step,
}

impl<T> Logged<T> {
    fn new(inner: T, log: &Log, device: Step) -> Self {
        Self {
            inner,
            log: log.clone(),
            device,
        }
    }

    fn record(&self, step: Step) {
        let mut log = self.log.borrow_mut();
        let repeated_access = !matches!(step, Step::Delay(_)) && log.last() == Some(&step);
        if !repeated_access {
            log.push(step);
        }
    }
}

impl<T: ErrorType> ErrorType for Logged<T> {
    type Error = T::Error;
}

impl<T: I2c> I2c for Logged<T> {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.record(self.device);
        self.inner.transaction(address, operations).await
    }
}

impl DelayNs for Logged<RecordingDelay> {
    async fn delay_ns(&mut self, ns: u32) {
        self.delay_ms(ns / 1_000_000).await;
    }

    async fn delay_ms(&mut self, ms: u32) {
        self.record(Step::Delay(ms));
        self.inner.delay_ms(ms).await;
    }
}

type Touch = Cst820<FakeCst820, EdgePin, RecordingDelay>;

fn touch(reports: impl IntoIterator<Item = Result<[u8; 5], ErrorKind>>) -> Touch {
    Cst820::new(
        FakeCst820::new(ADDRESS, reports),
        ADDRESS,
        EdgePin::default(),
        RecordingDelay::default(),
        SIZE,
    )
}

/// Touch controller whose interrupt line never moves.
fn silent_touch(reports: impl IntoIterator<Item = Result<[u8; 5], ErrorKind>>) -> Touch {
    Cst820::new(
        FakeCst820::new(ADDRESS, reports),
        ADDRESS,
        EdgePin {
            silent: true,
            ..EdgePin::default()
        },
        RecordingDelay::default(),
        SIZE,
    )
}

fn expander() -> Tca9554<FakeTca9554> {
    Tca9554::new(FakeTca9554::new(EXPANDER_ADDRESS), EXPANDER_ADDRESS)
}

fn point(x: u16, y: u16) -> TouchPoint {
    TouchPoint { x, y }
}

#[test]
fn init_pulses_reset_before_first_controller_access() {
    let log = Log::default();
    let mut touch = Cst820::new(
        Logged::new(FakeCst820::new(ADDRESS, []), &log, Step::Controller),
        ADDRESS,
        EdgePin::default(),
        Logged::new(RecordingDelay::default(), &log, Step::Delay(0)),
        SIZE,
    );
    let mut expander = Tca9554::new(
        Logged::new(FakeTca9554::new(EXPANDER_ADDRESS), &log, Step::Expander),
        EXPANDER_ADDRESS,
    );

    block_on(touch.init(&mut expander, Pin::P2)).unwrap();

    assert_that!(*log.borrow()).is_equal_to(vec![
        Step::Expander,
        Step::Delay(10),
        Step::Expander,
        Step::Delay(200),
        Step::Controller,
    ]);
}

#[test]
fn init_drives_only_reset_pin_low_then_releases_it() {
    let mut touch = touch([]);
    let fake = FakeTca9554::new(EXPANDER_ADDRESS);
    let initial = fake.registers;
    let mut expander = Tca9554::new(fake, EXPANDER_ADDRESS);

    block_on(touch.init(&mut expander, Pin::P2)).unwrap();

    let driven_low = expander.release().driven_low_history(initial);
    assert_that!(driven_low.contains(&Pin::P2.mask())).is_true();
    assert_that!(driven_low.iter().all(|low| low & !Pin::P2.mask() == 0)).is_true();
    assert_that!(driven_low.last()).is_equal_to(Some(&0));
}

#[test]
fn init_enables_touch_and_change_interrupts() {
    let mut touch = touch([]);

    block_on(touch.init(&mut expander(), Pin::P2)).unwrap();

    // Periodic pulses while touched, plus one on every touch change.
    assert_that!(touch.release().0.writes).is_equal_to(vec![(0xFA, 0x60)]);
}

#[test]
fn init_expander_failure_returns_reset_error_without_controller_access() {
    let mut touch = touch([]);
    let mut expander = Tca9554::new(
        FakeTca9554::new(EXPANDER_ADDRESS).failing(0),
        EXPANDER_ADDRESS,
    );

    let result = block_on(touch.init(&mut expander, Pin::P2));

    assert_that!(result).is_equal_to(Err(InitError::Reset(ErrorKind::Bus)));
    assert_that!(touch.release().0.transactions).is_equal_to(0);
}

#[test]
fn init_controller_failure_returns_bus_error() {
    // The controller is absent: its address NACKs.
    let mut touch = Cst820::new(
        FakeCst820::new(ADDRESS + 1, []),
        ADDRESS,
        EdgePin::default(),
        RecordingDelay::default(),
        SIZE,
    );

    let result = block_on(touch.init(&mut expander(), Pin::P2));

    assert_that!(result).is_equal_to(Err(InitError::Bus(ErrorKind::NoAcknowledge(
        NoAcknowledgeSource::Address,
    ))));
}

// Regression (t-34): a runtime reset used to forget a touch in progress, so
// the UI never saw it end and kept the pointer pressed.
#[test]
fn init_after_press_reports_cancelled_then_waits_for_new_press() {
    let mut touch = touch([FakeCst820::touch(1, 2, 0), FakeCst820::touch(3, 4, 0)]);
    block_on(touch.changed()).unwrap();

    block_on(touch.init(&mut expander(), Pin::P2)).unwrap();
    let cancelled = block_on(touch.changed());
    let next = block_on(touch.changed());

    assert_that!(cancelled).is_equal_to(Ok(TouchEvent::Cancelled(point(1, 2))));
    assert_that!(next).is_equal_to(Ok(TouchEvent::Pressed(point(3, 4))));
    assert_that!(touch.release().1.falling_edges).is_equal_to(2);
}

#[test]
fn init_keeps_cancellation_of_failed_touch() {
    let mut touch = touch([FakeCst820::touch(1, 2, 0), Err(ErrorKind::Bus)]);
    block_on(touch.changed()).unwrap();
    block_on(touch.changed()).unwrap_err();

    block_on(touch.init(&mut expander(), Pin::P2)).unwrap();
    let cancelled = block_on(touch.changed());

    assert_that!(cancelled).is_equal_to(Ok(TouchEvent::Cancelled(point(1, 2))));
}

#[test]
fn changed_idle_waits_for_interrupt_then_reports_pressed() {
    let mut touch = touch([FakeCst820::touch(100, 200, 0)]);

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Ok(TouchEvent::Pressed(point(100, 200))));
    let (_, pin, delay) = touch.release();
    assert_that!(pin.falling_edges).is_equal_to(1);
    assert_that!(delay.delays_ms).is_empty();
}

#[test]
fn changed_pressed_polls_without_interrupt_and_reports_moved() {
    let mut touch = touch([
        FakeCst820::touch(100, 200, 0),
        FakeCst820::touch(110, 205, 2),
    ]);
    block_on(touch.changed()).unwrap();

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Ok(TouchEvent::Moved(point(110, 205))));
    let (_, pin, delay) = touch.release();
    assert_that!(pin.falling_edges).is_equal_to(1);
    assert_that!(delay.delays_ms).is_equal_to(vec![POLL_MS]);
}

#[test]
fn changed_unmoved_finger_keeps_polling_silently() {
    let mut touch = touch([
        FakeCst820::touch(100, 200, 0),
        FakeCst820::touch(100, 200, 2),
        FakeCst820::touch(101, 200, 2),
    ]);
    block_on(touch.changed()).unwrap();

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Ok(TouchEvent::Moved(point(101, 200))));
    assert_that!(touch.release().2.delays_ms).is_equal_to(vec![POLL_MS, POLL_MS]);
}

#[test]
fn changed_lifted_finger_reports_released_at_last_point() {
    let mut touch = touch([
        FakeCst820::touch(100, 200, 0),
        FakeCst820::touch(120, 210, 2),
        FakeCst820::no_touch(),
    ]);
    block_on(touch.changed()).unwrap();
    block_on(touch.changed()).unwrap();

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Ok(TouchEvent::Released(point(120, 210))));
}

#[test]
fn changed_lift_with_stale_coordinates_reports_released() {
    // Controllers often keep the last coordinates after a lift.
    let mut touch = touch([FakeCst820::touch(100, 200, 0), Ok([0, 0x40, 99, 0, 98])]);
    block_on(touch.changed()).unwrap();

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Ok(TouchEvent::Released(point(100, 200))));
}

#[test]
fn changed_after_release_waits_for_next_interrupt() {
    let mut touch = touch([
        FakeCst820::touch(100, 200, 0),
        FakeCst820::no_touch(),
        FakeCst820::touch(50, 60, 0),
    ]);
    block_on(touch.changed()).unwrap();
    block_on(touch.changed()).unwrap();

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Ok(TouchEvent::Pressed(point(50, 60))));
    assert_that!(touch.release().1.falling_edges).is_equal_to(2);
}

#[test]
fn changed_interrupt_without_finger_waits_again() {
    let mut touch = touch([FakeCst820::no_touch(), FakeCst820::touch(5, 6, 0)]);

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Ok(TouchEvent::Pressed(point(5, 6))));
    assert_that!(touch.release().1.falling_edges).is_equal_to(2);
}

#[test]
fn changed_event_bits_are_masked_out_of_x() {
    // Event "contact" (2) in the top bits of the X high byte.
    let mut touch = touch([FakeCst820::touch(0x123, 7, 2)]);

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Ok(TouchEvent::Pressed(point(0x123, 7))));
}

#[test]
fn changed_touch_id_bits_are_masked_out_of_y() {
    // Touch id 3 in the top bits of the Y high byte, Y = 0x123.
    let mut touch = touch([Ok([1, 0, 7, 0x30 | 0x01, 0x23])]);

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Ok(TouchEvent::Pressed(point(7, 0x123))));
}

#[test]
fn changed_last_pixel_of_panel_is_accepted() {
    let corner = point(SIZE.width - 1, SIZE.height - 1);
    let mut touch = touch([FakeCst820::touch(corner.x, corner.y, 0)]);

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Ok(TouchEvent::Pressed(corner)));
}

#[test]
fn changed_x_outside_panel_returns_invalid_report() {
    let mut touch = touch([FakeCst820::touch(SIZE.width, 10, 0)]);

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Err(Error::InvalidReport {
        fingers: 1,
        x: SIZE.width,
        y: 10,
    }));
}

#[test]
fn changed_y_outside_panel_returns_invalid_report() {
    let mut touch = touch([FakeCst820::touch(10, SIZE.height, 0)]);

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Err(Error::InvalidReport {
        fingers: 1,
        x: 10,
        y: SIZE.height,
    }));
}

#[test]
fn changed_two_fingers_returns_invalid_report() {
    let mut touch = touch([Ok([2, 0, 10, 0, 20])]);

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Err(Error::InvalidReport {
        fingers: 2,
        x: 10,
        y: 20,
    }));
}

#[test]
fn changed_garbage_report_returns_invalid_report() {
    let mut touch = touch([Ok([0xFF; 5])]);

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Err(Error::InvalidReport {
        fingers: 0xFF,
        x: 0xFFF,
        y: 0xFFF,
    }));
}

#[test]
fn changed_idle_error_cancels_nothing() {
    let mut touch = touch([Err(ErrorKind::Bus), FakeCst820::touch(30, 40, 0)]);
    let failed = block_on(touch.changed());

    let next = block_on(touch.changed());

    assert_that!(failed).is_equal_to(Err(Error::Bus(ErrorKind::Bus)));
    assert_that!(next).is_equal_to(Ok(TouchEvent::Pressed(point(30, 40))));
}

#[test]
fn changed_bus_error_while_pressed_returns_error_then_cancelled() {
    let mut touch = touch([
        FakeCst820::touch(100, 200, 0),
        Err(ErrorKind::Bus),
        FakeCst820::touch(30, 40, 0),
    ]);
    block_on(touch.changed()).unwrap();

    let failed = block_on(touch.changed());
    let cancelled = block_on(touch.changed());
    let next = block_on(touch.changed());

    assert_that!(failed).is_equal_to(Err(Error::Bus(ErrorKind::Bus)));
    assert_that!(cancelled).is_equal_to(Ok(TouchEvent::Cancelled(point(100, 200))));
    // The next touch starts over from the interrupt, as a new press.
    assert_that!(next).is_equal_to(Ok(TouchEvent::Pressed(point(30, 40))));
    assert_that!(touch.release().1.falling_edges).is_equal_to(2);
}

#[test]
fn changed_invalid_report_while_pressed_returns_error_then_cancelled() {
    let mut touch = touch([
        FakeCst820::touch(100, 200, 0),
        FakeCst820::touch(SIZE.width, 200, 2),
    ]);
    block_on(touch.changed()).unwrap();

    let failed = block_on(touch.changed());
    let cancelled = block_on(touch.changed());

    assert_that!(failed).is_equal_to(Err(Error::InvalidReport {
        fingers: 1,
        x: SIZE.width,
        y: 200,
    }));
    assert_that!(cancelled).is_equal_to(Ok(TouchEvent::Cancelled(point(100, 200))));
}

#[test]
fn changed_pin_failure_returns_pin_error() {
    let pin = EdgePin {
        fail: true,
        ..EdgePin::default()
    };
    let bus = FakeCst820::new(ADDRESS, []);
    let mut touch = Cst820::new(bus, ADDRESS, pin, RecordingDelay::default(), SIZE);

    let event = block_on(touch.changed());

    assert_that!(event).is_equal_to(Err(Error::Pin(PinErrorKind::Other)));
}

// Regression (t-34 review): a controller that stopped pulsing its interrupt
// never failed a read, so it was never reset.
#[test]
fn changed_silent_interrupt_probes_and_reports_bus_error() {
    let mut touch = silent_touch([Err(ErrorKind::Bus)]);

    let result = block_on(touch.changed());

    assert_that!(result).is_equal_to(Err(Error::Bus(ErrorKind::Bus)));
    assert_that!(touch.release().2.delays_ms).is_equal_to(vec![IDLE_PROBE_MS]);
}

#[test]
fn changed_silent_interrupt_probe_reports_missed_press() {
    let mut touch = silent_touch([FakeCst820::touch(5, 6, 0)]);

    let result = block_on(touch.changed());

    assert_that!(result).is_equal_to(Ok(TouchEvent::Pressed(point(5, 6))));
}

#[test]
fn changed_silent_interrupt_idle_probe_keeps_waiting() {
    let mut touch = silent_touch([FakeCst820::no_touch(), FakeCst820::touch(5, 6, 0)]);

    let result = block_on(touch.changed());

    assert_that!(result).is_equal_to(Ok(TouchEvent::Pressed(point(5, 6))));
    assert_that!(touch.release().2.delays_ms).is_equal_to(vec![IDLE_PROBE_MS, IDLE_PROBE_MS]);
}

#[test]
fn init_failed_reset_still_cancels_touch_in_progress() {
    let mut touch = touch([FakeCst820::touch(1, 2, 0)]);
    block_on(touch.changed()).unwrap();
    let mut failing = Tca9554::new(
        FakeTca9554::new(EXPANDER_ADDRESS).failing(0),
        EXPANDER_ADDRESS,
    );

    block_on(touch.init(&mut failing, Pin::P2)).unwrap_err();
    let cancelled = block_on(touch.changed());

    assert_that!(cancelled).is_equal_to(Ok(TouchEvent::Cancelled(point(1, 2))));
}

#[test]
fn hold_in_reset_latches_low_then_makes_only_reset_pin_an_output() {
    let mut touch = touch([]);
    let fake = FakeTca9554::new(EXPANDER_ADDRESS);
    let initial = fake.registers;
    let mut expander = Tca9554::new(fake, EXPANDER_ADDRESS);

    let result = block_on(touch.hold_in_reset(&mut expander, Pin::P2));

    assert_that!(result).is_ok();
    let fake = expander.release();
    // The low level is latched while the pin is still an input, so no pin
    // is driven low until the reset pin becomes an output.
    assert_that!(fake.driven_low_history(initial)).is_equal_to(vec![0, Pin::P2.mask()]);
    assert_that!(fake.registers[usize::from(TCA_CONFIG)]).is_equal_to(!Pin::P2.mask());
    assert_that!(touch.release().0.transactions).is_equal_to(0);
}

#[test]
fn hold_in_reset_after_press_reports_cancelled() {
    let mut touch = touch([FakeCst820::touch(1, 2, 0)]);
    block_on(touch.changed()).unwrap();

    block_on(touch.hold_in_reset(&mut expander(), Pin::P2)).unwrap();
    let cancelled = block_on(touch.changed());

    assert_that!(cancelled).is_equal_to(Ok(TouchEvent::Cancelled(point(1, 2))));
}

#[test]
fn hold_in_reset_expander_failure_returns_its_error() {
    let mut touch = touch([]);
    let mut expander = Tca9554::new(
        FakeTca9554::new(EXPANDER_ADDRESS).failing(0),
        EXPANDER_ADDRESS,
    );

    let result = block_on(touch.hold_in_reset(&mut expander, Pin::P2));

    assert_that!(result).is_equal_to(Err(ErrorKind::Bus));
}

#[test]
fn hold_in_reset_failed_expander_still_cancels_touch_in_progress() {
    let mut touch = touch([FakeCst820::touch(1, 2, 0)]);
    block_on(touch.changed()).unwrap();
    let mut failing = Tca9554::new(
        FakeTca9554::new(EXPANDER_ADDRESS).failing(0),
        EXPANDER_ADDRESS,
    );

    let result = block_on(touch.hold_in_reset(&mut failing, Pin::P2));
    let cancelled = block_on(touch.changed());

    assert_that!(result).is_equal_to(Err(ErrorKind::Bus));
    assert_that!(cancelled).is_equal_to(Ok(TouchEvent::Cancelled(point(1, 2))));
}

// Regression (t-83): idle probes must prove progress without producing events.
#[test]
fn changed_with_progress_counts_idle_probes() {
    let mut touch = silent_touch([FakeCst820::no_touch(), FakeCst820::touch(5, 6, 0)]);
    let mut progress = 0;
    let event = block_on(touch.changed_with_progress(|| progress += 1));
    assert_eq!(event, Ok(TouchEvent::Pressed(point(5, 6))));
    assert_eq!(progress, 2);
}

// Regression (t-83): a held stationary finger remains a live input task.
#[test]
fn changed_with_progress_counts_unmoved_finger() {
    let mut touch = touch([
        FakeCst820::touch(5, 6, 0),
        FakeCst820::touch(5, 6, 2),
        FakeCst820::touch(7, 6, 2),
    ]);
    block_on(touch.changed()).unwrap();
    let mut progress = 0;
    let event = block_on(touch.changed_with_progress(|| progress += 1));
    assert_eq!(event, Ok(TouchEvent::Moved(point(7, 6))));
    assert_eq!(progress, 2);
}

// Regression (t-83): returned errors prove progress, including invalid reports.
#[test]
fn changed_with_progress_counts_completed_read_errors() {
    let mut touch = touch([Err(ErrorKind::Bus), Ok([2, 0, 0, 0, 0])]);
    let mut progress = 0;
    assert_eq!(
        block_on(touch.changed_with_progress(|| progress += 1)),
        Err(Error::Bus(ErrorKind::Bus))
    );
    assert_eq!(progress, 1);
    assert!(matches!(
        block_on(touch.changed_with_progress(|| progress += 1)),
        Err(Error::InvalidReport { .. })
    ));
    assert_eq!(progress, 2);
}

struct PendingBus;

impl ErrorType for PendingBus {
    type Error = ErrorKind;
}

impl I2c for PendingBus {
    async fn transaction(
        &mut self,
        _address: u8,
        _operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        core::future::pending().await
    }
}

// Regression (t-83): a pending bus read must never receive timer heartbeats.
#[test]
fn changed_with_progress_does_not_count_pending_read() {
    use core::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    let mut touch = Cst820::new(
        PendingBus,
        ADDRESS,
        EdgePin::default(),
        RecordingDelay::default(),
        SIZE,
    );
    let progress = std::cell::Cell::new(0);
    let mut future = pin!(touch.changed_with_progress(|| progress.set(progress.get() + 1)));
    let mut context = Context::from_waker(Waker::noop());
    for _ in 0..10 {
        assert_eq!(future.as_mut().poll(&mut context), Poll::Pending);
        assert_eq!(progress.get(), 0);
    }
}

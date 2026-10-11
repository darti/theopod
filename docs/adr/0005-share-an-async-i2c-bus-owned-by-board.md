# ADR-0005: Share an async I2C bus owned by Board

- Status: Accepted, amended by the t-19 update below
- Date: 2026-10-11

## Context

`Board::init` created a blocking esp-hal I2C driver, used it once to power
up the panel through the TCA9554 expander, then dropped it. The touch
controller sits on the same bus (GPIO15/14), and its reset is an expander
pin, so the touch driver needs both the bus and the expander after boot
(superseded: see the t-19 update, the expander is only needed at boot).
`Tca9554` used blocking `embedded-hal` I2C inside async code. Polling touch
that way would stall the executor, and with it the single UI task, for
every transfer.

Waveshare's ESP-IDF board code (`board_variant.c`, shared by both board
revisions) names the expander pins: EXIO0 `LCD_RST`, EXIO1 `DSI_PWR_EN`,
EXIO2 `TOUCH_RST`, EXIO7 `SD_CS`. It releases EXIO2, then detects the V2
(CO5300 + CST820) by probing the touch controller at 0x15. `PanelPower`
pulsed EXIO0..2, so it also reset the touch controller.

## Decision

- `Tca9554` and `PanelPower` take any `embedded_hal_async::i2c::I2c`; every
  register access is awaited. `FakeTca9554` implements the async trait, and
  the host tests run through `block_on`.
- `Board` keeps the bus as `&'static I2cBus`, an
  `embassy_sync::mutex::Mutex<CriticalSectionRawMutex, I2c<Async>>`
  leaked from the internal heap. Each driver takes an `I2cDevice`
  (`embassy-embedded-hal` shared bus), which locks the bus per transaction.
  The critical-section mutex makes the bus `Sync`: it can be shared with
  another executor (interrupt priority, second core) at the cost of a short
  critical section per lock. Today every task runs on the one esp-rtos
  executor, where `NoopRawMutex` would also work. (t-19 update: `Board`
  no longer exposes the bus; `Board::init` hands out the handles.)
- `PANEL_POWER` drives EXIO0 and EXIO1 only. `Board::init` then makes
  EXIO2 (`TOUCH_RESET`) an output held high, leaving the touch controller
  out of reset for its driver to pulse. (t-19 update: `Cst820::init` now
  configures and pulses EXIO2 itself.)

Out of scope: the touch driver itself (t-19).

## Consequences

- Positive: I2C transfers yield to the executor, so touch polling and
  rendering can interleave.
- Positive: `embassy-embedded-hal` 0.6 was already in the build (through
  esp-hal); no new crate is compiled.
- Negative: an expander read-modify-write spans two bus transactions, so
  two `Tca9554` handles in different tasks could lose each other's
  changes. Drive the expander from one task; who owns it after boot is
  decided with the touch driver (resolved by the t-19 update: no one).
- Negative: the touch controller is no longer reset at boot. After a cold
  boot it relies on its power-on reset; after an MCU-only reset (flash,
  panic, watchdog) the TCA9554 keeps EXIO2 high, so the controller keeps
  its previous state. Its driver must pulse EXIO2 before the first access
  (resolved by the t-19 update).
- Neutral: `Board::i2c` is unread until the touch driver lands; its
  `#[expect(dead_code)]` fails `just lint` (`-D warnings`) once it is read,
  forcing its removal. (Resolved by the t-19 update: the field is gone.)

## Alternatives considered

- `RefCell` around a blocking bus: rejected, every transfer would still
  stall the executor.
- Keep a single `Tca9554` in `Board` instead of the bus: rejected, the
  touch controller also needs the bus itself. Keeping both (bus and a
  shared expander) is left to t-19, the expander's first user after boot
  (decided in the t-19 update).
- `NoopRawMutex`: viable on the single executor used today; not chosen,
  so the bus stays usable from another executor or core.

## Update (t-19): touch driver and expander ownership

- `Board` no longer exposes the bus: `Board::init` hands an `I2cDevice` to
  the CST820 driver and returns it as `Board::touch`. The bus stays
  `'static`, so later devices (IMU, RTC, codec) get their own handle, also
  created in `Board::init`.
- The expander is driven only during `Board::init`. The touch driver pulses
  EXIO2 there (`Cst820::init`, before its first access), then the expander
  handle is dropped: no task owns it after boot, so its read-modify-writes
  cannot race. A later runtime user of the expander must become its single
  owner instead of creating a second handle.
- Consequence: the touch controller cannot be reset at runtime. If it
  wedges, touch stays dead until the next boot; recovering it needs that
  single expander owner.
- The touch task waits for the CST820 interrupt (GPIO21) while idle and
  polls the registers every 10 ms while a finger is down, sharing the bus
  with nothing else at runtime today. A stuck bus would block every device
  on it, so keep esp-hal's I2C timeout enabled.

# ADR-0001: Split HAL-independent drivers from device code

- Status: Accepted
- Date: 2026-10-11
- Amended: 2026-10-15, HAL-free logic and the current `device` items

## Context

The firmware targets one board (Waveshare ESP32-S3-Touch-AMOLED-1.8 V2),
but its drivers (CO5300 panel, TCA9554 expander, panel power-up, button
debouncing, framebuffer flushing) encode protocol rules that are easy to
get wrong and impossible to observe without hardware: opcode and lane
choices, 2x2 area alignment, pin sequencing. The firmware crate only builds
for `xtensa-esp32s3-none-elf`, so nothing in it can run as a host unit test.

## Decision

- `crates/theo-drivers` holds every driver. It is `no_std`, depends only on
  `embedded-hal`, `embedded-hal-async` and its own `QspiBus`
  trait (ADR-0002), and is unit-tested on the host with `just test`. It
  also holds the firmware's HAL-free logic, so it is host-tested too:
  view orientation, flush bands and retry policy, the arithmetic of the
  PSRAM DMA path, and the RAM self-test. Logic needing esp-hal stays in
  `src/device/`.
- `src/device/` is the only code that touches esp-hal, the ESP runtime
  crates, pin numbers, panel geometry and board timings. It exposes
  `entry`, `Board` (display, button, deep sleep, touch, IMU, watchdog),
  `Display` (which owns the PSRAM framebuffer), `Button`, `Touch`,
  `Expander`, `Imu`, `IMU_AXES`, the shared I2C bus types, the main
  `Watchdog` (ADR-0009), `log_start_cause`, the display reboot counter
  (`display_resets`, `reset_for_display`, `clear_display_resets`),
  `log_heap_usage` and, with the `frame-dump` feature, `dump_frame`.
- `src/main.rs`, `src/input.rs` and `src/slint_backend.rs` see the board
  only through `device` and driver traits such as `Panel`.
- Board *values* (expander pins, delays, geometry) stay in `src/device/`;
  the *sequence* using them lives in the drivers (for example
  `PanelPower::power_up`), so tests can enforce its ordering.

Out of scope: a board-support abstraction for several boards at once.

## Consequences

- Positive: protocol rules have regression tests (e.g. "levels before
  outputs" for the panel rails) that run in milliseconds on the host.
- Positive: porting to another board means writing another `device`
  module; drivers and UI code are reused unchanged.
- Negative: each driver needs a fake (`test_support/`) and a thin esp-hal
  adapter (`src/device/esp_qspi.rs`) that is only verified on hardware.
- Neutral: a second crate in the workspace, built for two targets.

## Alternatives considered

- Keep drivers in the firmware crate: rejected, they could not be tested
  on the host.
- Use existing driver crates from crates.io: rejected, none covers the
  CO5300 over QSPI, and the TCA9554 needs are a few registers.

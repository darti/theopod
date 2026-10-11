# ADR-0001: Split HAL-independent drivers from device code

- Status: Accepted
- Date: 2026-10-11

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
  trait (ADR-0002), and is unit-tested on the host with `just test`.
- `src/device/` is the only code that touches esp-hal, the ESP runtime
  crates, pin numbers, panel geometry and board timings. It exposes
  `entry`, `Board` (display, button, PSRAM framebuffer), `Display`,
  `Button`, `log_heap_usage` and, with the `frame-dump` feature,
  `dump_frame`.
- `src/main.rs` and `src/slint_backend.rs` see the board only through
  `device` and driver traits such as `Panel`.
- Board *values* (expander pins, delays, geometry) stay in `src/device/`;
  the *sequence* using them lives in the drivers (for example
  `PanelPower::power_up`), so tests can enforce its ordering.

Out of scope: a board-support abstraction for several boards at once.

## Consequences

- Positive: protocol rules have regression tests (e.g. "levels before
  outputs" for the panel rails) that run in milliseconds on the host.
- Positive: porting to another board means writing another `device`
  module; drivers and UI code are reused unchanged.
- Negative: each driver needs a fake (`test_support.rs`) and a thin esp-hal
  adapter (`src/device/esp_qspi.rs`) that is only verified on hardware.
- Neutral: a second crate in the workspace, built for two targets.

## Alternatives considered

- Keep drivers in the firmware crate: rejected, they could not be tested
  on the host.
- Use existing driver crates from crates.io: rejected, none covers the
  CO5300 over QSPI, and the TCA9554 needs are a few registers.

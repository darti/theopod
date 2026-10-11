# Agent instructions

## Project

Theo Pod is a Rust `no_std` firmware for the ESP32-S3-Touch-AMOLED-1.8 V2 board.
The target, linker, and `core` compilation are configured in `.cargo/config.toml`;
the ESP toolchain and target are defined in `rust-toolchain.toml`.

## Conventions

- Preserve the existing `no_std`/`no_main` model and ESP-HAL startup conventions.
- Display pin assignments, panel geometry, and the TCA9554 power sequence are
  implemented in `src/device/mod.rs`, the single source of truth for board
  wiring. Do not assume touch or audio pin assignments: they are not yet
  documented or implemented.
- Keep ESP- and board-specific code in `src/device/`. Drivers in
  `crates/theo-drivers` must stay HAL-independent (`embedded-hal` traits and
  `QspiBus` only) and keep their host unit tests passing.
- Keep changes compatible with the `xtensa-esp32s3-none-elf` target and the
  dependency versions declared in `Cargo.toml`.
- Use `just` for firmware operations; its recipes configure the required ESP
  environment.

## Verification

- Run `just fmt` to format the code.
- Run `just check` to verify compilation with the ESP toolchain.
- Run `just test` to run the driver unit tests on the host.
- Run `just build` to produce a release firmware image.

Flashing and the serial monitor (`just flash`, `just monitor`, `just run`,
`just flash-monitor`)
require the board to be connected and must not be run without an explicit
request.

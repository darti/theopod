# Agent instructions

## Project

Theo Pod is a Rust `no_std` firmware for the ESP32-S3-Touch-ePaper-1.54 board.
The target, linker, and `core` compilation are configured in `.cargo/config.toml`;
the ESP toolchain and target are defined in `rust-toolchain.toml`.

## Conventions

- Preserve the existing `no_std`/`no_main` model and ESP-HAL startup conventions.
- Do not assume the display, touch, or audio pin assignments: they are not yet
  documented or implemented.
- Keep changes compatible with the `xtensa-esp32s3-none-elf` target and the
  dependency versions declared in `Cargo.toml`.
- Use `just` for firmware operations; its recipes configure the required ESP
  environment.

## Verification

- Run `just fmt` to format the code.
- Run `just check` to verify compilation with the ESP toolchain.
- Run `just build` to produce a release firmware image.

Flashing and the serial monitor (`just flash`, `just monitor`, `just run`)
require the board to be connected and must not be run without an explicit
request.

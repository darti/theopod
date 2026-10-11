# Agent instructions

## Project

Theo Pod is a Rust `no_std` firmware for the ESP32-S3-Touch-AMOLED-1.8 V2 board.
The target, linker, and `core` compilation are configured in `.cargo/config.toml`;
the ESP toolchain and target are defined in `rust-toolchain.toml`.

## Conventions

- Preserve the existing `no_std`/`no_main` model and ESP-HAL startup conventions.
- Display pin assignments, panel geometry, the BOOT button, PSRAM mode and
  frequency, and the TCA9554 power-up values are defined in
  `src/device/mod.rs`, the single source of truth for board wiring; the
  power-up sequence itself is `PanelPower` in `crates/theo-drivers`. The
  CST820 touch wiring (I2C 0x15, reset on TCA9554 EXIO2, interrupt on
  GPIO21) and the shared async I2C bus (ADR-0005) are there too. Do not
  assume the audio pin assignments: they are not yet documented or
  implemented.
- Keep ESP- and board-specific code in `src/device/`. Drivers in
  `crates/theo-drivers` must stay HAL-independent (`embedded-hal` and
  `embedded-hal-async` traits and `QspiBus` only) and keep their host unit tests passing.
- Keep changes compatible with the `xtensa-esp32s3-none-elf` target and the
  dependency versions declared in `Cargo.toml`.
- Record significant technical decisions in `docs/adr/`.
- Layout: `README.md`; driver map in `crates/theo-drivers/ARCHITECTURE.md`.
  Debug tooling: `crates/theo-frame-dump` (`no_std`, shared by the firmware
  and the host) and the host-only `tools/frame-png`.
- Use `just` for firmware operations; its recipes configure the required ESP
  environment.

## Gotchas

- The display DMA reads the PSRAM framebuffer in place only through
  `PsramTxBuf` (64-byte bursts, aligned descriptors) with PSRAM at 80 MHz
  and transfers starting 64-byte aligned; dropping any of these brings back
  SPI underruns, i.e. bands on the panel (ADR-0004).
- `.cargo/config.toml` forces the xtensa target: host builds need
  `RUSTUP_TOOLCHAIN=stable cargo <cmd> --target $(rustc +stable -vV | sed -n 's/^host: //p')`.
- Firmware `log` output is silent unless `ESP_LOG` (e.g. `info`) is set at
  build time (`init_logger_from_env`).
- To see what the firmware renders: `just flash-dump` (records from boot,
  Ctrl-C after `theo-frame-end`), then `just frame-png` → `target/frame.png`.
  Capturing after boot misses the first rows.

## Verification

- Run `just fmt` to format the code.
- Run `just check` to verify compilation with the ESP toolchain.
- Run `just test` to run the host unit tests (drivers, frame dump, frame-png).
- Run `just lint` to run clippy on the firmware and the host crates.
- Run `just build` to produce a release firmware image.

Flashing and the serial monitor (`just flash`, `just monitor`, `just run`,
`just flash-monitor`, `just flash-dump`, `just monitor-log`) require the board
to be connected and must not be run without an explicit request.

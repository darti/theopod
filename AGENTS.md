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
  GPIO21), the QMI8658 address and axis map (`IMU_AXES`, measured on the
  board) and the shared async I2C bus (ADR-0005) are there too. Do not
  assume the audio pin assignments: they are not yet documented or
  implemented.
- Keep ESP- and board-specific code in `src/device/`. Drivers in
  `crates/theo-drivers` must stay HAL-independent (`embedded-hal` and
  `embedded-hal-async` traits and `QspiBus` only) and keep their host unit tests passing.
- Keep changes compatible with the `xtensa-esp32s3-none-elf` target and the
  dependency versions declared in `Cargo.toml`.
- Record significant technical decisions in `docs/adr/`.
- Regression tests live in the side-by-side `*_tests.rs` files, each
  tagged `// Regression (t-N): ...` (or `Regression guard`), not in
  `tests/regressions/` (ADR-0007).
- Layout: `README.md`; driver map in `crates/theo-drivers/ARCHITECTURE.md`.
  Debug tooling: `crates/theo-frame-dump` (`no_std`, shared by the firmware
  and the host) and the host-only `tools/frame-png`.
- Use `just` for firmware operations; its recipes configure the required ESP
  environment.
- Undocumented board wiring: check Waveshare's sources
  (`waveshareteam/ESP32-S3-Touch-AMOLED-1.8`: `pin_config.h`,
  `board_variant.c`; BSP in `waveshareteam/Waveshare-ESP32-components`) and
  cite them in `src/device/mod.rs`.

## Gotchas

- The display DMA reads the PSRAM framebuffer in place only through
  `PsramTxBuf` (64-byte bursts, aligned descriptors) with PSRAM at 80 MHz
  and transfers starting 64-byte aligned; dropping any of these brings back
  SPI underruns, i.e. bands on the panel (ADR-0004).
- `.cargo/config.toml` forces the xtensa target: host builds need
  `RUSTUP_TOOLCHAIN=stable cargo <cmd> --target $(rustc +stable -vV | sed -n 's/^host: //p')`.
- Firmware `log` output is silent unless `ESP_LOG` (e.g. `info`) is set at
  build time (`init_logger_from_env`).
- `IMU_AXES` is measured, not documented: `just imu-calibrate` keeps the
  view upright and logs raw accelerometer axes once a second. Calibrate from
  positions defined against gravity (vertical with text upright, a quarter
  turn clockwise, flat); don't infer axes from how a tilted screen "points".
- Slint renderer behaviour (rotation, dirty regions) can be checked on the
  host with a scratch `std` crate using the same `slint` version and
  `renderer-software`, rendering into a buffer.
- To see what the firmware renders: `just flash-dump` (records from boot,
  Ctrl-C after `theo-frame-end`), then `just frame-png` → `target/frame.png`.
  Capturing after boot misses the first rows.
- In a worktree under `.claude/worktrees/`, Cargo also reads the main
  checkout's `.cargo/config.toml` and links `-Tlinkall.x` twice
  ("region 'vectors_seg' already defined"): prefix link/flash recipes with
  `RUSTFLAGS="-C link-arg=-Tlinkall.x"`.
- `just monitor`, `just run` and `just imu-calibrate` need a TTY; from an
  agent use `just flash-dump` (logs from boot) or `just monitor-log`.
- A flash stuck on "Connecting…": check `lsof /dev/cu.usbmodem*` for a
  process holding the serial port.
- `flash-dump` shows the framebuffer, not the panel: panel-side effects
  (MADCTL, area addressing) need a human looking at the screen. CO5300
  row/column exchange caps written lines at 368 px; don't retry hardware
  landscape rotation (ADR-0006).

## Verification

- Run `just fmt` to format the code.
- Run `just check` to verify compilation with the ESP toolchain.
- Run `just test` to run the host unit tests (drivers, frame dump, frame-png).
- Run `just lint` to run clippy on the firmware and the host crates.
- Run `just miri` after changing `unsafe` code in `crates/theo-drivers`
  (slow, about 5 minutes), and `just audit` after changing dependencies.
- Run `just build` to produce a release firmware image.

Flashing and the serial monitor (`just flash`, `just monitor`, `just run`,
`just flash-monitor`, `just flash-dump`, `just monitor-log`,
`just imu-calibrate`) require the board
to be connected and must not be run without an explicit request.

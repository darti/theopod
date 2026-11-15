# Agent instructions

Theo Pod: Rust `no_std`/`no_main` firmware for the ESP32-S3-Touch-AMOLED-1.8 V2
(target `xtensa-esp32s3-none-elf`; see `.cargo/config.toml`, `rust-toolchain.toml`).

## Rules

- Before any work (code, tests, docs, investigation) create or reuse an Oku
  task. Keep it open until the changes are merged into `main`, then close it;
  a local commit or passing tests do not justify closing it.
- Oku: `oku cli -a http://127.0.0.1:50051 task … --project theo-pod`
  (`OKU_ADDRESS` points to 50151, the wrong port).
- The commit hook requires `Reviewed-by: …` (after `/review`) or
  `Review-waived: <reason>`.
- Keep the `no_std`/`no_main` model and ESP-HAL startup conventions.
- ESP/board code lives in `src/device/`. Drivers in `crates/theo-drivers` stay
  HAL-independent (`embedded-hal`, `embedded-hal-async`, `QspiBus`/`FrameBus`
  only) with passing host unit tests.
- Record significant decisions in `docs/adr/`. ADR numbers race between
  branches: after a fetch take the next free one from
  `git ls-tree --name-only origin/main docs/adr/`, recheck before merging.
- Regression tests live in side-by-side `*_tests.rs`, tagged
  `// Regression (t-N): ...` (ADR-0007).
- Use `just` for firmware operations (it sets the ESP environment).

NEVER:
- add an unbounded wait in the UI loop; use `with_timeout` (ADR-0009);
- remove `PsramTxBuf`, PSRAM at 80 MHz or 64-byte alignment (ADR-0004);
- block on the shared I2C expander to select the SD card; only identification
  may ask core0 for it (ADR-0005, ADR-0023);
- retry hardware landscape rotation (ADR-0006).

`just monitor`, `run` and `imu-calibrate` need a TTY.

## Verification

`just fmt`, `check`, `test`, `lint`, `build`; `just miri` after `unsafe`
changes in `crates/theo-drivers`; `just audit` after dependency changes;
`just test-catalogue-bounds` after changing the catalogue, redb backend or
sync; `just frame-check` after changing the SD owner, podcast runtime or
network task (see `calibration-sd-logging.md`; it leaves a placeholder-
credential image in `target/`, rebuild before flashing).
Never flash or monitor (`flash`, `monitor`, `run`, `flash-monitor`,
`flash-dump`, `monitor-log`, `monitor-passive`, `sleep-check`,
`imu-calibrate`, `sd-provision-v3` (erases the card), `watchdog-*`) without an explicit request.

## Read when (docs/agents/)

- `board-wiring.md`: before touching wiring, pins or I2C.
- `host-build-and-worktrees.md`: before any host build or work in a worktree.
- `serial-and-flash.md`: before any flash or serial monitoring.
- `calibration-sd-logging.md`: before touching IMU axes or the SD logger.

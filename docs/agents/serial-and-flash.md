# Serial, flash and debugging

Read before any flash or serial monitoring. Flashing needs an explicit request.

- Bound serial runs and kill the whole process group, otherwise espflash
  keeps the port. macOS has no `timeout`:
  `perl -e 'setpgrp(0,0); exec @ARGV' just … &` then `kill -- -PID`
  (killing only `just` leaves espflash holding the port).
- `just monitor`, `just run` and `just imu-calibrate` need a TTY; from an
  agent use `just flash-dump` (logs from boot), `just monitor-log` or
  `just monitor-passive`.
- Firmware `log` output is silent unless `ESP_LOG` (e.g. `info`) is set at
  build time. `src/device/logging.rs` installs the dual serial/SD logger
  before HAL startup; normal build/flash recipes default to `info`.
- To see what the firmware renders: `just flash-dump` (records from boot,
  Ctrl-C after `theo-frame-end`), then `just frame-png` -> `target/frame.png`.
  Capturing after boot misses the first rows.
- `flash-dump` shows the framebuffer, not the panel: panel-side effects
  (MADCTL, area addressing) need a human looking at the screen. CO5300
  row/column exchange caps written lines at 368 px; don't retry hardware
  landscape rotation (ADR-0006).
- espflash's monitor (`just monitor-log`, `monitor`, `run`, the `--monitor`
  recipes) resets the chip through DTR/RTS when it connects. To see a
  deep-sleep wake, use `just monitor-passive` (`tools/passive-monitor.py`,
  macOS: no reset observed on open, reattaches after each wake): through
  espflash the wake is replaced by a reset reported as `ChipPowerOn`
  (t-67).
- A flash stuck on "Connecting…": check `lsof /dev/cu.usbmodem*` for a
  process holding the serial port.
- Python tools in `tools/` use the standard library only, and no `just`
  recipe lints or tests them.
- Layout and debug tooling: `README.md`; driver map in
  `crates/theo-drivers/ARCHITECTURE.md`; `crates/theo-frame-dump` (`no_std`,
  shared by the firmware and the host), the host-only `tools/frame-png`, and
  `tools/passive-monitor.py` (serial log without reset).
- Display DMA reads the PSRAM framebuffer in place only through
  `PsramTxBuf` (64-byte bursts, aligned descriptors) with PSRAM at 80 MHz and
  transfers starting 64-byte aligned; dropping any of these brings back SPI
  underruns, i.e. bands on the panel (ADR-0004).

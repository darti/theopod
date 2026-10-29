# Theo Pod

Rust `no_std` firmware targeting the Waveshare ESP32-S3-Touch-AMOLED-1.8 V2.

## Hardware

The V2 board has a 368 × 448 AMOLED display driven by the CO5300 and a CST820
touch controller, using QSPI for the display and I²C for touch. Its other
onboard hardware includes an AXP2101 PMU, PCF85063 RTC, QMI8658 IMU, ES8311
audio codec, and microSD card slot.

This firmware targets the V2 hardware. The display is implemented: the
CO5300 is driven over QSPI and its rails are cycled through the TCA9554 I/O
expander. Slint renders into a framebuffer in PSRAM and only the dirty rows
are sent to the panel by async DMA. The BOOT button (GPIO0) is forwarded to
the UI as the Return key; holding it for 2 s stops the pod (panel asleep
and unpowered, touch and IMU parked, ESP32-S3 in deep sleep) and the next
press starts it again (ADR-0008).
Touches on the CST820 (I²C 0x15, interrupt on GPIO21) reach the UI as
pointer events. The QMI8658 accelerometer (I²C 0x6B or 0x6A)
turns the view in four orientations (ADR-0006). The pin assignments live in
`src/device/mod.rs`. Audio and the other peripherals are not implemented yet. Refer to the
[Waveshare ESP32-S3-Touch-AMOLED-1.8 documentation](https://docs.waveshare.com/ESP32-S3-Touch-AMOLED-1.8)
for the V2 pinout and hardware resources.

## Layout

- `src/main.rs`: Slint event loop; talks to the board only through the
  `device` module and `theo-drivers` types.
- `src/input.rs`: the BOOT button (and its long-press stop), touch and
  orientation tasks, and the channels and signals between them and the
  event loop.
- `src/device/`: everything specific to the ESP32-S3 and this board (chip
  runtime, heaps and PSRAM, logger, pins, panel geometry, esp-hal QSPI DMA
  transport, BOOT button and deep sleep, shared async I2C bus, touch
  controller, IMU and its measured axis map); `restart.rs` holds the main
  watchdog and its reboot cap, the start-cause log and the display reboot
  count, `expander.rs` the shared TCA9554 and the touch controller,
  `i2c_bus.rs` the shared I2C bus (freed at every start), `sleep.rs` deep
  sleep.
- `src/slint_backend.rs`: Slint platform and framebuffer rendering.
- `crates/theo-drivers/`: HAL-independent drivers (CO5300, TCA9554, CST820,
  QMI8658, panel power-up/down, button and long press, `Panel`,
  `QspiBus`/`FrameBus`, framebuffer flushing) and the view-orientation logic,
  unit-tested on the host. See its `ARCHITECTURE.md`.
- `crates/theo-frame-dump/`: `no_std` text format for dumping the framebuffer
  over the serial log (firmware writer, host parser).
- `crates/theo-storage/`: `no_std` bounded log buffering, filtering and
  rotation policy, LittleFS sector backing and owned-sector mailboxes, plus the
  EKV copy-on-write store and optional legacy FAT transport; host-tested by `just test`.
- `tools/frame-png/`: host tool turning a dump in a serial log into a PNG.
- `tools/passive-monitor.py`: serial log recorder (macOS, `python3`) that
  does not reset the chip and follows it across deep-sleep wakes
  (`just monitor-passive`).
- `docs/adr/`: architecture decision records.

## Setup

```sh
just install
```

The Cargo configuration in `.cargo/config.toml` automatically provides the
paths required by the ESP toolchain. No `export-esp.sh` is needed.

## Build and flash

```sh
just check
just test                          # host unit tests (drivers, frame dump, frame-png)
just lint                          # clippy on the firmware and host crates
just miri                          # driver tests under Miri (nightly + miri component)
just audit                         # advisories, licenses, sources (cargo install cargo-deny)
just build
just flash                         # automatically detect the port
just flash /dev/cu.usbmodemXXXX   # specify the port
just monitor /dev/cu.usbmodemXXXX
just run /dev/cu.usbmodemXXXX     # flash and start the serial monitor
just flash-monitor                 # flash, reset, then start the monitor
```

The firmware renders the Slint UI from `ui/main.slint` on the AMOLED display.
It logs heap usage (internal RAM and PSRAM) at start-up and after the first
frame. Normal build/flash recipes set `ESP_LOG=info`; direct Cargo builds
must set it explicitly. Module filters such as `info,theo_pod::input=debug`
are supported by the dual serial/SD logger.

## Persistent logs

An inserted LittleFS microSD is mounted at boot over SPI3 on the V2 board.
The public pure Rust `littlefs-rust` API runs on core 1; the non-Send async SD
and I2C peripherals remain on core 0, reached through bounded owned-sector
messages. A 32 KiB internal stack is reserved for core 1.
The logger captures events from startup and appends level, target and message
to `/theo.log`. Every second a dedicated async task drains a 4 KiB queue;
producers never await filesystem I/O. Records larger than 1 KiB and oldest
queued lines on overflow are dropped and counted. Serial diagnostics remain
available independently of the card.

The active file rotates at 128 KiB into `/theo.1.log` (one previous file is
retained). Each batch is closed and synced. A controlled stop requests a final
flush before deep sleep; a sudden reset can lose the current in-memory batch.
Startup identification/mount waits are bounded at 3 s/2 s, and each write
batch at 500 ms. A card/write/sync error or timeout disables SD logging until
the next boot, preserves the unwritten batch in RAM, and leaves the UI and
serial logging running. Reinsertion takes effect after reboot.

`just flash-log target/serial.log` captures normal firmware logs from boot.
`just sd-check full`, `readonly`, `removed` or `timeout` builds a diagnostic
firmware that injects the corresponding block-write failure without filling
or reformatting the card. `just sd-check rotate` exercises the real rotation
path using isolated `sdtest2.log`/`sdtest2.1` fixtures; it refuses existing fixture
files and leaves the generated files for inspection. `just sd-sleep-check`
performs three actual deep-sleep/timer-wake cycles with a passive monitor.
Restore normal firmware with `just flash` after diagnostic tests.

### Initial card provisioning

Boot never formats media. Existing FAT cards will report a mount failure and
leave the UI and serial logs available. Back up and unmount a card before
explicit provisioning: this replaces its filesystem and uses the whole card,
starting at byte zero. Use its exact capacity in 512-byte sectors:

```sh
just sd-format /dev/rdiskN EXACT_SECTOR_COUNT
```

For an existing sized image, `just sd-format /path/card.img` derives capacity
from the file length. Both firmware and formatter use 4096-byte blocks,
512-byte reads/programs, a 4096-byte cache and 128-byte allocator lookahead.
The firmware does not expose the volume as FAT to a desktop.

Software tests cover reboot/reopen, rotation, sector tears, failed sync and
cross-thread cancellation. These do not replace board validation; the earlier
[SD validation matrix](docs/sd-validation.md) describes the previous FAT build.
Upstream aliasing fixes in [PR #12](https://github.com/PhotomancerArt/littlefs-rust/pull/12)
are still pending: the requested main dependency is locked to `732d038b`.
See [ADR-0012](docs/adr/0012-use-the-safe-littlefs-rust-api.md).

## Podcast storage groundwork

`theo-storage` provides an asynchronous EKV 1.0.0 backend for a preallocated
FAT file (available with `theo-storage/legacy-fat`). Two CRC-protected copies of each logical page preserve the previous
copy while a replacement is written. Incompatible format/configuration headers
are rejected; failed or cancelled I/O disables the backend until reopening
after reboot. See [ADR-0011](docs/adr/0011-adapt-ekv-to-sector-storage-with-copy-on-write-pages.md).

This backend is not initialized by the running firmware. Its FAT-file transport
is retained for host tests, not used by the LittleFS runtime. EKV placement on
a separate raw region remains to be designed; a large random-access database
file would cause costly LittleFS tail rewrites. Radio France
subscription commands, feed synchronization, downloads and space reclamation
remain planned in [the implementation plan](docs/superpowers/plans/2026-10-29-radio-france.md).
It does not establish physical power-loss safety for consumer SD cards or FAT
metadata; this still needs board validation. No wiring or audio playback is added.

## Seeing what the firmware renders

```sh
just flash-dump      # flash with the `frame-dump` feature, record the log from boot
                     # (Ctrl-C after `theo-frame-end`)
just frame-png       # target/frame.log -> target/frame.png
just monitor-log     # record later dumps (one after each BOOT press)
```

`just monitor-log` resets the chip when it connects. To follow the board
across a stop and wake without resetting it, use `just monitor-passive`
(`tools/passive-monitor.py`: macOS, needs `python3`, standard library
only).

## License

The source code is licensed under either of
[Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT),
at your option.

The firmware image also links [Slint](https://slint.dev), which is licensed
under GPL-3.0-only, its royalty-free license, or a commercial license. The
royalty-free license does not cover embedded systems, so a distributed
firmware image must comply with the GPL-3.0 (this source is compatible with
it) unless a Slint commercial license applies.

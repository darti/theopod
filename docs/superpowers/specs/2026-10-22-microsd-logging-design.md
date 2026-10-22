# t-94 microSD logging design

## Goal

Initialize the V2 onboard microSD slot over the ESP32-S3 SDMMC peripheral,
mount ordinary FAT media with `hadris-fat`, and persist a bounded firmware log
without making display/UI startup depend on a card.

## Constraints

- Board wiring remains in `src/device/`: GPIO2/1/3 are the one-bit SDMMC signal group
  and TCA9554 EXIO7 is `SD_CS`.
- The firmware remains `no_std`/`no_main` and uses the existing ESP-HAL startup
  conventions.
- The filesystem uses `hadris-fat` 3.0.0-rc.2 with `async` and `write`.
- Card absence, mount/read/write/full-media/removal failures are recoverable.
- SD operations are bounded and buffered so a UI-loop pass remains within the
  main watchdog budget.
- Host tests cover log formatting, buffering, flush behavior, and failures.

## Design

`src/device/sd.rs` owns the board-specific SDMMC slot and exposes a small
board-local storage service. It configures the controller in 1-bit mode using
the GPIO2 clock, GPIO1 command, and GPIO3 data pin, and drives the
expander's EXIO7 card-select/power signal through the existing shared expander
ownership rules. The SDMMC slot is converted to async mode and wrapped in an
adapter implementing hadris-storage's async `BlockDevice` contract.

`src/storage.rs` is HAL-independent and contains a fixed-capacity log buffer
and sink abstraction. Formatting happens into bounded storage; flushing drains
at most a fixed number of bytes/records per invocation and reports write
failures without panicking. This keeps the policy testable on the host and
prevents a large log backlog from monopolizing the firmware.

`Board::init` attempts SD setup/mount after the required board peripherals are
available. A successful mount starts the storage task/service; every failure is
logged and initialization continues with storage disabled. The UI loop does
not await card I/O directly. Existing serial logging remains the diagnostic
fallback, and deep-sleep/restart paths do not assume the card is present.

## Testing and acceptance

- Host tests prove bounded formatting, FIFO buffering, partial flushes, and
  sink failures preserving unwritten records.
- Firmware `cargo check` proves the concrete ESP-HAL SDMMC and hadris adapter
  compile for `xtensa-esp32s3-none-elf`.
- `just test`, `just check`, and `just lint` pass.
- Hardware validation is limited to the explicitly requested firmware change;
  no flashing or monitor recipe is run without a separate request.


# ADR-0010: Use the onboard microSD over SPI

- Status: Accepted for the V2 board wiring; firmware implementation is tracked by `t-94`.
- Date: 2026-10-22

## Context

The Theo Pod V2 has an onboard microSD slot, but the firmware did not
initialize or mount it. The official Waveshare example identifies the feature
as SDMMC with a FAT filesystem and demonstrates mounting, writing, reading,
renaming, and unmounting files.

The V2 board routes SPI signals to ESP32-S3 GPIO2 (clock), GPIO1
(MOSI), and GPIO3 (MISO). The TCA9554 power expander provides the card-select signal on EXIO7;
the existing board wiring documentation already names that signal `SD_CS`.

## Decision

- Use the ESP32-S3 SPI3 peripheral with card select on TCA9554 EXIO7.
- Adapt `sdio` 0.5.1's native response model to SPI R1/R3/R7 responses.
  ACMD41 returns R1 in SPI mode; CMD58 supplies the OCR. Read CMD9's
  token-prefixed CSD block explicitly because the library's SPI acquisition
  skips it, leaving a bogus four-block capacity. Store CSD words in the
  little-word-endian order expected by `sdio::R2`.
- Mount FAT32 either directly or through an MBR primary FAT32 partition.
- Use the FAT filesystem expected by the Waveshare example and ordinary
  FAT-formatted microSD cards.
- Use `hadris-fat` `3.0.0-rc.2` with its `async` and `write` features behind a
  board-local block-device adapter.
- Keep the SD pin and expander definitions in `src/device/mod.rs`.
- Treat card absence, mount failure, write failure, full media, and removal as
  recoverable runtime conditions; the display and UI must still boot.
- Keep persistent logging buffered and bounded so SD I/O cannot exceed the
  main watchdog budget (ADR-0009).

## Evidence

- [Waveshare ESP32-S3-Touch-AMOLED-1.8 schematic](https://files.waveshare.com/wiki/ESP32-S3-Touch-AMOLED-1.8/ESP32-S3-Touch-AMOLED-1.8.pdf)
- [Waveshare `09_sdmmc` ESP-IDF example](https://github.com/waveshareteam/ESP32-S3-Touch-AMOLED-1.8/tree/main/examples/esp-idf/09_sdmmc)
- `docs/adr/0005-share-an-async-i2c-bus-owned-by-board.md` records EXIO7 as
  `SD_CS` on the board expander.

## Validation

- Host tests cover log formatting, buffering, flushing, write failures, and
  oversized formatted records.
- The firmware-target `cargo check` passes with `hadris-fat`; the SPI block
  device adapter, FAT mount, and bounded boot-log flush are implemented by
  `t-94`.
- Hardware validation on the V2 with the inserted FAT32 card confirms
  122,265,600 sectors (59,700 MiB), mounting its MBR partition at LBA 32768,
  and appending and reading back `theo.log`. Two successive normal-firmware
  boots report 10 then 15 bytes, confirming persistence across reset.
  Captures: `target/sd-normal.log` and `target/sd-reboot.log`.
- Deep-sleep persistence and card-absence/removal behavior still need
  hardware validation.

## Consequences

- SD support is board-specific at initialization while the log sink can
  remain reusable and HAL-independent.
- Exact electrical behavior and sustained write performance still require
  hardware validation on the V2 board.

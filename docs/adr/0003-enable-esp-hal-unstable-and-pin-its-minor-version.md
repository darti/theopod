# ADR-0003: Enable esp-hal `unstable` and pin its minor version

- Status: Accepted
- Date: 2026-10-11

## Context

esp-hal 1.x splits its API into stable and `unstable` parts. The firmware
needs several unstable items: half-duplex SPI writes with separate
command/address/data modes (`half_duplex_write_async`), SPI DMA
(`with_dma`, `into_async`), `min_async_transfer_size`, and the PSRAM driver.
Unstable APIs are outside esp-hal's semver guarantees and can change in a
minor release.

## Decision

- Enable the `unstable` feature of `esp-hal`.
- Pin the dependency with a tilde requirement (`~1.2`) in the workspace
  `Cargo.toml`, so only patch releases are picked up automatically.
- Keep every unstable call inside `src/device/` (ADR-0001), so an upgrade
  touches one module.

## Consequences

- Positive: access to QSPI, DMA and PSRAM, which the display needs.
- Positive: `cargo update` cannot break the build through an unstable API
  change.
- Negative: moving to esp-hal 1.3+ is a deliberate upgrade that may require
  code changes in `src/device/`.
- Negative: `esp_qspi::MAX_TRANSFER_LEN` is derived from esp-hal's private
  SPI `MAX_DMA_SIZE` (32 736 in 1.2.2, rounded down to 64-byte bursts);
  re-check it on every esp-hal upgrade, patch releases included, with its
  copy `PANEL_TRANSFER` in `crates/theo-drivers/src/framebuffer_tests.rs`.
- Neutral: the companion ESP crates (`esp-alloc`, `esp-rtos`, ...) must be
  upgraded together with esp-hal.

## Alternatives considered

- Stay on stable APIs only: rejected, the display cannot be driven without
  half-duplex QSPI.
- Use a caret requirement (`1.2`): rejected, a minor release could change
  unstable APIs under us.
- Use ESP-IDF (`esp-idf-hal`): rejected, the project is `no_std` on bare
  metal.

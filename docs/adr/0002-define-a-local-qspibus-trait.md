# ADR-0002: Define a local QspiBus trait

- Status: Accepted
- Date: 2026-10-11

## Context

The CO5300 is driven over Quad-SPI with an 8-bit opcode and a 24-bit
address on one line, then data on one or four lines. `embedded-hal` and
`embedded-hal-async` only model single-line SPI, so a driver written
against them cannot express the quad data phase. The driver must stay
HAL-independent (ADR-0001).

## Decision

`theo-drivers` defines `QspiBus`:

```rust
pub trait QspiBus {
    type Error: Debug;
    const MAX_TRANSFER_LEN: usize;
    async fn write(&mut self, opcode: u8, address: u32, lanes: Lanes, data: &[u8])
        -> Result<(), Self::Error>;
}
```

- Command and address phases are always single-line; only data uses
  `lanes`. Chip select stays asserted for the whole transfer.
- The trait is async so DMA implementations can yield (ADR-0004).
- `MAX_TRANSFER_LEN` lets drivers split long payloads; the CO5300 repeats
  "memory write continue" per chunk.
- The ESP32-S3 implementation is `src/device/esp_qspi.rs`.

Out of scope: reads, dummy cycles, and quad command/address phases, which
no current device needs.

## Consequences

- Positive: the CO5300 driver is host-tested against a recording bus,
  including opcode, lane and chunking choices.
- Positive: the trait is small enough to implement on another HAL in a few
  dozen lines.
- Negative: a project-specific trait; other drivers in the ecosystem will
  not implement it, and the adapter is only verified on hardware.
- Neutral: `async fn` in a public trait carries no `Send` bound, which fits
  the single-threaded executor.

## Alternatives considered

- Depend on esp-hal's SPI type in the driver: rejected, it ties the driver
  to one HAL and prevents host tests.
- Emulate quad writes with `embedded-hal` `SpiBus`: rejected, the data
  phase lane count cannot be expressed.
- Wait for a standard Quad-SPI trait: rejected, none exists today.

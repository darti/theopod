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

## Update (t-46): the bus owns the framebuffer

`QspiBus` also holds the panel's framebuffer: `frame()`, `frame_mut()`,
and `write_frame(opcode, address, lanes, bytes: Range<usize>)`, which sends
a byte range of it. A DMA implementation reads the framebuffer in place
(ADR-0004), so it must know which bytes are the frame: an explicit range
replaces the address-range test `EspQspi` used on `write`'s slices, and its
`unsafe` constructor. `Panel` follows (`frame`, `frame_mut`, `write_frame`
instead of `write_pixels`), and `flush_rows`/`flush_bands` no longer take
the frame as an argument.

Consequence: rendering and sending share the one framebuffer the bus
holds (`frame_mut` and `write_frame` both borrow it), so double buffering
(drawing the next frame during the DMA of the previous one) needs a trait
change; ADR-0004 already keeps it out of scope.

## Update (t-58): the framebuffer moves to a `FrameBus` subtrait

`QspiBus` is back to a transport: `Error`, `MAX_TRANSFER_LEN` and
`write`. `FrameBus: QspiBus` adds `frame`, `frame_mut` and `write_frame`
(whose length stays fixed); `Co5300`'s constructor and `Panel` impl
require it. A command-only QSPI device no longer has to carry a frame.
`Co5300`'s register methods (`init`, `sleep`) are written against
`QspiBus` only, so generic code holding a `Co5300` (a panel recovery, for
example) needs no frame bound, but a `Co5300` is always built on a
`FrameBus`. Double buffering is still out of reach: `Panel`, `flush_rows`
and the shared `&mut self` borrow of the one frame would all have to
change, not only `FrameBus` (ADR-0004 keeps it out of scope).
`Co5300::new` also checks that the bus frame holds exactly one RGB565
frame of the geometry (`Error::FrameSizeMismatch`), so a wrong-sized
framebuffer fails at construction rather than on a flush.

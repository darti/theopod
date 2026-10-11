# ADR-0004: Render into a PSRAM framebuffer sent by async DMA

- Status: Accepted
- Date: 2026-10-11

## Context

Slint first rendered line by line, and each line went to the panel through
a blocking SPI write fed by the CPU. Every frame was a full repaint, the
CPU stayed busy for the whole transfer, and no other task (button, future
touch) could run meanwhile. A full 368 x 448 RGB565 frame is 329,728
bytes, more than the internal heap can spare, but the ESP32-S3R8 has 8 MB
of PSRAM that the GDMA can read.

## Decision

- `Board::init` maps the octal PSRAM, adds it as an esp-alloc region, and
  allocates one zeroed full-panel framebuffer there with
  `allocator_api2` + `esp_alloc::ExternalMemory`.
- Slint renders into it with `RepaintBufferType::ReusedBuffer` and
  `Rgb565BigEndianPixel`, so pixels are already in panel byte order and
  only dirty regions are redrawn.
- `theo_drivers::flush_rows` sends the dirty rows, widened to even rows and
  full width, as one contiguous area; `Co5300` splits it into transfers of
  `QspiBus::MAX_TRANSFER_LEN` (32,736 bytes for esp-hal SPI DMA).
- `QspiBus` and `Panel` are async; `EspQspi` uses
  `SpiDma::half_duplex_write_async`. Transfers under 64 bytes (register
  writes) use the SPI FIFO, skipping the DMA setup cost.
- The DMA never reads PSRAM: `EspQspi` copies each transfer into a 32 KB
  static bounce buffer in internal RAM (outside the heap) first. Sending
  straight from PSRAM showed a stale band (~16 KB, top of the first frame)
  on hardware, although the framebuffer dumped by the CPU was correct and
  esp-hal writes the cache back before each transfer. The root cause in the
  cache/DMA path is not understood (tracked in t-4); the copy removes the
  band and also covers payloads in flash.
- If a flush fails, the next wake-up resends the whole frame: the
  framebuffer stays the source of truth.

Out of scope: double buffering (rendering the next frame during the DMA of
the previous one).

## Consequences

- Positive: idle UI sends nothing; small changes send only the affected row
  band; the executor runs other tasks (the button task) during transfers.
- Positive: no per-line byte swap; flushing logic is host-tested.
- Negative: 330 KB of PSRAM is permanently reserved, and CPU rendering
  into PSRAM is slower than internal RAM.
- Negative: the bounce buffer takes 32 KB of internal RAM (a static, so the
  64 KB heap stays whole), and the CPU copies every byte sent (about 330 KB
  for a full frame).
- Negative: PSRAM is part of the global heap, so general allocations spill
  there once internal RAM is full; atomics must not live in PSRAM on the
  ESP32-S3.
- Neutral: flushing awaits the DMA, so rendering and transfer still run
  back to back within one task.

## Alternatives considered

- Keep line rendering with DMA per line: rejected, 736-byte transfers pay
  DMA setup on every line, and full repaints remain.
- Framebuffer in internal RAM: rejected, it does not fit next to the heap
  and stacks.
- DMA straight from PSRAM: rejected after the stale band seen on hardware;
  revisit if the cache writeback issue is understood (would save the copy
  and 32 KB of internal RAM).
- Partial-width areas: rejected, rows of a full-width framebuffer are not
  contiguous, so each row would need its own transfer.

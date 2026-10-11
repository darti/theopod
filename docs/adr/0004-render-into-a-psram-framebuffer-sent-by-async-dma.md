# ADR-0004: Render into a PSRAM framebuffer sent by async DMA

- Status: Accepted
- Date: 2026-10-11
- Amended: 2026-10-11, DMA reads the framebuffer in place (t-4)

## Context

Slint first rendered line by line, and each line went to the panel through
a blocking SPI write fed by the CPU. Every frame was a full repaint, the
CPU stayed busy for the whole transfer, and no other task (button, future
touch) could run meanwhile. A full 368 x 448 RGB565 frame is 329,728
bytes, more than the internal heap can spare, but the ESP32-S3R8 has 8 MB
of PSRAM that the GDMA can read.

## Decision

- `Board::init` maps the octal PSRAM at 80 MHz, adds it as an esp-alloc
  region, and allocates one zeroed full-panel framebuffer there with
  `allocator_api2` + `esp_alloc::ExternalMemory`, 64-byte aligned.
- Slint renders into it with `RepaintBufferType::ReusedBuffer` and
  `Rgb565BigEndianPixel`, so pixels are already in panel byte order and
  only dirty regions are redrawn.
- `theo_drivers::flush_rows` sends the dirty rows, widened to even rows and
  full width, as one contiguous area; `Co5300` splits it into transfers of
  `QspiBus::MAX_TRANSFER_LEN` (32,704 bytes: the largest multiple of 64
  within esp-hal's 32,736-byte SPI DMA limit). Every transfer then starts
  64-byte aligned (even rows are 1,472 bytes, 23 x 64). A compile-time
  assert in `src/device/mod.rs` keeps two panel rows a multiple of 64
  bytes, and a host test pins the transfer starts; `EspQspi` refuses a
  framebuffer payload that is misaligned or too long
  (`EspQspiError::Misaligned`, `TooLong`) rather than send it slowly.
- `QspiBus` and `Panel` are async; `EspQspi` uses
  `SpiDma::half_duplex_write_buffer` for framebuffer transfers and
  `half_duplex_write_async` for the rest. Transfers under 64 bytes
  (register writes) use the SPI FIFO, skipping the DMA setup cost.
- The DMA reads the framebuffer in place. `EspQspi` sends framebuffer
  transfers through `PsramTxBuf`, a `DmaTxBuffer` of ours: 64-byte PSRAM
  bursts, descriptors of 4,032 bytes that each start a whole burst, and a
  cache write-back of each descriptor (interrupts off for at most 4 KB, as
  ESP-IDF guards the S3 write-back errata) before the DMA starts. The
  guard assumes only core 0 runs: starting the app core needs ESP-IDF's
  cache freeze instead. esp-hal's `half_duplex_write_async(&[u8])` cannot
  do this: it reads PSRAM in 16-byte bursts through 4,095-byte
  descriptors, unaligned after the first.
- After each framebuffer transfer, `EspQspi` reads the SPI's "DMA TX slower
  than SPI" flag (`DMA_OUTFIFO_EMPTY_ERR`) and fails with
  `EspQspiError::Underrun` if it is set, so the frame is resent. The flag
  is proven: during t-4 it fired on exactly the transfers that showed
  bands.
- If a flush fails, every following wake-up (one frame period apart)
  resends the whole frame until one succeeds: the framebuffer stays the
  source of truth. Only the first failure of a streak and the recovery are
  logged; a lasting failure means the bandwidth margin is gone.

Out of scope: double buffering (rendering the next frame during the DMA of
the previous one).

## Consequences

- Positive: idle UI sends nothing; small changes send only the affected row
  band; the executor runs other tasks (the button task) during transfers.
- Positive: no per-line byte swap; flushing logic is host-tested.
- Negative: 330 KB of PSRAM is permanently reserved, and CPU rendering
  into PSRAM is slower than internal RAM.
- Negative: `PsramTxBuf` relies on esp-hal's unstable DMA buffer API and on
  ROM cache routines provided by esp-rom-sys's linker scripts; re-check
  both on every esp-hal upgrade (ADR-0003).
- Negative: other PSRAM payloads of 64 bytes or more (heap spill) still go
  through esp-hal's 16-byte bursts, too slow for long transfers.
- Negative: cancelling a framebuffer transfer loses the SPI master, and
  the display stops (`EspQspiError::Unavailable`); nothing cancels a flush.
- Negative: PSRAM runs at 80 MHz without the MSPI timing tuning ESP-IDF
  performs (esp-hal 1.2.2 has none); 40 MHz with the bursts and alignment
  was not tried.
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
- Copy every transfer into a 32 KB internal-RAM bounce buffer: the first
  implementation. It cost 32 KB of internal RAM and a CPU copy of every
  byte sent; replaced once t-4 found the cause of the bands.
- Patch esp-hal (vendored) for 64-byte bursts: worked, but forks a
  dependency; the public `DmaTxBuffer` trait gives the same result.
- Partial-width areas: rejected, rows of a full-width framebuffer are not
  contiguous, so each row would need its own transfer.

## Investigation (t-4)

DMA straight from PSRAM first showed bands on the panel while the CPU view
of the framebuffer was right. Each band was an SPI underrun: the DMA could
not read PSRAM as fast as the 40 MHz quad SPI sends (20 MB/s), and the SPI
stalled mid-transfer. Three causes added up, each confirmed on hardware with
the underrun flag:

- Transfer starts alternating between 0 and 32 mod 64 (32,736-byte
  transfers) underran every other transfer: fixed by 64-byte alignment.
- PSRAM at esp-hal's default 40 MHz left too little bandwidth next to the
  CPU caches: 80 MHz removed most underruns.
- 16-byte bursts and unaligned descriptors: 64-byte bursts with
  burst-aligned descriptors removed the last one.

Ruled out: cache coherency (a whole-frame write-back changed nothing), the
FIFO-to-DMA switch before the first transfer, and the order of esp-hal's
SPI AFIFO reset.

# ADR-0004: Render into a PSRAM framebuffer sent by async DMA

- Status: Accepted
- Date: 2026-10-11
- Amended: 2026-10-11, DMA reads the framebuffer in place (t-4)
- Amended: 2026-10-14, cancel-safe flushes (t-3, t-29, t-31), host-tested
  DMA arithmetic (t-30), flush retry policy (t-1, t-28), several row bands
  per frame (t-5), PSRAM clock measured and self-tested (t-27)

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
- `theo_drivers::flush_bands` sends the dirty rows as up to three bands
  (`DirtyRows`, one per row span of Slint's at most three dirty
  rectangles; bands touching once widened are merged, and past three the
  closest two). Each band goes through `flush_rows`, widened to even rows
  and full width, as one contiguous area; `Co5300` splits it into transfers of
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
  descriptors, unaligned after the first. The descriptor chunking and the
  framebuffer-payload check (bounds, length, alignment) are pure functions
  in `theo_drivers::dma_layout`, host-tested (t-30).
- After each framebuffer transfer, `EspQspi` reads the SPI's "DMA TX slower
  than SPI" flag (`DMA_OUTFIFO_EMPTY_ERR`) and fails with
  `EspQspiError::Underrun` if it is set, so the frame is resent. The flag
  is proven: during t-4 it fired on exactly the transfers that showed
  bands.
- If a flush fails, the panel is stale and the next flush sends every row:
  the framebuffer stays the source of truth (`theo_drivers::FlushRetry`).
  Errors say whether a retry can succeed (`Transient`). Transient ones
  (`Underrun`, unknown SPI errors, chip select) are resent after 16 ms,
  doubling up to 1 s; a lasting underrun means the bandwidth margin is
  gone. Permanent ones (`TooLong`, `Misaligned`, `Unavailable`, esp-hal's
  DMA setup errors and `MaxDmaTransferSizeExceeded`, rows or areas out of
  range) schedule no resend: before t-1 they were resent every 16 ms
  forever, flooding the log and keeping the loop awake. While the panel is
  stale, the loop flushes nothing until the resend is due, so redraws (an
  animation) cannot bypass the backoff; after a permanent failure a redraw
  schedules a resend 1 s later, so an animation retries at most once a
  second. A change of kind restarts the backoff. Each kind is logged once
  per streak, and the recovery is logged. A failed or cancelled transfer
  closes the `Co5300` area: its progress on the panel is unknown (an
  underrun is only seen once every byte is out).
- At boot, `Board::init` clears the panel with one full-frame flush, whose
  first transfer is `MAX_TRANSFER_LEN` long; a permanent failure panics,
  so an esp-hal upgrade that lowers its private DMA limit fails at once
  rather than at every frame.

Out of scope: double buffering (rendering the next frame during the DMA of
the previous one).

## Consequences

- Positive: idle UI sends nothing; small changes send only the affected row
  bands; the executor runs other tasks (the button task) during transfers.
- Positive: no per-line byte swap; flushing logic is host-tested.
- Negative: 330 KB of PSRAM is permanently reserved, and CPU rendering
  into PSRAM is slower than internal RAM.
- Negative: `PsramTxBuf` relies on esp-hal's unstable DMA buffer API and on
  ROM cache routines provided by esp-rom-sys's linker scripts; re-check
  both on every esp-hal upgrade (ADR-0003).
- Negative: other PSRAM payloads of 64 bytes or more (heap spill) still go
  through esp-hal's 16-byte bursts, too slow for long transfers.
- Neutral: flushes are cancel-safe (t-3, t-29). Dropping `EspQspi::write`
  releases chip select (`theo_drivers::ChipSelect`) and, mid-framebuffer
  transfer, stops the DMA and puts the SPI master back (`InFlight`);
  `Co5300` refuses further pixels until a new area is opened. The rows of
  a cancelled flush are stale and must be sent again.
- Negative: leaking a write future (`mem::forget`) skips those guards: the
  SPI master is lost (`EspQspiError::Unavailable`) and the DMA keeps
  reading the framebuffer after the write's borrow ended. `EspQspi::new` is
  `unsafe` and requires a framebuffer that is never freed, so that read
  stays in live memory and can only show wrong pixels, as with esp-hal's
  own in-place DMA (t-31).
- Negative: PSRAM runs at 80 MHz without the MSPI timing tuning ESP-IDF
  performs (esp-hal 1.2.2 has none). 40 MHz is not an option even with
  the bursts and alignment (t-27): resending the full frame every 16 ms,
  81% of 1,000 flushes underran with an idle CPU, and every one with a
  task copying PSRAM meanwhile; 80 MHz underran none of 2,000 in the same
  test. To catch a marginal clock, `Board::init` pattern-tests the
  framebuffer region (`theo_drivers::check_pattern`, an address hash and
  its complement in 32-bit words, 81 ms) and panics on a mismatch; the
  last data-cache-sized part may be read back from the cache.
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

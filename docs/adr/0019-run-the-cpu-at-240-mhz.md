# ADR-0019: Run the CPU at 240 MHz and build for speed

- Status: Accepted
- Date: 2026-11-14

## Context

`esp_hal::init(esp_hal::Config::default())` leaves the ESP32-S3 CPU at
80 MHz: in esp-hal 1.2.2, `CpuClock::_80MHz` is the `#[default]`
(`src/soc/esp32s3/clocks.rs`). Everything the firmware does on the CPU
runs at a third of the chip's speed: the Slint software renderer, the
software rotation (ADR-0006), TLS and the storage CRCs. TLS shows the
cost: at 80 MHz, AES-128-GCM takes 36 ms per 16 KiB record even with the
AES peripheral, 33 ms of it software GHASH (t-154). The firmware never
chose that clock; it inherited it. Release builds were also optimised for
size (`opt-level = "s"`), with only `esp-radio` at `opt-level = 3`, while
the image uses about 2.3 MB of a 16 MB app partition.

## Decision

- `CPU_CLOCK` in `src/device/mod.rs` sets `CpuClock::_240MHz`, passed to
  `esp_hal::Config::with_cpu_clock` (t-157).
- No peripheral timing moves with it. The 240 MHz preset keeps the PLL at
  480 MHz, so USB Serial/JTAG is unaffected; SPI (display, SD) and the TIMG
  timers (watchdogs, Embassy time driver) run from the fixed 80 MHz APB;
  I2C and SYSTIMER run from the 40 MHz crystal. The firmware has no
  cycle-counted delays; `IDLE_SPIN_LIMIT` (`esp_qspi.rs`) is a poll count
  whose duration shrinks, with a margin far above what it bounds.
- PSRAM stays at 80 MHz (ADR-0004).
- The release profile builds everything at `opt-level = 3` instead of
  `"s"`: `.text` grows from 2,306,741 to 2,975,953 bytes (+29%), well
  within the partition. Dev builds stay at `"s"`, `esp-radio` at 3.

## Measurements

On the board (2026-11-14), with a temporary build, since removed, that
redrew and resent the whole 368 x 448 frame every 16 ms while a task
copied PSRAM in 4 KiB chunks whenever the UI loop awaited (the t-27
conditions of ADR-0004), both builds at `opt-level = 3`:

| CPU | Frames | Failed flushes | Render avg / max | Flush avg / max |
| --- | --- | --- | --- | --- |
| 80 MHz | 1,200 | 0 | 25.8 / 27.4 ms | 31.8 / 44.3 ms |
| 240 MHz | 1,800 | 0 | 15.7 / 16.7 ms | 29.8 / 40.1 ms |

- A full-frame render is 39% shorter (1.64x faster): less than the 3x of
  the clock, since the renderer also waits on PSRAM.
- The flush barely moves: it is bound by the display bus, not the CPU.
- No DMA underrun at 240 MHz under PSRAM load: the ADR-0004 margin holds.
- Not measured: `opt-level = 3` against `"s"` on its own, power draw, and
  160 MHz.

## Consequences

- Positive: faster rendering and CPU-bound work (TLS GHASH, CRCs) with no
  code change.
- Negative: higher active current, and somewhat higher idle current while
  the executor waits, on a battery device. Deep sleep (ADR-0008) is not
  affected.
- Negative: a larger image, so longer flashing; still far from the
  partition size.
- Also checked on the board: the normal image boots without panic or
  watchdog reset, with no bands on the panel and IMU rotation working. The
  microSD LittleFS mount failure seen there is identical at 80 MHz, so
  unrelated.

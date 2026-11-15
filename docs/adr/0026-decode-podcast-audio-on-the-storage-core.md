# ADR-0026: Decode podcast audio on the storage core and output it through I2S DMA

- Status: Proposed (decoder benchmark and test tone measured on the board
  2026-11-15, see "Board measurements"; playback not implemented)
- Date: 2026-11-15
- Related: t-164 (parent t-153); ADR-0004 (PSRAM DMA), ADR-0005 (shared I2C),
  ADR-0018 (bounded requests, `Scheduler`), ADR-0020 (media units, leases),
  ADR-0022 (one commit per podcast request), ADR-0023 (SD on core1), t-156
  (Range resume), t-163 (core0 starvation)

## Context

Playback does not exist yet. Goals: use the ESP32-S3 hardware as much as
possible, and start playing an episode while it is still downloading.

### What the firmware downloads (measured 2026-11-15)

Ten Radio France feeds from `rss-rf.aerion.me` (about 1,000 items) and Range
probes of the newest file of each format per feed:

| Format | Share | Codec | Rate | Bitrate | Container |
|---|---|---|---|---|---|
| `.m4a` | 7 of 10 shows almost entirely | AAC-LC (AOT 2) | 48 kHz stereo | 192 kbps CBR | MP4, `moov` at offset 28 (faststart), 104–610 KB |
| `.mp3` | shows ended ≤ 2022 (La Méthode scientifique 100/100, Sur les épaules de Darwin 98/100), rebroadcasts | MPEG-1 Layer III | 44.1 kHz joint stereo | 128 kbps CBR | ID3v2 19–386 KB (cover art), Xing/Info frame |

All nine AAC files (2020–2026) and all six MP3 files (2015–2022) had the same
parameters. The `moov` size is the per-frame `stsz` table (4 B × 46.9
frames/s; a 39 min episode: 109,816 frames, 439 KB, only 54 `stco` chunks;
frames 338–875 B, average 512 B). RSS declares `audio/mpeg` for both formats,
so the decoder is chosen from the file magic (`ftyp` vs ID3/`0xFFE` sync),
not the feed. Both decoders are needed.

### Board audio wiring (Waveshare sources)

Sources: `waveshareteam/ESP32-S3-Touch-AMOLED-1.8` at `78e13f85`
(`examples/arduino-v2/libraries/Mylibrary/pin_config.h`,
`examples/arduino-v2/examples/15_ES8311/`, `examples/esp-idf/12_i2s_codec/`),
BSP `waveshareteam/Waveshare-ESP32-components` at `23a6012d`
(`bsp/esp32_s3_touch_amoled_1_8/include/bsp/esp32_s3_touch_amoled_1_8.h`),
and the schematic linked from the Waveshare wiki
(`files.waveshare.com/wiki/ESP32-S3-Touch-AMOLED-1.8/ESP32-S3-Touch-AMOLED-1.8.pdf`).

| Function | Wiring | Citation |
|---|---|---|
| Codec | ES8311 (mono DAC + ADC), I2C 0x18 (CE to GND) on the shared bus SDA 15 / SCL 14 | `es8311.h` l.17–19; BSP header l.36–37 |
| I2S MCLK / BCLK / WS | GPIO16 / GPIO9 / GPIO45; S3 master, MCLK = 256 × Fs | BSP header l.39–41; `pin_config.h` l.20–23 |
| I2S DOUT → DSDIN / DIN ← ASDOUT | GPIO8 / GPIO10 | BSP header l.42–43; example `i2s.setPins(...)` l.49; schematic |
| Amplifier | NS4150B, enable GPIO46 active high (10 k pull-down: off at reset), not the TCA9554 | BSP header l.44; `pin_config.h` l.31 |
| Speaker / mic | One 8 Ω 1 W speaker (mono); one analog MEMS mic on MIC1 | schematic H2, MIC1 |

`pin_config.h` l.29–30 also defines `DOPIN 10` / `DIPIN 8`, apparently
swapped and unused; the example and the schematic agree on 8 = out, 10 = in.
V1 and V2 wiring is identical; Waveshare's IDF example raises the volume on V2
("lower level observed on V2 hardware"). Waveshare examples use 16 kHz and
22.05 kHz; the ES8311 driver accepts 8–96 kHz. Details:
side-session notes `t-164-board-audio.md`.

### What the ESP32-S3 accelerates

- **No hardware MP3/AAC decoder.** Decoding is software.
- **I2S + GDMA:** sample output costs no CPU beyond refilling a DMA ring.
- **ES8311:** DAC, digital volume (reg 0x32), so no software gain stage. Its
  DAC is mono: stereo is downmixed (in software, ~1 MHz, or by the codec's
  channel select).
- **Second core:** the largest real gain (see placement).
- **FPU:** single precision; rustc emits `mul.s` + `add.s` where GCC fuses
  `madd.s`. No 64-bit MAC instruction (`mull` + `mulsh`): fixed-point decoders
  run an estimated 1.3–1.7× slower than their ARM figures.
- **PIE (128-bit SIMD, `EE.*`):** integer lanes only (8/16/32-bit), no float.
  LLVM never auto-vectorizes to it and there are no Rust intrinsics
  (esp-rs/rust#190). Inline `asm!` with `ee.vld.128.ip`,
  `ee.vmulas.s16.accx`, `ee.fft.r2bf.s16` compiles with the esp rustc and
  disassembles correctly (checked locally, not run). Each kernel must be one
  self-contained asm block (the compiler knows no Q/ACCX registers); whether
  esp-hal/esp-rtos enables the PIE coprocessor and saves its state is
  unverified. esp-dsp (Apache-2.0) has S3 assembly for the s16 FFT (256 pt:
  3,412 cycles on S3 vs 45,746 on ESP32), dot products and FIR; usable by
  vendoring the `.S` files. No candidate decoder uses PIE.
- **AES/SHA peripherals:** already used by TLS (ADR-0024); irrelevant here.

### Decoder candidates (CPU at 240 MHz; [est] = estimate)

| Decoder | Format | Language / licence | State | CPU |
|---|---|---|---|---|
| Helix AAC (fixed point) | AAC-LC | C, RealNetworks RPSL/RCSL | ~21 KB + 27 KB tables | 28–45 MHz [est], from ARM9E 17/21 MHz at 44.1k/128k |
| `esp_audio_codec` | AAC, MP3 | closed `.a` (4.4 MB), "Espressif Modified MIT" | AAC 51 KB heap, ~20 KB stack | Espressif table: AAC-LC 48k stereo 6.75 %, MP3 8.17 % (≈ 16–20 MHz if per core; AAC figure from a 90 kbps test) |
| `symphonia-codec-aac` | AAC-LC | Rust, MPL-2.0, f32, `std` only | – | 30–60 MHz [est] after a `no_std` fork |
| `nanomp3-core` 0.2 | MP3 | Rust `no_std`, no alloc, no `unsafe`, MIT/Apache, f32, bit-exact with minimp3 | ~18.6 KB | 20–40 MHz [est] |
| Helix MP3 | MP3 | C, RPSL/RCSL | not measured | 22–35 MHz [est] |
| esphome micro-mp3 (OpenCore) | MP3 | C, Apache-2.0, fixed point | ~22.8 KB | 18.2 MHz (published RTF 0.076, 128 kbps 48k stereo) |

Excluded: faad2 (GPL), fdk-aac (C++, no patent grant, heavy), symphonia MP3
(`std`), puremp3 (`std`, unmaintained), minimp3 bindings (flagged unsound).
No `no_std` MP4 demuxer exists (`mp4`, `mp4parse`, `mp4-atom` need `std`).
The ESP toolchain's `xtensa-esp32s3-elf-gcc` 16.2 compiles freestanding C
(`-O2 -mlongcalls -ffreestanding`); the `cc` crate needs
`CC_xtensa_esp32s3_none_elf` / `AR_…`, and `malloc` must be shimmed onto
esp-alloc. Details and sources: side-session notes `t-164-decoders.md`.

### Load today

- Core0: Slint UI, input (touch/IMU on the shared I2C bus), Wi-Fi and TLS.
  During a download it decrypts TLS records back to back (~40 ms per record in
  one poll); that already starved touch and the IMU until t-163.
- Core1: the SD owner on a 48 KiB stack. Card I/O is blocking (SPI3 polled,
  `block_on`): ~19 ms per 16 KiB record, ~230 ms per redb commit, up to the
  1 s `PODCAST_BUDGET`. During a download it handles 1.0–1.25 s per MiB at
  3.7–3.9 s/MiB, i.e. roughly 30 % busy. The SD worker's deepest chain left
  ~2.4 KB of headroom at the last `frame-check` (42.7 / 45.1 KB, t-153).
- esp-rtos 0.4 exposes `embassy::InterruptExecutor` (up to 4, software
  interrupts) but not its preemptive threads (`create_task` is `pub(crate)`
  for esp-radio).

## Decision (proposed)

1. **Decoders.** AAC-LC: Helix AAC (measured 22 MHz), built by `build.rs`
   with the ESP GCC as the benchmark does, after a licence review of
   RPSL/RCSL; `esp_audio_codec` stays the fallback. MP3: Helix MP3 from the
   same sources (to be measured), not `nanomp3-core`: the Xtensa LLVM backend
   only builds it at opt-level 1, where it needs 101 MHz (42 % of a core).
   Revisit `nanomp3-core` if that backend bug is fixed. A host-tested `no_std` crate
   (`crates/theo-audio`) holds format sniffing, a hand-written MP4 reader
   (`moov/trak/mdia/minf/stbl`, `mdhd`, `esds`, `stsc`, `stco/co64`, `stsz`
   read in 4 KiB pages from the card, never loaded whole; a sparse seek index
   every 256 frames) and the MP3 ID3/Xing skip. Decoders sit behind one
   `Decoder` trait returning PCM i16 frames. Recording the C dependency is
   part of this ADR.
2. **Hardware paths.** I2S1 master, Philips, 16-bit stereo slots, MCLK 256 ×
   Fs, sample rate switched per episode (48 kHz AAC, 44.1 kHz MP3) so nothing
   is resampled. The ES8311 does D/A and volume over the shared I2C bus
   (ADR-0005: init and volume changes only, never per frame). GPIO46 enables
   the amplifier only while playing (pop-free order: codec up, DAC muted, amp
   on, unmute). Output uses esp-hal 1.2's `I2sTx::write(DmaTxStreamBuf)`
   (`dma_tx_stream_buffer!`, chunks ≤ 4095 B, `push_with` decodes straight
   into the ring, `wait_for_available_async`). The DMA ring lives in internal
   RAM (8 × 4 KiB ≈ 170 ms at 48 kHz) to stay off the PSRAM bus the display
   DMA depends on (ADR-0004). An empty ring stops the DMA (not circular): the
   feeder restarts it and logs an underrun. No PIE until a measurement shows a
   decoder above budget; then the first candidate is a PIE s16 IMDCT/synthesis
   kernel in a fixed-point decoder.
3. **Placement: decode on core1, feed I2S at interrupt priority.**
   - The decoder runs as an ordinary task in core1's thread-mode executor,
     next to the SD owner. Tasks of one executor run one at a time, so the
     core1 stack needs max(owner, decoder), not their sum; the decoder state
     (20–50 KB) and its future live in PSRAM (`run_in`, t-121).
   - It decodes ahead into a PCM ring in PSRAM of ≥ 2 s (384 KB at 48 kHz
     stereo), larger than the longest owner request (1 s budget, ~230 ms
     commit), so a blocked core1 executor never empties the output.
   - A small feeder task in a core1 `InterruptExecutor` (software interrupt,
     above thread mode) moves PCM from the PSRAM ring to the internal DMA
     ring. It preempts the owner's blocking SPI: the S3 is SPI master and the
     card tolerates a paused SCK. Xtensa interrupts run on the interrupted
     stack, so the feeder frame adds to the owner's deepest chain: keep it
     under 1 KB and extend `just frame-check` to it.
   - Core0 is not used: it already has no headroom during downloads (t-163),
     and decoding there (7–19 % of a core) would come out of the UI and TLS.
     Core0 only sends play/pause/seek commands and reads progress.
4. **Play while downloading.**
   - *Readable bytes.* `UnitMedia::append` writes whole sectors to the card
     before `append_temp` returns (`write_sectors` blocks until CMD25 ends);
     only the sub-sector tail (< 512 B) stays in `temp.tail`. Within one boot
     the readable prefix of a downloading episode is therefore
     `floor(temp.len / 512) × 512`, a watermark held in the owner's RAM. None
     of it is committed: with a known length the catalogue commits only at
     reservation/growth and at publish (ADR-0020, one commit per request
     ADR-0022). After a reboot, recovery unlinks the temp record and the
     partial audio is lost; the playback position (catalogue) is not.
   - *Request.* A new podcast request `ReadAudio { id, offset, max }` goes
     through the ADR-0018 mailbox and fair `Scheduler` like any other: one
     request in flight, 2.5 s client budget. The owner answers the bytes up to
     the watermark (published: up to `len`) plus `{watermark, complete}`.
     Reads are 32–64 KiB (1.3–2.7 s of AAC at 24 KB/s) into a PSRAM
     compressed buffer kept ≥ 5 s ahead, so a request queued behind a commit
     or a download chunk is harmless. Multi-block reads (CMD18, t-122) cut its
     cost; until then a 64 KiB read is ~128 single-sector reads.
   - *Leases.* `MediaStore::active(id)` becomes true while a player holds the
     episode (ADR-0020's precondition for any `read_at`). Reclaim and unlink
     skip leased units.
   - *Catching up.* At start, playback needs the `moov` (AAC, ≤ 610 KB, about
     2 s at 270 KB/s) or the ID3 tag plus a few frames (MP3). If the read
     position reaches the watermark, the player pauses ("buffering") and
     resumes once the watermark is ≥ 5 s ahead or the download completes.
     Downloads run at ~17× real time, so this happens only at start or on a
     network stall.
   - *Failure and retry.* `abort` on a leased episode must not unlink: today
     `abort` returns `InvalidState` when `active`. Proposed: mark the download
     failed, keep the temp units leased, let playback continue to the
     watermark, and unlink when the lease ends. A retry must not
     `reset_temp` a leased temp (it restarts at offset 0 because Range resume
     does not exist); it waits for the lease to end or, once t-156 lands,
     resumes with Range/If-Range at the sector-aligned watermark.
   - *Load when both run.* Core0: unchanged (TLS, UI). Core1: owner ~30 %,
     decode 7–19 %, playback reads < 2 % (24 KB/s against ~270 KB/s of
     writes): ~40–50 %, with ≥ 2 s of PCM covering the owner's worst request.
     The download slows slightly (owner requests share the core with
     decoding); playback should not starve UI/input since none of it runs on
     core0.

## Board measurements (2026-11-15)

`just audio-bench` (feature `audio-bench`, `src/device/audio_bench.rs`,
`audio_tone.rs`): 30 s clips copied to PSRAM, decoded on core1 as a task in
the SD owner's executor (yielding after each frame), CCOUNT per frame with
interrupts included; one decode call in 64 runs with the free stack painted
instead of timed. Two runs, the second after reflashing; figures agree within
1 %.

| Decoder (build) | Clip | Decoder state | Mean cycles/frame | MHz for real time | Worst frame | Stack per call |
|---|---|---|---|---|---|---|
| Helix AAC (GCC -O2, no SBR) | AAC-LC 48 kHz stereo 192 kbps, 1,384 frames | 20,656 B internal | 474,619 | 22.2 (9.2 %) | 2.85 ms | ≤ 0.5 KB (measurement floor) |
| Helix AAC | same | PSRAM | 462,806 | 21.7 (9.0 %) | 3.06 ms | ≤ 0.5 KB |
| `nanomp3-core` 0.2 (opt-level 1) | MP3 128 kbps 44.1 kHz, 1,130 frames | 22,784 B internal | 2,637,134 | 101.0 (42 %) | 13.0 ms | 1.6 KB |
| `nanomp3-core` 0.2 (opt-level 1) | same | PSRAM | 2,631,850 | 100.7 (42 %) | 11.5 ms | 1.6 KB |

- No decode error; peak samples 25,807 (AAC) and 25,435 (MP3): real audio,
  not silence. MP3 frames before the first decodable one (bit reservoir) are
  excluded.
- Decoder state in PSRAM costs nothing measurable (cache), so the state
  lives in PSRAM and internal RAM stays free.
- Helix AAC's 22 MHz is below the 28–45 MHz estimate. Its worst frame (3 ms)
  is far below a 21.3 ms frame period.
- `nanomp3-core` fails to build at opt-level `s`, 2 and 3 ("LLVM ERROR:
  Cannot select: Constant<4608>" in `minimp3::l3_decode`); at opt-level 1
  it is ~4.5× Helix AAC per second of audio. Not usable as is.
- Test tone (I2S0 + GDMA, 1 kHz, 48 kHz stereo 16-bit, ES8311 init from
  Waveshare's `es8311.c`, amp on GPIO46): the ES8311 answers at 0x18 (chip
  ID register 0xFD = 0x83); the DMA ring drained 581,064 B in 3,013 ms =
  192,852 B/s for 192,000 expected (pushes are counted per refill), no DMA
  error. Whether the tone is audible needs someone listening at the board.
- Core1 stack: the bench task panicked on the stack guard on its first run
  because `Box::try_new_in(Decoder::new())` built the 18.6 KB decoder on the
  stack before boxing (the t-121 pattern); copying a const static into the
  allocation fixed it. Playback code must build decoder state in place.

## Consequences

- Expected gains (estimates until measured): I2S+GDMA and the ES8311 remove
  all per-sample CPU work besides decoding; placing the decoder on core1
  keeps core0's UI/TLS budget intact (t-163); per-episode sample rates avoid
  a resampler (~5–10 MHz); PIE would save at most part of a 16–45 MHz decoder
  and is deferred.
- A C dependency (Helix) needs a toolchain recipe (`cc` env), an ADR-level
  licence decision and host tests through its C build. `esp_audio_codec`
  is Espressif-only and closed.
- New risks to verify on the board: core1 stack headroom with the feeder
  interrupt (~2.4 KB today), owner request deadlines stretched by decode and
  feeder time, PIE state saving if PIE is ever used, PSRAM bandwidth with the
  display DMA (the PCM ring is in PSRAM; DMA reads stay in internal RAM).
- Measured load with AAC playback on core1: ~9 % of the core for decoding,
  so owner (~30 % during a download) plus decoding stays under half of
  core1.
- Next steps: measure Helix MP3 the same way; listen to the tone on the
  board; then the licence review that gates committing Helix sources.

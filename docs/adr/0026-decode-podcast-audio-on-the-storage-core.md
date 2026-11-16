# ADR-0026: Decode podcast audio on the storage core and output it through I2S DMA

- Status: Proposed (decoder benchmark and test tone measured on the board
  2026-11-15, see "Board measurements"; playback not implemented)
- Date: 2026-11-15
- Related: t-164 (parent t-153), t-165 (licensing), t-167 (licence-clean
  decoders), t-168 (decoder decision revised); ADR-0004 (PSRAM DMA), ADR-0005 (shared I2C),
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
| Helix MP3 | MP3 | C, RPSL/RCSL | 23.8 KB (measured) | 22–35 MHz [est]; measured 32.5 MHz |
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

1. **Decoders** (revised by t-168 after the t-165 licence review and the
   t-167 bench; Helix is no longer used, see Licensing).
   - *AAC-LC: opencore-aacdec* (PacketVideo, Apache-2.0; AOSP
     `android-4.0.4_r2.1`, `media/libstagefright/codecs/aacdec`), measured
     21.6 MHz, the same as Helix AAC. `build.rs` builds it with the ESP G++
     as the benchmark does, behind a C shim modelled on AOSP's `SoftAAC.cpp`
     (`tools/audio-bench/shim/theo_opencore.cpp`). Its sources are fetched
     into `target/opencore-aacdec` by `just audio-bench-inputs` (a future
     playback build fetches the same pinned tag) and are never committed.
     Its 108 KB state does not fit in internal RAM: it lives in PSRAM
     (no measurable cost) and must be built in place in its allocation, never
     on the core1 stack (t-121 pattern, see Board measurements).
     The code is unmaintained (dropped from AOSP in 2012) and had never
     been fuzzed, while its input is downloaded and untrusted: a host
     fuzzing harness of the raw/ADTS decode path (the shim's entry point,
     built with the host compiler and sanitizers) had to run before
     playback ships. ADR-0028 records it: four campaigns, 18 patches to
     the fetched sources, each with a regression input.
     `esp_audio_codec` stays the fallback.
   - *MP3: `nanomp3-core` 0.2* (MIT/Apache, pure Rust, `no_std`, no
     `unsafe`), measured 34.1 MHz with the t-157 release profile, within 5 %
     of Helix MP3 (32.5 MHz). It stays pinned at opt-level 1
     (`[profile.release.package.nanomp3-core]`): the Xtensa LLVM backend
     fails on its `l3_decode` at `s`, 2 and 3. The 34 MHz depends on the
     workspace release profile (LTO, one codegen unit); without it the same
     opt-level costs 101 MHz, so a profile change must re-run
     `just audio-bench`. Raise the opt-level once the backend bug is fixed.
   - The AAC patent licence (Via LA) is still needed to distribute the
     firmware, whichever AAC decoder is used; MP3 needs none.

   A host-tested `no_std` crate
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
     (opencore-aacdec 108 KB, `nanomp3-core` 23 KB) and its future live in
     PSRAM (`run_in`, t-121), built in place.
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
instead of timed. Three runs for Helix AAC and `nanomp3-core`, one for Helix
MP3 (added in the third); figures agree within 1 %.

| Decoder (build) | Clip | Decoder state | Mean cycles/frame | MHz for real time | Worst frame | Stack per call |
|---|---|---|---|---|---|---|
| Helix AAC (GCC -O2, no SBR) | AAC-LC 48 kHz stereo 192 kbps, 1,384 frames | 20,656 B internal | 474,619 | 22.2 (9.2 %) | 2.85 ms | ≤ 0.5 KB (measurement floor) |
| Helix AAC | same | PSRAM | 462,806 | 21.7 (9.0 %) | 3.06 ms | ≤ 0.5 KB |
| Helix MP3 (GCC -O2) | MP3 128 kbps 44.1 kHz, 1,130 frames | 23,824 B internal | 848,117 | 32.5 (13.5 %) | 3.85 ms | ≤ 0.5 KB |
| Helix MP3 | same | PSRAM | 847,759 | 32.5 (13.5 %) | 4.01 ms | ≤ 0.5 KB |
| `nanomp3-core` 0.2 (opt-level 1) | same | 22,784 B internal | 2,637,134 | 101.0 (42 %) | 13.0 ms | 1.6 KB |
| `nanomp3-core` 0.2 (opt-level 1) | same | PSRAM | 2,631,850 | 100.7 (42 %) | 11.5 ms | 1.6 KB |

- No decode error; peak samples 25,807 (AAC) and 25,435 (MP3, identical for
  Helix MP3 and `nanomp3-core`, a cross-check of both): real audio, not
  silence. MP3 frames before the first decodable one (bit reservoir) are
  excluded.
- Decoder state in PSRAM costs nothing measurable (cache), so the state
  lives in PSRAM and internal RAM stays free.
- Helix AAC's 22 MHz is below the 28–45 MHz estimate. Its worst frame (3 ms)
  is far below a 21.3 ms frame period. Helix MP3's 32.5 MHz is within its
  22–35 MHz estimate; worst frame 4 ms against a 26.1 ms frame period. MP3
  costs more per second than AAC despite the lower bitrate: Helix AAC's
  fixed-point IMDCT is cheaper than MP3's hybrid filterbank.
- `nanomp3-core` fails to build at opt-level `s`, 2 and 3 ("LLVM ERROR:
  Cannot select: Constant<4608>" in `minimp3::l3_decode`); at opt-level 1
  it is ~4.5× Helix AAC per second of audio. Not usable as is.
- Test tone (I2S0 + GDMA, 1 kHz, 48 kHz stereo 16-bit, ES8311 init from
  Waveshare's `es8311.c`, amp on GPIO46): the ES8311 answers at 0x18 (chip
  ID register 0xFD = 0x83); the DMA ring drained 581,064 B in 3,013 ms =
  192,852 B/s for 192,000 expected (pushes are counted per refill), no DMA
  error. The user heard the 3 s 1 kHz tone from the speaker (2026-11-15):
  codec init, I2S wiring and the GPIO46 amplifier enable are confirmed.
- Core1 stack: the bench task panicked on the stack guard on its first run
  because `Box::try_new_in(Decoder::new())` built the 18.6 KB decoder on the
  stack before boxing (the t-121 pattern); copying a const static into the
  allocation fixed it. Playback code must build decoder state in place.

### Licence-clean alternatives (t-167, 2026-11-15)

One run with the t-157 release profile (opt-level 3, thin LTO, one codegen
unit; `nanomp3-core` still pinned at opt-level 1), same harness and clips:

| Decoder (build) | Decoder state | Mean cycles/frame | MHz for real time | Worst frame | Stack per call |
|---|---|---|---|---|---|
| opencore-aacdec (PacketVideo, Apache-2.0, G++ -O2, AAC+ and PS built in) | 108,344 B PSRAM | 459,949 | 21.6 (8.9 %) | 3.13 ms | 0.6 KB |
| Helix AAC, same run | 20,656 B internal / PSRAM | 590,475 / 461,586 | 27.7 / 21.6 | 4.21 / 5.01 ms | ≤ 0.5 KB |
| Helix MP3, same run | internal / PSRAM | 848,874 / 851,823 | 32.5 / 32.6 | 4.59 / 4.85 ms | ≤ 0.5 KB |
| `nanomp3-core` 0.2 (opt-level 1, LTO) | 22,784 B internal | 890,160 | 34.1 (14.1 %) | 5.20 ms | 1.7 KB |
| `nanomp3-core` 0.2 (opt-level 1, LTO) | PSRAM | 890,788 | 34.1 (14.2 %) | 5.46 ms | 1.7 KB |

- opencore-aacdec (AOSP `android-4.0.4_r2.1`,
  `media/libstagefright/codecs/aacdec`, unchanged sources, C shim
  `tools/audio-bench/shim/theo_opencore.cpp` driven like AOSP's
  `SoftAAC.cpp`) decodes the clip with no error and the same peak (25,806)
  as Helix: it matches Helix AAC's cost, with a lower worst frame. Its state
  is 108 KB on Xtensa (80 KB on a 64-bit host) and does not fit in internal
  RAM next to the firmware (the internal-state run gets no memory): it lives
  in PSRAM, which costs nothing measurable here either.
- `nanomp3-core` drops from 101 to 34 MHz with LTO and one codegen unit,
  within 5 % of Helix MP3, at the same opt-level 1 (cross-crate inlining of
  `minimp3` helpers is the likely cause; not profiled).
- Helix AAC with internal state measured 27.7 MHz in this run against 22.2
  before; the PSRAM run agrees with before (21.6 vs 21.7). One run only:
  treat ±25 % between runs of the internal-state AAC case as noise until
  repeated.
- Both RPSL/RCSL decoders now have a licence-clean equivalent at the same
  CPU cost: opencore-aacdec (Apache-2.0; unmaintained, dropped from AOSP in 2012, fuzzed
  and patched since, ADR-0028; inputs to treat as untrusted) and `nanomp3-core` (MIT/Apache, pure
  Rust, no `unsafe`). The decision above now uses them (t-168); the AAC
  patent licence is needed whichever decoder is used.

## Licensing (t-165, 2026-11-15; not legal advice)

Sources read: `RPSL.txt` (RPSL 1.0, rev. 28 Oct 2002), `RCSL.txt` and
`LICENSE.txt` shipped in `src/libhelix-mp3` at `f6e55a3`. The license header
in every AAC `.c` file points to the same RPSL/RCSL. Section numbers below
are RPSL sections unless marked otherwise.

Since t-168 the decision uses opencore-aacdec and `nanomp3-core`; Helix
stays only in the `audio-bench` comparison build. The review below still
applies to that build.

**Verdict.** Helix may be used under the RPSL for personal use. It must not
go into a distributed firmware until a lawyer has reviewed the plan, or until
it is replaced. RCSL is not a usable route.

- **Personal use is outside the RPSL's duties.** The RPSL defines "Deploy"
  as use or distribution other than internal R&D or Personal Use (§1.5).
  Personal Use is an individual's "personal, private and non-commercial"
  use, and use on behalf of any organisation does not count (§1.11). The
  §2.1(d)/(e) source-publication duties and §4.1 attach to "Deploy" and
  "Externally Deploy". A firmware the author builds and runs on his own
  board deploys nothing.
- **Distribution triggers §4.1.** Handing the firmware to anyone else, sold
  or free, Externally Deploys it (§1.7). Any distributed Derivative Work
  must then be licensed "as a whole at no charge" under the RPSL, with its
  source available (§4.1). Our own MIT OR Apache-2.0 code is acceptable
  under §4.2, since both licences are on Exhibit B. However, §4.2 also
  requires everything linked into the image to be "wholly subject to" an
  Exhibit B licence, and two dependencies are not:
  - **Slint.** It is `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR
    LicenseRef-Slint-Software-3.0`. Neither Slint licence is on Exhibit B,
    and Exhibit B forbids applying the GPL to Covered Code.
  - **The esp-radio Wi-Fi stack.** It links Espressif's binary-only blobs
    (`esp-wifi-sys`: `libnet80211.a`, `libpp.a`, `libphy.a`, …), which have
    no source. That conflicts with §1.2 and §4.2.

  **Lawyer needed.** Whether a statically linked image is one "Derivative
  Work" or a "mere aggregation" (§4.3) is a legal question, and so is
  whether these conflicts can be cured. Get advice before shipping.
- **RCSL is not a route.** It grants Research Use only. "Commercial Use"
  needs a separately signed Attachment D with RealNetworks (RCSL §3.2).
  `helixcommunity.org` now redirects to an unrelated `.com.cn` host, and
  realnetworks.com lists no Helix licensing (checked 2026-11-15). Whether
  RealNetworks still offers Attachment D is a question for a lawyer.

**Obligations while Helix is used (personal use):**

1. Do not commit Helix sources to this repository (it is public:
   committing redistributes them). The `audio-bench-inputs` recipe fetches
   the pinned commit into `target/helix`, and a future `build.rs` must do the
   same.
2. Keep every RealNetworks notice intact (§2.1(a)).
3. If the decoder is ever redistributed in source form:
   - ship `RPSL.txt` with both directories; `libhelix-aac` lacks it upstream
     (§2.1(b));
   - mark each change with a dated notice beside the Exhibit A header
     (§2.1(c)).

   The `codec-helix` fork already changed RealNetworks files without such
   notices: the `ConfigHelix.h` and `utils/helix_memory.h` includes, and the
   ARDUINO/ESP32/ESP_PLATFORM/Zephyr/Apple `#if`s in `assembly.h` and
   `aacdec.h`. Our shim (`tools/audio-bench/shim`, MIT OR Apache-2.0) is a
   separate module and modifies no Helix file.
4. If the firmware is ever deployed in object form, after a lawyer has
   cleared it:
   - include the Object Code Notice in an about screen and in the
     documentation (§2.1(e), Exhibit A): "Helix DNA Client technology
     included. Copyright (c) RealNetworks, Inc., 1995-2002. All rights
     reserved.";
   - publish the source of our modifications for at least 12 months
     (§2.1(d)).
5. Do not use the "Helix" or "RealNetworks" names to brand the product (§5.1,
   Exhibit C).
6. Patents are not covered beyond RealNetworks' own. The licensee must
   obtain any third-party patent licences itself (§2.2, §2.3).

**Patents.**

- **AAC.** Via LA's AAC programme
  (<https://www.via-la.com/licensing-programs/aac/>) licenses "manufacturers
  or developers of end-user encoder and/or decoder products". Components and
  implementations are not covered. There is no exemption for free or
  non-commercial products, and the programme has no licence for individuals.
  Terms:
  - $0.98 per unit for units 1–500,000, with tiers below that;
  - a $15,000 one-time fee ($1,000 for small entities);
  - a 5-year term and quarterly reports.

  Any distributed AAC product needs this licence. A single board kept for
  personal use has no licensing path, and the practical risk is low, but it
  is not legally exempt.
- **MP3.** Technicolor's MP3 licensing programme "has been terminated" as of
  23 April 2017 (Fraunhofer IIS,
  <https://www.iis.fraunhofer.de/en/ff/amm/consumer-electronics/mp3.html>).
  MP3 decoding needs no patent licence.

**Alternatives if Helix cannot ship:**

| Decoder | Licence | Fit |
|---|---|---|
| `esp_audio_codec` | Espressif Modified MIT, "EXCLUSIVELY with Espressif Systems products"; `lib/` prebuilt (not checked) | Allowed on this chip; adds IDF shims and closed code; AAC patents still apply |
| FAAD2 | GPL-2.0-or-later; COPYING: "Any non-GPL usage … is strictly forbidden", commercial licence via Ahead/Nero | Makes the firmware GPL, which Slint's GPL-3.0-only option accepts; Wi-Fi blobs remain a GPL question |
| opencore-aacdec | Apache-2.0 | Licence-compatible; 21.6 MHz, state 108 KB in PSRAM (t-167, above) |
| fdk-aac | FDK licence, no patent grant | C++, heavy; excluded above |
| `nanomp3-core` | MIT/Apache | MP3 only; 34 MHz with the t-157 release profile (t-167, above) |

Distribution route with the fewest unknowns: Helix MP3 is royalty-free, but
its RPSL conflicts remain. The cleanest path is opencore-aacdec plus an
MIT/Apache MP3 decoder, together with an AAC patent licence and a Slint
licence choice; the decision takes that path (t-168).

## Consequences

- Expected gains (estimates until measured): I2S+GDMA and the ES8311 remove
  all per-sample CPU work besides decoding; placing the decoder on core1
  keeps core0's UI/TLS budget intact (t-163); per-episode sample rates avoid
  a resampler (~5–10 MHz); PIE would save at most part of a 16–45 MHz decoder
  and is deferred.
- A C++ dependency (opencore-aacdec) needs a toolchain recipe (`cc` env,
  ESP G++), a pinned fetch into `target/` (never committed) and host tests
  through its C build, including a fuzzing harness of the raw/ADTS path: the
  code is unmaintained and had never been fuzzed, and podcast files are
  untrusted input. Bugs found there are ours to patch (vendored patches on
  top of the fetched tag, Apache-2.0 notices kept; ADR-0028). `esp_audio_codec` is
  Espressif-only and closed.
- The AAC decoder state (108 KB) takes PSRAM, not internal RAM, and must be
  constructed in place.
- `nanomp3-core`'s 34 MHz rests on the release profile (LTO, one codegen
  unit) and its opt-level 1 pin; any profile change re-runs
  `just audio-bench`.
- New risks to verify on the board: core1 stack headroom with the feeder
  interrupt (~2.4 KB today), owner request deadlines stretched by decode and
  feeder time, PIE state saving if PIE is ever used, PSRAM bandwidth with the
  display DMA (the PCM ring is in PSRAM; DMA reads stay in internal RAM).
- Measured load on core1: ~9 % (AAC) or ~14 % (MP3) for decoding, so the
  owner (~30 % during a download) plus decoding stays under half of core1.
- Licensing (t-165, t-168): both chosen decoders are Apache-2.0 or
  MIT/Apache, so the RPSL §4.1/§4.2 conflicts (Slint, Wi-Fi blobs) no longer
  apply to the firmware; Helix remains only in the personal-use bench build.
  Distributing the firmware still needs a Via LA AAC patent licence and a
  Slint licence choice.

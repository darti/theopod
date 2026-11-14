# ADR-0024: Run TLS AES-GCM on the AES peripheral

- Status: Accepted
- Date: 2026-11-14
- Related: t-154 (parent t-153); ADR-0016 (TLS stack)

## Context

Podcast audio downloads ran at about 84 KB/s, with about 20 ms of each
4 KB chunk spent in `reader.read` while a 32 KB TCP window kept data
buffered. On the board (80 MHz, esp-hal 1.2's default CPU clock), software
AES-128-GCM (`embassy-crypto-rustcrypto`) took 78 ms per 16 KiB record:
44 ms of AES-CTR and 34 ms of GHASH. In a feed transfer, 1.5 s of 1.9 s
of read time per 256 KiB was decryption; the socket wait was 0.4 s.

The ESP32-S3 AES peripheral runs ECB and CTR (block by block through
registers, or over a buffer by DMA) but has no GCM mode. embassy-crypto
selects one global driver per algorithm at link time
(`aes128_gcm_impl!`), so embedded-tls needs no change.

## Decision

- `src/device/tls_aes.rs` registers the global `Aes128Gcm` driver in
  place of RustCrypto's (`embassy-crypto-aes128-gcm` feature dropped).
  It owns the AES peripheral and `DMA_CH1` through esp-hal's
  `AesDmaBackend` work queue, installed at board start.
- `theo_drivers::gcm` composes GCM from any `GcmCipher`: two ECB blocks
  for the hash key and tag mask, the CTR keystream from the engine, GHASH
  in software (RustCrypto `ghash`, constant time), tag compared in
  constant time before decryption. It is HAL-free and host-tested against
  the NIST test vectors and the `aes-gcm` crate.
- CTR runs by DMA in pieces of at most 4032 bytes, each from a fresh
  context with an explicitly computed counter; a partial last block runs
  as one zero-padded block (`gcm::ctr_in_pieces`). esp-hal 1.2.2's AES DMA
  links an operation to a single descriptor (it panicked on a 16 KiB
  record) and does not carry the CTR counter from one operation to the
  next, nor into the CPU fallback that finishes a partial block (wrong
  keystream, `TLS InvalidRecord`, on the board).
- The register-driven `Aes::encrypt` was rejected: 20 µs per block at
  80 MHz (key and text register writes per block), 24 ms per record.

## Consequences

- Per 16 KiB record at 80 MHz: 36 ms instead of 78 ms; DMA CTR is 2 ms,
  GHASH is the remaining 33 ms. An audio download measured 104 KB/s with
  about 10 ms read per 4 KB chunk (was 84 KB/s, 20 ms); storing the chunk
  (about 28 ms) is now the larger cost.
- `opt-level = 3` for `polyval`/`ghash` gained 4 % and was not kept. A
  faster GHASH would need table lookups indexed by secret data (not
  constant time) or a higher CPU clock (t-157).
- AES DMA reads and writes the TLS buffers in PSRAM, beside the display
  DMA (ADR-0004); the traffic is the record data rate, about 200 KB/s
  during a download.
- An esp-hal upgrade that fixes the descriptor limit or the CTR state may
  allow one operation per record; `MAX_DMA_BYTES` and the regression test
  in `gcm_tests.rs` mark the workaround.

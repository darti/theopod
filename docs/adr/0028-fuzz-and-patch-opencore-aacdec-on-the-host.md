# ADR-0028: Fuzz opencore-aacdec on the host and patch what it finds

- Status: Accepted
- Date: 2026-11-15
- Related: t-169 (sub-tasks t-174 to t-177), ADR-0026 (decoder choice,
  "never fuzzed"), ADR-0007 (regression tests inline)

## Context

ADR-0026 picked opencore-aacdec (Apache-2.0, AOSP `android-4.0.4_r2.1`,
dropped from AOSP in 2012) for AAC and required a host fuzzing harness of
the raw/ADTS decode path before playback ships: the code is unmaintained,
was never fuzzed, and podcast files are downloaded, untrusted input. The
sources are fetched into `target/opencore-aacdec` and never committed.

## Decision

- **Harness.** `tools/aacdec-fuzz` builds the fetched decoder, the
  firmware's bench shim (`tools/audio-bench/shim/theo_opencore.cpp`) and a
  driver (`harness/driver.cpp`) with ASan and UBSan, and links them with
  libFuzzer (`just aacdec-fuzz [seconds] [jobs]`, Homebrew LLVM: Apple's
  clang has no libFuzzer runtime). The driver goes through the shim's entry
  points only, as the firmware does: zeroed state of `theo_opencore_size()`
  bytes, `theo_opencore_init`, then `theo_opencore_decode` per access unit,
  going on after errors. Byte 0 of an input selects raw access units after
  an AudioSpecificConfig, or an ADTS stream. Every buffer is an exact-size
  heap copy, so a byte past the input, the state or the PCM output is
  reported. Seeds come from the bench AAC clip (`seeds.py`: LC, explicit
  SBR and PS, backward-compatible SBR, ADTS with and without CRC) and stay
  under `target/` (copyrighted audio). `just aacdec-fuzz-triage` replays
  the artifacts and groups them by sanitizer report.
- **Regression tests.** The `aacdec-fuzz` crate builds the same objects
  with the host C++ compiler and sanitizers (`build.rs`) and replays each
  crash input in `tools/aacdec-fuzz/regressions/` from
  `src/decode_tests.rs`, tagged `// Regression (t-169)` (ADR-0007); `just
  test` and `just lint` include it. A sanitizer report aborts the test
  binary. Calls are serialised: the shim keeps the decoder's external
  structure in a static.
- **Patches.** Each fix is a patch in `tools/opencore-aacdec/patches/`,
  applied in order by `tools/opencore-aacdec/fetch.sh` to the fetched tag
  (`just opencore-aacdec`; the SHA-256 of the patch set is stamped, so a
  new patch refetches). Every patch states the Apache-2.0 licence, and
  every modified file keeps its notice and gains a "Modified by theo-pod
  (t-169)" line (Apache-2.0 §4(b)).
- **Sanitizer scope.** Negative left shifts (`shift-base`) are left out:
  they are this fixed-point code's idiom, defined since C++20 and two's
  complement in GCC. Signed overflow is made defined instead of fixed: the
  code relies on wrapping (e.g. `tns_ar_filter` adds 1 to
  `2147483647`), so the firmware build (`build.rs`), the fuzzer and the
  tests compile with `-fwrapv`, and UBSan's `signed-integer-overflow`
  check is off. Out-of-range shift counts (negative, or 32 and more) are
  not covered by `-fwrapv` and are patched.

## Findings

One patch per root cause, each with a regression input in
`tools/aacdec-fuzz/regressions/` whose test fails without it:

1. `0001` getbits: the bytes-left count wrapped once past the input, and
   the bit readers loaded past the buffer.
2. `0002` getics: pulse data past the window wrote past the spectrum.
3. `0003` unpack_idx/pulse: unbounded escape prefixes and pulse
   amplitudes indexed past `inverseQuantTable`.
4. `0004` huffspec: bands no section covers kept stale values, then
   indexed past the same table.
5. `0005` reserved sampling-frequency indices (ADTS, PCE, SBR extension)
   indexed past `samp_rate_info`.
6. `0006` get_tns: a negative filter bottom walked `apply_tns` off its
   tables.
7. `0007` sbr_reset_dec: a crossover band past the master table.
8. `0008` switching between AAC and SBR frames left `ltp_buffer_state`
   in the other layout, and SBR wrote before `ltp_buffer` inside the
   decoder state.
9. `0009` out-of-range shifts in the IMDCT, M/S and bit-rate code.
10. `0010` the SBR bit reader, CRC check and PS extension read past the
    SBR payload.
11. `0011` SBR frame borders: too many envelopes or decreasing borders
    addressed QMF slots past their buffers.
12. `0012` SBR frames decoded with stale or half-reset frequency tables.
13. `0013` the high-band patch construction overflowed `MAX_NUM_PATCHES`
    (stack overflow) or looped forever.
14. `0014` out-of-range exponent shifts in the SBR envelope and PS.
15. `0015` a program config element without a front element gave 12
    channels and indexed past the channel state.
16. `0016` an inconsistent window group table walked PNS/intensity past
    `mask[]`.
17. `0017` the short-window overlap read past the window tables, and an
    LTP Q format of 0 shifted by -1.
18. `0018` explicitly signalled PS ran without its buffers (NULL
    dereference). The same patch fixes PS pointer arrays carved out of
    `Int32` buffers, which overlap on a 64-bit host only: a host
    artefact, the 32-bit firmware layout is unchanged.

Every finding is reachable from a downloaded file: 1–7 and 9–17 are
memory-safety bugs (reads or writes past a buffer, a hang) or undefined
shifts, 8 corrupted the decoder state, 18 crashed.

## Campaigns

`just aacdec-fuzz` on an arm64 MacBook (8 jobs), each round after fixing
everything the previous one found (all earlier artifacts replay clean):

| Round | Duration | Executions | Coverage (cov / ft) | Corpus | Artifacts |
| ----- | -------- | ---------- | ------------------- | ------ | --------- |
| 1     | 126 s    | 195 774    | 1 149 / 5 335       | –      | 700       |
| 2     | 600 s    | 4 740 177  | 2 167 / 11 504      | 2 373  | 2 439     |
| 3     | 600 s    | 2 683 283  | 2 789 / 15 068      | 2 514  | 23        |
| 4     | 610 s    | 2 226 951  | 2 823 / 15 662      | 2 341  | 1         |

Artifacts are mostly duplicates of a few sites; round 4's one artifact
was a last exponent shift in `calc_sbr_envelope.cpp` (patch 0014).
<!-- ROUND5 -->
The deeper SBR and PS paths only opened up once the earlier crashes were
fixed, so coverage kept growing between rounds; later campaigns should
start from the saved corpus (`target/aacdec-fuzz/corpus`).

## Consequences

- The decoder's state can no longer be corrupted, read past or indexed past
  its tables by the inputs found so far; a malformed frame is rejected or
  decoded into bounded garbage. Clamped shifts and the restarted LTP/SBR
  history change only malformed streams; well-formed AAC-LC/HE-AAC decode
  as before.
- `-fwrapv` may change code generation in the firmware's decoder: the
  ADR-0026 benchmark (21.6 MHz) must be re-run on the board (t-171) before
  its figure is relied on again.
- The fuzzing runs on a 64-bit host (arm64 macOS) with Clang; the firmware
  is 32-bit Xtensa built by GCC. Pointer-size and compiler differences are
  not covered: `Int`/`Int32` are 32-bit on both, but layout-dependent bugs
  (struct padding, pointer arithmetic on 64-bit) may behave differently.
- Overruns inside the 110 KB decoder state are invisible to ASan (one
  allocation); they show up only through their effects, as the
  `ltp_buffer_state` one did (an invalid `bool` read by UBSan). The fuzzer
  under-reports that class.
- Any new patch must come with its regression input and test, and pass
  `just aacdec-fuzz` without new reports; patches keep applying to the
  pinned tag only.

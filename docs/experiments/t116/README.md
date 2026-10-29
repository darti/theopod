# t-116 EKV metadata recovery experiment

This retains the historical isolated correction to **EKV 1.0.0** and its
red/green evidence. The correction has now been adopted as a local override
at `vendor/ekv` after broader testing; see ADR-0015 and the project regressions.
The remaining t-106 runtime/resource gate is incomplete. Database initialization,
card provisioning and networking remain disabled; deployed logging is unchanged.

## Findings

The original t-106 experiment was rerun: **11 pass, 1 fails**. During version
62 → 63, interruption after the new metadata page header or before its chunk
header makes mount succeed but committed keys disappear (`KeyNotFound`).

`FileTransaction::commit` publishes the replacement page's higher sequence
through `write_header` before `PageWriter::commit` writes the chunk header that
makes its records visible. Mount selects the highest sequence from page headers
alone and treats an uncommitted chunk as empty. Therefore individually atomic
PageStore writes cannot correct the engine's publication order.

The candidate commits the chunk first and writes the replacement page header
last. Until that final publication, recovery selects the old metadata page;
after it, the new page already contains a committed chunk. No format or API
change is proposed. The old page is freed only after publication as before.

The expanded test then exposed another failure: `write_file_meta(...).await
.unwrap()` in that same rollover path panics on a storage error. The candidate
propagates the flash error and reports impossible full/corrupt metadata as
`Error::Corrupted`; it does not continue using the failed PageStore instance.
This case was observed failing before the error-propagation correction.

[Upstream issue #26](https://github.com/embassy-rs/ekv/issues/26) and
[reproducer PR #25](https://github.com/embassy-rs/ekv/pull/25) were checked on
2026-10-29: both remain open; the PR contains a reproducer, not a correction.
The local patch has not been submitted upstream.

## Reproduction

Use an unmodified registry source directory for EKV **1.0.0**. The preparation
script checks the SHA-256 of `src/file.rs`, refuses an existing destination,
copies the source and applies the retained patch only to that disposable copy.
It reuses t-106's locked dependency graph and actual project PageStore.

```sh
python3 docs/experiments/t116/prepare.py \
  /path/to/cargo/registry/src/index.crates.io-HASH/ekv-1.0.0 \
  /tmp/theo-t116-repro
cd /tmp/theo-t116-repro/db
export THEO_T106_DIR="$PWD"
RUSTUP_TOOLCHAIN=stable RUSTC_WRAPPER= cargo test --offline --release \
  --target aarch64-apple-darwin -- --nocapture
```

Use the host target for your computer. The first run updates Cargo.lock only
to use the local patched EKV source; later runs can use `--locked`. Append
`--unpatched` to the preparation command with a different empty destination to
observe the original missing-key failure and storage-error panic.

`rollover_tests.rs` extends t-106's reproducer (same MIT/Apache-2.0 provenance):

- Every logical write **and erase**, before and after completion, during normal
  commit and the first metadata rollover.
- Every underlying sector-write and sync boundary during that rollover, with
  zero, 217, or all 512 bytes of the affected sector persisted.
- Both returned I/O errors and futures left pending at the same boundaries,
  then dropped before reopening the physical image and mounting a fresh DB.
- Recovery must return a complete old or new pair of previously committed keys.
- The retained probe also runs the actual-file reopen/schema/scan/delete test
  and the existing PageStore sector/cancellation/barrier regressions.

Sync failures model ordered storage where preceding completed writes may
already be durable. This does not simulate arbitrary controller write
reordering, collateral sector damage, delayed writes across a failed barrier,
or physical SD power loss. Logical erases are PageStore replacements, not
assumptions about SD erase atomicity.

## Target compilation

Prepare the t-106 `xtensa` probe, add `[patch.crates-io]` pointing at the same
patched source, and use a disposable copy of the project's Justfile with its
ESP environment (as documented in t-106). The checked command is:

```text
cargo check --offline --manifest-path /tmp/theo-t116-xtensa/Cargo.toml \
  --lib --target xtensa-esp32s3-none-elf -Z build-std=core,alloc
```

This succeeded with the patched engine and t-106's pinned network/TLS candidate.
It is generic/API compilation, not linked firmware, RAM headroom, runtime
latency, Wi-Fi initialization, or physical storage validation.

## Verification results

The original probe failed as expected (11 passing, missing-key rollover test
failing). Publishing the header last made that original 12-test probe pass.
The expanded returned-error test then failed with the original rollover
`unwrap()` panic; propagating that error made the **13-test suite pass**.

`recovery-result.txt` records **1352 sector/sync boundaries × 3 persisted
prefixes × returned error/cancellation = 8112 fault cases**, plus logical
write/erase boundaries and the real-file/PageStore tests. The release probe
completed in 260.93 seconds. A fresh preparation from the retained script and
tests independently passed the same 13 tests in 253.32 seconds; see
`retained-reproduction-result.txt`. Each fault case mounts a fresh database and reads
both committed keys. This is targeted model coverage, not universal crash safety.
`publication-order-result.txt` records the intermediate original-suite success;
`xtensa-result.txt` records target compilation of the final two-fix candidate.

The unchanged project passed `just fmt`, `just check`, `just test` (366 host
tests), `just lint`, and `just build`. No dependency change entered the firmware;
Miri/audit were not required. An accidental direct host-target build of the root
firmware failed in ESP-only dependencies; the supported `just test` host recipe
and the separate Xtensa checks passed.

## Remaining gate

The broader history/compaction/delete interruption sweep passed and the patch
was reviewed before local adoption. Project source now holds the current
regressions; these disposable files preserve the preceding experiments.
The descriptor geometry and allocation API are implemented, but runtime region
I/O/provisioning and the combined memory/latency budget remain unresolved.
Do not enable podcast storage based on the recovery correction alone.

A separate read-only code review found no actionable issue in the patch,
fault assertions, preparation script or stated limitations. It confirmed that
`PageWriter::commit` finishes buffered records and their chunk header, and
subsequent `write_header` does not re-erase the page. This review covers the
isolated candidate; it does not approve enabling firmware podcast storage.

Retained evidence hashes are in `SHA256SUMS`; verify with `shasum -a 256 -c SHA256SUMS` from this directory.

## Broader history and project adoption

`history-red-result.txt` records the original engine failing full-model recovery
at generation 61, logical cut 539 after its 20-byte metadata page header.
`history-result.txt` records the patched engine passing 3238 logical cuts at six
checkpoints and 2964 sector/sync cases at generation 96. Both tests build a
192-commit/reopen mixed history with three metadata rollovers, 787 erases and
page reuse; the model includes 24 episode keys and a persistent read tombstone.
`post-recovery-result.txt` records 12 selected recovery endpoints followed by
12 commits of 4096-byte values each and another reopen. Its unpatched red result
is also retained. Torn-sector testing of every history is not claimed.

The current source and long-test recipe are now in the actual project:
`page_store_rollover_tests.rs`, `page_store_history_tests.rs`, and
`just test-storage-recovery`. A short metadata-header publication guard runs in
normal `just test`. Descriptor and allocation tests are also in theo-storage.
The disposable db helper now uses the current PageStore, including its explicit
allocator API; exact total probe test counts therefore grow with that source.
The earlier 12/13-test counts above describe their retained historical runs.

`psram-xtensa-result.txt` records successful explicit-PSRAM constructor type checks
with the adopted allocator API (`&esp_alloc::ExternalMemory`). The initial
constructor probe found ExternalMemory itself is not Clone; the reference form
meets the allocator contract without a custom allocator or unsafe wrapper.
The local correction, layout and allocator changes passed a separate final
read-only review with no actionable findings. Full project verification is
recorded in ADR-0015 after the final recovery run.

## Final adopted-source verification

The full project recovery recipe passed **7 tests**, including every ignored
sweep, in 361.79 seconds (`project-recovery-result.txt`). The standard host
suite passed **374 tests**, with five long sweeps explicitly delegated to that
recipe. Formatting, firmware check, lint across all variants, release build,
and the dependency advisory/license/source audit passed. No driver unsafe code
changed; Miri was not required. No hardware operation was performed.

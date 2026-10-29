# t-106 disposable evidence

These are **documentation reproductions**, excluded from the Cargo workspace.
Run them in a fresh external directory. Firmware/storage implementation is
unchanged. Base storage source: `91fb77a`; EKV `=1.0.0`. The metadata-rollover
probe is adapted from the public [EKV reproducer PR #25](https://github.com/embassy-rs/ekv/pull/25),
using the actual project PageStore instead of a direct NOR image. It checks
complete logical writes, so the failure does not require a torn logical header.
EKV and that reproducer are MIT/Apache-2.0 licensed.

## Host database

From the repository root (substitute your host target if necessary):

```sh
python3 docs/experiments/t106/prepare.py db /tmp/theo-t106-repro-db
cd /tmp/theo-t106-repro-db
export THEO_T106_DIR="$PWD"
RUSTUP_TOOLCHAIN=stable RUSTC_WRAPPER= cargo test --locked --target aarch64-apple-darwin -- --nocapture
```

**Expected result with unpatched EKV: exit 101, missing-key rollover failure.**
The retained original run has 11 passing and 1 failing test. The preparation
uses current PageStore source, so later allocator regressions add passing tests;
the historical count is not a fixed count for later source revisions. This is
an intentionally preserved negative feasibility result, not a passing project
regression suite. The failing rollover test mounts successfully then gets
`KeyNotFound` for committed data after the metadata header write and before its
following write. `db-result.txt` retains that result. `db-file-result.txt`
records the separate real-host-file test: insert/reopen, update/reopen,
delete/reopen, ordered scan, schema rejection without writes, and 90 torn-sector
positions plus the successful endpoint, all passing. Host `sync_all` and a
simulated failed sector do not model a physical card's controller.

The rollover test samples cancellation before/after each non-erase logical
write, not every erase failure or every possible history. A clean result here
would not prove general engine correctness. Here two adjacent boundaries
already disprove sufficient recovery for the selected path.

## Host verified HTTPS

```sh
python3 docs/experiments/t106/prepare.py https /tmp/theo-t106-repro-https
cd /tmp/theo-t106-repro-https
export THEO_T106_DIR="$PWD"
curl -fsSL https://pki.goog/repo/certs/gtsr4.der -o gtsr4.der
RUSTUP_TOOLCHAIN=stable RUSTC_WRAPPER= cargo run --locked --target aarch64-apple-darwin
```

The same generic TLS + low-level HTTP adapter used in the Xtensa probe requests
the public Aerion search endpoint. `tls-result.txt`: 1680 response bytes;
wrong hostname → `InvalidCertificate`, empty root → `DecodeError`, unavailable
time → `Clock`. The first case also accepts a non-200 HTTP status as evidence
that TLS succeeded, **not** as evidence of a useful feed. The RSS endpoint later
returned 530 with both curl and this Rust client. No HTTP downgrade or bypass.

The clock in this host probe uses SystemTime. The wrapper checks the clock
again at certificate verification: embedded-tls upstream accepts undated
certificates for a clock returning None, which this adapter explicitly rejects.
The 43 selected upstream verifier tests also passed (`tls-verifier-result.txt`),
including wrong name, invalid signature, expired certificates, intermediates,
and large leaf rejection. Their insecure-clock cases are upstream behaviour,
not application policy. Command from the pinned embedded-tls checkout:

```sh
RUSTUP_TOOLCHAIN=stable RUSTC_WRAPPER= cargo test --lib --target aarch64-apple-darwin --no-default-features --features std,webpki,p256,p384,ed25519,rsa webpki
```

## Xtensa dependency/API and size proof

Prepare `xtensa` the same way. In a copy of the project's Justfile **outside the
repository**, add this recipe (the existing variables load the ESP environment):

```just
t106-probe:
    LIBCLANG_PATH="{{libclang_path}}" PATH="{{gcc_path}}:${PATH}" RUSTC_WRAPPER= RUSTUP_TOOLCHAIN=esp cargo rustc --manifest-path /tmp/theo-t106-repro-xtensa/Cargo.toml --lib --target xtensa-esp32s3-none-elf -Z build-std=core,alloc --locked -- --emit=obj
```

Run it through `just --justfile /tmp/your-copy.just t106-probe`. The probe checks
actual DNS/TCP/TLS/request/body APIs and the candidate DB adapter. It does not
link a full firmware or initialize Wi-Fi. `ProbeClock` and `ProbeFile` exist
only for type sizes; they are never used for a real handshake or disk access.
Inspect the generated object with the ESP toolchain's `nm -S`, filtering
`T106_`. Measured symbols (hex sizes):

```text
000027a4 T106_DB_BYTES         # 10148, ProbeFile has no fields
00000c30 T106_STACK_BYTES      # 3120, StackResources<4>
000001e0 T106_TLS_BYTES        # 480, excludes borrowed record buffers
000010f0 T106_VERIFIER_BYTES   # 4336, includes 4096-byte leaf capacity
00000018 T106_VERSION_BYTES   # 24 per Option<Version>
```

This proves compilation and fixed layouts, not firmware RAM headroom,
Wi-Fi entropy initialization, synchronous crypto latency, linked image size,
or physical storage/network behaviour. No board operation was performed.

## Live data

`feeds.md` records current source links, timestamp, full-capture provenance and
HTTP behaviour. `feed-page{0,1}.xml.gz` decompress to the untouched complete
responses (100 + 34 entries). `feed-items.xml` is a reserialized six-item
extract, including a real repeated GUID. It is a fixture, not a complete feed.
Move these fixtures to `crates/theo-podcasts/fixtures/` when t-107 is authorized
to create that crate; do not create product scaffolding during this spike.

HTTP headers, source URLs and duplicate indices are retained alongside them;
only public content is included. Audio bodies are not retained or downloaded
in full. `SHA256SUMS` covers retained evidence; check with `shasum -a 256 -c
SHA256SUMS` from this directory. Decompressed response hashes are in the main
feasibility report. TLS root DER is retrieved from its official publisher, not
copied from a server-provided chain as an implicit trust anchor.

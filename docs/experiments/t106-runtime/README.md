# t-106 shared-owner, publication, linked-memory and CDN evidence

Date: 2026-10-30. Base source: **acca56f**. These probes are external experiments;
none is called by firmware boot. No board was flashed, monitored or formatted.
The parser/engine correction from t-116 stays unchanged.

## Reproduce

From the repository root, use a new output directory for each mode:

```sh
python3 docs/experiments/t106-runtime/prepare.py host /tmp/theo-runtime-host
cd /tmp/theo-runtime-host/probe
RUSTUP_TOOLCHAIN=stable RUSTC_WRAPPER= cargo test --locked --offline --release --target aarch64-apple-darwin -- --nocapture
```

Use your computer's host target. The script archives acca56f into the external
output, preserving the tested PageStore, layout and corrected engine rather
than compiling whatever a future checkout happens to contain. The locked host
probe uses the same littlefs-rust main revision as the firmware (732d038b).

For `linked`, prepare another output, change into its printed `base` directory,
then run **only `RUSTC_WRAPPER= CARGO_NET_OFFLINE=true just build`**. This creates
a link-only image, with an opaque false branch retaining the candidate code and
both database/network futures simultaneously. It is not a deployable product
image; do not flash it. The script never runs a build/flash/network command.
The four expected dead-code warnings concern unused prototype hooks and error
payloads; this experiment is not described as a lint-clean firmware feature.

For `cdn`, prepare another output, change to its printed `probe` directory, and
run `RUSTUP_TOOLCHAIN=stable RUSTC_WRAPPER= cargo run --locked --release --target
aarch64-apple-darwin`. This performs two public, bounded network tests: verified
1024-byte audio range, then rejection with a wrong root. It never downloads the
full episode. Each case has a 60-second host timeout.

`SHA256SUMS` covers retained evidence. Verify from this directory with
`shasum -a 256 -c SHA256SUMS`.

## Shared owner and publication

`regions.rs` provides the exact candidate signatures:

- `split<D: SectorIo>(D, CardLayout) -> (RegionIo<D>, RegionIo<D>)` transfers one
  synchronous device into one Rc/RefCell owner and returns non-cloneable,
  disjoint bounded handles. No handle or device reference crosses cores.
- `RegionIo<D>: SectorIo` translates checked relative sectors, drops the unique
  device on I/O error, and refuses every later operation in either region.
- `AsyncRegion<D, B>: SectorFile` yields once after every completed sector/sync,
  with no device borrow held. A supplied `B: FnMut() -> Result<(), Error>` hook
  restores the database request's deadline before each I/O; it must preserve
  the absolute transaction deadline and observe the physical bus poison flag.
- Cancellation before completion drops the shared device. Completion rechecks
  the shared session after yielding, so a log failure wins over a late DB reply.
  The prototype initially missed that last check: `late-completion-red.txt`
  records the failing test before correction.
- `discover(D, card_sectors) -> Discovered<D>` only reads the immutable header
  pair. Absent descriptors try whole-card legacy LittleFS; malformed or
  conflicting descriptors fail. Discovery never formats or initializes a DB.
- `provision_new_image(D, CardLayout)` is an explicitly invoked **host-image
  experiment**, not a firmware provisioning entry point. It refuses a nonblank
  descriptor/superblock prefix, initializes and syncs real LittleFS/log data and
  real EKV/schema, releases all region handles, then publishes/syncs descriptors
  0 and 8. It does not migrate a volume. A future real provisioning tool must
  additionally own/check exact image capacity and require explicit destructive
  authorization; these generic traits cannot establish that provenance.

**12 host tests pass.** They include actual EKV and LittleFS sharing one device,
logs making progress throughout DB commits, reopen of both stores, bounds,
I/O failure, cancellation, deadline-hook restoration, late completion, legacy
read-only discovery and explicit new-image publication. Every one of **625
write/sync boundaries** in preparation is faulted with 0, 217 or all 512 bytes
persisted: **1875 cases**. A discoverable layout must always have the complete
schema and log data. Six cancellation points are sampled from 1203 yields.

The model persists successful writes immediately; it does not model arbitrary
volatile SD caches, reordering, collateral sector damage or physical power loss.
Cooperative host progress establishes scheduling opportunities, not hardware
latency or Embassy/RTOS scheduling measurements. The real service must retain
its cross-core RequestGuard, absolute timeouts, poison checks and bounded sleep
handshake; the local Rc owner does not replace those frontend protections.

## Full linked placement

The disposable full firmware retains Wi-Fi beta.1 initialization and connection,
embassy-net DNS/TCP/DHCP runner, verified TLS/HTTP, and corrected EKV through the
shared region adapter. Symbols confirm real radio initialization, crypto RNG,
webpki and P-256 verification are retained, not just dummy-sized types. The
baseline is the unchanged acca56f release image.

| ELF allocation | Baseline bytes | Combined probe bytes |
| --- | ---: | ---: |
| IRAM vectors + ordinary code | 15196 | 15912 |
| Additional Wi-Fi IRAM | 0 | 34644 |
| DRAM data (including Wi-Fi) | 11524 | 19384 |
| DRAM BSS | 114512 | 150820 |
| Remaining main-stack section | 200512 | 120988 |
| Reclaimed heap section | 0 | 65536 |
| Main Embassy future pool (included in BSS) | 1632 | 23520 |
| Network resources StaticCell (included in BSS) | 0 | 3128 |

Do not add `.rwdata_dummy` to IRAM code: it reserves the aliased DRAM and would
double-count physical use. `.stack` is the linker's remaining stack section,
not measured free stack or a high-water mark. BSS already includes the ordinary
65536-byte heap and existing core-1 stack. The extra 65536-byte heap is at
0x3fcdb700 in `.dram2_uninit`, separate from the normal stack. These are physical
reservations, not proof of runtime allocation margins.

The candidate keeps the existing ordinary64KiB heap and adds reclaimed64KiB,
for 128 KiB internal heap capacity. It uses 6 static RX, 8 dynamic RX, 4 dynamic
TX buffers, BA window3 and RX/TX queues2. Dynamic driver buffer sizes and other
allocations are not all known from the Rust public API. Radio uses opt-level3
in the probe because its package documentation requires optimization2/3; the
existing firmware's size profile is preserved for the rest of the image.

All six transport byte buffers are explicitly allocated in PSRAM: 41216 bytes.
The PageStore cache/page payload adds 102400 bytes at 4096 pages, for **143616
known external payload bytes**. The previously measured 49152-byte transport
total also includes internal objects; do not count that whole total as PSRAM or
add those objects again to the measured future/BSS. The existing framebuffer
adds 329728 PSRAM bytes. Parser/batch/scan buffers are future implementation
costs, not measured here. Exact linked manifests and locks are retained.

The entropy driver refuses use until RF connection succeeded and clears its
ready flag on session drop. UTC starts at zero and blocks TLS until trusted setup
supplies time; no build-time clock or verification bypass is used. No connection
was attempted by this image. This is linked compatibility and placement evidence,
not Wi-Fi bring-up, runtime heap/stack margins, crypto latency, or device throughput.

## Verified audio CDN

The public example from t-106 resolves to media.radiofrance-podcast.net. OpenSSL
verified the observed chain through YR1 / Root YR to **ISRG Root X1**. The trust
anchor was independently retrieved from its official publisher, not adopted
from the peer's chain:

- [ISRG Root X1 DER](https://letsencrypt.org/certs/isrgrootx1.der), SHA-256
  `96bcec06264976f37460779acf28c5a7cfe8a3c0aae11a8ffcee05c0bddf08c6`.
- [Let's Encrypt chain documentation](https://letsencrypt.org/certificates/)
  documents YR1 → Root YR → ISRG Root X1.
- GTS Root R4 remains the independently pinned feed-root candidate; its existing
  SHA-256 is checked by prepare.py. The CDN test rejects that wrong root.

The pinned Rust TLS/HTTP candidate verified chain, hostname, signature and dates,
then returned HTTP206, Content-Length1024 and Content-Range bytes0-1023 with a
1024-byte body. The second fresh TLS connection failed InvalidCertificate with
GTS Root R4. Source and output are retained; no audio body or session secrets
are retained. This does not establish range/resume support or the same chain
for every enclosure. Future code must validate each redirect and reject any
unconfigured authority; unknown trust/configuration must never fall back to
unverified TLS or plaintext.

## Remaining work

Wire these proven contracts into the actual single-owner firmware service and
explicit host provisioning path, enforce queue/transaction/field limits and
shutdown, then implement the catalogue/feed/download stages. The linked probe
shows a viable candidate layout, not a certified runtime memory budget. Device
heap/stack/throughput and physical power-loss tests need explicit authorization.

ADR-0016 records the software compatibility gate and the selected bounded
contracts. Host catalogue work can proceed; production wiring and runtime /
hardware certification are separate requirements, not claims of these probes.

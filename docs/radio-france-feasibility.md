# Radio France implementation feasibility — t-106

Current update (2026-10-30): t-106's software compatibility gate is verified in
ADR-0016 and `docs/experiments/t106-runtime/`. Corrected engine recovery, actual
shared-owner/publication models, combined linked placement and verified feed/audio
TLS permit host catalogue work. Earlier negative-gate evidence below remains
historical. Firmware activation and physical validation are separate requirements.


Date: 2026-10-29. **Decision: stop dependent podcast persistence work.** The
selected EKV 1.0.0 + PageStore path loses access to committed keys in a metadata
rollover interruption test. This is a negative feasibility result, permitted
by Task 1's stop condition; it is not completion of the podcast feature.
[ADR-0014](adr/0014-gate-podcast-persistence-on-recovery.md) records the gate;
Oku **t-116** tracks correction and revalidation. No product dependency, database
initialization, card layout, or flashed firmware changed during this stage.

Feed retrieval and verified HTTPS now have concrete evidence. No engine is
currently verified for the **current** SD architecture. This does not establish
that all embedded engines are unsuitable: redb remains an alternative requiring
a new storage/latency proof, rather than an unannounced substitution for EKV.

## Current source and complete live capture

Aerion's current source is Go, commit
[`95e519deab04037c002ef22563e7c56d3bcb51bc`](https://github.com/Aerion/rss-radio-france-pour-tous/tree/95e519deab04037c002ef22563e7c56d3bcb51bc).
The earlier JavaScript inspection at `12037d21d9465885efa9eeab951045898ca178af`
is historical evidence, not the current deployed contract.

- [HTTP handlers](https://github.com/Aerion/rss-radio-france-pour-tous/blob/95e519deab04037c002ef22563e7c56d3bcb51bc/internal/httpapi/handlers.go)
  expose `/rss/{showId}?page={zeroBasedPage}` and the legacy `/audio/{id}` route.
- [Radio France retrieval](https://github.com/Aerion/rss-radio-france-pour-tous/blob/95e519deab04037c002ef22563e7c56d3bcb51bc/internal/radiofrance/shows.go)
  clamps negative pages to zero. **`page=-1` no longer means complete history.**
- [Feed builder](https://github.com/Aerion/rss-radio-france-pour-tous/blob/95e519deab04037c002ef22563e7c56d3bcb51bc/internal/feed/build.go)
  emits `atom:link rel="next"`, generally direct CDN enclosures, optional duration,
  and no enclosure length. GUIDs on/before 2022-09-12 UTC use manifestation IDs;
  later GUIDs use diffusion IDs. GUID and audio identity remain different.

Captured 2026-10-29 17:29–17:31 UTC: **Espions, une histoire vraie**, show
`4a41823f-f1f7-4725-8380-e428893eb93b`, at
[page 0](https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b)
and [page 1](https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b?page=1).
100 + 34 emitted items, **129 distinct GUIDs**, five repeated pairs; page 1 has
no next link. This is all history exposed by that capture, not independent
proof that Radio France has no additional archive. Pagination has no snapshot
guarantee; duplicates or shifts must never delete absent local records or reset
read/download state.

The complete original responses are retained compressed under
[experiments/t106](experiments/t106/README.md), together with a derived six-item
fixture, headers and provenance. Decompressed SHA-256:

```text
page0 7fdfc9f83c4457ab015f81a1786c4d755b87e0ea573999f1f490e59dad0bd413
page1 2e0b489a819c06cbe15e8096ac5fd4171f3a76d9444c8f02e40de45a1bfac60b
```

Observed RSS responses: 200 XML, no ETag, Last-Modified or Content-Length;
134/134 enclosures omit length and include duration. Stream pages (page 0 is
309381 bytes), rather than buffering a whole response. `page=-1` returned the
same body as page 0. First/last observed dates are 2026-08-21 and 2020-06-27.
Use conditional requests only when the actual response supplies validators;
otherwise refetch and reconcile every page idempotently. A failed/truncated page
must not advance a complete-sync checkpoint.

For the first CDN enclosure, HEAD returned 200, length **56678816**, strong
ETag, Last-Modified and Accept-Ranges. Range `0-1023` and `1024-2047` with the
matching If-Range both returned 206 and consistent Content-Range. RSS declares
`audio/mpeg` but this `.m4a` representation is `audio/mp4`; container/decoder
selection cannot rely solely on RSS MIME. No full audio file was downloaded.
A legacy `/audio/` request returned 502 twice: successful live legacy redirects
remain unverified. Feed/CDN requests succeeded, while later RSS requests returned
530 with both curl and the Rust client. Treat these as retryable server errors,
not a reason to disable TLS. More shows/CDN authorities need later coverage.

## Database result and crash boundary

[EKV 1.0.0](https://github.com/embassy-rs/ekv) and
[redb 4.4.0](https://docs.rs/redb/4.4.0/redb/) with `default-features=false` and
`experimental-api-5` both compile for Xtensa. redb's earlier host experiment
passed insert/update/delete/abort/reopen with a 64 KiB cache and 61440-byte file.
Its synchronous backend was not integrated or proved against the current SD
owner; this is an alternative, not a selected working backend.

EKV's async Flash and ordered transactions fit the intended owner better, but
page integrity alone is insufficient:

| Experiment | Observed result |
| --- | --- |
| Existing PageStore tests | 9 pass, including schema rejection, failed sync and cancellation |
| Real host file + EKV | CRUD/reopen/scan pass; 90 torn-sector positions and successful endpoint preserve atomic pair |
| Interrupted ordinary commit, complete logical write boundaries | Pass |
| Interrupted metadata rollover, same PageStore | **Fail: mount succeeds, read returns KeyNotFound** |

The rollover test publishes a complete 20-byte metadata header at page 13,
then interrupts before its following 8-byte write. Both adjacent cancellation
boundaries reproduce loss of access to an earlier committed pair. No partial
logical header is necessary. The complete run has 11 passing tests and one
intentional failure, retained with a reproducible external-project setup in
[the evidence directory](experiments/t106/README.md). It is not part of the
normal firmware test suite and must not be described as green.

This is consistent with open upstream [issue #26](https://github.com/embassy-rs/ekv/issues/26)
and the [reproducer PR #25](https://github.com/embassy-rs/ekv/pull/25), checked
on the experiment date. Those upstream reports are supporting context; the
actual gate is our reproduced failure through PageStore. No dependency patch
was applied. Existing tests sampled a smaller history and did not establish
metadata-rollover safety.

### Verified candidate types, not an accepted durable backend

The disposable [adapter](experiments/t106/adapter.rs) compiles on host and
Xtensa. Its concrete type is:

```rust
type Db<F> = ekv::Database<PageStore<F>,
    embassy_sync_06::blocking_mutex::raw::NoopRawMutex>;
```

`embassy-sync-06` is the explicit alias for EKV's 0.6 mutex trait; firmware's
0.8 mutex type is not interchangeable. `recover(&Db<F>)` returns
`MountError<PageError<F::Error>>`; `get` fills a borrowed output slice;
`put`/`batch` return distinct Write/Commit errors. `scan` keeps one read
transaction/cursor and fixed 64-byte key/4096-byte value buffers, invokes a
non-async callback and retains no result collection. This prototype bounds
memory, not scan duration: production must add resumable cursors, cancellation
and a per-request item budget. Batch callers must provide sorted unique keys;
production must enforce its operation/byte cap before entering a transaction.
**A successful commit cannot currently be treated as interruption-safe.**

PageStore stays 32–4096 logical 4096-byte pages, two physical slots each:
`512 + pages * 2 * 4608`; 4096 pages occupy **37749248 bytes**. Maximum value
4096, key 64. Header/schema mismatch rejects open without formatting or writes.
Application schema migration remains t-107; this probe tests storage format
version rejection, not a finished podcast schema.

### Storage architecture after LittleFS

ADR-0012 replaced runtime FAT with LittleFS on core 1, served by a core-0 async
sector owner. The old FatPageFile is legacy evidence. A large random-access EKV
file on LittleFS is unsuitable: updating near its beginning copies the file
suffix. A **future separately bounded raw sector region** is a candidate,
sharing the same unique sector client and owner; it is not allocated now.
The current card has a whole-card LittleFS mount. Reserving a region requires
an explicit versioned layout and separately authorized migration/reformat;
never carve it out during boot or overlap it with the existing mount.

Even after engine correction, each I/O failure/cancellation must poison that
owner until reboot, and sync ordering must hold. Core 1 may block waiting for
core 0 only through the existing deadline-aware sector bridge; the core-0
executor must remain free to service the shared I2C bus. No synchronous database
may run on that core-0 executor. Simulated host barriers cannot establish
physical SD power-loss guarantees or throughput.

## Verified HTTPS candidate

Disposable locked combination, **not added to product Cargo.toml**:

| Component | Exact tested version/revision |
| --- | --- |
| esp-hal / esp-alloc / esp-rtos | 1.2.2 / 0.11.0 / 0.4.0, matching firmware |
| esp-radio / radio RTOS driver | 1.0.0-beta.1 / 0.4.2 |
| embassy-net / embedded-io-async | 0.9.1 / 0.7.0 |
| reqwless | 0.14.0, default features disabled |
| embedded-tls | 0.19.0 git `6487c62bb084c33361822022e21836ddfd48388f` |
| embassy-crypto + RustCrypto drivers | 0.1.0 git `822a76ea91f2c0885c6eb0c65fe91181e31133e1` |
| unitrait, transitive | git `bb96601b3d293fa27d1ca2891f6ea148e534e433` |
| rustls-webpki / rustls-pki-types | 0.103.16 / 1.15.1 |

[embedded-tls source](https://github.com/embassy-rs/embedded-tls/tree/6487c62bb084c33361822022e21836ddfd48388f)
now provides a no_std WebPKI verifier, avoiding the earlier 0.18 Rust-PKI DER
compile failure and CN-only limitation. Features used: `webpki,p256,p384,ed25519,rsa`;
matching SHA-256/384, HMAC, AES-128-GCM, P-256 ECDH/ECDSA, P-384 ECDSA and Ed25519
software drivers are pinned in the retained manifest/lock. RSA requires alloc.
The registry beta.2 radio candidate was yanked in the earlier resolution;
beta.1 resolves and compiles with the exact existing HAL/RTOS above.

Actual DNS → `embassy_net::tcp::TcpSocket` →
`embedded_tls::TlsConnection<'_, _, Aes128GcmSha256>` →
`Request::get(...).write_header(...)` → `Response::read(...).body().reader()`
APIs compile for Xtensa. reqwless's high-level TLS client is deliberately not
used: the [low-level adapter](experiments/t106/https.rs) chooses its own verifier
and supplies streaming Read/Write. No complete-response allocation is needed.

The same TLS/HTTP adapter on the host fetched **1680 bytes** from the public
Aerion search endpoint over authenticated HTTPS. Wrong hostname fails
InvalidCertificate; empty root fails DecodeError; missing time fails Clock.
43 upstream WebPKI tests pass, including intermediate-chain, wrong-signature,
wrong-name, expiry, and leaf-size cases. Host success and Xtensa compilation do
not prove linked firmware size, Wi-Fi initialization/entropy, RF connectivity,
or crypto scheduling latency. No board operation was needed or performed.

### Trust, time and credentials policy

The live Aerion chain used WE1 and **GTS Root R4**, retrieved from
[Google's root repository](https://pki.goog/repo/certs/gtsr4.der), SHA-256
`349dfa4058c5e263123b398ae795573c4e1313c83fe68f93556cd5e8031b3c7d`.
Production must pin an explicit firmware trust-anchor set for each permitted
feed/CDN authority, updated through firmware releases; this single-root spike
is not a complete CDN trust policy. Always set the DNS hostname, validate SAN,
chain, dates and handshake signature, and use fresh TLS state after failure.
Never promote a server-supplied root to trusted status.

Upstream treats `TlsClock::now() == None` as permission to ignore dates.
The tested StrictVerifier checks time again inside `verify_certificate` and
rejects None, in addition to the pre-connect check. Production time bootstrap
is a user-provisioned UTC timestamp through the local setup channel, combined
with monotonic uptime during the powered session; require new time after a
cold boot or sleep without a proven retained clock. An SD timestamp or firmware
build time is only a sanity bound, never sufficient current trusted time.
Unauthenticated SNTP and an HTTP Date header cannot bootstrap certificate trust.
Until setup supplies time, offline reading works and networking waits. No
certificate failure falls back to plaintext or unverified TLS.

Define local credentials at `config/wifi.local.toml` (gitignored); t-110 must
implement its parser/setup path and pass values directly to station config.
No credentials were needed or read here. Do not log passwords or auto-load
this file into distributable release images. Cryptographic RNG registration
must use a documented ESP entropy source with the required radio/TRNG state;
checking the generic TLS library does not prove this runtime setup.

## Measured memory and provisional operational bounds

Xtensa object symbol sizes, not host estimates: `Option<Version>` **24** bytes,
`Db<ProbeFile>` **10148** bytes (zero-size dummy backend), `StackResources<4>`
**3120**, TLS connection **480** excluding borrowed buffers, verifier **4336**
including a 4096-byte leaf. At 4096 pages PageStore heap payload is
**102400 bytes** (98304 version cache + 4096 page). Host DB with actual file
backend is 10328 bytes. No whole-catalogue RAM image is needed.

Measured buffer configuration: TCP RX/TX 4096 each, TLS RX 16640/TX 4096,
HTTP headers 8192, streaming chunk 4096. Together with the measured network
objects this is **49152 bytes**, excluding async future/stack, Wi-Fi allocations,
crypto scratch, allocator metadata, domain parser and existing firmware use.
Board configuration supplies a 64 KiB internal heap and 8 MiB PSRAM; this is
**not a proof of combined runtime fit**. Large buffers/cache must be explicitly
allocated in PSRAM, leaving required internal/DMA memory available. Do not
use the allocator's internal-first spill behaviour as a memory budget.

The following are bounded **proposals for revalidation after t-116**, not
measured hardware performance or already implemented interfaces:

| Resource / operation | Proposed cap / policy |
| --- | --- |
| Concurrent network transfer / storage queue | 1 / 1; reuse existing sector bridge, no second mount |
| Batch / scan response | 16 operations and 16 KiB encoded input / 16 records per request |
| Feed field UTF-8 bytes | GUID 256; title 512; description 2048; URL 2048; host 64 |
| HTTP headers / redirects | 8192 bytes and library's 64 headers / 5 HTTPS hops; detect loops, validate each authority, strip origin-specific headers |
| XML processing | Stream; reject oversized required fields; skip optional oversized description; no DTD/entity expansion; no checkpoint on truncation |
| Network waits | DNS 10 s, TCP 10 s, handshake 20 s, header 15 s, per-read idle 15 s; cancellation closes connection |
| Storage waits | Existing mount 2 s and append 500 ms remain; DB transaction/startup budget still unproved, never run in UI pass |
| Free-space reserve | Greater of 16 MiB and 1% of media region; keep DB region separate; reserve at least one 4096-byte chunk before every unknown-length append |
| Synchronization | Follow, explicit request, then every 6 h while awake/connected; full initial pagination; no wake solely to sync |
| Retry | 30 s, 2 min, 10 min, 1 h with jitter; honor longer bounded Retry-After; cert/time/config failure waits for correction |

Unknown audio length is normal. Reclaim oldest eligible read audio only when
an actual download needs space, never unread/active audio and never a scheduled
purge. Tombstones remain after unlink. Resumption is optional: require saved
strong validator and exact 206 Content-Range; a 200, changed/missing validator,
416 or changed total restarts safely through a temporary file. RSS validators
are not a delta API, and observed support on one CDN representation is not a
guarantee for every enclosure.

## Exit decision and next work

t-106 yields a **negative persistence gate**, retained source/fixtures, concrete
candidate APIs, a compiled TLS combination and measured fixed sizes. Production
scan/batch enforcement, trustworthy device clock/setup, multi-authority roots,
combined RAM/latency and SD-region publication are not implemented or certified.
They cannot be inferred from successful generic compilation.

First resolve **t-116**: an upstream EKV fix with the rollover regression plus
full boundary tests, or an explicitly reviewed alternative such as redb on the
isolated owner, or transaction-level copy-on-write publication with its extra
space/I/O costs. Then repeat the positive feasibility gate, decide the versioned
SD layout without touching a live whole-card mount, and refresh t-107–t-110
interfaces. No custom index, engine switch, repartition, or firmware network
integration is silently authorized by this negative finding.

### t-116 continuation: isolated recovery correction

The follow-up [t-116 experiment](experiments/t116/README.md) reproduces the
missing-key defect and identifies publication ordering in
`FileTransaction::commit`: the new metadata page header becomes visible before
its final record chunk is committed. An isolated EKV 1.0.0 patch reverses that
order. Expanded fault injection additionally found an `unwrap()` panic on
rollover metadata I/O errors; the candidate propagates those errors instead.
The original 12-test probe passes with the publication-order correction, and
the two-correction candidate compiles with the pinned Xtensa transport probe.
The expanded 13-test suite passes, including 8112 sector/sync fault cases
(1352 boundaries × 3 persisted prefixes × error/cancellation). Results are
retained with that experiment.
This is not an adopted firmware dependency or a reopened positive gate; the
combined budget, separate storage layout and broader engine validation remain.

### Adopted t-116 correction and broader history evidence

The project now uses EKV 1.0.0 from a local crates.io override containing only
the two reviewed `src/file.rs` corrections. `just test-storage-recovery` runs
the retained side-by-side tests, including 3238 logical cuts across six mixed
history checkpoints, 2964 sector/sync fault cases at generation 96, and the
earlier first-rollover sweep. The 192-commit history has three rollovers,
787 erases, variable values, deletions and a retained read/reclaimed record.
Unpatched EKV fails both the full-model and post-recovery-history assertions;
the patched engine passes. Twelve recovery endpoints then accept twelve 4 KiB
value commits each and reopen with the complete expected catalogue.

`card_layout` is a pure checked descriptor API, not runtime provisioning.
PageStore now supports `create_in`/`open_in`; both buffers use the supplied
allocator and OOM fails before storage writes. Xtensa compilation proves
`&esp_alloc::ExternalMemory` for both constructors. ADR-0015 documents exact
regions, descriptor validation, measured fixed sizes and the unresolved
internal-RAM/radio allocation issue. No DB region has been reserved on the card.

## 2026-10-30 shared-owner and full-link continuation

See ADR-0016 and the reproducible t106-runtime evidence for twelve passing
shared-owner/publication tests, 1875 initialization fault cases, joined real
engine/network candidate linkage and a bounded verified audio-CDN range request.
The main future pool is23520 bytes and the remaining linker stack section120988;
these are reservations, not measured runtime margins. Ordinary64KiB plus a
separate reclaimed64KiB heap link successfully. The byte buffers/cache have
143616 bytes of known PSRAM payload; the previous transport total also contains
internal objects and must not be double-counted. Actual boot still has none of
these experimental network/region changes. Host catalogue work may proceed;
runtime wiring, trusted setup, queue/bounds/failure handling and device profiling
remain separate required work before activation.

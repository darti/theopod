# Radio France Offline Podcasts Implementation Plan

> Oku: t-105, stages t-106–t-110. **t-106 software compatibility gate passed 2026-10-30** (ADR-0016).
> Corrected engine recovery, shared-owner/publication model, full linked placement
> and verified feed/audio TLS have retained evidence. Host catalogue work may
> proceed. Production activation, bounds/scheduling and hardware validation remain
> Task 5 / explicitly authorized follow-up work, not claims of these experiments.
> Default lifecycle: close after main merge. Explicit user exception on2026-10-30:
> commit and close completed t-106/t-107/t-116 checkpoints; parent and unfinished stages remain open.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Download the complete available history of followed Radio France podcasts, synchronize metadata changes, and recycle read audio only when space is needed.

**Architecture:** A host-testable no_std podcast crate owns catalogue and download rules. One firmware storage service owns LittleFS and any future separately bounded database region; background network tasks access it through bounded requests. Radio France rules remain isolated for future providers, without implementing generic feed support now.

**Tech Stack:** Rust no_std, ESP32-S3, Embassy, littlefs-rust on core 1 with the core-0 async sector service (ADR-0012); database and HTTPS dependencies selected by compatibility experiments in Task 1.

**Spec:** `docs/superpowers/specs/2026-10-29-radio-france-design.md`

## Global Constraints

- Preserve no_std/no_main, Xtensa target and current dependency versions.
- First release supports Radio France only; no generic subscription UI or player implementation.
- Follow imports all available history. Local state survives feed changes and audio recycling.
- Read means reclaimable, not immediately unavailable. No scheduled purge or age threshold.
- Recycle oldest eligible audio only for a download needing space; never unread or active audio.
- Keep a single storage owner, bounded memory and waits, responsive UI and existing sleep handshake.
- A failed/cancelled storage operation invalidates the mount until reboot; expected capacity exhaustion must be handled before writes where possible, with a full-write failure stopping storage safely.
- No flash or serial monitoring without explicit user authorization.
- Regression tests are side-by-side `*_tests.rs` with `Regression guard` comments.

## Review Focus

- Duplicate or changed GUID/enclosure values must not reset read state (Task 2).
- Oversized or truncated historical feeds must not advance HTTP validators (Task 3).
- Unknown-length audio must not consume metadata/log reserve (Task 4).
- Reset between file deletion and database update must retain a tombstone (Task 4).
- Full media, failed storage and network stalls must not block UI shutdown (Task 5).

## Delivery sequence

Execution update (2026-10-29): t-106 has a negative persistence outcome, not a
working podcast backend. PageStore protects logical page replacements, but a
metadata-rollover interruption still yields KeyNotFound after mount. t-116 must
resolve this before later stages. A real two-page feed and verified TLS/HTTP
adapter are retained; negative page numbers are no longer an all-history API.
Original FAT directions below are historical implementation proposals only:
ADR-0012 replaces them with LittleFS and the existing unique sector service.
A large random-access LittleFS database file is unsuitable, and a raw database
region is unallocated; design its versioned layout only after recovery is sound.
No existing whole-card mount may be overwritten to implement that proposal.

Storage feasibility, domain rules, feed reconciliation, downloads/recycling and
firmware integration are sequential deliverables. Task 1 resolves dependencies
before later integration interfaces are fixed; do not invent a working engine
or HTTPS stack in the plan. If it fails, report the measured limitation and
revise the affected tasks before implementation continues.

### Task 1: Prove database, transport and feed compatibility

**Current outcome:** software feasibility verified (ADR-0016). The original
negative outcome below is historical. Host Task 2 may proceed.

**Original outcome:** stop condition exercised; see ADR-0014. The checkboxes below are
the original positive-gate criteria, not a claim that every one passed. Successful
feed/transport checks and the failing recovery case are recorded in the
feasibility report. t-116 precedes resuming the remaining positive gate.

**Files:** create `docs/radio-france-feasibility.md`; extend the approved spec with measured limits; add an ADR under the next free number in `docs/adr/` only after selection. Use disposable experiments outside the workspace for candidate APIs, without product scaffolding.

**Interfaces:** produces a documented database adapter contract (`get`, `put`, bounded ordered scan, durable batch commit, reopen/recover), its exact Rust types and the selected engine; produces selected HTTPS adapter types and Radio France fixture provenance.

- [x] Read Aerion's current worker and source modules, record commit SHA, exact feed route and pagination behaviour. Retrieve a representative complete RSS and a small fixture; verify GUID, publication dates, enclosures, redirects, validators and content lengths. Save sanitized fixtures under `crates/theo-podcasts/fixtures/` when Task 2 creates the crate, with source/commit provenance.
- [x] Compare available embedded database engines using primary documentation and code. Reject engines requiring std, unavailable filesystem APIs or incompatible existing dependencies. Exercise an on-file database through an adapter resembling async FAT; measure memory and bounded I/O requirements. Determine whether bounded synchronous engine calls can safely run on the existing async executor without blocking it.
- [x] Prove insert/reopen, update/reopen, interrupted commit recovery, deletion and schema-version rejection on the host. Compile the chosen dependency and adapter for Xtensa using the ESP environment through a temporary just recipe. Record expected crash guarantees and remaining FAT power-loss limits.
- [x] Prove the Wi-Fi/DNS/TLS/HTTP dependency combination compiles with existing esp-hal/esp-rtos versions. Define credential provisioning (local ignored configuration), certificate roots, trusted time bootstrap and certificate failure behaviour. No actual board connection is required.
- [x] Record exact dependency versions, adapter signatures, queue/chunk/field limits, timeouts, storage reserve, sync trigger and retry schedule. Values must fit measured internal RAM/PSRAM constraints. Resolve missing HTTP features explicitly rather than assuming remote delta or range support.
- [x] If no engine/transport fits, stop dependent implementation and present evidence and alternatives. Otherwise add the architecture ADR and update this plan with verified signatures and limits before Task 2.

### Task 2: Implement persistent episode lifecycle rules

**Files:** create `crates/theo-podcasts/{Cargo.toml,src/lib.rs,src/catalogue.rs,src/catalogue_tests.rs}`; modify root `Cargo.toml` and host package lists in `justfile`.

**Interfaces:** `SubscriptionId(u64)`, `EpisodeId(u64)`; `Source::RadioFrance`; `AudioState::{Missing,Queued,Downloading,Available,Deleting,Reclaimed}`. Define `EpisodeState { read: bool, audio: AudioState }`, `mark_read(&mut EpisodeState)`, `should_queue(&EpisodeState) -> bool`. Database adapter uses Task 1's proven contract; domain IDs never derive from a mutable title or URL filename.

- [x] Add failing tests: `read_audio_stays_available` asserts marking Available read preserves Available; `reclaimed_read_is_not_queued` asserts read+Reclaimed never queues; `metadata_update_preserves_state` asserts local ID/read/audio unchanged after metadata updates; `subscription_identity_scopes_guid` asserts identical GUIDs across subscriptions remain distinct.
- [x] Run `just test`; confirm new assertions fail because the lifecycle is not implemented, then implement the minimal state transitions and persistent key/schema encoding.
- [x] Add tests for GUID-less enclosure fallback, GUID retained with changed enclosure, duplicate GUID upsert, schema rejection and reopen after state changes. Reuse the selected database adapter on a host file; do not claim persistence from an in-memory mock alone.
- [x] Run `just fmt`, `just test`, `just check` and `just audit` for added dependencies. Commit the independently testable crate and recipe updates.

### Task 3: Reconcile complete Radio France feeds

**Files:** create `crates/theo-podcasts/src/{radio_france.rs,radio_france_tests.rs,sync.rs,sync_tests.rs}` and fixture files; export modules from `src/lib.rs`.

**Interfaces:** incremental `RadioFranceFeed` parser emits owned, size-bounded `EpisodeMetadata`; `SyncSession` stages upserts and commits response validators only after valid end-of-document and all required pages. Transport supplies chunks and HTTP response metadata, never an unbounded full body.

- [x] Write failing fixture tests for full-history import and repeat import: first run queues every unique episode; second run adds zero download jobs. A third fixture changes a title and adds one GUID: update one entry and queue exactly one new episode.
- [x] Run `just test` and observe the intended failures. Implement source validation, incremental RSS parsing, entity/CDATA handling and feed identity reconciliation with bounded batch commits.
- [x] Test HTTP 304 as a no-op, missing validators, absent old entries, missing GUID, malformed dates, XML split across chunks, truncated XML, oversized fields and failed final commit. Assert no completed sync/validator advance on partial failure and idempotent replay after reopening.
- [x] Test source boundary: unsupported subscription source is rejected; enclosure redirects use transport validation rather than treating audio CDN URLs as subscription URLs. Explicitly disable XML external entity resolution.
- [x] Run `just fmt`, `just test` and `just check`; commit feed reconciliation and fixtures.

### Task 4: Download audio and reclaim only under pressure

**Files:** create `crates/theo-podcasts/src/{download.rs,download_tests.rs,reclaim.rs,reclaim_tests.rs,recovery.rs,recovery_tests.rs}`; extend the selected persistence adapter.

**Interfaces:** `DownloadJob` carries episode ID, enclosure URL and optional expected bytes; `DownloadOutcome::{Complete,WaitingForSpace,RetryLater}`. `ReclaimCandidate` carries episode ID, read flag, availability, publication/import order and active-use state. `choose_reclaim` yields oldest eligible candidates until the requested deficit is met. Missing timestamps use import order.

- [ ] Write failing tests proving no reclamation with sufficient space, oldest-first selection for a deficit, read audio playable before deletion, exclusion of unread/active files, waiting when candidates are exhausted, and no automatic requeue of reclaimed read episodes.
- [ ] Run `just test`, confirm failures, implement the selection policy and durable deletion intent → unlink → Reclaimed transition. No clock-driven purge job exists.
- [ ] Test temporary download publication after sync only, short HTTP body, unknown length with reserve intact, connection failure, Range ignored (200), stale validator and invalid Content-Range. Restart partial downloads unless safe resume is proven.
- [ ] Inject failure before/after every deletion and completion step, reopen storage and assert no unread deletion, no false Available state and preserved read tombstones. Recover orphan temporary files by persisted identity, never remote-provided paths.
- [ ] Run `just fmt`, `just test` and `just check`; commit lifecycle and recovery logic.

### Task 5: Integrate background services with the existing firmware

**Files:** create `src/device/{network.rs,storage.rs}` and `src/podcasts.rs`; modify `src/device/{mod.rs,logging.rs,sd.rs,sleep.rs}` only where ownership/shutdown requires, `src/main.rs`, `Cargo.toml`, `justfile`, `.gitignore`, `README.md` and ADR index.

**Interfaces:** `storage.rs` becomes sole owner of `Option<SdStorage>` and the database. Logging retains its current nonblocking producer queue; storage serves bounded catalogue/file requests alongside log flushes. `podcasts.rs` exposes follow-Radio-France-feed and mark-read commands, progress/status and explicit sync/retry requests. UI integration is a minimal command/status hook, not a full player.

- [ ] Add host-testable scheduler tests: continuous audio writes do not starve logs/control messages; shutdown cancels network work and acknowledges bounded storage flush; storage invalidation rejects queued writes; capacity wait wakes only on relevant state change/retry.
- [ ] Run `just test`, confirm failures, implement the service scheduler and migrate mount ownership without adding a second FAT instance. Preserve existing 500 ms log I/O and 1.2 s final-flush bounds unless a documented, tested ADR revision is necessary.
- [ ] Add the verified Wi-Fi/HTTPS adapter, bounded sync/download worker and ignored credential configuration. Keep no-credentials/offline startup functional. Wire Radio France subscriptions and states to persisted configuration; reconnect triggers reconciliation without duplicate simultaneous jobs.
- [ ] Add transport/fake-storage integration tests for offline boot, TLS rejection, redirects, card removal, full-media waiting and sleep during each operation. Verify storage errors retain the existing disable-until-reboot policy.
- [ ] Update README with configuration, Radio France-only scope, sync triggers, read/reclamation behaviour, recovery limits and verification commands. Document playback as future work.
- [ ] Run `just fmt`, `just check`, `just test`, `just lint`, `just audit`, `just build`; all must succeed. Run `just miri` only if driver unsafe code changed. Review final diff for credentials and generic-feed features outside scope; commit.

## Hardware validation after explicit authorization

Confirm complete-history retrieval over HTTPS, sustained SD writes with responsive
touch/display/logging, full-card reclamation, offline reboot, deep sleep during
download and recovery after power loss. Host and compilation results do not
establish board throughput or FAT power-loss integrity. Do not flash to obtain
these results without a separate explicit request.

## t-116 recovery investigation continuation

See `docs/experiments/t116/README.md` for the isolated EKV 1.0.0 candidate:
commit replacement metadata records before publishing its higher-sequence page
header, and propagate rollover flash errors instead of panicking. This does not
change the firmware dependency or lift Task 1's stop condition. Retain the
expanded fault-injection result and revalidate broader histories, combined
memory/latency and the separate database region before resuming Task 2.

## t-116 source adoption

The recovery correction is now adopted through `vendor/ekv` (same version,
API and format), with side-by-side regressions and `just test-storage-recovery`.
The original recovery stop reason is corrected; the overall t-106 positive
runtime gate remains incomplete. `card_layout` validates a future immutable
region descriptor without writing the card. PageStore's explicit allocator
constructors compile with PSRAM on Xtensa. See ADR-0015 for measured sizes and
remaining internal heap, shared-owner region I/O and latency requirements.

## 2026-10-30 software gate and next task

ADR-0016 and `docs/experiments/t106-runtime/` supersede the remaining software
stop reasons from t-106. Twelve shared-owner/publication tests pass, including
1875 preparation fault cases; the full candidate links with joined database /
network futures, a separately placed64KiB reclaimed heap and a120988-byte
linker stack section. The real audio CDN passes a bounded verified range request
and rejects the wrong root. These are software feasibility results, not runtime
heap/stack/latency or physical durability certification.

Proceed to t-107 on the proven host-file engine/adapter. Keep both t-106 and
later tasks open until their corresponding changes merge. The next crate must
use GUID256/title512/URL2048/description1024 byte caps, keeping encoded episode
values within4096 bytes. Batch and scan count caps also have16KiB byte caps.
Prefer caller-supplied bounded catalogue scratch buffers so firmware can choose
PSRAM placement; do not silently spend the driver's internal heap on records.
Actual owner wiring, offline allocation failure, trusted setup, queue fairness
and bounded sleep remain t-110 implementation/validation requirements.

## t-107 implementation checkpoint (2026-10-30)

The no_std catalogue now owns the corrected EKV instance, persists local IDs /
identity indexes / read and audio state atomically, rejects unknown schemas,
and exposes bounded record-buffer views and16-row/16KiB scans with continuations.
Required identity/title/URL limits fail before writes; optional descriptions
above1024bytes are omitted. Repeated metadata and state no-ops do not rewrite
storage. Normal tests use real sync_all files; the510-cut file fault model is
explicitly immediate-durability. Twenty catalogue tests pass plus its separate
fault sweep; all normal host tests total394 passing. Xtensa check/lint, audit
and release build pass; separate review clean. ADR-0017 specifies the schema.
RSS/provider reconciliation remains t-108, audio/reclaim t-109, and actual
runtime service/activation t-110. The user explicitly requested committing and
closing completed tasks on2026-10-30; closure follows that request while the
parent feature and incomplete stages stay open.

## t-108 reconciliation checkpoint (2026-10-31)

Bounded UTF8 XML parsing now streams the retained100+34item real history,
resolves Atom namespace aliases, reconciles129unique identities and preserves
local read/audio state across repeats and metadata changes. Sync checkpoints
commit only after every page ends validly. Invalid XML, limits, source/pagination
changes, failures and cancellation cannot advance validators. Multi-page histories
are fetched unconditionally because a root304 cannot certify historical pages.
One-record durable upserts are bounded batches within16rows/16KiB. The large
real-file history test uses the explicit immediate-durability file model;
small persistence/failure/reopen tests keep sync_all. See the t-108 extension
in ADR-0017 for syntax/resource limits and remaining transport responsibilities.

Validation:410normal host tests pass (six existing explicit long-sweep ignores),
fmt/check/lint/audit/release build pass. Separate review found three XML/pagination
issues, corrected with regression coverage. No transport or firmware activation,
no card operation. t-108 and parent t-105 stay open until merge; t-109 and t-110
remain required for downloads, pressure reclamation and runtime integration.

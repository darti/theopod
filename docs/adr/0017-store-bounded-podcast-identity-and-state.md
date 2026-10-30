# ADR-0017: Store bounded podcast identity and local state in EKV

- Status: Implemented host catalogue; firmware activation remains t-110.
- Date: 2026-10-30
- Related: t-105, t-107; consumes the verified contracts in ADR-0016.

## Decision

Add the HAL-independent no_std `theo-podcasts` crate, using the selected corrected
EKV engine. `Catalogue<F: Flash>` owns its database and private embassy-sync0.6
NoopRawMutex. No database reference or competing writer escapes. Operations
require `&mut self`; the eventual service keeps the owner on the storage core.
The caller supplies a mutable4096-byte record buffer, and decoded metadata
borrows that buffer. The crate allocates no record strings or whole catalogue
and introduces no implicit internal-heap dependency for record data. Rust
lifetimes prevent retaining a view while reusing its buffer. Parser/transport
ownership and firmware PSRAM placement remain the integration's responsibility.

### Identity and atomic persistence

Stable local EpisodeId values are checked monotonic u64 counters, independent
of title, URL spelling or filename. SubscriptionId is supplied by the owning
subscription service and stored with every episode; zero is rejected. Source
is RadioFrance only. A subscription-plus-GUID identifies a record; missing or
empty GUID falls back to the exact enclosure URL. Changed GUIDs are not guessed
to match old episodes by title or URL: old records/read state remain preserved.
A retained GUID with a changed enclosure keeps its ID and local state.

Identity keys contain `i`, subscription ID big-endian8, identity-kind1 and the
SHA-256 digest32 of the exact identity bytes:42 bytes within the64-byte engine
limit. GUID and enclosure identities use different kind tags. The complete
identity is checked against the loaded record before update, so a damaged index
or digest collision cannot overwrite another episode's state. Episode keys are
`e` plus big-endian ID8, giving import-order scans without loading all records.

A new episode record, identity mapping and next-ID counter commit together in
one sorted EKV transaction (`e`, `i`, `m/next`). Cached next-ID changes only
after success. Reopen observes the entire old/new transaction. Counter exhaustion
rejects new identities before writes but preserves access to existing records.

`create` is destructive and only for a fresh, unpublished backend. It explicitly
formats and commits schema1 with next-ID1. `open` only mounts and checks exact
schema/counter lengths and values, never formatting or migrating on error.
Schema mismatch and corruption fail closed. Source/backend provisioning still
uses PageStore's separate exact format/config header.

### Value format and bounds

Episode values begin with record-version1, source1, subscription u64LE, episode
u64LE, audio tag u8, read flag0/1 and option flags u8. Bits0..4 indicate GUID,
description, publication time, enclosure length and duration. Unknown versions,
sources, flag bits, states, invalid UTF-8, lengths or trailing bytes are rejected.
Optional scalar fields follow in publication i64LE, enclosure u64LE, duration
u32LE order, then optional GUID, title, enclosure and optional description.
Every text uses a u16LE byte length. Numeric audio tags0..5 are explicit and
stable in the enum; changing this format requires a schema/version decision.

GUID256/title512/enclosure2048/description1024 byte caps keep even all optional
fields inside4096 bytes. Oversized required fields fail before writes and ID
allocation; an oversized optional description is omitted rather than truncated.
No unbounded string serialization or filename-derived identity is used.

### Local lifecycle and scans

Marking read changes only the read flag. Available audio stays Available.
Only Missing+unread state is admitted to Queued; Queued is already admitted.
Repeated identity imports do not create duplicate jobs. Unchanged metadata and
idempotent state updates also skip storage writes. Reclaimed read state
survives metadata updates/reopen and cannot automatically return to Queued.
Transition guards require read state for Available→Deleting→Reclaimed. These
are durable state transitions, not file deletion or an active-playback lease;
t-109 must still enforce space pressure and active-use exclusion before unlink.

A scan emits at most16 records and16KiB of encoded key/value payload, whichever
limit comes first, using one caller4096-byte buffer. The synchronous higher-ranked
callback cannot retain a reused metadata view or await under the read cursor.
A continuation is the last emitted local ID and survives reopen. One lookahead
record may be read to detect continuation; it is not emitted or skipped on the
next page. Scan work yields through the backend; real request timeouts remain
the service's responsibility, not a claimed elapsed-time bound of the row cap.

Storage, corruption, identity-index inconsistency, full-engine errors and
cancelled operation futures permanently disable that catalogue owner. Expected
input/state/not-found errors do not. Firmware must also disable its shared mount
on failed/cancelled storage requests using the existing global poison/RequestGuard;
the catalogue does not independently own or invalidate LittleFS.

## Verification and limits

Side-by-side t-107 regressions use real host files and sync_all for normal
create/update/state/reopen and schema checks. Tests cover identity scoping,
GUID-less fallback, duplicate import, changed enclosure/GUID policy, durable read
and reclaim state, field/UTF-8 bounds, optional metadata, malformed encoding,
index corruption, ID exhaustion, storage failure/cancellation and scan caps /
continuation. These establish catalogue behaviour, not filesystem/controller
power-loss immunity or physical performance.

`just test-catalogue-recovery` additionally injects170 sector-write cuts at
three persisted prefixes (510 cases), checking a near-limit value, its identity
index and next-ID counter recover together. This is a file-backed immediate-
durability model: writes are visible to reopen; sync is a model barrier, not
thousands of physical disk flushes. Normal persistence tests retain actual
sync_all. The initial all-flush sweep was stopped because host flush latency
made the model unnecessarily slow; no assertion or interruption point was
removed. SD cache loss/reordering and real power loss are not simulated.

`just check` and `just lint` explicitly include the standalone crate on Xtensa,
as it is not wired into the firmware yet. Host recipes include it as well.
The new record/index schema uses an existing SHA-2560.11 dependency version with
default features disabled; no new engine, filesystem or network dependency is
selected here. RSS, network activation, downloads, reclamation selection, actual
files and the storage scheduler remain subsequent stages of t-105.

Final verification: `just fmt`, `just check` (firmware and standalone catalogue
on Xtensa), `just test` (394 passing, six explicit long-sweep ignores),
`just test-catalogue-recovery` (510 model cases), `just lint`, `just audit`
and `just build` pass. A separate read-only review found no actionable issue.
Firmware networking/database initialization remains unchanged and no board
operation was performed.

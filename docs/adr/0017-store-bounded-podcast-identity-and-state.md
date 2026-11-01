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

## 2026-10-31: bounded Radio France reconciliation (t-108)

`RadioFranceFeed` consumes UTF-8 bytes incrementally and emits one owned,
size-bounded item at a time. It keeps no page or history buffer. Required
GUID/title/enclosure caps match the catalogue; descriptions beyond1024bytes
are omitted, including when the boundary falls within a Unicode scalar.
XML declarations, matching element names, scoped namespaces, predefined /
numeric references, comments and CDATA are checked. DTDs and custom/external
entities are rejected, with no resolver or external I/O. This is a bounded
RSS-specific XML subset: ASCII element/attribute names,16 nested elements,
16 namespace bindings,32 attributes per element,4096byte markup tokens and
128byte namespace URIs. Unsupported constructs fail synchronization rather
than asserting an incomplete import succeeded. Optional malformed dates or
durations become absent; valid RFC822-style dates normalize to Unix seconds.

Subscription URLs must use the measured HTTPS Aerion `/rss/{show UUID}` route.
Root requests are page0; each Atom next link must have the same show/origin
and the immediately following nonnegative page number. Namespace aliases and
scoped/default Atom declarations are resolved. At most4096pages are reconciled;
reaching the cap with a next link is an error, never a completed history.
Audio enclosure HTTPS URLs are independent of the subscription boundary;
TLS and redirect authority validation remain the transport's responsibility.

`SyncSession` admits one HTTP200 body at a time in chunks of at most4096bytes.
Each emitted item commits through the existing idempotent catalogue upsert:
a one-record batch is within both the16row and16KiB ceilings, and network
consumption pauses while storage commits. Previously imported episodes absent
from later responses remain untouched. A malformed, truncated, oversized,
failed or cancelled operation permanently poisons that session, so its caller
cannot bypass an error by continuing with another chunk or page. Committed
partial upserts survive and are replayable; the existing catalogue storage
failure/cancellation policy remains unchanged.

Only after every required page ends validly does `complete` commit a version1
checkpoint under `s` plus subscription ID big-endian8. Its bounded encoding
contains first-page ETag512 / Last-Modified128 byte strings, feed URL256,
page count and completion generation. The optional new key leaves existing
schema1 catalogue data readable without destructive migration. Missing
validators clear old validators after a successful unconditional import.
There is no complete marker on failed final commit; old checkpoints remain.

An unchanged first page cannot certify unchanged historical pages. Conditional
requests and HTTP304 are therefore allowed only for a previously completed
single-page history with a supplied validator. Paginated histories are always
reconciled unconditionally. A valid304 is a no-op, including storage writes.
The caller must request the session's URL/conditions and enforce transport
framing, TLS, deadlines and cancellation; firmware service wiring remains t-110.

The retained real134item/129identity fixture exercises full history, repeat
import and changed/new metadata against actual host-file EKV. That larger
scenario uses the explicit immediate-durability file model to avoid thousands
of physical host flushes; small checkpoint, failure, cancellation and reopen
tests retain actual sync_all. Neither model certifies card-controller power
loss, runtime memory/latency or end-to-end networking. Parser/session placement
and the bounded storage owner remain integration requirements.

`t-108` verification: `just fmt`, `just check`, `just test` (410passing,
six pre-existing explicit long-sweep ignores), `just lint`, `just audit` and
`just build` pass. Independent review findings for declaration/reference grammar
and namespace-aware pagination were corrected and covered by regressions.

## 2026-10-31: pressure-only audio lifecycle (t-109)

`MediaStore` is an ID-addressed interface for the future unique storage owner.
No remote path or filename crosses it. Each method must durably finish before
success and invalidate the entire shared mount after I/O failure/cancellation.
It exposes bounded4096byte appends and16ID file-list pages. Filesystem/radio
implementation and global request guards remain t-110, not a second mount.

Downloads restart, without Range/If-Range or retained representation validators.
A stale partial is discarded before space preflight so it cannot cause needless
reclamation of playable audio. Only Queued+unread+inactive jobs start; current
enclosure URL and optional length must match the job snapshot. HTTP200 without
Content-Range is required. Known lengths must agree;206, changed lengths, short
bodies and failed framing never publish. Before publication, current catalogue
URL/length are rechecked. Ordinary HTTP/body failures durably remove partials and
restore unread jobs to Queued in place; read jobs become Missing. Failed/cancelled
storage work stays poisoned for reboot recovery rather than retrying that mount.

Each known-length append preflights its actual payload; unknown-length appends
require a full4096byte budget. Free capacity must retain the greater of16MiB
and1% of the media region, rounded up. The filesystem adapter must report a
conservative usable capacity that accounts for allocation/metadata costs. A
chunk that cannot fit is not consumed and returns WaitingForSpace; the caller
retains it until a relevant state/space change or explicit retry. There is no
internal capacity polling timer. Waiting cannot be declared a completed body.

Successful transfer requires complete transport framing and the expected byte
count when known, then temporary-file synchronization with the actual length,
then durable filesystem publication, then a sorted atomic EKV commit of:

- `a` plus local EpisodeId big-endian8 → actual nonzero byte length u64LE;
- the existing episode record → Available, preserving its read flag.

The optional audio fact key leaves schema1 records readable. Moving an episode
to Missing or Reclaimed clears the fact atomically with the episode state.
A reset after file publication but before that commit leaves Downloading; recovery
discards that uncommitted representation and queues an unread restart. There is
no Available state preceding durable file publication.

`make_space` runs only for an actual pending download/append deficit. It scans
bounded catalogue pages, selecting read+Available+inactive candidates. Dated
episodes are oldest-first, followed by undated episodes in import order; IDs
break ties. This total ordering avoids a nontransitive comparison when one of
two timestamps is missing. It persists Deleting before durable unlink and only
then commits Reclaimed/clears the audio fact. It rechecks free space and stops
as soon as the deficit is met. Exhausted candidates return a wait; unread or
active audio is protected. The owner must forbid new active leases for Deleting
and Downloading states and serialize lease/state decisions with file operations.

Startup recovery walks both bounded catalogue pages and sorted local file IDs.
Valid Available files are checked against their persisted actual lengths and
kept, regardless of mutable RSS length metadata. Missing/truncated representations
are made non-Available; interrupted Downloading/Deleting operations are settled
idempotently. Read flags/tombstones survive. Unknown local IDs are orphan files
and are removed through the same ID-only interface. Active IDs are untouched.
No scheduled purge, filename inferred from URL or guessed identity is added.

Host tests use actual database and media files. Normal publication retains real
sync_all/directory barriers; fault sweeps use the explicit immediate-durability
file model and inject errors before/after every observed media step. Additional
cases cover DB failure after publication/unlink, append cancellation, short /
failed bodies, unknown-length reserve, in-place retry, metadata changes, oldest
selection, protected files and partial/orphan recovery. These are software
lifecycle guarantees, not physical card durability or firmware timing evidence.
Whole-history scan/recovery calls need cooperative service admission/deadlines
in t-110; bounded memory alone is not a measured shutdown/latency bound.

`t-109` verification: `just fmt`, `just check`, `just test` (427passing,
six pre-existing long-sweep ignores), `just lint` and `just build` pass.
Dependencies are unchanged since the passing t-108 audit. Separate review's
four Important findings have RED→GREEN regression coverage; service scheduling
remains an explicit t-110 integration obligation. No hardware was accessed.

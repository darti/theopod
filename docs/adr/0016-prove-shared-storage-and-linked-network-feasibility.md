# ADR-0016: Prove shared storage and linked network feasibility

- Status: Software feasibility verified; selected integration contracts, not firmware activation.
- Date: 2026-10-30
- Related: t-105, t-106; refines the remaining gate in ADR-0014/0015.

## Context and evidence

ADR-0015 corrected engine recovery and introduced checked region descriptors
and explicit allocation, but API-only network compilation did not establish
full link compatibility, memory placement, cooperative SD ownership or safe
initial descriptor publication. The approved t-106 stage requests disposable
experiments before the catalogue implementation; it does not require a physical
board connection. New pinned experiments in `docs/experiments/t106-runtime/`
address those software questions without changing firmware boot or dependencies.

The host experiment passes 12 tests using actual corrected EKV and actual
littlefs-rust main at the firmware's pinned revision. Two non-cloneable region
handles share one synchronous device on the storage core. Database sector/sync
operations yield after releasing the device borrow; a log operation can run
between them. I/O error or cancellation removes the shared device and rejects
later operations. A red/green regression corrected late database completion
after the media side failed. The real frontend must still retain the existing
cross-core poison flag, absolute request deadlines and RequestGuard checks.

Read-only discovery preserves legacy whole-card LittleFS. Explicit new-image
preparation initializes/syncs both stores and schema before publishing immutable
descriptors. All 625 preparation write/sync boundaries were faulted at three
persisted-prefix lengths (1875 cases); a discoverable new layout always reopened
with complete schema and log data. Cancellation was sampled at six of 1203
yield points. This is an ordered-sector model, not physical SD power-loss proof.
A future host provisioning tool must require an exclusively owned new image of
verified capacity and explicit destructive authorization; boot never provisions.

The complete firmware, radio, network runner, strict TLS/HTTP and shared-region
EKV candidate link together in an external copy of acca56f. Both database and
network futures are retained concurrently, rather than only sizing dummy types.
The additional 64 KiB reclaimed heap is separately placed at 0x3fcdb700; with
the existing ordinary 64 KiB heap it supplies 128 KiB candidate internal capacity.
Combined DRAM BSS is 150820 bytes; the remaining linker main-stack section is
120988 bytes (baseline 200512), and the main future pool is 23520 bytes (baseline
1632). The latter is already included in BSS. Aliased `.rwdata_dummy` must not
be counted again as independent physical RAM. No runtime stack/heap margin is
inferred from those reservations.

The controller configuration compiled with six static RX, eight dynamic RX,
four dynamic TX buffers, BA window three and two-entry RX/TX queues. Radio is
compiled at optimization level three, as its pinned package requires level two
or three. The rest of the firmware retains its existing size profile. Runtime
radio allocations and synchronous crypto latency still need device validation;
allocation failure must leave the product offline rather than bypass trust.

All six transport byte buffers are explicitly PSRAM allocated (41216 bytes).
PageStore's 4096-page buffer payload is 102400 bytes. Their known external
payload is 143616 bytes, separate from the existing 329728-byte framebuffer.
The older 49152-byte transport total includes internal objects: do not count it
as entirely PSRAM or add its objects again to the measured future/BSS.

The pinned host TLS/HTTP candidate also verifies a real audio CDN connection,
validates HTTP206/length/range for a bounded 1024-byte body, and rejects a wrong
root. Feed host trust uses independently published GTS Root R4; the observed
media.radiofrance-podcast.net chain uses independently published ISRG Root X1.
Root DER, hashes, provenance, limited chain summary and results are retained.
Peer-supplied certificates never become trust anchors. Every redirect still
needs authority validation and an explicit configured trust policy.

## Selected contracts and bounds

Use the actual EKV/PageStore adapter contract from t-106, with the reviewed
local correction: get, put, bounded ordered scan, durable sorted batch commit,
reopen and exact schema rejection. Catalogue code owns its database instance;
no database reference escapes to create a competing writer. Stable local IDs
and read state remain independent of mutable titles or URLs.

The shared-device prototype selects the integration shape: region-translated
SectorIo for LittleFS and an async SectorFile facade for PageStore on core 1.
A pre-I/O hook must restore the database request's own absolute deadline after
another task runs. Owned-sector requests still go to the single async HAL owner
on core 0. No HAL references or Rc handles cross cores. Production wiring,
fair queues and bounded sleep remain Task 5 rather than hidden prototype features.

Retain the existing timeout/reserve/retry proposals from the feasibility report,
with these concrete catalogue constraints for the next stage:

- Encoded values at most 4096 bytes, keys at most 64 bytes.
- UTF-8 byte limits: GUID256, title512, enclosure URL2048, optional description
  **1024**, with at most 256 bytes of encoding/identity/state overhead. The four
  fields total3840; the earlier description2048 proposal would exceed EKV's
  value limit. Oversized optional descriptions may be omitted; required identity,
  title and URL fields must be rejected rather than silently truncated.
- Batch at most16 operations **and**16 KiB encoded input; scan at most16 records
  **and**16 KiB output. These are simultaneous caps, not permission for16 full
  4 KiB values in a16 KiB buffer. The implementation must enforce both.
- One network transfer at a time, one bounded queued request, 4096-byte chunks,
  8192-byte/64-header HTTP limit and at mostfive validated HTTPS redirects.
- The runtime must explicitly allocate large buffers, bound temporary records
  and report allocation/capacity failure. Proposed parser/batch/scan buffers are
  implementation costs, not already measured within the linked prototype.

A 256 KiB additional PSRAM planning envelope leaves 118528 bytes beyond the two
known external payload groups for those bounded domain buffers and allocator
costs. This is an integration limit to verify as the services are built, not
an allocator currently reserved by firmware. Driver heap high-water usage,
stack margins, crypto scheduling, network/card throughput and power loss remain
hardware follow-ups requiring explicit user authorization.

## Gate interpretation

The t-106 **software compatibility gate** is satisfied: actual durable engine
recovery, shared-owner/publication model, exact candidate interfaces, full linked
placement and verified feed/audio TLS candidates have evidence. Host catalogue
and synchronization work may now proceed using the selected engine and bounded
contracts. t-106 remains open until merge, like all project tasks.

This does not authorize enabling a database on the existing card or claiming
runtime/hardware certification. The real single-owner service, trusted setup,
field/queue enforcement, failure handling and shutdown must be implemented and
checked before activation. Physical peak-memory/latency tests cannot be made a
silent prerequisite for the approved host-only feasibility stage: the plan
explicitly says no board connection is required. They remain a distinct,
explicitly authorized validation phase.

## Alternatives

Continuing to treat API-only compilation as readiness would omit demonstrated
link and publication questions. Conversely, keeping host domain work blocked
until unrequested physical tests would contradict the approved stage. The
selected boundary advances only software implementation, while preserving
runtime activation and physical validation requirements. No engine switch,
custom file index, live repartition, credential provisioning or hardware action
is performed by these experiments.

# ADR-0018: Integrate bounded podcast background services

- Status: Implemented in software; hardware validation pending. SD HAL ownership amended by ADR-0023.
- Date: 2026-11-01
- Related: t-110, t-105; implements ADR-0016/0017 contracts.

## Ownership and media

Core1 retains the unique filesystem/catalogue owner. Read-only discovery validates
both immutable descriptors against actual card capacity, translates disjoint raw
database/media regions, and opens the existing PageStore and catalogue schema.
Missing descriptors preserve the legacy whole-card LittleFS log mount. Neither
normal boot nor a failed database/schema check authorizes formatting or migration.
The old explicit `sd-provision` build remains a separate legacy-only operation.

The shared-sector adapter is promoted from the retained t-106 experiment, with
its host tests for disjoint ranges, cancellation, late replies, errors, yielding
and legacy log preservation. Its Rc device and filesystem handles stay on core1.
Core0 remains the only asynchronous SD HAL owner. Log and podcast file operations
use the same LittleFS mount; the catalogue owns one EKV writer.

A new host-only image tool uses create-new regular-file semantics. It initializes
both regions and the actual current catalogue schema, performs a real inode sync,
then writes and syncs each immutable descriptor. Existing files, devices and
symlinks are refused. It never copies an image onto a physical card. Image
preparation is not authorization to overwrite the user's existing media.

## Bounded commands, scheduling and shutdown

Owned domain commands cover follows, mark-read, paged subscription/job scans,
feed response/chunk/end, download response/chunk/end, abort and incremental mount
recovery. No database, filesystem, borrowed network buffer or HAL reference crosses
cores. A chunk is at most4096bytes. Mailboxes admit one queued request; a serialized
frontend checks an absolute2.5s client deadline (queueing plus the existing bounded I/O) and the existing cancellation poison
flag. Core1 retains a500ms absolute I/O budget per domain/log request and a2s mount
budget. A timeout or failed storage operation disables both regions until reboot.
Parser/protocol/input rejection is a domain error and leaves healthy logging usable.

The client budget covers two serialized callers, each with a fair log/data
admission (at most500ms each), plus scheduling slack. It is separate from the
unchanged500ms individual I/O and1.2s combined shutdown deadlines. Ordinary logger
flushes wake the fair log queue; only shutdown has control priority.

The production frontend uses the host-tested scheduler. Between bounded domain
requests, pending data and logs alternate; shutdown has priority. Recovery admits
one bounded catalogue scan, one identity repair, or one file-ID page per request.
Pressure reclamation within one download command remains covered by that command's
absolute budget; exhausting it disables the mount safely, rather than extending
an arbitrary library traversal. Capacity exhaustion detected before writes produces
WaitingForSpace, with no retry until relevant state change or explicit retry.

Sleep stops new domain admission, cancels/finishes active network work, then flushes
logs, all within the existing1.2s final deadline. Its final acknowledgement is
separate from ordinary log flush requests. Cancelled RPCs cannot return late data
to a new caller: the same shared-sector RequestGuard rejects the entire session.
Any final timeout disables the session and sleep continues.

## Transport and trusted setup

Use the exact selected embedded-tls/Embassy revisions and esp-radio configuration
from ADR-0016, now with real production sockets and streaming storage commands.
TLS buffers, HTTP headers and chunks are explicitly allocated from PSRAM with
fallible reservations. PageStore indexes/page buffers use ExternalMemory as in the
experiment; an additional4096-byte domain scratch reservation is fallible. The
ordinary64KiB internal heap is supplemented by the separately reclaimed64KiB region.

The first enabled link exceeded DRAM by64460bytes because the core1 task pool
included113336bytes of domain state. A small internal Embassy wrapper now owns
one fallibly PSRAM-allocated, pinned inner future; its task header and atomic
wake state remain internal. The actual boxed future after owned-command boxing
is109208bytes, capped at114000bytes before construction. Commands themselves are
fallibly boxed in PSRAM, one admitted request at a time, avoiding repeated4096-byte
enum moves on the executors' stacks. Allocation failure occurs before admission
and does not poison a healthy mount; cancellation after admission does.

The enabled link's generated owner poll frame is33392bytes, so the storage-core
stack is explicitly48KiB rather than the previous32KiB. This is a compiler frame
check, not a measured runtime high-water margin. Linked initialization writes
initial owner fields directly into its PSRAM allocation; it does not construct
an entire109208-byte temporary on that stack. The frontend poll frame reduced
from31184bytes to12608bytes after command boxing. Final section/pool measurements
are retained with the t-110 verification report.

Known external payload is143616bytes of network/PageStore buffers plus109208bytes
of owner future,4096bytes of scratch, and at most one4104-byte owned command
(261024bytes total). That nearly consumes ADR-0016's256KiB planning envelope before
allocator overhead and filesystem objects. Integration therefore reserves a512KiB
planning envelope for background services, separate from the329728-byte framebuffer;
all large explicit reservations remain fallible. This records the real implementation
cost instead of counting Rust future state as free. Runtime allocator/stack peaks
still require separately authorized board measurements.

No allocation failure authorizes weaker trust or storage initialization.

Credentials are parsed from ignored local TOML only with explicit `local-network`
opt-in. `THEO_WIFI_CONFIG` supports a synthetic compile fixture without touching
real setup. Release/default builds do not read or embed local credentials. The
USB receive-only command channel supplies trusted current UTC after every boot/
sleep, follows, mark-read, sync, retry and phase status. UTC advances by monotonic
uptime and expires after24h. It is never read from a card, build date, HTTP header
or unauthenticated time server. The crypto RNG is registered only while the RF
entropy source is active after a successful station connection.

Every connection verifies hostname, pinned publisher root, certificate dates,
chain and handshake signature, including an explicit in-verifier missing-time
rejection. Roots are the retained independently sourced GTSR4 and ISRGX1 DERs.
Only the selected Aerion feed/audio proxy and Radio France audio CDN authorities
are permitted. Redirects preserve feed identity/page, detect loops, strip
conditionals and are limited to five hops. Single-page sync uses persisted HTTP
conditionals; multi-page histories remain unconditional as required by ADR-0017.

DNS/TCP waits are10s, TLS20s, headers/per-read idle15s, all cancellable by shutdown.
A body also has an absolute5min feed-page or1h audio deadline. HTTP is capped at
8192header bytes/64headers. Ambiguous length/transfer coding, compression, unknown
unframed EOF, short fixed bodies and unfinished chunks cannot publish files or
checkpoints. reqwless0.14 fixed reads are explicitly clamped to remaining bytes.
Retries use30s/2min/10min/1h plus0–5s jitter; numeric Retry-After is bounded to24h.
Trust failure waits for corrected setup/explicit retry. Follow/reconnect/manual
sync and six awake hours request reconciliation; one worker coalesces triggers.

## Limits of software verification

Host tests exercise the real catalogue/PageStore files, real LittleFS adapter,
command lifecycle, parser/framing/redirect policy, scheduler, recovery, cancellation
and immutable image publication. The enabled firmware must also link using only
synthetic credentials so optimization cannot erase the selected network path.
No flash, serial monitor or physical media write is implied. Runtime heap/stack
high-water marks, synchronous crypto latency, SD throughput, full-card behavior
and physical power-loss durability remain separate hardware validation work.

## Final software verification (2026-11-01)

`just fmt`, `just check`, `just test` (464 passed, six explicit long-sweep
ignores), `just lint`, `just audit`, `just security-audit`, `just build` and
`just network-map /tmp/theopod-final-network-fixture.toml` pass. The latter uses
synthetic credentials and retains the complete network path. Both audit tools
use only the two explicitly authorized advisory exceptions described in
[podcast-security-assessment.md](../podcast-security-assessment.md).

The final enabled linker map reserves219452bytes of DRAM BSS and47988bytes in
the main linker stack section; the reclaimed64KiB heap is separate. Compiler
poll frames are33392bytes for the owner,12608bytes for the frontend and2432bytes
for the network task, before deeper calls. The owner has its explicit48KiB
stack. The task header/pool remains88bytes internally;109208bytes of pinned
owner state are allocated in PSRAM. Disassembly shows initial owner fields
written directly into the allocation, without a109208-byte stack temporary.
These are static placement/frame measurements, not runtime high-water margins.

Independent review identified and corrected queue starvation after a failed
first enclosure, ignored RetryLater completion, transient TLS disconnection
classification, and a duplicate network timeout shorter than the storage queue
budget. The production-used host worker tests exercise these paths through the
real catalogue and a simulated external transport. Stop drops pending work;
a single supplied deadline spans network acknowledgment and final flush. These
host proofs do not measure physical radio/SD timing. Separate review found no
remaining actionable issue in the scoped fixes, placement or image publication.

## 2026-11-11 amendment

ADR-0020 stores podcast audio in raw allocation units instead of the shared
LittleFS volume and narrows "any failed storage request disables both
regions": log and podcast failures now disable only their own scope, while bus
faults and caller-side cancellation still disable both. The 500 ms, 1.2 s and
2 s budgets are unchanged. The sections above are kept as written.

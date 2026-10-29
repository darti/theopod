# Radio France: local catalogue and offline episodes

Status: approved by the user on 2026-10-29; Radio France-only scope clarified.
Date: 2026-10-29.

## Intent and agreed behaviour

Theo Pod follows Radio France podcasts using the RSS feeds exposed by
[Aerion/rss-radio-france-pour-tous](https://github.com/Aerion/rss-radio-france-pour-tous).
Only Radio France podcasts are supported in this first version. Other feed
providers are future work; keep source-specific URL and parsing rules separate
from the catalogue and download lifecycle, without building a plugin framework.
Following a podcast imports its entire available history and queues all its
episodes for download to microSD. Subsequent synchronizations reconcile new
and changed metadata without duplicating episodes or downloading existing audio.
The local catalogue remains usable offline.

Marking an episode as read sets a flag permitting its audio to be recycled.
It stays visible and playable while the file exists. There is no periodic or
age-based purge. Only a download needing space triggers reclamation, oldest
eligible episodes first, and only until enough space has been recovered.
Persistent metadata and the read state survive reclamation so that automatic
synchronization does not download the same episode again. Unread episodes
are never automatically deleted. Downloads wait if reclaimable space runs out.

## Approaches

The user selected an embedded database on microSD, with audio stored in
separate files. The alternative was a custom file index: fewer initial
dependencies, but additional indexing, migration and recovery machinery to
maintain. The database implementation must support Rust no_std and the existing
async FAT storage; no SQLite or other std-dependent engine is assumed.
The user subsequently authorized selecting EKV and revising the SD transport.
EKV 1.0.0 now has a copy-on-write page adapter and async FAT file transport
(ADR-0011); host persistence and Xtensa compatibility checks cover this layer.
Integration into the firmware storage service is still required.
If no suitable engine integrates, return with the concrete tradeoffs rather
than silently replacing the approved database approach with a custom index.

## Existing system and constraints

The firmware has no network stack or podcast/audio service today. Storage is
FAT32 over SPI3, owned by the asynchronous logging task (ADR-0010). General
storage operations must extend this single owner through bounded requests;
they must not create a second mount or contend unsafely with logging.
Cancellation or storage failure invalidates the mount until reboot, as today.
The UI loop must remain within the main watchdog budget (ADR-0009).
Network, parsing and storage work run in background tasks with bounded waits
and memory, backpressure, and control paths for sleep and cancellation.
Board-specific initialization remains in src/device; domain logic is host-testable.

## Proposed implementation decisions

These details are proposals by the implementer, not additional user requirements.

- Store subscriptions (stable local ID, feed URL, enabled state and HTTP
  validators), episodes (subscription ID, source identity, title, publication
  date, enclosure URL, optional length and duration), read/reclaimable state,
  audio availability, download progress, and synchronization state separately.
- Identify episodes by subscription plus RSS GUID. Where GUID is absent, use
  a persisted enclosure-URL identity. Metadata changes never reset local states.
  A changed GUID/URL cannot safely be guessed to be the same episode.
- Support configured Radio France feed URLs first. Discovery/search, playback decoding,
  audio wiring and a full player UI are separate work; provide domain commands
  to follow feeds and mark episodes read for later UI integration.
- Follow imports the full history exposed by the selected feed. Fetch and parse
  incrementally with explicit resource limits; detect truncated or invalid input
  instead of silently declaring synchronization complete.
- Use conditional HTTP requests when the server supplies ETag or Last-Modified.
  RSS may require fetching a complete feed: delta means local reconciliation,
  not an assumed remote delta API. Absence from a later feed never deletes
  cached history, local state or files.
- Commit validators only after successful reconciliation. Retries are
  idempotent; interrupted imports can replay without duplicates.
- Stream audio into temporary files. Publish availability only after successful
  completion and durable storage. Resume only when HTTP range support and a
  stable representation validator make it safe; otherwise restart that partial.
- Reconcile partial files and database/file disagreements at startup. The
  database must document crash guarantees; FAT32 is not assumed to provide
  atomic database-plus-file transactions or immunity to power-loss corruption.
- Reclaim downloaded, read episodes by publication date, with local import
  order as a stable fallback/tie breaker. Exclude files actively being played
  or written. Keep a durable deletion intent, remove the file, then mark its
  audio absent; recovery finishes interrupted operations idempotently.
- For unknown enclosure lengths, download in bounded chunks and reclaim as
  needed. Keep room for metadata/log updates. Pause rather than repeatedly
  retrying a full card; resume on space/state changes or an explicit retry.
- Retain read tombstones even when audio is absent. Future explicit re-download
  and unsubscribe retention policies are outside this first scope.

## Network and failure handling

Add Wi-Fi, DNS, HTTPS with certificate validation and a trusted time strategy.
Do not commit credentials or embed them in diagnostic logs. The implementation
plan must define credential provisioning, trust storage, supported redirects
and maximum response/field sizes before the network integration is built.
Offline operation, malformed RSS, unavailable servers and missing/full SD cards
must leave the UI responsive and preserve committed catalogue entries.
The existing logging shutdown handshake must include pending database/file
writes without waiting for full downloads. Hardware tests require an explicit
user request; no flash or monitor operation is authorized by this design.

## Acceptance and validation

Host fixtures cover complete-history import, repeat synchronization, added and
changed episodes, missing GUIDs, truncated feeds, persistent read state,
reclamation order, no unread deletion, full-card waiting, and unknown lengths.
Fault injection covers interrupted imports/downloads/deletions and recovery,
including database commits failing between file operations. A read episode
remains playable until reclamation and is never automatically requeued after it.

Run just fmt, just check, just test, just lint and just build for implementation;
run just audit for dependency changes, and just miri if driver unsafe code changes.
Record the selected database and storage/network architecture in docs/adr.
Hardware throughput, Wi-Fi coexistence and power-loss durability need separate
board validation after explicit authorization.

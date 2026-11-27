# ADR-0029: Resume interrupted audio downloads within a boot

- Status: Accepted
- Date: 2026-11-27
- Related: t-156 (parent t-153); supersedes the "Downloads restart, without
  Range/If-Range…" paragraph of ADR-0017 (section "2026-10-31:
  pressure-only audio lifecycle")

## Context

Episodes are about 75 MiB and download at about 84 KB/s: some 15 minutes
each. On the board, connection resets and 15 s body stalls happen during
that time, and ADR-0017 restarts every failed download from byte 0, so a
failure late in an episode costs the whole transfer again. The audio host
(`media.radiofrance-podcast.net`) answers `accept-ranges: bytes` with a
strong `ETag`.

## Decision

- On an ordinary network failure (not storage, not stop) in the body of an
  audio download, after at least one byte was written, the temporary audio
  is kept, the episode goes back to `Queued` (one commit), and
  `theo_podcasts::resume::Resume` remembers in RAM the job (local id,
  SHA-256 of the enclosure, announced length), the bytes written N, the
  total length, and the validator of the whole response: its strong ETag,
  else, when it has no ETag at all, its `Last-Modified`. A weak ETag, no
  validator, an unknown total, a failure before the first byte, a body
  already complete, or an episode marked read meanwhile keeps nothing: the
  partial is recovered as before. A resumed attempt that fails before its
  response headers (reconnect, TLS, `429`, `404`, `416`, a `5xx`) keeps
  the partial as it was but counts as a resume: offered ahead of the queue,
  a range the host keeps refusing would otherwise starve every other
  download.
- The next `NextJob` offers that job first, whatever the scan cursor, and
  `DownloadSession::start` resumes it only if it is still `Queued`, unread,
  not leased, with the same enclosure and length, its temp still N bytes
  long, and fewer than 5 resumes made. Otherwise the kept partial is
  discarded through `recover_one` (one commit), and a later start begins
  from 0.
- A resumed request sends `Range: bytes=N-` and `If-Range: <validator>`
  (`Command::DownloadRange`), on every redirect hop: a redirect ignores
  them, and If-Range makes the final host answer in full unless its
  representation still matches.
- A `206` must carry `Content-Range: bytes N-(total-1)/total` with the same
  N and total, a matching `Content-Length` if any, and, when it carries a
  validator, the kept one (a host that ignored If-Range would otherwise
  splice another representation): the body then appends to the kept bytes. A `200` restarts from 0 in the same response, its
  length bound only by the feed's: the first chunk discards the partial
  (`DownloadSession::restart`, one commit, `Reply::Pending`) and is sent
  again. A non-fatal refusal of that reset (no space) recovers the episode
  to `Queued` rather than leaving it `Downloading` until the next boot.
  Anything else, or any other `Content-Range`, aborts and
  discards the partial, as before.
- A reboot forgets `Resume`, so boot recovery still discards every partial:
  resume is same-boot only, and nothing new is persisted.
- `MediaStore::temp_len` reports the current temp's length from RAM; the
  unit store keeps the temp's extents and unsynced tail sector across the
  pause, so appends continue where they stopped.

## Consequences

- A failure late in an episode costs the remaining bytes, at most five times
  per job; a sixth failure restarts from 0, so a server that keeps cutting
  the body or refuse the range cannot pin a partial forever. A restart from
  0 resets the count: a new representation earns its own five resumes.
- A kept partial holds its reserved units until it resumes or is discarded;
  only one is kept at a time (the unit store has a single temp anyway).
- A `Last-Modified` date is used without checking it against the response
  `Date` (RFC 9110 13.1.5 asks for a strong date); the exact
  `Content-Range` and total-length checks bound the risk, and the Radio
  France host sends a strong ETag.
- The validator of a redirected resource is sent to every hop of the
  enclosure's redirect chain: it is a public file's tag, not a credential.
- The request headers come from `network_policy::request_headers`, a pure
  host-tested function; a range is never sent without a validator.

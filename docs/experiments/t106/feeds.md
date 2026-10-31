# t-106 — Radio France live feed feasibility evidence

Measured 2026-10-29 17:29–17:31 UTC. Research only; no repository edits, firmware changes, flash, or commit. Parent authorized task t-106 remains open.

## Primary provenance and changed contract

Pinned old source: https://github.com/Aerion/rss-radio-france-pour-tous/tree/12037d21d9465885efa9eeab951045898ca178af . Files worker-script.js, src_api.js, src_feed.js, src_config.js captured under this directory. Old API supported `page=-1` aggregate, worker `/audio/{manifestationId}` 302, and page-next links.

GitHub main response `github-head.body` identifies current SHA `95e519deab04037c002ef22563e7c56d3bcb51bc`. Current implementation is Go; current-tree.json records its tree. Initial attempts at old JS paths at current SHA produced captured `404: Not Found`, not implementation files. Authoritative current sources:
- https://github.com/Aerion/rss-radio-france-pour-tous/blob/95e519deab04037c002ef22563e7c56d3bcb51bc/internal/httpapi/handlers.go
- https://github.com/Aerion/rss-radio-france-pour-tous/blob/95e519deab04037c002ef22563e7c56d3bcb51bc/internal/radiofrance/shows.go
- https://github.com/Aerion/rss-radio-france-pour-tous/blob/95e519deab04037c002ef22563e7c56d3bcb51bc/internal/feed/build.go

Current `GetShowDiffusions` clamps negative page to zero. Live `?page=-1` body was identical to page0, with a next-page link; it is NOT complete history. Follow atom:link rel=next until absent. Current builder resolves CDN URLs directly when possible, falls back to legacy `/audio/`, includes iTunes duration when available, omits enclosure length. GUID cutoff is UTC 2022-09-12 00:00:00 inclusive: manifestation ID on/before cutoff, diffusion ID after. pubDate uses CreatedTime in UTC; future-scheduled items are skipped. Duration/image/description enrichment and cached responses mean metadata can change. Do not equate GUID to a current audio representation.

## Actual live capture

Search URL https://radio-france-rss.aerion.workers.dev/search?query=espions returned301 to `/search/?query=espions`, then200 JSON (1680 bytes). Evidence search.headers/search.body. This disproves assuming the prior execution's502/530 is permanent. Service reachable from escalated curl here; sandbox DNS itself failed first.

Selected `Espions, une histoire vraie`, show4a41823f-f1f7-4725-8380-e428893eb93b:
- https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b → feed-page0.xml.gz (gzip),100 items,309381 bytes, next page1.
- https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b?page=1 → feed-page1.xml.gz (gzip),34 items, terminal (no next).
- https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b?page=-1 → same bytes/hash as page0 (redundant body not retained).

The two pages form the complete available history EXPOSED BY THIS CAPTURE, not independent proof Radio France has no other episodes.134 emitted items,129 distinct GUIDs, zero absent GUIDs; five duplicate GUID pairs occur within page1 (duplicates.json gives zero-based concatenated item indices). Every enclosure points directly to media.radiofrance-podcast.net;134/134 omit length;134/134 have duration. First item2026-08-21 18:09:20 UTC; last item2020-06-27 11:17:40 UTC.

RSS GET responses are200 application/xml; charset=utf-8. No ETag, Last-Modified, or Content-Length in observed feed headers. This only describes these responses; absence is not a universal guarantee. Incremental reconciliation cannot depend on feed validators.

feed-items.xml contains six actual extracted items (concatenated indices0,99,100,105,111,133), including a genuine repeated historical GUID and both ends/page boundary. It is DERIVED, reserialized XML, not an original server response. feed-page0.xml/feed-page1.xml are untouched originals. The earlier four-item extraction is not retained.

## Audio transport measurements

Exact tested direct URL is in audio.url (also first enclosure page0). RSS declares audio/mpeg, URL ends.m4a, actual CDN Content-Type audio/mp4: decoder/container selection must not blindly trust RSS type. HEAD audio-head.headers returned200, Content-Length56678816, Accept-Ranges bytes, ETag `"820876596acd1d5f907c62f322010c6a:1787677707.892406"`, Last-Modified Tue25 Aug2026 17:08:27 GMT. Direct URL had no redirect in this request.

GET Range bytes=0-1023 returned206, Content-Range bytes0-1023/56678816, Content-Length1024 (audio-range.headers/.bin). GET Range bytes=1024-2047 with If-Range equal to that strong ETag returned206 and bytes1024-2047/56678816 (audio-if-range.headers/.bin). This is positive measured support for validator-guarded resumption for this representation; other CDN hosts/expired URLs/mismatched validators still require safe restart handling. Full audio was not downloaded.

Legacy https://rss-rf.aerion.me/audio/6dccf76b-a3e5-4ffc-8943-42805c084cb0 (old terminal-item GUID, which by source cutoff is manifestation identity) returned502 twice, no302. Evidence legacy-audio-range.headers and legacy-audio-retry.headers. Therefore the source's legacy302 route is inspected but its live successful redirect chain remains unverified. Do not infer all enclosures fail: current feed's direct CDN ranges succeeded.

## Integration implications and limits

Use current observed paginated contract, idempotent GUID upsert, and terminal-link detection. Duplicate GUIDs are present in a real response and must preserve local read/download state. Missing length is routine; streaming/reclamation requires unknown-length handling. A single300KiB feed page should be parsed incrementally with bounds, not buffered whole. Snapshot pagination is not guaranteed; concurrent upstream changes can shift boundaries, so retry/reconciliation must tolerate duplicates and avoid deleting absent entries.

Remaining: successful legacy redirect chain; more shows/CDN variants; expiry and changed validator behaviour; ESP TLS/redirect/range and SD integration (outside this independent feed experiment); hardware throughput not tested. No credentials or private tokens recorded in report; downloaded pinned public source itself contains its upstream public application header and must not be indiscriminately copied into application configuration/logs.

## Artifact integrity

SHA256SUMS records all captured artifacts plus this report. Raw HTTP headers preserve Date/status and redirect chain where any occurred. Commands used curl-L for feeds/search, curl-I-L for audio HEAD, curl-L--range for audio slices; exact URLs above or in audio.url/legacy-audio.url. No synthetic feed is presented as live capture.

# Radio France history fixtures (t-108)

`radio-france-page0.xml` and `radio-france-page1.xml` are byte-for-byte
UTF-8 decompressions of the retained t-106 responses in
`docs/experiments/t106/feed-page{0,1}.xml.gz`. No network access is needed
for tests. They contain 100 + 34 items and 129 unique GUIDs, including
five duplicate identities and optional descriptions above the catalogue cap.

Captured 2026-10-29 17:29–17:31 UTC from:

- https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b
- https://rss-rf.aerion.me/rss/4a41823f-f1f7-4725-8380-e428893eb93b?page=1

Source: Aerion/rss-radio-france-pour-tous, Go commit
`95e519deab04037c002ef22563e7c56d3bcb51bc`, `internal/feed/build.go`.
See `docs/radio-france-feasibility.md` for the captured HTTP evidence.

Decompressed SHA-256:

- page0: `7fdfc9f83c4457ab015f81a1786c4d755b87e0ea573999f1f490e59dad0bd413`
- page1: `2e0b489a819c06cbe15e8096ac5fd4171f3a76d9444c8f02e40de45a1bfac60b`

Smaller synthetic mutation, malformed XML, CDATA and fault inputs are constructed
in the side-by-side test modules; they are not representations of live feeds.

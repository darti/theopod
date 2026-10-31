# Theo local EKV correction (t-116)

Base: published ekv 1.0.0, upstream commit e9b3ff36d29ada21dba4f440a214305b7d965abc.
Licenses: MIT OR Apache-2.0; original files and licenses retained.

Only src/file.rs differs from the registry source:
- Commit replacement metadata records before publishing their higher-sequence header.
- Propagate rollover metadata flash errors rather than unwrap/panic.

Exact patch and red/green evidence: docs/experiments/t116/.
Retained project regressions: crates/theo-storage/src/page_store_rollover_tests.rs
and page_store_history_tests.rs. Run just test-storage-recovery for exhaustive
release-mode injection; normal just test includes the short commit regression.

The local patch changes neither format nor API. Remove the override only after
an upstream version carrying the correction passes the same recovery tests.
This is not an upstream-maintained fork or a claim of physical SD power-loss safety.

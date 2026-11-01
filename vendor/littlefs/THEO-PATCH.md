# Temporary LittleFS source correction (t-110)

The user explicitly approved this temporary adoption on2026-10-31.

- Upstream baseline:732d038b684803edb713a0f87d00b6878674276a.
- Exact retained source:e51ca62cd33dbd9564d07f718214cae842f0d22f.
- Reviewable upstream PR:https://github.com/PhotomancerArt/littlefs-rust/pull/12.
- Fork:https://github.com/darti/littlefs-rust/tree/e51ca62cd33dbd9564d07f718214cae842f0d22f.

Both crate directories are byte-for-byte copies of that commit, including
upstream tests and license notices. Root workspace metadata is reduced to
these two crates; C compatibility tooling/docs are not dependencies and are
not vendored. Versions remain0.1.1 and public safe API/on-disk format do not
change. The low-level lfs_dir_fetch pair argument becomes a raw pointer;
Theo uses the safe wrapper only.

The correction preserves raw pointer provenance across the existing C-style
call graph, separates retained configuration/storage allocations and stabilizes
open handles under the filesystem's RefCell operation serialization. It is not
a complete safety audit or physical card durability certification.

Remove the patch and this directory when upstream main contains this correction
(or its reviewed equivalent), then update the lockfile and re-run the same
host/ESP checks, Miri safe-API regressions and relevant recovery tests. Do not
silently follow the fork's moving main or update this snapshot in place.

## Local adoption verification (2026-11-01)

A fresh checkout of the recorded correction resolved to exactly
`e51ca62cd33dbd9564d07f718214cae842f0d22f`. Recursive comparisons of both
retained crate directories found no differences. The retained workspace's
host tests and doctests pass (1588 tests). Its two `miri_aliasing` safe-API
regressions pass under both Miri Stacked Borrows and Tree Borrows. These
checks cover the retained source; firmware integration is verified separately.

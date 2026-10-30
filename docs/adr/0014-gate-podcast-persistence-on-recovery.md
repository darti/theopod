# ADR-0014: Gate podcast persistence on interruption recovery

- Status: Recovery defect corrected by ADR-0015; remaining t-106 integration/resource gate still applies.
- Date: 2026-10-29
- Related: t-105, t-106, t-116; ADR-0011 and ADR-0012

## Context

Read flags must survive audio reclamation so that synchronization does not
redownload read episodes. EKV 1.0.0's PageStore adapter protects individual
logical writes with sector copy-on-write, but this does not establish safe
publication of an engine transaction or metadata page.

The t-106 rollover reproducer, using the actual PageStore, mounts after an
interruption between complete metadata writes and then returns KeyNotFound for
previously committed keys. Ordinary commit and real-file sector-cut tests pass;
the longer rollover history fails. This is an observed recovery defect, not a
physical SD power-loss experiment. Reproduction and exact versions are retained
in [the feasibility evidence](../experiments/t106/README.md).

The live Aerion source has also changed to Go. Complete history now requires
following next-page links; negative page numbers select page zero. Live feeds
and verified host HTTPS are available, and candidate APIs compile for Xtensa.
They do not remove the database recovery requirement.

## Decision

Stop dependent persistence implementation in t-107–t-110 until t-116 corrects
or replaces the selected database path and the positive gate is rerun. Keep
podcast DB initialization disabled. Preserve the failed experiment, the measured
limits, live fixtures and tested transport interfaces in the feasibility report.
Do not characterize EKV 1.0.0 as interruption-safe because PageStore tests pass.

Do not place a large random-access database file on LittleFS or reserve raw
sectors inside the currently mounted whole-card filesystem. A separate checked
region is a future candidate only, requiring explicit layout/provisioning work.
The single core-1 owner and core-0 async sector service from ADR-0012 remain the
storage architecture; no runtime filesystem or card layout changes here.

No alternative engine is selected by this ADR. Prefer validating an upstream
EKV correction first; alternatives include redb with a proved bounded synchronous
backend on the isolated owner, or transaction-level publication with quantified
space/I/O cost. Any choice must preserve the approved database approach and
no_std constraints, pass reopen/schema/commit/rollover interruption tests, compile
with current firmware dependencies, and establish a combined RAM/latency budget.

## Consequences

The feasibility stage has a useful negative outcome rather than a false
production readiness claim. Podcasts remain unimplemented; existing LittleFS
logging remains as deployed. No card was reformatted or flashed during t-106.
A working TLS spike is retained, including fail-closed clock handling, without
introducing network dependencies into firmware before persistence is sound.

## Recovery follow-up

ADR-0015 records the reviewed local EKV correction, broader model regressions,
checked region descriptors and explicit allocator support. These supersede the
engine-recovery stop reason above. Runtime provisioning, shared-owner region I/O
and combined memory/latency validation remain open; they are not implied by
correcting the engine.

## 2026-10-30 software feasibility follow-up

ADR-0016 now verifies the remaining software compatibility questions through
pinned disposable shared-owner/publication tests, a full linked candidate and
verified audio TLS. Host catalogue implementation may proceed. The historical
stop reasons above do not require an unrequested board connection before host
work; production activation and physical validation remain separate requirements.

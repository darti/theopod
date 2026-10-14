# ADR-0007: Keep regression tests inline, tagged by task

- Status: Accepted
- Date: 2026-10-14

## Context

Guideline G16 asks for one regression test per bug fix in
`tests/regressions/issue_<N>_<description>.rs`. This project's regression
tests sit in the side-by-side `*_tests.rs` files of `theo-drivers` and
`theo-frame-dump`, marked only by a `// Regression` comment, and most of
them do not say which bug they pin (t-7).

Integration tests in `tests/` only see a crate's public API. The tests
that pin past bugs drive private state and the crate-private fakes of
`test_support/` (a recording QSPI bus, async I2C fakes per chip, pending
delays and buses to cancel futures mid-way). Moving them out would mean
publishing those fakes and internals from `no_std` driver crates, or
duplicating them. The work tracker numbers tasks (`t-N`), not issues, and
the oldest fixes predate it.

## Decision

- Regression tests stay in the side-by-side `*_tests.rs` file of the code
  they pin, next to the tests of the same unit.
- Each one starts with a comment naming its origin and what broke:
  `// Regression (t-N): <what went wrong>`. A test that guards an
  invariant whose violation is known to break the hardware, rather than a
  fixed bug, says `// Regression guard (t-N): ...`.
- A fix that predates the tracker names its commit instead:
  `// Regression (f954dfb): ...`.
- `grep -rn "// Regression" crates tools` lists them all.

## Consequences

- Positive: regression tests keep using the crate-private fakes; nothing
  is made public for tests' sake.
- Positive: every regression test points to the task (and its review
  notes) or commit that explains it.
- Negative: they are not grouped in one directory, and the tag is a
  convention that only review enforces.

## Alternatives considered

- `tests/regressions/` per crate: rejected, it needs a public test-support
  API in `no_std` crates and splits a unit's tests across two places.
- A `regressions` module per crate under `#[cfg(test)]`: keeps private
  access but still separates a unit's tests from each other, for little
  gain over a greppable tag.

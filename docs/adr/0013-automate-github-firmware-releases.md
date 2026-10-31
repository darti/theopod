# ADR-0013: Automate GitHub firmware releases

Status: Accepted (t-111)

## Context

Theo Pod ships firmware rather than crates.io packages. Version changes and
release notes should be reviewable and security advisories should block releases.

## Decision

Use release-plz in git-only mode for the root `theo-pod` package, with `vX.Y.Z`
tags and a single `CHANGELOG.md`. Include all workspace components in its notes.
Synchronize all workspace package versions with a `firmware` version group.
Process all packages to detect component-only changes, but enable GitHub release
creation and changelog writing only for the root package. Original packages use
the root tag as their comparison baseline. Disable library semver checking for
this embedded firmware distribution.

Packages introduced after the first firmware release use package-specific tags
with tag creation enabled, while retaining the `firmware` version group and
inclusion in the root changelog. For `theo-podcasts`, use
`theo-podcasts-v{{ version }}` (t-117). In release-plz 0.4.1, assigning it the
existing `v0.1.0` baseline fails because that historical workspace has no such
package. Its own tag namespace allows an initial release without that baseline
and records valid baselines for later releases. These extra tags do not create
additional GitHub releases.

Validate Conventional Commit PR titles and use squash merges. Share `cliff.toml`
between local git-cliff previews and release-plz's automatic changelog generation.
Only merging a release PR permits publication (`release_always = false`).

Run a reusable cargo audit job before release-plz, plus independent PR, main and
weekly audits. No registry credentials are used. Use a fine-grained token for
release PR creation when automatic PR checks are required; the default Actions
token remains sufficient for GitHub release creation.

## Consequences

Maintainers review generated versions and notes before merging. Initial releases
use the root manifest version when no matching tag exists. CI does not need the
ESP compiler for release metadata or security auditing. These workflows create
tags and release notes; firmware binary attachment is outside this decision.

## References

- https://release-plz.dev/docs/config
- https://release-plz.dev/docs/github/token
- https://git-cliff.org/docs/configuration/git/

# GitHub firmware releases (t-111)

## Intent and scope

Automate Conventional Commit validation, version proposals and release notes for
Theo Pod. Publish only on GitHub; never publish crates on crates.io or flash a
board. Preserve the ESP firmware and the existing dependency versions.

## Approaches

A single firmware release with tags `vX.Y.Z` matches this product. Independent
crate releases would add versions and release pages without a distribution
need. Hand-written tags and notes would retain the manual work being replaced.
The proposed approach is a single GitHub release managed by release-plz.

## Design

- release-plz uses git-only mode and publishes only the root firmware package.
  Synchronize workspace versions and include component changes in the firmware
  changelog; process components without producing separate tags or releases.
- A push to `main` updates a release PR. Merging that PR creates the version tag
  and GitHub release. The changelog generated using `cliff.toml` supplies notes.
- Validate PR titles as Conventional Commits; document squash merging so the
  validated title becomes the commit used for versioning.
- Run cargo audit on PRs, pushes and a schedule; gate release publication on a
  fresh audit. Keep the existing cargo-deny audit available.
- Use minimal job permissions. Document Actions PR permissions and token setup,
  including the limitation that PRs created with GITHUB_TOKEN do not trigger CI.

## Validation

Parse configuration with the actual tools, render a changelog preview and lint
GitHub workflows. Exercise valid and invalid Conventional Commit titles. Run
the repository's format, compilation, tests, lint and release build checks.
Do not publish a release during local verification. Keep t-111 open until merge.

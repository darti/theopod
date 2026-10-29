# GitHub Releases Implementation Plan

> Execute inline using superpowers:executing-plans. User approved the design
> and implementation on 2026-10-29. Oku parent: t-111; keep all tasks open until merge.

**Goal:** GitHub-only firmware releases with automatic notes and security checks.
**Architecture:** One root release, synchronized workspace versions, git-only
release-plz and shared cliff.toml.
**Tech Stack:** GitHub Actions, release-plz 0.4.1, git-cliff, cargo audit.
**Spec:** ../specs/2026-10-29-github-releases-design.md

## Constraints and review focus

Preserve existing changes and firmware dependencies. No hardware operations or
registry publication. Check fork PR safety, bot PR CI triggers, fresh audit
before releases, main-only publication, and inclusion of workspace changes.

## Task 1: Configure Conventional Commits and changelogs (t-112)

- [x] Add cliff.toml and a PR title check with no untrusted shell interpolation.
- [x] Document squash merging, commit types and local changelog preview.
- [x] Render actual git history and exercise valid/invalid titles.

## Task 2: Automate audited GitHub releases (t-113)

- [x] Add release-plz.toml with root-only publication, synchronized workspace
      versions, git_only and release_always=false.
- [x] Add reusable cargo audit workflow and main release workflow with a gate.
- [x] Document token setup and release process; record the decision in an ADR.
- [x] Lint workflows, check release-plz in a disposable local repository and run
      just fmt, just check, just test, just lint and just build.
- [x] Review the resulting diff and record evidence without closing Oku tasks.

## Rulings

Implement in the current feature checkout, modifying only release configuration
and focused documentation sections; unrelated work is already in progress here.
Use PR titles and squash merges as the Conventional Commit enforcement boundary.
No Rust runtime change requires additional unit tests.

Processing disabled children loses component-only changes with release-plz 0.4.1.
Process the whole workspace in one version group, sharing the root tag baseline;
only root tag/release/changelog creation is enabled. Verified driver-only and
tool-only changes each produce the next root patch and correct notes.

Cargo audit initially found RUSTSEC-2026-0414 in pulldown-cmark 0.13.4.
Update only its lockfile entry to compatible 0.13.5, without suppressing advisories.

## Verification evidence

- actionlint: all three workflows valid.
- git-cliff: actual repository history renders successfully.
- PR matcher: accepts ordinary, scoped, breaking and release titles; rejects
  malformed and multiline input.
- release-plz 0.4.1 disposable repository: initial 0.1.0, driver-only 0.1.1,
  tool-only 0.1.2, root-only 0.1.3, storage-only 0.1.4. All package versions
  synchronize and notes do not repeat previous releases.
- just fmt, check, test, lint and build: pass.
- just security-audit: pass after pulldown-cmark correction; existing warnings
  remain visible. just audit: advisories, licenses and sources pass.
- Fresh reviewer identified the disabled-child issue, resolved and verified above.
- GitHub settings and PAT secret remain maintainer setup documented in README.
- No remote release, hardware operation, push or merge performed. t-111, t-112
  and t-113 remain open until merge.

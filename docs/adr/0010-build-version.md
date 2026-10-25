# ADR-0010: Build version

- **Status:** Accepted
- **Date:** 2026-10-24

## Context

Shomsky Linux had no single product build version. `Cargo.toml` crate versions,
`/etc/os-release`, Docker image tags (`dev`, `ci`, GHCR semver), and the
optional `SHOMSKY_VERSION` env var could each say something different. `:sys`
printed `version=` only when that env var was set, so a live dump often stopped
at `context=` with no version. The splash footer had none.

Operators and reviewers need one number that moves on every merge to `main`,
shows on the splash and in `:sys`, and is the GHCR tag when we publish.

Related: [ADR-0005](0005-ci-integration-steering.md),
[docs/build-and-publish.md](../build-and-publish.md).

## Decision

The product **build version** is the single line in repo-root [`VERSION`](../../VERSION):
`MAJOR.MINOR.PATCH`, no `v` prefix.

- Crate versions in `Cargo.toml` stay independent. This is the distro build
  number, not a workspace crate bump.
- [`rootfs/etc/os-release`](../../rootfs/etc/os-release) `VERSION_ID` and
  `PRETTY_NAME="Shomsky Linux X.Y.Z"` must equal `VERSION` in the same change.
- Every pull request into `main` must make `VERSION` strictly greater than
  `VERSION` on `main`. Patch `+1` is the default; minor or major is the author's
  choice when the change warrants it. CI enforces "greater than", not which
  component moved.
- If `main` has no `VERSION` yet, a valid semver is enough for that first PR.
- Release tags stay `v` + `VERSION` (existing `v*` publish in ADR-0005). This
  ADR does not publish from every merge.
- `engram-shell` compiles `VERSION` in (`include_str!`). The splash footer
  prints `Build:X.Y.Z`. `:sys` / `uname` always print `version=X.Y.Z`.
  `SHOMSKY_VERSION`, when set and non-empty, overrides the compiled value.

## Steering

When changing identity, splash, publish, or CI:

1. Bump `VERSION` in the same PR that lands on `main`. Use
   `./scripts/set-version.sh` (suggests patch `+1`; type another `X.Y.Z` to
   override) so `rootfs/etc/os-release` stays in lockstep.
2. Keep `rootfs/etc/os-release` in lockstep; do not edit one without the other.
3. Do not invent a next GHCR tag from registry/git history. Publish the
   contents of `VERSION`.
4. Do not default `SHOMSKY_VERSION` in the image to `dev` / `ci`; those tags
   must not mask the compiled number.

## Consequences

- Splash and `:sys` always show a build version.
- PRs that forget to bump fail a cheap CI job before the image build.
- Manual GHCR publish (`scripts/update-container-registry.sh`) pushes
  `VERSION` and refuses if that tag is already published.

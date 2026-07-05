---
type: Runbook
title: Cutting a release
description: Version bump, changelog, tag, release workflow, and post-release verification.
tags: [release, versioning, ghcr]
timestamp: 2026-07-03T00:00:00Z
---

# Cutting a release

`.github/workflows/release.yml` builds a multi-arch (amd64+arm64) image,
pushes it to `ghcr.io/miztertea/nim-proxy`, signs it with keyless cosign,
attests SLSA build provenance, generates an SPDX SBOM, and publishes a GitHub
Release with the static binaries and the SBOM attached. The downloadable
assets (tarballs + SBOM) are themselves signed with `cosign sign-blob`, each
getting a `.sig` + `.pem` alongside it. SemVer + Keep a Changelog throughout.

It has two entry points: **Run workflow** in the Actions UI (the normal path
since v0.6.1 — the workflow's `prepare` job resolves the version from
Cargo.toml on `main`, refuses if that tag already exists, and mints/pushes the
`v*` tag itself), or a manual `v*` tag push (the classic path, still guarded
by tag-must-match-Cargo.toml). The dispatch-minted tag is pushed with
`GITHUB_TOKEN`, whose ref pushes trigger no follow-on runs — by design the
dispatch run carries the release end-to-end itself.

## 1. Prepare a release PR

- Bump `version` in `Cargo.toml` and sync `Cargo.lock`
  (`cargo update --package nim-proxy`). The boot banner and dashboard status
  report `CARGO_PKG_VERSION`; the workflow releases exactly this version.
- `CHANGELOG.md`: promote `[Unreleased]` to `[X.Y.Z] - <date>`, leave a fresh
  empty `[Unreleased]`, and update the compare/tag links in the footer.
- Update the supported-versions table in `SECURITY.md` to the new minor.
- Open a PR, wait for all CI jobs, merge.

## 2. Release

Actions → **Release** → *Run workflow* (from `main`), or ask the agent to
trigger it (`workflow_dispatch` is an ordinary API call — unlike tag pushes,
it works from restricted sessions). Equivalent manual path:

```sh
git fetch origin main
git tag -a vX.Y.Z -m "nim-proxy X.Y.Z" origin/main   # tag the merge commit
git push origin vX.Y.Z
```

Watch the run (`prepare` → `build amd64`/`build arm64` in parallel on native
runners → `merge` → `release`) under Actions — a few minutes end to end
(the arm64 leg builds natively on `ubuntu-24.04-arm`; QEMU emulation used to
make it ~30 minutes). If the `prepare` job fails with "already exists", the
version in Cargo.toml was never bumped — do step 1 first.

The cosign image signature, provenance attestation, and SBOM all target the
final **multi-arch manifest digest** (stitched by the `merge` job from the
per-arch digests), so `cosign verify` on any release tag resolves and verifies
the same manifest. The `release` job additionally signs each downloadable
asset (`cosign sign-blob`, same keyless workflow identity) so a tarball or the
SBOM pulled from the Releases page is verifiable without the registry.

## 3. Verify the shipped artifacts

```sh
docker pull ghcr.io/miztertea/nim-proxy:X.Y.Z
docker buildx imagetools inspect ghcr.io/miztertea/nim-proxy:X.Y.Z   # amd64 + arm64
docker run -d --name rel-smoke -p 127.0.0.1:8000:8000 ghcr.io/miztertea/nim-proxy:X.Y.Z
# boots into setup-required (no store yet); /health is public regardless
curl -fsS http://127.0.0.1:8000/health && docker logs rel-smoke | head -20  # banner shows vX.Y.Z
docker rm -f rel-smoke

cosign verify ghcr.io/miztertea/nim-proxy:X.Y.Z \
  --certificate-identity-regexp 'https://github.com/miztertea/nim-proxy/.github/workflows/release.yml@.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com

# and a downloaded asset against its detached .sig + .pem:
cosign verify-blob nim-proxy-X.Y.Z-linux-amd64.tar.gz \
  --signature nim-proxy-X.Y.Z-linux-amd64.tar.gz.sig \
  --certificate nim-proxy-X.Y.Z-linux-amd64.tar.gz.pem \
  --certificate-identity-regexp 'https://github.com/miztertea/nim-proxy/.github/workflows/release.yml@.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

Also check the GitHub Release page: two `nim-proxy-X.Y.Z-linux-*.tar.gz` assets
plus `nim-proxy-sbom.spdx.json`, each with its `.sig` + `.pem`, and generated
release notes. The notes are
grouped by PR label via `.github/release.yml` (Security / Breaking changes /
Features=`enhancement` / Fixes=`bug` / Documentation / Dependencies —
Dependabot's default label / Other; `skip-changelog` excludes a PR) — so
label PRs as they merge, not at release time.

## Fixing a bad release

Prefer roll-forward: fix on a branch, merge, tag the next patch version. Don't
retag or force-move a published tag — the image, signature, and provenance are
already public under the old digest.

Exception: if the workflow failed **before the GitHub Release was published**
(nothing user-facing exists yet beyond image tags), merging the fix and
re-pushing the same tag at the fixed commit is acceptable — a re-run of the
failed run won't help, because re-runs use the workflow file snapshot from the
original (broken) commit:

```sh
git push origin :refs/tags/vX.Y.Z          # delete the remote tag
git fetch origin main
git tag -fa vX.Y.Z -m "nim-proxy X.Y.Z" origin/main
git push origin vX.Y.Z
```

## Repo settings (applied — recorded for reference)

- **Tag ruleset on `v*`** (Settings → Rules → Rulesets): restrict creation to
  the repository admin role (GitHub Actions' `GITHUB_TOKEN` acts as the repo
  and passes), block deletion and non-fast-forward updates. This codifies the
  "never retag a published release" rule above instead of relying on
  discipline.
- **Branch ruleset on `main`** (Settings → Rules → Rulesets): require a pull
  request before merging (0 approvals — solo maintainer; the point is blocking
  direct pushes, and 1 approval would deadlock a solo repo); require these
  status checks, which must stay in sync with the CI job list in
  `.github/workflows/ci.yml` (+ `codeql.yml`): `fmt, clippy, tests`,
  `coverage`, `msrv`, `cargo-deny`, `gitleaks`, `workflow lint`,
  `dependency review`, `docker build`, `codeql (rust)`; block force pushes and
  deletions. Do **not** require signed commits — session commits are unsigned
  and would be blocked. Leave "require branches up to date" off so Dependabot
  PRs don't need a rebase per merge.
- **Private vulnerability reporting** enabled (Settings → Code security) —
  `SECURITY.md` lists advisories as the only reporting channel.
- **Auto-delete head branches** enabled.

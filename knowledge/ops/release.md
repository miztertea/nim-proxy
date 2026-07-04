---
type: Runbook
title: Cutting a release
description: Version bump, changelog, tag, release workflow, and post-release verification.
tags: [release, versioning, ghcr]
timestamp: 2026-07-03T00:00:00Z
---

# Cutting a release

Releases are tag-driven: pushing a `v*` tag runs `.github/workflows/release.yml`,
which builds a multi-arch (amd64+arm64) image, pushes it to
`ghcr.io/miztertea/nim-proxy`, signs it with keyless cosign, attests SLSA build
provenance, generates an SPDX SBOM, and publishes a GitHub Release with the
static binaries and the SBOM attached. SemVer + Keep a Changelog throughout.

## 1. Prepare a release PR

- Bump `version` in `Cargo.toml` and sync `Cargo.lock`
  (`cargo update --package nim-proxy`). The boot banner and dashboard status
  report `CARGO_PKG_VERSION`, and the release workflow **fails if the tag does
  not match Cargo.toml** (guard step in the `image` job).
- `CHANGELOG.md`: promote `[Unreleased]` to `[X.Y.Z] - <date>`, leave a fresh
  empty `[Unreleased]`, and update the compare/tag links in the footer.
- Update the supported-versions table in `SECURITY.md` to the new minor.
- Open a PR, wait for all CI jobs, merge.

## 2. Tag

```sh
git fetch origin main
git tag -a vX.Y.Z -m "nim-proxy X.Y.Z" origin/main   # tag the merge commit
git push origin vX.Y.Z
```

Watch the **Release** workflow (`image` job → `release` job) under Actions.

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
```

Also check the GitHub Release page: two `nim-proxy-X.Y.Z-linux-*.tar.gz` assets
plus `nim-proxy-sbom.spdx.json`, and generated release notes.

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

## One-time repo settings (already done; recorded for reference)

- **Private vulnerability reporting** enabled (Settings → Code security) —
  `SECURITY.md` lists advisories as the only reporting channel.
- **Auto-delete head branches** enabled.
- **Branch protection / ruleset on `main`** (Settings → Rules → Rulesets):
  require a pull request before merging (0 approvals acceptable solo — the
  point is blocking direct pushes); require status checks
  `fmt, clippy, tests`, `coverage`, `cargo-deny`, `gitleaks`, `docker build`
  (strict/up-to-date); block force pushes and deletions. Do **not** require
  signed commits — session commits are unsigned and would be blocked.

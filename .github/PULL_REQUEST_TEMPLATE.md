<!--
Thanks for contributing to nim-proxy! Before opening this PR:
  • Read CONTRIBUTING.md and AGENTS.md — they define the required checks and the
    "knowledge base in lockstep" rule that CI and review enforce.
  • Keep the scope tight — nim-proxy is deliberately small. One thing done well
    beats many things done partway.

Fill in every section below. Keep the headings even if a section is short; put
"None" / "N/A" rather than deleting one. In the checklist, tick every box that
applies and ~~strike through~~ any that genuinely don't with a one-line reason —
an unexplained blank box reads as "not done", not "not applicable".
-->

## Summary

<!-- What does this change, in 2-4 sentences? Lead with the outcome. -->

## Type of change

<!-- Tick all that apply. -->

- [ ] Bug fix (no behavior change beyond the fix)
- [ ] New feature / behavior change
- [ ] Performance / rate-limiting
- [ ] Security / hardening
- [ ] Refactor (no behavior change)
- [ ] Docs / knowledge base only
- [ ] CI / build / release infrastructure
- [ ] Release (version bump)

## Related issues

<!-- "Fixes #123" / "Closes #123" / "Relates to #123", or "None". For anything
beyond a small fix, an issue should exist first (see CONTRIBUTING.md). -->

## What & why

<!-- The reasoning, not just the diff. What problem does this solve, and what
alternatives did you weigh? If it changes a decision recorded under
knowledge/decisions/, link that page (or add a new one — see the checklist). -->

## How it was tested

<!-- The exact commands you ran and what you observed. Paste the key output;
"tests pass" without evidence is not enough. -->

```sh

```

## Screenshots / output

<!-- Dashboard/UI or notable CLI-output changes. Delete this section if N/A. -->

## Breaking changes & migration

<!-- Anything an operator must act on: config keys, env vars, the `/v1` surface,
or the on-disk config store format/version. Write "None" if there are none. -->

## Checklist

### Code quality
- [ ] `cargo fmt --check` is clean.
- [ ] `cargo clippy --all-targets -- -D warnings` is clean (zero warnings).
- [ ] `cargo test` passes locally (unit + end-to-end).
- [ ] New behavior ships with tests (unit and/or e2e), and existing guard tests still pass.
- [ ] Scope is tight; commit messages explain the *why*.

### Docs & knowledge base — lockstep is a hard rule (see AGENTS.md)
- [ ] README / `examples/` updated if user-facing behavior or setup changed.
- [ ] Affected `knowledge/` pages updated **in this PR** (code and knowledge must not diverge).
- [ ] New decision → new `knowledge/decisions/` page **and** a `knowledge/index.md` row, in ADR shape (Context → Options → Choice → Consequences).
- [ ] Dated entry appended to `knowledge/log.md` (`## [YYYY-MM-DD] ingest|decision|lint — summary`).
- [ ] `CHANGELOG.md` `[Unreleased]` updated (Keep a Changelog format).

### Security — required if this touches `src/auth.rs`, `src/config.rs`, `src/settings.rs`, the API-key gate or label sanitizing in `src/proxy.rs`, or the dashboard `innerHTML`
- [ ] Fail-closed posture preserved (pre-setup data plane closed; post-setup surfaces require a session; corrupt/future-version store refuses to boot).
- [ ] Every dynamic value written to `innerHTML` goes through `esc()`; label sanitizing invariants (non-empty, ≤64 chars, safe charset) hold.
- [ ] Reviewed against [SECURITY.md](../SECURITY.md) and the auth-posture / input-sanitizing decision pages.

### Rate-limiting — required if this touches pacing, the key pool, the dispatcher, or affinity
- [ ] Load harness shows **zero upstream rate violations** (`scripts/mock_nim.py --enforce` + `scripts/loadtest.py`) — a hard requirement, not a target.

### Workflows & supply chain — required if this touches `.github/workflows/**` or the release path
- [ ] New/changed Actions are pinned to a full commit SHA with a `# vX.Y.Z` comment; `actionlint` + `zizmor` pass (the CI workflow-lint job).
- [ ] Release or version changes follow [`knowledge/ops/release.md`](../knowledge/ops/release.md) (version bump + `Cargo.lock` sync, CHANGELOG promotion, SECURITY.md supported-versions table).

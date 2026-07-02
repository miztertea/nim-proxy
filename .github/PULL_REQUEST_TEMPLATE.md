<!--
Thanks for contributing to nim-proxy! Please read CONTRIBUTING.md first.
Keep the scope tight — nim-proxy is deliberately small.
-->

## What & why

<!-- What does this change, and what problem does it solve? Link any issue: Fixes #123 -->

## How it was tested

<!-- Commands you ran and what you observed. -->

## Checklist

- [ ] **What/why** is explained above (and an issue is linked if one exists).
- [ ] **Tests added or updated** for the new behavior (unit and/or e2e).
- [ ] `cargo fmt --check` is clean.
- [ ] `cargo clippy --all-targets -- -D warnings` is clean (zero warnings).
- [ ] `cargo test` passes locally.
- [ ] **Docs updated in lockstep** — README and/or `examples/` reflect the change.
- [ ] **Knowledge base updated in the same PR** — affected `knowledge/` pages,
      a new `decisions/` page + `index.md` row for new decisions, and a dated
      `knowledge/log.md` entry (see AGENTS.md).
- [ ] **Security considered** — if this touches auth, label sanitizing, or the
      dashboard `innerHTML`, the fail-closed and escaping invariants are
      preserved (see SECURITY.md and the auth/input-sanitizing decision pages).
- [ ] **Rate-limiting proven** — if this touches pacing, the key pool, the
      dispatcher, or affinity, the load harness shows **zero upstream rate
      violations** (`scripts/mock_nim.py --enforce` + `scripts/loadtest.py`).

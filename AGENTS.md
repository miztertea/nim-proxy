# Agent guide for nim-proxy

nim-proxy is a Rust proxy that makes NVIDIA NIM's free tier usable for agent
harnesses: it paces requests to the per-key rate limit, load-balances across
keys, and keeps client connections alive while it waits. Source lives in
`src/` (`main.rs`, `proxy.rs`, `pool.rs`, `dispatch.rs`, `history.rs`,
`auth.rs`, `dashboard.html`), tests in `tests/`, load harness in `scripts/`.

Auth lives in `src/auth.rs` (fail-closed posture, admin-password session
cookie, constant-time compares); the API-key gate and label sanitizing are in
`src/proxy.rs`. Any change touching auth, request labels, or the dashboard's
`innerHTML` must keep the security invariants — see the `decisions/` pages on
auth posture and input sanitizing before editing.

## Working on the code

- `cargo test` runs 13 unit + 15 end-to-end tests (the e2e suite launches the
  real binary against a scriptable mock; see `tests/support/mod.rs`).
- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` must stay clean
  (CI enforces both).
- Rate-limit changes must pass the load harness:
  `scripts/mock_nim.py --enforce` + `scripts/loadtest.py` — zero upstream
  violations is a hard requirement, not a target.
- The dashboard is one embedded file, `src/dashboard.html` — no build step,
  no external assets except optional CDN logos with an offline fallback.

## The knowledge base (`knowledge/`)

`knowledge/` is an Open Knowledge Format bundle (the LLM-wiki pattern): the
project's compiled memory — why decisions were made, validated research about
NIM, how components work, and operational runbooks. **Read
`knowledge/index.md` before making non-trivial changes**; the reasoning that
constrains your change is probably recorded there.

Schema:

- One concept per file, kebab-case filename, path = identity.
- YAML frontmatter: `type` (required — one of `Decision`, `Research Finding`,
  `Component`, `Runbook`), plus `title`, `description`, `tags`, `timestamp`,
  and `resource` (URL) where applicable.
- Markdown body with relative links to other pages — keep the graph connected.
- Decision pages follow a lightweight ADR shape: Context → Options →
  Choice → Consequences.

Maintenance workflow (you, the agent, are the maintainer):

1. **Ingest**: when a merged change alters behavior described in the wiki,
   update the affected pages in the same PR — don't let code and knowledge
   diverge.
2. **New decisions** get a new page under `decisions/` and a line in
   `knowledge/index.md`.
3. **Log**: append a dated entry to `knowledge/log.md` for every ingest
   (`## [YYYY-MM-DD] ingest|decision|lint — summary`).
4. **Lint**: if you spot a contradiction between a page and the code, flag it
   in your summary and fix the page — the code is the source of truth for
   *what*, the wiki for *why*.

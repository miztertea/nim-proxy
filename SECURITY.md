# Security Policy

nim-proxy sits in the request path for every harness and every API key it
fronts, and it can be exposed to the public internet (VPS, ECS, Railway, Fly).
We take its security posture seriously and welcome private reports.

## Supported versions

Security fixes land on the latest `0.6.x` release. Older minors are not
patched — upgrade to the newest `0.6.x` tag.

| Version | Supported          |
|---------|--------------------|
| 0.6.x   | :white_check_mark: |
| < 0.6   | :x:                |

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Open a private report through GitHub's
[Security Advisories](https://github.com/miztertea/nim-proxy/security/advisories/new)
("Report a vulnerability"). This keeps the details confidential until a fix
ships, and it is the only reporting channel — no email is monitored for this
project.

Please include: affected version/commit, the `/v1` API mode (`keyed` vs.
`open`) and whether setup was completed, a description of the impact, and a
minimal reproduction (a request body, a config, or a curl one-liner is ideal).
If you have a suggested fix, include it — but a clear report is enough.

### Response expectations

This is a small, volunteer-maintained project. We aim to:

- **Acknowledge** your report within 3 business days.
- **Triage** and confirm (or explain why it's out of scope) within 7 days.
- **Fix** confirmed vulnerabilities in a `0.6.x` patch release and credit you in
  the advisory (unless you prefer to stay anonymous).

Please give us a reasonable window to ship a fix before any public disclosure.

## Scope and current posture

nim-proxy has already been through a dedicated security-hardening pass. The
reasoning behind the current defenses is documented in the knowledge base:

- **Auth / fail-closed posture** —
  [`knowledge/decisions/auth-posture-and-dashboard-password.md`](knowledge/decisions/auth-posture-and-dashboard-password.md).
  The proxy **fails closed**: before setup the data plane is closed
  (`/v1`→`503`, browsers→`/setup`); after setup the dashboard, `/metrics`, and
  `/api/history` **always** require a logged-in session (HMAC-signed, HttpOnly,
  SameSite=Strict cookie carrying a password-hash fragment so a password
  change/reset invalidates sessions). Users, roles (superuser/admin/user), and
  per-key ownership live in the config store; passwords are PBKDF2-HMAC-SHA256
  (600k iterations). The `/v1` API is `keyed` (client `npk_…` bearer keys,
  stored as SHA-256 digests) or `open` (an explicit, loudly-warned
  loopback/firewalled opt-in that affects only `/v1`). Secret comparison is
  constant-time; there is a failed-login throttle and a rejected-key delay. The
  store is 0600 with atomic writes; a corrupt store refuses to boot. See also
  [`ui-managed-config-store`](knowledge/decisions/ui-managed-config-store.md).
- **Input sanitizing / XSS / injection** —
  [`knowledge/decisions/input-sanitizing-and-xss.md`](knowledge/decisions/input-sanitizing-and-xss.md).
  Client-controlled fields (`model`, `path`) are sanitized to a conservative
  charset, length-capped, and cardinality-bounded at ingest, so they can't
  inject into the Prometheus exposition format, access logs, or persisted
  history. The embedded dashboard HTML-escapes every dynamic `innerHTML` sink,
  and all responses carry a strict `Content-Security-Policy` plus
  anti-framing/anti-sniffing headers.

- **Supply chain / release integrity.** Every GitHub Actions step is pinned to
  a full commit SHA (Dependabot keeps pins fresh); every CI/release job runs
  [`step-security/harden-runner`](https://github.com/step-security/harden-runner)
  egress monitoring; an [OpenSSF Scorecard](https://scorecard.dev/viewer/?uri=github.com/miztertea/nim-proxy)
  workflow scores the posture weekly (badge in the README). CodeQL scans the
  Rust source on every change; the workflows themselves are linted by
  `actionlint` + `zizmor` in CI; PRs that introduce a known-vulnerable crate
  are blocked by dependency review; and a weekly scheduled `cargo-deny`
  advisories run catches new RUSTSEC advisories between pushes. Release images are
  built on native runners from the repo Dockerfile, signed with keyless cosign,
  and published with SLSA build provenance and an SPDX SBOM — all anchored to
  the multi-arch manifest digest. `v*` release tags are protected by a ruleset
  (no updates, deletions, or force pushes), so a published tag can never be
  silently moved.

**In scope** (please report):

- Authentication or authorization bypass (reaching `/v1/*` in `keyed` mode, or
  the dashboard, `/metrics`, or `/api/history` without a valid session; a role
  or ownership bypass — e.g. a `user` reaching admin endpoints or reading
  another user's key rows; claiming an already-configured instance via `/setup`).
- Injection reaching the metrics exposition, logs, persisted history, or the
  dashboard (XSS), including via request bodies or the `model`/`path` fields.
- Ways to make the proxy **exceed a key's upstream rate limit** (its core
  invariant) or otherwise misbehave against the upstream.
- Secret leakage (NIM keys, client-key secrets, password hashes, session
  cookie) via logs, metrics, error responses, the config store, or history
  snapshots — including a `/v1` client secret or NIM key value being readable
  back through any API after creation.
- Denial of service that bypasses the in-flight cap (`max_inflight`) or the
  failed-login throttle.

**Out of scope:**

- Running the `/v1` API in `open` mode on a network-reachable interface — this
  is a documented, opt-in "no client-key" configuration for trusted networks,
  not a vulnerability.
- The setup-claim window on a fresh, unconfigured instance (first visitor
  becomes the superuser) — an accepted, documented risk (the data plane is
  closed pre-setup; a loud boot log says to finish setup immediately).
- Missing TLS. nim-proxy has **no built-in TLS by design**; terminate TLS at a
  reverse proxy or platform edge for any exposed deployment (see the README's
  Security & deployment section).
- Rate-limit abuse of your own NVIDIA keys, or NVIDIA-side terms-of-service
  questions — those are between you and NVIDIA.
- Vulnerabilities in third-party dependencies already flagged by `cargo audit`
  (CI runs it); report those upstream, though a note is still welcome.

Thank you for helping keep nim-proxy and its users safe.

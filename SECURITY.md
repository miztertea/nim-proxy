# Security Policy

nim-proxy sits in the request path for every harness and every API key it
fronts, and it can be exposed to the public internet (VPS, ECS, Railway, Fly).
We take its security posture seriously and welcome private reports.

## Supported versions

Security fixes land on the latest `0.5.x` release. Older minors are not
patched — upgrade to the newest `0.5.x` tag.

| Version | Supported          |
|---------|--------------------|
| 0.5.x   | :white_check_mark: |
| < 0.5   | :x:                |

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Open a private report through GitHub's
[Security Advisories](https://github.com/miztertea/nim-proxy/security/advisories/new)
("Report a vulnerability"). This keeps the details confidential until a fix
ships, and it is the only reporting channel — no email is monitored for this
project.

Please include: affected version/commit, run mode (secure vs. `INSECURE_NO_AUTH`),
a description of the impact, and a minimal reproduction (a request body, a
config, or a curl one-liner is ideal). If you have a suggested fix, include it —
but a clear report is enough.

### Response expectations

This is a small, volunteer-maintained project. We aim to:

- **Acknowledge** your report within 3 business days.
- **Triage** and confirm (or explain why it's out of scope) within 7 days.
- **Fix** confirmed vulnerabilities in a `0.5.x` patch release and credit you in
  the advisory (unless you prefer to stay anonymous).

Please give us a reasonable window to ship a fix before any public disclosure.

## Scope and current posture

nim-proxy has already been through a dedicated security-hardening pass. The
reasoning behind the current defenses is documented in the knowledge base:

- **Auth / fail-closed posture** —
  [`knowledge/decisions/auth-posture-and-dashboard-password.md`](knowledge/decisions/auth-posture-and-dashboard-password.md).
  The proxy **refuses to start on a network-reachable port without auth**.
  Secure mode requires both `PROXY_API_KEYS` (gates `/v1/*`) and `ADMIN_PASSWORD`
  (gates the dashboard, `/metrics`, `/api/history` via an HMAC-signed, HttpOnly,
  SameSite=Strict session cookie). Open mode (`INSECURE_NO_AUTH=true`) is an
  explicit, loudly-warned opt-in for loopback/firewalled use only. Secret
  comparison is constant-time; there is a failed-login throttle and a
  rejected-key delay.
- **Input sanitizing / XSS / injection** —
  [`knowledge/decisions/input-sanitizing-and-xss.md`](knowledge/decisions/input-sanitizing-and-xss.md).
  Client-controlled fields (`model`, `path`) are sanitized to a conservative
  charset, length-capped, and cardinality-bounded at ingest, so they can't
  inject into the Prometheus exposition format, access logs, or persisted
  history. The embedded dashboard HTML-escapes every dynamic `innerHTML` sink,
  and all responses carry a strict `Content-Security-Policy` plus
  anti-framing/anti-sniffing headers.

**In scope** (please report):

- Authentication or authorization bypass (reaching `/v1/*`, the dashboard,
  `/metrics`, or `/api/history` without valid credentials in secure mode).
- Injection reaching the metrics exposition, logs, persisted history, or the
  dashboard (XSS), including via request bodies or the `model`/`path` fields.
- Ways to make the proxy **exceed a key's upstream rate limit** (its core
  invariant) or otherwise misbehave against the upstream.
- Secret leakage (API keys, `ADMIN_PASSWORD`, session cookie) via logs,
  metrics, error responses, or history snapshots.
- Denial of service that bypasses the in-flight cap (`MAX_INFLIGHT`) or the
  failed-login throttle.

**Out of scope:**

- Running in `INSECURE_NO_AUTH=true` mode on a network-reachable interface — this
  is a documented, opt-in "no auth" configuration, not a vulnerability.
- Missing TLS. nim-proxy has **no built-in TLS by design**; terminate TLS at a
  reverse proxy or platform edge for any exposed deployment (see the README's
  Security & deployment section).
- Rate-limit abuse of your own NVIDIA keys, or NVIDIA-side terms-of-service
  questions — those are between you and NVIDIA.
- Vulnerabilities in third-party dependencies already flagged by `cargo audit`
  (CI runs it); report those upstream, though a note is still welcome.

Thank you for helping keep nim-proxy and its users safe.

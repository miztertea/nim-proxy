---
type: Decision
title: Sanitize client-controlled labels; escape + CSP the dashboard
description: The request `model` field flowed unsanitized into metric labels, logs, and the dashboard — a stored-XSS + cardinality + log-injection vector. Fixed at ingest and in the view.
tags: [security, xss, metrics, dashboard]
timestamp: 2026-07-02T00:00:00Z
---

# Sanitize client-controlled labels; escape + CSP the dashboard

## Context

The security review found one root cause with three sinks: the client-supplied
`model` field (and `path`) flowed **unsanitized** into (1) Prometheus metric
labels, (2) the access-log line, and (3) the dashboard via `innerHTML`. A
semi-trusted client (an authenticated "friend" in a shared pool) could:

- **Stored XSS** — `model` = `<img src=x onerror=…>` is stored as a label,
  persisted in `history.jsonl`, and executes in the operator's browser when
  they open the dashboard.
- **Cardinality blowup** — unbounded distinct `model` values grow the metrics
  registry (RAM) and every 5-min history snapshot (disk).
- **Exposition / log injection** — quotes, braces, and newlines break the
  Prometheus text format (spoof series) or forge log lines / inject ANSI.

## Choice

Defense in depth, fixing it at both ends:

- **At ingest** (`src/proxy.rs`): `sanitize_label` keeps a conservative
  charset `[A-Za-z0-9._/:-]` (what model ids actually use), drops everything
  else, and caps length at 64 — killing exposition + log injection. A bounded
  seen-set (cap 256) collapses further distinct models to `"other"`, bounding
  cardinality. `path` is reduced to an allowlist of known endpoints.
- **In the view** (`src/dashboard.html`): an `esc()` HTML-escaper wraps every
  dynamic value interpolated into `innerHTML` (model, client, tooltip/legend
  series names). Pure defense-in-depth now that labels are sanitized server
  side — the payload can no longer even reach the label.
- **Response CSP** (`src/main.rs`): `connect-src 'self'` blocks the classic
  exfil vector even if an escape were missed; `frame-ancestors 'none'` +
  `nosniff` + `X-Frame-Options` round it out.

## Consequences

- Verified end-to-end in a real browser: a malicious `model` sent through the
  proxy renders as inert text, fires no script, and injects no element; the
  `/metrics` label is stripped to a safe token on one line.
- Covered by unit tests (`sanitize_label`, `bounded_label`, `label_path`) and
  an e2e test asserting `/metrics` carries no injection chars and no spurious
  series.
- Constant-time comparison is used for API keys and the admin password (see
  [auth-posture](auth-posture-and-dashboard-password.md)).

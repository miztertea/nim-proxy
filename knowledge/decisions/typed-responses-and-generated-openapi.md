---
type: Decision
title: Typed API responses and a generated OpenAPI spec
description: Response bodies become derive(Serialize) structs whose declaration order is the wire order, and openapi.json is generated from those types with utoipa — spec file only, no served UI.
tags: [api, openapi, dashboard, wire-format, ci]
timestamp: 2026-07-29T00:00:00Z
---

# Typed API responses and a generated OpenAPI spec

## Context

Every response the operator surface served was built by hand with
`serde_json::json!`: ~20 sites in `src/settings.rs`, two more in `src/lib.rs`.
The shape of `/api/config` existed in exactly two places — inside a macro
invocation, and inside whatever `src/dashboard.html` happened to read — and
nowhere as a declaration. That has three costs:

- **No generator input.** There was nothing for an OpenAPI tool to read, so
  there was no machine-readable contract for the dashboard, for a third-party
  client, or for a reviewer asking "what does this return for a `user` role?".
- **Silent shape drift.** Adding a key to a `json!` body is invisible to the
  compiler. Removing one is too — the 0.6.6 pricing removal had to be chased
  through both the macro and the page.
- **Role filtering by omission.** `/api/config` built an admin body and then
  *added* `server`/`users` keys, which reads as "start open, remove things" —
  the opposite of the [fail-closed posture](auth-posture-and-dashboard-password.md)
  the rest of the code holds.

`src/config.rs` and `src/history.rs` were already typed; the API layer was the
hold-out.

The constraint that shaped everything: **the wire format must not move.**
0.6.6 already carries two breaking changes (the pricing removal and the
`nimproxy_lane_benched_total` rename). A third — an accidental one — would be
unforgivable, and 47 API-touching tests plus a dashboard that parses these bodies
are the things that would break.

## Options

1. **Hand-write `openapi.json` alongside the `json!` bodies.** Rejected: two
   descriptions of one thing, and the one CI can check is not the one that
   serves requests. This is the drift the change exists to remove.
2. **Type the responses, generate the spec, and serve it through
   `utoipa-scalar` / `utoipa-redoc`.** Rejected for the *serving* half only.
   Both UIs load JavaScript from a CDN, which the dashboard's
   `script-src 'self' 'unsafe-inline'` CSP forbids; the fixes are widening the
   CSP for a docs page (weakening a control that exists to contain XSS) or
   bundling ~1 MB of assets into a `FROM scratch` image whose whole point is
   being a single static binary.
3. **Type the responses and generate a spec *file*.** Chosen.

## Choice

Option 3, in two halves.

**Wire types.** `src/api.rs` holds every response body as a
`derive(Serialize, ToSchema)` struct. Handlers construct a value instead of a
`json!` literal. `/api/config` now *builds or does not build* the admin
sections — role filtering became a `Option<ServerSettings>` that is `None`
for a `user`, so the type says what the security posture already required.

**Generated spec.** `utoipa` (pinned `=5.5.0`, project convention) renders
`openapi.json` at the repo root from `#[utoipa::path]` on the handlers and
`ToSchema` on the types. 16 operations: **14** `/api/*` routes, plus
`POST /setup` and `POST /setup/validate-key`. `GET /api/locale-bootstrap`
returns the typed, field-ordered installed-locale registry and is explicitly
public; the other 13 `/api/*` operations inherit operator authentication.
The account request is the one schema-only exception to derive generation:
utoipa 5.5 collapses its untagged password-or-locale mixed enum to the first
variant, so a small `PartialSchema` builder records those two native JSON
objects while the handler retains the typed runtime branches. One serde map
visitor owns that runtime boundary: duplicate known account fields are data
errors (`422 invalid_json`) before a generic JSON object can collapse them,
the locale action is an exact closed object, and the legacy password shape
continues to ignore unknown extension fields.

- **The bootstrap and two `/setup` routes are flagged unauthenticated.**
  Bootstrap is required before any page can choose its compiled locale. The
  setup routes
  are JSON endpoints that a wizard, a scripted install, or an operator
  debugging upstream reachability legitimately calls, and leaving them
  undocumented would not make them less reachable. They sit outside the
  `route_layer` because no user exists yet, so they carry an explicit empty
  `security: []` — the document-level requirement (session cookie *or* header
  credentials) applies to everything else. Once a superuser exists, the page
  `GET /setup` is a bare 404 while either setup POST is a typed 409
  `setup_complete` conflict, so the claim window is exactly one claim wide.
- **Out of scope, deliberately:** the OpenAI-compatible `/v1` passthrough (that
  contract is the upstream's, not ours), the HTML page routes, the
  form-encoded `/login`/`/logout` browser flow, plain-text `/health`, and the
  Prometheus exposition at `/metrics`. The spec describes the *dashboard API*,
  not every path the router answers.
- **No served UI**, per option 2.

### JSON rejection boundary

The protected `/api/*` router and the two setup **POST** routes use the same
typed rejection boundary. `ApiJson<T>` delegates parsing and the configured
`DefaultBodyLimit` to Axum, then translates syntax/data failures to
`invalid_json` (preserving Axum's existing 400 syntax and 422 data statuses), missing or unsuitable JSON media types to
`unsupported_media_type`, and a limit hit to `body_too_large`. `ApiQuery<T>`
translates dashboard-query decoding failures to `invalid_query`. The nested
control-plane router supplies `not_found` and `method_not_allowed` fallbacks,
so these framework-level rejections serialize as `ApiError` too.

This boundary is intentionally narrow: setup **GET** retains its HTML/bare-404
contract, and login/form, health, metrics, and `/v1` retain their existing
contracts. After a claim, setup POSTs check that phase before inspecting JSON
headers or buffering the body and answer the typed `409 setup_complete`
conflict; the page GET remains a bare 404. The generated spec documents each
non-success dashboard/setup response with the `ApiError` schema, and its test
rejects an untyped non-2xx response.

### Field order is the wire order

The load-bearing detail. `serde` emits struct fields in **declaration order**;
`serde_json::Map` is a `BTreeMap` unless the `preserve_order` feature is on
(it is not), so every `json!` body this change replaced emitted its keys in
**ASCII order**. A struct that declares fields in a "natural" reading order
therefore *reshapes the response*.

So every wire struct declares its fields ASCII-sorted, and three existing
types were reordered to match what they had always serialized as:
`history::MetricValue`, `history::RollupPoint`, `history::HistoryDiagnostics`,
and `config::Limits`. `_` (0x5F) sorts before every lowercase letter, so
`lane` < `last4` and `username` < `users` — not obvious, hence the guard test
`api::field_order_stays_ascii_sorted`, which serializes a populated value of
every wire type and asserts the emitted keys come out sorted.

`config::GovernorCfg::overrides` changed from `HashMap` to `BTreeMap` for the
same reason: inside `json!` it was sorted by the intermediate `Map`, and
serialized directly it would have been hash-ordered. The side benefit is that
`config.json` is now byte-deterministic — two saves of the same config used to
differ.

### Drift is checked, not trusted

`tests/openapi.rs` regenerates the spec and compares it to the committed file;
CI's `check` job additionally runs `UPDATE_OPENAPI=1 cargo test --test openapi`
followed by `git diff --exit-code -- openapi.json`, so a stale spec fails the
build rather than merely being discouraged. `spec_is_usable` asserts the
document is consumable at all: 16 operations, every one tagged with a
documented 200, the 13 protected `/api/*` operations inheriting the auth
requirement, and bootstrap plus every `/setup` operation explicitly waiving
it.

## Consequences

- **The wire format did not move.** Verified two ways: the 47 API-touching tests
  in `tests/e2e.rs` pass **unmodified**, and a throwaway harness captured raw
  response bytes for 31 request/response pairs (both `/api/config` role views,
  both dashboard endpoints with and without traffic, every settings write, and
  every error branch) before and after — key-for-key identical at every
  nesting level.
- **`info.version` tracks `CARGO_PKG_VERSION`**, so a version bump makes
  `openapi.json` stale and CI says so. [Cutting a release](../ops/release.md)
  step 1 now includes regenerating it.
- Two new crates in the tree (`utoipa`, `utoipa-gen`); `indexmap` was already
  there. Both MIT OR Apache-2.0, so `cargo deny`'s allowlist did not move.
- The `/api/config` role filter is now expressed in the type. A future field
  that should be admin-only goes inside `ServerSettings` and inherits the
  filtering; adding it to `ConfigResponse` instead is a visible choice in a
  diff rather than an invisible one inside a macro.
- `config.json`'s `limits` block and `governor.overrides` are written in a new
  key order. Purely cosmetic — the store is read by serde, which does not care
  — and no migration runs.
- The spec is a file, not a page. Operators who want a browsable UI can point
  any offline viewer at `openapi.json`; nothing is served, so the CSP and the
  scratch image are untouched.
- Malformed JSON, wrong media type, request-size, query, route, method, and
  post-claim setup failures now have stable codes and one raw-byte E2E matrix.
  Every row also asserts that `config.json` is byte-identical after rejection.

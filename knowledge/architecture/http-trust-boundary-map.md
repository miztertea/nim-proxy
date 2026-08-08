---
type: Component
title: HTTP trust-boundary map
description: Live route inventory across setup phase, client/operator authentication, roles, wire types, side effects, UI callers, and generated OpenAPI.
tags: [http, routes, auth, openapi, security]
timestamp: 2026-07-30T00:00:00Z
---

# HTTP trust-boundary map

`src/lib.rs` is the only handler registry. It registers handlers through the
fixed path constants in `src/routes.rs`; the `RouteContract` rows in that
module are test-only descriptive metadata and cannot dispatch a request.
`RouteContract.path` is the registered/OpenAPI template, while
`probe_path` is a concrete fixture-backed request path. This distinction is
load-bearing for `/v1/{*path}` and future parameterized routes.

The current router has 34 method/path contracts, including nine presentation
asset routes. A
superuser has no exclusive route: it has admin endpoint power plus the
undeletable/undemotable account invariant. `OperatorSuperuser` exists so a
future exclusive boundary must be declared deliberately, not inferred from
the role name.

## Contract matrix

The compact proof labels are:

- **M** — `routes::tests::inventory_agrees_with_generated_openapi` owns the
  compiled inventory, method/path membership, OpenAPI security inheritance,
  phase counts, fixture probe paths, the explicit asset inventory, and the
  no-superuser-exclusive-route decision.
- **B** — `route_contract_behavior_matrix` sends real requests through the
  binary in before-setup, anonymous configured, ordinary-user, admin, and
  superuser states. Each row asserts status/error, request and success content
  types, exact `config.json` before/after bytes, session-cookie presence, and
  exact coarse upstream-call deltas. Endpoint-specific tests and component
  pages, not B, own internal publication, cache, pool, and session semantics.
- **C** — `route_contract_client_auth_matrix` proves pre-setup closure and
  keyed-mode missing/wrong/valid bearer behavior independently of operator
  sessions, including upstream and durable-byte effects.
- **O** — `route_contract_ownership_matrix` adds own/other rows for NIM and
  client keys and proves rejected foreign writes preserve durable bytes.

Every JSON `/api` and setup POST additionally inherits the extractor codes
`body_too_large`, `invalid_json`, and `unsupported_media_type`; the `/api`
subtree owns `invalid_query`, `method_not_allowed`, and `not_found` where
applicable. Their exact envelopes and ordering live in
[typed responses and generated OpenAPI](../decisions/typed-responses-and-generated-openapi.md),
not a second copy here.

| Method and path | Consumer | Public/private | Phase | Authentication / role | Request type | Success type | Stable boundary errors | Side effects | UI caller | OpenAPI | Proof |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `GET /` | Browser operator | Private | Post-setup | Session; any role | None | HTML | Pre-setup/anonymous redirect | None | Dashboard navigation | No | B, M |
| `GET /dash` | Browser operator | Private | Post-setup | Session; any role | None | HTML | Pre-setup/anonymous redirect | None | Dashboard alias | No | B, M |
| `GET /assets/operator/operator.css` | Browser operator | Private | Post-setup | Session/header credentials; any role | None | CSS | `setup_required`, `unauthorized` | None | Dashboard page | No | B, M |
| `GET /assets/operator/shared.js` | Browser operator | Private | Post-setup | Session/header credentials; any role | None | JavaScript | `setup_required`, `unauthorized` | None | Dashboard page | No | B, M |
| `GET /assets/operator/dashboard.js` | Browser operator | Private | Post-setup | Session/header credentials; any role | None | JavaScript | `setup_required`, `unauthorized` | None | Dashboard page | No | B, M |
| `GET /assets/operator/settings.js` | Browser operator | Private | Post-setup | Session/header credentials; any role | None | JavaScript | `setup_required`, `unauthorized` | None | Dashboard page | No | B, M |
| `GET /assets/operator/locales/{locale}.json` | Browser operator | Private | Post-setup | Session/header credentials; any role; gate precedes locale lookup | None | JSON catalog | `setup_required`, `unauthorized`; unknown locale is 404 only after the gate | None | Dashboard startup | No | B, M |
| `GET /metrics` | Prometheus/operator | Private | Post-setup | Session, Basic, or header credentials; any role | None | Prometheus text | `setup_required`, `unauthorized` | Config unchanged; no upstream call | External scraper | No | B, M |
| `GET /api/dashboard` | Browser operator | Private | Post-setup | Session/header credentials; any role | Query | JSON | `setup_required`, `unauthorized`, `invalid_query`, `invalid_time_window` | Config unchanged; no upstream call | Dashboard range load | Yes | B, M |
| `GET /api/dashboard/now` | Browser operator | Private | Post-setup | Session/header credentials; any role | None | JSON | `setup_required`, `unauthorized` | Config unchanged; no upstream call | Dashboard polling | Yes | B, M |
| `GET /api/config` | Browser operator | Private | Post-setup | Session/header credentials; any role; response filtered by role/ownership | None | JSON including current-user `locale`; admin server includes `default_locale` | `setup_required`, `unauthorized` | Config unchanged; no upstream call | Operator startup and Settings | Yes | B, M |
| `GET /api/locale-bootstrap` | Browser | Public | Always | None | None | Installed locale registry plus persisted server default | None | None | Dashboard/setup/login startup | Yes, explicit no security | B, M |
| `POST /api/settings/nim-keys` | Browser/operator API | Private | Post-setup | Session/header credentials; own keys for user, any key for admin | JSON | JSON | `setup_required`, `unauthorized`, `forbidden`, `invalid_config` | Success changes config bytes; rejection does not | Settings Access & keys | Yes | B, O, M |
| `POST /api/settings/clients` | Browser/operator API | Private | Post-setup | Session/header credentials; own keys for user, any key/mode for admin | JSON | JSON | `setup_required`, `unauthorized`, `forbidden`, `invalid_config` | Success changes config bytes; rejection does not | Settings Access & keys | Yes | B, O, M |
| `POST /api/settings/upstream` | Browser/operator API | Private | Post-setup | Session/header credentials; admin or superuser | JSON | JSON | `setup_required`, `unauthorized`, `forbidden`, `invalid_config` | Success changes config bytes; rejection does not | Settings Server | Yes | B, M |
| `POST /api/settings/limits` | Browser/operator API | Private | Post-setup | Session/header credentials; admin or superuser | JSON | JSON | `setup_required`, `unauthorized`, `forbidden`, `invalid_config` | Success changes config bytes; rejection does not | Settings Server | Yes | B, M |
| `POST /api/settings/history` | Browser/operator API | Private | Post-setup | Session/header credentials; admin or superuser | JSON | JSON | `setup_required`, `unauthorized`, `forbidden`, `invalid_config` | Success changes config bytes; rejection does not | Settings Server | Yes | B, M |
| `POST /api/settings/governor` | Browser/operator API | Private | Post-setup | Session/header credentials; admin or superuser | JSON | JSON | `setup_required`, `unauthorized`, `forbidden`, `invalid_config` | Success changes config bytes; rejection does not | Settings Server | Yes | B, M |
| `POST /api/settings/users` | Browser/operator API | Private | Post-setup | Session/header credentials; admin or superuser; superuser target protected | JSON | JSON | `setup_required`, `unauthorized`, `forbidden`, `weak_password`, `invalid_config` | Success changes config bytes; rejection does not | Settings Users | Yes | B, M |
| `POST /api/settings/account` | Browser/operator API | Private | Post-setup | Session/header credentials; any role, own account only | Password change or exact locale set/clear JSON; duplicate known fields fail as typed 422 before dispatch, while password-only unknown extensions remain ignored | JSON; password success also sets session cookie | `setup_required`, `unauthorized`, `invalid_json`, `wrong_password`, `weak_password`, `password_changed`, `invalid_action`, `invalid_locale`, `locale_not_installed`, `invalid_config` | Success changes only the caller's account bytes; password success sets a session cookie | Settings Account and dormant locale contract | Yes | B, M |
| `POST /api/settings/locale` | Browser/operator API | Private | Post-setup | Session/header credentials; admin or superuser; role check precedes locale validation | `SetServerLocale` JSON | `OkResponse` JSON | `setup_required`, `unauthorized`, `forbidden`, `invalid_locale`, `locale_not_installed`, `invalid_config` | Success changes server-default config bytes; rejection does not | Dormant; no visible selector | Yes | B, M |
| `POST /api/settings/validate-key` | Browser/operator API | Private | Post-setup | Session/header credentials; any role | JSON | JSON | `setup_required`, `unauthorized` | Success makes one upstream call; config unchanged | Settings key validation | Yes | B, M |
| `GET /health` | Orchestrator | Public | Always | None | None | Plain text | None | None | Container healthcheck | No | B, M |
| `GET /assets/public/public.css` | Browser | Public | Always | None | None | CSS | None | None | Setup/login pages | No | B, M |
| `GET /assets/public/setup.js` | Browser | Public | Always | None | None | JavaScript | None | None | Setup page | No | B, M |
| `GET /assets/public/login.js` | Browser | Public | Always | None | None | JavaScript | None | None | Login page | No | B, M |
| `GET /assets/public/locales/{locale}.json` | Browser | Public | Always | None | None | JSON catalog limited to `setup.*`, `login.*`, and `common.app_name` | Unknown locale 404 | None | Setup/login startup | No | B, M |
| `GET /login` | Browser operator | Public | Always | None; authenticated callers redirect | None | HTML/redirect | None (HTML flow) | None | Login page | No | B, M |
| `POST /login` | Browser operator | Public | Always | Username/password form | Form URL encoded | Redirect + session cookie | HTML 401 or plain 429; no `ApiError` code | Success sets a session cookie; config unchanged | Login form | No | B, M |
| `POST /logout` | Browser operator | Public | Always | None required | None | Redirect + clearing cookie | None | Response sets a clearing cookie; config unchanged | Account/logout control | No | B, M |
| `GET /setup` | First operator | Public | Pre-setup | None | None | HTML | Post-setup bare 404 | None | Setup wizard | No | B, M |
| `POST /setup` | First operator | Public | Pre-setup | None; first complete claim wins | JSON | JSON + session cookie | `weak_password`, `invalid_config`, `setup_complete` | Success creates config bytes and sets a session cookie | Setup wizard finish | Yes, explicit no security | B, M |
| `POST /setup/validate-key` | First operator | Public | Pre-setup | None; pre-auth throttle | JSON | JSON | `invalid_base_url`, `throttled`, `setup_complete` | Success makes one upstream call; config unchanged | Setup wizard key step | Yes, explicit no security | B, M |
| `ANY /v1/{*path}` (`POST /v1/chat/completions` probe) | OpenAI-compatible client | Client boundary: keyed private or explicitly open trusted-network mode | Post-setup | Client bearer in keyed mode; none in open mode | Upstream-owned bytes | Upstream content type or locally framed SSE | `setup_required`, `unauthorized`, `invalid_deadline`, `overloaded`, transport codes | Success makes one upstream call; config unchanged | External agent harness | No: upstream owns schema | B, C, M |

## Boundary ownership

Phase and operator authentication are owned by `require_session`; client
authentication and pre-setup data-plane closure are owned by the proxy handler.
Setup POST handlers perform their phase check before extraction. Handler role
and resource-ownership checks run only after the session boundary. The exact
identity and role rules remain in [client auth](client-auth.md).

All config writers follow build candidate → validate → persist → publish live
state. The E2E table therefore treats a rejected write as wrong if even one
durable byte changes. Upstream-only probes must change no config bytes. The
data-plane scheduling, response, and observation details remain in the
[streaming pipeline](streaming-pipeline.md).

Generated OpenAPI describes 14 `/api` operations and two setup POST
operations. The locale bootstrap is public with explicit `security: []`; the
other 13 `/api` operations inherit operator authentication. HTML/form,
presentation assets, health, metrics, and `/v1`
omissions are explicit
decisions. The generated-file authority and error schema remain in
[typed responses and generated OpenAPI](../decisions/typed-responses-and-generated-openapi.md).

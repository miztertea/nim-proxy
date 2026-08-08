//! The JSON wire format of the dashboard API, as types.
//!
//! Every `/api/*` (and setup) response body is a struct in this module with
//! `derive(Serialize, ToSchema)`. Two things follow from that: the shape a
//! handler returns is readable without executing it, and `openapi.json` is
//! generated from the same declarations the handlers serialize — there is no
//! second, hand-maintained description of the API to drift.
//!
//! # Field order is the wire order
//!
//! Serde emits struct fields in **declaration order**. The hand-built
//! `serde_json::json!` bodies these types replaced emitted keys in **ASCII
//! order**, because `serde_json::Map` is a `BTreeMap` unless the crate's
//! `preserve_order` feature is on (it is not, here). So every struct below
//! declares its fields in ASCII order, which is what keeps the bytes on the
//! wire byte-for-byte identical to the pre-0.6.6 responses.
//!
//! Note that `_` (0x5F) sorts *before* every lowercase letter: `lane` <
//! `last4`, `username` < `users`. `api::field_order` guards the rule so a
//! future edit that reorders a field trips a test instead of a client.
//!
//! See `knowledge/decisions/typed-responses-and-generated-openapi.md`.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, FromRequestParts, Request, State};
use axum::http::{header, request::Parts, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::Arc;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};

use crate::config::{DashboardCfg, GovernorCfg, Limits, Mode, Role, StoredConfig};
use crate::history::{HistoryDiagnostics, MetricValue, RollupPoint, Tail};
use crate::AppState;

// ---------------------------------------------------------------------------
// Shared envelopes
// ---------------------------------------------------------------------------

/// The single error envelope every JSON endpoint answers failures with —
/// the same shape `/v1` uses, so one client-side branch handles both.
#[derive(Serialize, ToSchema)]
pub struct ApiError {
    pub error: ApiErrorBody,
}

#[derive(Serialize, ToSchema)]
pub struct ApiErrorBody {
    /// Machine-readable reason, e.g. `invalid_config`, `forbidden`.
    pub code: String,
    /// Operator-facing explanation; safe to show in the UI.
    pub message: String,
    /// Always `proxy_error` — the error came from nim-proxy, not upstream.
    #[serde(rename = "type")]
    #[schema(rename = "type", example = "proxy_error")]
    pub kind: &'static str,
}

impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            error: ApiErrorBody {
                code: code.to_owned(),
                message: message.into(),
                kind: "proxy_error",
            },
        }
    }
}

/// A JSON extractor whose failures use the dashboard API's stable error
/// envelope instead of Axum's framework-default text responses.
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(JsonRejection::MissingJsonContentType(_)) => Err(api_error_response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                "Content-Type must be application/json",
            )),
            Err(JsonRejection::BytesRejection(rejection)) => {
                let (status, code, message) = if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE
                {
                    (
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "body_too_large",
                        "request body is too large",
                    )
                } else {
                    (StatusCode::BAD_REQUEST, "invalid_json", "invalid JSON")
                };
                Err(api_error_response(status, code, message))
            }
            Err(JsonRejection::JsonDataError(_)) => Err(api_error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_json",
                "invalid JSON",
            )),
            Err(_) => Err(api_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                "invalid JSON",
            )),
        }
    }
}

/// A query extractor whose failures use the dashboard API's stable error
/// envelope instead of Axum's framework-default text responses.
pub struct ApiQuery<T>(pub T);

impl<T, S> FromRequestParts<S> for ApiQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(value)) => Ok(Self(value)),
            Err(_) => Err(api_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_query",
                "invalid query",
            )),
        }
    }
}

fn api_error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (status, axum::Json(ApiError::new(code, message))).into_response()
}

/// The protected `/api/*` fallback. It is deliberately not installed on page,
/// health, metrics, login/form, setup GET, or `/v1` routes.
pub async fn api_not_found() -> Response {
    api_error_response(StatusCode::NOT_FOUND, "not_found", "not found")
}

/// The protected `/api/*` method fallback. It has the same narrow scope as
/// [`api_not_found`].
pub async fn api_method_not_allowed() -> Response {
    api_error_response(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "method not allowed",
    )
}

/// A settings write that applied. Success carries no data: the dashboard
/// re-reads `/api/config` rather than trusting an echo.
#[derive(Serialize, ToSchema)]
pub struct OkResponse {
    /// Always `true`; failures are an [`ApiError`] with a 4xx status.
    pub ok: bool,
}

impl OkResponse {
    pub fn new() -> Self {
        Self { ok: true }
    }
}

mod locale_bootstrap {
    use super::*;

    /// Public startup data: the installed presentation locales and the
    /// persisted server default.
    #[derive(Serialize, ToSchema)]
    pub(crate) struct LocaleBootstrap {
        installed_locales: Vec<String>,
        server_default: String,
    }

    impl LocaleBootstrap {
        pub(super) fn from_config(stored: &StoredConfig, installed: &[&str]) -> Self {
            Self {
                installed_locales: installed
                    .iter()
                    .map(|locale| (*locale).to_owned())
                    .collect(),
                server_default: stored.default_locale.clone(),
            }
        }
    }
}

pub(crate) use locale_bootstrap::LocaleBootstrap;

#[utoipa::path(
    get,
    path = "/api/locale-bootstrap",
    operation_id = "api_locale_bootstrap",
    tag = "localization",
    security(),
    responses(
        (status = 200, description = "Installed locales and the server default", body = LocaleBootstrap),
    ),
)]
pub(crate) async fn locale_bootstrap(
    State(state): State<Arc<AppState>>,
) -> (
    [(header::HeaderName, &'static str); 1],
    axum::Json<LocaleBootstrap>,
) {
    let stored = state.store.lock().unwrap();
    (
        [(header::CACHE_CONTROL, "no-store")],
        axum::Json(LocaleBootstrap::from_config(
            &stored,
            &crate::presentation::INSTALLED_LOCALES,
        )),
    )
}

// ---------------------------------------------------------------------------
// GET /api/config
// ---------------------------------------------------------------------------

/// The Settings page's data source. `server` and `users` are absent — not
/// null — for non-admin callers: the filtering happens before serialization,
/// so DOM tampering reveals nothing.
#[derive(Serialize, ToSchema)]
pub struct ConfigResponse {
    pub client_keys: Vec<ClientKeyRow>,
    #[schema(required = true)]
    pub locale: Option<String>,
    pub mode: Mode,
    pub nim_keys: Vec<NimKeyRow>,
    pub pool: PoolSummary,
    pub role: Role,
    /// Admin-only; omitted entirely for the `user` role.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub server: Option<ServerSettings>,
    pub username: String,
    /// Admin-only; omitted entirely for the `user` role.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub users: Option<Vec<UserRow>>,
}

/// A stored client key. The secret itself is never served back — only the
/// masked tail kept for display.
#[derive(Serialize, ToSchema)]
pub struct ClientKeyRow {
    pub last4: String,
    pub name: String,
    pub owner: String,
}

/// A stored NIM key plus its live lane state. `lane`, `in_window` and
/// `cooldown_ms` are null for a disabled key (it holds no lane).
#[derive(Serialize, ToSchema)]
pub struct NimKeyRow {
    pub cooldown_ms: Option<u64>,
    pub enabled: bool,
    /// First 8 hex chars of SHA-256(key) — the key's public identifier.
    pub fingerprint: String,
    /// True for the superuser's only enabled key: the pool floor, which
    /// `config::validate` refuses to remove or disable.
    pub guarded: bool,
    pub in_window: Option<usize>,
    pub lane: Option<usize>,
    pub last4: String,
    pub owner: String,
    pub rpm: usize,
}

/// Pool aggregate — visible to every role, since it carries no ownership.
#[derive(Serialize, ToSchema)]
pub struct PoolSummary {
    pub capacity_rpm: usize,
    pub enabled: usize,
}

/// Admin-only server settings, mirroring the stored config sections.
#[derive(Serialize, ToSchema)]
pub struct ServerSettings {
    pub base_url: String,
    pub dashboard: DashboardCfg,
    pub default_locale: String,
    pub governor: GovernorCfg,
    pub history: HistorySettings,
    pub limits: Limits,
}

/// Retention setting plus the live state of the history file.
#[derive(Serialize, ToSchema)]
pub struct HistorySettings {
    pub available_from: Option<u64>,
    pub compaction_pending: bool,
    /// Retention in days; 0 = keep forever.
    pub days: u64,
    pub file_bytes: u64,
    pub persistence: String,
}

/// A user row with how much of the pool they own.
#[derive(Serialize, ToSchema)]
pub struct UserRow {
    pub client_keys: usize,
    pub nim_keys: usize,
    pub role: Role,
    pub username: String,
}

// ---------------------------------------------------------------------------
// POST /api/settings/*
// ---------------------------------------------------------------------------

/// `POST /api/settings/clients`. `secret` appears only on `add`, and only
/// this once — the store keeps a SHA-256 digest, never the secret.
#[derive(Serialize, ToSchema)]
pub struct ClientsResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub secret: Option<String>,
}

/// `POST /api/settings/validate-key` and `POST /setup/validate-key`.
/// `models` is the size of the upstream catalog the key could read;
/// `error` explains a refusal. Exactly one of the two is present.
#[derive(Serialize, ToSchema)]
pub struct ValidateKeyResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub models: Option<usize>,
    pub ok: bool,
}

impl ValidateKeyResponse {
    pub fn probed(result: Result<usize, String>) -> Self {
        match result {
            Ok(models) => Self {
                error: None,
                models: Some(models),
                ok: true,
            },
            Err(error) => Self {
                error: Some(error),
                models: None,
                ok: false,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// POST /setup
// ---------------------------------------------------------------------------

/// The first-run claim. `client_key` is present when the wizard asked for a
/// first client key — the one and only time that secret leaves the server.
#[derive(Serialize, ToSchema)]
pub struct SetupResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub client_key: Option<MintedClientKey>,
    pub ok: bool,
}

#[derive(Serialize, ToSchema)]
pub struct MintedClientKey {
    pub name: String,
    pub secret: String,
}

// ---------------------------------------------------------------------------
// GET /api/dashboard, GET /api/dashboard/now
// ---------------------------------------------------------------------------

/// Rolled-up history for one analytical window. `config_revision` and
/// `history_revision` let the dashboard skip a redraw when nothing moved.
#[derive(Serialize, ToSchema)]
pub struct DashboardResponse {
    pub config_revision: u64,
    pub diagnostics: HistoryDiagnostics,
    pub history_revision: u64,
    /// Latest value of each gauge in the window.
    pub latest: Vec<MetricValue>,
    pub points: Vec<RollupPoint>,
    /// Counter deltas summed across the window.
    pub totals: Vec<MetricValue>,
    pub window: DashboardWindow,
}

/// What was asked for, what could be served, and what exists on disk —
/// kept separate so the UI can say "you asked for 30d, I have 3d".
#[derive(Serialize, ToSchema)]
pub struct DashboardWindow {
    pub available_from: Option<u64>,
    pub available_to: Option<u64>,
    pub complete: bool,
    pub default_window_days: u64,
    pub effective_from: Option<u64>,
    pub effective_to: Option<u64>,
    /// True when the caller sent no `to`, i.e. the window tracks now.
    pub following_now: bool,
    pub requested_from: u64,
    pub requested_to: u64,
    pub retention_days: u64,
}

/// The "Now" strip: live registry values plus the configuration they were
/// sampled under. Deliberately not a slice of history — it is this instant.
#[derive(Serialize, ToSchema)]
pub struct DashboardNowResponse {
    /// True when `/v1` requires a client key (keyed mode).
    pub auth: bool,
    pub available_from: Option<u64>,
    pub available_to: Option<u64>,
    pub capacity_rpm: usize,
    pub config_revision: u64,
    pub default_window_days: u64,
    pub history_revision: u64,
    pub lanes: usize,
    pub metrics: Vec<MetricValue>,
    pub retention_days: u64,
    pub rpms: Vec<usize>,
    pub sampled_at: u64,
    pub slo_target_percent: f64,
    /// Unix time this process started.
    pub started: u64,
    /// Counters accumulated since the last persisted sample.
    pub tail: Tail,
    pub version: String,
}

// ---------------------------------------------------------------------------
// The generated spec
// ---------------------------------------------------------------------------

/// Auth schemes: a session cookie for browsers, HTTP Basic (or
/// `Bearer user:password`) for Prometheus scrapers. Either satisfies the
/// `require_session` guard, so they are listed as alternatives.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi
            .components
            .as_mut()
            .expect("components exist: every path declares a response schema");
        components.add_security_scheme(
            "session_cookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "nimproxy_session",
                "HMAC-signed session cookie minted by POST /login. The signing key is \
                 regenerated every boot, so a restart invalidates all sessions.",
            ))),
        );
        components.add_security_scheme(
            "basic_auth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Basic)
                    .description(Some(
                        "Dashboard username and password, for non-browser clients such as a \
                         Prometheus scraper. `Authorization: Bearer <user>:<password>` is \
                         accepted equivalently.",
                    ))
                    .build(),
            ),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "nim-proxy dashboard API",
        description = "The operator surface of nim-proxy: the settings store, user and key \
                       management, and the metrics history the dashboard renders. This document \
                       is generated from the handlers; regenerate it with \
                       `UPDATE_OPENAPI=1 cargo test --test openapi`.\n\n\
                       Not covered: the OpenAI-compatible `/v1` passthrough (that contract is \
                       the upstream's, not nim-proxy's), the HTML page routes, the form-encoded \
                       `/login` and `/logout` browser flow, plain-text `/health`, and the \
                       Prometheus exposition at `/metrics`.",
        license(name = "MIT", url = "https://github.com/miztertea/nim-proxy/blob/main/LICENSE"),
        contact(name = "nim-proxy", url = "https://github.com/miztertea/nim-proxy"),
    ),
    paths(
        crate::api_dashboard,
        crate::api_dashboard_now,
        crate::api::locale_bootstrap,
        crate::settings::api_config,
        crate::settings::nim_keys,
        crate::settings::clients,
        crate::settings::upstream,
        crate::settings::limits,
        crate::settings::history,
        crate::settings::governor_cfg,
        crate::settings::users,
        crate::settings::account,
        crate::settings::locale,
        crate::settings::validate_key,
        crate::settings::setup_submit,
        crate::settings::setup_validate_key,
    ),
    components(schemas(
        ApiError,
        ApiErrorBody,
        ClientKeyRow,
        ClientsResponse,
        ConfigResponse,
        DashboardCfg,
        DashboardNowResponse,
        DashboardResponse,
        DashboardWindow,
        GovernorCfg,
        HistoryDiagnostics,
        HistorySettings,
        Limits,
        LocaleBootstrap,
        MetricValue,
        MintedClientKey,
        Mode,
        NimKeyRow,
        OkResponse,
        PoolSummary,
        Role,
        RollupPoint,
        ServerSettings,
        SetupResponse,
        Tail,
        UserRow,
        ValidateKeyResponse,
    )),
    // Applies to every operation that does not override it. The two `/setup`
    // routes declare `security()` — an explicit empty list, i.e. "no auth" —
    // because they run before any user exists.
    security(
        ("session_cookie" = []),
        ("basic_auth" = []),
    ),
    modifiers(&SecurityAddon),
    tags(
        (name = "dashboard", description = "Read-only telemetry the operator console renders."),
        (name = "settings", description = "The config store's only writers. Every write is \
            validate -> persist -> swap; a failed disk write applies nothing."),
        (name = "setup", description = "First-run claim. **Unauthenticated** — these routes sit \
            outside the session guard because no user exists yet. They answer 404 the moment a \
            superuser exists, so the window is exactly one claim wide."),
    ),
)]
pub struct ApiDoc;

/// The committed spec, pretty-printed with a trailing newline. `openapi.json`
/// at the repo root is exactly this string; CI regenerates and diffs.
pub fn openapi_json() -> String {
    let mut s = ApiDoc::openapi()
        .to_pretty_json()
        .expect("the generated spec serializes");
    s.push('\n');
    s
}

/// Serialized key order for a value, top level only.
#[cfg(test)]
fn keys_of<T: Serialize>(value: &T) -> Vec<String> {
    match serde_json::to_value(value).unwrap() {
        serde_json::Value::Object(map) => map.keys().cloned().collect(),
        other => panic!("expected an object, got {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    fn metric_for(metric: &str, model: &str, value: f64) -> MetricValue {
        MetricValue {
            labels: BTreeMap::from([
                ("client".into(), "fixture-client".into()),
                ("model".into(), model.into()),
                ("path".into(), "/v1/chat/completions".into()),
                ("status".into(), "200".into()),
            ]),
            metric: metric.into(),
            value,
        }
    }

    fn active_requests(value: f64) -> MetricValue {
        MetricValue {
            labels: BTreeMap::new(),
            metric: "nimproxy_active_requests".into(),
            value,
        }
    }

    fn labeled_metric(metric: &str, labels: &[(&str, &str)], value: f64) -> MetricValue {
        MetricValue {
            labels: labels
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect(),
            metric: metric.into(),
            value,
        }
    }

    // Keep the fixture histogram shape tied to the recorder configured in
    // `lib.rs`. A generic "fast/slow/+Inf" shape would exercise a dashboard
    // that the production recorder can never emit.
    fn histogram_bounds(metric: &str) -> &'static [f64] {
        crate::HISTOGRAM_BUCKETS
            .iter()
            .find_map(|(name, bounds)| (*name == metric).then_some(*bounds))
            .unwrap_or_else(|| panic!("ui-fixture: unknown production histogram {metric}"))
    }

    #[derive(Clone, Copy)]
    enum FixtureSegment {
        All,
        First,
        Second,
    }

    fn histogram_samples(metric: &str, segment: FixtureSegment) -> &'static [f64] {
        let all = match metric {
            "nimproxy_ttft_seconds" => &[0.03, 0.07, 0.2, 0.4, 0.8, 1.5, 3.0, 7.0, 12.0, 20.0][..],
            "nimproxy_tokens_per_second" => {
                &[0.5, 1.5, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0, 400.0][..]
            }
            "nimproxy_queue_wait_seconds" => {
                &[0.0005, 0.02, 0.1, 0.5, 2.0, 10.0, 30.0, 90.0, 300.0, 500.0][..]
            }
            "nimproxy_upstream_seconds" => {
                &[0.1, 0.3, 0.7, 1.5, 3.0, 7.0, 15.0, 45.0, 100.0, 250.0][..]
            }
            "nimproxy_tpot_seconds" => {
                &[0.003, 0.008, 0.015, 0.03, 0.06, 0.12, 0.24, 0.4, 0.5, 0.6][..]
            }
            _ => panic!("ui-fixture: unknown production histogram {metric}"),
        };
        match segment {
            FixtureSegment::All => all,
            FixtureSegment::First => &all[..5],
            FixtureSegment::Second => &all[5..],
        }
    }

    fn histogram(metric: &str, labels: &[(&str, &str)], samples: &[f64]) -> Vec<MetricValue> {
        let mut values = Vec::new();
        // A series comes from a small concrete set of observations. This ties
        // bucket/count/sum values together and keeps each fixture physically
        // possible under the production recorder's configured bounds.
        for bound in histogram_bounds(metric) {
            let mut bucket_labels = labels.to_vec();
            let bound_text = bound.to_string();
            bucket_labels.push(("le", &bound_text));
            values.push(labeled_metric(
                &format!("{metric}_bucket"),
                &bucket_labels,
                samples.iter().filter(|sample| **sample <= *bound).count() as f64,
            ));
        }
        let mut terminal_labels = labels.to_vec();
        terminal_labels.push(("le", "+Inf"));
        values.push(labeled_metric(
            &format!("{metric}_bucket"),
            &terminal_labels,
            samples.len() as f64,
        ));
        values.push(labeled_metric(
            &format!("{metric}_count"),
            labels,
            samples.len() as f64,
        ));
        values.push(labeled_metric(
            &format!("{metric}_sum"),
            labels,
            samples.iter().sum(),
        ));
        values
    }

    fn fixture_delta(total: f64, segment: FixtureSegment, is_sum: bool) -> f64 {
        match segment {
            FixtureSegment::All => total,
            FixtureSegment::First if is_sum => total / 2.0,
            FixtureSegment::First => (total as u64 / 2) as f64,
            FixtureSegment::Second if is_sum => total / 2.0,
            FixtureSegment::Second => total - (total as u64 / 2) as f64,
        }
    }

    fn representative_dashboard_metrics(segment: FixtureSegment) -> Vec<MetricValue> {
        let mut values = vec![
            metric_for("nimproxy_requests_total", "fixture/alpha", 30.0),
            labeled_metric(
                "nimproxy_requests_total",
                &[
                    ("client", "fixture-client"),
                    ("model", "fixture/zeta"),
                    ("path", "/v1/chat/completions"),
                    ("status", "504"),
                ],
                3.0,
            ),
            labeled_metric(
                "nimproxy_prompt_tokens_total",
                &[("client", "fixture-client"), ("model", "fixture/alpha")],
                1_200.0,
            ),
            labeled_metric(
                "nimproxy_completion_tokens_total",
                &[
                    ("client", "fixture-client"),
                    ("model", "fixture/alpha"),
                    ("source", "usage"),
                ],
                300.0,
            ),
            labeled_metric(
                "nimproxy_reasoning_tokens_total",
                &[("model", "fixture/alpha")],
                60.0,
            ),
            labeled_metric(
                "nimproxy_finish_reason_total",
                &[("model", "fixture/alpha"), ("reason", "stop")],
                25.0,
            ),
            labeled_metric(
                "nimproxy_stream_requests_total",
                &[("client", "fixture-client"), ("stream", "true")],
                20.0,
            ),
            labeled_metric(
                "nimproxy_tool_calls_total",
                &[("model", "fixture/alpha")],
                8.0,
            ),
            labeled_metric("nimproxy_tool_choice_total", &[("mode", "auto")], 10.0),
            labeled_metric(
                "nimproxy_json_mode_total",
                &[("client", "fixture-client")],
                4.0,
            ),
            labeled_metric("nimproxy_affinity_total", &[("result", "sticky")], 24.0),
            labeled_metric("nimproxy_affinity_total", &[("result", "spill")], 2.0),
            labeled_metric("nimproxy_lane_requests_total", &[("lane", "0")], 33.0),
            labeled_metric(
                "nimproxy_lane_cooldown_total",
                &[("lane", "0"), ("status", "429")],
                2.0,
            ),
            labeled_metric(
                "nimproxy_worker_exhausted_total",
                &[("model", "fixture/alpha")],
                1.0,
            ),
            labeled_metric("nimproxy_shed_total", &[], 1.0),
            labeled_metric("nimproxy_unauthorized_total", &[], 1.0),
            labeled_metric("nimproxy_login_failures_total", &[], 1.0),
            labeled_metric(
                "nimproxy_request_tools_count",
                &[("client", "fixture-client")],
                10.0,
            ),
            labeled_metric(
                "nimproxy_request_tools_sum",
                &[("client", "fixture-client")],
                15.0,
            ),
            labeled_metric(
                "nimproxy_request_messages_count",
                &[("client", "fixture-client")],
                10.0,
            ),
            labeled_metric(
                "nimproxy_request_messages_sum",
                &[("client", "fixture-client")],
                40.0,
            ),
            labeled_metric(
                "nimproxy_request_temperature_count",
                &[("client", "fixture-client")],
                10.0,
            ),
            labeled_metric(
                "nimproxy_request_temperature_sum",
                &[("client", "fixture-client")],
                7.0,
            ),
            labeled_metric(
                "nimproxy_request_max_tokens_count",
                &[("client", "fixture-client")],
                10.0,
            ),
            labeled_metric(
                "nimproxy_request_max_tokens_sum",
                &[("client", "fixture-client")],
                10_240.0,
            ),
        ];
        values.extend(
            [
                "prompt_tokens",
                "completion_tokens",
                "total_tokens",
                "cached_tokens",
                "reasoning_tokens",
            ]
            .into_iter()
            .map(|field| {
                labeled_metric(
                    "nimproxy_usage_observations_total",
                    &[("field", field), ("result", "measured")],
                    10.0,
                )
            }),
        );
        for metric in &mut values {
            metric.value = fixture_delta(metric.value, segment, metric.metric.ends_with("_sum"));
        }
        values.extend(histogram(
            "nimproxy_ttft_seconds",
            &[("model", "fixture/alpha")],
            histogram_samples("nimproxy_ttft_seconds", segment),
        ));
        values.extend(histogram(
            "nimproxy_tokens_per_second",
            &[("model", "fixture/alpha"), ("source", "usage")],
            histogram_samples("nimproxy_tokens_per_second", segment),
        ));
        values.extend(histogram(
            "nimproxy_tpot_seconds",
            &[("model", "fixture/alpha")],
            histogram_samples("nimproxy_tpot_seconds", segment),
        ));
        values.extend(histogram(
            "nimproxy_upstream_seconds",
            &[("model", "fixture/alpha")],
            histogram_samples("nimproxy_upstream_seconds", segment),
        ));
        values.extend(histogram(
            "nimproxy_queue_wait_seconds",
            &[],
            histogram_samples("nimproxy_queue_wait_seconds", segment),
        ));
        values
    }

    fn observation_quality_fixture_metrics(results: &[(&str, f64)]) -> Vec<MetricValue> {
        results
            .iter()
            .flat_map(|(result, value)| {
                [
                    "prompt_tokens",
                    "completion_tokens",
                    "total_tokens",
                    "cached_tokens",
                    "reasoning_tokens",
                ]
                .into_iter()
                .map(move |field| {
                    labeled_metric(
                        "nimproxy_usage_observations_total",
                        &[("field", field), ("result", result)],
                        *value,
                    )
                })
            })
            .collect()
    }

    fn assert_representative_metrics_are_production_faithful(range: &DashboardResponse) {
        let histogram_families = [
            "nimproxy_ttft_seconds",
            "nimproxy_tokens_per_second",
            "nimproxy_queue_wait_seconds",
            "nimproxy_upstream_seconds",
            "nimproxy_tpot_seconds",
        ];
        for family in histogram_families {
            let bucket_name = format!("{family}_bucket");
            let count_name = format!("{family}_count");
            let expected_bounds: Vec<_> = histogram_bounds(family)
                .iter()
                .map(ToString::to_string)
                .chain(std::iter::once("+Inf".to_owned()))
                .collect();
            let buckets: Vec<_> = range
                .totals
                .iter()
                .filter(|metric| metric.metric == bucket_name)
                .collect();
            let actual_bounds: Vec<_> = buckets
                .iter()
                .map(|metric| metric.labels.get("le").map(String::as_str))
                .collect();
            assert_eq!(
                actual_bounds,
                expected_bounds
                    .iter()
                    .map(|bound| Some(bound.as_str()))
                    .collect::<Vec<_>>(),
                "ui-fixture:{family}: bucket bounds must match the production recorder"
            );
            assert!(
                buckets
                    .windows(2)
                    .all(|pair| pair[0].value <= pair[1].value),
                "ui-fixture:{family}: cumulative buckets must be monotonic"
            );
            assert!(
                buckets.iter().all(|metric| metric.value.fract() == 0.0),
                "ui-fixture:{family}: bucket counts must be integral"
            );
            let mut series_labels = buckets
                .first()
                .expect("ui-fixture: representative histogram has buckets")
                .labels
                .clone();
            series_labels.remove("le");
            let count = range
                .totals
                .iter()
                .find(|metric| metric.metric == count_name && metric.labels == series_labels)
                .expect("ui-fixture: representative histogram has count");
            assert_eq!(
                buckets.last().expect("ui-fixture: terminal bucket").value,
                count.value,
                "ui-fixture:{family}: +Inf bucket must equal count"
            );
        }
        for point in &range.points {
            for metric in &point.values {
                if metric.metric.ends_with("_total")
                    || metric.metric.ends_with("_count")
                    || metric.metric.ends_with("_bucket")
                {
                    assert_eq!(
                        metric.value.fract(),
                        0.0,
                        "ui-fixture:{}: point counters/counts/buckets must be integral",
                        metric.metric
                    );
                }
            }
        }
        for total in &range.totals {
            let point_total: f64 = range
                .points
                .iter()
                .map(|point| {
                    point
                        .values
                        .iter()
                        .find(|metric| {
                            metric.metric == total.metric && metric.labels == total.labels
                        })
                        .expect("ui-fixture: every point has every total series")
                        .value
                })
                .sum();
            assert_eq!(
                point_total, total.value,
                "ui-fixture:{}: point deltas must sum exactly to range total",
                total.metric
            );
        }
        for metric in [
            "nimproxy_tokens_per_second_bucket",
            "nimproxy_tpot_seconds_bucket",
        ] {
            assert!(
                range
                    .points
                    .iter()
                    .all(|point| point.values.iter().any(|value| value.metric == metric)),
                "ui-fixture:{metric}: every point must drive its dashboard chart"
            );
        }
    }

    fn representative_latest_metrics() -> Vec<MetricValue> {
        vec![
            active_requests(2.0),
            labeled_metric("nimproxy_queue_depth", &[], 1.0),
            labeled_metric(
                "nimproxy_model_inflight",
                &[("model", "fixture/alpha")],
                2.0,
            ),
            labeled_metric("nimproxy_model_limit", &[("model", "fixture/alpha")], 8.0),
        ]
    }

    fn dashboard_ui_fixture(kind: &str) -> DashboardResponse {
        let empty = kind == "empty";
        let partial = kind == "partial" || kind == "observation-incomplete";
        let unavailable = kind == "unavailable";
        let outside_before = kind == "outside-before";
        let outside_after = kind == "outside-after";
        let extreme = kind == "extreme";
        let long = kind == "long";
        let observation_results: Option<&[(&str, f64)]> = match kind {
            "observation-estimated" => Some(&[("estimated", 9.0)]),
            "observation-unavailable" => Some(&[("unavailable", 9.0)]),
            "observation-invalid" => Some(&[("invalid", 9.0)]),
            "observation-incomplete" | "observation-measured" => Some(&[("measured", 9.0)]),
            "observation-zero" => Some(&[("measured", 0.0)]),
            "observation-tie-invalid" => Some(&[
                ("invalid", 9.0),
                ("unavailable", 9.0),
                ("estimated", 9.0),
                ("measured", 9.0),
            ]),
            "observation-tie-unavailable" => {
                Some(&[("unavailable", 9.0), ("estimated", 9.0), ("measured", 9.0)])
            }
            "observation-tie-estimated" => Some(&[("estimated", 9.0), ("measured", 9.0)]),
            _ => None,
        };
        let observation_absent = kind == "observation-absent";
        let mut values = if empty || unavailable || outside_before || outside_after {
            Vec::new()
        } else if long {
            let client = format!("client-fixture-{}", "c".repeat(49));
            let publisher = format!("publisher-fixture-{}", "p".repeat(20));
            let model = format!("{publisher}/model-{}", "m".repeat(19));
            assert_eq!(client.len(), 64);
            assert_eq!(model.len(), 64);
            vec![MetricValue {
                labels: BTreeMap::from([
                    ("client".into(), client),
                    ("model".into(), model),
                    ("path".into(), "/v1/chat/completions".into()),
                    ("status".into(), "200".into()),
                ]),
                metric: "nimproxy_requests_total".into(),
                value: 3.0,
            }]
        } else {
            let value = if extreme { f64::MAX / 4.0 } else { 3.0 };
            if extreme {
                vec![
                    metric_for("nimproxy_requests_total", "fixture/alpha", value),
                    metric_for("nimproxy_requests_total", "fixture/zeta", value),
                ]
            } else {
                representative_dashboard_metrics(FixtureSegment::All)
            }
        };
        if observation_absent || observation_results.is_some() {
            values.retain(|metric| metric.metric != "nimproxy_usage_observations_total");
            if let Some(results) = observation_results {
                values.extend(observation_quality_fixture_metrics(results));
            }
        }
        let available_from = if partial {
            1_699_913_600
        } else {
            1_697_411_600
        };
        let effective_from = available_from;
        let effective_to = 1_700_003_600;
        let requested_from = if unavailable {
            1_699_000_000
        } else if outside_before {
            1_697_000_000
        } else if outside_after {
            1_700_100_000
        } else {
            1_697_411_600
        };
        let requested_to = if unavailable {
            1_699_003_600
        } else if outside_before {
            1_697_003_600
        } else if outside_after {
            1_700_103_600
        } else {
            effective_to
        };
        let point = |from, to, point_values| RollupPoint {
            capacity: Some(crate::history::CapacityRollup {
                average_rpm: 80.0,
                latest_rpms: vec![40, 40],
            }),
            duration_seconds: to - from,
            from,
            to,
            values: point_values,
        };
        let points = if empty || unavailable || outside_before || outside_after {
            Vec::new()
        } else if extreme || long || observation_absent || observation_results.is_some() {
            vec![point(effective_from, effective_to, values.clone())]
        } else {
            let midpoint = effective_from + (effective_to - effective_from) / 2;
            vec![
                point(
                    effective_from,
                    midpoint,
                    representative_dashboard_metrics(FixtureSegment::First),
                ),
                point(
                    midpoint,
                    effective_to,
                    representative_dashboard_metrics(FixtureSegment::Second),
                ),
            ]
        };
        DashboardResponse {
            config_revision: 7,
            diagnostics: if unavailable {
                HistoryDiagnostics {
                    excluded_epochs: 1,
                    excluded_records: 3,
                    normalized_series: 1,
                    skipped_metric_lines: 0,
                    valid_checkpoints: 0,
                    valid_samples: 1,
                }
            } else {
                HistoryDiagnostics {
                    excluded_epochs: usize::from(partial),
                    excluded_records: usize::from(partial),
                    normalized_series: usize::from(!empty),
                    skipped_metric_lines: usize::from(partial),
                    valid_checkpoints: 0,
                    valid_samples: usize::from(!empty),
                }
            },
            history_revision: 11,
            latest: if empty || unavailable || outside_before || outside_after {
                Vec::new()
            } else if extreme {
                vec![active_requests(f64::MAX / 4.0)]
            } else {
                representative_latest_metrics()
            },
            points,
            totals: values,
            window: DashboardWindow {
                available_from: (!empty).then_some(available_from),
                available_to: (!empty).then_some(effective_to),
                complete: !empty && !partial && !unavailable && !outside_before && !outside_after,
                default_window_days: 30,
                effective_from: (!empty && !unavailable && !outside_before && !outside_after)
                    .then_some(effective_from),
                effective_to: (!empty && !unavailable && !outside_before && !outside_after)
                    .then_some(effective_to),
                following_now: false,
                requested_from,
                requested_to,
                retention_days: 30,
            },
        }
    }

    fn dashboard_window_ui_fixture(requested_from: u64, requested_to: u64) -> DashboardResponse {
        let mut response = dashboard_ui_fixture("healthy");
        response.window.effective_from = Some(requested_from);
        response.window.effective_to = Some(requested_to);
        response.window.requested_from = requested_from;
        response.window.requested_to = requested_to;
        let midpoint = requested_from + (requested_to - requested_from) / 2;
        for (point, (from, to)) in response
            .points
            .iter_mut()
            .zip([(requested_from, midpoint), (midpoint, requested_to)])
        {
            point.from = from;
            point.to = to;
            point.duration_seconds = to - from;
        }
        response
    }

    fn server_ui_fixture() -> ServerSettings {
        ServerSettings {
            base_url: "https://integrate.api.nvidia.com".into(),
            dashboard: DashboardCfg::default(),
            default_locale: "en-US".into(),
            governor: GovernorCfg {
                enabled: true,
                overrides: BTreeMap::from([("fixture/model".into(), 8)]),
            },
            history: HistorySettings {
                available_from: Some(1_700_000_000),
                compaction_pending: true,
                days: 30,
                file_bytes: 12_345,
                persistence: "degraded".into(),
            },
            limits: Limits::default(),
        }
    }

    fn config_ui_fixture(role: Role, scenario: &str) -> ConfigResponse {
        let username = match role {
            Role::Superuser => "fixture-superuser",
            Role::Admin => "fixture-admin",
            Role::User => "fixture-user",
        };
        // Build the full state first, mutate it, then derive the same
        // aggregates/filtering that api_config derives from StoredConfig.
        let mut response = ConfigResponse {
            client_keys: vec![ClientKeyRow {
                last4: "abcd".into(),
                name: "fixture-client".into(),
                owner: "fixture-user".into(),
            }],
            locale: Some("en-US".into()),
            mode: Mode::Keyed,
            nim_keys: vec![
                NimKeyRow {
                    cooldown_ms: Some(0),
                    enabled: true,
                    fingerprint: "f17e0001".into(),
                    guarded: false,
                    in_window: Some(1),
                    lane: Some(0),
                    last4: "wxyz".into(),
                    owner: "fixture-user".into(),
                    rpm: 40,
                },
                NimKeyRow {
                    cooldown_ms: Some(0),
                    enabled: true,
                    fingerprint: "5a000001".into(),
                    guarded: false,
                    in_window: Some(0),
                    lane: Some(1),
                    last4: "root".into(),
                    owner: "fixture-superuser".into(),
                    rpm: 40,
                },
            ],
            pool: PoolSummary {
                capacity_rpm: 0,
                enabled: 0,
            },
            role,
            server: Some(server_ui_fixture()),
            username: username.into(),
            users: Some({
                let mut users = vec![UserRow {
                    client_keys: 0,
                    nim_keys: 0,
                    role: Role::Superuser,
                    username: "fixture-superuser".into(),
                }];
                if role == Role::Admin {
                    users.push(UserRow {
                        client_keys: 0,
                        nim_keys: 0,
                        role: Role::Admin,
                        username: "fixture-admin".into(),
                    });
                }
                users.push(UserRow {
                    client_keys: 0,
                    nim_keys: 0,
                    role: Role::User,
                    username: "fixture-user".into(),
                });
                users
            }),
        };
        match scenario {
            "base"
            | "access-refreshed"
            | "auth-keyed-after"
            | "governor-enabled-before"
            | "history-before"
            | "limits-before"
            | "users-role-before"
            | "users-delete-before" => {}
            "access-before" => response.nim_keys[0].in_window = Some(0),
            "nim-create-before" => {
                response.nim_keys.remove(0);
            }
            "nim-created-after" => {
                response.nim_keys[0].owner = username.into();
                response.nim_keys.swap(0, 1);
            }
            "nim-rpm-after" => {
                response.nim_keys[0].rpm = 41;
            }
            "nim-disabled-after" => {
                let key = &mut response.nim_keys[0];
                key.cooldown_ms = None;
                key.enabled = false;
                key.in_window = None;
                key.lane = None;
            }
            "nim-deleted-after" => {
                response.nim_keys.remove(0);
            }
            "client-deleted-after" => response.client_keys.clear(),
            "client-create-before" => response.client_keys.clear(),
            "client-created-after" => {
                response.client_keys[0].owner = username.into();
            }
            "auth-open-before" => response.mode = Mode::Open,
            "override-create-before" | "override-deleted-after" => {
                response
                    .server
                    .as_mut()
                    .expect("ui-fixture: admin scenario has server")
                    .governor
                    .overrides
                    .clear();
            }
            "limits-after" => {
                let server = response
                    .server
                    .as_mut()
                    .expect("ui-fixture: admin scenario has server");
                server.base_url = "https://fixture.invalid/v1".into();
                server.limits.heartbeat_secs = 10;
                server.limits.max_inflight = 512;
                server.limits.max_wait_secs = 900;
                server.limits.models_ttl_secs = 600;
                server.limits.request_timeout_secs = 300;
                server.limits.stream_idle_secs = 300;
                server.limits.strict_passthrough = false;
            }
            "history-after" => {
                let server = response
                    .server
                    .as_mut()
                    .expect("ui-fixture: admin scenario has server");
                server.dashboard.default_window_days = 7;
                server.dashboard.slo_target_percent = 99.9;
                server.history.days = 30;
            }
            "governor-disabled-after" => {
                response
                    .server
                    .as_mut()
                    .expect("ui-fixture: admin scenario has server")
                    .governor
                    .enabled = false;
            }
            "override-created-after" | "override-delete-before" => {}
            "users-create-before" | "users-deleted-after" => {
                response.nim_keys.retain(|key| key.owner != "fixture-user");
                response
                    .client_keys
                    .retain(|key| key.owner != "fixture-user");
                response
                    .users
                    .as_mut()
                    .expect("ui-fixture: admin scenario has users")
                    .retain(|user| user.username != "fixture-user");
            }
            "users-created-after" => {
                response.nim_keys.retain(|key| key.owner != "fixture-user");
                response
                    .client_keys
                    .retain(|key| key.owner != "fixture-user");
            }
            "users-role-admin-after" => {
                response
                    .users
                    .as_mut()
                    .expect("ui-fixture: admin scenario has users")
                    .iter_mut()
                    .find(|user| user.username == "fixture-user")
                    .expect("ui-fixture: fixture user exists")
                    .role = Role::Admin;
            }
            "long-values" => {
                let client = format!("client-fixture-{}", "c".repeat(49));
                let long_username = format!("user-fixture-{}", "u".repeat(19));
                assert_eq!(client.len(), 64);
                assert_eq!(long_username.len(), 32);
                response.client_keys[0].name = client;
                response.client_keys[0].owner = long_username.clone();
                response.nim_keys[0].owner = long_username.clone();
                response
                    .users
                    .as_mut()
                    .expect("admin long scenario")
                    .iter_mut()
                    .find(|user| user.username == "fixture-user")
                    .expect("ui-fixture: fixture user exists")
                    .username = long_username;
            }
            other => panic!("ui-fixture: unknown config scenario {other}"),
        }

        let users = response
            .users
            .as_mut()
            .expect("ui-fixture: full state has users");
        for user in users.iter_mut() {
            user.nim_keys = response
                .nim_keys
                .iter()
                .filter(|key| key.owner == user.username)
                .count();
            user.client_keys = response
                .client_keys
                .iter()
                .filter(|key| key.owner == user.username)
                .count();
        }
        let mut next_lane = 0;
        for key in &mut response.nim_keys {
            if key.enabled {
                key.lane = Some(next_lane);
                key.cooldown_ms.get_or_insert(0);
                key.in_window.get_or_insert(0);
                next_lane += 1;
            } else {
                key.lane = None;
                key.cooldown_ms = None;
                key.in_window = None;
            }
        }
        response.pool.enabled = response.nim_keys.iter().filter(|key| key.enabled).count();
        response.pool.capacity_rpm = response
            .nim_keys
            .iter()
            .filter(|key| key.enabled)
            .map(|key| key.rpm)
            .sum();
        for key in &mut response.nim_keys {
            key.guarded = false;
        }
        let superuser_enabled: Vec<usize> = response
            .nim_keys
            .iter()
            .enumerate()
            .filter(|(_, key)| key.enabled && key.owner == "fixture-superuser")
            .map(|(index, _)| index)
            .collect();
        if let [index] = superuser_enabled.as_slice() {
            response.nim_keys[*index].guarded = true;
        }
        assert_eq!(response.pool.enabled, next_lane);
        assert_eq!(
            response.pool.capacity_rpm,
            response
                .nim_keys
                .iter()
                .filter(|key| key.enabled)
                .map(|key| key.rpm)
                .sum::<usize>()
        );
        assert!(response.nim_keys.iter().all(|key| {
            key.fingerprint.len() == 8
                && key
                    .fingerprint
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
        }));
        assert_eq!(
            response.nim_keys.iter().filter(|key| key.guarded).count(),
            usize::from(superuser_enabled.len() == 1)
        );
        response.role = users
            .iter()
            .find(|user| user.username == response.username)
            .expect("ui-fixture: authenticated user exists")
            .role;
        if !response.role.is_admin() {
            response
                .nim_keys
                .retain(|key| key.owner == response.username);
            response
                .client_keys
                .retain(|key| key.owner == response.username);
            response.server = None;
            response.users = None;
        }
        response
    }

    fn dashboard_now_ui_fixture(changed: bool) -> DashboardNowResponse {
        let mut metrics = representative_dashboard_metrics(FixtureSegment::All);
        metrics.extend(representative_latest_metrics());
        DashboardNowResponse {
            auth: true,
            available_from: Some(1_697_411_600),
            available_to: Some(1_700_003_600),
            capacity_rpm: 80,
            config_revision: 7,
            default_window_days: 30,
            history_revision: 11,
            lanes: 2,
            metrics,
            retention_days: 30,
            rpms: vec![40, 40],
            sampled_at: if changed {
                1_700_007_200
            } else {
                1_700_003_600
            },
            slo_target_percent: 99.9,
            started: 1_700_000_000,
            tail: Tail {
                base_history_revision: 11,
                from: Some(1_700_003_600),
                to: if changed {
                    1_700_007_200
                } else {
                    1_700_003_600
                },
                totals: if changed {
                    representative_dashboard_metrics(FixtureSegment::Second)
                } else {
                    Vec::new()
                },
            },
            version: "0.6.6".into(),
        }
    }

    fn dashboard_now_empty_ui_fixture() -> DashboardNowResponse {
        let mut response = dashboard_now_ui_fixture(false);
        response.available_from = None;
        response.available_to = None;
        response.metrics = vec![active_requests(0.0)];
        response.tail.from = None;
        response.tail.totals.clear();
        response
    }

    fn dashboard_now_observation_tail_ui_fixture() -> DashboardNowResponse {
        let mut response = dashboard_now_ui_fixture(false);
        response
            .metrics
            .retain(|metric| metric.metric != "nimproxy_usage_observations_total");
        response.tail.from = Some(1_700_003_600);
        response.tail.to = 1_700_007_200;
        response.tail.totals = vec![
            labeled_metric(
                "nimproxy_usage_observations_total",
                &[("field", "completion_tokens"), ("result", "estimated")],
                7.0,
            ),
            labeled_metric(
                "nimproxy_usage_observations_total",
                &[("field", "prompt_tokens"), ("result", "invalid")],
                -1.0,
            ),
        ];
        response
    }

    fn assert_dashboard_pair(label: &str, range: &DashboardResponse, now: &DashboardNowResponse) {
        assert_eq!(
            range.history_revision, now.history_revision,
            "ui-fixture:{label}: history revision must come from one HistoryInner"
        );
        assert_eq!(
            range.window.available_from, now.available_from,
            "ui-fixture:{label}: available_from is a global indexed bound"
        );
        assert_eq!(
            range.window.available_to, now.available_to,
            "ui-fixture:{label}: available_to excludes the unindexed tail"
        );
        assert_eq!(
            now.tail.base_history_revision, now.history_revision,
            "ui-fixture:{label}: tail must extend the paired indexed revision"
        );
        assert!(
            !range.window.following_now,
            "ui-fixture:{label}: every fixture request supplies an explicit to bound"
        );
    }

    fn dashboard_now_one_key_ui_fixture() -> DashboardNowResponse {
        let mut response = dashboard_now_ui_fixture(false);
        response.capacity_rpm = 40;
        response.lanes = 1;
        response.rpms = vec![40];
        response
    }

    fn dashboard_now_open_auth_ui_fixture() -> DashboardNowResponse {
        let mut response = dashboard_now_ui_fixture(false);
        response.auth = false;
        response
    }

    fn dashboard_now_partial_ui_fixture() -> DashboardNowResponse {
        let mut response = dashboard_now_ui_fixture(false);
        response.available_from = Some(1_699_913_600);
        response
    }

    #[test]
    fn ui_fixtures() {
        let stored = StoredConfig::default();
        let initial_now = dashboard_now_ui_fixture(false);
        let changed_now = dashboard_now_ui_fixture(true);
        let empty_now = dashboard_now_empty_ui_fixture();
        let partial_now = dashboard_now_partial_ui_fixture();
        let observation_tail_now = dashboard_now_observation_tail_ui_fixture();
        let healthy_range = dashboard_ui_fixture("healthy");
        assert_representative_metrics_are_production_faithful(&healthy_range);
        let metric_inventory: std::collections::BTreeSet<_> = healthy_range
            .totals
            .iter()
            .chain(healthy_range.latest.iter())
            .chain(initial_now.metrics.iter())
            .map(|metric| metric.metric.as_str())
            .collect();
        let expected_metric_inventory = std::collections::BTreeSet::from([
            "nimproxy_active_requests",
            "nimproxy_affinity_total",
            "nimproxy_completion_tokens_total",
            "nimproxy_finish_reason_total",
            "nimproxy_json_mode_total",
            "nimproxy_lane_cooldown_total",
            "nimproxy_lane_requests_total",
            "nimproxy_login_failures_total",
            "nimproxy_model_inflight",
            "nimproxy_model_limit",
            "nimproxy_prompt_tokens_total",
            "nimproxy_queue_depth",
            "nimproxy_queue_wait_seconds_bucket",
            "nimproxy_queue_wait_seconds_count",
            "nimproxy_queue_wait_seconds_sum",
            "nimproxy_reasoning_tokens_total",
            "nimproxy_request_max_tokens_count",
            "nimproxy_request_max_tokens_sum",
            "nimproxy_request_messages_count",
            "nimproxy_request_messages_sum",
            "nimproxy_request_temperature_count",
            "nimproxy_request_temperature_sum",
            "nimproxy_request_tools_count",
            "nimproxy_request_tools_sum",
            "nimproxy_requests_total",
            "nimproxy_shed_total",
            "nimproxy_stream_requests_total",
            "nimproxy_tokens_per_second_bucket",
            "nimproxy_tokens_per_second_count",
            "nimproxy_tokens_per_second_sum",
            "nimproxy_tool_calls_total",
            "nimproxy_tool_choice_total",
            "nimproxy_tpot_seconds_bucket",
            "nimproxy_tpot_seconds_count",
            "nimproxy_tpot_seconds_sum",
            "nimproxy_ttft_seconds_bucket",
            "nimproxy_ttft_seconds_count",
            "nimproxy_ttft_seconds_sum",
            "nimproxy_unauthorized_total",
            "nimproxy_upstream_seconds_bucket",
            "nimproxy_upstream_seconds_count",
            "nimproxy_upstream_seconds_sum",
            "nimproxy_usage_observations_total",
            "nimproxy_worker_exhausted_total",
        ]);
        assert_eq!(
            metric_inventory, expected_metric_inventory,
            "ui-fixture: representative dashboard metric inventory changed"
        );
        for (label, range, now) in [
            ("healthy-initial", healthy_range, &initial_now),
            (
                "healthy-changed-tail",
                dashboard_ui_fixture("healthy"),
                &changed_now,
            ),
            ("empty", dashboard_ui_fixture("empty"), &empty_now),
            ("partial", dashboard_ui_fixture("partial"), &partial_now),
            (
                "unavailable",
                dashboard_ui_fixture("unavailable"),
                &initial_now,
            ),
            (
                "outside-before",
                dashboard_ui_fixture("outside-before"),
                &initial_now,
            ),
            (
                "outside-after",
                dashboard_ui_fixture("outside-after"),
                &initial_now,
            ),
            ("extreme", dashboard_ui_fixture("extreme"), &initial_now),
            ("long", dashboard_ui_fixture("long"), &initial_now),
            (
                "observation-measured",
                dashboard_ui_fixture("observation-measured"),
                &initial_now,
            ),
            (
                "observation-estimated",
                dashboard_ui_fixture("observation-estimated"),
                &initial_now,
            ),
            (
                "observation-unavailable",
                dashboard_ui_fixture("observation-unavailable"),
                &initial_now,
            ),
            (
                "observation-invalid",
                dashboard_ui_fixture("observation-invalid"),
                &initial_now,
            ),
            (
                "observation-incomplete",
                dashboard_ui_fixture("observation-incomplete"),
                &partial_now,
            ),
            (
                "observation-zero",
                dashboard_ui_fixture("observation-zero"),
                &initial_now,
            ),
            (
                "observation-absent",
                dashboard_ui_fixture("observation-absent"),
                &initial_now,
            ),
            (
                "observation-tie-invalid",
                dashboard_ui_fixture("observation-tie-invalid"),
                &initial_now,
            ),
            (
                "observation-tie-unavailable",
                dashboard_ui_fixture("observation-tie-unavailable"),
                &initial_now,
            ),
            (
                "observation-tie-estimated",
                dashboard_ui_fixture("observation-tie-estimated"),
                &initial_now,
            ),
            (
                "observation-live-tail",
                dashboard_ui_fixture("observation-absent"),
                &observation_tail_now,
            ),
            (
                "one-hour",
                dashboard_window_ui_fixture(1_700_000_000, 1_700_003_600),
                &initial_now,
            ),
            (
                "custom",
                dashboard_window_ui_fixture(1_700_000_100, 1_700_000_200),
                &initial_now,
            ),
        ] {
            assert_dashboard_pair(label, &range, now);
        }
        assert_eq!(
            observation_tail_now.tail.totals,
            vec![
                labeled_metric(
                    "nimproxy_usage_observations_total",
                    &[("field", "completion_tokens"), ("result", "estimated")],
                    7.0,
                ),
                labeled_metric(
                    "nimproxy_usage_observations_total",
                    &[("field", "prompt_tokens"), ("result", "invalid")],
                    -1.0,
                ),
            ],
            "ui-fixture: live tail contains one valid quality row and one ignored negative row"
        );
        let partial_range = dashboard_ui_fixture("partial");
        let unavailable_range = dashboard_ui_fixture("unavailable");
        let unavailable_from = unavailable_range
            .window
            .available_from
            .expect("ui-fixture: unavailable has usable history before the gap");
        let unavailable_to = unavailable_range
            .window
            .available_to
            .expect("ui-fixture: unavailable has usable history after the gap");
        assert!(unavailable_from < unavailable_range.window.requested_from);
        assert!(unavailable_range.window.requested_from < unavailable_range.window.requested_to);
        assert!(unavailable_range.window.requested_to < unavailable_to);
        assert_eq!(unavailable_range.window.effective_from, None);
        assert_eq!(unavailable_range.window.effective_to, None);
        assert!(!unavailable_range.window.complete);
        assert!(unavailable_range.totals.is_empty());
        assert!(unavailable_range.latest.is_empty());
        assert!(unavailable_range.points.is_empty());
        assert_eq!(
            unavailable_range.diagnostics,
            HistoryDiagnostics {
                excluded_epochs: 1,
                excluded_records: 3,
                normalized_series: 1,
                skipped_metric_lines: 0,
                valid_checkpoints: 0,
                valid_samples: 1,
            },
            "ui-fixture: unavailable query has cumulative recovered-gap evidence"
        );
        assert!(unavailable_range.diagnostics.excluded_epochs > 0);
        assert!(unavailable_range.diagnostics.excluded_records > 0);
        assert!(unavailable_range.diagnostics.normalized_series > 0);
        assert!(unavailable_range.diagnostics.valid_samples > 0);
        for (label, range, before) in [
            (
                "outside-before",
                dashboard_ui_fixture("outside-before"),
                true,
            ),
            (
                "outside-after",
                dashboard_ui_fixture("outside-after"),
                false,
            ),
        ] {
            assert!(
                !range.window.complete,
                "ui-fixture:{label}: range is incomplete"
            );
            assert_eq!(range.window.effective_from, None);
            assert_eq!(range.window.effective_to, None);
            assert!(range.points.is_empty());
            assert!(range.totals.is_empty());
            assert!(range.latest.is_empty());
            let available_from = range
                .window
                .available_from
                .expect("ui-fixture: global start");
            let available_to = range.window.available_to.expect("ui-fixture: global end");
            if before {
                assert!(range.window.requested_to < available_from);
            } else {
                assert!(available_to < range.window.requested_from);
            }
        }
        assert!(
            partial_range.window.requested_from
                < partial_range
                    .window
                    .available_from
                    .expect("ui-fixture: partial has indexed data")
        );
        assert_eq!(
            partial_range.window.available_from, partial_range.window.effective_from,
            "ui-fixture:partial: a query before global retention starts at available_from"
        );
        let scenarios = BTreeMap::from([
            (
                "access-before".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "access-before")).unwrap(),
            ),
            (
                "access-refreshed".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "access-refreshed"))
                    .unwrap(),
            ),
            (
                "auth-keyed-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "auth-keyed-after"))
                    .unwrap(),
            ),
            (
                "auth-open-before".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "auth-open-before"))
                    .unwrap(),
            ),
            (
                "client-create-before".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "client-create-before"))
                    .unwrap(),
            ),
            (
                "client-created-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "client-created-after"))
                    .unwrap(),
            ),
            (
                "client-deleted-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "client-deleted-after"))
                    .unwrap(),
            ),
            (
                "dashboard-custom-window".to_owned(),
                serde_json::to_value(dashboard_window_ui_fixture(1_700_000_100, 1_700_000_200))
                    .unwrap(),
            ),
            (
                "dashboard-now-empty".to_owned(),
                serde_json::to_value(dashboard_now_empty_ui_fixture()).unwrap(),
            ),
            (
                "dashboard-now-one-key".to_owned(),
                serde_json::to_value(dashboard_now_one_key_ui_fixture()).unwrap(),
            ),
            (
                "dashboard-now-open-auth".to_owned(),
                serde_json::to_value(dashboard_now_open_auth_ui_fixture()).unwrap(),
            ),
            (
                "dashboard-now-partial".to_owned(),
                serde_json::to_value(dashboard_now_partial_ui_fixture()).unwrap(),
            ),
            (
                "dashboard-one-hour-window".to_owned(),
                serde_json::to_value(dashboard_window_ui_fixture(1_700_000_000, 1_700_003_600))
                    .unwrap(),
            ),
            (
                "governor-disabled-after".to_owned(),
                serde_json::to_value(config_ui_fixture(
                    Role::Superuser,
                    "governor-disabled-after",
                ))
                .unwrap(),
            ),
            (
                "governor-enabled-before".to_owned(),
                serde_json::to_value(config_ui_fixture(
                    Role::Superuser,
                    "governor-enabled-before",
                ))
                .unwrap(),
            ),
            (
                "history-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "history-after")).unwrap(),
            ),
            (
                "history-before".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "history-before")).unwrap(),
            ),
            (
                "limits-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "limits-after")).unwrap(),
            ),
            (
                "limits-before".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "limits-before")).unwrap(),
            ),
            (
                "long-values".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "long-values")).unwrap(),
            ),
            (
                "nim-create-before".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "nim-create-before"))
                    .unwrap(),
            ),
            (
                "nim-created-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "nim-created-after"))
                    .unwrap(),
            ),
            (
                "nim-deleted-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "nim-deleted-after"))
                    .unwrap(),
            ),
            (
                "nim-disabled-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "nim-disabled-after"))
                    .unwrap(),
            ),
            (
                "nim-rpm-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "nim-rpm-after")).unwrap(),
            ),
            (
                "override-create-before".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "override-create-before"))
                    .unwrap(),
            ),
            (
                "override-created-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "override-created-after"))
                    .unwrap(),
            ),
            (
                "override-delete-before".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "override-delete-before"))
                    .unwrap(),
            ),
            (
                "override-deleted-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "override-deleted-after"))
                    .unwrap(),
            ),
            (
                "setup-error".to_owned(),
                serde_json::to_value(ApiError::new("invalid_setup", "setup fixture rejected"))
                    .unwrap(),
            ),
            (
                "users-create-before".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "users-create-before"))
                    .unwrap(),
            ),
            (
                "users-created-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "users-created-after"))
                    .unwrap(),
            ),
            (
                "users-deleted-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "users-deleted-after"))
                    .unwrap(),
            ),
            (
                "users-delete-before".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "users-delete-before"))
                    .unwrap(),
            ),
            (
                "users-role-admin-after".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "users-role-admin-after"))
                    .unwrap(),
            ),
            (
                "users-role-before".to_owned(),
                serde_json::to_value(config_ui_fixture(Role::Superuser, "users-role-before"))
                    .unwrap(),
            ),
        ]);
        let fixtures: Vec<(&str, serde_json::Value)> = vec![
            (
                "api-error.json",
                serde_json::to_value(ApiError::new("invalid_history", "invalid history fixture"))
                    .unwrap(),
            ),
            (
                "api-error-locale.json",
                serde_json::to_value(ApiError::new("invalid_locale", "locale fixture rejected"))
                    .unwrap(),
            ),
            (
                "clients-secret-absent.json",
                serde_json::to_value(ClientsResponse {
                    ok: true,
                    secret: None,
                })
                .unwrap(),
            ),
            (
                "clients-secret-present.json",
                serde_json::to_value(ClientsResponse {
                    ok: true,
                    secret: Some("npk_fixture_secret".into()),
                })
                .unwrap(),
            ),
            (
                "config-admin.json",
                serde_json::to_value(config_ui_fixture(Role::Admin, "base")).unwrap(),
            ),
            (
                "config-superuser.json",
                serde_json::to_value(config_ui_fixture(Role::Superuser, "base")).unwrap(),
            ),
            (
                "config-user.json",
                serde_json::to_value(config_ui_fixture(Role::User, "base")).unwrap(),
            ),
            (
                "dashboard-empty.json",
                serde_json::to_value(dashboard_ui_fixture("empty")).unwrap(),
            ),
            (
                "dashboard-extreme.json",
                serde_json::to_value(dashboard_ui_fixture("extreme")).unwrap(),
            ),
            (
                "dashboard-healthy.json",
                serde_json::to_value(dashboard_ui_fixture("healthy")).unwrap(),
            ),
            (
                "dashboard-long.json",
                serde_json::to_value(dashboard_ui_fixture("long")).unwrap(),
            ),
            (
                "dashboard-now-changed.json",
                serde_json::to_value(dashboard_now_ui_fixture(true)).unwrap(),
            ),
            (
                "dashboard-now-initial.json",
                serde_json::to_value(dashboard_now_ui_fixture(false)).unwrap(),
            ),
            (
                "dashboard-now-observation-tail.json",
                serde_json::to_value(dashboard_now_observation_tail_ui_fixture()).unwrap(),
            ),
            (
                "dashboard-partial.json",
                serde_json::to_value(dashboard_ui_fixture("partial")).unwrap(),
            ),
            (
                "dashboard-outside-after.json",
                serde_json::to_value(dashboard_ui_fixture("outside-after")).unwrap(),
            ),
            (
                "dashboard-outside-before.json",
                serde_json::to_value(dashboard_ui_fixture("outside-before")).unwrap(),
            ),
            (
                "dashboard-unavailable.json",
                serde_json::to_value(dashboard_ui_fixture("unavailable")).unwrap(),
            ),
            (
                "dashboard-observation-estimated.json",
                serde_json::to_value(dashboard_ui_fixture("observation-estimated")).unwrap(),
            ),
            (
                "dashboard-observation-incomplete.json",
                serde_json::to_value(dashboard_ui_fixture("observation-incomplete")).unwrap(),
            ),
            (
                "dashboard-observation-invalid.json",
                serde_json::to_value(dashboard_ui_fixture("observation-invalid")).unwrap(),
            ),
            (
                "dashboard-observation-measured.json",
                serde_json::to_value(dashboard_ui_fixture("observation-measured")).unwrap(),
            ),
            (
                "dashboard-observation-unavailable.json",
                serde_json::to_value(dashboard_ui_fixture("observation-unavailable")).unwrap(),
            ),
            (
                "dashboard-observation-zero.json",
                serde_json::to_value(dashboard_ui_fixture("observation-zero")).unwrap(),
            ),
            (
                "dashboard-observation-absent.json",
                serde_json::to_value(dashboard_ui_fixture("observation-absent")).unwrap(),
            ),
            (
                "dashboard-observation-tie-invalid.json",
                serde_json::to_value(dashboard_ui_fixture("observation-tie-invalid")).unwrap(),
            ),
            (
                "dashboard-observation-tie-unavailable.json",
                serde_json::to_value(dashboard_ui_fixture("observation-tie-unavailable")).unwrap(),
            ),
            (
                "dashboard-observation-tie-estimated.json",
                serde_json::to_value(dashboard_ui_fixture("observation-tie-estimated")).unwrap(),
            ),
            (
                "locale-bootstrap.json",
                serde_json::to_value(LocaleBootstrap::from_config(&stored, &["en-US"])).unwrap(),
            ),
            ("ok.json", serde_json::to_value(OkResponse::new()).unwrap()),
            (
                "scenarios.json",
                serde_json::to_value(scenarios).expect("ui-fixture: scenario manifest serializes"),
            ),
            (
                "setup-minted-client-key.json",
                serde_json::to_value(SetupResponse {
                    client_key: Some(MintedClientKey {
                        name: "default".into(),
                        secret: "npk_fixture_secret".into(),
                    }),
                    ok: true,
                })
                .unwrap(),
            ),
            (
                "setup-no-client-key.json",
                serde_json::to_value(SetupResponse {
                    client_key: None,
                    ok: true,
                })
                .unwrap(),
            ),
            (
                "validate-failure.json",
                serde_json::to_value(ValidateKeyResponse {
                    error: Some("validation fixture rejected".into()),
                    models: None,
                    ok: false,
                })
                .unwrap(),
            ),
            (
                "validate-success.json",
                serde_json::to_value(ValidateKeyResponse {
                    error: None,
                    models: Some(3),
                    ok: true,
                })
                .unwrap(),
            ),
        ];
        let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ui");
        let generated: Vec<(&str, Vec<u8>)> = fixtures
            .into_iter()
            .map(|(name, value)| {
                let mut json = serde_json::to_string_pretty(&value)
                    .expect("ui-fixture: typed value serializes");
                json.push('\n');
                (name, json.into_bytes())
            })
            .collect();
        let mut expected_names: Vec<String> = generated
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        expected_names.sort();
        let update =
            std::env::var_os("UPDATE_UI_FIXTURES").as_deref() == Some(std::ffi::OsStr::new("1"));
        if update {
            fs::create_dir_all(&fixture_dir).expect("ui-fixture: fixture directory is writable");
            for entry in
                fs::read_dir(&fixture_dir).expect("ui-fixture: fixture directory is readable")
            {
                let entry = entry.expect("ui-fixture: fixture directory entry is readable");
                let name = entry.file_name().to_string_lossy().into_owned();
                if !expected_names.contains(&name) {
                    fs::remove_file(entry.path())
                        .expect("ui-fixture: obsolete fixture can be removed");
                }
            }
            for (name, bytes) in &generated {
                fs::write(fixture_dir.join(name), bytes)
                    .expect("ui-fixture: generated fixture can be written");
            }
        }
        let mut actual_names: Vec<String> = fs::read_dir(&fixture_dir)
            .expect("ui-fixture: fixture directory exists")
            .map(|entry| {
                entry
                    .expect("ui-fixture: fixture directory entry is readable")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        actual_names.sort();
        assert_eq!(
            actual_names, expected_names,
            "ui-fixture:inventory-drift: committed Rust-owned fixture inventory must be exact"
        );
        for (name, generated) in generated {
            let committed = fs::read(fixture_dir.join(name))
                .expect("ui-fixture: committed fixture is readable");
            if committed != generated {
                let mismatch = committed
                    .iter()
                    .zip(&generated)
                    .position(|(left, right)| left != right)
                    .unwrap_or_else(|| committed.len().min(generated.len()));
                panic!(
                    "ui-fixture:{name}-drift: committed fixture differs at byte {mismatch} \
                     (committed {} bytes, generated {} bytes); regenerate with \
                     UPDATE_UI_FIXTURES=1",
                    committed.len(),
                    generated.len(),
                );
            }
        }
    }

    /// Declaration order is the wire order, and it must stay ASCII-sorted:
    /// that is what the `serde_json::Map` (a `BTreeMap`) behind the old
    /// `json!` bodies produced, so reordering a field silently reshapes a
    /// response that shipped. Serializing a fully-populated value and
    /// asserting the keys come out sorted catches it for every type at once.
    #[test]
    fn field_order_stays_ascii_sorted() {
        fn sorted<T: Serialize>(label: &str, value: &T) {
            let keys = keys_of(value);
            let mut want = keys.clone();
            want.sort();
            assert_eq!(
                keys, want,
                "{label}: field declaration order must be ASCII-sorted — it is the wire order"
            );
            assert!(!keys.is_empty(), "{label}: nothing was serialized");
        }

        sorted("ApiError", &ApiError::new("code", "message"));
        sorted(
            "CapacityRollup",
            &crate::history::CapacityRollup {
                average_rpm: 0.0,
                latest_rpms: vec![],
            },
        );
        sorted("ApiErrorBody", &ApiError::new("code", "message").error);
        sorted("OkResponse", &OkResponse::new());
        sorted(
            "LocaleBootstrap",
            &LocaleBootstrap::from_config(&crate::config::StoredConfig::default(), &["en-US"]),
        );
        sorted(
            "ClientsResponse",
            &ClientsResponse {
                ok: true,
                secret: Some("npk_x".into()),
            },
        );
        sorted(
            "ValidateKeyResponse",
            &ValidateKeyResponse {
                error: Some("nope".into()),
                models: Some(1),
                ok: false,
            },
        );
        sorted(
            "SetupResponse",
            &SetupResponse {
                client_key: Some(MintedClientKey {
                    name: "first".into(),
                    secret: "npk_x".into(),
                }),
                ok: true,
            },
        );
        sorted(
            "MintedClientKey",
            &MintedClientKey {
                name: "first".into(),
                secret: "npk_x".into(),
            },
        );
        sorted(
            "ClientKeyRow",
            &ClientKeyRow {
                last4: "abcd".into(),
                name: "harness".into(),
                owner: "root".into(),
            },
        );
        sorted(
            "NimKeyRow",
            &NimKeyRow {
                cooldown_ms: Some(0),
                enabled: true,
                fingerprint: "abcd1234".into(),
                guarded: false,
                in_window: Some(0),
                lane: Some(0),
                last4: "wxyz".into(),
                owner: "root".into(),
                rpm: 40,
            },
        );
        sorted(
            "PoolSummary",
            &PoolSummary {
                capacity_rpm: 40,
                enabled: 1,
            },
        );
        sorted(
            "HistorySettings",
            &HistorySettings {
                available_from: Some(0),
                compaction_pending: false,
                days: 30,
                file_bytes: 0,
                persistence: "ok".into(),
            },
        );
        sorted(
            "UserRow",
            &UserRow {
                client_keys: 0,
                nim_keys: 0,
                role: Role::User,
                username: "root".into(),
            },
        );
        let server = ServerSettings {
            base_url: "https://example.invalid".into(),
            dashboard: DashboardCfg::default(),
            default_locale: "en-US".into(),
            governor: GovernorCfg::default(),
            history: HistorySettings {
                available_from: None,
                compaction_pending: false,
                days: 30,
                file_bytes: 0,
                persistence: "ok".into(),
            },
            limits: Limits::default(),
        };
        sorted("Limits", &server.limits);
        sorted("DashboardCfg", &server.dashboard);
        sorted("GovernorCfg", &server.governor);
        sorted("ServerSettings", &server);
        sorted(
            "ConfigResponse",
            &ConfigResponse {
                client_keys: Vec::new(),
                locale: None,
                mode: Mode::Keyed,
                nim_keys: Vec::new(),
                pool: PoolSummary {
                    capacity_rpm: 40,
                    enabled: 1,
                },
                role: Role::Superuser,
                server: Some(server),
                username: "root".into(),
                users: Some(Vec::new()),
            },
        );
        let window = DashboardWindow {
            available_from: None,
            available_to: None,
            complete: false,
            default_window_days: 30,
            effective_from: None,
            effective_to: None,
            following_now: true,
            requested_from: 0,
            requested_to: 1,
            retention_days: 30,
        };
        sorted("DashboardWindow", &window);
        sorted(
            "DashboardResponse",
            &DashboardResponse {
                config_revision: 1,
                diagnostics: HistoryDiagnostics::default(),
                history_revision: 1,
                latest: Vec::new(),
                points: Vec::new(),
                totals: Vec::new(),
                window,
            },
        );
        sorted("HistoryDiagnostics", &HistoryDiagnostics::default());
        let tail = Tail {
            base_history_revision: 1,
            from: None,
            to: 1,
            totals: Vec::new(),
        };
        sorted("Tail", &tail);
        sorted(
            "DashboardNowResponse",
            &DashboardNowResponse {
                auth: true,
                available_from: None,
                available_to: None,
                capacity_rpm: 40,
                config_revision: 1,
                default_window_days: 30,
                history_revision: 1,
                lanes: 1,
                metrics: Vec::new(),
                retention_days: 30,
                rpms: vec![40],
                sampled_at: 0,
                slo_target_percent: 99.9,
                started: 0,
                tail,
                version: "0.0.0".into(),
            },
        );
        sorted(
            "MetricValue",
            &MetricValue {
                labels: BTreeMap::new(),
                metric: "nimproxy_requests_total".into(),
                value: 1.0,
            },
        );
        sorted(
            "RollupPoint",
            &RollupPoint {
                capacity: None,
                duration_seconds: 1,
                from: 0,
                to: 1,
                values: Vec::new(),
            },
        );
    }

    /// Absent, not null: a non-admin `/api/config` body must not even carry
    /// the admin keys, and a successful probe must not carry `error`.
    #[test]
    fn optional_sections_are_omitted_not_nulled() {
        let user_view = ConfigResponse {
            client_keys: Vec::new(),
            locale: None,
            mode: Mode::Open,
            nim_keys: Vec::new(),
            pool: PoolSummary {
                capacity_rpm: 40,
                enabled: 1,
            },
            role: Role::User,
            server: None,
            username: "alice".into(),
            users: None,
        };
        assert_eq!(
            serde_json::to_string(&user_view).unwrap(),
            r#"{"client_keys":[],"locale":null,"mode":"open","nim_keys":[],"pool":{"capacity_rpm":40,"enabled":1},"role":"user","username":"alice"}"#
        );
        assert_eq!(
            serde_json::to_string(&ValidateKeyResponse::probed(Ok(3))).unwrap(),
            r#"{"models":3,"ok":true}"#
        );
        assert_eq!(
            serde_json::to_string(&ValidateKeyResponse::probed(Err("nope".into()))).unwrap(),
            r#"{"error":"nope","ok":false}"#
        );
        assert_eq!(
            serde_json::to_string(&SetupResponse {
                client_key: None,
                ok: true
            })
            .unwrap(),
            r#"{"ok":true}"#
        );
    }

    /// A null lane is meaningful (the key holds none), so those stay null.
    #[test]
    fn lane_state_is_null_for_a_benched_key() {
        assert_eq!(
            serde_json::to_string(&NimKeyRow {
                cooldown_ms: None,
                enabled: false,
                fingerprint: "abcd1234".into(),
                guarded: false,
                in_window: None,
                lane: None,
                last4: "wxyz".into(),
                owner: "root".into(),
                rpm: 40,
            })
            .unwrap(),
            r#"{"cooldown_ms":null,"enabled":false,"fingerprint":"abcd1234","guarded":false,"in_window":null,"lane":null,"last4":"wxyz","owner":"root","rpm":40}"#
        );
    }

    /// The error envelope is the `/v1` envelope: `{error:{code,message,type}}`.
    #[test]
    fn error_envelope_matches_the_v1_shape() {
        assert_eq!(
            serde_json::to_string(&ApiError::new(
                "forbidden",
                "server settings require an admin"
            ))
            .unwrap(),
            r#"{"error":{"code":"forbidden","message":"server settings require an admin","type":"proxy_error"}}"#
        );
    }

    #[test]
    fn api_error_wire_order_is_exact() {
        let body = serde_json::to_string(&ApiError::new("invalid_json", "invalid JSON")).unwrap();
        assert_eq!(
            body,
            r#"{"error":{"code":"invalid_json","message":"invalid JSON","type":"proxy_error"}}"#
        );
    }

    #[test]
    fn locale_bootstrap_config_factory_uses_the_persisted_default() {
        let stored: crate::config::StoredConfig =
            serde_json::from_value(serde_json::json!({"default_locale": "fr-FR"}))
                .expect("locale-bootstrap: distinct StoredConfig");
        let bootstrap = LocaleBootstrap::from_config(&stored, &["en-US", "fr-FR"]);
        assert_eq!(
            serde_json::to_string(&bootstrap).unwrap(),
            r#"{"installed_locales":["en-US","fr-FR"],"server_default":"fr-FR"}"#,
            "locale-bootstrap: the private config-owned factory must select the persisted default, not the compiled default"
        );
    }
}

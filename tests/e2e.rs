//! End-to-end tests: the real proxy binary against a scriptable mock NIM.
//!
//! Config now lives in a UI-managed store (DATA_DIR/config.json) rather than
//! env vars: `StoreOpts` writes the fixture, and the dashboard/metrics/history
//! surface always requires auth. See `tests/support/mod.rs` for the harness.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use reqwest::header::CONTENT_TYPE;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use support::{
    chat_body, complete_setup, expect_refuses_to_start, login, login_as, metrics, read_sse,
    restart, scratch_data_dir, start_mock, start_proxy, start_proxy_fresh, start_proxy_in,
    start_proxy_with, Behavior, StoreOpts, TEST_PASSWORD,
};

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// A client that does NOT follow redirects, so we can assert on 302/303.
fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn usage_observation_counter_lines(exposition: &str) -> BTreeSet<String> {
    let rows: Vec<_> = exposition
        .lines()
        .filter(|line| line.starts_with("nimproxy_usage_observations_total{"))
        .collect();
    let unique_rows: BTreeSet<_> = rows.iter().copied().collect();
    assert_eq!(
        unique_rows.len(),
        rows.len(),
        "usage observation exposition must not duplicate a series: {rows:?}"
    );
    for row in &rows {
        let (labels, value) = row
            .strip_prefix("nimproxy_usage_observations_total{")
            .and_then(|row| row.split_once("} "))
            .expect("usage observation counter has labels and an integer value");
        let labels: Vec<_> = labels.split(',').collect();
        assert_eq!(
            labels.len(),
            2,
            "usage observation label count is closed: {row}"
        );
        let field = labels[0]
            .strip_prefix("field=\"")
            .and_then(|field| field.strip_suffix('"'))
            .expect("field is the first, quoted label");
        let result = labels[1]
            .strip_prefix("result=\"")
            .and_then(|result| result.strip_suffix('"'))
            .expect("result is the second, quoted label");
        assert!(
            matches!(
                field,
                "prompt_tokens"
                    | "completion_tokens"
                    | "total_tokens"
                    | "cached_tokens"
                    | "reasoning_tokens"
            ),
            "usage observation field is closed: {row}"
        );
        assert!(
            matches!(result, "measured" | "estimated" | "unavailable" | "invalid"),
            "usage observation result is closed: {row}"
        );
        value
            .parse::<u64>()
            .expect("usage observation counter value is an unsigned integer");
    }
    unique_rows.into_iter().map(str::to_owned).collect()
}

async fn assert_exact_api_error(
    response: reqwest::Response,
    status: reqwest::StatusCode,
    code: &str,
    message: &str,
) {
    assert_eq!(response.status(), status);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|content_type| content_type.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        format!(r#"{{"error":{{"code":"{code}","message":"{message}","type":"proxy_error"}}}}"#)
            .as_bytes()
    );
}

#[derive(Clone, Copy, Debug)]
enum ContractActor {
    BeforeSetup,
    Anonymous,
    User,
    Admin,
    Superuser,
}

impl ContractActor {
    const ALL: [Self; 5] = [
        Self::BeforeSetup,
        Self::Anonymous,
        Self::User,
        Self::Admin,
        Self::Superuser,
    ];

    fn index(self) -> usize {
        match self {
            Self::BeforeSetup => 0,
            Self::Anonymous => 1,
            Self::User => 2,
            Self::Admin => 3,
            Self::Superuser => 4,
        }
    }

    fn username(self) -> &'static str {
        match self {
            Self::User => "matrix-user",
            Self::Admin => "matrix-admin",
            Self::BeforeSetup | Self::Anonymous | Self::Superuser => support::TEST_USER,
        }
    }

    fn ordinal(self) -> usize {
        self.index() + 1
    }
}

#[derive(Clone, Copy, Debug)]
enum ContractAccess {
    Client,
    Public,
    OperatorAny,
    OperatorAdmin,
}

#[derive(Clone, Copy, Debug)]
enum ContractPhase {
    Always,
    PreSetup,
    PostSetup,
}

#[derive(Clone, Copy, Debug)]
enum ContractRequest {
    None,
    Json,
    Form,
}

#[derive(Clone, Copy, Debug)]
enum ContractSideEffect {
    None,
    SessionCookie,
    DurableConfig,
    DurableConfigIdempotent,
    DurableConfigAndSession,
    Upstream,
}

#[derive(Clone, Copy, Debug)]
struct ContractExpectation {
    status: u16,
    error: Option<&'static str>,
}

const fn contract_expectation(status: u16, error: Option<&'static str>) -> ContractExpectation {
    ContractExpectation { status, error }
}

#[derive(Clone, Copy, Debug)]
struct RouteBehavior {
    access: ContractAccess,
    accept_html: bool,
    expectations: [ContractExpectation; 5],
    method: &'static str,
    name: &'static str,
    phase: ContractPhase,
    request: ContractRequest,
    side_effect: ContractSideEffect,
    success_content_type: Option<&'static str>,
    success_status: u16,
    path: &'static str,
}

fn configured_contract_expectations(
    success_status: u16,
    ordinary_user_status: u16,
) -> [ContractExpectation; 5] {
    [
        contract_expectation(503, Some("setup_required")),
        contract_expectation(401, Some("unauthorized")),
        contract_expectation(
            ordinary_user_status,
            (ordinary_user_status == 403).then_some("forbidden"),
        ),
        contract_expectation(success_status, None),
        contract_expectation(success_status, None),
    ]
}

fn contract_config_bytes(proxy: &support::Proxy) -> Option<Vec<u8>> {
    match std::fs::read(proxy.data_dir.join("config.json")) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("route-contract:durable-read: config.json: {error}"),
    }
}

fn assert_contract_durable_change(
    before: &Option<Vec<u8>>,
    after: &Option<Vec<u8>>,
    context: &str,
) {
    assert!(
        after.is_some(),
        "route-contract:durable-bytes: successful {context} removed config.json"
    );
    assert_ne!(
        after, before,
        "route-contract:durable-bytes: successful {context} did not change config.json"
    );
}

fn contract_upstream_calls(mock: &support::MockNim) -> usize {
    mock.state.hit_count() + mock.state.models_hits.load(Ordering::SeqCst)
}

fn contract_body(
    row: &RouteBehavior,
    actor: ContractActor,
    mock: &support::MockNim,
) -> Option<String> {
    let suffix = match actor {
        ContractActor::BeforeSetup => "before",
        ContractActor::Anonymous => "anonymous",
        ContractActor::User => "user",
        ContractActor::Admin => "admin",
        ContractActor::Superuser => "superuser",
    };
    let value = match row.name {
        "api-settings-nim-keys" => {
            serde_json::json!({"add": {"key": format!("matrix-nim-{suffix}"), "rpm": 41}})
        }
        "api-settings-clients" => {
            serde_json::json!({"add": {"name": format!("matrix-client-{suffix}")}})
        }
        "api-settings-limits" => serde_json::json!({
            "heartbeat_secs": 1,
            "max_inflight": 512 + actor.ordinal(),
            "max_wait_secs": 31,
            "models_ttl_secs": 300,
            "request_timeout_secs": 300,
            "stream_idle_secs": 300,
            "strict_passthrough": false
        }),
        "api-settings-history" => serde_json::json!({
            "days": 60 + actor.ordinal(),
            "default_window_days": 30,
            "slo_target_percent": 99.8
        }),
        "api-settings-governor" => {
            serde_json::json!({"enabled": matches!(actor, ContractActor::Superuser)})
        }
        "api-settings-users" => serde_json::json!({
            "add": {
                "password": "matrix-password-1",
                "role": "user",
                "username": format!("created-{suffix}")
            }
        }),
        "api-settings-validate-key" => serde_json::json!({"key": "probe-key"}),
        "api-settings-upstream" => {
            serde_json::json!({"base_url": format!("{}/matrix-{suffix}", mock.url)})
        }
        "api-settings-locale" => serde_json::json!({"locale": "en-US"}),
        "api-settings-account" => serde_json::json!({
            "current_password": TEST_PASSWORD,
            "new_password": format!("matrix-new-password-{suffix}")
        }),
        "v1-wildcard" => serde_json::json!({
            "messages": [{"content": format!("route contract {suffix}"), "role": "user"}],
            "model": "mock/model-a",
            "stream": false
        }),
        "login-post" => {
            return Some(format!(
                "username={}&password={}",
                actor.username(),
                TEST_PASSWORD
            ));
        }
        "setup-post" => serde_json::json!({
            "base_url": mock.url,
            "nim_keys": [{"key": "matrix-setup-key", "rpm": 40}],
            "password": "matrix-setup-password",
            "username": "matrix-root"
        }),
        "setup-validate-key" => {
            serde_json::json!({"base_url": mock.url, "key": "probe-key"})
        }
        _ => return None,
    };
    Some(serde_json::to_string(&value).unwrap())
}

async fn send_contract_request(
    row: &RouteBehavior,
    actor: ContractActor,
    proxy: &support::Proxy,
    cookie: Option<&str>,
    mock: &support::MockNim,
) -> reqwest::Response {
    let mut request = match row.method {
        "GET" => no_redirect_client().get(proxy.url(row.path)),
        "POST" => no_redirect_client().post(proxy.url(row.path)),
        other => panic!("route-contract:request: unsupported method {other}"),
    };
    if row.accept_html {
        request = request.header("accept", "text/html");
    }
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    if let Some(body) = contract_body(row, actor, mock) {
        request = match row.request {
            ContractRequest::Json => request.header(CONTENT_TYPE, "application/json"),
            ContractRequest::Form => {
                request.header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            }
            ContractRequest::None => {
                panic!(
                    "route-contract:request: {} unexpectedly has a body",
                    row.name
                )
            }
        }
        .body(body);
    }
    request.send().await.unwrap()
}

#[tokio::test]
async fn route_contract_behavior_matrix() {
    let rows = vec![
        RouteBehavior {
            access: ContractAccess::Public,
            accept_html: false,
            expectations: [
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
            ],
            method: "GET",
            name: "health",
            phase: ContractPhase::Always,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/plain; charset=utf-8"),
            success_status: 200,
            path: "/health",
        },
        RouteBehavior {
            access: ContractAccess::Public,
            accept_html: false,
            expectations: [
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
            ],
            method: "GET",
            name: "asset-public-css",
            phase: ContractPhase::Always,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/css; charset=utf-8"),
            success_status: 200,
            path: "/assets/public/public.css",
        },
        RouteBehavior {
            access: ContractAccess::Public,
            accept_html: false,
            expectations: [
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
            ],
            method: "GET",
            name: "asset-public-setup-js",
            phase: ContractPhase::Always,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/javascript; charset=utf-8"),
            success_status: 200,
            path: "/assets/public/setup.js",
        },
        RouteBehavior {
            access: ContractAccess::Public,
            accept_html: false,
            expectations: [
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
            ],
            method: "GET",
            name: "asset-public-login-js",
            phase: ContractPhase::Always,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/javascript; charset=utf-8"),
            success_status: 200,
            path: "/assets/public/login.js",
        },
        RouteBehavior {
            access: ContractAccess::Public,
            accept_html: false,
            expectations: [
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
            ],
            method: "GET",
            name: "asset-public-locale",
            phase: ContractPhase::Always,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/assets/public/locales/en-US.json",
        },
        RouteBehavior {
            access: ContractAccess::Public,
            accept_html: false,
            expectations: [
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
            ],
            method: "GET",
            name: "api-locale-bootstrap",
            phase: ContractPhase::Always,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/locale-bootstrap",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "GET",
            name: "asset-operator-css",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/css; charset=utf-8"),
            success_status: 200,
            path: "/assets/operator/operator.css",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "GET",
            name: "asset-operator-shared-js",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/javascript; charset=utf-8"),
            success_status: 200,
            path: "/assets/operator/shared.js",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "GET",
            name: "asset-operator-dashboard-js",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/javascript; charset=utf-8"),
            success_status: 200,
            path: "/assets/operator/dashboard.js",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "GET",
            name: "asset-operator-settings-js",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/javascript; charset=utf-8"),
            success_status: 200,
            path: "/assets/operator/settings.js",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "GET",
            name: "asset-operator-locale",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/assets/operator/locales/en-US.json",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: true,
            expectations: [
                contract_expectation(302, None),
                contract_expectation(302, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
            ],
            method: "GET",
            name: "root-page",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/html; charset=utf-8"),
            success_status: 200,
            path: "/",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: true,
            expectations: [
                contract_expectation(302, None),
                contract_expectation(302, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
            ],
            method: "GET",
            name: "dash-page",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/html; charset=utf-8"),
            success_status: 200,
            path: "/dash",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "GET",
            name: "metrics",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/plain; charset=utf-8"),
            success_status: 200,
            path: "/metrics",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "GET",
            name: "api-dashboard",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/dashboard",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "GET",
            name: "api-dashboard-now",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/dashboard/now",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "GET",
            name: "api-config",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/config",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "POST",
            name: "api-settings-nim-keys",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::DurableConfig,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/settings/nim-keys",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "POST",
            name: "api-settings-clients",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::DurableConfig,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/settings/clients",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAdmin,
            accept_html: false,
            expectations: configured_contract_expectations(200, 403),
            method: "POST",
            name: "api-settings-limits",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::DurableConfig,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/settings/limits",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAdmin,
            accept_html: false,
            expectations: configured_contract_expectations(200, 403),
            method: "POST",
            name: "api-settings-history",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::DurableConfig,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/settings/history",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAdmin,
            accept_html: false,
            expectations: configured_contract_expectations(200, 403),
            method: "POST",
            name: "api-settings-governor",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::DurableConfig,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/settings/governor",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAdmin,
            accept_html: false,
            expectations: configured_contract_expectations(200, 403),
            method: "POST",
            name: "api-settings-users",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::DurableConfig,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/settings/users",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "POST",
            name: "api-settings-validate-key",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::Upstream,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/settings/validate-key",
        },
        RouteBehavior {
            access: ContractAccess::Client,
            accept_html: false,
            expectations: [
                contract_expectation(503, Some("setup_required")),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
                contract_expectation(200, None),
            ],
            method: "POST",
            name: "v1-wildcard",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::Upstream,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/v1/chat/completions",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAdmin,
            accept_html: false,
            expectations: configured_contract_expectations(200, 403),
            method: "POST",
            name: "api-settings-upstream",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::DurableConfig,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/settings/upstream",
        },
        RouteBehavior {
            access: ContractAccess::Public,
            accept_html: true,
            expectations: [
                contract_expectation(302, None),
                contract_expectation(200, None),
                contract_expectation(302, None),
                contract_expectation(302, None),
                contract_expectation(302, None),
            ],
            method: "GET",
            name: "login-get",
            phase: ContractPhase::Always,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/html; charset=utf-8"),
            success_status: 200,
            path: "/login",
        },
        RouteBehavior {
            access: ContractAccess::Public,
            accept_html: true,
            expectations: [
                contract_expectation(302, None),
                contract_expectation(303, None),
                contract_expectation(303, None),
                contract_expectation(303, None),
                contract_expectation(303, None),
            ],
            method: "POST",
            name: "login-post",
            phase: ContractPhase::Always,
            request: ContractRequest::Form,
            side_effect: ContractSideEffect::SessionCookie,
            success_content_type: None,
            success_status: 303,
            path: "/login",
        },
        RouteBehavior {
            access: ContractAccess::Public,
            accept_html: true,
            expectations: [
                contract_expectation(303, None),
                contract_expectation(303, None),
                contract_expectation(303, None),
                contract_expectation(303, None),
                contract_expectation(303, None),
            ],
            method: "POST",
            name: "logout",
            phase: ContractPhase::Always,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::SessionCookie,
            success_content_type: None,
            success_status: 303,
            path: "/logout",
        },
        RouteBehavior {
            access: ContractAccess::Public,
            accept_html: true,
            expectations: [
                contract_expectation(200, None),
                contract_expectation(404, None),
                contract_expectation(404, None),
                contract_expectation(404, None),
                contract_expectation(404, None),
            ],
            method: "GET",
            name: "setup-get",
            phase: ContractPhase::PreSetup,
            request: ContractRequest::None,
            side_effect: ContractSideEffect::None,
            success_content_type: Some("text/html; charset=utf-8"),
            success_status: 200,
            path: "/setup",
        },
        RouteBehavior {
            access: ContractAccess::Public,
            accept_html: false,
            expectations: [
                contract_expectation(200, None),
                contract_expectation(409, Some("setup_complete")),
                contract_expectation(409, Some("setup_complete")),
                contract_expectation(409, Some("setup_complete")),
                contract_expectation(409, Some("setup_complete")),
            ],
            method: "POST",
            name: "setup-validate-key",
            phase: ContractPhase::PreSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::Upstream,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/setup/validate-key",
        },
        RouteBehavior {
            access: ContractAccess::OperatorAdmin,
            accept_html: false,
            expectations: configured_contract_expectations(200, 403),
            method: "POST",
            name: "api-settings-locale",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::DurableConfigIdempotent,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/settings/locale",
        },
        // Password rotation invalidates every prior cookie for that actor, so
        // keep this after all other authenticated probes.
        RouteBehavior {
            access: ContractAccess::OperatorAny,
            accept_html: false,
            expectations: configured_contract_expectations(200, 200),
            method: "POST",
            name: "api-settings-account",
            phase: ContractPhase::PostSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::DurableConfigAndSession,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/api/settings/account",
        },
        // Keep the successful claim last: it intentionally closes the
        // before-setup fixture for every subsequent request.
        RouteBehavior {
            access: ContractAccess::Public,
            accept_html: false,
            expectations: [
                contract_expectation(200, None),
                contract_expectation(409, Some("setup_complete")),
                contract_expectation(409, Some("setup_complete")),
                contract_expectation(409, Some("setup_complete")),
                contract_expectation(409, Some("setup_complete")),
            ],
            method: "POST",
            name: "setup-post",
            phase: ContractPhase::PreSetup,
            request: ContractRequest::Json,
            side_effect: ContractSideEffect::DurableConfigAndSession,
            success_content_type: Some("application/json"),
            success_status: 200,
            path: "/setup",
        },
    ];

    assert_eq!(rows.len(), 34, "route-contract:inventory");

    let mock = start_mock().await;
    let before_setup = start_proxy_fresh().await;
    let configured = start_proxy_with(
        &mock.url,
        StoreOpts {
            extra_users: vec![
                ("matrix-user".into(), "user".into()),
                ("matrix-admin".into(), "admin".into()),
            ],
            ..Default::default()
        },
        &[],
    )
    .await;
    let user_cookie = login_as(&configured, "matrix-user").await;
    let admin_cookie = login_as(&configured, "matrix-admin").await;
    let superuser_cookie = login(&configured).await;

    for row in &rows {
        for actor in ContractActor::ALL {
            let (proxy, cookie) = match actor {
                ContractActor::BeforeSetup => (&before_setup, None),
                ContractActor::Anonymous => (&configured, None),
                ContractActor::User => (&configured, Some(user_cookie.as_str())),
                ContractActor::Admin => (&configured, Some(admin_cookie.as_str())),
                ContractActor::Superuser => (&configured, Some(superuser_cookie.as_str())),
            };
            let expected = row.expectations[actor.index()];
            let config_before = contract_config_bytes(proxy);
            let upstream_before = contract_upstream_calls(&mock);
            let response = send_contract_request(row, actor, proxy, cookie, &mock).await;
            let actual_status = response.status().as_u16();
            assert_eq!(
                actual_status, expected.status,
                "route-contract:phase/auth: {} {} actor={actor:?} access={:?} phase={:?}",
                row.method, row.path, row.access, row.phase
            );

            let succeeded = actual_status == row.success_status;
            if succeeded {
                if let Some(content_type) = row.success_content_type {
                    assert_eq!(
                        response
                            .headers()
                            .get(CONTENT_TYPE)
                            .and_then(|value| value.to_str().ok()),
                        Some(content_type),
                        "route-contract:success-content-type: {} {} actor={actor:?}",
                        row.method,
                        row.path
                    );
                }
            }

            let response_content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let has_session_cookie = response.headers().contains_key("set-cookie");
            let body = response.bytes().await.unwrap();
            if let Some(error) = expected.error {
                assert_eq!(
                    response_content_type.as_deref(),
                    Some("application/json"),
                    "route-contract:error-content-type: {} {} actor={actor:?}",
                    row.method,
                    row.path
                );
                let value: serde_json::Value = serde_json::from_slice(&body).unwrap_or_else(|e| {
                    panic!(
                        "route-contract:error: {} {} actor={actor:?} was not JSON: {e}",
                        row.method, row.path
                    )
                });
                assert_eq!(
                    value["error"]["code"], error,
                    "route-contract:error: {} {} actor={actor:?}",
                    row.method, row.path
                );
            }

            let config_after = contract_config_bytes(proxy);
            let upstream_after = contract_upstream_calls(&mock);
            match row.side_effect {
                ContractSideEffect::None => {
                    assert_eq!(
                        config_after, config_before,
                        "route-contract:durable-bytes: {} {} actor={actor:?}",
                        row.method, row.path
                    );
                    assert_eq!(
                        upstream_after, upstream_before,
                        "route-contract:side-effect: {} {} actor={actor:?}",
                        row.method, row.path
                    );
                }
                ContractSideEffect::SessionCookie => {
                    assert_eq!(
                        config_after, config_before,
                        "route-contract:durable-bytes: {} {} actor={actor:?}",
                        row.method, row.path
                    );
                    assert_eq!(
                        has_session_cookie, succeeded,
                        "route-contract:session: {} {} actor={actor:?}",
                        row.method, row.path
                    );
                    assert_eq!(
                        upstream_after, upstream_before,
                        "route-contract:side-effect: {} {} actor={actor:?}",
                        row.method, row.path
                    );
                }
                ContractSideEffect::DurableConfig => {
                    if succeeded {
                        assert_contract_durable_change(
                            &config_before,
                            &config_after,
                            &format!("{} {} actor={actor:?}", row.method, row.path),
                        );
                    } else {
                        assert_eq!(
                            config_after, config_before,
                            "route-contract:durable-bytes: rejected {} {} actor={actor:?}",
                            row.method, row.path
                        );
                    }
                    assert_eq!(
                        upstream_after, upstream_before,
                        "route-contract:side-effect: {} {} actor={actor:?}",
                        row.method, row.path
                    );
                }
                ContractSideEffect::DurableConfigIdempotent => {
                    if succeeded {
                        assert!(
                            config_after.is_some(),
                            "route-contract:durable-bytes: successful {} {} actor={actor:?} removed config.json",
                            row.method,
                            row.path
                        );
                    } else {
                        assert_eq!(
                            config_after, config_before,
                            "route-contract:durable-bytes: rejected {} {} actor={actor:?}",
                            row.method, row.path
                        );
                    }
                    assert_eq!(
                        upstream_after, upstream_before,
                        "route-contract:side-effect: {} {} actor={actor:?}",
                        row.method, row.path
                    );
                }
                ContractSideEffect::DurableConfigAndSession => {
                    if succeeded {
                        assert_contract_durable_change(
                            &config_before,
                            &config_after,
                            &format!("{} {} actor={actor:?}", row.method, row.path),
                        );
                    } else {
                        assert_eq!(
                            config_after, config_before,
                            "route-contract:durable-bytes: rejected {} {} actor={actor:?}",
                            row.method, row.path
                        );
                    }
                    assert_eq!(
                        has_session_cookie, succeeded,
                        "route-contract:session: {} {} actor={actor:?}",
                        row.method, row.path
                    );
                    assert_eq!(
                        upstream_after, upstream_before,
                        "route-contract:side-effect: {} {} actor={actor:?}",
                        row.method, row.path
                    );
                }
                ContractSideEffect::Upstream => {
                    assert_eq!(
                        upstream_after,
                        upstream_before + usize::from(succeeded),
                        "route-contract:side-effect: {} {} actor={actor:?}",
                        row.method,
                        row.path
                    );
                    assert_eq!(
                        config_after, config_before,
                        "route-contract:durable-bytes: {} {} actor={actor:?}",
                        row.method, row.path
                    );
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ClientAuthBehavior {
    bearer: Option<&'static str>,
    error: Option<&'static str>,
    status: u16,
    upstream: bool,
}

#[tokio::test]
async fn route_contract_client_auth_matrix() {
    let mock = start_mock().await;
    let before_setup = start_proxy_fresh().await;
    let keyed_proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            clients: vec![("matrix-client".into(), "matrix-secret".into())],
            open: false,
            ..Default::default()
        },
        &[],
    )
    .await;
    let rows = [
        (
            &before_setup,
            ClientAuthBehavior {
                bearer: None,
                error: Some("setup_required"),
                status: 503,
                upstream: false,
            },
        ),
        (
            &keyed_proxy,
            ClientAuthBehavior {
                bearer: None,
                error: Some("unauthorized"),
                status: 401,
                upstream: false,
            },
        ),
        (
            &keyed_proxy,
            ClientAuthBehavior {
                bearer: Some("wrong-secret"),
                error: Some("unauthorized"),
                status: 401,
                upstream: false,
            },
        ),
        (
            &keyed_proxy,
            ClientAuthBehavior {
                bearer: Some("matrix-secret"),
                error: None,
                status: 200,
                upstream: true,
            },
        ),
    ];

    for (proxy, row) in rows {
        let config_before = contract_config_bytes(proxy);
        let upstream_before = contract_upstream_calls(&mock);
        let mut request = no_redirect_client()
            .post(proxy.url("/v1/chat/completions"))
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&chat_body("route-contract-client-auth", false)).unwrap());
        if let Some(bearer) = row.bearer {
            request = request.bearer_auth(bearer);
        }
        let response = request.send().await.unwrap();
        assert_eq!(
            response.status().as_u16(),
            row.status,
            "route-contract:client-auth: {row:?}"
        );
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json"),
            "route-contract:client-auth-content-type: {row:?}"
        );
        let body: serde_json::Value = response.json().await.unwrap();
        if let Some(error) = row.error {
            assert_eq!(
                body["error"]["code"], error,
                "route-contract:client-auth: {row:?}"
            );
        }
        assert_eq!(
            contract_upstream_calls(&mock),
            upstream_before + usize::from(row.upstream),
            "route-contract:client-auth-side-effect: {row:?}"
        );
        assert_eq!(
            contract_config_bytes(proxy),
            config_before,
            "route-contract:client-auth-durable-bytes: {row:?}"
        );
    }
}

#[derive(Clone, Copy, Debug)]
enum ContractOwnership {
    Own,
    Other,
}

#[derive(Debug)]
struct OwnershipBehavior {
    body: serde_json::Value,
    durable_change: bool,
    ownership: ContractOwnership,
    path: &'static str,
    status: u16,
}

#[tokio::test]
async fn route_contract_ownership_matrix() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            clients: vec![("root-client".into(), "root-secret".into())],
            extra_users: vec![("matrix-user".into(), "user".into())],
            ..Default::default()
        },
        &[],
    )
    .await;
    let root_cookie = login(&proxy).await;
    let user_cookie = login_as(&proxy, "matrix-user").await;

    let root_nim_fingerprint = api_config(&proxy, &root_cookie).await["nim_keys"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = post_json(
        &proxy,
        &user_cookie,
        "/api/settings/nim-keys",
        serde_json::json!({"add": {"key": "matrix-user-nim", "rpm": 40}}),
    )
    .await;
    assert_eq!(status, 200, "ownership fixture add failed: {body}");
    let user_nim_fingerprint = api_config(&proxy, &user_cookie).await["nim_keys"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = post_json(
        &proxy,
        &user_cookie,
        "/api/settings/clients",
        serde_json::json!({"add": {"name": "user-client"}}),
    )
    .await;
    assert_eq!(status, 200, "ownership fixture add failed: {body}");

    let rows = vec![
        OwnershipBehavior {
            body: serde_json::json!({
                "set": {"fingerprint": user_nim_fingerprint, "rpm": 42}
            }),
            durable_change: true,
            ownership: ContractOwnership::Own,
            path: "/api/settings/nim-keys",
            status: 200,
        },
        OwnershipBehavior {
            body: serde_json::json!({
                "set": {"fingerprint": root_nim_fingerprint, "rpm": 42}
            }),
            durable_change: false,
            ownership: ContractOwnership::Other,
            path: "/api/settings/nim-keys",
            status: 403,
        },
        OwnershipBehavior {
            body: serde_json::json!({"remove": "user-client"}),
            durable_change: true,
            ownership: ContractOwnership::Own,
            path: "/api/settings/clients",
            status: 200,
        },
        OwnershipBehavior {
            body: serde_json::json!({"remove": "root-client"}),
            durable_change: false,
            ownership: ContractOwnership::Other,
            path: "/api/settings/clients",
            status: 403,
        },
    ];

    for row in rows {
        let before = contract_config_bytes(&proxy);
        let (status, body) = post_json(&proxy, &user_cookie, row.path, row.body).await;
        assert_eq!(
            status.as_u16(),
            row.status,
            "route-contract:ownership: {:?} {}: {body}",
            row.ownership,
            row.path
        );
        let after = contract_config_bytes(&proxy);
        if row.durable_change {
            assert_contract_durable_change(
                &before,
                &after,
                &format!("{:?} {}", row.ownership, row.path),
            );
        } else {
            assert_eq!(
                after, before,
                "route-contract:durable-bytes: {:?} {}",
                row.ownership, row.path
            );
            assert_eq!(
                body["error"]["code"], "forbidden",
                "route-contract:ownership: {:?} {}",
                row.ownership, row.path
            );
        }
    }
}

#[test]
#[should_panic(expected = "route-contract:durable-bytes")]
fn route_contract_durable_self_test_names_missing_post_write_file() {
    assert_contract_durable_change(
        &Some(b"before".to_vec()),
        &None,
        "self-test configured writer",
    );
}

/// Send only headers for an over-limit body. `Expect: 100-continue` keeps the
/// client from streaming 64 MiB into a setup route that must reject its closed
/// phase before body buffering; the request limit still sees the declared size.
async fn assert_closed_setup_rejects_oversized_body(proxy: &support::Proxy, path: &str) {
    const MAX_RESPONSE_BYTES: u64 = 4 * 1024;

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", proxy.port))
        .await
        .unwrap();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n",
        64 * 1024 * 1024 + 1,
    );
    stream.write_all(request.as_bytes()).await.unwrap();

    let mut response = Vec::new();
    let mut bounded = stream.take(MAX_RESPONSE_BYTES + 1);
    tokio::time::timeout(Duration::from_secs(2), bounded.read_to_end(&mut response))
        .await
        .expect("closed setup route should respond without the oversized body")
        .unwrap();
    assert!(
        response.len() <= MAX_RESPONSE_BYTES as usize,
        "closed setup response exceeded {MAX_RESPONSE_BYTES} bytes"
    );
    let response = String::from_utf8(response).unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(headers.starts_with("HTTP/1.1 409"), "{headers}");
    assert!(
        headers.contains("content-type: application/json"),
        "{headers}"
    );
    assert_eq!(
        body,
        r#"{"error":{"code":"setup_complete","message":"setup is already complete","type":"proxy_error"}}"#
    );
}

/// A keyed-`/v1` fixture: one client key (name, secret), otherwise defaults.
fn keyed(name: &str, secret: &str) -> StoreOpts {
    StoreOpts {
        open: false,
        clients: vec![(name.into(), secret.into())],
        ..Default::default()
    }
}

async fn send_successful_chats(proxy: &support::Proxy, count: usize) {
    for request in 0..count {
        let response = client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body(&format!("history request {request}"), false))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
}

async fn dashboard_range(
    proxy: &support::Proxy,
    cookie: &str,
    from: u64,
    to: u64,
    points: usize,
) -> serde_json::Value {
    let response = client()
        .get(proxy.url(&format!(
            "/api/dashboard?from={from}&to={to}&points={points}"
        )))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

async fn dashboard_now(proxy: &support::Proxy, cookie: &str) -> serde_json::Value {
    let response = client()
        .get(proxy.url("/api/dashboard/now"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

fn successful_chat_requests(rows: &serde_json::Value) -> f64 {
    rows.as_array()
        .unwrap()
        .iter()
        .filter(|row| {
            row["metric"] == "nimproxy_requests_total"
                && row["labels"]["path"] == "/v1/chat/completions"
                && row["labels"]["status"] == "200"
        })
        .filter_map(|row| row["value"].as_f64())
        .sum()
}

async fn wait_for_persisted_chat_total(
    proxy: &support::Proxy,
    cookie: &str,
    after_revision: u64,
    expected_total: f64,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let range = dashboard_range(proxy, cookie, 1, 4_102_444_800, 1000).await;
        let revision = range["history_revision"].as_u64().unwrap();
        if revision > after_revision && successful_chat_requests(&range["totals"]) == expected_total
        {
            return range;
        }
        assert!(
            Instant::now() < deadline,
            "history did not reach revision > {after_revision} and request total \
             {expected_total}: {range}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum CanonicalFixtureKind {
    Boot,
    Sample,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum CanonicalFixtureStateKind {
    Counter,
}

#[derive(serde::Serialize)]
struct CanonicalFixtureCapacity {
    capacity_rpm: usize,
    enabled_keys: usize,
    key_rpms: [usize; 3],
}

#[derive(serde::Serialize)]
struct CanonicalFixtureState {
    kind: CanonicalFixtureStateKind,
    metric: &'static str,
    labels: BTreeMap<String, String>,
    value: f64,
}

#[derive(serde::Serialize)]
struct CanonicalFixtureRow {
    format: &'static str,
    v: u8,
    kind: CanonicalFixtureKind,
    timestamp: u64,
    boot_id: &'static str,
    capacity: CanonicalFixtureCapacity,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<Vec<CanonicalFixtureState>>,
}

fn canonical_history_row(
    kind: CanonicalFixtureKind,
    timestamp: u64,
    value: Option<f64>,
) -> CanonicalFixtureRow {
    let labels = [("client".to_owned(), "retention-seed".to_owned())]
        .into_iter()
        .collect();
    CanonicalFixtureRow {
        format: "nimproxy-history",
        v: 1,
        kind,
        timestamp,
        boot_id: "seed-boot",
        capacity: CanonicalFixtureCapacity {
            capacity_rpm: 120,
            enabled_keys: 3,
            key_rpms: [40, 40, 40],
        },
        state: value.map(|value| {
            vec![CanonicalFixtureState {
                kind: CanonicalFixtureStateKind::Counter,
                metric: "nimproxy_seeded_requests_total",
                labels,
                value,
            }]
        }),
    }
}

struct RetentionFixture {
    data_dir: std::path::PathBuf,
    canonical: std::path::PathBuf,
    now: u64,
    cutoff: u64,
    seed_boot_timestamp: u64,
    ancient_seed_sample_timestamp: u64,
}

fn retention_fixture(upstream: &str) -> RetentionFixture {
    const HORIZON_DAYS: u64 = 30;
    const HORIZON_SECONDS: u64 = HORIZON_DAYS * 86_400;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cutoff = now.saturating_sub(HORIZON_SECONDS);
    let seed_boot_timestamp = now.saturating_sub(HORIZON_SECONDS * 3);
    let ancient_seed_sample_timestamp = now.saturating_sub(HORIZON_SECONDS * 3 - 1);
    let data_dir = scratch_data_dir();
    let mut config = StoreOpts::default().json(upstream);
    config["history"] = serde_json::json!({"days": HORIZON_DAYS});
    std::fs::write(
        data_dir.join("config.json"),
        serde_json::to_vec_pretty(&config).unwrap(),
    )
    .unwrap();

    let seed = [
        canonical_history_row(CanonicalFixtureKind::Boot, seed_boot_timestamp, None),
        canonical_history_row(
            CanonicalFixtureKind::Sample,
            ancient_seed_sample_timestamp,
            Some(1.0),
        ),
        canonical_history_row(
            CanonicalFixtureKind::Sample,
            cutoff.saturating_sub(1),
            Some(2.0),
        ),
        canonical_history_row(
            CanonicalFixtureKind::Sample,
            cutoff.saturating_add(600),
            Some(3.0),
        ),
    ];
    let mut seed_bytes = Vec::new();
    for row in seed {
        let line = serde_json::to_vec(&row).unwrap();
        seed_bytes.extend(line);
        seed_bytes.push(b'\n');
    }
    // Readiness validates this raw seed with the private production codec;
    // reopening the same directory validates every later replacement again.
    let canonical = data_dir.join("history-v1.jsonl");
    std::fs::write(&canonical, seed_bytes).unwrap();

    RetentionFixture {
        data_dir,
        canonical,
        now,
        cutoff,
        seed_boot_timestamp,
        ancient_seed_sample_timestamp,
    }
}

fn read_canonical_history(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .expect("history-retention: canonical history remains readable")
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let row: serde_json::Value = serde_json::from_str(line)
                .expect("history-retention: canonical row remains valid JSON");
            assert_eq!(row["format"], "nimproxy-history");
            assert_eq!(row["v"], 1);
            assert!(row["kind"].is_string());
            assert!(row["timestamp"].is_u64());
            assert!(row["boot_id"].is_string());
            assert!(row["capacity"].is_object());
            row
        })
        .collect()
}

fn idle_metric_snapshot(metrics: &str) -> Vec<&str> {
    let mut rows: Vec<_> = metrics
        .lines()
        .filter(|line| {
            line.starts_with("nimproxy_requests_total")
                || line.starts_with("nimproxy_lane_requests_total")
                || line.starts_with("nimproxy_queue_wait_seconds_count")
                || line.starts_with("nimproxy_queue_wait_seconds_sum")
        })
        .collect();
    rows.sort_unstable();
    rows
}

async fn wait_for_idle_checkpoint(
    canonical: &std::path::Path,
    row_count: usize,
    sample_count: usize,
    checkpoint_count: usize,
) -> Vec<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let rows = read_canonical_history(canonical);
        let samples = rows.iter().filter(|row| row["kind"] == "sample").count();
        let checkpoints = rows
            .iter()
            .filter(|row| row["kind"] == "checkpoint")
            .count();
        if checkpoints > checkpoint_count {
            assert_eq!(
                samples, sample_count,
                "history-retention:idle-samples: an idle interval added a full sample instead of a checkpoint"
            );
            assert_eq!(
                checkpoints,
                checkpoint_count + 1,
                "history-retention:idle-checkpoints: idle cadence added more than one checkpoint"
            );
            assert_eq!(
                rows.len(),
                row_count + 1,
                "history-retention:idle-rows: idle cadence added more than one canonical row"
            );
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "history-retention:idle-checkpoint: sampler did not persist a checkpoint"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_retention_compaction(
    proxy: &support::Proxy,
    cookie: &str,
    canonical: &std::path::Path,
    ancient_seed_sample_timestamp: u64,
) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let configured = api_config(proxy, cookie).await;
        let compacted_rows = read_canonical_history(canonical);
        if configured["server"]["history"]["compaction_pending"] == false
            && !compacted_rows
                .iter()
                .any(|row| row["timestamp"] == ancient_seed_sample_timestamp)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "history-retention:compaction-idle: canonical retention did not settle before idle cadence"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn assert_retained_history_invariants(
    rows: &[serde_json::Value],
    proxy: &support::Proxy,
    fixture: &RetentionFixture,
) {
    let has_timestamp = |timestamp| rows.iter().any(|row| row["timestamp"] == timestamp);
    assert!(
        rows.windows(2).all(|pair| {
            pair[0]["timestamp"].as_u64().unwrap() <= pair[1]["timestamp"].as_u64().unwrap()
        }),
        "history-retention:physical-order: canonical rows retain physical timestamp order"
    );
    assert!(
        has_timestamp(fixture.cutoff.saturating_sub(1)),
        "history-retention:baseline: the preceding full-sample baseline is retained"
    );
    assert!(
        rows.iter().any(|row| {
            row["kind"] == "boot"
                && row["boot_id"] == "seed-boot"
                && row["timestamp"] == fixture.seed_boot_timestamp
        }),
        "history-retention:owning-boot: the baseline's owning boot is retained"
    );
    assert!(
        has_timestamp(fixture.cutoff.saturating_add(600)),
        "history-retention:horizon: the configured retained horizon is present"
    );
    assert!(
        rows.iter().any(|row| row["boot_id"] != "seed-boot"),
        "history-retention:fresh-epoch: a fresh process epoch is retained"
    );
    assert!(
        !has_timestamp(fixture.ancient_seed_sample_timestamp),
        "history-retention:expired: rows older than the required baseline are removed"
    );
    assert!(
        !proxy.data_dir.join("history.jsonl").exists(),
        "history-retention:legacy: canonical retention does not create the legacy path"
    );
}

async fn exercise_history_retention_restarts() {
    let mock = start_mock().await;
    let fixture = retention_fixture(&mock.url);
    let query_to = fixture.now.saturating_add(60);

    let mut proxy = start_proxy_in(fixture.data_dir.clone(), &[("HISTORY_SAMPLE_SECS", "1")]).await;
    let mut cookie = login(&proxy).await;
    let initial = dashboard_range(&proxy, &cookie, fixture.cutoff, query_to, 1000).await;
    send_successful_chats(&proxy, 1).await;
    let _ = wait_for_persisted_chat_total(
        &proxy,
        &cookie,
        initial["history_revision"].as_u64().unwrap(),
        1.0,
    )
    .await;
    let before_restart = dashboard_range(&proxy, &cookie, fixture.cutoff, query_to, 1000).await;
    let before_idle_metrics_text = metrics(&proxy).await;
    let before_idle_metrics = idle_metric_snapshot(&before_idle_metrics_text);
    wait_for_retention_compaction(
        &proxy,
        &cookie,
        &fixture.canonical,
        fixture.ancient_seed_sample_timestamp,
    )
    .await;
    let before_idle_rows = read_canonical_history(&fixture.canonical);
    let before_idle_samples = before_idle_rows
        .iter()
        .filter(|row| row["kind"] == "sample")
        .count();
    let before_idle_checkpoints = before_idle_rows
        .iter()
        .filter(|row| row["kind"] == "checkpoint")
        .count();
    let idle_rows = wait_for_idle_checkpoint(
        &fixture.canonical,
        before_idle_rows.len(),
        before_idle_samples,
        before_idle_checkpoints,
    )
    .await;
    let after_idle_metrics_text = metrics(&proxy).await;
    assert_eq!(
        idle_metric_snapshot(&after_idle_metrics_text),
        before_idle_metrics,
        "history-retention:idle-metrics: requests and scheduling do not advance without traffic"
    );

    proxy = restart(proxy, &[("HISTORY_SAMPLE_SECS", "3600")]).await;
    cookie = login(&proxy).await;
    let after_restart = dashboard_range(&proxy, &cookie, fixture.cutoff, query_to, 1000).await;
    assert_eq!(
        after_restart["totals"], before_restart["totals"],
        "history-retention:restart-totals: retained counter totals survive restart"
    );
    assert_eq!(
        after_restart["window"]["complete"], before_restart["window"]["complete"],
        "history-retention:restart-complete: retained-window completeness survives restart"
    );
    assert_eq!(
        after_restart["window"]["available_from"], before_restart["window"]["available_from"],
        "history-retention:restart-bounds: retained-window lower bound survives restart"
    );

    let rows = read_canonical_history(&fixture.canonical);
    assert_retained_history_invariants(&rows, &proxy, &fixture);
    assert!(
        idle_rows.iter().any(|row| row["kind"] == "checkpoint"),
        "history-retention:idle-checkpoint: idle cadence persists checkpoints"
    );
}

#[tokio::test]
async fn history_retention_survives_restart() {
    exercise_history_retention_restarts().await;
}

/// Long-form release proof for bounded canonical retention. The seed spans
/// more than two retention horizons without waiting for wall-clock history.
#[tokio::test]
#[ignore]
async fn release_restart_and_idle_history() {
    const RELEASE_CYCLES: u64 = 3;

    let mock = start_mock().await;
    let fixture = retention_fixture(&mock.url);
    let query_to = fixture.now.saturating_add(60);
    let mut proxy = start_proxy_in(fixture.data_dir.clone(), &[("HISTORY_SAMPLE_SECS", "1")]).await;
    let mut cookie = login(&proxy).await;
    let initial = dashboard_range(&proxy, &cookie, fixture.cutoff, query_to, 1000).await;
    send_successful_chats(&proxy, 1).await;
    let _ = wait_for_persisted_chat_total(
        &proxy,
        &cookie,
        initial["history_revision"].as_u64().unwrap(),
        1.0,
    )
    .await;
    wait_for_retention_compaction(
        &proxy,
        &cookie,
        &fixture.canonical,
        fixture.ancient_seed_sample_timestamp,
    )
    .await;

    let retained_before_restarts =
        dashboard_range(&proxy, &cookie, fixture.cutoff, query_to, 1000).await;
    let pre_idle_bytes = std::fs::metadata(&fixture.canonical)
        .expect("release-restart-idle-history:pre-idle-bytes: canonical history remains present")
        .len();
    let mut checkpoint_byte_allowance = None;

    for cycle in 0..RELEASE_CYCLES {
        let before_rows = read_canonical_history(&fixture.canonical);
        let before_samples = before_rows
            .iter()
            .filter(|row| row["kind"] == "sample")
            .count();
        let before_checkpoints = before_rows
            .iter()
            .filter(|row| row["kind"] == "checkpoint")
            .count();
        let before_bytes = std::fs::metadata(&fixture.canonical)
            .expect("release-restart-idle-history:checkpoint-before-bytes: canonical history remains present")
            .len();
        let before_metrics_text = metrics(&proxy).await;
        let before_metrics = idle_metric_snapshot(&before_metrics_text);
        assert!(
            [
                "nimproxy_requests_total",
                "nimproxy_lane_requests_total",
                "nimproxy_queue_wait_seconds_count",
                "nimproxy_queue_wait_seconds_sum",
            ]
            .iter()
            .all(|metric| before_metrics.iter().any(|row| row.starts_with(metric))),
            "release-restart-idle-history:idle-metric-rows: cycle {cycle} lacks a selected metric row"
        );

        let _ = wait_for_idle_checkpoint(
            &fixture.canonical,
            before_rows.len(),
            before_samples,
            before_checkpoints,
        )
        .await;
        let after_bytes = std::fs::metadata(&fixture.canonical)
            .expect("release-restart-idle-history:checkpoint-after-bytes: canonical history remains present")
            .len();
        let after_metrics_text = metrics(&proxy).await;
        assert_eq!(
            idle_metric_snapshot(&after_metrics_text),
            before_metrics,
            "release-restart-idle-history:idle-metrics: cycle {cycle} changed request or scheduling metrics without traffic"
        );

        let checkpoint_delta = after_bytes.checked_sub(before_bytes).expect(
            "release-restart-idle-history:checkpoint-byte-delta: canonical bytes shrank during an idle checkpoint",
        );
        if cycle == 0 {
            assert_eq!(
                before_bytes, pre_idle_bytes,
                "release-restart-idle-history:pre-idle-base: first idle cycle did not start at the recorded base"
            );
            assert!(
                checkpoint_delta > 0,
                "release-restart-idle-history:checkpoint-byte-allowance-positive: first checkpoint must establish a positive allowance"
            );
            checkpoint_byte_allowance = Some(checkpoint_delta);
        } else {
            assert!(
                checkpoint_delta
                    <= checkpoint_byte_allowance.expect(
                        "release-restart-idle-history:checkpoint-byte-allowance: first cycle did not establish an allowance",
                    ),
                "release-restart-idle-history:checkpoint-byte-allowance: cycle {cycle} exceeded the first checkpoint allowance"
            );
        }
    }

    let post_idle_bytes = std::fs::metadata(&fixture.canonical)
        .expect("release-restart-idle-history:post-idle-bytes: canonical history remains present")
        .len();
    let mut restart_byte_allowance = None;
    let mut restart_boot_ids = BTreeSet::new();
    let kind_counts = |rows: &[serde_json::Value]| {
        ["boot", "sample", "checkpoint"]
            .map(|kind| rows.iter().filter(|row| row["kind"] == kind).count())
    };

    for cycle in 0..RELEASE_CYCLES {
        let before_rows = read_canonical_history(&fixture.canonical);
        let before_file_bytes = std::fs::read(&fixture.canonical).expect(
            "release-restart-idle-history:restart-before-bytes: canonical history remains readable",
        );
        let before_bytes = u64::try_from(before_file_bytes.len()).expect(
            "release-restart-idle-history:restart-before-byte-length: canonical byte length exceeds u64",
        );
        if cycle == 0 {
            assert_eq!(
                before_bytes, post_idle_bytes,
                "release-restart-idle-history:post-idle-base: first restart did not start at the post-idle byte snapshot"
            );
        }
        let before_boot_ids: BTreeSet<_> = before_rows
            .iter()
            .filter(|row| row["kind"] == "boot")
            .map(|row| {
                row["boot_id"]
                    .as_str()
                    .expect("release-restart-idle-history:restart-boot-id: boot id is a string")
                    .to_owned()
            })
            .collect();
        let before_kind_counts = kind_counts(&before_rows);

        proxy = restart(proxy, &[("HISTORY_SAMPLE_SECS", "3600")]).await;
        let after_rows = read_canonical_history(&fixture.canonical);
        let after_file_bytes = std::fs::read(&fixture.canonical).expect(
            "release-restart-idle-history:restart-after-bytes: canonical history remains readable",
        );
        let after_bytes = u64::try_from(after_file_bytes.len()).expect(
            "release-restart-idle-history:restart-after-byte-length: canonical byte length exceeds u64",
        );
        assert!(
            after_file_bytes.starts_with(&before_file_bytes),
            "release-restart-idle-history:restart-byte-prefix: cycle {cycle} changed pre-restart canonical bytes"
        );
        let expected_row_count = before_rows
            .len()
            .checked_add(2)
            .expect("release-restart-idle-history:restart-row-kind-counts: row count overflowed");
        assert_eq!(
            after_rows.len(), expected_row_count,
            "release-restart-idle-history:restart-row-kind-counts: cycle {cycle} did not append exactly two rows"
        );
        assert_eq!(
            &after_rows[..before_rows.len()],
            before_rows.as_slice(),
            "release-restart-idle-history:restart-prefix: cycle {cycle} changed pre-restart canonical rows"
        );
        let tail = &after_rows[before_rows.len()..];
        assert_eq!(
            tail.iter()
                .map(|row| row["kind"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["boot", "sample"],
            "release-restart-idle-history:restart-row-kinds: cycle {cycle} tail is not boot then sample"
        );
        assert_eq!(
            kind_counts(&after_rows),
            [
                before_kind_counts[0]
                    .checked_add(1)
                    .expect("release-restart-idle-history:restart-row-kind-counts: boot count overflowed"),
                before_kind_counts[1]
                    .checked_add(1)
                    .expect("release-restart-idle-history:restart-row-kind-counts: sample count overflowed"),
                before_kind_counts[2],
            ],
            "release-restart-idle-history:restart-row-kind-counts: cycle {cycle} did not add exactly one boot, one sample, and zero checkpoints"
        );

        let boot_id = tail[0]["boot_id"]
            .as_str()
            .expect("release-restart-idle-history:restart-boot-id: boot id is a string");
        assert_eq!(
            tail[1]["boot_id"], boot_id,
            "release-restart-idle-history:restart-boot-id: cycle {cycle} boot and sample ids differ"
        );
        assert!(
            !before_boot_ids.contains(boot_id),
            "release-restart-idle-history:restart-boot-id: cycle {cycle} reused a pre-restart boot id"
        );
        assert!(
            restart_boot_ids.insert(boot_id.to_owned()),
            "release-restart-idle-history:restart-boot-id: cycle {cycle} reused a release restart boot id"
        );

        let restart_delta = after_bytes.checked_sub(before_bytes).expect(
            "release-restart-idle-history:restart-byte-delta: canonical bytes shrank during restart",
        );
        if cycle == 0 {
            assert!(
                restart_delta > 0,
                "release-restart-idle-history:restart-byte-allowance-positive: first restart must establish a positive allowance"
            );
            restart_byte_allowance = Some(restart_delta);
        } else {
            assert!(
                restart_delta
                    <= restart_byte_allowance.expect(
                        "release-restart-idle-history:restart-byte-allowance: first restart did not establish an allowance",
                    ),
                "release-restart-idle-history:restart-byte-allowance: cycle {cycle} exceeded the first restart allowance"
            );
        }

        cookie = login(&proxy).await;
        let configured = api_config(&proxy, &cookie).await;
        assert_eq!(
            configured["server"]["history"]["compaction_pending"], false,
            "release-restart-idle-history:restart-compaction: cycle {cycle} left unclassified compaction work"
        );
        let after_restart = dashboard_range(&proxy, &cookie, fixture.cutoff, query_to, 1000).await;
        assert_eq!(
            after_restart["totals"], retained_before_restarts["totals"],
            "release-restart-idle-history:restart-totals: cycle {cycle} changed retained totals"
        );
        assert_eq!(
            after_restart["window"]["complete"],
            retained_before_restarts["window"]["complete"],
            "release-restart-idle-history:restart-complete: cycle {cycle} changed retained completeness"
        );
        assert_eq!(
            after_restart["window"]["available_from"],
            retained_before_restarts["window"]["available_from"],
            "release-restart-idle-history:restart-bounds: cycle {cycle} changed retained lower bound"
        );
    }

    assert_eq!(
        restart_boot_ids.len(),
        usize::try_from(RELEASE_CYCLES)
            .expect("release-restart-idle-history:restart-epoch-count: cycle count fits usize"),
        "release-restart-idle-history:restart-epoch-count: every restart produced a unique boot id"
    );
    let checkpoint_byte_allowance = checkpoint_byte_allowance.expect(
        "release-restart-idle-history:checkpoint-byte-allowance: first checkpoint did not establish an allowance",
    );
    let restart_byte_allowance = restart_byte_allowance.expect(
        "release-restart-idle-history:restart-byte-allowance: first restart did not establish an allowance",
    );
    let final_byte_bound = pre_idle_bytes
        .checked_add(
            checkpoint_byte_allowance
                .checked_mul(RELEASE_CYCLES)
                .expect("release-restart-idle-history:final-byte-bound: checkpoint multiplication overflowed"),
        )
        .and_then(|bound| {
            restart_byte_allowance
                .checked_mul(RELEASE_CYCLES)
                .and_then(|restart_bytes| bound.checked_add(restart_bytes))
        })
        .expect("release-restart-idle-history:final-byte-bound: allowance arithmetic overflowed");
    let final_bytes = std::fs::metadata(&fixture.canonical)
        .expect("release-restart-idle-history:final-bytes: canonical history remains present")
        .len();
    assert!(
        final_bytes <= final_byte_bound,
        "release-restart-idle-history:final-byte-bound: final canonical bytes {final_bytes} exceed independent bound {final_byte_bound}"
    );

    let rows = read_canonical_history(&fixture.canonical);
    assert_retained_history_invariants(&rows, &proxy, &fixture);
}

#[tokio::test]
async fn open_mode_admits_requests_without_a_client_key() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "hello world");
}

#[tokio::test]
async fn keyed_mode_rejects_bad_tokens_and_accepts_good_ones() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(&mock.url, keyed("alice", "sekrit"), &[]).await;

    let missing = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 401);
    let body: serde_json::Value = missing.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unauthorized");

    let wrong = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth("nope")
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);

    let ok = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth("sekrit")
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    assert_eq!(
        mock.state.hit_count(),
        1,
        "only the authorized call reached upstream"
    );
}

#[tokio::test]
async fn deadline_header_validation_runs_after_auth_and_before_upstream() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(&mock.url, keyed("alice", "sekrit"), &[]).await;

    let unauthorized = client()
        .post(proxy.url("/v1/chat/completions"))
        .header("x-nim-proxy-deadline-ms", "not-a-number")
        .json(&chat_body("unauthorized", false))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 401, "auth fails before validation");

    let malformed = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth("sekrit")
        .header("x-nim-proxy-deadline-ms", "10.0")
        .json(&chat_body("malformed", false))
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), 400);
    let body: serde_json::Value = malformed.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_deadline");

    let duplicate = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth("sekrit")
        .header("x-nim-proxy-deadline-ms", "100")
        .header("x-nim-proxy-deadline-ms", "200")
        .json(&chat_body("duplicate", false))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), 400);
    let body: serde_json::Value = duplicate.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_deadline");
    assert_eq!(mock.state.hit_count(), 0, "invalid input never reaches NIM");
}

#[tokio::test]
async fn deadline_applies_to_models_cache_refresh() {
    use std::sync::atomic::Ordering;

    let mock = start_mock().await;
    mock.state.models_delay_ms.store(10_000, Ordering::SeqCst);
    let proxy = start_proxy(&mock.url, &[]).await;

    let started = Instant::now();
    let resp = client()
        .get(proxy.url("/v1/models"))
        .header("x-nim-proxy-deadline-ms", "100")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 504);
    assert!(started.elapsed() < Duration::from_secs(2));
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "deadline_exceeded");
}

#[tokio::test]
async fn buffered_deadline_cancels_header_wait_and_releases_inflight_slot() {
    let mock = start_mock().await;
    mock.state.push(Behavior::DelayHeaders(10_000));
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            max_inflight: 1,
            request_timeout_secs: 30,
            ..Default::default()
        },
        &[],
    )
    .await;

    let started = Instant::now();
    let expired = client()
        .post(proxy.url("/v1/chat/completions"))
        .header("x-nim-proxy-deadline-ms", "150")
        .json(&chat_body("deadline", false))
        .send()
        .await
        .unwrap();
    assert_eq!(expired.status(), 504);
    assert!(started.elapsed() < Duration::from_secs(2));
    let body: serde_json::Value = expired.json().await.unwrap();
    assert_eq!(body["error"]["code"], "deadline_exceeded");

    let after = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("after-deadline", false))
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), 200, "deadline released max_inflight slot");

    let metrics = metrics(&proxy).await;
    assert!(metrics.contains(
        r#"nimproxy_requests_total{client="local",model="mock/model-a",path="/v1/chat/completions",status="deadline"} 1"#
    ));
    assert!(metrics.contains(
        r#"nimproxy_deadline_exceeded_total{client="local",model="mock/model-a",path="/v1/chat/completions"} 1"#
    ));
}

#[tokio::test]
async fn streaming_deadline_stops_retry_wait() {
    let mock = start_mock().await;
    mock.state.push(Behavior::RateLimited(2));
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            nim_keys: vec![("only-key".into(), 40)],
            ..Default::default()
        },
        &[],
    )
    .await;

    let started = Instant::now();
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .header("x-nim-proxy-deadline-ms", "150")
        .json(&chat_body("retry-deadline", true))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "stream was already committed");
    let body = read_sse(resp).await;
    assert!(body.contains(": retrying"), "retry was observed: {body}");
    assert!(
        body.contains("deadline_exceeded"),
        "deadline surfaced: {body}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn streaming_deadline_stops_an_active_non_idle_stream() {
    let mock = start_mock().await;
    mock.state.push(Behavior::ActiveStream(25));
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            stream_idle_secs: 5,
            ..Default::default()
        },
        &[],
    )
    .await;

    let started = Instant::now();
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .header("x-nim-proxy-deadline-ms", "175")
        .json(&chat_body("active-deadline", true))
        .send()
        .await
        .unwrap();
    let body = read_sse(resp).await;
    assert!(
        body.matches("delta").count() >= 2,
        "stream stayed active: {body}"
    );
    assert!(
        body.contains("deadline_exceeded"),
        "deadline surfaced: {body}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn streaming_deadline_releases_inflight_when_downstream_is_not_reading() {
    let mock = start_mock().await;
    mock.state.push(Behavior::FloodStream);
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            max_inflight: 1,
            stream_idle_secs: 5,
            ..Default::default()
        },
        &[],
    )
    .await;

    let unread = client()
        .post(proxy.url("/v1/chat/completions"))
        .header("x-nim-proxy-deadline-ms", "75")
        .json(&chat_body("unread-deadline", true))
        .send()
        .await
        .unwrap();
    assert_eq!(unread.status(), 200);
    tokio::time::sleep(Duration::from_millis(250)).await;

    let after = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("after-unread-deadline", false))
        .send()
        .await
        .unwrap();
    assert_eq!(
        after.status(),
        200,
        "deadline cleanup cannot block on SSE send"
    );
}

#[tokio::test]
async fn streaming_rides_out_429s_with_lane_failover() {
    let mock = start_mock().await;
    mock.state.push(Behavior::RateLimited(1));
    mock.state.push(Behavior::RateLimited(1));
    let proxy = start_proxy(&mock.url, &[]).await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "SSE committed despite upstream 429s");
    let body = read_sse(resp).await;
    assert!(
        body.contains(": retrying"),
        "client saw retry comments: {body}"
    );
    assert!(body.contains("hello"), "stream delivered data: {body}");
    assert!(body.contains("data: [DONE]"));

    let keys = mock.state.hit_keys();
    assert_eq!(keys.len(), 3, "two 429s then a success");
    assert_ne!(keys[0], keys[1], "429 failed over to a different key");
}

#[tokio::test]
async fn retry_after_is_honored_when_only_one_lane_exists() {
    let mock = start_mock().await;
    mock.state.push(Behavior::RateLimited(1));
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            nim_keys: vec![("only-key".into(), 40)],
            ..Default::default()
        },
        &[],
    )
    .await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(mock.state.hit_count(), 2);
    let gap = mock.state.hit_gap(0, 1);
    assert!(
        gap >= Duration::from_millis(900),
        "waited Retry-After, gap {gap:?}"
    );
}

#[tokio::test]
async fn buffered_retries_5xx_then_returns_verbatim_body() {
    let mock = start_mock().await;
    mock.state.push(Behavior::ServerError(503));
    let proxy = start_proxy(&mock.url, &[]).await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["usage"]["prompt_tokens"], 11);
    assert_eq!(mock.state.hit_count(), 2);
}

#[tokio::test]
async fn non_retryable_error_is_relayed_buffered_and_surfaced_in_stream() {
    let mock = start_mock().await;
    mock.state.push(Behavior::BadRequest);
    // strict_passthrough disables usage injection so a streamed 400 can't be
    // masked by the injection-retry path — it surfaces in-stream instead.
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            strict_passthrough: true,
            ..Default::default()
        },
        &[],
    )
    .await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "buffered 400 relayed verbatim");
    assert!(resp.text().await.unwrap().contains("bad stream_options"));

    mock.state.push(Behavior::BadRequest);
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "stream already committed to 200");
    let body = read_sse(resp).await;
    assert!(
        body.contains("proxy_error"),
        "error surfaced in-stream: {body}"
    );
}

#[tokio::test]
async fn saturation_fails_fast_with_504() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            nim_keys: vec![("only-key".into(), 2)],
            max_wait_secs: 2,
            ..Default::default()
        },
        &[],
    )
    .await;

    for _ in 0..2 {
        let r = client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body("hi", false))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
    let third = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(third.status(), 504, "no slot within max_wait_secs");
    let v: serde_json::Value = third.json().await.unwrap();
    assert_eq!(v["error"]["code"], "rate_limited");
    assert_eq!(
        mock.state.hit_count(),
        2,
        "pacer let exactly the per-key rpm through"
    );
}

#[tokio::test]
async fn conversation_affinity_pins_a_conversation_to_one_key() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    for _ in 0..3 {
        let r = client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body("same conversation", false))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
    let keys = mock.state.hit_keys();
    assert_eq!(keys[0], keys[1]);
    assert_eq!(keys[1], keys[2], "conversation stayed on one key: {keys:?}");

    for i in 0..12 {
        let r = client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body(&format!("distinct conversation {i}"), false))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
    let distinct: std::collections::HashSet<String> = mock.state.hit_keys().into_iter().collect();
    assert!(
        distinct.len() >= 2,
        "distinct conversations spread across keys"
    );
}

#[tokio::test]
async fn models_catalog_is_cached_and_auth_gated() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(&mock.url, keyed("alice", "sekrit"), &[]).await;

    let unauth = client().get(proxy.url("/v1/models")).send().await.unwrap();
    assert_eq!(unauth.status(), 401);

    for _ in 0..3 {
        let r = client()
            .get(proxy.url("/v1/models"))
            .bearer_auth("sekrit")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v["data"][0]["id"], "mock/model-a");
    }
    assert_eq!(
        mock.state
            .models_hits
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "catalog served from cache after first fetch"
    );
}

#[tokio::test]
async fn observation_preserves_upstream_bytes() {
    // Mutation caught: the old side-band reader accepts an invalid reasoning
    // value (3 > completion 2) and records it, instead of omitting it while
    // preserving every upstream byte at the proxy boundary.
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    for fixture in [
        include_str!("fixtures/nim-observations/buffered-basic.json"),
        include_str!("fixtures/nim-observations/buffered-tools.json"),
        include_str!("fixtures/nim-observations/streamed-basic.json"),
        include_str!("fixtures/nim-observations/streamed-tools.json"),
    ] {
        let evidence: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let body = evidence["body"].as_str().unwrap().to_owned();
        let content_type = evidence["content_type"].as_str().unwrap().to_owned();
        let stream = evidence["transport"] == "sse";
        mock.state.push(Behavior::ExactResponse {
            content_type: content_type.clone(),
            body: body.clone(),
        });
        let response = client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body(evidence["case"].as_str().unwrap(), stream))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            evidence["status"].as_u64().unwrap() as u16
        );
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            content_type
        );
        // Streaming already has a fixed local connection frame; the fixture
        // bytes must follow it unchanged. Buffered responses are pure relay.
        let expected_proxy_bytes = if stream {
            format!(": connected\n\n{body}")
        } else {
            body.clone()
        };
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            expected_proxy_bytes.as_bytes(),
            "observation must preserve the literal {} bytes in the established proxy frame",
            evidence["case"].as_str().unwrap()
        );
    }

    let invalid_reasoning = r#"{"choices":[{"index":0,"message":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"completion_tokens_details":{"reasoning_tokens":3}}}"#;
    mock.state.push(Behavior::ExactResponse {
        content_type: "application/json".to_owned(),
        body: invalid_reasoning.to_owned(),
    });

    let response = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("observation byte boundary", false))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "observation must not change status");
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/json",
        "observation must not change content type"
    );
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        invalid_reasoning.as_bytes(),
        "observation must relay the exact upstream bytes"
    );

    assert!(
        !metrics(&proxy)
            .await
            .lines()
            .any(|line| line.starts_with("nimproxy_reasoning_tokens_total{")),
        "invalid reasoning must be omitted instead of accepted by the old side-band reader"
    );
}

#[tokio::test]
async fn usage_injection_asks_for_usage_and_backs_off_on_rejection() {
    // Default: stream_options injected.
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    read_sse(resp).await;
    {
        let hits = mock.state.hits.lock().unwrap();
        assert_eq!(
            hits[0].body["stream_options"]["include_usage"], true,
            "proxy injected stream_options"
        );
    }

    // Model that 400s on stream_options: retried untouched, then remembered.
    let mock2 = start_mock().await;
    mock2.state.push(Behavior::BadRequestIfInjected);
    let proxy2 = start_proxy(&mock2.url, &[]).await;
    let resp = client()
        .post(proxy2.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    let body = read_sse(resp).await;
    assert!(body.contains("data: [DONE]"), "recovered after 400: {body}");
    {
        let hits = mock2.state.hits.lock().unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits[0].body.get("stream_options").is_some());
        assert!(hits[1].body.get("stream_options").is_none());
    }
    // Next request for the same model: no injection attempt at all.
    let resp = client()
        .post(proxy2.url("/v1/chat/completions"))
        .json(&chat_body("again", true))
        .send()
        .await
        .unwrap();
    read_sse(resp).await;
    {
        let hits = mock2.state.hits.lock().unwrap();
        assert!(
            hits[2].body.get("stream_options").is_none(),
            "model remembered"
        );
    }

    // strict_passthrough disables injection entirely.
    let mock3 = start_mock().await;
    let proxy3 = start_proxy_with(
        &mock3.url,
        StoreOpts {
            strict_passthrough: true,
            ..Default::default()
        },
        &[],
    )
    .await;
    let resp = client()
        .post(proxy3.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    read_sse(resp).await;
    let hits = mock3.state.hits.lock().unwrap();
    assert!(hits[0].body.get("stream_options").is_none());
}

#[tokio::test]
async fn stalled_upstream_stream_errors_out_within_idle_timeout() {
    let mock = start_mock().await;
    mock.state.push(Behavior::Hang);
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            stream_idle_secs: 1,
            ..Default::default()
        },
        &[],
    )
    .await;

    let started = Instant::now();
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    let body = read_sse(resp).await;
    assert!(body.contains("stalled"), "stall surfaced: {body}");
    assert!(started.elapsed() < Duration::from_secs(10), "did not hang");
}

#[tokio::test]
async fn metrics_report_traffic_tokens_and_affinity() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(&mock.url, keyed("alice", "sekrit"), &[]).await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth("sekrit")
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    read_sse(resp).await;

    let metrics = metrics(&proxy).await;
    assert!(metrics.contains(r#"nimproxy_requests_total{"#), "{metrics}");
    assert!(metrics.contains(r#"client="alice""#));
    assert!(metrics.contains(r#"model="mock/model-a""#));
    assert!(
        metrics.contains(r#"nimproxy_completion_tokens_total{client="alice",model="mock/model-a",source="usage"} 2"#),
        "exact usage counted: {metrics}"
    );
    assert!(metrics.contains("nimproxy_affinity_total"));
}

#[tokio::test]
async fn dashboard_observation_quality_is_honest() {
    // Mutation caught: the proxy omits `nimproxy_usage_observations_total`,
    // does not record the terminal disconnected-stream result, or exposes a
    // request/model/client/error label instead of the closed field/result pair.
    let mock = start_mock().await;
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            stream_idle_secs: 1,
            strict_passthrough: true,
            ..Default::default()
        },
        &[],
    )
    .await;
    let cookie = login(&proxy).await;

    // A successful buffered response makes all five usage fields measured,
    // including a measured zero. This is a distinct finalization path from
    // the ordinary streamed response below.
    mock.state.push(Behavior::ExactResponse {
        content_type: "application/json".into(),
        body: r#"{"choices":[],"usage":{"prompt_tokens":0,"completion_tokens":2,"total_tokens":2,"prompt_tokens_details":{"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":1}}}"#.into(),
    });
    client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("buffered-measured", false))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    // Measured: prompt, completion, and reasoning arrive in the ordinary
    // completed stream. Total and cached are absent.
    read_sse(
        client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body("measured", true))
            .send()
            .await
            .unwrap(),
    )
    .await;

    // Unavailable: a valid buffered response carries no usage object.
    mock.state.push(Behavior::ExactResponse {
        content_type: "application/json".into(),
        body: r#"{"choices":[]}"#.into(),
    });
    client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("unavailable", false))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    // Estimated: one successful nonterminal SSE event has no measured usage.
    mock.state.push(Behavior::ExactResponse {
        content_type: "text/event-stream".into(),
        body:
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"}}]}\n\ndata: [DONE]\n\n"
                .into(),
    });
    read_sse(
        client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body("estimated", true))
            .send()
            .await
            .unwrap(),
    )
    .await;

    // Invalid: a present non-object usage value invalidates exactly the five
    // bounded usage fields without turning any of them into zero.
    mock.state.push(Behavior::ExactResponse {
        content_type: "application/json".into(),
        body: r#"{"choices":[],"usage":[]}"#.into(),
    });
    client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("invalid", false))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    // Final accounting must also happen when the downstream disconnects after
    // its first streamed chunk, not only on the completed-stream path.
    mock.state.push(Behavior::Hang);
    let mut disconnected = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("disconnect", true))
        .send()
        .await
        .unwrap();
    assert!(disconnected.chunk().await.unwrap().is_some());
    drop(disconnected);
    let disconnect_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let request_rows = metrics(&proxy).await;
        if request_rows.lines().any(|line| {
            line == r#"nimproxy_requests_total{client="local",model="mock/model-a",path="/v1/chat/completions",status="disconnect"} 1"#
        }) {
            break;
        }
        assert!(
            Instant::now() < disconnect_deadline,
            "disconnect never finalized in request metrics: {request_rows}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The idle timeout finalizes a successfully opened but stalled upstream
    // stream as unavailable rather than leaving its observations unrecorded.
    mock.state.push(Behavior::Hang);
    let stalled = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("idle-stall", true))
        .send()
        .await
        .unwrap();
    assert!(
        read_sse(stalled).await.contains("stalled"),
        "idle-stalled upstream stream must return a terminal proxy error"
    );

    // A nominal completed response with an unterminated final SSE event is
    // still a truncated observation, not measured or estimated usage.
    mock.state.push(Behavior::ExactResponse {
        content_type: "text/event-stream".into(),
        body: "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}"
            .into(),
    });
    read_sse(
        client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body("unterminated", true))
            .send()
            .await
            .unwrap(),
    )
    .await;

    let exposition = metrics(&proxy).await;
    assert_eq!(
        exposition
            .lines()
            .filter(|line| line.starts_with("# HELP nimproxy_usage_observations_total"))
            .collect::<Vec<_>>(),
        vec!["# HELP nimproxy_usage_observations_total Final classified upstream usage observations by field and result."],
        "usage observation HELP contract is exact"
    );
    assert_eq!(
        exposition
            .lines()
            .filter(|line| line.starts_with("# TYPE nimproxy_usage_observations_total"))
            .collect::<Vec<_>>(),
        vec!["# TYPE nimproxy_usage_observations_total counter"],
        "usage observation TYPE contract is exact"
    );
    assert_eq!(
        usage_observation_counter_lines(&exposition),
        BTreeSet::from([
            r#"nimproxy_usage_observations_total{field="prompt_tokens",result="measured"} 2"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="prompt_tokens",result="unavailable"} 5"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="prompt_tokens",result="invalid"} 1"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="completion_tokens",result="measured"} 2"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="completion_tokens",result="estimated"} 1"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="completion_tokens",result="unavailable"} 4"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="completion_tokens",result="invalid"} 1"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="total_tokens",result="measured"} 1"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="total_tokens",result="unavailable"} 6"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="total_tokens",result="invalid"} 1"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="cached_tokens",result="measured"} 1"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="cached_tokens",result="unavailable"} 6"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="cached_tokens",result="invalid"} 1"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="reasoning_tokens",result="measured"} 2"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="reasoning_tokens",result="unavailable"} 5"#.to_owned(),
            r#"nimproxy_usage_observations_total{field="reasoning_tokens",result="invalid"} 1"#.to_owned(),
        ]),
        "every finalized success/disconnect/stall/unterminated path has exactly one of the closed five-field results"
    );

    // The existing typed dashboard payload carries registry series directly;
    // no observation-availability API field is added for this UI state.
    let now: serde_json::Value = client()
        .get(proxy.url("/api/dashboard/now"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(now.get("observation_availability").is_none());
    assert!(
        now["metrics"].as_array().unwrap().iter().any(|metric| {
            metric["metric"] == "nimproxy_usage_observations_total"
                && metric["labels"]
                    == serde_json::json!({"field":"completion_tokens","result":"estimated"})
                && metric["value"] == 1.0
        }),
        "dashboard must consume the existing metrics payload, not an invented API field: {now}"
    );

    // A failed final response is excluded entirely: it must not synthesize an
    // observation result merely because the request reached the proxy.
    let no_success_mock = start_mock().await;
    let no_success_proxy = start_proxy_with(
        &no_success_mock.url,
        StoreOpts {
            strict_passthrough: true,
            ..Default::default()
        },
        &[],
    )
    .await;
    no_success_mock.state.push(Behavior::BadRequest);
    assert_eq!(
        client()
            .post(no_success_proxy.url("/v1/chat/completions"))
            .json(&chat_body("failed", false))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    assert!(
        usage_observation_counter_lines(&metrics(&no_success_proxy).await).is_empty(),
        "non-successful responses are excluded from finalized observations"
    );

    // The pre-observation retry response is likewise excluded; only its final
    // successful attempt leaves the five literal final results.
    let retry_mock = start_mock().await;
    let retry_proxy = start_proxy(&retry_mock.url, &[]).await;
    retry_mock.state.push(Behavior::RateLimited(0));
    retry_mock.state.push(Behavior::Ok);
    read_sse(
        client()
            .post(retry_proxy.url("/v1/chat/completions"))
            .json(&chat_body("retry", true))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        usage_observation_counter_lines(&metrics(&retry_proxy).await),
        BTreeSet::from([
            r#"nimproxy_usage_observations_total{field="prompt_tokens",result="measured"} 1"#
                .to_owned(),
            r#"nimproxy_usage_observations_total{field="completion_tokens",result="measured"} 1"#
                .to_owned(),
            r#"nimproxy_usage_observations_total{field="total_tokens",result="unavailable"} 1"#
                .to_owned(),
            r#"nimproxy_usage_observations_total{field="cached_tokens",result="unavailable"} 1"#
                .to_owned(),
            r#"nimproxy_usage_observations_total{field="reasoning_tokens",result="measured"} 1"#
                .to_owned(),
        ]),
        "pre-observation retry responses do not add observation counters"
    );
}

#[tokio::test]
async fn request_shape_and_quality_metrics_are_recorded() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    // A plain streaming request (finishes "stop").
    read_sse(
        client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body("hi", true))
            .send()
            .await
            .unwrap(),
    )
    .await;

    // A tool-using request with sampling params: the mock answers with a
    // tool_calls delta and finish_reason "tool_calls".
    let tool_req = serde_json::json!({
        "model": "mock/model-a",
        "stream": true,
        "temperature": 0.7,
        "max_tokens": 4096,
        "tools": [{"type": "function", "function": {"name": "get_weather"}}],
        "tool_choice": "auto",
        "messages": [
            {"role": "system", "content": "you are a test"},
            {"role": "user", "content": "weather?"}
        ]
    });
    read_sse(
        client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&tool_req)
            .send()
            .await
            .unwrap(),
    )
    .await;

    let metrics = metrics(&proxy).await;

    // Request shape (labeled by client — open mode admits everyone as "local").
    assert!(
        metrics.contains(r#"nimproxy_stream_requests_total{client="local",stream="true"}"#),
        "stream flag counted: {metrics}"
    );
    assert!(
        metrics.contains(r#"nimproxy_request_messages_count{client="local"}"#),
        "conversation depth histogram present"
    );
    assert!(
        metrics.contains(r#"nimproxy_request_tools_count{client="local"}"#),
        "tools-offered histogram present"
    );
    assert!(
        metrics.contains("nimproxy_request_temperature_count"),
        "temperature histogram present"
    );
    assert!(
        metrics.contains("nimproxy_request_max_tokens_count"),
        "max_tokens histogram present"
    );
    assert!(
        metrics.contains(r#"nimproxy_tool_choice_total{mode="auto"}"#),
        "tool_choice mode counted"
    );

    // Response quality.
    assert!(
        metrics.contains(r#"nimproxy_finish_reason_total{model="mock/model-a",reason="stop"}"#),
        "stop finish recorded: {metrics}"
    );
    assert!(
        metrics
            .contains(r#"nimproxy_finish_reason_total{model="mock/model-a",reason="tool_calls"}"#),
        "tool_calls finish recorded"
    );
    assert!(
        metrics.contains(r#"nimproxy_tool_calls_total{model="mock/model-a"}"#),
        "tool-call volume recorded"
    );
    assert!(
        metrics.contains(r#"nimproxy_reasoning_tokens_total{model="mock/model-a"}"#),
        "reasoning tokens recorded"
    );

    // Cardinality stays bounded: the stream label is a two-value enum.
    for line in metrics
        .lines()
        .filter(|l| l.starts_with("nimproxy_stream_requests_total{"))
    {
        assert!(
            line.contains(r#"stream="true""#) || line.contains(r#"stream="false""#),
            "stream label bounded to true/false: {line}"
        );
    }
}

/// The buffered (non-streaming) path extracts finish_reason, reasoning tokens,
/// and tool-call count from `relay()`; an unknown finish_reason collapses to
/// `other`; JSON mode and non-`auto` tool_choice are recorded. These paths are
/// distinct from the streaming assertions above.
#[tokio::test]
async fn buffered_quality_and_edge_cases_are_recorded() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    let post = |body: serde_json::Value| {
        let proxy = &proxy;
        async move {
            let r = client()
                .post(proxy.url("/v1/chat/completions"))
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            r.text().await.unwrap();
        }
    };

    // Buffered tool call: mock answers with message.tool_calls + finish tool_calls.
    post(serde_json::json!({
        "model": "mock/model-a", "stream": false, "tool_choice": "required",
        "tools": [{"type": "function", "function": {"name": "run"}}],
        "messages": [{"role": "user", "content": "go"}]
    }))
    .await;

    // Buffered JSON mode.
    post(serde_json::json!({
        "model": "mock/model-a", "stream": false,
        "response_format": {"type": "json_object"},
        "messages": [{"role": "user", "content": "as json"}]
    }))
    .await;

    // Unknown upstream finish_reason must collapse to "other".
    mock.state.push(Behavior::OddFinish);
    post(serde_json::json!({
        "model": "mock/model-a", "stream": false,
        "messages": [{"role": "user", "content": "hi"}]
    }))
    .await;

    let metrics = metrics(&proxy).await;

    // Buffered quality extraction (from relay()).
    assert!(
        metrics
            .contains(r#"nimproxy_finish_reason_total{model="mock/model-a",reason="tool_calls"}"#),
        "buffered tool_calls finish recorded: {metrics}"
    );
    assert!(
        metrics.contains(r#"nimproxy_tool_calls_total{model="mock/model-a"}"#),
        "buffered tool-call count recorded"
    );
    assert!(
        metrics.contains(r#"nimproxy_reasoning_tokens_total{model="mock/model-a"}"#),
        "buffered reasoning tokens recorded"
    );
    assert!(
        metrics.contains(r#"nimproxy_upstream_seconds_count{model="mock/model-a"}"#),
        "upstream latency recorded on the buffered path"
    );

    // Edge cases.
    assert!(
        metrics.contains(r#"nimproxy_tool_choice_total{mode="required"}"#),
        "non-auto tool_choice mode recorded"
    );
    assert!(
        metrics.contains(r#"nimproxy_json_mode_total{client="local"}"#),
        "JSON mode recorded"
    );
    assert!(
        metrics.contains(r#"nimproxy_finish_reason_total{model="mock/model-a",reason="other"}"#),
        "unknown finish_reason collapsed to other: {metrics}"
    );
    assert!(
        !metrics.contains(r#"reason="banana""#),
        "raw upstream finish_reason never becomes a label"
    );
}

// ---------- correctness & security hardening (PR 6a) ----------

/// A malformed percent-escape with a multibyte char (`%€`) in the login body
/// must not panic the pre-auth handler (it used to slice a &str on a non-char
/// boundary). The request should come back as a normal failed-login page.
#[tokio::test]
async fn login_handles_malformed_urlencoded_without_panic() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    let resp = client()
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("username=root&password=%a\u{20ac}")
        .send()
        .await
        .unwrap();
    // No panic / connection reset: a clean 401 login page with the error.
    assert_eq!(resp.status(), 401);
    let html = resp.text().await.unwrap();
    assert!(html.contains(r#"data-error-code="invalid_credentials""#));
    let login_js = client()
        .get(proxy.url("/assets/public/login.js"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(login_js.contains("login.error.invalid_credentials"));
    let catalog: serde_json::Value = client()
        .get(proxy.url("/assets/public/locales/en-US.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        catalog["messages"]["login.error.invalid_credentials"],
        "Incorrect username or password."
    );
}

/// Repeated failed logins trip the throttle: a burst past the failure cap
/// returns 429 + Retry-After, even for a subsequently-correct password.
#[tokio::test]
async fn login_throttles_after_repeated_failures() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    // The cap is 10 failures per window; 11 wrong attempts trips it. Every
    // attempt names a real user so the throttle (not a parse path) is what fires.
    for _ in 0..11 {
        let r = client()
            .post(proxy.url("/login"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body("username=root&password=wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401); // wrong password re-renders the form (401)
    }
    // Now throttled: even the correct password is refused with 429 + Retry-After.
    let r = client()
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("username=root&password={TEST_PASSWORD}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
    assert_eq!(r.headers().get("retry-after").unwrap(), "60");
}

/// A buffered request against an upstream that sends headers then stalls the
/// body must not hang forever holding an in-flight slot — the request timeout
/// surfaces a gateway error instead.
#[tokio::test]
async fn buffered_request_times_out_on_hung_upstream() {
    let mock = start_mock().await;
    mock.state.push(Behavior::Hang);
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            request_timeout_secs: 1,
            ..Default::default()
        },
        &[],
    )
    .await;

    let started = Instant::now();
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502, "hung body surfaces as bad_gateway");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "returned promptly, did not hang"
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "bad_gateway");
}

/// Past the in-flight cap the proxy sheds load with 503 instead of growing the
/// queue unbounded.
#[tokio::test]
async fn overloaded_requests_are_shed_with_503() {
    let mock = start_mock().await;
    mock.state.push(Behavior::Hang);
    let proxy = std::sync::Arc::new(
        start_proxy_with(
            &mock.url,
            StoreOpts {
                max_inflight: 1,
                request_timeout_secs: 30,
                ..Default::default()
            },
            &[],
        )
        .await,
    );

    // Occupy the single in-flight slot with a buffered request whose body hangs.
    let hog = {
        let proxy = proxy.clone();
        tokio::spawn(async move {
            let _ = client()
                .post(proxy.url("/v1/chat/completions"))
                .json(&chat_body("hog", false))
                .send()
                .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("shed-me", false))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        503,
        "second request shed at the in-flight cap"
    );
    assert_eq!(resp.headers().get("retry-after").unwrap(), "5");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "overloaded");
    hog.abort();
}

/// An unreachable upstream exercises the connection-error arm: the lane is
/// put in cooldown with status "connect" and the request fails fast at the deadline.
#[tokio::test]
async fn upstream_connection_error_enters_cooldown() {
    // Nothing listens on port 1 → every connect attempt fails.
    let proxy = start_proxy_with(
        "http://127.0.0.1:1",
        StoreOpts {
            max_wait_secs: 2,
            ..Default::default()
        },
        &[],
    )
    .await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 504, "connect failures exhaust to a 504");

    let metrics = metrics(&proxy).await;
    assert!(
        metrics.contains(r#"nimproxy_lane_cooldown_total{lane="0",status="connect"}"#),
        "connection error put the lane in cooldown: {metrics}"
    );
}

#[tokio::test]
async fn history_records_snapshots_and_survives_restart() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[("HISTORY_SAMPLE_SECS", "1")]).await;

    // Drive traffic so snapshots have metric series in them.
    let r = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    tokio::time::sleep(Duration::from_millis(2500)).await;

    // Canonical records land at the separate v1 path; experimental history is
    // never written or read by the new runtime.
    let jsonl = proxy.data_dir.join("history-v1.jsonl");
    let raw = std::fs::read_to_string(&jsonl).expect("history-v1.jsonl written");
    let lines: Vec<&str> = raw.lines().filter(|l| !l.is_empty()).collect();
    assert!(lines.len() >= 2, "sampler ran: {} snapshots", lines.len());
    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        records.iter().any(|value| value["kind"] == "boot"),
        "process epoch is persisted"
    );
    assert!(
        records.iter().any(|value| {
            value["format"] == "nimproxy-history"
                && value["v"] == 1
                && value["boot_id"].is_string()
                && value["capacity"]["capacity_rpm"] == 120
                && value["state"]
                    .as_array()
                    .is_some_and(|state| !state.is_empty())
        }),
        "canonical samples carry normalized metrics and contemporaneous capacity: {raw}"
    );
    let before = records
        .iter()
        .filter(|value| value["kind"] == "sample")
        .count();

    // Restart on the SAME data dir: history reloads into the normalized index
    // and remains visible through the typed dashboard range contract.
    let proxy = restart(proxy, &[("HISTORY_SAMPLE_SECS", "1")]).await;
    let cookie = login(&proxy).await;
    let history: serde_json::Value = client()
        .get(proxy.url("/api/dashboard?from=1&to=4102444800&points=1000"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        history["history_revision"].as_u64().unwrap() >= before as u64,
        "history persisted across restart: {history}"
    );
}

#[tokio::test]
async fn dashboard_history_combines_process_epochs() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[("HISTORY_SAMPLE_SECS", "1")]).await;
    let cookie = login(&proxy).await;
    let initial = dashboard_range(&proxy, &cookie, 1, 4_102_444_800, 1000).await;

    send_successful_chats(&proxy, 2).await;
    wait_for_persisted_chat_total(
        &proxy,
        &cookie,
        initial["history_revision"].as_u64().unwrap(),
        2.0,
    )
    .await;

    let proxy = restart(proxy, &[("HISTORY_SAMPLE_SECS", "1")]).await;
    let cookie = login(&proxy).await;
    let second_epoch = dashboard_range(&proxy, &cookie, 1, 4_102_444_800, 1000).await;
    assert_eq!(successful_chat_requests(&second_epoch["totals"]), 2.0);

    send_successful_chats(&proxy, 3).await;
    wait_for_persisted_chat_total(
        &proxy,
        &cookie,
        second_epoch["history_revision"].as_u64().unwrap(),
        5.0,
    )
    .await;

    for points in [2, 1000] {
        let range = dashboard_range(&proxy, &cookie, 1, 4_102_444_800, points).await;
        assert_eq!(
            successful_chat_requests(&range["totals"]),
            5.0,
            "exact total must not depend on points={points}: {range}"
        );
    }
}

#[tokio::test]
async fn dashboard_history_reports_completeness() {
    let mock = start_mock().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let record = |kind: &str, timestamp: u64, boot_id: &str, state: &str| {
        format!(
            "{{\"format\":\"nimproxy-history\",\"v\":1,\"kind\":\"{kind}\",\"timestamp\":{timestamp},\"boot_id\":\"{boot_id}\",\"capacity\":{{\"capacity_rpm\":80,\"enabled_keys\":2,\"key_rpms\":[40,40]}}{state}}}\n"
        )
    };
    let sample = |timestamp, boot_id, value| {
        record(
            "sample",
            timestamp,
            boot_id,
            &format!(
                ",\"state\":[{{\"kind\":\"counter\",\"metric\":\"nimproxy_requests_total\",\"labels\":{{\"client\":\"synthetic-client\"}},\"value\":{value}}}]"
            ),
        )
    };
    let configured_dir = |dir: &std::path::Path| {
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_vec_pretty(&StoreOpts::default().json(&mock.url)).unwrap(),
        )
        .unwrap();
    };
    let assert_wire_order = |body: &str, object: &str, keys: &[&str]| {
        let prefix = format!("\"{object}\":{{");
        let payload = body
            .split_once(&prefix)
            .map(|(_, payload)| payload)
            .unwrap_or("");
        let positions: Vec<_> = keys
            .iter()
            .map(|key| payload.find(&format!("\"{key}\":")))
            .collect();
        assert!(
            positions.iter().all(Option::is_some),
            "{object} lacks locked wire keys {keys:?}: {body}"
        );
        let positions: Vec<_> = positions.into_iter().flatten().collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{object} keys are not in locked ASCII wire order: {body}"
        );
    };

    let data_dir = scratch_data_dir();
    configured_dir(&data_dir);
    let valid_history = format!(
        "{}{}",
        record("boot", now - 3, "boot-a", ""),
        sample(now - 2, "boot-a", 2.0),
    );
    std::fs::write(data_dir.join("history-v1.jsonl"), valid_history).unwrap();
    let proxy = start_proxy_in(data_dir, &[("HISTORY_SAMPLE_SECS", "3600")]).await;
    let cookie = login(&proxy).await;
    let response = client()
        .get(proxy.url(&format!(
            "/api/dashboard?from={}&to={}&points=1000",
            now - 3,
            now - 1,
        )))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let raw = response.text().await.unwrap();
    assert_wire_order(
        &raw,
        "window",
        &[
            "available_from",
            "available_to",
            "complete",
            "default_window_days",
            "effective_from",
            "effective_to",
            "following_now",
            "requested_from",
            "requested_to",
            "retention_days",
        ],
    );
    assert_wire_order(
        &raw,
        "diagnostics",
        &[
            "excluded_epochs",
            "excluded_records",
            "normalized_series",
            "skipped_metric_lines",
            "valid_checkpoints",
            "valid_samples",
        ],
    );
    let range: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(range["window"]["complete"], true, "valid epoch is complete");
    assert_eq!(
        range["diagnostics"],
        serde_json::json!({
            "excluded_epochs": 0,
            "excluded_records": 0,
            "normalized_series": 1,
            "skipped_metric_lines": 0,
            "valid_checkpoints": 0,
            "valid_samples": 1,
        }),
        "valid stream diagnostics are exact"
    );

    let damaged_dir = scratch_data_dir();
    configured_dir(&damaged_dir);
    let damaged_history = format!(
        "{}{}{{not-json}}\n{}{}",
        record("boot", now - 7, "boot-a", ""),
        sample(now - 6, "boot-a", 2.0),
        record("boot", now - 4, "boot-b", ""),
        sample(now - 3, "boot-b", 7.0),
    );
    std::fs::write(damaged_dir.join("history-v1.jsonl"), damaged_history).unwrap();
    let recovered = start_proxy_in(damaged_dir, &[("HISTORY_SAMPLE_SECS", "3600")]).await;
    let recovered_cookie = login(&recovered).await;
    let recovered_range =
        dashboard_range(&recovered, &recovered_cookie, now - 7, now - 2, 1000).await;
    assert_eq!(recovered_range["window"]["complete"], false);
    assert_eq!(
        recovered_range["diagnostics"],
        serde_json::json!({
            "excluded_epochs": 1,
            "excluded_records": 3,
            "normalized_series": 1,
            "skipped_metric_lines": 0,
            "valid_checkpoints": 0,
            "valid_samples": 1,
        })
    );
    let recovered_total = recovered_range["totals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| {
            value["metric"] == "nimproxy_requests_total"
                && value["labels"]["client"] == "synthetic-client"
        })
        .and_then(|value| value["value"].as_f64());
    assert_eq!(recovered_total, Some(7.0));
}

#[tokio::test]
async fn dashboard_tail_rolls_into_persisted_history_once() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[("HISTORY_SAMPLE_SECS", "2")]).await;
    let cookie = login(&proxy).await;
    let initial = dashboard_range(&proxy, &cookie, 1, 4_102_444_800, 1000).await;

    send_successful_chats(&proxy, 1).await;
    let persisted = wait_for_persisted_chat_total(
        &proxy,
        &cookie,
        initial["history_revision"].as_u64().unwrap(),
        1.0,
    )
    .await;
    let persisted_revision = persisted["history_revision"].as_u64().unwrap();

    send_successful_chats(&proxy, 1).await;
    let live = dashboard_now(&proxy, &cookie).await;
    assert_eq!(
        live["history_revision"], persisted_revision,
        "the second request is still newer than persisted history: {live}"
    );
    assert_eq!(successful_chat_requests(&live["tail"]["totals"]), 1.0);

    let refreshed = wait_for_persisted_chat_total(&proxy, &cookie, persisted_revision, 2.0).await;
    assert!(
        refreshed["history_revision"].as_u64().unwrap() > persisted_revision,
        "{refreshed}"
    );
    assert_eq!(successful_chat_requests(&refreshed["totals"]), 2.0);

    let rolled = dashboard_now(&proxy, &cookie).await;
    assert!(
        rolled["history_revision"].as_u64().unwrap() > persisted_revision,
        "{rolled}"
    );
    assert_eq!(
        successful_chat_requests(&rolled["tail"]["totals"]),
        0.0,
        "the persisted request must not remain in the live tail: {rolled}"
    );
}

#[tokio::test]
async fn experimental_legacy_history_is_ignored_without_mutation() {
    let mock = start_mock().await;
    let data_dir = scratch_data_dir();
    std::fs::write(
        data_dir.join("config.json"),
        serde_json::to_string_pretty(&StoreOpts::default().json(&mock.url)).unwrap(),
    )
    .unwrap();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        data_dir.join("history.jsonl"),
        format!(
            "{{\"t\":{},\"m\":\"# TYPE nimproxy_requests_total counter\\nnimproxy_requests_total{{client=\\\"local\\\",model=\\\"mock/model-a\\\",path=\\\"/v1/chat/completions\\\",status=\\\"200\\\"}} 10\\n\"}}\n\
             {{\"t\":{},\"m\":\"# TYPE nimproxy_requests_total counter\\nnimproxy_requests_total{{client=\\\"local\\\",model=\\\"mock/model-a\\\",path=\\\"/v1/chat/completions\\\",status=\\\"200\\\"}} 15\\n\"}}\n\
             {{\"t\":{},\"m\":\"# TYPE nimproxy_requests_total counter\\nnimproxy_requests_total{{client=\\\"local\\\",model=\\\"mock/model-a\\\",path=\\\"/v1/chat/completions\\\",status=\\\"200\\\"}} 4\\n\"}}\n",
            now - 3,
            now - 2,
            now - 1,
        ),
    )
    .unwrap();

    let legacy_before = std::fs::read(data_dir.join("history.jsonl")).unwrap();
    let proxy = start_proxy_in(data_dir, &[("HISTORY_SAMPLE_SECS", "3600")]).await;
    let cookie = login(&proxy).await;
    let range = dashboard_range(&proxy, &cookie, 1, now.saturating_sub(1), 1000).await;
    assert_eq!(
        successful_chat_requests(&range["totals"]),
        0.0,
        "legacy values are not imported into canonical history: {range}"
    );
    assert_eq!(
        range["diagnostics"],
        serde_json::json!({
            "excluded_epochs": 0,
            "excluded_records": 0,
            "normalized_series": 0,
            "skipped_metric_lines": 0,
            "valid_checkpoints": 0,
            "valid_samples": 0,
        })
    );
    assert_eq!(
        std::fs::read(proxy.data_dir.join("history.jsonl")).unwrap(),
        legacy_before
    );
}

#[tokio::test]
async fn historical_capacity_uses_snapshot_configuration() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[("HISTORY_SAMPLE_SECS", "1")]).await;
    let cookie = login(&proxy).await;
    let initial = dashboard_range(&proxy, &cookie, 1, 4_102_444_800, 1000).await;

    send_successful_chats(&proxy, 1).await;
    let at_120 = wait_for_persisted_chat_total(
        &proxy,
        &cookie,
        initial["history_revision"].as_u64().unwrap(),
        1.0,
    )
    .await;
    let at_120_revision = at_120["history_revision"].as_u64().unwrap();

    let fingerprint = api_config(&proxy, &cookie).await["nim_keys"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = post_json(
        &proxy,
        &cookie,
        "/api/settings/nim-keys",
        serde_json::json!({"set": {"fingerprint": fingerprint, "rpm": 20}}),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    send_successful_chats(&proxy, 1).await;
    let at_100 = wait_for_persisted_chat_total(&proxy, &cookie, at_120_revision, 2.0).await;
    let available_from = at_100["window"]["available_from"].as_u64().unwrap();
    let available_to = at_100["window"]["available_to"].as_u64().unwrap();
    let range = dashboard_range(
        &proxy,
        &cookie,
        available_from.saturating_sub(1),
        available_to,
        1000,
    )
    .await;
    let capacities: Vec<f64> = range["points"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|point| point["capacity"]["average_rpm"].as_f64())
        .collect();
    assert!(
        capacities.contains(&120.0),
        "120 RPM snapshot capacity is retained: {range}"
    );
    assert!(
        capacities.contains(&100.0),
        "100 RPM snapshot capacity is retained: {range}"
    );

    let now = dashboard_now(&proxy, &cookie).await;
    assert_eq!(now["capacity_rpm"], 100);
}

#[tokio::test]
async fn dashboard_range_contract_defaults_validates_and_requires_auth() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[("HISTORY_SAMPLE_SECS", "1")]).await;
    let cookie = login(&proxy).await;

    let response = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("dashboard range", false))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let response = client()
        .get(proxy.url("/api/dashboard?from=1&to=4102444800&points=24"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(body["history_revision"].as_u64().is_some());
    assert_eq!(body["window"]["requested_from"], 1);
    assert_eq!(body["window"]["requested_to"], 4_102_444_800u64);
    assert_eq!(body["window"]["following_now"], false);
    assert!(body["config_revision"].as_u64().is_some());
    assert!(body["window"]["available_from"].as_u64().is_some());
    assert!(body["totals"].as_array().is_some());
    assert!(body["latest"].as_array().is_some());
    assert!(body["points"].as_array().is_some());

    let response = client()
        .get(proxy.url("/api/dashboard?from=99&to=99"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let error: serde_json::Value = response.json().await.unwrap();
    assert_eq!(error["error"]["code"], "invalid_time_window");

    let response = client()
        .get(proxy.url("/api/dashboard"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let defaulted: serde_json::Value = response.json().await.unwrap();
    let from = defaulted["window"]["requested_from"].as_u64().unwrap();
    let to = defaulted["window"]["requested_to"].as_u64().unwrap();
    assert_eq!(to - from, 30 * 86_400);
    assert_eq!(defaulted["window"]["following_now"], true);
    assert_eq!(defaulted["window"]["default_window_days"], 30);
    assert_eq!(defaulted["window"]["retention_days"], 30);

    let response = client()
        .get(proxy.url("/api/dashboard"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    let settings = api_config(&proxy, &cookie).await;
    assert!(settings["server"]["history"]["available_from"]
        .as_u64()
        .is_some());
    assert!(settings["server"]["history"]["file_bytes"]
        .as_u64()
        .is_some());
    assert_eq!(settings["server"]["history"]["compaction_pending"], false);
}

#[tokio::test]
async fn dashboard_now_contract_uses_current_pool_config_and_registry() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let cookie = login(&proxy).await;

    let response = client()
        .get(proxy.url("/api/dashboard/now"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["lanes"], 3);
    assert_eq!(body["rpms"], serde_json::json!([40, 40, 40]));
    assert_eq!(body["capacity_rpm"], 120);
    assert_eq!(body["default_window_days"], 30);
    assert_eq!(body["retention_days"], 30);
    assert_eq!(body["slo_target_percent"], 99.9);
    assert!(body["history_revision"].as_u64().is_some());
    assert_eq!(
        body["history_revision"],
        body["tail"]["base_history_revision"]
    );
    assert!(body["config_revision"].as_u64().is_some());
    assert!(body["tail"]["totals"].as_array().is_some());
    assert!(body["metrics"].as_array().is_some());

    let response = client()
        .get(proxy.url("/api/dashboard/now"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn legacy_retention_fixture_is_not_migrated_or_compacted() {
    let mock = start_mock().await;
    let data_dir = scratch_data_dir();
    std::fs::write(
        data_dir.join("config.json"),
        serde_json::to_string_pretty(&StoreOpts::default().json(&mock.url)).unwrap(),
    )
    .unwrap();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cutoff = now - 86_400;
    let old = cutoff - 400;
    let boot = cutoff - 300;
    let baseline = cutoff - 200;
    let retained_one = cutoff + 10;
    let retained_two = now - 10;
    std::fs::write(
        data_dir.join("history.jsonl"),
        format!(
            "{{\"t\":{old},\"m\":\"# TYPE fixture_requests_total counter\\nfixture_requests_total 5\\n\"}}\n\
             {{\"v\":2,\"t\":{boot},\"boot\":\"boot-a\",\"kind\":\"boot\",\"capacity\":{{\"enabled_lanes\":3,\"rpms\":[40,40,40],\"capacity_rpm\":120}}}}\n\
             {{\"v\":2,\"t\":{baseline},\"boot\":\"boot-a\",\"capacity\":{{\"enabled_lanes\":3,\"rpms\":[40,40,40],\"capacity_rpm\":120}},\"m\":\"# TYPE fixture_requests_total counter\\nfixture_requests_total 50\\n\"}}\n\
             {{\"v\":2,\"t\":{retained_one},\"boot\":\"boot-a\",\"capacity\":{{\"enabled_lanes\":3,\"rpms\":[40,40,40],\"capacity_rpm\":120}},\"m\":\"# TYPE fixture_requests_total counter\\nfixture_requests_total 60\\n\"}}\n\
             {{\"v\":2,\"t\":{retained_two},\"boot\":\"boot-a\",\"capacity\":{{\"enabled_lanes\":3,\"rpms\":[40,40,40],\"capacity_rpm\":120}},\"m\":\"# TYPE fixture_requests_total counter\\nfixture_requests_total 70\\n\"}}\n"
        ),
    )
    .unwrap();

    let legacy_before = std::fs::read(data_dir.join("history.jsonl")).unwrap();
    let proxy = start_proxy_in(data_dir, &[("HISTORY_SAMPLE_SECS", "3600")]).await;
    let cookie = login(&proxy).await;
    let (status, body) = post_json(
        &proxy,
        &cookie,
        "/api/settings/history",
        serde_json::json!({
            "days": 1,
            "default_window_days": 1,
            "slo_target_percent": 99.9
        }),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let proxy = restart(proxy, &[("HISTORY_SAMPLE_SECS", "3600")]).await;
    assert_eq!(
        std::fs::read(proxy.data_dir.join("history.jsonl")).unwrap(),
        legacy_before,
        "Task 11 neither imports nor compacts experimental history"
    );
}

#[tokio::test]
async fn sigterm_shuts_down_cleanly() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let status = proxy.terminate();
    assert!(status.success(), "clean exit on SIGTERM, got {status:?}");
}

#[tokio::test]
async fn dashboard_and_config_are_served_to_authenticated_users() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let cookie = login(&proxy).await;

    let dash = client()
        .get(proxy.url("/"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(dash.status(), 200);
    let html = dash.text().await.unwrap();
    assert!(html.contains("NIM"));
    assert!(html.contains("data-range=\"default\""));
    assert!(html.contains("data-range=\"all-retained\""));
    let dashboard_js = client()
        .get(proxy.url("/assets/operator/dashboard.js"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(dashboard_js.contains("/api/dashboard/now"));
    assert!(!dashboard_js.contains("fetch('/metrics')"));
    assert!(!dashboard_js.contains("/api/history?"));
    assert!(!dashboard_js.contains("/dash/config.json"));

    let now: serde_json::Value = client()
        .get(proxy.url("/api/dashboard/now"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(now["lanes"], 3);
    assert_eq!(now["auth"], false, "open /v1 mode reports auth=false");

    for retired in ["/api/history", "/dash/config.json"] {
        assert_eq!(
            client()
                .get(proxy.url(retired))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            404,
            "{retired} stays retired"
        );
    }
}

#[tokio::test]
async fn dashboard_history_settings_markup() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let cookie = login(&proxy).await;

    let settings_js = client()
        .get(proxy.url("/assets/operator/settings.js"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let catalog: serde_json::Value = client()
        .get(proxy.url("/assets/operator/locales/en-US.json"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(settings_js.contains(r#"data-i18n="settings.server.history.heading""#));
    assert_eq!(
        catalog["messages"]["settings.server.history.heading"],
        "History & dashboard"
    );
    assert!(settings_js.contains("sv-default-days"));
    assert!(settings_js.contains("sv-retention-days"));
    assert!(settings_js.contains("sv-slo"));
    assert!(settings_js.contains("/api/settings/history"));
    assert!(!settings_js.contains("Pricing &amp; history"));
    assert!(!settings_js.contains("const SLO = 0.999"));
}

#[tokio::test]
async fn dashboard_range_state_guards_markup() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let cookie = login(&proxy).await;

    let shared_js = client()
        .get(proxy.url("/assets/operator/shared.js"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let dashboard_js = client()
        .get(proxy.url("/assets/operator/dashboard.js"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let scripts = shared_js + &dashboard_js;
    assert!(scripts.contains("let rangeRequestGeneration = 0"));
    assert!(scripts.contains("const generation = ++rangeRequestGeneration"));
    assert!(scripts.contains("generation !== rangeRequestGeneration"));
    assert!(!scripts.contains("mode.kind === 'fixed' && historyChanged"));
    assert!(scripts.contains("let frozenHasTraffic = false"));
    assert!(
        scripts.contains("if (mode.kind !== 'following' || !rangeData || !samples.length) return;")
    );
}

#[tokio::test]
async fn dashboard_pause_traffic_is_derived_from_rendered_samples() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let cookie = login(&proxy).await;

    let shared_js = client()
        .get(proxy.url("/assets/operator/shared.js"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let dashboard_js = client()
        .get(proxy.url("/assets/operator/dashboard.js"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let scripts = shared_js + &dashboard_js;
    assert!(scripts.contains("function hasSelectedRequestTraffic(selectedSamples)"));
    assert!(scripts.contains("row => row.name === 'nimproxy_requests_total' && +row.value > 0"));
    assert!(scripts.contains("frozenHasTraffic = hasSelectedRequestTraffic(samples);"));
    assert!(scripts.contains(
        "const hasTraffic = mode.paused ? frozenHasTraffic : hasSelectedRequestTraffic(samples);"
    ));
    assert!(!scripts.contains("const acceptedTail = nowData?.tail"));
}

#[tokio::test]
async fn dashboard_capacity_history_has_no_guessed_key_size() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let cookie = login(&proxy).await;

    let catalog: serde_json::Value = client()
        .get(proxy.url("/assets/operator/locales/en-US.json"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        catalog["messages"]["dashboard.capacity.history.shortfall"],
        "Peak shortfall · rpm"
    );
    assert!(
        catalog["messages"]["dashboard.capacity.history.utilization"]
            .as_str()
            .unwrap()
            .contains("of capacity at the time")
    );
    assert!(
        catalog["messages"]["dashboard.capacity.history.no_data.other"]
            .as_str()
            .unwrap()
            .contains("with no capacity data")
    );
    let dashboard_js = client()
        .get(proxy.url("/assets/operator/dashboard.js"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!dashboard_js.contains("const moreKeys"));
    assert!(!dashboard_js.contains("MORE KEY"));
}

#[tokio::test]
async fn dashboard_now_refreshes_after_settings_change() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let cookie = login(&proxy).await;

    let before: serde_json::Value = client()
        .get(proxy.url("/api/dashboard/now"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fingerprint = api_config(&proxy, &cookie).await["nim_keys"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = post_json(
        &proxy,
        &cookie,
        "/api/settings/nim-keys",
        serde_json::json!({"set": {"fingerprint": fingerprint, "rpm": 41}}),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let after: serde_json::Value = client()
        .get(proxy.url("/api/dashboard/now"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        after["config_revision"].as_u64().unwrap() > before["config_revision"].as_u64().unwrap()
    );
    assert_ne!(after["capacity_rpm"], before["capacity_rpm"]);
    assert_eq!(
        after["history_revision"], before["history_revision"],
        "current config changes do not rewrite retained history"
    );
}

// ---------- boot posture & the setup wizard ----------

/// With no store, the proxy boots healthy but claimably closed: /v1 answers
/// 503 setup_required, browsers land on /setup, and /setup serves the wizard.
#[tokio::test]
async fn fresh_boot_enters_setup_mode() {
    let proxy = start_proxy_fresh().await;
    let nr = no_redirect_client();

    // Health stays public so orchestrators can probe a not-yet-claimed proxy.
    assert_eq!(
        client()
            .get(proxy.url("/health"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // /v1 is closed until setup completes.
    let api = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(api.status(), 503);
    let body: serde_json::Value = api.json().await.unwrap();
    assert_eq!(body["error"]["code"], "setup_required");

    // Browsers are steered to the wizard, from both the dashboard and /login.
    let dash = nr
        .get(proxy.url("/"))
        .header("accept", "text/html")
        .send()
        .await
        .unwrap();
    assert_eq!(dash.status(), 302);
    assert_eq!(dash.headers()["location"], "/setup");

    let login = nr.get(proxy.url("/login")).send().await.unwrap();
    assert_eq!(login.status(), 302);
    assert_eq!(login.headers()["location"], "/setup");

    let setup = client().get(proxy.url("/setup")).send().await.unwrap();
    assert_eq!(setup.status(), 200);
    assert!(setup.text().await.unwrap().contains("setup"));
}

/// A corrupt or future-version store is a hard boot error, never a silent
/// fall-through to setup mode (which would discard credentials and keys).
#[tokio::test]
async fn corrupt_or_future_store_refuses_to_start() {
    let corrupt = scratch_data_dir();
    std::fs::write(corrupt.join("config.json"), "{ not json").unwrap();
    expect_refuses_to_start(corrupt).await;

    let future = scratch_data_dir();
    std::fs::write(future.join("config.json"), r#"{"version": 2}"#).unwrap();
    expect_refuses_to_start(future).await;
}

/// Rejected canonical history degrades to in-memory history without changing
/// the configured store or canonical bytes.
#[tokio::test]
async fn history_startup_degrades_to_memory_without_mutating_canonical() {
    let mock = start_mock().await;
    let canonical_data_dir = scratch_data_dir();
    std::fs::write(
        canonical_data_dir.join("config.json"),
        serde_json::to_vec_pretty(&StoreOpts::default().json(&mock.url)).unwrap(),
    )
    .unwrap();
    let canonical_proxy =
        start_proxy_in(canonical_data_dir, &[("HISTORY_SAMPLE_SECS", "3600")]).await;
    let canonical_cookie = login(&canonical_proxy).await;
    let canonical_config = api_config(&canonical_proxy, &canonical_cookie).await;
    assert_eq!(
        canonical_config["server"]["history"]["persistence"], "ok",
        "history-startup:canonical: canonical persistence is healthy"
    );
    assert!(
        metrics(&canonical_proxy)
            .await
            .lines()
            .any(|line| line == "nimproxy_history_persistence_degraded 0"),
        "history-startup:canonical: canonical persistence gauge is zero"
    );
    canonical_proxy.terminate();
    for (name, contents) in [
        ("empty", b"".as_slice()),
        (
            "future",
            b"{\"format\":\"nimproxy-history\",\"v\":2,\"kind\":\"boot\",\"timestamp\":1,\"boot_id\":\"future\",\"capacity\":{\"capacity_rpm\":80,\"enabled_keys\":2,\"key_rpms\":[40,40]}}\n",
        ),
    ] {
        let data_dir = scratch_data_dir();
        std::fs::write(
            data_dir.join("config.json"),
            serde_json::to_vec_pretty(&StoreOpts::default().json(&mock.url)).unwrap(),
        )
        .unwrap();
        std::fs::write(data_dir.join("history-v1.jsonl"), contents).unwrap();

        let config_before = std::fs::read(data_dir.join("config.json")).unwrap();
        let history_before = std::fs::read(data_dir.join("history-v1.jsonl")).unwrap();
        let proxy = start_proxy_in(data_dir, &[("HISTORY_SAMPLE_SECS", "3600")]).await;
        let cookie = login(&proxy).await;
        let config = api_config(&proxy, &cookie).await;
        assert_eq!(
            config["server"]["history"]["persistence"],
            "degraded",
            "history-startup:{name}: rejected canonical persistence is degraded"
        );
        assert!(
            metrics(&proxy)
                .await
                .lines()
                .any(|line| line == "nimproxy_history_persistence_degraded 1"),
            "history-startup:{name}: rejected canonical persistence gauge is one"
        );
        let response = client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body(&format!("history startup {name}"), false))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "history-startup:{name}: /v1 remains available");
        assert_eq!(
            std::fs::read(proxy.data_dir.join("config.json")).unwrap(),
            config_before,
            "history-startup:{name}: config bytes remain unchanged"
        );
        assert_eq!(
            std::fs::read(proxy.data_dir.join("history-v1.jsonl")).unwrap(),
            history_before,
            "history-startup:{name}: canonical bytes remain unchanged"
        );
        proxy.terminate();
    }
}

/// `history.jsonl` is deliberately opaque upgrade-reset evidence. Startup
/// emits one bounded path-and-size warning while leaving its bytes untouched.
#[tokio::test]
async fn legacy_history_is_warned_once_without_parsing_or_mutating_it() {
    let mock = start_mock().await;
    let data_dir = scratch_data_dir();
    std::fs::write(
        data_dir.join("config.json"),
        serde_json::to_vec_pretty(&StoreOpts::default().json(&mock.url)).unwrap(),
    )
    .unwrap();
    let legacy = data_dir.join("history.jsonl");
    let legacy_bytes = b"not canonical and never parsed\n";
    std::fs::write(&legacy, legacy_bytes).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_nim-proxy"))
        .env_clear()
        .current_dir(std::env::temp_dir())
        .env("PORT", port.to_string())
        .env("DATA_DIR", &data_dir)
        .env("RUST_LOG", "nim_proxy=warn")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if client()
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            break;
        }
        assert!(Instant::now() < deadline, "proxy did not become healthy");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    child.kill().unwrap();
    let output = child.wait_with_output().unwrap();
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let legacy_display = legacy.display().to_string();
    assert_eq!(output.matches(&legacy_display).count(), 1, "{output}");
    assert!(output.contains(&legacy_bytes.len().to_string()), "{output}");
    assert_eq!(std::fs::read(&legacy).unwrap(), legacy_bytes);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn stale_canonical_temporaries_are_counted_once_without_inspection_or_deletion() {
    let mock = start_mock().await;
    let data_dir = scratch_data_dir();
    std::fs::write(
        data_dir.join("config.json"),
        serde_json::to_vec_pretty(&StoreOpts::default().json(&mock.url)).unwrap(),
    )
    .unwrap();
    let stale = data_dir.join("history-v1.jsonl.tmp-crash-evidence");
    let stale_bytes = b"partial canonical temporary";
    std::fs::write(&stale, stale_bytes).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_nim-proxy"))
        .env_clear()
        .current_dir(std::env::temp_dir())
        .env("PORT", port.to_string())
        .env("DATA_DIR", &data_dir)
        .env("RUST_LOG", "nim_proxy=warn")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if client()
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            break;
        }
        assert!(Instant::now() < deadline, "proxy did not become healthy");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    child.kill().unwrap();
    let output = child.wait_with_output().unwrap();
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output
            .matches("stale canonical history temporaries")
            .count(),
        1,
        "{output}"
    );
    assert!(!output.contains(&stale.display().to_string()), "{output}");
    assert_eq!(std::fs::read(&stale).unwrap(), stale_bytes);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn api_config_history_file_bytes_reports_canonical_history() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[("HISTORY_SAMPLE_SECS", "3600")]).await;
    let cookie = login(&proxy).await;
    let configured = api_config(&proxy, &cookie).await;
    let canonical_bytes = std::fs::metadata(proxy.data_dir.join("history-v1.jsonl"))
        .unwrap()
        .len();
    assert!(canonical_bytes > 0);
    assert_eq!(
        configured["server"]["history"]["file_bytes"],
        canonical_bytes
    );
    assert!(!proxy.data_dir.join("history.jsonl").exists());
}

#[tokio::test]
async fn invalid_noncanonical_and_uninstalled_durable_locales_refuse_to_start() {
    let mock = start_mock().await;
    for (scope, class, locale) in [
        ("default", "invalid", "en_US"),
        ("default", "noncanonical", "EN-us"),
        ("default", "uninstalled", "fr-FR"),
        ("user", "invalid", "en_US"),
        ("user", "noncanonical", "EN-us"),
        ("user", "uninstalled", "fr-FR"),
    ] {
        let data_dir = scratch_data_dir();
        let mut store = StoreOpts::default().json(&mock.url);
        match scope {
            "default" => {
                store
                    .as_object_mut()
                    .unwrap()
                    .insert("default_locale".into(), serde_json::json!(locale));
            }
            "user" => {
                store["users"][0]
                    .as_object_mut()
                    .unwrap()
                    .insert("locale".into(), serde_json::json!(locale));
            }
            _ => unreachable!(),
        }
        std::fs::write(
            data_dir.join("config.json"),
            serde_json::to_vec_pretty(&store).unwrap(),
        )
        .unwrap();
        expect_refuses_to_start(data_dir).await;
        eprintln!("locale-store:startup:{scope}:{class}: refused {locale}");
    }
}

/// The wizard's single POST claims the proxy: creates the superuser, writes a
/// 0600 store, mints a session, closes /setup (404), and opens /v1.
#[tokio::test]
async fn setup_wizard_claims_the_proxy() {
    let mock = start_mock().await;
    let proxy = start_proxy_fresh().await;

    complete_setup(
        &proxy,
        "admin",
        "hunter2hunter2",
        &mock.url,
        &[("nvapi-key", 40)],
    )
    .await;

    // Credentials file is owner-only.
    let mode = std::fs::metadata(proxy.data_dir.join("config.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "config store must be 0600");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let history_mode = std::fs::metadata(proxy.data_dir.join("history-v1.jsonl"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(history_mode, 0o600, "canonical history file must be 0600");
    }

    // The wizard is gone once the proxy is claimed.
    assert_eq!(
        client()
            .get(proxy.url("/setup"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let post_setup = client()
        .post(proxy.url("/setup"))
        .json(&serde_json::json!({"username": "x", "password": "yyyyyyyyyy"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        post_setup.status(),
        409,
        "POST /setup conflicts after claim"
    );
    let post_validate = client()
        .post(proxy.url("/setup/validate-key"))
        .json(&serde_json::json!({"key": "k"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        post_validate.status(),
        409,
        "POST /setup/validate-key conflicts after claim"
    );

    // The /v1 setup gate has lifted: it no longer answers 503 setup_required.
    // A wizard-created store is keyed (see setup.html: "create client keys in
    // Settings"), so with no client key yet it fails closed with 401.
    let r = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "keyed /v1 with no client key fails closed");
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "unauthorized");
}

/// Every rejection from the JSON control plane keeps the same typed envelope
/// and leaves the durable config store untouched. Removing the narrow
/// extractors or `/api` fallbacks makes one or more rows answer Axum's plain
/// text defaults instead.
#[tokio::test]
async fn control_plane_rejections_are_typed() {
    struct RejectionCase {
        method: reqwest::Method,
        path: &'static str,
        content_type: Option<&'static str>,
        body: &'static str,
        status: reqwest::StatusCode,
        code: &'static str,
        message: &'static str,
        oversized: bool,
    }

    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let cookie = login(&proxy).await;
    let before = std::fs::read(proxy.data_dir.join("config.json")).unwrap();
    let cases = [
        RejectionCase {
            method: reqwest::Method::POST,
            path: "/api/settings/upstream",
            content_type: Some("application/json"),
            body: "{",
            status: reqwest::StatusCode::BAD_REQUEST,
            code: "invalid_json",
            message: "invalid JSON",
            oversized: false,
        },
        RejectionCase {
            method: reqwest::Method::POST,
            path: "/api/settings/upstream",
            content_type: None,
            body: r#"{"base_url":"http://example.invalid"}"#,
            status: reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            code: "unsupported_media_type",
            message: "Content-Type must be application/json",
            oversized: false,
        },
        RejectionCase {
            method: reqwest::Method::POST,
            path: "/api/settings/upstream",
            content_type: Some("text/plain"),
            body: r#"{"base_url":"http://example.invalid"}"#,
            status: reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            code: "unsupported_media_type",
            message: "Content-Type must be application/json",
            oversized: false,
        },
        RejectionCase {
            method: reqwest::Method::POST,
            path: "/api/settings/upstream",
            content_type: Some("application/json"),
            body: "",
            status: reqwest::StatusCode::PAYLOAD_TOO_LARGE,
            code: "body_too_large",
            message: "request body is too large",
            oversized: true,
        },
        RejectionCase {
            method: reqwest::Method::GET,
            path: "/api/dashboard?from=not-a-timestamp",
            content_type: None,
            body: "",
            status: reqwest::StatusCode::BAD_REQUEST,
            code: "invalid_query",
            message: "invalid query",
            oversized: false,
        },
        RejectionCase {
            method: reqwest::Method::GET,
            path: "/api/not-a-route",
            content_type: None,
            body: "",
            status: reqwest::StatusCode::NOT_FOUND,
            code: "not_found",
            message: "not found",
            oversized: false,
        },
        RejectionCase {
            method: reqwest::Method::PUT,
            path: "/api/dashboard",
            content_type: None,
            body: "",
            status: reqwest::StatusCode::METHOD_NOT_ALLOWED,
            code: "method_not_allowed",
            message: "method not allowed",
            oversized: false,
        },
        RejectionCase {
            method: reqwest::Method::POST,
            path: "/setup",
            content_type: Some("application/json"),
            body: r#"{"username":"another","password":"long-enough-password"}"#,
            status: reqwest::StatusCode::CONFLICT,
            code: "setup_complete",
            message: "setup is already complete",
            oversized: false,
        },
        RejectionCase {
            method: reqwest::Method::POST,
            path: "/setup/validate-key",
            content_type: Some("application/json"),
            body: r#"{"key":"another"}"#,
            status: reqwest::StatusCode::CONFLICT,
            code: "setup_complete",
            message: "setup is already complete",
            oversized: false,
        },
    ];

    let mut failures = Vec::new();
    for case in cases {
        let body = if case.oversized {
            "x".repeat(64 * 1024 * 1024 + 1)
        } else {
            case.body.to_owned()
        };
        let mut request = client()
            .request(case.method.clone(), proxy.url(case.path))
            .body(body);
        if case.path.starts_with("/api/") {
            request = request.header("cookie", &cookie);
        }
        if let Some(content_type) = case.content_type {
            request = request.header("content-type", content_type);
        }

        let response = request.send().await.unwrap();
        let mut errors = Vec::new();
        if response.status() != case.status {
            errors.push(format!("status {:?}", response.status()));
        }
        match response.headers().get(CONTENT_TYPE) {
            Some(content_type) if content_type == "application/json" => {}
            Some(content_type) => errors.push(format!("content-type {content_type:?}")),
            None => errors.push("content-type missing".to_owned()),
        }
        let body = response.bytes().await.unwrap();
        let expected = format!(
            r#"{{"error":{{"code":"{}","message":"{}","type":"proxy_error"}}}}"#,
            case.code, case.message
        );
        if body.as_ref() != expected.as_bytes() {
            errors.push(format!("body {:?}", String::from_utf8_lossy(&body)));
        }
        if std::fs::read(proxy.data_dir.join("config.json")).unwrap() != before {
            errors.push("config.json changed".to_owned());
        }
        if !errors.is_empty() {
            failures.push(format!(
                "{} {}: {}",
                case.method,
                case.path,
                errors.join(", ")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "rejection failures:\n{}",
        failures.join("\n")
    );
}

/// The raw-request setup handlers phase-check first, but while setup remains
/// open their manual `ApiJson` call must preserve the typed extractor errors.
#[tokio::test]
async fn open_setup_posts_keep_typed_extractor_rejections() {
    struct OpenCase {
        content_type: Option<&'static str>,
        body: &'static str,
        status: reqwest::StatusCode,
        code: &'static str,
        message: &'static str,
        oversized: bool,
    }

    let proxy = start_proxy_fresh().await;
    let before = std::fs::read(proxy.data_dir.join("config.json")).ok();
    let cases = [
        OpenCase {
            content_type: Some("application/json"),
            body: "{",
            status: reqwest::StatusCode::BAD_REQUEST,
            code: "invalid_json",
            message: "invalid JSON",
            oversized: false,
        },
        OpenCase {
            content_type: None,
            body: r#"{"key":"k"}"#,
            status: reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            code: "unsupported_media_type",
            message: "Content-Type must be application/json",
            oversized: false,
        },
        OpenCase {
            content_type: Some("text/plain"),
            body: r#"{"key":"k"}"#,
            status: reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            code: "unsupported_media_type",
            message: "Content-Type must be application/json",
            oversized: false,
        },
        OpenCase {
            content_type: Some("application/json"),
            body: "",
            status: reqwest::StatusCode::PAYLOAD_TOO_LARGE,
            code: "body_too_large",
            message: "request body is too large",
            oversized: true,
        },
    ];

    for path in ["/setup", "/setup/validate-key"] {
        for case in &cases {
            let body = if case.oversized {
                "x".repeat(64 * 1024 * 1024 + 1)
            } else {
                case.body.to_owned()
            };
            let mut request = client().post(proxy.url(path)).body(body);
            if let Some(content_type) = case.content_type {
                request = request.header(CONTENT_TYPE, content_type);
            }
            assert_exact_api_error(
                request.send().await.unwrap(),
                case.status,
                case.code,
                case.message,
            )
            .await;
            assert_eq!(
                std::fs::read(proxy.data_dir.join("config.json")).ok(),
                before
            );
        }
    }
}

/// An unknown `/api/*` path is still an operator-surface request: it must not
/// disclose its typed fallback before the setup/session gate has run.
#[tokio::test]
async fn unknown_control_plane_paths_are_gated_before_fallback() {
    let fresh = start_proxy_fresh().await;
    let before_fresh = std::fs::read(fresh.data_dir.join("config.json")).ok();
    assert_exact_api_error(
        client()
            .get(fresh.url("/api/not-a-route"))
            .send()
            .await
            .unwrap(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "setup_required",
        "first-time setup has not been completed; open the dashboard to create the superuser",
    )
    .await;
    assert_eq!(
        std::fs::read(fresh.data_dir.join("config.json")).ok(),
        before_fresh
    );

    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let before = std::fs::read(proxy.data_dir.join("config.json")).unwrap();
    assert_exact_api_error(
        client()
            .get(proxy.url("/api/not-a-route"))
            .send()
            .await
            .unwrap(),
        reqwest::StatusCode::UNAUTHORIZED,
        "unauthorized",
        "authentication required (session cookie, or Authorization: Bearer <username>:<password>)",
    )
    .await;
    assert_eq!(
        std::fs::read(proxy.data_dir.join("config.json")).unwrap(),
        before
    );

    let cookie = login(&proxy).await;
    assert_exact_api_error(
        client()
            .get(proxy.url("/api/not-a-route"))
            .header("cookie", cookie)
            .send()
            .await
            .unwrap(),
        reqwest::StatusCode::NOT_FOUND,
        "not_found",
        "not found",
    )
    .await;
    assert_eq!(
        std::fs::read(proxy.data_dir.join("config.json")).unwrap(),
        before
    );
}

/// Once claimed, setup POSTs reject before they inspect headers or buffer a
/// body, so the closed phase always has one stable conflict result.
#[tokio::test]
async fn closed_setup_posts_win_before_body_rejections() {
    struct ClosedCase {
        content_type: Option<&'static str>,
        body: &'static str,
    }

    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let before = std::fs::read(proxy.data_dir.join("config.json")).unwrap();
    let cases = [
        ClosedCase {
            content_type: Some("application/json"),
            body: "{",
        },
        ClosedCase {
            content_type: None,
            body: r#"{"key":"k"}"#,
        },
        ClosedCase {
            content_type: Some("text/plain"),
            body: r#"{"key":"k"}"#,
        },
    ];

    for path in ["/setup", "/setup/validate-key"] {
        for case in &cases {
            let mut request = client().post(proxy.url(path)).body(case.body);
            if let Some(content_type) = case.content_type {
                request = request.header("content-type", content_type);
            }
            assert_exact_api_error(
                request.send().await.unwrap(),
                reqwest::StatusCode::CONFLICT,
                "setup_complete",
                "setup is already complete",
            )
            .await;
            assert_eq!(
                std::fs::read(proxy.data_dir.join("config.json")).unwrap(),
                before
            );
        }
        assert_closed_setup_rejects_oversized_body(&proxy, path).await;
        assert_eq!(
            std::fs::read(proxy.data_dir.join("config.json")).unwrap(),
            before
        );
    }
}

/// The claim persists: after a restart on the same data dir, the created user
/// can log in and the setup-provided key is still in the pool.
#[tokio::test]
async fn setup_claim_survives_restart() {
    let mock = start_mock().await;
    let proxy = start_proxy_fresh().await;
    // TEST_PASSWORD so `login_as` (which uses it) works after the restart.
    complete_setup(
        &proxy,
        "admin",
        TEST_PASSWORD,
        &mock.url,
        &[("nvapi-key", 40)],
    )
    .await;

    let proxy = restart(proxy, &[]).await;

    // Session auth works against the persisted user.
    let cookie = login_as(&proxy, "admin").await;
    // The persisted store rehydrated: one lane (the setup key), keyed /v1.
    let cfg: serde_json::Value = client()
        .get(proxy.url("/api/dashboard/now"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cfg["lanes"], 1, "setup key survived the restart");
    assert_eq!(cfg["auth"], true, "keyed /v1 mode persisted");

    // /v1 is live behind auth (not the pre-setup 503).
    let r = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "keyed /v1 fails closed, no longer 503");
}

/// Lockout recovery: a store whose users were hand-emptied on the volume (its
/// keys left with dangling owners) boots into setup mode; the new superuser
/// adopts the orphan keys, so /v1 works without re-supplying them.
#[tokio::test]
async fn recovery_store_adopts_orphan_keys() {
    let mock = start_mock().await;
    let dir = scratch_data_dir();
    let fixture = serde_json::json!({
        "version": 1,
        "upstream": {
            "base_url": mock.url,
            "nim_keys": [{"key": "orphan-key", "owner": "ghost", "enabled": true, "rpm": 40}],
        },
        // Open /v1 so the test can observe the adopted key reaching upstream
        // (a wizard-created store would be keyed; this recovery store predates it).
        "client_auth": {"mode": "open"},
        "users": [],
    });
    std::fs::write(
        dir.join("config.json"),
        serde_json::to_string_pretty(&fixture).unwrap(),
    )
    .unwrap();

    let proxy = start_proxy_in(dir, &[]).await;
    // No superuser -> setup mode despite the store existing.
    assert_eq!(
        client()
            .get(proxy.url("/setup"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // Claim with an empty key list: the orphan is re-owned by the superuser.
    complete_setup(&proxy, "admin", TEST_PASSWORD, &mock.url, &[]).await;

    let r = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "adopted key serves /v1");
    assert_eq!(mock.state.hit_keys(), vec!["orphan-key".to_owned()]);
}

/// The wizard rejects a password shorter than 10 characters up front.
#[tokio::test]
async fn setup_rejects_weak_password() {
    let proxy = start_proxy_fresh().await;
    let resp = client()
        .post(proxy.url("/setup"))
        .json(&serde_json::json!({
            "username": "admin", "password": "short", "nim_keys": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "weak_password");
}

/// The wizard's pre-auth key probe reports how many models an upstream key can
/// see (the mock exposes exactly one).
#[tokio::test]
async fn setup_validate_key_probes_upstream() {
    let mock = start_mock().await;
    let proxy = start_proxy_fresh().await;
    let resp = client()
        .post(proxy.url("/setup/validate-key"))
        .json(&serde_json::json!({"key": "nvapi-probe", "base_url": mock.url}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["models"], 1, "{body}");
}

// ---------- security hardening ----------

/// Post-setup, the operator surface (dashboard, metrics, history) always
/// requires auth — there is no insecure mode. Health stays public.
#[tokio::test]
async fn operator_surface_always_requires_auth() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    // Health stays public (load balancers / Docker probe).
    assert_eq!(
        client()
            .get(proxy.url("/health"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // Metrics require creds; Bearer <user>:<pass> works (Prometheus scrape path).
    assert_eq!(
        client()
            .get(proxy.url("/metrics"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let ok = client()
        .get(proxy.url("/metrics"))
        .header(
            "authorization",
            format!("Bearer {}:{TEST_PASSWORD}", support::TEST_USER),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);

    // Both dashboard data surfaces require credentials.
    for path in ["/api/dashboard", "/api/dashboard/now"] {
        assert_eq!(
            client().get(proxy.url(path)).send().await.unwrap().status(),
            401,
            "{path} requires auth"
        );
    }

    // Browser hitting the dashboard without a session is redirected to /login.
    let nr = no_redirect_client();
    let redir = nr
        .get(proxy.url("/"))
        .header("accept", "text/html")
        .send()
        .await
        .unwrap();
    assert_eq!(redir.status(), 302);
    assert_eq!(redir.headers()["location"], "/login");
    assert_eq!(
        nr.get(proxy.url("/login")).send().await.unwrap().status(),
        200
    );

    // Wrong password is rejected; correct password sets a hardened session cookie.
    let bad = nr
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("username=root&password=wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);

    let good = nr
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "username={}&password={TEST_PASSWORD}",
            support::TEST_USER
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(good.status(), 303);
    let cookie = good.headers()["set-cookie"].to_str().unwrap().to_owned();
    assert!(cookie.contains("nimproxy_session="));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));

    // The session cookie then opens the dashboard.
    let session = cookie.split(';').next().unwrap();
    let dash = nr
        .get(proxy.url("/"))
        .header("accept", "text/html")
        .header("cookie", session)
        .send()
        .await
        .unwrap();
    assert_eq!(dash.status(), 200);
    assert!(dash.text().await.unwrap().contains("NIM"));
}

#[tokio::test]
async fn model_label_is_sanitized_in_metrics() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    // A malicious model id carrying Prometheus/HTML/log injection payloads.
    let evil = "<img src=x onerror=alert(1)>\"} pwn 1\nmeta";
    let body = serde_json::json!({
        "model": evil,
        "messages": [{"role": "user", "content": "hi"}]
    });
    let r = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let metrics = metrics(&proxy).await;
    // The sanitized label keeps only safe chars; none of the injection
    // characters survive, and no spurious `pwn` series was created.
    // The model label value (after `model="`) must contain only safe chars —
    // no `<`, `>`, quote, brace, or newline that could break the exposition
    // format, inject a series, or become HTML. The payload collapses to one
    // harmless alphanumeric token on a single line.
    let req_line = metrics
        .lines()
        .find(|l| l.starts_with("nimproxy_requests_total"))
        .expect("requests_total present");
    let value = req_line
        .split("model=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("model label present");
    assert!(
        value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':')),
        "unsafe chars in model label: {value:?}"
    );
    // No injected standalone series (the `\n... pwn 1` part of the payload).
    assert!(
        !metrics.lines().any(|l| l.trim_start().starts_with("pwn")),
        "injected metric series present"
    );
}

#[tokio::test]
async fn dashboard_sends_security_headers() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    // The dashboard now requires a session; assert the CSP on an authenticated
    // 200 (the hardening headers wrap every response, success or redirect).
    let cookie = login(&proxy).await;
    let resp = client()
        .get(proxy.url("/"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let h = resp.headers();
    let csp = h["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(
        csp.contains("connect-src 'self'"),
        "blocks cross-origin exfil"
    );
    assert_eq!(
        csp,
        "default-src 'none'; img-src 'self' data:; style-src 'self'; script-src 'self'; \
         connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
    );
    assert!(!csp.contains("unsafe-inline"));
    assert!(!csp.contains("http:"));
    assert!(!csp.contains("https:"));
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["x-frame-options"], "DENY");
}

#[derive(Clone, Copy, Debug)]
enum PresentationActor {
    BeforeSetup,
    Anonymous,
    Authenticated,
}

#[derive(Clone, Copy, Debug)]
struct PresentationRoute {
    content_type: Option<&'static str>,
    path: &'static str,
    statuses: [u16; 3],
}

#[tokio::test]
async fn presentation_assets_are_gated() {
    const CSP: &str = "default-src 'none'; img-src 'self' data:; style-src 'self'; \
        script-src 'self'; connect-src 'self'; frame-ancestors 'none'; \
        base-uri 'none'; form-action 'self'";
    const PAGE_ROUTES: &[PresentationRoute] = &[
        PresentationRoute {
            content_type: Some("text/html; charset=utf-8"),
            path: "/",
            statuses: [302, 302, 200],
        },
        PresentationRoute {
            content_type: Some("text/html; charset=utf-8"),
            path: "/dash",
            statuses: [302, 302, 200],
        },
        PresentationRoute {
            content_type: Some("text/html; charset=utf-8"),
            path: "/login",
            statuses: [302, 200, 302],
        },
        PresentationRoute {
            content_type: Some("text/html; charset=utf-8"),
            path: "/setup",
            statuses: [200, 404, 404],
        },
    ];
    const PUBLIC_ASSETS: &[PresentationRoute] = &[
        PresentationRoute {
            content_type: Some("text/css; charset=utf-8"),
            path: "/assets/public/public.css",
            statuses: [200, 200, 200],
        },
        PresentationRoute {
            content_type: Some("text/javascript; charset=utf-8"),
            path: "/assets/public/setup.js",
            statuses: [200, 200, 200],
        },
        PresentationRoute {
            content_type: Some("text/javascript; charset=utf-8"),
            path: "/assets/public/login.js",
            statuses: [200, 200, 200],
        },
    ];
    const OPERATOR_ASSETS: &[PresentationRoute] = &[
        PresentationRoute {
            content_type: Some("text/css; charset=utf-8"),
            path: "/assets/operator/operator.css",
            statuses: [503, 401, 200],
        },
        PresentationRoute {
            content_type: Some("text/javascript; charset=utf-8"),
            path: "/assets/operator/shared.js",
            statuses: [503, 401, 200],
        },
        PresentationRoute {
            content_type: Some("text/javascript; charset=utf-8"),
            path: "/assets/operator/dashboard.js",
            statuses: [503, 401, 200],
        },
        PresentationRoute {
            content_type: Some("text/javascript; charset=utf-8"),
            path: "/assets/operator/settings.js",
            statuses: [503, 401, 200],
        },
    ];

    let before_setup = start_proxy_fresh().await;
    let mock = start_mock().await;
    let configured = start_proxy_with(
        &mock.url,
        StoreOpts {
            clients: vec![(
                "presentation-private-client".into(),
                "presentation-private-secret".into(),
            )],
            nim_keys: vec![("presentation-private-nim-key".into(), 40)],
            ..Default::default()
        },
        &[],
    )
    .await;
    let cookie = login(&configured).await;
    let actors = [
        (PresentationActor::BeforeSetup, &before_setup, None),
        (PresentationActor::Anonymous, &configured, None),
        (
            PresentationActor::Authenticated,
            &configured,
            Some(cookie.as_str()),
        ),
    ];
    let mut public_bodies: std::collections::HashMap<&str, Vec<u8>> =
        std::collections::HashMap::new();

    for route in PAGE_ROUTES
        .iter()
        .chain(PUBLIC_ASSETS)
        .chain(OPERATOR_ASSETS)
    {
        for (actor_index, (actor, proxy, cookie)) in actors.iter().enumerate() {
            let mut request = no_redirect_client().get(proxy.url(route.path));
            if PAGE_ROUTES.iter().any(|page| page.path == route.path) {
                request = request.header("accept", "text/html");
            }
            if let Some(cookie) = cookie {
                request = request.header("cookie", *cookie);
            }
            let response = request.send().await.unwrap();
            assert_eq!(
                response.status().as_u16(),
                route.statuses[actor_index],
                "presentation-assets:status: GET {} actor={actor:?}",
                route.path
            );
            assert_eq!(
                response
                    .headers()
                    .get("cache-control")
                    .and_then(|value| value.to_str().ok()),
                Some("no-store"),
                "presentation-assets:no-store: GET {} actor={actor:?}",
                route.path
            );
            assert_eq!(
                response
                    .headers()
                    .get("content-security-policy")
                    .and_then(|value| value.to_str().ok()),
                Some(CSP),
                "presentation-assets:csp: GET {} actor={actor:?}",
                route.path
            );
            if response.status().is_success() {
                assert_eq!(
                    response
                        .headers()
                        .get(CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok()),
                    route.content_type,
                    "presentation-assets:content-type: GET {} actor={actor:?}",
                    route.path
                );
            }
            let body = response.bytes().await.unwrap().to_vec();
            if PUBLIC_ASSETS.iter().any(|public| public.path == route.path) {
                if let Some(first) = public_bodies.get(route.path) {
                    assert_eq!(
                        &body, first,
                        "presentation-assets:public-byte-isolation: GET {} actor={actor:?}",
                        route.path
                    );
                } else {
                    public_bodies.insert(route.path, body.clone());
                }
                let text = String::from_utf8_lossy(&body);
                for forbidden in [
                    "settings.",
                    "presentation-private-client",
                    "presentation-private-secret",
                    "presentation-private-nim-key",
                    TEST_PASSWORD,
                ] {
                    assert!(
                        !text.contains(forbidden),
                        "presentation-assets:public-byte-isolation: GET {} contains {forbidden:?}",
                        route.path
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn locale_catalog_routes_are_gated() {
    const CSP: &str = "default-src 'none'; img-src 'self' data:; style-src 'self'; \
        script-src 'self'; connect-src 'self'; frame-ancestors 'none'; \
        base-uri 'none'; form-action 'self'";
    const ROUTES: &[PresentationRoute] = &[
        PresentationRoute {
            content_type: Some("application/json"),
            path: "/api/locale-bootstrap",
            statuses: [200, 200, 200],
        },
        PresentationRoute {
            content_type: Some("application/json"),
            path: "/assets/public/locales/en-US.json",
            statuses: [200, 200, 200],
        },
        PresentationRoute {
            content_type: Some("application/json"),
            path: "/assets/operator/locales/en-US.json",
            statuses: [503, 401, 200],
        },
        PresentationRoute {
            content_type: None,
            path: "/assets/public/locales/en-XA.json",
            statuses: [404, 404, 404],
        },
        PresentationRoute {
            content_type: None,
            path: "/assets/operator/locales/en-XA.json",
            statuses: [503, 401, 404],
        },
        PresentationRoute {
            content_type: None,
            path: "/assets/public/locales/fr-FR.json",
            statuses: [404, 404, 404],
        },
        PresentationRoute {
            content_type: None,
            path: "/assets/operator/locales/fr-FR.json",
            statuses: [503, 401, 404],
        },
    ];

    let before_setup = start_proxy_fresh().await;
    let mock = start_mock().await;
    let configured = start_proxy(&mock.url, &[]).await;
    let cookie = login(&configured).await;
    let actors = [
        (PresentationActor::BeforeSetup, &before_setup, None),
        (PresentationActor::Anonymous, &configured, None),
        (
            PresentationActor::Authenticated,
            &configured,
            Some(cookie.as_str()),
        ),
    ];
    let public_catalog = include_str!("fixtures/locales/public-en-US.json");
    let source: serde_json::Value =
        serde_json::from_str(include_str!("../src/web/locales/en-US.json"))
            .expect("canonical authoring catalog");
    let operator_messages: std::collections::BTreeMap<String, String> = source["messages"]
        .as_object()
        .expect("authoring messages")
        .iter()
        .map(|(id, message)| {
            (
                id.clone(),
                message["en"]
                    .as_str()
                    .expect("authoring message text")
                    .to_owned(),
            )
        })
        .collect();
    let operator_catalog = serde_json::to_string(&serde_json::json!({
        "locale": "en-US",
        "messages": operator_messages,
    }))
    .expect("operator projection");

    for route in ROUTES {
        for (actor_index, (actor, proxy, cookie)) in actors.iter().enumerate() {
            let mut request = no_redirect_client().get(proxy.url(route.path));
            if let Some(cookie) = cookie {
                request = request.header("cookie", *cookie);
            }
            let response = request.send().await.unwrap();
            assert_eq!(
                response.status().as_u16(),
                route.statuses[actor_index],
                "locale-routes:status: GET {} actor={actor:?}",
                route.path
            );
            assert_eq!(
                response
                    .headers()
                    .get("cache-control")
                    .and_then(|value| value.to_str().ok()),
                Some("no-store"),
                "locale-routes:no-store: GET {} actor={actor:?}",
                route.path
            );
            assert_eq!(
                response
                    .headers()
                    .get("content-security-policy")
                    .and_then(|value| value.to_str().ok()),
                Some(CSP),
                "locale-routes:csp: GET {} actor={actor:?}",
                route.path
            );
            if response.status().is_success() {
                assert_eq!(
                    response
                        .headers()
                        .get(CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok()),
                    route.content_type,
                    "locale-routes:content-type: GET {} actor={actor:?}",
                    route.path
                );
                let body = response.text().await.unwrap();
                if route.path == "/api/locale-bootstrap" {
                    assert_eq!(
                        body, r#"{"installed_locales":["en-US"],"server_default":"en-US"}"#,
                        "locale-routes:bootstrap-bytes"
                    );
                } else {
                    let expected = if route.path.contains("/public/") {
                        public_catalog
                    } else {
                        operator_catalog.as_str()
                    };
                    assert_eq!(
                        body, expected,
                        "locale-routes:exact-projection: GET {}",
                        route.path
                    );
                    let catalog: serde_json::Value =
                        serde_json::from_str(&body).expect("catalog JSON");
                    assert_eq!(catalog["locale"], "en-US");
                    let messages = catalog["messages"].as_object().expect("plain messages");
                    assert_eq!(
                        messages.get("common.app_name"),
                        Some(&serde_json::Value::String("NIM Proxy".into()))
                    );
                    assert!(
                        messages.values().all(serde_json::Value::is_string),
                        "locale-routes:plain-strings"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn worker_exhaustion_governs_the_model_and_spares_the_lane() {
    let mock = start_mock().await;
    mock.state.push(Behavior::WorkerExhausted);
    let proxy = start_proxy(&mock.url, &[]).await;

    let started = Instant::now();
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "hello world");
    assert_eq!(mock.state.hit_count(), 2, "one exhausted try, one success");
    // The retry waited out the governor's ~2s drain gap, not the 10s default
    // lane cooldown a plain 429-without-Retry-After would have earned.
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "retry took {:?} — looks like a lane cooldown, not a model drain gap",
        started.elapsed()
    );

    let metrics = metrics(&proxy).await;
    assert!(
        metrics.contains(r#"nimproxy_worker_exhausted_total{model="mock/model-a"} 1"#),
        "exhaustion counted: {metrics}"
    );
    assert!(
        !metrics.contains("nimproxy_lane_cooldown_total"),
        "worker exhaustion must never cool down a lane: {metrics}"
    );
    assert!(
        metrics.contains(r#"nimproxy_model_limit{model="mock/model-a"} 1"#),
        "governor engaged at max(1, inflight/2) = 1: {metrics}"
    );
    assert!(
        metrics.contains(r#"nimproxy_model_inflight{model="mock/model-a"} 0"#),
        "permit released after completion: {metrics}"
    );
}

#[tokio::test]
async fn worker_exhaustion_streaming_retries_inside_the_stream() {
    let mock = start_mock().await;
    mock.state.push(Behavior::WorkerExhausted);
    let proxy = start_proxy(&mock.url, &[]).await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "stream commits to 200 before retrying");
    let body = read_sse(resp).await;
    assert!(body.contains(": retrying"), "retry notice sent: {body}");
    assert!(body.contains("hello"), "content delivered: {body}");
    assert!(body.contains("data: [DONE]"), "stream completed: {body}");

    let metrics = metrics(&proxy).await;
    assert!(
        metrics.contains(r#"nimproxy_worker_exhausted_total{model="mock/model-a"} 1"#),
        "exhaustion counted: {metrics}"
    );
    assert!(
        !metrics.contains("nimproxy_lane_cooldown_total"),
        "worker exhaustion must never cool down a lane: {metrics}"
    );
}

// ---------------------------------------------------------------------------
// Settings API: role filtering, ownership, invariants, live application.
// ---------------------------------------------------------------------------

async fn api_config(proxy: &support::Proxy, cookie: &str) -> serde_json::Value {
    client()
        .get(proxy.url("/api/config"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn post_json(
    proxy: &support::Proxy,
    cookie: &str,
    path: &str,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client()
        .post(proxy.url(path))
        .header("cookie", cookie)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let v = resp.json().await.unwrap_or_default();
    (status, v)
}

fn locale_store_bytes(proxy: &support::Proxy) -> Vec<u8> {
    std::fs::read(proxy.data_dir.join("config.json"))
        .expect("locale-preferences: durable config.json")
}

async fn locale_post(
    proxy: &support::Proxy,
    cookie: Option<&str>,
    path: &str,
    body: &serde_json::Value,
) -> (reqwest::StatusCode, Option<String>, Vec<u8>) {
    let mut request = client().post(proxy.url(path)).json(body);
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = response.bytes().await.unwrap().to_vec();
    (status, content_type, bytes)
}

async fn locale_post_raw(
    proxy: &support::Proxy,
    cookie: Option<&str>,
    path: &str,
    content_type: Option<&str>,
    body: &str,
) -> (reqwest::StatusCode, Option<String>, Vec<u8>) {
    let mut request = client().post(proxy.url(path)).body(body.to_owned());
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    if let Some(content_type) = content_type {
        request = request.header(CONTENT_TYPE, content_type);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = response.bytes().await.unwrap().to_vec();
    (status, content_type, bytes)
}

async fn locale_config_revision(proxy: &support::Proxy, cookie: &str) -> u64 {
    dashboard_now(proxy, cookie).await["config_revision"]
        .as_u64()
        .expect("locale-preferences: numeric config revision")
}

fn locale_response_code(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value["error"]["code"].as_str().map(str::to_owned)
}

fn locale_response_type(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value["error"]["type"].as_str().map(str::to_owned)
}

fn locale_response_message(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value["error"]["message"].as_str().map(str::to_owned)
}

fn record_locale_response(
    failures: &mut Vec<String>,
    label: &str,
    status: reqwest::StatusCode,
    content_type: Option<&str>,
    bytes: &[u8],
    expected_status: reqwest::StatusCode,
    expected_code: Option<&str>,
) {
    if status != expected_status {
        failures.push(format!(
            "{label}: status {status}, expected {expected_status}"
        ));
    }
    if content_type != Some("application/json") {
        failures.push(format!(
            "{label}: content-type {content_type:?}, expected application/json"
        ));
    }
    match expected_code {
        Some(code) => {
            if locale_response_code(bytes).as_deref() != Some(code) {
                failures.push(format!(
                    "{label}: code {:?}, expected {code}; body={}",
                    locale_response_code(bytes),
                    String::from_utf8_lossy(bytes)
                ));
            }
            if locale_response_type(bytes).as_deref() != Some("proxy_error") {
                failures.push(format!(
                    "{label}: error type {:?}, expected proxy_error",
                    locale_response_type(bytes)
                ));
            }
        }
        None => {
            if bytes != br#"{"ok":true}"# {
                failures.push(format!(
                    "{label}: success bytes {}, expected {{\"ok\":true}}",
                    String::from_utf8_lossy(bytes)
                ));
            }
        }
    }
}

async fn locale_config_body(proxy: &support::Proxy, cookie: &str) -> (u16, String) {
    let response = client()
        .get(proxy.url("/api/config"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.text().await.unwrap();
    (status, body)
}

fn record_only_user_locale_changed(
    failures: &mut Vec<String>,
    label: &str,
    before_bytes: &[u8],
    after_bytes: &[u8],
    username: &str,
    expected_locale: Option<&str>,
) {
    let before: serde_json::Value =
        serde_json::from_slice(before_bytes).expect("locale-preferences: before store JSON");
    let after: serde_json::Value =
        serde_json::from_slice(after_bytes).expect("locale-preferences: after store JSON");
    let mut expected = before.clone();
    let users = expected["users"]
        .as_array_mut()
        .expect("locale-preferences: users array");
    let caller = users
        .iter_mut()
        .find(|entry| entry["username"] == username)
        .unwrap_or_else(|| panic!("locale-preferences: missing caller {username}"));
    match expected_locale {
        Some(locale) => {
            caller
                .as_object_mut()
                .expect("locale-preferences: user object")
                .insert("locale".into(), serde_json::json!(locale));
        }
        None => {
            caller
                .as_object_mut()
                .expect("locale-preferences: user object")
                .remove("locale");
        }
    }
    if after["users"] != expected["users"] {
        failures.push(format!(
            "{label}: complete users array changed outside authenticated caller; before={} after={} expected={}",
            before["users"], after["users"], expected["users"]
        ));
    }
    if after != expected {
        failures.push(format!(
            "{label}: persisted store changed outside the caller locale; before={} after={} expected={expected}",
            String::from_utf8_lossy(before_bytes),
            String::from_utf8_lossy(after_bytes),
        ));
    }
}

#[tokio::test]
async fn locale_preferences_are_fail_closed() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        extra_users: vec![
            ("locale-admin".into(), "admin".into()),
            ("locale-user".into(), "user".into()),
        ],
        ..Default::default()
    };
    let data_dir = scratch_data_dir();
    let mut store = opts.json(&mock.url);
    // Complete v1 document immediately before locale fields were added. Keep
    // every older additive default explicit so the first locale save can be
    // compared as exactly one schema addition.
    store.as_object_mut().unwrap().insert(
        "dashboard".into(),
        serde_json::json!({
            "default_window_days": 30,
            "slo_target_percent": 99.9,
        }),
    );
    store.as_object_mut().unwrap().insert(
        "governor".into(),
        serde_json::json!({
            "enabled": true,
            "overrides": {},
        }),
    );
    store
        .as_object_mut()
        .unwrap()
        .insert("history".into(), serde_json::json!({"days": 30}));
    store["limits"]
        .as_object_mut()
        .unwrap()
        .insert("models_ttl_secs".into(), serde_json::json!(600));
    std::fs::write(
        data_dir.join("config.json"),
        serde_json::to_vec_pretty(&store).unwrap(),
    )
    .unwrap();
    let proxy = start_proxy_in(data_dir, &[]).await;
    let superuser = login(&proxy).await;
    let admin = login_as(&proxy, "locale-admin").await;
    let user = login_as(&proxy, "locale-user").await;
    let mut failures = Vec::new();
    let mut rejection_byte_checks = 0;

    // GET /api/config is real server output, not a test-side Rust-wire copy.
    let (super_status, super_body) = locale_config_body(&proxy, &superuser).await;
    if super_status != 200 {
        failures.push(format!("locale-config: superuser status {super_status}"));
    }
    let super_json: serde_json::Value =
        serde_json::from_str(&super_body).unwrap_or(serde_json::Value::Null);
    if super_json.get("locale") != Some(&serde_json::Value::Null) {
        failures.push(format!(
            "locale-config: absent superuser override must be null: {super_body}"
        ));
    }
    if super_json["server"]["default_locale"] != "en-US" {
        failures.push(format!(
            "locale-config: admin server.default_locale missing: {super_body}"
        ));
    }
    let client_keys_at = super_body.find("\"client_keys\"").unwrap_or(usize::MAX);
    let locale_at = super_body.find("\"locale\"").unwrap_or(usize::MAX);
    let mode_at = super_body.find("\"mode\"").unwrap_or(usize::MAX);
    if !(client_keys_at < locale_at && locale_at < mode_at) {
        failures.push(format!(
            "locale-config: top-level wire order must be client_keys, locale, mode: {super_body}"
        ));
    }
    let base_url_at = super_body.find("\"base_url\"").unwrap_or(usize::MAX);
    let dashboard_at = super_body.find("\"dashboard\"").unwrap_or(usize::MAX);
    let default_locale_at = super_body.find("\"default_locale\"").unwrap_or(usize::MAX);
    let governor_at = super_body.find("\"governor\"").unwrap_or(usize::MAX);
    if !(base_url_at < dashboard_at
        && dashboard_at < default_locale_at
        && default_locale_at < governor_at)
    {
        failures.push(format!(
            "locale-config: server wire order must be base_url, dashboard, default_locale, governor: {super_body}"
        ));
    }

    let (user_status, user_body) = locale_config_body(&proxy, &user).await;
    if user_status != 200 {
        failures.push(format!("locale-config: user status {user_status}"));
    }
    let user_json: serde_json::Value =
        serde_json::from_str(&user_body).unwrap_or(serde_json::Value::Null);
    if user_json.get("locale") != Some(&serde_json::Value::Null) {
        failures.push(format!(
            "locale-config: absent user override must be null: {user_body}"
        ));
    }
    if user_json.get("server").is_some() {
        failures.push("locale-config: ordinary user received admin server section".into());
    }

    let bootstrap = client()
        .get(proxy.url("/api/locale-bootstrap"))
        .send()
        .await
        .unwrap();
    let bootstrap_status = bootstrap.status();
    let bootstrap_bytes = bootstrap.bytes().await.unwrap();
    if bootstrap_status != 200
        || bootstrap_bytes.as_ref()
            != br#"{"installed_locales":["en-US"],"server_default":"en-US"}"#
    {
        failures.push(format!(
            "locale-bootstrap: persisted default not reflected exactly: status={bootstrap_status} body={}",
            String::from_utf8_lossy(&bootstrap_bytes)
        ));
    }

    // Server-default authorization is checked before any mutation.
    for (label, cookie, expected_status, expected_code) in [
        (
            "anonymous-server-default",
            None,
            reqwest::StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "ordinary-user-server-default",
            Some(user.as_str()),
            reqwest::StatusCode::FORBIDDEN,
            "forbidden",
        ),
    ] {
        let before = locale_store_bytes(&proxy);
        let (status, content_type, body) = locale_post(
            &proxy,
            cookie,
            "/api/settings/locale",
            &serde_json::json!({"locale": "en-US"}),
        )
        .await;
        record_locale_response(
            &mut failures,
            label,
            status,
            content_type.as_deref(),
            &body,
            expected_status,
            Some(expected_code),
        );
        if locale_store_bytes(&proxy) != before {
            failures.push(format!("{label}: rejected mutation changed config.json"));
        }
        rejection_byte_checks += 1;
    }
    for (
        actor,
        cookie,
        expected_status,
        expected_code,
        expected_message,
    ) in [
        (
            "anonymous",
            None,
            reqwest::StatusCode::UNAUTHORIZED,
            "unauthorized",
            "authentication required (session cookie, or Authorization: Bearer <username>:<password>)",
        ),
        (
            "ordinary-user",
            Some(user.as_str()),
            reqwest::StatusCode::FORBIDDEN,
            "forbidden",
            "server settings require an admin",
        ),
    ] {
        for (shape, content_type, request_body) in [
            ("malformed-json", Some("application/json"), "{"),
            (
                "wrong-media-type",
                Some("text/plain"),
                r#"{"locale":"en-US"}"#,
            ),
        ] {
            let label = format!("{actor}-server-default-{shape}");
            let before = locale_store_bytes(&proxy);
            let (status, content_type, body) = locale_post_raw(
                &proxy,
                cookie,
                "/api/settings/locale",
                content_type,
                request_body,
            )
            .await;
            record_locale_response(
                &mut failures,
                &label,
                status,
                content_type.as_deref(),
                &body,
                expected_status,
                Some(expected_code),
            );
            if locale_response_message(&body).as_deref() != Some(expected_message) {
                failures.push(format!(
                    "{label}: message {:?}, expected {expected_message:?}",
                    locale_response_message(&body)
                ));
            }
            if locale_store_bytes(&proxy) != before {
                failures.push(format!("{label}: rejected body changed config.json"));
            }
            rejection_byte_checks += 1;
        }
    }
    let before = locale_store_bytes(&proxy);
    let (status, content_type, body) = locale_post(
        &proxy,
        None,
        "/api/settings/account",
        &serde_json::json!({"action": "locale", "locale": "en-US"}),
    )
    .await;
    record_locale_response(
        &mut failures,
        "anonymous-user-override",
        status,
        content_type.as_deref(),
        &body,
        reqwest::StatusCode::UNAUTHORIZED,
        Some("unauthorized"),
    );
    if locale_store_bytes(&proxy) != before {
        failures.push("anonymous-user-override: rejected mutation changed config.json".into());
    }
    rejection_byte_checks += 1;

    // Every boundary row goes through each real mutation endpoint. Valid but
    // uninstalled tags are distinguished from syntactically invalid tags.
    let syntax_rows = [
        ("empty", ""),
        ("whitespace", " "),
        ("one-letter-language", "e"),
        ("underscore", "en_US"),
        ("trailing-separator", "en-"),
        ("private-use", "x-private"),
        ("four-letter-language", "abcd"),
        ("three-letter-script", "zh-Abc"),
        ("five-letter-script", "zh-Abcde"),
        ("three-letter-alpha-region", "en-USA"),
        ("one-digit-region", "en-1"),
        ("two-digit-region", "en-12"),
        ("four-digit-region", "en-1234"),
        ("extension", "en-u-ca"),
        ("padded", " en-US "),
        ("non-ascii", "fr-ÉR"),
        ("variant", "sl-rozaj"),
        ("extra-subtag", "zh-Hans-CN-extra"),
    ];
    let uninstalled_rows = [
        ("two-letter-language", "eN", "en"),
        ("three-letter-language", "eNg", "eng"),
        ("script", "zH-hAnS", "zh-Hans"),
        ("alpha-region", "pT-bR", "pt-BR"),
        ("numeric-region", "eS-419", "es-419"),
        ("script-region", "zH-hAnS-cN", "zh-Hans-CN"),
        ("test-pseudolocale", "eN-xA", "en-XA"),
    ];
    for (path, cookie, surface) in [
        ("/api/settings/locale", superuser.as_str(), "server-default"),
        ("/api/settings/account", user.as_str(), "user-override"),
    ] {
        for (row, locale) in syntax_rows {
            let before = locale_store_bytes(&proxy);
            let request = if path.ends_with("/account") {
                serde_json::json!({"action": "locale", "locale": locale})
            } else {
                serde_json::json!({"locale": locale})
            };
            let (status, content_type, body) =
                locale_post(&proxy, Some(cookie), path, &request).await;
            let label = format!("{surface}-invalid-{row}");
            record_locale_response(
                &mut failures,
                &label,
                status,
                content_type.as_deref(),
                &body,
                reqwest::StatusCode::BAD_REQUEST,
                Some("invalid_locale"),
            );
            if locale_store_bytes(&proxy) != before {
                failures.push(format!("{label}: rejected mutation changed config.json"));
            }
            rejection_byte_checks += 1;
        }
        for (row, locale, canonical) in uninstalled_rows {
            let before = locale_store_bytes(&proxy);
            let request = if path.ends_with("/account") {
                serde_json::json!({"action": "locale", "locale": locale})
            } else {
                serde_json::json!({"locale": locale})
            };
            let (status, content_type, body) =
                locale_post(&proxy, Some(cookie), path, &request).await;
            let label = format!("{surface}-uninstalled-{row}");
            record_locale_response(
                &mut failures,
                &label,
                status,
                content_type.as_deref(),
                &body,
                reqwest::StatusCode::BAD_REQUEST,
                Some("locale_not_installed"),
            );
            let expected_message = format!("locale {canonical} is not installed");
            if locale_response_message(&body).as_deref() != Some(expected_message.as_str()) {
                failures.push(format!(
                    "{label}: canonical locale message {:?}, expected {expected_message:?}",
                    locale_response_message(&body)
                ));
            }
            if locale_store_bytes(&proxy) != before {
                failures.push(format!("{label}: rejected mutation changed config.json"));
            }
            rejection_byte_checks += 1;
        }
    }

    let before = locale_store_bytes(&proxy);
    let (status, content_type, body) = locale_post(
        &proxy,
        Some(&user),
        "/api/settings/account",
        &serde_json::json!({"action": "unknown", "locale": "en-US"}),
    )
    .await;
    record_locale_response(
        &mut failures,
        "user-override-invalid-action",
        status,
        content_type.as_deref(),
        &body,
        reqwest::StatusCode::BAD_REQUEST,
        Some("invalid_action"),
    );
    if locale_store_bytes(&proxy) != before {
        failures.push("user-override-invalid-action: changed config.json".into());
    }
    rejection_byte_checks += 1;

    // Both existing admin roles may set the default. Mixed case is
    // canonicalized before the durable write. The fixture began as a
    // pre-locale v1 document, so compare the complete store and prove that
    // each role reaches commit even when the canonical value is idempotent.
    let before_server_default = locale_store_bytes(&proxy);
    let mut expected_server_default: serde_json::Value =
        serde_json::from_slice(&before_server_default).unwrap();
    expected_server_default
        .as_object_mut()
        .expect("locale-preferences: config object")
        .insert("default_locale".into(), serde_json::json!("en-US"));
    for (label, cookie, locale) in [
        ("admin-server-default", admin.as_str(), "EN-us"),
        ("superuser-server-default", superuser.as_str(), "en-US"),
    ] {
        let revision_before = locale_config_revision(&proxy, cookie).await;
        let (status, content_type, body) = locale_post(
            &proxy,
            Some(cookie),
            "/api/settings/locale",
            &serde_json::json!({"locale": locale}),
        )
        .await;
        record_locale_response(
            &mut failures,
            label,
            status,
            content_type.as_deref(),
            &body,
            reqwest::StatusCode::OK,
            None,
        );
        let revision_after = locale_config_revision(&proxy, cookie).await;
        if revision_after <= revision_before {
            failures.push(format!(
                "{label}: config revision did not advance across commit: before={revision_before} after={revision_after}"
            ));
        }
        let stored: serde_json::Value =
            serde_json::from_slice(&locale_store_bytes(&proxy)).unwrap();
        if stored != expected_server_default {
            failures.push(format!(
                "{label}: complete store changed outside canonical default_locale; before={} after={stored} expected={expected_server_default}",
                String::from_utf8_lossy(&before_server_default)
            ));
        }
    }

    // Every authenticated role may set and clear only its own preference,
    // without supplying a password. Compare the complete persisted document,
    // including the full users array, after each caller-scoped mutation.
    for (actor, username, cookie, is_admin) in [
        ("ordinary-user", "locale-user", user.as_str(), false),
        ("admin", "locale-admin", admin.as_str(), true),
        ("superuser", "root", superuser.as_str(), true),
    ] {
        let set_label = format!("{actor}-override-set");
        let before_set = locale_store_bytes(&proxy);
        let (status, content_type, body) = locale_post(
            &proxy,
            Some(cookie),
            "/api/settings/account",
            &serde_json::json!({"action": "locale", "locale": "EN-us"}),
        )
        .await;
        record_locale_response(
            &mut failures,
            &set_label,
            status,
            content_type.as_deref(),
            &body,
            reqwest::StatusCode::OK,
            None,
        );
        let after_set = locale_store_bytes(&proxy);
        record_only_user_locale_changed(
            &mut failures,
            &set_label,
            &before_set,
            &after_set,
            username,
            Some("en-US"),
        );

        let (configured_status, configured_body) = locale_config_body(&proxy, cookie).await;
        let configured: serde_json::Value =
            serde_json::from_str(&configured_body).unwrap_or_default();
        if configured_status != 200 || configured["locale"] != "en-US" {
            failures.push(format!(
                "{set_label}: caller /api/config did not expose canonical en-US: status={configured_status} body={configured_body}"
            ));
        }
        if is_admin && configured["server"]["default_locale"] != "en-US" {
            failures.push(format!(
                "{set_label}: admin /api/config must expose server.default_locale plus its own locale: {configured_body}"
            ));
        }

        let clear_label = format!("{actor}-override-clear");
        let before_clear = locale_store_bytes(&proxy);
        let (status, content_type, body) = locale_post(
            &proxy,
            Some(cookie),
            "/api/settings/account",
            &serde_json::json!({"action": "locale", "locale": null}),
        )
        .await;
        record_locale_response(
            &mut failures,
            &clear_label,
            status,
            content_type.as_deref(),
            &body,
            reqwest::StatusCode::OK,
            None,
        );
        let after_clear = locale_store_bytes(&proxy);
        record_only_user_locale_changed(
            &mut failures,
            &clear_label,
            &before_clear,
            &after_clear,
            username,
            None,
        );

        let (configured_status, configured_body) = locale_config_body(&proxy, cookie).await;
        let configured: serde_json::Value =
            serde_json::from_str(&configured_body).unwrap_or_default();
        if configured_status != 200 || configured.get("locale") != Some(&serde_json::Value::Null) {
            failures.push(format!(
                "{clear_label}: caller /api/config locale is not null: status={configured_status} body={configured_body}"
            ));
        }
        if is_admin && configured["server"]["default_locale"] != "en-US" {
            failures.push(format!(
                "{clear_label}: admin /api/config lost server.default_locale: {configured_body}"
            ));
        }
    }

    if rejection_byte_checks != 58 {
        failures.push(format!(
            "locale-preferences: executed {rejection_byte_checks} rejection byte checks, expected 58"
        ));
    }
    assert!(
        failures.is_empty(),
        "locale-preferences failures ({}) after {rejection_byte_checks} rejection byte checks:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[tokio::test]
async fn account_rejects_duplicate_known_fields_without_mutating_the_store() {
    let mock = start_mock().await;
    let cases = [
        (
            "duplicate-current-password",
            r#"{"current_password":"wrong-password","current_password":"test-password-1","new_password":"replacement-password"}"#,
        ),
        (
            "duplicate-new-password",
            r#"{"current_password":"test-password-1","new_password":"replacement-password-a","new_password":"replacement-password-b"}"#,
        ),
        (
            "duplicate-action",
            r#"{"action":"unknown","action":"locale","locale":"en-US"}"#,
        ),
        (
            "duplicate-locale",
            r#"{"action":"locale","locale":null,"locale":"en-US"}"#,
        ),
    ];
    let mut failures = Vec::new();

    for (label, body) in cases {
        let proxy = start_proxy(&mock.url, &[]).await;
        let cookie = login(&proxy).await;
        let before = locale_store_bytes(&proxy);
        let (status, content_type, response) = locale_post_raw(
            &proxy,
            Some(&cookie),
            "/api/settings/account",
            Some("application/json"),
            body,
        )
        .await;
        record_locale_response(
            &mut failures,
            label,
            status,
            content_type.as_deref(),
            &response,
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            Some("invalid_json"),
        );
        if locale_response_message(&response).as_deref() != Some("invalid JSON") {
            failures.push(format!(
                "{label}: message {:?}, expected \"invalid JSON\"",
                locale_response_message(&response)
            ));
        }
        if locale_store_bytes(&proxy) != before {
            failures.push(format!("{label}: rejected request changed config.json"));
        }
    }

    assert!(
        failures.is_empty(),
        "account duplicate-field failures:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn account_password_change_still_ignores_unknown_fields() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let cookie = login(&proxy).await;
    let before: serde_json::Value = serde_json::from_slice(&locale_store_bytes(&proxy)).unwrap();
    let (status, content_type, response) = locale_post_raw(
        &proxy,
        Some(&cookie),
        "/api/settings/account",
        Some("application/json"),
        r#"{"current_password":"test-password-1","new_password":"replacement-password","legacy_extension":{"nested":true}}"#,
    )
    .await;
    let mut failures = Vec::new();
    record_locale_response(
        &mut failures,
        "password-unknown-field",
        status,
        content_type.as_deref(),
        &response,
        reqwest::StatusCode::OK,
        None,
    );
    let after: serde_json::Value = serde_json::from_slice(&locale_store_bytes(&proxy)).unwrap();
    if after["users"][0]["password_hash"] == before["users"][0]["password_hash"] {
        failures.push("password-unknown-field: password hash did not change".into());
    }
    assert!(
        failures.is_empty(),
        "password unknown-field compatibility failures:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn api_config_is_filtered_by_role_before_serialization() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        extra_users: vec![("alice".into(), "user".into())],
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;

    // Admin view: server settings, users, and every key row (owner-labeled).
    let root = support::login(&proxy).await;
    let admin_view = api_config(&proxy, &root).await;
    assert_eq!(admin_view["role"], "superuser");
    assert!(admin_view["server"].is_object(), "{admin_view}");
    assert_eq!(admin_view["users"].as_array().unwrap().len(), 2);
    assert_eq!(admin_view["nim_keys"].as_array().unwrap().len(), 3);

    // User view: the raw JSON body simply has no server/users sections and
    // no foreign key rows — CSS tampering can reveal nothing.
    let alice = support::login_as(&proxy, "alice").await;
    let user_view = api_config(&proxy, &alice).await;
    assert_eq!(user_view["role"], "user");
    assert!(user_view.get("server").is_none(), "{user_view}");
    assert!(user_view.get("users").is_none(), "{user_view}");
    assert_eq!(
        user_view["nim_keys"].as_array().unwrap().len(),
        0,
        "alice owns no keys and must not see root's: {user_view}"
    );
    // The pool aggregate stays visible to everyone.
    assert_eq!(user_view["pool"]["enabled"], 3);
}

#[tokio::test]
async fn user_role_is_denied_server_settings_and_foreign_keys() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        extra_users: vec![("alice".into(), "user".into())],
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let root = support::login(&proxy).await;
    let alice = support::login_as(&proxy, "alice").await;

    for (path, body) in [
        (
            "/api/settings/upstream",
            serde_json::json!({"base_url": "http://x"}),
        ),
        (
            "/api/settings/history",
            serde_json::json!({
                "days": 30,
                "default_window_days": 30,
                "slo_target_percent": 99.9
            }),
        ),
        (
            "/api/settings/users",
            serde_json::json!({"add": {"username": "eve", "password": "long-enough-pw", "role": "user"}}),
        ),
        ("/api/settings/clients", serde_json::json!({"mode": "open"})),
        (
            "/api/settings/limits",
            serde_json::json!({
                "max_wait_secs": 60, "heartbeat_secs": 5, "models_ttl_secs": 600,
                "stream_idle_secs": 300, "request_timeout_secs": 300,
                "max_inflight": 512, "strict_passthrough": false
            }),
        ),
        (
            "/api/settings/governor",
            serde_json::json!({"enabled": false}),
        ),
    ] {
        let (status, v) = post_json(&proxy, &alice, path, body).await;
        assert_eq!(status, 403, "{path} should be admin-only: {v}");
    }

    // Removing / disabling someone else's NIM key is also forbidden.
    let fp = api_config(&proxy, &root).await["nim_keys"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, v) = post_json(
        &proxy,
        &alice,
        "/api/settings/nim-keys",
        serde_json::json!({"remove": fp}),
    )
    .await;
    assert_eq!(status, 403, "{v}");
    let (status, _) = post_json(
        &proxy,
        &alice,
        "/api/settings/nim-keys",
        serde_json::json!({"set": {"fingerprint": fp, "enabled": false}}),
    )
    .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn superuser_is_undeletable_and_the_pool_floor_holds() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        nim_keys: vec![("only-key".into(), 40)],
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let root = support::login(&proxy).await;

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/users",
        serde_json::json!({"remove": support::TEST_USER}),
    )
    .await;
    assert_eq!(status, 403, "superuser must be undeletable: {v}");

    // The superuser's last enabled key is the pool floor: neither removable
    // nor disableable, and the config marks it guarded for the padlock UI.
    let cfg = api_config(&proxy, &root).await;
    assert_eq!(cfg["nim_keys"][0]["guarded"], true, "{cfg}");
    let fp = cfg["nim_keys"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/nim-keys",
        serde_json::json!({"remove": &fp}),
    )
    .await;
    assert_eq!(status, 400, "{v}");
    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/nim-keys",
        serde_json::json!({"set": {"fingerprint": &fp, "enabled": false}}),
    )
    .await;
    assert_eq!(status, 400, "{v}");
}

#[tokio::test]
async fn deleting_a_user_pulls_their_keys_and_kills_their_session() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        extra_users: vec![("alice".into(), "user".into())],
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let root = support::login(&proxy).await;
    let alice = support::login_as(&proxy, "alice").await;

    // Any role may contribute a key to the shared pool.
    let (status, v) = post_json(
        &proxy,
        &alice,
        "/api/settings/nim-keys",
        serde_json::json!({"add": {"key": "alice-key", "rpm": 10}}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(api_config(&proxy, &root).await["pool"]["enabled"], 4);

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/users",
        serde_json::json!({"remove": "alice"}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let cfg = api_config(&proxy, &root).await;
    assert_eq!(
        cfg["pool"]["enabled"], 3,
        "alice's key left the pool: {cfg}"
    );
    assert_eq!(cfg["users"].as_array().unwrap().len(), 1);

    // Her session dies on the next lookup.
    let resp = client()
        .get(proxy.url("/api/config"))
        .header("cookie", &alice)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn client_key_lifecycle_mints_once_and_revokes() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        open: false, // keyed, no keys yet: /v1 rejects everyone
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let root = support::login(&proxy).await;

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/clients",
        serde_json::json!({"add": {"name": "opencode"}}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let secret = v["secret"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("npk_"), "{secret}");

    // The minted secret works on /v1; the stored config never returns it.
    let ok = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth(&secret)
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let cfg = api_config(&proxy, &root).await;
    assert_eq!(cfg["client_keys"][0]["name"], "opencode");
    assert!(
        !serde_json::to_string(&cfg).unwrap().contains(&secret),
        "secret must never be served back"
    );

    // Revoke: the same bearer stops working on the next request.
    let (status, _) = post_json(
        &proxy,
        &root,
        "/api/settings/clients",
        serde_json::json!({"remove": "opencode"}),
    )
    .await;
    assert_eq!(status, 200);
    let denied = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth(&secret)
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401);

    // Flipping to open mode admits keyless clients again (admin-only).
    let (status, _) = post_json(
        &proxy,
        &root,
        "/api/settings/clients",
        serde_json::json!({"mode": "open"}),
    )
    .await;
    assert_eq!(status, 200);
    let open = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(open.status(), 200);
}

#[tokio::test]
async fn rpm_raise_applies_to_the_live_pool_immediately() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        nim_keys: vec![("solo".into(), 1)],
        max_wait_secs: 2,
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let root = support::login(&proxy).await;

    let first = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    let second = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 504, "rpm 1 is spent for the window");

    // Raising the key's rpm rebuilds the pool with carried state — the new
    // headroom serves requests immediately, no restart, no window reset.
    let fp = api_config(&proxy, &root).await["nim_keys"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/nim-keys",
        serde_json::json!({"set": {"fingerprint": fp, "rpm": 5}}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let third = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(third.status(), 200, "raised rpm applies live");
    assert_eq!(mock.state.hit_count(), 2);
}

#[tokio::test]
async fn password_change_requires_current_and_rotates_other_sessions() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let session_a = support::login(&proxy).await;
    let session_b = support::login(&proxy).await;

    let (status, v) = post_json(
        &proxy,
        &session_a,
        "/api/settings/account",
        serde_json::json!({"current_password": "wrong", "new_password": "a-brand-new-pw"}),
    )
    .await;
    assert_eq!(
        status, 403,
        "re-auth is required regardless of session: {v}"
    );

    let resp = client()
        .post(proxy.url("/api/settings/account"))
        .header("cookie", &session_a)
        .json(&serde_json::json!({
            "current_password": support::TEST_PASSWORD,
            "new_password": "a-brand-new-pw",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // The change response re-mints THIS session; every other one dies.
    let fresh = resp.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let alive = client()
        .get(proxy.url("/api/config"))
        .header("cookie", &fresh)
        .send()
        .await
        .unwrap();
    assert_eq!(alive.status(), 200);
    let dead = client()
        .get(proxy.url("/api/config"))
        .header("cookie", &session_b)
        .send()
        .await
        .unwrap();
    assert_eq!(
        dead.status(),
        401,
        "old sessions bind the old password hash"
    );
}

#[tokio::test]
async fn base_url_change_flushes_the_models_cache() {
    let mock_a = start_mock().await;
    let mock_b = start_mock().await;
    let proxy = start_proxy(&mock_a.url, &[]).await;
    let root = support::login(&proxy).await;

    // Prime the (10-minute-TTL) catalog cache from upstream A.
    client()
        .get(proxy.url("/v1/models"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        mock_a
            .state
            .models_hits
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/upstream",
        serde_json::json!({"base_url": mock_b.url}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    client()
        .get(proxy.url("/v1/models"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        mock_b
            .state
            .models_hits
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "catalog refetches from the new upstream, not the stale cache"
    );
}

#[tokio::test]
async fn admin_cannot_reset_or_takeover_the_superuser() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        extra_users: vec![("adm".into(), "admin".into())],
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let adm = support::login_as(&proxy, "adm").await;

    // An admin resetting the superuser's password would be account takeover
    // (the change kills the real superuser's sessions). Must be refused.
    let (status, v) = post_json(
        &proxy,
        &adm,
        "/api/settings/users",
        serde_json::json!({"reset_password": {"username": support::TEST_USER, "new_password": "attacker-chosen-pw"}}),
    )
    .await;
    assert_eq!(
        status, 403,
        "admin must not reset the superuser's password: {v}"
    );

    // The superuser can still log in with the original password afterwards.
    let su = support::login(&proxy).await;
    assert!(!su.is_empty());

    // A normal reset of a peer admin still works.
    let (status, v) = post_json(
        &proxy,
        &su,
        "/api/settings/users",
        serde_json::json!({"reset_password": {"username": "adm", "new_password": "brand-new-admin-pw"}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "resetting a non-superuser must still work: {v}"
    );
}

#[tokio::test]
async fn authenticated_key_validation_ignores_caller_supplied_base_url() {
    // The configured upstream is the mock; a caller-supplied base_url must be
    // ignored so the endpoint can't be turned into an SSRF probe.
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let root = support::login(&proxy).await;

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/validate-key",
        serde_json::json!({"key": "nvapi-x", "base_url": "http://169.254.169.254"}),
    )
    .await;
    assert_eq!(status, 200);
    // It probed the real (mock) upstream, which answers with model-a — not the
    // attacker's target (which would have errored "cannot reach upstream").
    assert_eq!(
        v["ok"], true,
        "validated against the configured upstream: {v}"
    );
    assert_eq!(v["models"], 1, "{v}");
}

#[tokio::test]
async fn setup_key_validation_rejects_link_local_base_url() {
    // Pre-auth setup probe must not be usable as an SSRF oracle against the
    // cloud metadata endpoint; loopback/LAN upstreams stay allowed.
    let mock = start_mock().await;
    let proxy = support::start_proxy_fresh().await;

    let bad = client()
        .post(proxy.url("/setup/validate-key"))
        .json(
            &serde_json::json!({"key": "x", "base_url": "http://169.254.169.254/latest/meta-data"}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400, "link-local base_url must be rejected");

    // A normal (loopback mock) upstream still validates fine.
    let ok = client()
        .post(proxy.url("/setup/validate-key"))
        .json(&serde_json::json!({"key": "x", "base_url": mock.url}))
        .send()
        .await
        .unwrap();
    let v: serde_json::Value = ok.json().await.unwrap();
    assert_eq!(v["ok"], true, "loopback upstream still probes: {v}");
}

/// Streaming requests hold their in-flight slot for the stream's whole
/// lifetime — `max_inflight` caps total concurrent work, not just the
/// buffered path (streaming is what agent harnesses actually send).
#[tokio::test]
async fn streaming_requests_count_against_the_inflight_cap() {
    let mock = start_mock().await;
    mock.state.push(Behavior::Hang);
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            max_inflight: 1,
            ..Default::default()
        },
        &[],
    )
    .await;

    // Occupy the only slot with a stream that never ends. Reading the first
    // body chunk proves the proxy has fully committed to the stream.
    let mut hog = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hog", true))
        .send()
        .await
        .unwrap()
        .bytes_stream();
    use futures_util::StreamExt;
    let first = tokio::time::timeout(Duration::from_secs(5), hog.next())
        .await
        .expect("first chunk within 5s")
        .expect("stream not ended")
        .expect("stream chunk");
    assert!(!first.is_empty());

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("shed-me", false))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        503,
        "a live stream occupies the in-flight cap"
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "overloaded");
    drop(hog);
}

/// The wizard can mint a first client key atomically with the claim, so a
/// fresh keyed-mode proxy serves /v1 immediately — no Settings detour. The
/// secret is returned exactly once and never stored in plaintext.
#[tokio::test]
async fn setup_can_mint_a_first_client_key() {
    let mock = start_mock().await;
    let proxy = start_proxy_fresh().await;

    let resp = client()
        .post(proxy.url("/setup"))
        .json(&serde_json::json!({
            "username": "admin",
            "password": "hunter2hunter2",
            "base_url": mock.url,
            "nim_keys": [{"key": "nvapi-key", "rpm": 40}],
            "create_client_key": {"name": "default"},
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    let secret = v["client_key"]["secret"].as_str().expect("minted secret");
    assert!(secret.starts_with("npk_"), "{v}");
    assert_eq!(v["client_key"]["name"], "default");

    // The store holds only the digest, never the bearer token itself.
    let store = std::fs::read_to_string(proxy.data_dir.join("config.json")).unwrap();
    assert!(
        !store.contains(secret),
        "client secret must not be persisted in plaintext"
    );

    // The minted key opens /v1 right away; keyless calls still fail closed.
    let ok = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth(secret)
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200, "minted key serves /v1 with no detour");
    let no_key = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(no_key.status(), 401, "keyed mode still fails closed");
}

// ---------- settings-endpoint coverage backfill ----------

/// A DATA_DIR whose path is blocked by a regular file is a hard boot error.
/// (The write-probe posture; a chmod-based fixture would pass vacuously when
/// the tests run as root.)
#[tokio::test]
async fn boot_refuses_an_unwritable_data_dir() {
    let dir = scratch_data_dir();
    std::fs::write(dir.join("blocker"), b"not a directory").unwrap();
    expect_refuses_to_start(dir.join("blocker").join("data")).await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Governor settings write through the shared pipeline: they reflect in
/// /api/config, out-of-range overrides are refused, and the state persists
/// across a restart.
#[tokio::test]
async fn governor_settings_reflect_and_persist() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let root = support::login(&proxy).await;

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/governor",
        serde_json::json!({"set_override": {"model": "mock/model-a", "cap": 4}}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let cfg = api_config(&proxy, &root).await;
    assert_eq!(cfg["server"]["governor"]["overrides"]["mock/model-a"], 4);

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/governor",
        serde_json::json!({"set_override": {"model": "mock/model-a", "cap": 0}}),
    )
    .await;
    assert_eq!(status, 400, "cap 0 must fail the rulebook: {v}");

    let (status, _) = post_json(
        &proxy,
        &root,
        "/api/settings/governor",
        serde_json::json!({"enabled": false}),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = post_json(
        &proxy,
        &root,
        "/api/settings/governor",
        serde_json::json!({"remove_override": "mock/model-a"}),
    )
    .await;
    assert_eq!(status, 200);

    let proxy = restart(proxy, &[]).await;
    let root = support::login(&proxy).await;
    let cfg = api_config(&proxy, &root).await;
    assert_eq!(
        cfg["server"]["governor"]["enabled"], false,
        "master toggle persisted across restart: {cfg}"
    );
    assert!(
        cfg["server"]["governor"]["overrides"]
            .as_object()
            .unwrap()
            .is_empty(),
        "removed override stays gone: {cfg}"
    );
}

/// Dashboard history settings save through the shared pipeline and reflect in
/// /api/config; invalid candidates leave every value unchanged.
#[tokio::test]
async fn history_settings_reflect_in_api_config() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let root = support::login(&proxy).await;

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/history",
        serde_json::json!({
            "days": 45,
            "default_window_days": 30,
            "slo_target_percent": 99.5
        }),
    )
    .await;
    assert_eq!(status, 200, "{v}");

    let cfg = api_config(&proxy, &root).await;
    assert_eq!(cfg["server"]["history"]["days"], 45);
    assert_eq!(cfg["server"]["dashboard"]["default_window_days"], 30);
    assert_eq!(cfg["server"]["dashboard"]["slo_target_percent"], 99.5);

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/history",
        serde_json::json!({
            "days": 7,
            "default_window_days": 30,
            "slo_target_percent": 98.0
        }),
    )
    .await;
    assert_eq!(status, 400, "invalid window/retention pair accepted: {v}");
    let cfg = api_config(&proxy, &root).await;
    assert_eq!(cfg["server"]["history"]["days"], 45);
    assert_eq!(cfg["server"]["dashboard"]["default_window_days"], 30);
    assert_eq!(cfg["server"]["dashboard"]["slo_target_percent"], 99.5);
}

/// The limits endpoint enforces the shared rulebook (heartbeat < max_wait)
/// and rejects partial bodies outright — omitted fields are never silently
/// reset to defaults.
#[tokio::test]
async fn limits_validation_rejects_bad_bounds_and_partial_bodies() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let root = support::login(&proxy).await;

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/limits",
        serde_json::json!({
            "max_wait_secs": 5, "heartbeat_secs": 10, "models_ttl_secs": 600,
            "stream_idle_secs": 300, "request_timeout_secs": 300,
            "max_inflight": 512, "strict_passthrough": false
        }),
    )
    .await;
    assert_eq!(status, 400, "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("max_wait_secs"),
        "the rulebook names the offending bound: {v}"
    );

    let partial = client()
        .post(proxy.url("/api/settings/limits"))
        .header("cookie", &root)
        .json(&serde_json::json!({"max_wait_secs": 60}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        partial.status(),
        422,
        "a partial limits body is rejected, not defaulted"
    );
}

/// The account endpoint enforces the same 10-character password floor the
/// wizard and user-management do.
#[tokio::test]
async fn account_rejects_a_short_new_password() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let root = support::login(&proxy).await;

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/account",
        serde_json::json!({"current_password": support::TEST_PASSWORD, "new_password": "short"}),
    )
    .await;
    assert_eq!(status, 400, "{v}");
    assert_eq!(v["error"]["code"], "weak_password");
}

/// A stream whose upstream hangs must release its in-flight slot promptly
/// once the client disconnects — otherwise hung upstreams accumulate and
/// permanently consume the cap (503s forever until restart).
#[tokio::test]
async fn disconnected_stream_releases_its_inflight_slot() {
    let mock = start_mock().await;
    mock.state.push(Behavior::Hang);
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            max_inflight: 1,
            ..Default::default()
        },
        &[],
    )
    .await;

    // Occupy the only slot with a hung stream. Read PAST the upstream's only
    // data chunk so the relay task is parked on the upstream read with
    // nothing left to send — the state where a disconnect used to go
    // unnoticed until the stream_idle cutoff — then hang up.
    let mut hog = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hog", true))
        .send()
        .await
        .unwrap()
        .bytes_stream();
    use futures_util::StreamExt;
    let read_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let chunk = tokio::time::timeout(
            read_deadline.saturating_duration_since(Instant::now()),
            hog.next(),
        )
        .await
        .expect("upstream chunk within 5s")
        .expect("stream open")
        .expect("chunk ok");
        if String::from_utf8_lossy(&chunk).contains("choices") {
            break; // the mock's single pre-hang data chunk has been relayed
        }
    }
    drop(hog);

    // The slot must come back well before the stream_idle cutoff (300s here):
    // the proxy notices the closed client channel, not just the stalled read.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let resp = client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body("after-disconnect", false))
            .send()
            .await
            .unwrap();
        if resp.status() == 200 {
            break;
        }
        assert_eq!(resp.status(), 503, "only sheds while the slot is held");
        assert!(
            Instant::now() < deadline,
            "slot never released after client disconnect"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

// ===========================================================================
// Coverage wave 2: setup edge cases, settings error/ownership legs, and the
// auth handler surface (Basic scrape creds, login redirects, logout). These
// drive previously-uncovered branches through the real HTTP surface.
// ===========================================================================

/// Two wizard claims race; the store mutex admits exactly one.
#[tokio::test]
async fn setup_double_claim_is_rejected_with_409() {
    let proxy = start_proxy_fresh().await;
    let body = serde_json::json!({
        "username": "admin",
        "password": "hunter2hunter2",
        "base_url": "http://127.0.0.1:9999",
        "nim_keys": [{"key": "nvapi-x", "rpm": 40}],
    });
    // Both requests pass the setup_required check before either finishes the
    // 600k-iteration PBKDF2 hash, so the mutex arbitrates one winner.
    let (a, b) = tokio::join!(
        client().post(proxy.url("/setup")).json(&body).send(),
        client().post(proxy.url("/setup")).json(&body).send(),
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    let (success, conflict) = if a.status() == reqwest::StatusCode::OK {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(success.status(), reqwest::StatusCode::OK);
    assert_exact_api_error(
        conflict,
        reqwest::StatusCode::CONFLICT,
        "setup_complete",
        "setup is already complete",
    )
    .await;
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(proxy.data_dir.join("config.json")).unwrap())
            .unwrap();
    assert_eq!(stored["users"].as_array().unwrap().len(), 1);
    assert_eq!(stored["users"][0]["username"], "admin");
}

/// A lockout-recovery store (users hand-emptied) keeps orphan-owned client
/// keys; claiming the proxy re-owns them to the new superuser.
#[tokio::test]
async fn setup_adopts_orphan_client_keys_on_claim() {
    let mock = start_mock().await;
    let dir = scratch_data_dir();
    let fixture = serde_json::json!({
        "version": 1,
        "upstream": {
            "base_url": mock.url,
            "nim_keys": [{"key": "orphan-key", "owner": "ghost", "enabled": true, "rpm": 40}],
        },
        "client_auth": {
            "mode": "keyed",
            "keys": [{
                "name": "orphan-client",
                "secret_sha256": support::sha256_hex("orphan-secret"),
                "owner": "ghost",
            }],
        },
        "users": [],
    });
    std::fs::write(
        dir.join("config.json"),
        serde_json::to_string_pretty(&fixture).unwrap(),
    )
    .unwrap();

    let proxy = start_proxy_in(dir, &[]).await;
    let root = complete_setup(&proxy, "admin", support::TEST_PASSWORD, &mock.url, &[]).await;

    // The orphan client key is re-owned by the new superuser...
    let cfg = api_config(&proxy, &root).await;
    assert_eq!(cfg["client_keys"][0]["owner"], "admin", "{cfg}");
    // ...and its secret still authenticates on keyed /v1.
    let r = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth("orphan-secret")
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "adopted client key authenticates");
}

/// The pre-auth key probe shares the login throttle; hammering it trips 429.
#[tokio::test]
async fn setup_validate_key_throttles_after_repeated_probes() {
    let proxy = start_proxy_fresh().await;
    // A dead loopback fails fast (no real egress) but still burns throttle
    // budget on each probe.
    let body = serde_json::json!({"key": "x", "base_url": "http://127.0.0.1:1"});
    let mut last = 0u16;
    for _ in 0..12 {
        last = client()
            .post(proxy.url("/setup/validate-key"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
    }
    assert_eq!(last, 429, "the throttle trips after repeated failed probes");
}

/// A reachable upstream that 404s the models route is a key rejection, not a
/// connection failure (probe_key's non-success branch).
#[tokio::test]
async fn key_probe_reports_upstream_rejection() {
    let mock = start_mock().await;
    let proxy = start_proxy_fresh().await;
    let resp = client()
        .post(proxy.url("/setup/validate-key"))
        // A bogus path prefix 404s on the mock's own router.
        .json(&serde_json::json!({"key": "x", "base_url": format!("{}/bogus", mock.url)}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], false, "{v}");
    assert!(v["error"].as_str().unwrap().contains("rejected"), "{v}");
}

/// The authenticated key validator reports an unreachable upstream (probe_key's
/// connect-error branch, via /api/settings/validate-key).
#[tokio::test]
async fn authenticated_key_validation_reports_unreachable_upstream() {
    let proxy = start_proxy_with("http://127.0.0.1:1", support::StoreOpts::default(), &[]).await;
    let root = support::login(&proxy).await;
    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/validate-key",
        serde_json::json!({"key": "x"}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["ok"], false, "{v}");
    assert!(v["error"].as_str().unwrap().contains("reach"), "{v}");
}

/// Removing or reconfiguring a NIM key that doesn't exist is a 400, and the
/// action selector requires exactly one of add/remove/set.
#[tokio::test]
async fn nim_keys_reject_unknown_fingerprint_and_empty_action() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(&mock.url, support::StoreOpts::default(), &[]).await;
    let root = support::login(&proxy).await;
    for body in [
        serde_json::json!({"remove": "deadbeef"}),
        serde_json::json!({"set": {"fingerprint": "deadbeef", "enabled": true}}),
    ] {
        let (status, v) = post_json(&proxy, &root, "/api/settings/nim-keys", body).await;
        assert_eq!(status, 400, "{v}");
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no such key"),
            "{v}"
        );
    }
    let (status, _) = post_json(
        &proxy,
        &root,
        "/api/settings/nim-keys",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 400, "empty action rejected");
}

/// Client-key endpoint: unknown name, bad mode, empty oneof, empty name on
/// commit, and cross-owner revoke are all rejected with the right status.
#[tokio::test]
async fn clients_reject_unknown_bad_input_and_cross_owner_revoke() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        extra_users: vec![("alice".into(), "user".into())],
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let root = support::login(&proxy).await;
    let alice = support::login_as(&proxy, "alice").await;

    let (s, v) = post_json(
        &proxy,
        &root,
        "/api/settings/clients",
        serde_json::json!({"remove": "nope"}),
    )
    .await;
    assert_eq!(s, 400, "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no such client key"),
        "{v}"
    );

    let (s, _) = post_json(
        &proxy,
        &root,
        "/api/settings/clients",
        serde_json::json!({"mode": "bogus"}),
    )
    .await;
    assert_eq!(s, 400, "bad mode rejected");
    let (s, _) = post_json(
        &proxy,
        &root,
        "/api/settings/clients",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, 400, "empty action rejected");
    let (s, v) = post_json(
        &proxy,
        &root,
        "/api/settings/clients",
        serde_json::json!({"add": {"name": ""}}),
    )
    .await;
    assert_eq!(s, 400, "empty name rejected on commit: {v}");

    // Root mints a key; alice may not revoke someone else's.
    let (s, _) = post_json(
        &proxy,
        &root,
        "/api/settings/clients",
        serde_json::json!({"add": {"name": "root-key"}}),
    )
    .await;
    assert_eq!(s, 200);
    let (s, v) = post_json(
        &proxy,
        &alice,
        "/api/settings/clients",
        serde_json::json!({"remove": "root-key"}),
    )
    .await;
    assert_eq!(s, 403, "{v}");
    assert!(
        v["error"]["message"].as_str().unwrap().contains("your own"),
        "{v}"
    );
}

/// The upstream base_url is re-validated on write: a link-local target (SSRF /
/// cloud-metadata) is refused.
#[tokio::test]
async fn upstream_rejects_link_local_base_url() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(&mock.url, support::StoreOpts::default(), &[]).await;
    let root = support::login(&proxy).await;
    let (s, v) = post_json(
        &proxy,
        &root,
        "/api/settings/upstream",
        serde_json::json!({"base_url": "http://169.254.169.254"}),
    )
    .await;
    assert_eq!(s, 400, "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("link-local"),
        "{v}"
    );
}

/// User management: weak-password rejection on add and reset, commit-error on a
/// blank username, the add+hashing happy path, reset of an unknown user, and
/// role changes (promote a user; the superuser's role is immutable).
#[tokio::test]
async fn users_add_reset_and_set_role_paths() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        extra_users: vec![
            ("adm".into(), "admin".into()),
            ("bob".into(), "user".into()),
        ],
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let root = support::login(&proxy).await;

    // Add: weak password rejected.
    let (s, v) = post_json(
        &proxy,
        &root,
        "/api/settings/users",
        serde_json::json!({"add": {"username": "eve", "password": "short", "role": "user"}}),
    )
    .await;
    assert_eq!(s, 400, "{v}");
    assert_eq!(v["error"]["code"], "weak_password", "{v}");
    // Add: a username that trims to empty fails on commit.
    let (s, _) = post_json(
        &proxy,
        &root,
        "/api/settings/users",
        serde_json::json!({"add": {"username": "   ", "password": "long-enough-pw", "role": "user"}}),
    )
    .await;
    assert_eq!(s, 400, "blank username rejected");
    // Add: valid -> the new user can log in (exercises the hashing path).
    // login_as always uses TEST_PASSWORD, so create eve with it.
    let (s, v) = post_json(
        &proxy,
        &root,
        "/api/settings/users",
        serde_json::json!({"add": {"username": "eve", "password": support::TEST_PASSWORD, "role": "user"}}),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let _ = support::login_as(&proxy, "eve").await;

    // Reset: weak password and unknown user both rejected.
    let (s, v) = post_json(
        &proxy,
        &root,
        "/api/settings/users",
        serde_json::json!({"reset_password": {"username": "adm", "new_password": "short"}}),
    )
    .await;
    assert_eq!(s, 400, "{v}");
    assert_eq!(v["error"]["code"], "weak_password", "{v}");
    let (s, v) = post_json(
        &proxy,
        &root,
        "/api/settings/users",
        serde_json::json!({"reset_password": {"username": "ghost", "new_password": "long-enough-pw"}}),
    )
    .await;
    assert_eq!(s, 400, "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no such user"),
        "{v}"
    );

    // set_role: promote bob to admin (verified functionally), then confirm the
    // superuser's role can't be changed.
    let (s, v) = post_json(
        &proxy,
        &root,
        "/api/settings/users",
        serde_json::json!({"set_role": {"username": "bob", "role": "admin"}}),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let bob = support::login_as(&proxy, "bob").await;
    let (s, _) = post_json(
        &proxy,
        &bob,
        "/api/settings/governor",
        serde_json::json!({"enabled": true}),
    )
    .await;
    assert_eq!(s, 200, "bob now has admin rights");
    let (s, v) = post_json(
        &proxy,
        &root,
        "/api/settings/users",
        serde_json::json!({"set_role": {"username": support::TEST_USER, "role": "user"}}),
    )
    .await;
    assert_eq!(s, 403, "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("immutable"),
        "{v}"
    );
}

/// User-management input validation: invalid role and unknown-target legs on
/// add / set_role / remove, plus the exactly-one-action rule.
#[tokio::test]
async fn users_reject_invalid_role_unknown_target_and_bad_action() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(&mock.url, support::StoreOpts::default(), &[]).await;
    let root = support::login(&proxy).await;
    for body in [
        serde_json::json!({"add": {"username": "x", "password": "long-enough-pw", "role": "wizard"}}),
        serde_json::json!({"remove": "ghost"}),
        serde_json::json!({"set_role": {"username": "x", "role": "wizard"}}),
        serde_json::json!({"set_role": {"username": "ghost", "role": "user"}}),
        serde_json::json!({}),
    ] {
        let (s, v) = post_json(&proxy, &root, "/api/settings/users", body).await;
        assert_eq!(s, 400, "{v}");
    }
}

/// Scraper header auth: HTTP Basic works (a second identical call also drives
/// the credential-memo fast path), while an unknown scheme, a wrong password,
/// and a foreign cookie all 401.
#[tokio::test]
async fn scraper_header_auth_variants() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    for _ in 0..2 {
        let r = client()
            .get(proxy.url("/api/config"))
            .basic_auth(support::TEST_USER, Some(support::TEST_PASSWORD))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "HTTP Basic scrape credential");
    }
    let r = client()
        .get(proxy.url("/api/config"))
        .header("authorization", "Digest x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "unknown auth scheme");
    let r = client()
        .get(proxy.url("/api/config"))
        .bearer_auth(format!("{}:wrong", support::TEST_USER))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "wrong password");
    let r = client()
        .get(proxy.url("/api/config"))
        .header("cookie", "foo=bar")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "a foreign cookie is ignored");
}

/// Pre-setup, a non-HTML request to the operator surface answers 503
/// setup_required rather than redirecting.
#[tokio::test]
async fn require_session_pre_setup_answers_setup_required_json() {
    let proxy = start_proxy_fresh().await;
    let r = client()
        .get(proxy.url("/api/config"))
        .header("accept", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "setup_required", "{v}");
}

/// GET /login redirects an already-authenticated user to the dashboard.
#[tokio::test]
async fn login_page_redirects_when_already_authenticated() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let root = support::login(&proxy).await;
    let r = no_redirect_client()
        .get(proxy.url("/login"))
        .header("cookie", &root)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 302);
    assert_eq!(r.headers()["location"], "/");
}

/// POST /login before setup bounces to the wizard.
#[tokio::test]
async fn login_pre_setup_redirects_to_wizard() {
    let proxy = start_proxy_fresh().await;
    let r = no_redirect_client()
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("username=x&password=yyyyyyyyyy")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 302);
    assert_eq!(r.headers()["location"], "/setup");
}

/// An empty login body (both form fields absent) falls to the burner-hash path
/// and still fails closed.
#[tokio::test]
async fn login_with_empty_body_fails_closed() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let r = no_redirect_client()
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

/// POST /logout clears the session cookie and redirects to the login page.
#[tokio::test]
async fn logout_clears_the_session_cookie() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let root = support::login(&proxy).await;
    let r = no_redirect_client()
        .post(proxy.url("/logout"))
        .header("cookie", &root)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 303);
    assert_eq!(r.headers()["location"], "/login");
    let set = r.headers()["set-cookie"].to_str().unwrap();
    assert!(set.contains("nimproxy_session="), "{set}");
    assert!(set.contains("Max-Age=0"), "{set}");
}

/// The wizard's strong-password gate passes, but a candidate that fails
/// `validate()` at commit surfaces as `invalid_config` (not a panic/500).
#[tokio::test]
async fn setup_rejects_an_invalid_config_on_commit() {
    let proxy = start_proxy_fresh().await;
    let resp = client()
        .post(proxy.url("/setup"))
        .json(&serde_json::json!({
            "username": "bad user!", // fails the username charset check in validate()
            "password": "hunter2hunter2",
            "base_url": "http://127.0.0.1:9999",
            "nim_keys": [{"key": "k", "rpm": 40}],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "invalid_config", "{v}");
}

/// An empty DATA_DIR is a fatal misconfiguration — the store home must be a
/// real writable directory.
#[tokio::test]
async fn boot_refuses_an_empty_data_dir() {
    support::expect_refuses_to_start(std::path::PathBuf::from("")).await;
}

/// `nim-proxy --health` probes /health on $PORT and exits 0 (healthy) or 1
/// (unreachable) — the scratch image's shell-less HEALTHCHECK.
#[tokio::test]
async fn health_probe_flag_reports_liveness() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let run_health = |port: String| {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_nim-proxy"));
        cmd.arg("--health").env("PORT", port);
        // Forward the coverage profile path so the probe subprocess is counted
        // under `cargo llvm-cov` (a no-op in a normal test run).
        if let Ok(v) = std::env::var("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", v);
        }
        cmd.status().unwrap()
    };
    assert!(
        run_health(proxy.port.to_string()).success(),
        "--health exits 0 against a healthy proxy"
    );
    assert!(
        !run_health("1".into()).success(),
        "--health exits non-zero against a dead port"
    );
}

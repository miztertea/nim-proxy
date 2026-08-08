mod api;
mod auth;
mod config;
mod dispatch;
mod governor;
mod history;
mod observation;
mod pool;
mod presentation;
mod proxy;
mod routes;
mod settings;

pub use api::openapi_json;

// Fuzzing-only re-exports (the modules themselves stay private). Compiled
// only under cargo-fuzz's `--cfg fuzzing`, so normal builds, coverage, and
// the shipped binary never carry them.
#[cfg(fuzzing)]
#[doc(hidden)]
pub use config::fuzz as fuzz_config;
#[cfg(fuzzing)]
#[doc(hidden)]
pub use proxy::fuzz as fuzz_proxy;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::Router;
use bytes::Bytes;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use tokio::sync::Mutex;

use auth::Admin;
use dispatch::Dispatcher;
use pool::{Pool, PoolHandle};

// Recorder configuration and Rust-owned browser fixtures share this one
// production bucket registry; fixture code must never approximate it.
#[rustfmt::skip]
const HISTOGRAM_BUCKETS: &[(&str, &[f64])] = &[
    ("nimproxy_ttft_seconds",       &[0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]),
    ("nimproxy_tokens_per_second",  &[1.0, 2.0, 5.0, 10.0, 20.0, 40.0, 80.0, 160.0, 320.0]),
    ("nimproxy_queue_wait_seconds", &[0.001, 0.05, 0.25, 1.0, 5.0, 15.0, 60.0, 180.0, 600.0]),
    ("nimproxy_upstream_seconds",   &[0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0]),
    ("nimproxy_tpot_seconds",       &[0.005, 0.01, 0.02, 0.04, 0.08, 0.16, 0.32]),
    ("nimproxy_request_messages",   &[1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0]),
    ("nimproxy_request_tools",      &[0.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0]),
    ("nimproxy_request_max_tokens", &[128.0, 256.0, 512.0, 1024.0, 2048.0, 4096.0, 8192.0, 16384.0, 32768.0, 65536.0, 131072.0]),
    ("nimproxy_request_temperature", &[0.0, 0.2, 0.4, 0.6, 0.8, 1.0, 1.2, 1.5, 2.0]),
];

/// App-level configuration, published as an immutable snapshot: every request
/// takes one `Arc<Config>` via [`AppState::cfg`] and sees a consistent view;
/// the settings layer swaps in a replacement under the write lock.
pub struct Config {
    pub base_url: String,
    pub max_wait: Duration,
    pub heartbeat: Duration,
    pub models_ttl: Duration,
    /// Abort a stream when the upstream sends nothing for this long (0 = off).
    pub stream_idle: Duration,
    /// Overall deadline for a non-streaming upstream request (connect + body).
    /// Streaming has no overall cap (generation can be long) — it relies on
    /// `stream_idle` instead. Bounds a stalled buffered read holding a slot.
    pub request_timeout: Duration,
    /// Never modify request bodies (disables stream_options usage injection).
    pub strict_passthrough: bool,
    /// token -> client name. None = local mode, no client auth.
    pub clients: Option<HashMap<String, String>>,
    /// Cap on concurrent requests; bounds memory under floods.
    pub max_inflight: usize,
    /// Model-pressure governor settings (worker concurrency, not RPM).
    pub governor: GovernorSettings,
}

pub struct GovernorSettings {
    /// Adaptive governing on worker-exhaustion errors (on by default; the
    /// governor stays dormant until an upstream actually exhausts).
    pub enabled: bool,
    /// Operator-pinned per-model concurrency caps (model id -> max in-flight).
    pub overrides: BTreeMap<String, usize>,
}

impl Default for GovernorSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            overrides: BTreeMap::new(),
        }
    }
}

pub struct AppState {
    /// Current config snapshot; read via [`AppState::cfg`], swapped whole.
    pub cfg: RwLock<Arc<Config>>,
    /// The persisted store of truth. Its mutex doubles as the save-mutex:
    /// settings writes hold it across build → validate → persist → swap.
    pub store: std::sync::Mutex<config::StoredConfig>,
    /// Where the store lives (DATA_DIR).
    pub data_dir: std::path::PathBuf,
    /// True until a superuser exists: the wizard is open, everything else
    /// is closed (dashboard redirects to /setup, /v1 answers 503).
    pub setup_required: std::sync::atomic::AtomicBool,
    /// Current key pool; the dispatcher reads it per grant, settings swap it.
    pub pool: PoolHandle,
    pub dispatch: Dispatcher,
    pub http: reqwest::Client,
    pub models_cache: Mutex<Option<(Instant, Bytes)>>,
    /// Models that rejected stream_options injection; never inject for them again.
    pub no_inject: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Distinct sanitized model labels seen (bounds metric cardinality).
    pub model_labels: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Session + throttle machinery for the operator surface.
    pub admin: Admin,
    /// Requests currently in flight; capped to bound memory under floods.
    pub inflight: AtomicUsize,
    /// Per-model worker-concurrency gate (runtime state, settings in Config).
    pub governor: Arc<governor::Governor>,
    pub history: Arc<history::History>,
    /// Shared metrics registry rendered by both `/metrics` and dashboard-now.
    pub prometheus: PrometheusHandle,
    /// Monotonic settings generation for lightweight dashboard refreshes.
    pub config_revision: AtomicU64,
    /// Unix time this process started (dashboard uptime).
    pub started: u64,
}

impl AppState {
    /// One consistent config snapshot; never hold this across a save.
    pub fn cfg(&self) -> Arc<Config> {
        self.cfg.read().unwrap().clone()
    }

    /// The current pool generation (observability only — reservations go
    /// through the dispatcher, which snapshots under the same lock).
    pub fn pool(&self) -> Arc<Pool> {
        self.pool.read().unwrap().clone()
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

/// App-level settings moved from env into the UI-managed store (v0.6.0).
/// Ignore — but call out — any that are still set, so a stale .env can't
/// silently mislead an operator.
fn warn_legacy_env() {
    const LEGACY: &[&str] = &[
        "NIM_API_KEYS",
        "NIM_BASE_URL",
        "RPM_PER_KEY",
        "PROXY_API_KEYS",
        "ADMIN_PASSWORD",
        "INSECURE_NO_AUTH",
        "MAX_WAIT_SECS",
        "HEARTBEAT_SECS",
        "MODELS_TTL_SECS",
        "STREAM_IDLE_SECS",
        "REQUEST_TIMEOUT_SECS",
        "STRICT_PASSTHROUGH",
        "REF_PRICE_IN",
        "REF_PRICE_OUT",
        "HISTORY_DAYS",
        "MAX_INFLIGHT",
    ];
    let set: Vec<&str> = LEGACY
        .iter()
        .copied()
        .filter(|v| std::env::var_os(v).is_some())
        .collect();
    if !set.is_empty() {
        tracing::warn!(
            "ignoring legacy env vars ({}) — these settings live in the dashboard now",
            set.join(", ")
        );
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn capacity_snapshot(pool: &Pool) -> history::CapacitySnapshot {
    history::CapacitySnapshot {
        enabled_lanes: pool.len(),
        rpms: pool.rpms(),
        capacity_rpm: pool.capacity_rpm(),
    }
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct DashboardQuery {
    /// Window start, unix seconds. Defaults to `to - default_window_days`.
    from: Option<u64>,
    /// Window end, unix seconds. Omitting it means "follow now".
    to: Option<u64>,
    /// Rollup buckets, clamped to 2..=1000. Defaults to 288.
    points: Option<usize>,
}

#[utoipa::path(
    get,
    path = "/api/dashboard",
    tag = "dashboard",
    params(DashboardQuery),
    responses(
        (status = 200, description = "Rolled-up history for the window", body = api::DashboardResponse),
        (status = 400, description = "`from` is not before `to`", body = api::ApiError),
        (status = 401, description = "No session", body = api::ApiError),
    ),
)]
async fn api_dashboard(
    State(state): State<Arc<AppState>>,
    api::ApiQuery(query): api::ApiQuery<DashboardQuery>,
) -> Response {
    let stored = state.store.lock().unwrap();
    let config_revision = state
        .config_revision
        .load(std::sync::atomic::Ordering::SeqCst);
    let now = unix_now();
    let following_now = query.to.is_none();
    let requested_from = query.from.unwrap_or_else(|| {
        now.saturating_sub(stored.dashboard.default_window_days.saturating_mul(86_400))
    });
    let requested_to = query.to.unwrap_or(now);
    if requested_from >= requested_to {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(api::ApiError::new(
                "invalid_time_window",
                "from must be less than to",
            )),
        )
            .into_response();
    }

    let rollup = state.history.rollup(
        requested_from,
        requested_to,
        query.points.unwrap_or(288).clamp(2, 1000),
    );
    axum::Json(api::DashboardResponse {
        config_revision,
        diagnostics: rollup.diagnostics,
        history_revision: rollup.data.history_revision,
        latest: rollup.data.latest,
        points: rollup.data.points,
        totals: rollup.data.totals,
        window: api::DashboardWindow {
            available_from: rollup.data.available_from,
            available_to: rollup.data.available_to,
            complete: rollup.complete,
            default_window_days: stored.dashboard.default_window_days,
            effective_from: rollup.data.effective_from,
            effective_to: rollup.data.effective_to,
            following_now,
            requested_from,
            requested_to,
            retention_days: stored.history.days,
        },
    })
    .into_response()
}

#[utoipa::path(
    get,
    path = "/api/dashboard/now",
    tag = "dashboard",
    responses(
        (status = 200, description = "Live registry values plus the configuration they were \
            sampled under", body = api::DashboardNowResponse),
        (status = 401, description = "No session", body = api::ApiError),
    ),
)]
async fn api_dashboard_now(
    State(state): State<Arc<AppState>>,
) -> axum::Json<api::DashboardNowResponse> {
    let stored = state.store.lock().unwrap();
    let config_revision = state
        .config_revision
        .load(std::sync::atomic::Ordering::SeqCst);
    let pool = state.pool();
    let now = unix_now();
    let current = state.history.current(now, || state.prometheus.render());
    let history_revision = current.tail.base_history_revision;
    axum::Json(api::DashboardNowResponse {
        auth: stored.client_auth.mode == config::Mode::Keyed,
        available_from: current.available_from,
        available_to: current.available_to,
        capacity_rpm: pool.capacity_rpm(),
        config_revision,
        default_window_days: stored.dashboard.default_window_days,
        history_revision,
        lanes: pool.len(),
        metrics: current.metrics,
        retention_days: stored.history.days,
        rpms: pool.rpms(),
        sampled_at: now,
        slo_target_percent: stored.dashboard.slo_target_percent,
        started: state.started,
        tail: current.tail,
        version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

async fn metrics_text(State(state): State<Arc<AppState>>) -> String {
    state.prometheus.render()
}

fn page_response(page: presentation::Page) -> Response {
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        presentation::page(page),
    )
        .into_response()
}

async fn public_asset(uri: Uri) -> Response {
    asset_response(presentation::public_asset(uri.path()))
}

async fn operator_asset(uri: Uri) -> Response {
    asset_response(presentation::operator_asset(uri.path()))
}

async fn public_catalog(Path(locale_file): Path<String>) -> Response {
    asset_response(
        locale_file
            .strip_suffix(".json")
            .and_then(presentation::public_catalog),
    )
}

async fn operator_catalog(Path(locale_file): Path<String>) -> Response {
    asset_response(
        locale_file
            .strip_suffix(".json")
            .and_then(presentation::operator_catalog),
    )
}

fn asset_response(asset: Option<presentation::Asset>) -> Response {
    let Some(asset) = asset else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, asset.content_type),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        asset.body,
    )
        .into_response()
}

fn is_presentation_path(path: &str) -> bool {
    matches!(
        path,
        routes::ROOT | routes::DASH | routes::LOGIN | routes::SETUP
    ) || path.starts_with("/assets/")
}

/// Add hardening headers to every response. Presentation resources are
/// same-origin compile-time assets, so the policy needs no inline or external
/// source exceptions.
async fn security_headers(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::HeaderValue;
    let no_store = is_presentation_path(req.uri().path());
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'none'; img-src 'self' data:; style-src 'self'; script-src 'self'; \
             connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    if no_store {
        h.insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        );
    }
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    resp
}

const BANNER: &str = r#"
     _  _ ___ __  __   ___ ___  _____  ____   __
    | \| |_ _|  \/  | | _ \ _ \/ _ \ \/ /\ \ / /
    | .` || || |\/| | |  _/   / (_) >  <  \ V /
    |_|\_|___|_|  |_| |_| |_|_\\___/_/\_\  |_|
"#;

/// `nim-proxy --health`: probe our own /health endpoint and exit 0/1.
/// Exists because the scratch image has no shell or curl for HEALTHCHECK.
fn health_probe() -> ! {
    use std::io::{Read, Write};
    let port = env_or("PORT", "8000");
    let ok = (|| -> std::io::Result<bool> {
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port.parse().unwrap_or(8000)))?;
        s.set_read_timeout(Some(Duration::from_secs(2)))?;
        s.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
        let mut buf = [0u8; 32];
        let n = s.read(&mut buf)?;
        Ok(String::from_utf8_lossy(&buf[..n]).contains("200"))
    })()
    .unwrap_or(false);
    std::process::exit(if ok { 0 } else { 1 });
}

/// Full proxy entry point — everything `main()` used to be. Lives in the
/// library crate so the fuzz targets (fuzz/) can link the internals;
/// src/main.rs is a shim that calls this.
#[tokio::main]
pub async fn run() {
    if std::env::args().any(|a| a == "--health") {
        health_probe();
    }
    dotenvy::dotenv().ok();
    println!("{BANNER}    v{}\n", env!("CARGO_PKG_VERSION"));
    tracing_subscriber::fmt()
        .compact()
        .with_target(false)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nim_proxy=info".into()),
        )
        .init();

    let trust_proxy = env_or("TRUST_PROXY", "false") == "true";
    warn_legacy_env();

    // The config store is the app's source of truth and holds credentials,
    // so its home must exist and be writable before anything else happens.
    let data_dir = std::path::PathBuf::from(env_or("DATA_DIR", "data"));
    if data_dir.as_os_str().is_empty() {
        eprintln!("DATA_DIR must point at a writable directory (the config store lives there)");
        std::process::exit(1);
    }
    let writable = std::fs::create_dir_all(&data_dir).and_then(|()| {
        let probe = data_dir.join(".write-probe");
        std::fs::write(&probe, b"ok")?;
        std::fs::remove_file(&probe)
    });
    if let Err(e) = writable {
        eprintln!(
            "\nnim-proxy cannot start: DATA_DIR {} is not writable ({e}).\n\
             The config store (settings, users, keys) persists there.\n",
            data_dir.display()
        );
        std::process::exit(1);
    }
    let stored = match config::load(&data_dir) {
        Ok(Some(sc)) => sc,
        Ok(None) => config::StoredConfig::default(),
        Err(e) => {
            eprintln!("\nnim-proxy cannot start: {e}\n");
            std::process::exit(1);
        }
    };
    let setup_required = stored.superuser().is_none();
    let cfg = stored.runtime();
    let port: u16 = env_or("PORT", "8000").parse().expect("PORT");

    if setup_required {
        tracing::warn!(
            "SETUP REQUIRED — no superuser exists yet. The FIRST VISITOR to the dashboard \
             claims this proxy; finish setup immediately. /v1 stays closed until then."
        );
    }
    tracing::info!(
        "config store      {}",
        config::store_path(&data_dir).display()
    );
    tracing::info!("upstream          {}", cfg.base_url);
    let pool_specs = stored.pool_specs();
    tracing::info!(
        "lanes             {} enabled key(s), {} rpm aggregate",
        pool_specs.iter().filter(|s| s.enabled).count(),
        pool_specs
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.rpm)
            .sum::<usize>()
    );
    tracing::info!(
        "API auth          {}",
        match &cfg.clients {
            Some(c) => format!("keyed ({} client key(s))", c.len()),
            None => "open (no client keys required — keep this on a trusted network)".to_owned(),
        }
    );
    tracing::info!(
        "dashboard auth    {}",
        if setup_required {
            "setup wizard (no users yet)".to_owned()
        } else {
            format!("session ({} user(s))", stored.users.len())
        }
    );
    tracing::info!(
        "patience          waits up to {}s per request, heartbeat every {}s",
        cfg.max_wait.as_secs(),
        cfg.heartbeat.as_secs()
    );

    let mut builder = PrometheusBuilder::new();
    for (name, bounds) in HISTOGRAM_BUCKETS {
        builder = builder
            .set_buckets_for_metric(Matcher::Full((*name).into()), bounds)
            .unwrap();
    }
    let prometheus = builder.install_recorder().expect("prometheus recorder");
    metrics::describe_counter!(
        "nimproxy_usage_observations_total",
        "Final classified upstream usage observations by field and result."
    );
    metrics::describe_gauge!(
        "nimproxy_history_persistence_degraded",
        "Whether canonical history persistence is degraded (0 = ok, 1 = degraded)."
    );

    let pool: PoolHandle = Arc::new(RwLock::new(Arc::new(Pool::new(pool_specs))));

    // Metrics history: finish indexing before the listener can report ready,
    // then sample the registry with contemporaneous pool capacity.
    let history_capacity = capacity_snapshot(&pool.read().unwrap());
    let hist = match history::History::open(
        data_dir.clone(),
        stored.history.days,
        history_capacity.clone(),
    ) {
        Ok(history) => Arc::new(history),
        Err(error) => {
            tracing::error!(
                "canonical history unavailable: {error}; continuing with in-memory history"
            );
            Arc::new(history::History::load(
                None,
                stored.history.days,
                history_capacity,
            ))
        }
    };
    metrics::gauge!("nimproxy_history_persistence_degraded").set(
        if hist.status().persistence == "degraded" {
            1.0
        } else {
            0.0
        },
    );
    {
        let hist = hist.clone();
        let prom = prometheus.clone();
        let pool = pool.clone();
        // Undocumented test knob; the 5-minute default is the contract.
        let sample_secs: u64 = env_or("HISTORY_SAMPLE_SECS", &history::SAMPLE_SECS.to_string())
            .parse()
            .expect("HISTORY_SAMPLE_SECS");
        tokio::spawn(async move {
            loop {
                hist.append(
                    unix_now(),
                    &prom.render(),
                    capacity_snapshot(&pool.read().unwrap()),
                );
                tokio::time::sleep(Duration::from_secs(sample_secs.max(1))).await;
            }
        });
    }

    let state = Arc::new(AppState {
        dispatch: Dispatcher::new(pool.clone()),
        pool,
        http: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // No overall timeout: generations stream for a long time.
            .build()
            .expect("http client"),
        models_cache: Mutex::new(None),
        no_inject: std::sync::Mutex::new(std::collections::HashSet::new()),
        model_labels: std::sync::Mutex::new(std::collections::HashSet::new()),
        admin: Admin::new(trust_proxy),
        inflight: AtomicUsize::new(0),
        governor: Arc::new(governor::Governor::default()),
        history: hist,
        prometheus,
        config_revision: AtomicU64::new(1),
        started: unix_now(),
        store: std::sync::Mutex::new(stored),
        data_dir,
        setup_required: std::sync::atomic::AtomicBool::new(setup_required),
        cfg: RwLock::new(Arc::new(cfg)),
    });

    let dash = || async { page_response(presentation::Page::Dashboard) };
    // Session-gated surface: dashboard, config, history, metrics. The guard
    // middleware requires an authenticated user (session cookie, or
    // user:password header credentials for scrapers); pre-setup it routes
    // everything to the wizard.
    let control_plane = Router::new()
        .route(routes::API_DASHBOARD, get(api_dashboard))
        .route(routes::API_DASHBOARD_NOW, get(api_dashboard_now))
        .route(routes::API_CONFIG, get(settings::api_config))
        .route(routes::API_SETTINGS_NIM_KEYS, post(settings::nim_keys))
        .route(routes::API_SETTINGS_CLIENTS, post(settings::clients))
        .route(routes::API_SETTINGS_UPSTREAM, post(settings::upstream))
        .route(routes::API_SETTINGS_LIMITS, post(settings::limits))
        .route(routes::API_SETTINGS_HISTORY, post(settings::history))
        .route(routes::API_SETTINGS_GOVERNOR, post(settings::governor_cfg))
        .route(routes::API_SETTINGS_USERS, post(settings::users))
        .route(routes::API_SETTINGS_ACCOUNT, post(settings::account))
        .route(routes::API_SETTINGS_LOCALE, post(settings::locale))
        .route(
            routes::API_SETTINGS_VALIDATE_KEY,
            post(settings::validate_key),
        )
        .fallback(api::api_not_found)
        .method_not_allowed_fallback(api::api_method_not_allowed)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ));

    let protected = Router::new()
        .route(routes::ROOT, get(dash))
        .route(routes::DASH, get(dash))
        .route(routes::METRICS, get(metrics_text))
        .route(routes::ASSET_OPERATOR_CSS, get(operator_asset))
        .route(routes::ASSET_OPERATOR_SHARED_JS, get(operator_asset))
        .route(routes::ASSET_OPERATOR_DASHBOARD_JS, get(operator_asset))
        .route(routes::ASSET_OPERATOR_SETTINGS_JS, get(operator_asset))
        .route(routes::ASSET_OPERATOR_LOCALE, get(operator_catalog))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ))
        .nest(routes::API_PREFIX, control_plane);

    // Public surface: health probe, login flow, the first-run wizard (404
    // once setup completes), and the API (its own key gate + setup gate).
    let app = Router::new()
        .merge(protected)
        .route(routes::HEALTH, get(|| async { "ok" }))
        .route(routes::ASSET_PUBLIC_CSS, get(public_asset))
        .route(routes::ASSET_PUBLIC_SETUP_JS, get(public_asset))
        .route(routes::ASSET_PUBLIC_LOGIN_JS, get(public_asset))
        .route(routes::ASSET_PUBLIC_LOCALE, get(public_catalog))
        .nest(
            routes::API_PREFIX,
            Router::new().route(routes::API_LOCALE_BOOTSTRAP, get(api::locale_bootstrap)),
        )
        .route(
            routes::LOGIN,
            get(auth::login_page).post(auth::login_submit),
        )
        .route(routes::LOGOUT, post(auth::logout))
        .route(
            routes::SETUP,
            get(settings::setup_page).post(settings::setup_submit),
        )
        .route(
            routes::SETUP_VALIDATE_KEY,
            post(settings::setup_validate_key),
        )
        .route(routes::V1_WILDCARD, any(proxy::handle))
        .layer(axum::middleware::from_fn(security_headers))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(state);

    let host = env_or("HOST", "0.0.0.0");
    let addr = format!("{host}:{port}");
    tracing::info!("dashboard         http://localhost:{port}/  (metrics at /metrics)");
    tracing::info!("listening on      {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            // Docker sends SIGTERM on stop; terminals send SIGINT.
            let ctrl_c = tokio::signal::ctrl_c();
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("SIGTERM handler");
                tokio::select! {
                    _ = ctrl_c => {}
                    _ = term.recv() => {}
                }
            }
            #[cfg(not(unix))]
            {
                let _ = ctrl_c.await;
            }
            tracing::info!("shutting down");
        })
        .await
        .expect("server");
}

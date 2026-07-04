mod auth;
mod config;
mod dispatch;
mod governor;
mod history;
mod pool;
mod proxy;
mod settings;

use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, State};
use axum::routing::{any, get, post};
use axum::Router;
use bytes::Bytes;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use tokio::sync::Mutex;

use auth::Admin;
use dispatch::Dispatcher;
use pool::{Pool, PoolHandle};

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
    /// Reference $/1M token prices for the dashboard's "dollars saved" figure.
    pub price_in: f64,
    pub price_out: f64,
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
    pub overrides: HashMap<String, usize>,
}

impl Default for GovernorSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            overrides: HashMap::new(),
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

/// Live dashboard bootstrap config — reflects the current pool and pricing
/// even after a settings change swaps them.
async fn dash_config(State(state): State<Arc<AppState>>) -> axum::Json<serde_json::Value> {
    let cfg = state.cfg();
    let pool = state.pool();
    let rpms = pool.rpms();
    axum::Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "lanes": pool.len(),
        // Uniform-rpm compatibility value; per-lane truth is in `rpms`.
        "rpm": rpms.iter().copied().max().unwrap_or(0),
        "rpms": rpms,
        "capacity_rpm": pool.capacity_rpm(),
        "price_in": cfg.price_in,
        "price_out": cfg.price_out,
        "auth": cfg.clients.is_some(),
        "started": state.started,
    }))
}

async fn api_history(
    State(state): State<Arc<AppState>>,
    q: axum::extract::Query<HashMap<String, String>>,
) -> axum::Json<Vec<serde_json::Value>> {
    let now = unix_now();
    let get = |k: &str, d: u64| q.get(k).and_then(|v| v.parse().ok()).unwrap_or(d);
    let (from, to) = (get("from", now.saturating_sub(86400)), get("to", now));
    axum::Json(
        state
            .history
            .range(from, to, 288)
            .into_iter()
            .map(|(t, m)| serde_json::json!({"t": t, "m": m}))
            .collect(),
    )
}

/// Add hardening headers to every response. The CSP allows the dashboard's
/// own inline script/style, unpkg logos, and Google Fonts (system-font
/// fallback offline), but pins `connect-src` to 'self' so an injected
/// element can't exfiltrate to another origin — a second line of defense
/// behind server-side sanitizing and the dashboard's `esc()`.
async fn security_headers(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::HeaderValue;
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'none'; img-src 'self' https://unpkg.com data:; \
             style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
             font-src https://fonts.gstatic.com; \
             script-src 'self' 'unsafe-inline'; \
             connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
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

#[tokio::main]
async fn main() {
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
    tracing::info!("config store      {}", config::store_path(&data_dir).display());
    tracing::info!("upstream          {}", cfg.base_url);
    let pool_keys = stored.pool_keys();
    tracing::info!(
        "lanes             {} enabled key(s), {} rpm aggregate",
        pool_keys.len(),
        pool_keys.iter().map(|(_, rpm)| rpm).sum::<usize>()
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

    // Histogram bucket bounds, one row per metric.
    #[rustfmt::skip]
    let buckets: &[(&str, &[f64])] = &[
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
    let mut builder = PrometheusBuilder::new();
    for (name, bounds) in buckets {
        builder = builder
            .set_buckets_for_metric(Matcher::Full((*name).into()), bounds)
            .unwrap();
    }
    let prometheus = builder.install_recorder().expect("prometheus recorder");

    // Metrics history: 5-minute snapshots, store-configured retention,
    // persisted next to the config store.
    let hist = Arc::new(history::History::load(
        Some(data_dir.clone()),
        stored.history.days,
    ));
    {
        let hist = hist.clone();
        let prom = prometheus.clone();
        // Undocumented test knob; the 5-minute default is the contract.
        let sample_secs: u64 = env_or("HISTORY_SAMPLE_SECS", &history::SAMPLE_SECS.to_string())
            .parse()
            .expect("HISTORY_SAMPLE_SECS");
        tokio::spawn(async move {
            loop {
                hist.append(unix_now(), prom.render());
                tokio::time::sleep(Duration::from_secs(sample_secs.max(1))).await;
            }
        });
    }

    let pool: PoolHandle = Arc::new(RwLock::new(Arc::new(Pool::new(pool_keys))));
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
        started: unix_now(),
        store: std::sync::Mutex::new(stored),
        data_dir,
        setup_required: std::sync::atomic::AtomicBool::new(setup_required),
        cfg: RwLock::new(Arc::new(cfg)),
    });

    let dash = || async {
        (
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            include_str!("dashboard.html"),
        )
    };
    // Session-gated surface: dashboard, config, history, metrics. The guard
    // middleware requires an authenticated user (session cookie, or
    // user:password header credentials for scrapers); pre-setup it routes
    // everything to the wizard.
    let protected = Router::new()
        .route("/", get(dash))
        .route("/dash", get(dash))
        .route("/dash/config.json", get(dash_config))
        .route("/api/history", get(api_history))
        .route("/metrics", get(move || async move { prometheus.render() }))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ));

    // Public surface: health probe, login flow, the first-run wizard (404
    // once setup completes), and the API (its own key gate + setup gate).
    let app = Router::new()
        .merge(protected)
        .route("/health", get(|| async { "ok" }))
        .route("/login", get(auth::login_page).post(auth::login_submit))
        .route("/logout", post(auth::logout))
        .route(
            "/setup",
            get(settings::setup_page).post(settings::setup_submit),
        )
        .route("/setup/validate-key", post(settings::setup_validate_key))
        .route("/v1/{*path}", any(proxy::handle))
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

mod auth;
mod dispatch;
mod history;
mod pool;
mod proxy;

use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::DefaultBodyLimit;
use axum::routing::{any, get, post};
use axum::Router;
use bytes::Bytes;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use tokio::sync::Mutex;

use auth::Admin;
use dispatch::Dispatcher;
use pool::Pool;

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
}

pub struct AppState {
    pub cfg: Config,
    pub pool: Arc<Pool>,
    pub dispatch: Dispatcher,
    pub http: reqwest::Client,
    pub models_cache: Mutex<Option<(Instant, Bytes)>>,
    /// token -> client name. None = local mode, no client auth.
    pub clients: Option<HashMap<String, String>>,
    /// Models that rejected stream_options injection; never inject for them again.
    pub no_inject: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Distinct sanitized model labels seen (bounds metric cardinality).
    pub model_labels: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Admin gate for the dashboard + observability endpoints.
    pub admin: Admin,
    /// Requests currently in flight; capped to bound memory under floods.
    pub inflight: AtomicUsize,
    pub max_inflight: usize,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
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

    let keys: Vec<String> = env_or("NIM_API_KEYS", "")
        .split(',')
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
        .collect();
    if keys.is_empty() {
        eprintln!("NIM_API_KEYS is required (comma-separated nvapi-... keys)");
        std::process::exit(1);
    }

    // PROXY_API_KEYS entries gate /v1/*. Any key works; an optional
    // "name:secret" form labels that client in metrics, a bare secret is
    // auto-labeled. Empty = no API keys configured.
    let clients: Option<HashMap<String, String>> = {
        let entries: Vec<(String, String)> = env_or("PROXY_API_KEYS", "")
            .split(',')
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .enumerate()
            .map(|(i, e)| match e.split_once(':') {
                Some((name, secret)) => (secret.trim().to_owned(), name.trim().to_owned()),
                None => (e.to_owned(), format!("client{i}")),
            })
            .collect();
        (!entries.is_empty()).then(|| entries.into_iter().collect())
    };

    // Admin password gates the dashboard + observability endpoints.
    let admin_password = {
        let p = env_or("ADMIN_PASSWORD", "");
        (!p.is_empty()).then_some(p)
    };
    let insecure = env_or("INSECURE_NO_AUTH", "false") == "true";
    let trust_proxy = env_or("TRUST_PROXY", "false") == "true";

    // Fail closed: refuse to start exposed without auth. Secure mode requires
    // both an API key and an admin password; the only way to run fully open is
    // to opt in explicitly (loopback / firewalled deployments).
    let secure_mode = clients.is_some() && admin_password.is_some();
    if !(insecure || secure_mode) {
        eprintln!(
            "\nnim-proxy refuses to start without authentication.\n\n\
             Choose one:\n  \
             1. Secure mode  — set BOTH:\n       \
             PROXY_API_KEYS=<one-or-more-secrets>   (gates the API)\n       \
             ADMIN_PASSWORD=<password>              (gates the dashboard/metrics)\n  \
             2. Open mode    — set INSECURE_NO_AUTH=true\n       \
             (ONLY on localhost or behind a firewall/VPN; everything is unauthenticated)\n\n\
             Currently set: PROXY_API_KEYS={}, ADMIN_PASSWORD={}\n",
            if clients.is_some() { "yes" } else { "no" },
            if admin_password.is_some() {
                "yes"
            } else {
                "no"
            },
        );
        std::process::exit(1);
    }

    let rpm: usize = env_or("RPM_PER_KEY", "40").parse().expect("RPM_PER_KEY");
    if rpm == 0 {
        eprintln!("RPM_PER_KEY must be >= 1 (0 would stall every lane).");
        std::process::exit(1);
    }
    let cfg = Config {
        base_url: env_or("NIM_BASE_URL", "https://integrate.api.nvidia.com")
            .trim_end_matches('/')
            .to_owned(),
        max_wait: Duration::from_secs(
            env_or("MAX_WAIT_SECS", "900")
                .parse()
                .expect("MAX_WAIT_SECS"),
        ),
        heartbeat: Duration::from_secs(
            env_or("HEARTBEAT_SECS", "10")
                .parse()
                .expect("HEARTBEAT_SECS"),
        ),
        models_ttl: Duration::from_secs(
            env_or("MODELS_TTL_SECS", "600")
                .parse()
                .expect("MODELS_TTL_SECS"),
        ),
        stream_idle: Duration::from_secs(
            env_or("STREAM_IDLE_SECS", "300")
                .parse()
                .expect("STREAM_IDLE_SECS"),
        ),
        request_timeout: Duration::from_secs(
            env_or("REQUEST_TIMEOUT_SECS", "300")
                .parse()
                .expect("REQUEST_TIMEOUT_SECS"),
        ),
        strict_passthrough: env_or("STRICT_PASSTHROUGH", "false")
            .parse()
            .expect("STRICT_PASSTHROUGH"),
        price_in: env_or("REF_PRICE_IN", "0.5").parse().expect("REF_PRICE_IN"),
        price_out: env_or("REF_PRICE_OUT", "2.0")
            .parse()
            .expect("REF_PRICE_OUT"),
    };
    let port: u16 = env_or("PORT", "8000").parse().expect("PORT");

    tracing::info!("upstream          {}", cfg.base_url);
    tracing::info!(
        "lanes             {} keys x {} rpm = {} rpm aggregate",
        keys.len(),
        rpm,
        keys.len() * rpm
    );
    if insecure {
        tracing::warn!("INSECURE_NO_AUTH=true — API and dashboard are UNAUTHENTICATED. Use only on localhost or behind a firewall.");
    }
    tracing::info!(
        "API auth          {}",
        match &clients {
            Some(c) => format!("required ({} key(s))", c.len()),
            None => "OFF (no PROXY_API_KEYS)".to_owned(),
        }
    );
    tracing::info!(
        "dashboard auth    {}",
        if admin_password.is_some() {
            "required (ADMIN_PASSWORD)"
        } else {
            "OFF"
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

    let max_inflight: usize = env_or("MAX_INFLIGHT", "512").parse().expect("MAX_INFLIGHT");
    let pool = Arc::new(Pool::new(keys, rpm));
    let state = Arc::new(AppState {
        dispatch: Dispatcher::new(pool.clone()),
        pool,
        clients,
        http: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // No overall timeout: generations stream for a long time.
            .build()
            .expect("http client"),
        models_cache: Mutex::new(None),
        no_inject: std::sync::Mutex::new(std::collections::HashSet::new()),
        model_labels: std::sync::Mutex::new(std::collections::HashSet::new()),
        admin: Admin::new(admin_password, trust_proxy),
        inflight: AtomicUsize::new(0),
        max_inflight,
        cfg,
    });

    let unix_now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    };
    let started = unix_now();

    // Metrics history: 5-minute snapshots, HISTORY_DAYS retention (0 = keep
    // forever), persisted to DATA_DIR when writable.
    let history_days: u64 = env_or("HISTORY_DAYS", "30").parse().expect("HISTORY_DAYS");
    let data_dir = env_or("DATA_DIR", "data");
    let hist = Arc::new(history::History::load(
        (!data_dir.is_empty()).then(|| data_dir.into()),
        history_days,
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
    let dash_config = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "lanes": state.pool.len(),
        "rpm": rpm,
        "price_in": state.cfg.price_in,
        "price_out": state.cfg.price_out,
        "auth": state.clients.is_some(),
        "started": started,
    })
    .to_string();

    let dash = || async {
        (
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            include_str!("dashboard.html"),
        )
    };
    // Admin-gated surface: dashboard, config, history, metrics. The guard
    // middleware passes a valid session cookie / Bearer / Basic, else it
    // redirects browsers to /login and 401s API clients.
    let protected = Router::new()
        .route("/", get(dash))
        .route("/dash", get(dash))
        .route(
            "/dash/config.json",
            get(move || async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    dash_config,
                )
            }),
        )
        .route(
            "/api/history",
            get(
                move |q: axum::extract::Query<std::collections::HashMap<String, String>>| {
                    let hist = hist.clone();
                    async move {
                        let now = unix_now();
                        let get =
                            |k: &str, d: u64| q.get(k).and_then(|v| v.parse().ok()).unwrap_or(d);
                        let (from, to) = (get("from", now.saturating_sub(86400)), get("to", now));
                        let body: Vec<serde_json::Value> = hist
                            .range(from, to, 288)
                            .into_iter()
                            .map(|(t, m)| serde_json::json!({"t": t, "m": m}))
                            .collect();
                        axum::Json(body)
                    }
                },
            ),
        )
        .route("/metrics", get(move || async move { prometheus.render() }))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_admin,
        ));

    // Public surface: health probe, login flow, and the API (its own key gate).
    let app = Router::new()
        .merge(protected)
        .route("/health", get(|| async { "ok" }))
        .route("/login", get(auth::login_page).post(auth::login_submit))
        .route("/logout", post(auth::logout))
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

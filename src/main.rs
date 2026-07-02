mod dispatch;
mod history;
mod pool;
mod proxy;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::DefaultBodyLimit;
use axum::routing::{any, get};
use axum::Router;
use bytes::Bytes;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use tokio::sync::Mutex;

use dispatch::Dispatcher;
use pool::Pool;

pub struct Config {
    pub base_url: String,
    pub max_wait: Duration,
    pub heartbeat: Duration,
    pub models_ttl: Duration,
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
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
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

    // PROXY_API_KEYS entries are "name:secret" (name becomes the metrics
    // label) or a bare secret. Unset = local mode: no auth required.
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
    let rpm: usize = env_or("RPM_PER_KEY", "40").parse().expect("RPM_PER_KEY");
    let cfg = Config {
        base_url: env_or("NIM_BASE_URL", "https://integrate.api.nvidia.com")
            .trim_end_matches('/')
            .to_owned(),
        max_wait: Duration::from_secs(env_or("MAX_WAIT_SECS", "900").parse().expect("MAX_WAIT_SECS")),
        heartbeat: Duration::from_secs(env_or("HEARTBEAT_SECS", "10").parse().expect("HEARTBEAT_SECS")),
        models_ttl: Duration::from_secs(env_or("MODELS_TTL_SECS", "600").parse().expect("MODELS_TTL_SECS")),
        price_in: env_or("REF_PRICE_IN", "0.5").parse().expect("REF_PRICE_IN"),
        price_out: env_or("REF_PRICE_OUT", "2.0").parse().expect("REF_PRICE_OUT"),
    };
    let port: u16 = env_or("PORT", "8000").parse().expect("PORT");

    tracing::info!("upstream          {}", cfg.base_url);
    tracing::info!(
        "lanes             {} keys x {} rpm = {} rpm aggregate",
        keys.len(),
        rpm,
        keys.len() * rpm
    );
    tracing::info!(
        "client auth       {}",
        match &clients {
            Some(c) => format!("required ({} clients)", c.len()),
            None => "off (local mode)".to_owned(),
        }
    );
    tracing::info!("patience          waits up to {}s per request, heartbeat every {}s",
        cfg.max_wait.as_secs(), cfg.heartbeat.as_secs());

    let prometheus = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full("nimproxy_ttft_seconds".into()),
            &[0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0],
        )
        .unwrap()
        .set_buckets_for_metric(
            Matcher::Full("nimproxy_tokens_per_second".into()),
            &[1.0, 2.0, 5.0, 10.0, 20.0, 40.0, 80.0, 160.0, 320.0],
        )
        .unwrap()
        .set_buckets_for_metric(
            Matcher::Full("nimproxy_queue_wait_seconds".into()),
            &[0.001, 0.05, 0.25, 1.0, 5.0, 15.0, 60.0, 180.0, 600.0],
        )
        .unwrap()
        .set_buckets_for_metric(
            Matcher::Full("nimproxy_upstream_seconds".into()),
            &[0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0],
        )
        .unwrap()
        .install_recorder()
        .expect("prometheus recorder");

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
    let app = Router::new()
        .route("/", get(dash))
        .route("/dash", get(dash))
        .route(
            "/dash/config.json",
            get(move || async move {
                ([(axum::http::header::CONTENT_TYPE, "application/json")], dash_config)
            }),
        )
        .route(
            "/api/history",
            get(move |q: axum::extract::Query<std::collections::HashMap<String, String>>| {
                let hist = hist.clone();
                async move {
                    let now = unix_now();
                    let get = |k: &str, d: u64| q.get(k).and_then(|v| v.parse().ok()).unwrap_or(d);
                    let (from, to) = (get("from", now.saturating_sub(86400)), get("to", now));
                    let body: Vec<serde_json::Value> = hist
                        .range(from, to, 288)
                        .into_iter()
                        .map(|(t, m)| serde_json::json!({"t": t, "m": m}))
                        .collect();
                    axum::Json(body)
                }
            }),
        )
        .route("/health", get(|| async { "ok" }))
        .route("/metrics", get(move || async move { prometheus.render() }))
        .route("/v1/{*path}", any(proxy::handle))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    tracing::info!("dashboard         http://localhost:{port}/  (metrics at /metrics)");
    tracing::info!("listening on      {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server");
}

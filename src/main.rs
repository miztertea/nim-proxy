mod pool;
mod proxy;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::DefaultBodyLimit;
use axum::routing::{any, get};
use axum::Router;
use bytes::Bytes;
use tokio::sync::Mutex;

use pool::Pool;

pub struct Config {
    pub base_url: String,
    pub max_wait: Duration,
    pub heartbeat: Duration,
    pub models_ttl: Duration,
}

pub struct AppState {
    pub cfg: Config,
    pub pool: Pool,
    pub http: reqwest::Client,
    pub models_cache: Mutex<Option<(Instant, Bytes)>>,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
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

    let rpm: usize = env_or("RPM_PER_KEY", "40").parse().expect("RPM_PER_KEY");
    let cfg = Config {
        base_url: env_or("NIM_BASE_URL", "https://integrate.api.nvidia.com")
            .trim_end_matches('/')
            .to_owned(),
        max_wait: Duration::from_secs(env_or("MAX_WAIT_SECS", "900").parse().expect("MAX_WAIT_SECS")),
        heartbeat: Duration::from_secs(env_or("HEARTBEAT_SECS", "10").parse().expect("HEARTBEAT_SECS")),
        models_ttl: Duration::from_secs(env_or("MODELS_TTL_SECS", "600").parse().expect("MODELS_TTL_SECS")),
    };
    let port: u16 = env_or("PORT", "8000").parse().expect("PORT");

    tracing::info!(
        keys = keys.len(),
        rpm,
        base_url = %cfg.base_url,
        "starting nim-proxy: {} lanes x {} rpm = {} rpm aggregate",
        keys.len(),
        rpm,
        keys.len() * rpm
    );

    let state = Arc::new(AppState {
        pool: Pool::new(keys, rpm),
        http: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // No overall timeout: generations stream for a long time.
            .build()
            .expect("http client"),
        models_cache: Mutex::new(None),
        cfg,
    });

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/{*path}", any(proxy::handle))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    tracing::info!(%addr, "listening");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server");
}

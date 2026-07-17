//! Test harness: an in-process mock NIM upstream with scriptable behaviors,
//! and a launcher that runs the real proxy binary against it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::StreamExt;

/// One scripted response for the next chat-completions request. The queue is
/// consumed front-to-back; when empty, `Ok` is the default.
#[derive(Clone, Copy, Debug)]
pub enum Behavior {
    /// Respond normally (stream or JSON per the request's `stream` flag).
    Ok,
    /// 429 with this Retry-After (seconds).
    RateLimited(u64),
    /// 429 carrying NIM's worker-exhaustion signature — model-scoped, so the
    /// proxy must back off the model (governor), never bench the lane.
    WorkerExhausted,
    /// A retryable server error.
    ServerError(u16),
    /// 400 unconditionally.
    BadRequest,
    /// 400 only when the request carries stream_options (injection probe).
    BadRequestIfInjected,
    /// Send headers + one chunk, then stall forever.
    Hang,
    /// Wait before sending response headers, simulating an upstream that has
    /// accepted work but has not started its response.
    DelayHeaders(u64),
    /// Stream a data event at this interval forever. Unlike `Hang`, this stays
    /// active often enough that an idle timeout must not be what stops it.
    ActiveStream(u64),
    /// Write large chunks without pause until downstream backpressure fills
    /// the proxy's response channel.
    FloodStream,
    /// Buffered response with an unknown `finish_reason` — exercises the
    /// server-side clamp that collapses odd values to `other`.
    OddFinish,
}

pub struct Hit {
    pub key: String,
    pub body: serde_json::Value,
    pub at: Instant,
}

#[derive(Default)]
pub struct MockState {
    pub hits: Mutex<Vec<Hit>>,
    pub script: Mutex<VecDeque<Behavior>>,
    pub models_hits: AtomicUsize,
    pub models_delay_ms: AtomicU64,
}

impl MockState {
    pub fn push(&self, b: Behavior) {
        self.script.lock().unwrap().push_back(b);
    }
    pub fn hit_count(&self) -> usize {
        self.hits.lock().unwrap().len()
    }
    pub fn hit_keys(&self) -> Vec<String> {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .map(|h| h.key.clone())
            .collect()
    }
    pub fn hit_gap(&self, a: usize, b: usize) -> Duration {
        let hits = self.hits.lock().unwrap();
        hits[b].at.duration_since(hits[a].at)
    }
}

pub struct MockNim {
    pub url: String,
    pub state: Arc<MockState>,
}

pub async fn start_mock() -> MockNim {
    let state = Arc::new(MockState::default());
    let app = Router::new()
        .route("/v1/models", get(mock_models))
        .route("/v1/chat/completions", post(mock_chat))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    MockNim { url, state }
}

async fn mock_models(State(state): State<Arc<MockState>>) -> Response {
    state.models_hits.fetch_add(1, Ordering::SeqCst);
    let delay = state.models_delay_ms.load(Ordering::SeqCst);
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    axum::Json(serde_json::json!({
        "object": "list",
        "data": [{"id": "mock/model-a", "object": "model", "created": 0, "owned_by": "mock"}]
    }))
    .into_response()
}

async fn mock_chat(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let key = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();
    state.hits.lock().unwrap().push(Hit {
        key,
        body: parsed.clone(),
        at: Instant::now(),
    });
    let behavior = state
        .script
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or(Behavior::Ok);
    let wants_stream = parsed["stream"].as_bool().unwrap_or(false);
    let injected = parsed.get("stream_options").is_some();

    match behavior {
        Behavior::RateLimited(secs) => Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header(header::RETRY_AFTER, secs.to_string())
            .body(Body::from(r#"{"error":"rate limited"}"#))
            .unwrap(),
        Behavior::WorkerExhausted => Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .body(Body::from(
                r#"{"detail":"ResourceExhausted: Worker local total request limit reached (32/32)"}"#,
            ))
            .unwrap(),
        Behavior::ServerError(code) => Response::builder()
            .status(StatusCode::from_u16(code).unwrap())
            .body(Body::from(r#"{"error":"boom"}"#))
            .unwrap(),
        Behavior::BadRequest => bad_request(),
        Behavior::BadRequestIfInjected if injected => bad_request(),
        Behavior::OddFinish => axum::Json(serde_json::json!({
            "id": "mock-1", "object": "chat.completion",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "banana"}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 2}
        }))
        .into_response(),
        Behavior::Hang => {
            let stream = futures_util::stream::once(async {
                Ok::<_, std::io::Error>(Bytes::from("data: {\"choices\":[]}\n\n"))
            })
            .chain(futures_util::stream::pending());
            sse(Body::from_stream(stream))
        }
        Behavior::DelayHeaders(ms) => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            axum::Json(serde_json::json!({
                "id": "mock-delayed", "object": "chat.completion",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "delayed"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1}
            }))
            .into_response()
        }
        Behavior::ActiveStream(ms) => {
            let stream = futures_util::stream::unfold((), move |()| async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                Some((
                    Ok::<_, std::io::Error>(Bytes::from(
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"}}]}\n\n",
                    )),
                    (),
                ))
            });
            sse(Body::from_stream(stream))
        }
        Behavior::FloodStream => {
            let chunk = Bytes::from(vec![b'x'; 1024 * 1024]);
            let stream = futures_util::stream::unfold(chunk, |chunk| async move {
                Some((Ok::<_, std::io::Error>(chunk.clone()), chunk))
            });
            sse(Body::from_stream(stream))
        }
        Behavior::Ok | Behavior::BadRequestIfInjected => {
            // Echo the request's shape so e2e can exercise the quality metrics:
            // a request that offers tools gets a tool_calls response, otherwise
            // a normal stop. Usage always carries reasoning-token details.
            let offers_tools = parsed.get("tools").is_some();
            let usage = "\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2,\"completion_tokens_details\":{\"reasoning_tokens\":3}}";
            if wants_stream {
                let mut chunks: Vec<Result<Bytes, std::io::Error>> = Vec::new();
                if offers_tools {
                    chunks.push(Ok(Bytes::from(
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"get_weather\"}}]}}]}\n\n",
                    )));
                    chunks.push(Ok(Bytes::from(
                        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
                    )));
                } else {
                    chunks.push(Ok(Bytes::from(
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\n",
                    )));
                    chunks.push(Ok(Bytes::from(
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":\"stop\"}]}\n\n",
                    )));
                }
                chunks.push(Ok(Bytes::from(format!(
                    "data: {{\"choices\":[],{usage}}}\n\n"
                ))));
                chunks.push(Ok(Bytes::from("data: [DONE]\n\n")));
                sse(Body::from_stream(futures_util::stream::iter(chunks)))
            } else if offers_tools {
                axum::Json(serde_json::json!({
                    "id": "mock-1", "object": "chat.completion",
                    "choices": [{"index": 0, "message": {"role": "assistant", "tool_calls": [
                        {"index": 0, "id": "c1", "type": "function", "function": {"name": "get_weather", "arguments": "{}"}}
                    ]}, "finish_reason": "tool_calls"}],
                    "usage": {"prompt_tokens": 11, "completion_tokens": 2, "completion_tokens_details": {"reasoning_tokens": 3}}
                }))
                .into_response()
            } else {
                axum::Json(serde_json::json!({
                    "id": "mock-1", "object": "chat.completion",
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello world"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 11, "completion_tokens": 2, "completion_tokens_details": {"reasoning_tokens": 3}}
                }))
                .into_response()
            }
        }
    }
}

fn bad_request() -> Response {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(Body::from(r#"{"error":{"message":"bad stream_options"}}"#))
        .unwrap()
}

fn sse(body: Body) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .unwrap()
}

/// Test identity every store fixture contains: a superuser with a cheap
/// (1000-iteration) precomputed password hash — the iteration count is
/// encoded per hash, so the proxy verifies it with zero prod impact.
pub const TEST_USER: &str = "root";
pub const TEST_PASSWORD: &str = "test-password-1";
const TEST_HASH: &str = "pbkdf2-sha256$1000$00000000000000000000000000000000$dd5fe0be04ca7f9e24642561a5d4635c52c40be82cbd7587b5eddc913ad3c7a7";

pub fn sha256_hex(s: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(s.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// A config-store fixture. Defaults mirror the old test posture: open /v1,
/// three NIM keys at 40 rpm, short waits, 1s heartbeat.
pub struct StoreOpts {
    /// Open /v1 (no client keys needed). False = keyed mode.
    pub open: bool,
    /// (name, plaintext secret) client keys; stored as SHA-256 digests.
    pub clients: Vec<(String, String)>,
    /// (key, rpm) NIM keys, all enabled and owned by TEST_USER.
    pub nim_keys: Vec<(String, usize)>,
    /// Additional users (username, role: "admin" | "user"), all sharing
    /// TEST_PASSWORD.
    pub extra_users: Vec<(String, String)>,
    pub max_wait_secs: u64,
    pub heartbeat_secs: u64,
    pub stream_idle_secs: u64,
    pub request_timeout_secs: u64,
    pub max_inflight: usize,
    pub strict_passthrough: bool,
}

impl Default for StoreOpts {
    fn default() -> Self {
        Self {
            open: true,
            clients: Vec::new(),
            nim_keys: vec![
                ("test-key-0".into(), 40),
                ("test-key-1".into(), 40),
                ("test-key-2".into(), 40),
            ],
            extra_users: Vec::new(),
            max_wait_secs: 30,
            heartbeat_secs: 1,
            stream_idle_secs: 300,
            request_timeout_secs: 300,
            max_inflight: 512,
            strict_passthrough: false,
        }
    }
}

impl StoreOpts {
    pub fn json(&self, upstream: &str) -> serde_json::Value {
        let mut users = vec![serde_json::json!({
            "username": TEST_USER, "password_hash": TEST_HASH, "role": "superuser"
        })];
        for (name, role) in &self.extra_users {
            users.push(serde_json::json!({
                "username": name, "password_hash": TEST_HASH, "role": role
            }));
        }
        serde_json::json!({
            "version": 1,
            "upstream": {
                "base_url": upstream,
                "nim_keys": self.nim_keys.iter().map(|(k, rpm)| serde_json::json!({
                    "key": k, "owner": TEST_USER, "enabled": true, "rpm": rpm
                })).collect::<Vec<_>>(),
            },
            "client_auth": {
                "mode": if self.open { "open" } else { "keyed" },
                "keys": self.clients.iter().map(|(name, secret)| serde_json::json!({
                    "name": name, "secret_sha256": sha256_hex(secret), "owner": TEST_USER
                })).collect::<Vec<_>>(),
            },
            "limits": {
                "max_wait_secs": self.max_wait_secs,
                "heartbeat_secs": self.heartbeat_secs,
                "stream_idle_secs": self.stream_idle_secs,
                "request_timeout_secs": self.request_timeout_secs,
                "max_inflight": self.max_inflight,
                "strict_passthrough": self.strict_passthrough,
            },
            "users": users,
        })
    }
}

/// The real proxy binary running as a child process against a per-test
/// tempdir DATA_DIR (removed on drop).
pub struct Proxy {
    pub port: u16,
    pub child: std::process::Child,
    pub data_dir: std::path::PathBuf,
}

impl Proxy {
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    /// Kill -TERM; returns the exit status.
    pub fn terminate(mut self) -> std::process::ExitStatus {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "proxy did not exit after SIGTERM"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        // A SIGKILLed process never flushes its coverage profile. Under
        // `cargo llvm-cov` (detected via LLVM_PROFILE_FILE) ask for a graceful
        // SIGTERM — which the proxy handles — and wait briefly so the child
        // writes its profile; otherwise kill immediately to keep teardown fast.
        if std::env::var_os("LLVM_PROFILE_FILE").is_some() {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status();
            for _ in 0..150 {
                if let Ok(Some(_)) = self.child.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.data_dir);
    }
}

fn fresh_data_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "nimproxy-e2e-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Start the proxy with the default store fixture (open /v1, no auth
/// boilerplate for behavior tests). `envs` still override container-level
/// vars (PORT is set for you; HISTORY_SAMPLE_SECS etc. go here).
pub async fn start_proxy(upstream: &str, envs: &[(&str, &str)]) -> Proxy {
    start_proxy_with(upstream, StoreOpts::default(), envs).await
}

/// Start the proxy with a custom store fixture.
pub async fn start_proxy_with(upstream: &str, opts: StoreOpts, envs: &[(&str, &str)]) -> Proxy {
    let data_dir = fresh_data_dir();
    let store = serde_json::to_string_pretty(&opts.json(upstream)).unwrap();
    std::fs::write(data_dir.join("config.json"), store).unwrap();
    spawn_and_wait_healthy(data_dir, envs).await
}

/// Start the proxy with NO store: it must boot healthy in setup mode
/// (claimably closed — /v1 answers 503, browsers land on /setup).
pub async fn start_proxy_fresh() -> Proxy {
    spawn_and_wait_healthy(fresh_data_dir(), &[]).await
}

/// Boot the proxy against a pre-populated DATA_DIR and wait until healthy —
/// for hand-written recovery fixtures whose exact shape `StoreOpts` can't
/// express (e.g. orphan-owned keys with no users).
pub async fn start_proxy_in(data_dir: std::path::PathBuf, envs: &[(&str, &str)]) -> Proxy {
    spawn_and_wait_healthy(data_dir, envs).await
}

/// Gracefully stop the proxy but KEEP its DATA_DIR, then relaunch a fresh
/// instance against the same store — restart round-trip tests. `terminate()`
/// (via Drop) would delete the dir, so we repoint the doomed handle at a
/// throwaway path first and hand the real dir to the new instance.
pub async fn restart(mut proxy: Proxy, envs: &[(&str, &str)]) -> Proxy {
    let data_dir = std::mem::replace(
        &mut proxy.data_dir,
        std::env::temp_dir().join("nimproxy-restart-placeholder"),
    );
    proxy.terminate();
    spawn_and_wait_healthy(data_dir, envs).await
}

async fn spawn_and_wait_healthy(data_dir: std::path::PathBuf, envs: &[(&str, &str)]) -> Proxy {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut cmd = base_cmd(port, &data_dir);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("spawn proxy");
    let proxy = Proxy {
        port,
        child,
        data_dir,
    };

    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(r) = client.get(proxy.url("/health")).send().await {
            if r.status().is_success() {
                return proxy;
            }
        }
        assert!(Instant::now() < deadline, "proxy did not become healthy");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn base_cmd(port: u16, data_dir: &std::path::Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_nim-proxy"));
    cmd.env_clear()
        .current_dir(std::env::temp_dir()) // dodge any local .env
        .env("PORT", port.to_string())
        .env("DATA_DIR", data_dir)
        .env("RUST_LOG", "nim_proxy=warn")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Under `cargo llvm-cov` the spawned server must write its own coverage
    // profile, but env_clear() above dropped the profile path — forward it back
    // when present (a no-op in normal test runs).
    if let Ok(v) = std::env::var("LLVM_PROFILE_FILE") {
        cmd.env("LLVM_PROFILE_FILE", v);
    }
    cmd
}

/// Spawn the proxy against a pre-populated DATA_DIR and assert it exits
/// non-zero without ever becoming healthy — used for boot-posture tests
/// (corrupt store, future version, unwritable DATA_DIR).
pub async fn expect_refuses_to_start(data_dir: std::path::PathBuf) {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut child = base_cmd(port, &data_dir).spawn().expect("spawn proxy");
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            assert!(
                !status.success(),
                "proxy should exit non-zero, got {status:?}"
            );
            let _ = std::fs::remove_dir_all(&data_dir);
            return;
        }
        if let Ok(r) = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await
        {
            if r.status().is_success() {
                let _ = child.kill();
                panic!("proxy became healthy but should have refused to start");
            }
        }
        assert!(
            Instant::now() < deadline,
            "proxy neither exited nor became healthy"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A tempdir for boot-posture tests that pre-write store contents.
pub fn scratch_data_dir() -> std::path::PathBuf {
    fresh_data_dir()
}

/// Log in as `username` (TEST_PASSWORD) and return the session cookie value.
pub async fn login_as(proxy: &Proxy, username: &str) -> String {
    let resp = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("username={username}&password={TEST_PASSWORD}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 303, "login should succeed for {username}");
    let cookie = resp
        .headers()
        .get("set-cookie")
        .expect("session cookie")
        .to_str()
        .unwrap();
    cookie.split(';').next().unwrap().to_owned()
}

/// Session cookie for the default superuser.
pub async fn login(proxy: &Proxy) -> String {
    login_as(proxy, TEST_USER).await
}

/// Fetch /metrics authenticated with scraper-style header credentials.
pub async fn metrics(proxy: &Proxy) -> String {
    reqwest::Client::new()
        .get(proxy.url("/metrics"))
        .header(
            "authorization",
            format!("Bearer {TEST_USER}:{TEST_PASSWORD}"),
        )
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
}

/// Drive the setup wizard's single POST; returns the session cookie it mints.
pub async fn complete_setup(
    proxy: &Proxy,
    username: &str,
    password: &str,
    base_url: &str,
    nim_keys: &[(&str, usize)],
) -> String {
    let body = serde_json::json!({
        "username": username,
        "password": password,
        "base_url": base_url,
        "nim_keys": nim_keys.iter().map(|(k, rpm)| serde_json::json!({"key": k, "rpm": rpm})).collect::<Vec<_>>(),
    });
    let resp = reqwest::Client::new()
        .post(proxy.url("/setup"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "setup should succeed");
    let cookie = resp
        .headers()
        .get("set-cookie")
        .expect("setup mints a session")
        .to_str()
        .unwrap();
    cookie.split(';').next().unwrap().to_owned()
}

/// Read an SSE response to completion (until [DONE], an error event, or EOF);
/// returns the full body text.
pub async fn read_sse(resp: reqwest::Response) -> String {
    let mut out = String::new();
    let mut stream = resp.bytes_stream();
    let deadline = Instant::now() + Duration::from_secs(20);
    while let Ok(Some(chunk)) = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        stream.next(),
    )
    .await
    {
        out.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
        if out.contains("data: [DONE]") || out.contains("\"proxy_error\"") {
            break;
        }
    }
    out
}

/// A chat-completions body for conversation `convo` (affinity follows the
/// system + first-user messages).
pub fn chat_body(convo: &str, stream: bool) -> serde_json::Value {
    serde_json::json!({
        "model": "mock/model-a",
        "stream": stream,
        "messages": [
            {"role": "system", "content": "you are a test"},
            {"role": "user", "content": convo}
        ]
    })
}

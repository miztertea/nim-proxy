//! Test harness: an in-process mock NIM upstream with scriptable behaviors,
//! and a launcher that runs the real proxy binary against it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    /// A retryable server error.
    ServerError(u16),
    /// 400 unconditionally.
    BadRequest,
    /// 400 only when the request carries stream_options (injection probe).
    BadRequestIfInjected,
    /// Send headers + one chunk, then stall forever.
    Hang,
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
        Behavior::ServerError(code) => Response::builder()
            .status(StatusCode::from_u16(code).unwrap())
            .body(Body::from(r#"{"error":"boom"}"#))
            .unwrap(),
        Behavior::BadRequest => bad_request(),
        Behavior::BadRequestIfInjected if injected => bad_request(),
        Behavior::Hang => {
            let stream = futures_util::stream::once(async {
                Ok::<_, std::io::Error>(Bytes::from("data: {\"choices\":[]}\n\n"))
            })
            .chain(futures_util::stream::pending());
            sse(Body::from_stream(stream))
        }
        Behavior::Ok | Behavior::BadRequestIfInjected => {
            if wants_stream {
                let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
                    Ok(Bytes::from(
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\n",
                    )),
                    Ok(Bytes::from(
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"}}]}\n\n",
                    )),
                    Ok(Bytes::from(
                        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2}}\n\n",
                    )),
                    Ok(Bytes::from("data: [DONE]\n\n")),
                ];
                sse(Body::from_stream(futures_util::stream::iter(chunks)))
            } else {
                axum::Json(serde_json::json!({
                    "id": "mock-1", "object": "chat.completion",
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello world"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 11, "completion_tokens": 2}
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

/// The real proxy binary running as a child process.
pub struct Proxy {
    pub port: u16,
    pub child: std::process::Child,
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
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start the proxy against `upstream` with sane test defaults, overridable
/// via `envs`. History is disabled unless a test sets DATA_DIR.
pub async fn start_proxy(upstream: &str, envs: &[(&str, &str)]) -> Proxy {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_nim-proxy"));
    cmd.env_clear()
        .current_dir(std::env::temp_dir()) // dodge any local .env
        .env("PORT", port.to_string())
        .env("NIM_BASE_URL", upstream)
        .env("NIM_API_KEYS", "test-key-0,test-key-1,test-key-2")
        .env("DATA_DIR", "")
        .env("MAX_WAIT_SECS", "30")
        .env("HEARTBEAT_SECS", "1")
        .env("RUST_LOG", "nim_proxy=warn")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("spawn proxy");
    let proxy = Proxy { port, child };

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

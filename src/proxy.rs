//! Request handling: strict pass-through to NIM with three additions the
//! upstream doesn't give us — per-key rate-limit pacing, retry on 429/5xx,
//! and SSE comment heartbeats so agent harnesses (OpenCode etc.) keep the
//! connection open instead of aborting while we wait for a slot.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::pool::Reservation;
use crate::AppState;

/// Statuses worth waiting out: rate limit and transient server-side trouble.
fn retryable(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504)
}

/// Backoff for a benched lane: honor Retry-After when present.
fn backoff_for(resp: &reqwest::Response) -> Duration {
    resp.headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(10))
}

/// Reserve a rate-limit slot, sleeping (in `tick`-sized chunks so callers can
/// heartbeat between ticks) until one opens or the deadline passes.
async fn reserve_slot(
    state: &AppState,
    deadline: Instant,
    mut on_wait: impl FnMut(Duration) -> bool,
) -> Option<(usize, String)> {
    loop {
        match state.pool.reserve() {
            Reservation::Ready { lane, key } => return Some((lane, key)),
            Reservation::Wait(wait) => {
                if Instant::now() + wait > deadline || !on_wait(wait) {
                    return None;
                }
                tokio::time::sleep(wait.min(state.cfg.heartbeat)).await;
            }
        }
    }
}

fn upstream_request(
    state: &AppState,
    method: &Method,
    path_query: &str,
    headers: &HeaderMap,
    key: &str,
    body: &Bytes,
) -> reqwest::RequestBuilder {
    let url = format!("{}{}", state.cfg.base_url, path_query);
    let mut req = state
        .http
        .request(method.clone(), url)
        .header(header::AUTHORIZATION, format!("Bearer {key}"));
    for name in [header::CONTENT_TYPE, header::ACCEPT] {
        if let Some(v) = headers.get(&name) {
            req = req.header(name, v);
        }
    }
    if !body.is_empty() {
        req = req.body(body.clone());
    }
    req
}

/// Single entry point for every /v1/* call.
pub async fn handle(
    State(state): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path_query = uri
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| uri.path().to_owned());

    // Answer the model-catalog probe from cache: harnesses poll it and it
    // shouldn't burn rate-limit budget on every poll.
    if method == Method::GET && uri.path() == "/v1/models" {
        return models(state).await;
    }

    let wants_stream = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false);

    if wants_stream {
        streaming(state, method, path_query, headers, body).await
    } else {
        buffered(state, method, path_query, headers, body).await
    }
}

/// Non-streaming: pace, retry, and return the upstream response verbatim.
async fn buffered(
    state: Arc<AppState>,
    method: Method,
    path_query: String,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let deadline = Instant::now() + state.cfg.max_wait;
    loop {
        let Some((lane, key)) = reserve_slot(&state, deadline, |_| true).await else {
            return gateway_timeout(&state);
        };
        let resp = match upstream_request(&state, &method, &path_query, &headers, &key, &body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(lane, error = %e, "upstream connection error, retrying");
                state.pool.penalize(lane, Duration::from_secs(5));
                continue;
            }
        };
        if retryable(resp.status()) && Instant::now() < deadline {
            let backoff = backoff_for(&resp);
            tracing::info!(lane, status = %resp.status(), ?backoff, "lane benched, retrying");
            state.pool.penalize(lane, backoff);
            continue;
        }
        return relay(resp).await;
    }
}

/// Streaming: commit to a 200 SSE response immediately and emit `: heartbeat`
/// comment lines (ignored by every OpenAI SSE client) while we wait for a
/// slot or ride out 429/5xx, then pipe the upstream stream through.
async fn streaming(
    state: Arc<AppState>,
    method: Method,
    path_query: String,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(16);

    tokio::spawn(async move {
        let send = |b: &'static str| {
            let tx = tx.clone();
            async move { tx.send(Ok(Bytes::from(b))).await.is_ok() }
        };
        if !send(": connected\n\n").await {
            return;
        }
        let deadline = Instant::now() + state.cfg.max_wait;
        let mut last_beat = Instant::now();
        loop {
            // Reserve a slot, heartbeating so the harness doesn't hang up.
            let slot = reserve_slot(&state, deadline, |_| {
                if last_beat.elapsed() >= state.cfg.heartbeat {
                    last_beat = Instant::now();
                    if tx.try_send(Ok(Bytes::from(": heartbeat\n\n"))).is_err() {
                        return false; // client went away
                    }
                }
                true
            })
            .await;
            let Some((lane, key)) = slot else {
                let _ = tx
                    .send(Ok(sse_error("proxy timed out waiting for an upstream slot")))
                    .await;
                return;
            };

            let resp = match upstream_request(&state, &method, &path_query, &headers, &key, &body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(lane, error = %e, "upstream connection error, retrying");
                    state.pool.penalize(lane, Duration::from_secs(5));
                    continue;
                }
            };

            if retryable(resp.status()) {
                if Instant::now() >= deadline {
                    let _ = tx.send(Ok(sse_error("upstream unavailable, retries exhausted"))).await;
                    return;
                }
                let backoff = backoff_for(&resp);
                tracing::info!(lane, status = %resp.status(), ?backoff, "lane benched, retrying");
                state.pool.penalize(lane, backoff);
                if !send(": retrying\n\n").await {
                    return;
                }
                continue;
            }

            if !resp.status().is_success() {
                // Non-retryable upstream error after we already committed to
                // SSE: surface it as an in-stream error event.
                let status = resp.status();
                let detail = resp.text().await.unwrap_or_default();
                tracing::warn!(%status, "upstream rejected request");
                let _ = tx
                    .send(Ok(sse_error(&format!("upstream error {status}: {detail}"))))
                    .await;
                return;
            }

            let mut chunks = resp.bytes_stream();
            while let Some(chunk) = chunks.next().await {
                match chunk {
                    Ok(b) => {
                        if tx.send(Ok(b)).await.is_err() {
                            return; // client hung up
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "upstream stream broke mid-response");
                        let _ = tx.send(Ok(sse_error("upstream stream interrupted"))).await;
                        return;
                    }
                }
            }
            return;
        }
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap()
}

/// /v1/models, cached so harness catalog polls cost zero rate budget.
async fn models(state: Arc<AppState>) -> Response {
    {
        let cache = state.models_cache.lock().await;
        if let Some((at, body)) = cache.as_ref() {
            if at.elapsed() < state.cfg.models_ttl {
                return json_response(StatusCode::OK, body.clone());
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let Some((lane, key)) = reserve_slot(&state, deadline, |_| true).await else {
        return gateway_timeout(&state);
    };
    let url = format!("{}/v1/models", state.cfg.base_url);
    match state.http.get(url).bearer_auth(&key).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.bytes().await.unwrap_or_default();
            *state.models_cache.lock().await = Some((Instant::now(), body.clone()));
            json_response(StatusCode::OK, body)
        }
        Ok(resp) => {
            if retryable(resp.status()) {
                state.pool.penalize(lane, backoff_for(&resp));
            }
            relay(resp).await
        }
        Err(e) => {
            tracing::warn!(error = %e, "models fetch failed");
            gateway_timeout(&state)
        }
    }
}

/// Return an upstream response to the client as-is.
async fn relay(resp: reqwest::Response) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_owned();
    let body = resp.bytes().await.unwrap_or_default();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap()
}

fn sse_error(message: &str) -> Bytes {
    let event = serde_json::json!({
        "error": { "message": message, "type": "proxy_error", "code": "upstream_unavailable" }
    });
    Bytes::from(format!("data: {event}\n\ndata: [DONE]\n\n"))
}

fn json_response(status: StatusCode, body: Bytes) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn gateway_timeout(state: &AppState) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": format!(
                "no upstream slot became available within {}s (all {} keys saturated)",
                state.cfg.max_wait.as_secs(),
                state.pool.len()
            ),
            "type": "proxy_error",
            "code": "rate_limited"
        }
    });
    (StatusCode::GATEWAY_TIMEOUT, axum::Json(body)).into_response()
}

//! Request handling: strict pass-through to NIM with three additions the
//! upstream doesn't give us — per-key rate-limit pacing, retry on 429/5xx,
//! and SSE comment heartbeats so agent harnesses (OpenCode etc.) keep the
//! connection open instead of aborting while we wait for a slot. Every
//! request is measured on the way through (see README for the metric list).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use metrics::{counter, gauge, histogram};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::AppState;

/// Per-request metric labels, resolved once up front.
#[derive(Clone)]
struct Ctx {
    client: String,
    model: String,
    path: String,
    started: Instant,
}

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

/// Join the global FIFO queue for a rate-limit slot, invoking `on_wait` every
/// heartbeat interval so streaming callers can keep their client alive.
/// Returns None if the queue rejects us (no slot before the deadline) or
/// `on_wait` reports the client is gone.
async fn reserve_slot(
    state: &AppState,
    deadline: Instant,
    prefer: Option<usize>,
    mut on_wait: impl FnMut() -> bool,
) -> Option<(usize, String)> {
    let queued = Instant::now();
    let mut rx = state.dispatch.acquire(deadline, prefer);
    loop {
        tokio::select! {
            slot = &mut rx => {
                histogram!("nimproxy_queue_wait_seconds").record(queued.elapsed().as_secs_f64());
                if let Ok((lane, _)) = &slot {
                    counter!("nimproxy_lane_requests_total", "lane" => lane.to_string())
                        .increment(1);
                }
                return slot.ok();
            }
            _ = tokio::time::sleep(state.cfg.heartbeat) => {
                if !on_wait() {
                    return None;
                }
            }
        }
    }
}

fn bench(state: &AppState, lane: usize, status: &str, backoff: Duration) {
    counter!("nimproxy_lane_benched_total", "lane" => lane.to_string(), "status" => status.to_owned())
        .increment(1);
    state.pool.penalize(lane, backoff);
}

fn record_request(ctx: &Ctx, status: &str) {
    counter!(
        "nimproxy_requests_total",
        "client" => ctx.client.clone(),
        "model" => ctx.model.clone(),
        "path" => ctx.path.clone(),
        "status" => status.to_owned(),
    )
    .increment(1);
    tracing::info!(
        "{:<6} {} {} {} ({} ms)",
        status,
        ctx.client,
        ctx.model,
        ctx.path,
        ctx.started.elapsed().as_millis()
    );
}

fn record_tokens(ctx: &Ctx, prompt: Option<u64>, completion: Option<u64>, source: &str) {
    if let Some(p) = prompt {
        counter!("nimproxy_prompt_tokens_total", "client" => ctx.client.clone(), "model" => ctx.model.clone())
            .increment(p);
    }
    if let Some(c) = completion {
        counter!(
            "nimproxy_completion_tokens_total",
            "client" => ctx.client.clone(),
            "model" => ctx.model.clone(),
            "source" => source.to_owned(),
        )
        .increment(c);
    }
}

/// Watches an SSE byte stream for the `usage` object and counts data events
/// (a rough one-token-per-event estimate when the upstream omits usage).
/// Purely observational — bytes reach the client untouched.
#[derive(Default)]
struct SseScan {
    buf: String,
    events: u64,
    prompt: Option<u64>,
    completion: Option<u64>,
}

impl SseScan {
    fn feed(&mut self, bytes: &[u8]) {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=pos).collect();
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data:").map(str::trim_start) {
                if data == "[DONE]" {
                    continue;
                }
                self.events += 1;
                if data.contains("\"usage\"") {
                    if let Some(u) = serde_json::from_str::<serde_json::Value>(data)
                        .ok()
                        .and_then(|v| v.get("usage").filter(|u| !u.is_null()).cloned())
                    {
                        self.prompt = u
                            .get("prompt_tokens")
                            .and_then(|x| x.as_u64())
                            .or(self.prompt);
                        self.completion = u
                            .get("completion_tokens")
                            .and_then(|x| x.as_u64())
                            .or(self.completion);
                    }
                }
            }
        }
        // Guard against a pathological never-terminated line.
        if self.buf.len() > 1_048_576 {
            self.buf.clear();
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
    // Client auth: local mode (no configured keys) admits everyone as
    // "local"; otherwise the Bearer token must match a configured key.
    let client = match &state.clients {
        None => "local".to_owned(),
        Some(clients) => {
            let token = headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer "))
                .unwrap_or("");
            match clients.get(token) {
                Some(name) => name.clone(),
                None => {
                    counter!("nimproxy_unauthorized_total").increment(1);
                    return unauthorized();
                }
            }
        }
    };

    let path_query = uri
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| uri.path().to_owned());

    let parsed = serde_json::from_slice::<serde_json::Value>(&body).ok();
    let ctx = Ctx {
        client,
        model: parsed
            .as_ref()
            .and_then(|v| v.get("model").and_then(|m| m.as_str()))
            .unwrap_or("none")
            .to_owned(),
        path: uri.path().to_owned(),
        started: Instant::now(),
    };

    // Answer the model-catalog probe from cache: harnesses poll it and it
    // shouldn't burn rate-limit budget on every poll.
    if method == Method::GET && uri.path() == "/v1/models" {
        let resp = models(state).await;
        record_request(&ctx, resp.status().as_str());
        return resp;
    }

    let wants_stream = parsed
        .as_ref()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false);
    let prefer = parsed.as_ref().and_then(|v| affinity(v, state.pool.len()));

    // Usage injection: streamed responses only report exact token usage when
    // asked via stream_options, so ask on the client's behalf. `fallback`
    // keeps the untouched body for a one-shot retry if the model rejects it.
    let mut body = body;
    let mut fallback = None;
    if wants_stream && !state.cfg.strict_passthrough && uri.path() == "/v1/chat/completions" {
        let injectable = parsed
            .as_ref()
            .is_some_and(|v| v.is_object() && v.get("stream_options").is_none())
            && !state.no_inject.lock().unwrap().contains(&ctx.model);
        if injectable {
            let mut v = parsed.clone().unwrap();
            v["stream_options"] = serde_json::json!({ "include_usage": true });
            fallback = Some(std::mem::replace(
                &mut body,
                Bytes::from(serde_json::to_vec(&v).expect("serialize injected body")),
            ));
        }
    }

    if wants_stream {
        streaming(
            state, ctx, method, path_query, headers, body, prefer, fallback,
        )
        .await
    } else {
        buffered(state, ctx, method, path_query, headers, body, prefer).await
    }
}

/// Sticky-lane hint: hash the conversation's identity (model, system prompt,
/// and first user message — stable across every turn of an agent session) so
/// a conversation keeps hitting the same key while it has capacity, keeping
/// any upstream prefix cache warm. Purely an optimization; correctness never
/// depends on which key serves a request.
fn affinity(body: &serde_json::Value, lanes: usize) -> Option<usize> {
    let messages = body.get("messages")?.as_array()?;
    let mut h = DefaultHasher::new();
    body.get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .hash(&mut h);
    for msg in messages.iter().take(2) {
        msg.to_string().hash(&mut h);
    }
    Some((h.finish() % lanes as u64) as usize)
}

/// Non-streaming: pace, retry, and return the upstream response verbatim.
async fn buffered(
    state: Arc<AppState>,
    ctx: Ctx,
    method: Method,
    path_query: String,
    headers: HeaderMap,
    body: Bytes,
    prefer: Option<usize>,
) -> Response {
    let _active = crate::dispatch::scopeguard(|| gauge!("nimproxy_active_requests").decrement(1.0));
    gauge!("nimproxy_active_requests").increment(1.0);
    let deadline = Instant::now() + state.cfg.max_wait;
    loop {
        let Some((lane, key)) = reserve_slot(&state, deadline, prefer, || true).await else {
            record_request(&ctx, "504");
            return gateway_timeout(&state);
        };
        let sent_at = Instant::now();
        let resp = match upstream_request(&state, &method, &path_query, &headers, &key, &body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(lane, error = %e, "upstream connection error, retrying");
                bench(&state, lane, "connect", Duration::from_secs(5));
                continue;
            }
        };
        if retryable(resp.status()) && Instant::now() < deadline {
            let backoff = backoff_for(&resp);
            tracing::info!(lane, status = %resp.status(), ?backoff, "lane benched, retrying");
            bench(&state, lane, resp.status().as_str(), backoff);
            continue;
        }
        histogram!("nimproxy_upstream_seconds", "model" => ctx.model.clone())
            .record(sent_at.elapsed().as_secs_f64());
        record_request(&ctx, resp.status().as_str());
        return relay(resp, &ctx).await;
    }
}

/// Streaming: commit to a 200 SSE response immediately and emit `: heartbeat`
/// comment lines (ignored by every OpenAI SSE client) while we wait for a
/// slot or ride out 429/5xx, then pipe the upstream stream through.
#[allow(clippy::too_many_arguments)]
async fn streaming(
    state: Arc<AppState>,
    ctx: Ctx,
    method: Method,
    path_query: String,
    headers: HeaderMap,
    mut body: Bytes,
    prefer: Option<usize>,
    mut fallback: Option<Bytes>,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(16);

    tokio::spawn(async move {
        let _active =
            crate::dispatch::scopeguard(|| gauge!("nimproxy_active_requests").decrement(1.0));
        gauge!("nimproxy_active_requests").increment(1.0);
        let send = |b: &'static str| {
            let tx = tx.clone();
            async move { tx.send(Ok(Bytes::from(b))).await.is_ok() }
        };
        if !send(": connected\n\n").await {
            record_request(&ctx, "disconnect");
            return;
        }
        let deadline = Instant::now() + state.cfg.max_wait;
        loop {
            // Queue for a slot, heartbeating so the harness doesn't hang up.
            let slot = reserve_slot(&state, deadline, prefer, || {
                tx.try_send(Ok(Bytes::from(": heartbeat\n\n"))).is_ok()
            })
            .await;
            let Some((lane, key)) = slot else {
                record_request(&ctx, "504");
                let _ = tx
                    .send(Ok(sse_error(
                        "proxy timed out waiting for an upstream slot",
                    )))
                    .await;
                return;
            };

            let sent_at = Instant::now();
            let resp = match upstream_request(&state, &method, &path_query, &headers, &key, &body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(lane, error = %e, "upstream connection error, retrying");
                    bench(&state, lane, "connect", Duration::from_secs(5));
                    continue;
                }
            };

            // A 400 right after we injected stream_options usually means this
            // model rejects the field: remember that and retry untouched.
            if resp.status() == reqwest::StatusCode::BAD_REQUEST && fallback.is_some() {
                tracing::info!(model = %ctx.model, "model rejected stream_options; retrying without injection");
                state.no_inject.lock().unwrap().insert(ctx.model.clone());
                body = fallback.take().unwrap();
                continue;
            }

            if retryable(resp.status()) {
                if Instant::now() >= deadline {
                    record_request(&ctx, "504");
                    let _ = tx
                        .send(Ok(sse_error("upstream unavailable, retries exhausted")))
                        .await;
                    return;
                }
                let backoff = backoff_for(&resp);
                tracing::info!(lane, status = %resp.status(), ?backoff, "lane benched, retrying");
                bench(&state, lane, resp.status().as_str(), backoff);
                if !send(": retrying\n\n").await {
                    record_request(&ctx, "disconnect");
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
                record_request(&ctx, status.as_str());
                let _ = tx
                    .send(Ok(sse_error(&format!("upstream error {status}: {detail}"))))
                    .await;
                return;
            }

            let mut scan = SseScan::default();
            let mut first_chunk: Option<Instant> = None;
            let mut chunks = resp.bytes_stream();
            loop {
                // A stalled upstream would otherwise hold the client forever.
                let next = if state.cfg.stream_idle.is_zero() {
                    chunks.next().await
                } else {
                    match tokio::time::timeout(state.cfg.stream_idle, chunks.next()).await {
                        Ok(n) => n,
                        Err(_) => {
                            tracing::warn!(model = %ctx.model, idle = ?state.cfg.stream_idle, "upstream stream stalled");
                            record_request(&ctx, "stall");
                            let _ = tx.send(Ok(sse_error("upstream stream stalled"))).await;
                            return;
                        }
                    }
                };
                let Some(chunk) = next else { break };
                match chunk {
                    Ok(b) => {
                        if first_chunk.is_none() {
                            first_chunk = Some(Instant::now());
                            histogram!("nimproxy_ttft_seconds", "model" => ctx.model.clone())
                                .record(sent_at.elapsed().as_secs_f64());
                        }
                        scan.feed(&b);
                        if tx.send(Ok(b)).await.is_err() {
                            record_request(&ctx, "disconnect");
                            return; // client hung up
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "upstream stream broke mid-response");
                        record_request(&ctx, "stream_error");
                        let _ = tx.send(Ok(sse_error("upstream stream interrupted"))).await;
                        return;
                    }
                }
            }

            // Token accounting: exact when the upstream reported usage,
            // otherwise estimate ~1 token per SSE event.
            let source = if scan.completion.is_some() {
                "usage"
            } else {
                "estimate"
            };
            let completion = scan.completion.or(Some(scan.events));
            record_tokens(&ctx, scan.prompt, completion, source);
            if let (Some(first), Some(c)) = (first_chunk, completion) {
                let gen_secs = first.elapsed().as_secs_f64();
                if gen_secs > 0.1 && c > 0 {
                    histogram!("nimproxy_tokens_per_second", "model" => ctx.model.clone(), "source" => source.to_owned())
                        .record(c as f64 / gen_secs);
                }
            }
            record_request(&ctx, "200");
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

/// /v1/models, cached so harness catalog polls cost zero rate budget. The
/// lock is held across the refresh so concurrent misses make one upstream
/// call (followers see the fresh cache when they get the lock).
async fn models(state: Arc<AppState>) -> Response {
    let mut cache = state.models_cache.lock().await;
    if let Some((at, body)) = cache.as_ref() {
        if at.elapsed() < state.cfg.models_ttl {
            return json_response(StatusCode::OK, body.clone());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let Some((lane, key)) = reserve_slot(&state, deadline, None, || true).await else {
        return gateway_timeout(&state);
    };
    let url = format!("{}/v1/models", state.cfg.base_url);
    match state.http.get(url).bearer_auth(&key).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.bytes().await.unwrap_or_default();
            *cache = Some((Instant::now(), body.clone()));
            json_response(StatusCode::OK, body)
        }
        Ok(resp) => {
            if retryable(resp.status()) {
                bench(&state, lane, resp.status().as_str(), backoff_for(&resp));
            }
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = resp.bytes().await.unwrap_or_default();
            json_response(status, body)
        }
        Err(e) => {
            tracing::warn!(error = %e, "models fetch failed");
            gateway_timeout(&state)
        }
    }
}

/// Return an upstream response to the client as-is, harvesting the `usage`
/// object for token accounting on the way past.
async fn relay(resp: reqwest::Response, ctx: &Ctx) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_owned();
    let body = resp.bytes().await.unwrap_or_default();
    if status.is_success() {
        if let Some(u) = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("usage").filter(|u| !u.is_null()).cloned())
        {
            record_tokens(
                ctx,
                u.get("prompt_tokens").and_then(|x| x.as_u64()),
                u.get("completion_tokens").and_then(|x| x.as_u64()),
                "usage",
            );
        }
    }
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

fn unauthorized() -> Response {
    let body = serde_json::json!({
        "error": {
            "message": "missing or invalid proxy API key (Authorization: Bearer ...)",
            "type": "proxy_error",
            "code": "unauthorized"
        }
    });
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        axum::Json(body),
    )
        .into_response()
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

#[cfg(test)]
mod tests {
    use super::SseScan;

    #[test]
    fn scan_counts_events_and_finds_usage() {
        let mut scan = SseScan::default();
        // Feed in awkwardly split chunks to exercise line reassembly.
        scan.feed(b": heartbeat\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: {\"cho");
        scan.feed(b"ices\":[],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":45}}\n\ndata: [DONE]\n\n");
        assert_eq!(scan.events, 2);
        assert_eq!(scan.prompt, Some(120));
        assert_eq!(scan.completion, Some(45));
    }

    #[test]
    fn scan_without_usage_estimates_by_events() {
        let mut scan = SseScan::default();
        scan.feed(b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: {\"c\":3}\n\ndata: [DONE]\n\n");
        assert_eq!(scan.events, 3);
        assert_eq!(scan.completion, None);
    }
}

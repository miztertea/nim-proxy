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

use crate::dispatch::Slot;
use crate::governor::{self, ModelPermit};
use crate::{AppState, Config};

/// Per-request metric labels, resolved once up front.
#[derive(Clone)]
struct Ctx {
    client: String,
    model: String,
    path: String,
    started: Instant,
}

/// Cap on distinct `model` label values tracked, past which new models are
/// bucketed to "other" so an attacker can't explode metric cardinality.
const MODEL_LABEL_CAP: usize = 256;

/// Reduce an arbitrary client-supplied string to a safe metric-label / log
/// value: keep a conservative charset (which model ids use), drop everything
/// else (quotes, braces, newlines, control/ANSI — the injection vectors for
/// Prometheus exposition, structured logs, and terminals), and cap length.
fn sanitize_label(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':'))
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "none".to_owned()
    } else {
        cleaned
    }
}

/// Sanitize a model id and bound its cardinality: known models pass through,
/// but once `MODEL_LABEL_CAP` distinct values have been seen, further new
/// ones collapse to "other".
fn label_model(state: &AppState, raw: &str) -> String {
    let s = sanitize_label(raw);
    let mut seen = state.model_labels.lock().unwrap();
    bounded_label(&mut seen, s, MODEL_LABEL_CAP)
}

/// Cardinality guard: return `s` if already seen or under the cap (recording
/// it), else "other". Pure so it can be tested without an AppState.
fn bounded_label(seen: &mut std::collections::HashSet<String>, s: String, cap: usize) -> String {
    if seen.contains(&s) {
        s
    } else if seen.len() < cap {
        seen.insert(s.clone());
        s
    } else {
        "other".to_owned()
    }
}

/// Bound the `path` label to the known OpenAI endpoints; anything else
/// (arbitrary sub-paths a client can hit under /v1/) becomes "other".
fn label_path(path: &str) -> String {
    match path {
        "/v1/chat/completions"
        | "/v1/completions"
        | "/v1/embeddings"
        | "/v1/models"
        | "/v1/rankings" => path.to_owned(),
        _ => "other".to_owned(),
    }
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
    heartbeat: Duration,
    deadline: Instant,
    prefer: Option<usize>,
    mut on_wait: impl FnMut() -> bool,
) -> Option<Slot> {
    let queued = Instant::now();
    let mut rx = state.dispatch.acquire(deadline, prefer);
    loop {
        tokio::select! {
            slot = &mut rx => {
                histogram!("nimproxy_queue_wait_seconds").record(queued.elapsed().as_secs_f64());
                if let Ok(slot) = &slot {
                    counter!("nimproxy_lane_requests_total", "lane" => slot.lane.to_string())
                        .increment(1);
                }
                return slot.ok();
            }
            _ = tokio::time::sleep(heartbeat) => {
                if !on_wait() {
                    return None;
                }
            }
        }
    }
}

/// Wait for a model-pressure permit (the governor's worker-concurrency gate),
/// heartbeating so streaming callers keep their client alive. `Ok(None)`
/// means the request isn't gated (governor off, non-generation path, or no
/// model to scope by); `Err(())` means the deadline passed or the client left.
async fn acquire_model_permit(
    state: &AppState,
    cfg: &Config,
    ctx: &Ctx,
    deadline: Instant,
    mut on_wait: impl FnMut() -> bool,
) -> Result<Option<ModelPermit>, ()> {
    let gated = cfg.governor.enabled
        && ctx.model != "none"
        && matches!(
            ctx.path.as_str(),
            "/v1/chat/completions" | "/v1/completions"
        );
    if !gated {
        return Ok(None);
    }
    let pinned = cfg.governor.overrides.get(&ctx.model).copied();
    let mut next_heartbeat = Instant::now() + cfg.heartbeat;
    loop {
        if let Some(p) = state.governor.admit(&ctx.model, pinned) {
            return Ok(Some(p));
        }
        if Instant::now() + governor::POLL > deadline {
            return Err(());
        }
        tokio::time::sleep(governor::POLL).await;
        if Instant::now() >= next_heartbeat {
            if !on_wait() {
                return Err(());
            }
            next_heartbeat = Instant::now() + cfg.heartbeat;
        }
    }
}

/// Bench the granting lane after the upstream told us to back off. Routes
/// through the slot's own pool, so a bench that races a settings-driven pool
/// swap lands on the (possibly retired) generation that made the grant.
fn bench(slot: &Slot, status: &str, backoff: Duration) {
    counter!("nimproxy_lane_benched_total", "lane" => slot.lane.to_string(), "status" => status.to_owned())
        .increment(1);
    slot.pool.penalize(slot.lane, backoff);
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

/// Bound `finish_reason` to the known OpenAI set so an unexpected upstream
/// value can't grow label cardinality. `length` is the truncation signal.
fn finish_label(raw: &str) -> String {
    match raw {
        "stop" | "length" | "tool_calls" | "content_filter" | "function_call" => raw.to_owned(),
        _ => "other".to_owned(),
    }
}

/// The request's tool-selection mode, bounded to a small enum. Called only
/// when the request offers tools, so a missing `tool_choice` means the
/// provider default (auto).
fn tool_choice_mode(v: &serde_json::Value) -> &'static str {
    match v.get("tool_choice") {
        Some(serde_json::Value::String(s)) => match s.as_str() {
            "auto" => "auto",
            "none" => "none",
            "required" => "required",
            _ => "other",
        },
        Some(serde_json::Value::Object(_)) => "named",
        _ => "auto",
    }
}

/// Count tools offered in a request body (`tools`, or legacy `functions`).
fn count_tools(v: &serde_json::Value) -> Option<usize> {
    v.get("tools")
        .and_then(|t| t.as_array())
        .map(|a| a.len())
        .or_else(|| {
            v.get("functions")
                .and_then(|t| t.as_array())
                .map(|a| a.len())
        })
}

/// Whether the request asks for structured (JSON) output.
fn is_json_mode(v: &serde_json::Value) -> bool {
    v.get("response_format")
        .and_then(|rf| rf.get("type"))
        .and_then(|t| t.as_str())
        .is_some_and(|t| t == "json_object" || t == "json_schema")
}

/// Record request-shape metrics: what the harness asked for (stream flag,
/// conversation depth, tools offered, sampling params, output cap, JSON mode).
/// Counts and sizes only — never message content. All heavy values go to
/// histograms, never labels, so cardinality stays bounded.
fn record_shape(ctx: &Ctx, parsed: Option<&serde_json::Value>, wants_stream: bool) {
    // Labeled by client: request shape reflects the harness, not the model —
    // this is what powers the Harnesses view ("what is each agent doing").
    counter!(
        "nimproxy_stream_requests_total",
        "client" => ctx.client.clone(),
        "stream" => if wants_stream { "true" } else { "false" }.to_owned(),
    )
    .increment(1);
    let Some(v) = parsed else { return };
    if let Some(msgs) = v.get("messages").and_then(|m| m.as_array()) {
        histogram!("nimproxy_request_messages", "client" => ctx.client.clone())
            .record(msgs.len() as f64);
    }
    if let Some(n) = count_tools(v) {
        histogram!("nimproxy_request_tools", "client" => ctx.client.clone()).record(n as f64);
        counter!("nimproxy_tool_choice_total", "mode" => tool_choice_mode(v).to_owned())
            .increment(1);
    }
    if let Some(mt) = v
        .get("max_tokens")
        .and_then(|x| x.as_u64())
        .or_else(|| v.get("max_completion_tokens").and_then(|x| x.as_u64()))
    {
        histogram!("nimproxy_request_max_tokens", "client" => ctx.client.clone()).record(mt as f64);
    }
    if let Some(t) = v.get("temperature").and_then(|x| x.as_f64()) {
        histogram!("nimproxy_request_temperature", "client" => ctx.client.clone()).record(t);
    }
    if is_json_mode(v) {
        counter!("nimproxy_json_mode_total", "client" => ctx.client.clone()).increment(1);
    }
}

/// Record response-quality signals available once a generation completes:
/// how it ended (truncation), reasoning-token burn, and tool-call volume.
fn record_quality(
    ctx: &Ctx,
    finish: Option<&str>,
    reasoning: Option<u64>,
    tool_calls: Option<u64>,
) {
    if let Some(fr) = finish {
        counter!(
            "nimproxy_finish_reason_total",
            "model" => ctx.model.clone(),
            "reason" => finish_label(fr),
        )
        .increment(1);
    }
    if let Some(r) = reasoning {
        if r > 0 {
            counter!("nimproxy_reasoning_tokens_total", "model" => ctx.model.clone()).increment(r);
        }
    }
    if let Some(n) = tool_calls {
        if n > 0 {
            counter!("nimproxy_tool_calls_total", "model" => ctx.model.clone()).increment(n);
        }
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
    reasoning: Option<u64>,
    finish_reason: Option<String>,
    /// Highest `tool_calls[].index` seen; +1 is the tool-call count.
    tool_call_max: Option<u64>,
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
                // Only the events that carry usage, a concrete finish_reason
                // (a string, not the per-chunk `null`), or tool_calls are worth
                // a full JSON parse; plain content deltas are skipped.
                let interesting = data.contains("\"usage\"")
                    || data.contains("\"finish_reason\":\"")
                    || data.contains("\"tool_calls\"");
                if interesting {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                            self.prompt = u
                                .get("prompt_tokens")
                                .and_then(|x| x.as_u64())
                                .or(self.prompt);
                            self.completion = u
                                .get("completion_tokens")
                                .and_then(|x| x.as_u64())
                                .or(self.completion);
                            self.reasoning = u
                                .get("completion_tokens_details")
                                .and_then(|d| d.get("reasoning_tokens"))
                                .and_then(|x| x.as_u64())
                                .or(self.reasoning);
                        }
                        if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
                            for ch in choices {
                                if let Some(fr) = ch.get("finish_reason").and_then(|f| f.as_str()) {
                                    self.finish_reason = Some(fr.to_owned());
                                }
                                if let Some(tcs) = ch
                                    .get("delta")
                                    .and_then(|d| d.get("tool_calls"))
                                    .and_then(|t| t.as_array())
                                {
                                    for tc in tcs {
                                        if let Some(idx) = tc.get("index").and_then(|i| i.as_u64())
                                        {
                                            self.tool_call_max = Some(
                                                self.tool_call_max.map_or(idx, |m| m.max(idx)),
                                            );
                                        }
                                    }
                                }
                            }
                        }
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
    http: &reqwest::Client,
    base_url: &str,
    method: &Method,
    path_query: &str,
    headers: &HeaderMap,
    key: &str,
    body: &Bytes,
) -> reqwest::RequestBuilder {
    let url = format!("{base_url}{path_query}");
    let mut req = http
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
    // One consistent config view for this request's whole lifetime; a
    // concurrent settings save affects only requests that arrive after it.
    let cfg = state.cfg();

    // Shed load past the in-flight cap so a connection flood can't grow the
    // queue unbounded. A guard decrements on every exit path.
    let inflight = state
        .inflight
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    let _guard = crate::dispatch::scopeguard({
        let state = state.clone();
        move || {
            state
                .inflight
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    if inflight > cfg.max_inflight {
        counter!("nimproxy_shed_total").increment(1);
        return overloaded(cfg.max_inflight);
    }

    // Client auth: local mode (no configured keys) admits everyone as
    // "local"; otherwise the Bearer token must match a configured key.
    // The match is constant-time to avoid leaking a valid key byte-by-byte.
    let client = match &cfg.clients {
        None => "local".to_owned(),
        Some(clients) => {
            let token = headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer "))
                .unwrap_or("");
            let mut matched = None;
            for (secret, name) in clients {
                if crate::auth::ct_eq(token, secret) {
                    matched = Some(name.clone());
                }
            }
            match matched {
                Some(name) => name,
                None => {
                    counter!("nimproxy_unauthorized_total").increment(1);
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    return unauthorized();
                }
            }
        }
    };

    let path_query = uri
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| uri.path().to_owned());

    let mut parsed = serde_json::from_slice::<serde_json::Value>(&body).ok();
    let raw_model = parsed
        .as_ref()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()))
        .unwrap_or("none");
    let ctx = Ctx {
        client,
        model: label_model(&state, raw_model),
        path: label_path(uri.path()),
        started: Instant::now(),
    };

    // Answer the model-catalog probe from cache: harnesses poll it and it
    // shouldn't burn rate-limit budget on every poll.
    if method == Method::GET && uri.path() == "/v1/models" {
        let resp = models(state, cfg).await;
        record_request(&ctx, resp.status().as_str());
        return resp;
    }

    let wants_stream = parsed
        .as_ref()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false);
    let prefer = parsed
        .as_ref()
        .and_then(|v| affinity(v, state.pool().len()));

    // Fingerprint what the harness asked for (generation endpoints only).
    if ctx.path == "/v1/chat/completions" || ctx.path == "/v1/completions" {
        record_shape(&ctx, parsed.as_ref(), wants_stream);
    }

    // Usage injection: streamed responses only report exact token usage when
    // asked via stream_options, so ask on the client's behalf. `fallback`
    // keeps the untouched body for a one-shot retry if the model rejects it.
    let mut body = body;
    let mut fallback = None;
    if wants_stream && !cfg.strict_passthrough && uri.path() == "/v1/chat/completions" {
        let injectable = parsed
            .as_ref()
            .is_some_and(|v| v.is_object() && v.get("stream_options").is_none())
            && !state.no_inject.lock().unwrap().contains(&ctx.model);
        if injectable {
            // `parsed` is unused after this point, so move it rather than deep-
            // cloning the whole request body (the full conversation) to inject
            // one field.
            let mut v = parsed.take().unwrap();
            v["stream_options"] = serde_json::json!({ "include_usage": true });
            fallback = Some(std::mem::replace(
                &mut body,
                Bytes::from(serde_json::to_vec(&v).expect("serialize injected body")),
            ));
        }
    }

    if wants_stream {
        streaming(
            state, cfg, ctx, method, path_query, headers, body, prefer, fallback,
        )
        .await
    } else {
        buffered(state, cfg, ctx, method, path_query, headers, body, prefer).await
    }
}

/// Sticky-lane hint: hash the conversation's identity (model + the first two
/// messages — typically the system prompt and first user turn, stable across
/// every turn of an agent session) so a conversation keeps hitting the same key
/// while it has capacity, keeping any upstream prefix cache warm. Purely an
/// optimization; correctness never depends on which key serves a request.
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
#[allow(clippy::too_many_arguments)]
async fn buffered(
    state: Arc<AppState>,
    cfg: Arc<Config>,
    ctx: Ctx,
    method: Method,
    path_query: String,
    headers: HeaderMap,
    body: Bytes,
    prefer: Option<usize>,
) -> Response {
    let _active = crate::dispatch::scopeguard(|| gauge!("nimproxy_active_requests").decrement(1.0));
    gauge!("nimproxy_active_requests").increment(1.0);
    let deadline = Instant::now() + cfg.max_wait;
    loop {
        // Two admission gates: a model-pressure permit (worker concurrency,
        // held through the whole upstream exchange — dropped on every exit
        // from this iteration), then an RPM slot.
        let Ok(_permit) = acquire_model_permit(&state, &cfg, &ctx, deadline, || true).await else {
            record_request(&ctx, "504");
            return gateway_timeout(&cfg, state.pool().len());
        };
        let Some(slot) = reserve_slot(&state, cfg.heartbeat, deadline, prefer, || true).await
        else {
            record_request(&ctx, "504");
            return gateway_timeout(&cfg, state.pool().len());
        };
        let sent_at = Instant::now();
        // A non-streaming request gets an overall timeout so a stalled body read
        // can't pin an in-flight slot forever (streaming has no such cap).
        let resp = match upstream_request(
            &state.http,
            &cfg.base_url,
            &method,
            &path_query,
            &headers,
            &slot.key,
            &body,
        )
        .timeout(cfg.request_timeout)
        .send()
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(lane = slot.lane, error = %e, "upstream connection error, retrying");
                bench(&slot, "connect", Duration::from_secs(5));
                continue;
            }
        };
        if retryable(resp.status()) && Instant::now() < deadline {
            let status = resp.status();
            let backoff = backoff_for(&resp);
            // Sniff the error body: worker exhaustion is model-scoped (shared
            // across every key), so benching the lane would just burn healthy
            // key capacity on a failover that cannot help.
            let detail = resp.text().await.unwrap_or_default();
            if governor::is_worker_exhausted(&detail) {
                state
                    .governor
                    .note_exhausted(&ctx.model, cfg.governor.overrides.get(&ctx.model).copied());
                continue; // permit drops here; re-admission waits out the drain
            }
            tracing::info!(lane = slot.lane, %status, ?backoff, "lane benched, retrying");
            bench(&slot, status.as_str(), backoff);
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
    cfg: Arc<Config>,
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
            // Static control frames — no per-send alloc/copy.
            async move { tx.send(Ok(Bytes::from_static(b.as_bytes()))).await.is_ok() }
        };
        if !send(": connected\n\n").await {
            record_request(&ctx, "disconnect");
            return;
        }
        let deadline = Instant::now() + cfg.max_wait;
        loop {
            // Model-pressure permit first (worker concurrency), then an RPM
            // slot — both heartbeating so the harness doesn't hang up. The
            // permit spans the whole upstream exchange and drops on every
            // exit from this iteration.
            let Ok(_permit) = acquire_model_permit(&state, &cfg, &ctx, deadline, || {
                tx.try_send(Ok(Bytes::from_static(b": heartbeat\n\n")))
                    .is_ok()
            })
            .await
            else {
                record_request(&ctx, "504");
                let _ = tx
                    .send(Ok(sse_error(
                        "proxy timed out waiting for an upstream slot",
                    )))
                    .await;
                return;
            };
            let slot = reserve_slot(&state, cfg.heartbeat, deadline, prefer, || {
                tx.try_send(Ok(Bytes::from_static(b": heartbeat\n\n")))
                    .is_ok()
            })
            .await;
            let Some(slot) = slot else {
                record_request(&ctx, "504");
                let _ = tx
                    .send(Ok(sse_error(
                        "proxy timed out waiting for an upstream slot",
                    )))
                    .await;
                return;
            };

            let sent_at = Instant::now();
            let resp = match upstream_request(
                &state.http,
                &cfg.base_url,
                &method,
                &path_query,
                &headers,
                &slot.key,
                &body,
            )
            .send()
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(lane = slot.lane, error = %e, "upstream connection error, retrying");
                    bench(&slot, "connect", Duration::from_secs(5));
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
                let status = resp.status();
                let backoff = backoff_for(&resp);
                // Worker exhaustion is model-scoped: back off the model via
                // the governor, never the lane (see `buffered`).
                let detail = resp.text().await.unwrap_or_default();
                if governor::is_worker_exhausted(&detail) {
                    state.governor.note_exhausted(
                        &ctx.model,
                        cfg.governor.overrides.get(&ctx.model).copied(),
                    );
                } else {
                    tracing::info!(lane = slot.lane, %status, ?backoff, "lane benched, retrying");
                    bench(&slot, status.as_str(), backoff);
                }
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
                let next = if cfg.stream_idle.is_zero() {
                    chunks.next().await
                } else {
                    match tokio::time::timeout(cfg.stream_idle, chunks.next()).await {
                        Ok(n) => n,
                        Err(_) => {
                            tracing::warn!(model = %ctx.model, idle = ?cfg.stream_idle, "upstream stream stalled");
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
            record_quality(
                &ctx,
                scan.finish_reason.as_deref(),
                scan.reasoning,
                scan.tool_call_max.map(|m| m + 1),
            );
            if let (Some(first), Some(c)) = (first_chunk, completion) {
                let gen_secs = first.elapsed().as_secs_f64();
                if gen_secs > 0.1 && c > 0 {
                    histogram!("nimproxy_tokens_per_second", "model" => ctx.model.clone(), "source" => source.to_owned())
                        .record(c as f64 / gen_secs);
                    // Mean inter-token latency (time-per-output-token).
                    histogram!("nimproxy_tpot_seconds", "model" => ctx.model.clone())
                        .record(gen_secs / c as f64);
                }
            }
            // Total upstream time for streaming, for parity with the buffered
            // path (which records upstream_seconds directly).
            histogram!("nimproxy_upstream_seconds", "model" => ctx.model.clone())
                .record(sent_at.elapsed().as_secs_f64());
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
async fn models(state: Arc<AppState>, cfg: Arc<Config>) -> Response {
    let mut cache = state.models_cache.lock().await;
    if let Some((at, body)) = cache.as_ref() {
        if at.elapsed() < cfg.models_ttl {
            return json_response(StatusCode::OK, body.clone());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let Some(slot) = reserve_slot(&state, cfg.heartbeat, deadline, None, || true).await else {
        return gateway_timeout(&cfg, state.pool().len());
    };
    match fetch_models(&state.http, &cfg.base_url, &slot.key).await {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.bytes().await.unwrap_or_default();
            *cache = Some((Instant::now(), body.clone()));
            json_response(StatusCode::OK, body)
        }
        Ok(resp) => {
            if retryable(resp.status()) {
                bench(&slot, resp.status().as_str(), backoff_for(&resp));
            }
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = resp.bytes().await.unwrap_or_default();
            json_response(status, body)
        }
        Err(e) => {
            tracing::warn!(error = %e, "models fetch failed");
            gateway_timeout(&cfg, state.pool().len())
        }
    }
}

/// The raw model-catalog fetch with an explicit key — shared by the cached
/// `/v1/models` path above and the setup wizard's key-validation probe
/// (which must bypass both the pool and the cache).
pub async fn fetch_models(
    http: &reqwest::Client,
    base_url: &str,
    key: &str,
) -> reqwest::Result<reqwest::Response> {
    http.get(format!("{base_url}/v1/models"))
        .bearer_auth(key)
        .send()
        .await
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
    let body = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            // Body stalled past the request timeout, or the connection dropped
            // mid-body. Surface a clear gateway error rather than a truncated
            // "success" with an empty body.
            tracing::warn!(error = %e, "upstream body read failed");
            return bad_gateway();
        }
    };
    if status.is_success() {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) {
            if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                record_tokens(
                    ctx,
                    u.get("prompt_tokens").and_then(|x| x.as_u64()),
                    u.get("completion_tokens").and_then(|x| x.as_u64()),
                    "usage",
                );
            }
            let reasoning = v
                .get("usage")
                .and_then(|u| u.get("completion_tokens_details"))
                .and_then(|d| d.get("reasoning_tokens"))
                .and_then(|x| x.as_u64());
            let choices = v.get("choices").and_then(|c| c.as_array());
            let finish = choices
                .and_then(|c| c.first())
                .and_then(|c| c.get("finish_reason"))
                .and_then(|f| f.as_str());
            let tool_calls = choices.map(|cs| {
                cs.iter()
                    .filter_map(|c| {
                        c.get("message")
                            .and_then(|m| m.get("tool_calls"))
                            .and_then(|t| t.as_array())
                    })
                    .map(|a| a.len() as u64)
                    .sum::<u64>()
            });
            record_quality(ctx, finish, reasoning, tool_calls);
        }
    }
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap()
}

/// The proxy's standard error envelope: `{"error":{message,type,code}}`.
fn proxy_error_json(code: &str, message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "error": { "message": message.into(), "type": "proxy_error", "code": code }
    })
}

fn sse_error(message: &str) -> Bytes {
    let event = proxy_error_json("upstream_unavailable", message);
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
    let body = proxy_error_json(
        "unauthorized",
        "missing or invalid proxy API key (Authorization: Bearer ...)",
    );
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        axum::Json(body),
    )
        .into_response()
}

fn overloaded(max_inflight: usize) -> Response {
    let body = proxy_error_json(
        "overloaded",
        format!("proxy at capacity ({max_inflight} concurrent requests); retry shortly"),
    );
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, "5")],
        axum::Json(body),
    )
        .into_response()
}

fn bad_gateway() -> Response {
    let body = proxy_error_json("bad_gateway", "upstream response failed or timed out");
    (StatusCode::BAD_GATEWAY, axum::Json(body)).into_response()
}

fn gateway_timeout(cfg: &Config, pool_len: usize) -> Response {
    let body = proxy_error_json(
        "rate_limited",
        format!(
            "no upstream slot became available within {}s (all {} keys saturated)",
            cfg.max_wait.as_secs(),
            pool_len
        ),
    );
    (StatusCode::GATEWAY_TIMEOUT, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::{
        bounded_label, count_tools, finish_label, is_json_mode, label_path, sanitize_label,
        tool_choice_mode, SseScan,
    };
    use std::collections::HashSet;

    #[test]
    fn sanitize_strips_injection_chars() {
        // Quotes, braces, angle brackets, newlines, ANSI escapes all removed.
        assert_eq!(sanitize_label("meta/llama-3.3-70b"), "meta/llama-3.3-70b");
        assert_eq!(sanitize_label("a\"} fake_metric 1"), "afake_metric1");
        assert_eq!(
            sanitize_label("<img src=x onerror=alert(1)>"),
            "imgsrcxonerroralert1"
        );
        assert_eq!(sanitize_label("line1\nline2"), "line1line2");
        assert_eq!(sanitize_label("\x1b[31mred"), "31mred");
        assert_eq!(sanitize_label(""), "none");
        assert_eq!(sanitize_label("!!!"), "none");
    }

    #[test]
    fn sanitize_caps_length() {
        let long = "a".repeat(200);
        assert_eq!(sanitize_label(&long).len(), 64);
    }

    #[test]
    fn bounded_label_caps_cardinality() {
        let mut seen = HashSet::new();
        assert_eq!(bounded_label(&mut seen, "m1".into(), 2), "m1");
        assert_eq!(bounded_label(&mut seen, "m2".into(), 2), "m2");
        // Third distinct value exceeds the cap -> "other".
        assert_eq!(bounded_label(&mut seen, "m3".into(), 2), "other");
        // Already-seen values still pass through after the cap.
        assert_eq!(bounded_label(&mut seen, "m1".into(), 2), "m1");
    }

    #[test]
    fn path_label_is_allowlisted() {
        assert_eq!(label_path("/v1/chat/completions"), "/v1/chat/completions");
        assert_eq!(label_path("/v1/embeddings"), "/v1/embeddings");
        assert_eq!(label_path("/v1/anything-else"), "other");
        assert_eq!(label_path("/v1/../etc"), "other");
    }

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

    #[test]
    fn scan_captures_finish_reason_tool_calls_and_reasoning() {
        let mut scan = SseScan::default();
        // A content delta (finish_reason:null — skipped), two tool-call deltas
        // (indices 0 and 1), then a final chunk with finish_reason + usage that
        // carries reasoning-token details.
        scan.feed(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":null}]}\n\n",
        );
        scan.feed(b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"a\"}}]}}]}\n\n");
        scan.feed(b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"name\":\"b\"}}]}}]}\n\n");
        scan.feed(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n");
        scan.feed(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":8,\"completion_tokens_details\":{\"reasoning_tokens\":5}}}\n\ndata: [DONE]\n\n");
        assert_eq!(scan.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(scan.tool_call_max, Some(1)); // +1 => 2 tool calls
        assert_eq!(scan.reasoning, Some(5));
        assert_eq!(scan.completion, Some(8));
    }

    #[test]
    fn finish_label_bounds_unknown_values() {
        assert_eq!(finish_label("stop"), "stop");
        assert_eq!(finish_label("length"), "length");
        assert_eq!(finish_label("tool_calls"), "tool_calls");
        assert_eq!(finish_label("weird\"injection"), "other");
    }

    #[test]
    fn tool_choice_and_shape_readers() {
        let auto = serde_json::json!({"tools": [{}], "tool_choice": "auto"});
        assert_eq!(tool_choice_mode(&auto), "auto");
        let named = serde_json::json!({"tool_choice": {"type": "function"}});
        assert_eq!(tool_choice_mode(&named), "named");
        // tools present, no explicit choice -> provider default (auto)
        assert_eq!(
            tool_choice_mode(&serde_json::json!({"tools": [{}]})),
            "auto"
        );

        assert_eq!(
            count_tools(&serde_json::json!({"tools": [{}, {}, {}]})),
            Some(3)
        );
        assert_eq!(
            count_tools(&serde_json::json!({"functions": [{}]})),
            Some(1)
        );
        assert_eq!(count_tools(&serde_json::json!({"model": "x"})), None);

        assert!(is_json_mode(
            &serde_json::json!({"response_format": {"type": "json_object"}})
        ));
        assert!(!is_json_mode(
            &serde_json::json!({"response_format": {"type": "text"}})
        ));
        assert!(!is_json_mode(&serde_json::json!({"model": "x"})));
    }
}

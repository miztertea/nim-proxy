//! End-to-end tests: the real proxy binary against a scriptable mock NIM.
//!
//! Config now lives in a UI-managed store (DATA_DIR/config.json) rather than
//! env vars: `StoreOpts` writes the fixture, and the dashboard/metrics/history
//! surface always requires auth. See `tests/support/mod.rs` for the harness.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use support::{
    chat_body, complete_setup, expect_refuses_to_start, login, login_as, metrics, read_sse,
    restart, scratch_data_dir, start_mock, start_proxy, start_proxy_fresh, start_proxy_in,
    start_proxy_with, Behavior, StoreOpts, TEST_PASSWORD,
};

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// A client that does NOT follow redirects, so we can assert on 302/303.
fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// A keyed-`/v1` fixture: one client key (name, secret), otherwise defaults.
fn keyed(name: &str, secret: &str) -> StoreOpts {
    StoreOpts {
        open: false,
        clients: vec![(name.into(), secret.into())],
        ..Default::default()
    }
}

#[tokio::test]
async fn open_mode_admits_requests_without_a_client_key() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "hello world");
}

#[tokio::test]
async fn keyed_mode_rejects_bad_tokens_and_accepts_good_ones() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(&mock.url, keyed("alice", "sekrit"), &[]).await;

    let missing = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 401);
    let body: serde_json::Value = missing.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unauthorized");

    let wrong = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth("nope")
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);

    let ok = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth("sekrit")
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    assert_eq!(
        mock.state.hit_count(),
        1,
        "only the authorized call reached upstream"
    );
}

#[tokio::test]
async fn streaming_rides_out_429s_with_lane_failover() {
    let mock = start_mock().await;
    mock.state.push(Behavior::RateLimited(1));
    mock.state.push(Behavior::RateLimited(1));
    let proxy = start_proxy(&mock.url, &[]).await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "SSE committed despite upstream 429s");
    let body = read_sse(resp).await;
    assert!(
        body.contains(": retrying"),
        "client saw retry comments: {body}"
    );
    assert!(body.contains("hello"), "stream delivered data: {body}");
    assert!(body.contains("data: [DONE]"));

    let keys = mock.state.hit_keys();
    assert_eq!(keys.len(), 3, "two 429s then a success");
    assert_ne!(keys[0], keys[1], "429 failed over to a different key");
}

#[tokio::test]
async fn retry_after_is_honored_when_only_one_lane_exists() {
    let mock = start_mock().await;
    mock.state.push(Behavior::RateLimited(1));
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            nim_keys: vec![("only-key".into(), 40)],
            ..Default::default()
        },
        &[],
    )
    .await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(mock.state.hit_count(), 2);
    let gap = mock.state.hit_gap(0, 1);
    assert!(
        gap >= Duration::from_millis(900),
        "waited Retry-After, gap {gap:?}"
    );
}

#[tokio::test]
async fn buffered_retries_5xx_then_returns_verbatim_body() {
    let mock = start_mock().await;
    mock.state.push(Behavior::ServerError(503));
    let proxy = start_proxy(&mock.url, &[]).await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["usage"]["prompt_tokens"], 11);
    assert_eq!(mock.state.hit_count(), 2);
}

#[tokio::test]
async fn non_retryable_error_is_relayed_buffered_and_surfaced_in_stream() {
    let mock = start_mock().await;
    mock.state.push(Behavior::BadRequest);
    // strict_passthrough disables usage injection so a streamed 400 can't be
    // masked by the injection-retry path — it surfaces in-stream instead.
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            strict_passthrough: true,
            ..Default::default()
        },
        &[],
    )
    .await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "buffered 400 relayed verbatim");
    assert!(resp.text().await.unwrap().contains("bad stream_options"));

    mock.state.push(Behavior::BadRequest);
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "stream already committed to 200");
    let body = read_sse(resp).await;
    assert!(
        body.contains("proxy_error"),
        "error surfaced in-stream: {body}"
    );
}

#[tokio::test]
async fn saturation_fails_fast_with_504() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            nim_keys: vec![("only-key".into(), 2)],
            max_wait_secs: 2,
            ..Default::default()
        },
        &[],
    )
    .await;

    for _ in 0..2 {
        let r = client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body("hi", false))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
    let third = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(third.status(), 504, "no slot within max_wait_secs");
    let v: serde_json::Value = third.json().await.unwrap();
    assert_eq!(v["error"]["code"], "rate_limited");
    assert_eq!(
        mock.state.hit_count(),
        2,
        "pacer let exactly the per-key rpm through"
    );
}

#[tokio::test]
async fn conversation_affinity_pins_a_conversation_to_one_key() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    for _ in 0..3 {
        let r = client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body("same conversation", false))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
    let keys = mock.state.hit_keys();
    assert_eq!(keys[0], keys[1]);
    assert_eq!(keys[1], keys[2], "conversation stayed on one key: {keys:?}");

    for i in 0..12 {
        let r = client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body(&format!("distinct conversation {i}"), false))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
    let distinct: std::collections::HashSet<String> = mock.state.hit_keys().into_iter().collect();
    assert!(
        distinct.len() >= 2,
        "distinct conversations spread across keys"
    );
}

#[tokio::test]
async fn models_catalog_is_cached_and_auth_gated() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(&mock.url, keyed("alice", "sekrit"), &[]).await;

    let unauth = client().get(proxy.url("/v1/models")).send().await.unwrap();
    assert_eq!(unauth.status(), 401);

    for _ in 0..3 {
        let r = client()
            .get(proxy.url("/v1/models"))
            .bearer_auth("sekrit")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v["data"][0]["id"], "mock/model-a");
    }
    assert_eq!(
        mock.state
            .models_hits
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "catalog served from cache after first fetch"
    );
}

#[tokio::test]
async fn usage_injection_asks_for_usage_and_backs_off_on_rejection() {
    // Default: stream_options injected.
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    read_sse(resp).await;
    {
        let hits = mock.state.hits.lock().unwrap();
        assert_eq!(
            hits[0].body["stream_options"]["include_usage"], true,
            "proxy injected stream_options"
        );
    }

    // Model that 400s on stream_options: retried untouched, then remembered.
    let mock2 = start_mock().await;
    mock2.state.push(Behavior::BadRequestIfInjected);
    let proxy2 = start_proxy(&mock2.url, &[]).await;
    let resp = client()
        .post(proxy2.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    let body = read_sse(resp).await;
    assert!(body.contains("data: [DONE]"), "recovered after 400: {body}");
    {
        let hits = mock2.state.hits.lock().unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits[0].body.get("stream_options").is_some());
        assert!(hits[1].body.get("stream_options").is_none());
    }
    // Next request for the same model: no injection attempt at all.
    let resp = client()
        .post(proxy2.url("/v1/chat/completions"))
        .json(&chat_body("again", true))
        .send()
        .await
        .unwrap();
    read_sse(resp).await;
    {
        let hits = mock2.state.hits.lock().unwrap();
        assert!(
            hits[2].body.get("stream_options").is_none(),
            "model remembered"
        );
    }

    // strict_passthrough disables injection entirely.
    let mock3 = start_mock().await;
    let proxy3 = start_proxy_with(
        &mock3.url,
        StoreOpts {
            strict_passthrough: true,
            ..Default::default()
        },
        &[],
    )
    .await;
    let resp = client()
        .post(proxy3.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    read_sse(resp).await;
    let hits = mock3.state.hits.lock().unwrap();
    assert!(hits[0].body.get("stream_options").is_none());
}

#[tokio::test]
async fn stalled_upstream_stream_errors_out_within_idle_timeout() {
    let mock = start_mock().await;
    mock.state.push(Behavior::Hang);
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            stream_idle_secs: 1,
            ..Default::default()
        },
        &[],
    )
    .await;

    let started = Instant::now();
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    let body = read_sse(resp).await;
    assert!(body.contains("stalled"), "stall surfaced: {body}");
    assert!(started.elapsed() < Duration::from_secs(10), "did not hang");
}

#[tokio::test]
async fn metrics_report_traffic_tokens_and_affinity() {
    let mock = start_mock().await;
    let proxy = start_proxy_with(&mock.url, keyed("alice", "sekrit"), &[]).await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth("sekrit")
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    read_sse(resp).await;

    let metrics = metrics(&proxy).await;
    assert!(metrics.contains(r#"nimproxy_requests_total{"#), "{metrics}");
    assert!(metrics.contains(r#"client="alice""#));
    assert!(metrics.contains(r#"model="mock/model-a""#));
    assert!(
        metrics.contains(r#"nimproxy_completion_tokens_total{client="alice",model="mock/model-a",source="usage"} 2"#),
        "exact usage counted: {metrics}"
    );
    assert!(metrics.contains("nimproxy_affinity_total"));
}

#[tokio::test]
async fn request_shape_and_quality_metrics_are_recorded() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    // A plain streaming request (finishes "stop").
    read_sse(
        client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body("hi", true))
            .send()
            .await
            .unwrap(),
    )
    .await;

    // A tool-using request with sampling params: the mock answers with a
    // tool_calls delta and finish_reason "tool_calls".
    let tool_req = serde_json::json!({
        "model": "mock/model-a",
        "stream": true,
        "temperature": 0.7,
        "max_tokens": 4096,
        "tools": [{"type": "function", "function": {"name": "get_weather"}}],
        "tool_choice": "auto",
        "messages": [
            {"role": "system", "content": "you are a test"},
            {"role": "user", "content": "weather?"}
        ]
    });
    read_sse(
        client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&tool_req)
            .send()
            .await
            .unwrap(),
    )
    .await;

    let metrics = metrics(&proxy).await;

    // Request shape (labeled by client — open mode admits everyone as "local").
    assert!(
        metrics.contains(r#"nimproxy_stream_requests_total{client="local",stream="true"}"#),
        "stream flag counted: {metrics}"
    );
    assert!(
        metrics.contains(r#"nimproxy_request_messages_count{client="local"}"#),
        "conversation depth histogram present"
    );
    assert!(
        metrics.contains(r#"nimproxy_request_tools_count{client="local"}"#),
        "tools-offered histogram present"
    );
    assert!(
        metrics.contains("nimproxy_request_temperature_count"),
        "temperature histogram present"
    );
    assert!(
        metrics.contains("nimproxy_request_max_tokens_count"),
        "max_tokens histogram present"
    );
    assert!(
        metrics.contains(r#"nimproxy_tool_choice_total{mode="auto"}"#),
        "tool_choice mode counted"
    );

    // Response quality.
    assert!(
        metrics.contains(r#"nimproxy_finish_reason_total{model="mock/model-a",reason="stop"}"#),
        "stop finish recorded: {metrics}"
    );
    assert!(
        metrics
            .contains(r#"nimproxy_finish_reason_total{model="mock/model-a",reason="tool_calls"}"#),
        "tool_calls finish recorded"
    );
    assert!(
        metrics.contains(r#"nimproxy_tool_calls_total{model="mock/model-a"}"#),
        "tool-call volume recorded"
    );
    assert!(
        metrics.contains(r#"nimproxy_reasoning_tokens_total{model="mock/model-a"}"#),
        "reasoning tokens recorded"
    );

    // Cardinality stays bounded: the stream label is a two-value enum.
    for line in metrics
        .lines()
        .filter(|l| l.starts_with("nimproxy_stream_requests_total{"))
    {
        assert!(
            line.contains(r#"stream="true""#) || line.contains(r#"stream="false""#),
            "stream label bounded to true/false: {line}"
        );
    }
}

/// The buffered (non-streaming) path extracts finish_reason, reasoning tokens,
/// and tool-call count from `relay()`; an unknown finish_reason collapses to
/// `other`; JSON mode and non-`auto` tool_choice are recorded. These paths are
/// distinct from the streaming assertions above.
#[tokio::test]
async fn buffered_quality_and_edge_cases_are_recorded() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    let post = |body: serde_json::Value| {
        let proxy = &proxy;
        async move {
            let r = client()
                .post(proxy.url("/v1/chat/completions"))
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            r.text().await.unwrap();
        }
    };

    // Buffered tool call: mock answers with message.tool_calls + finish tool_calls.
    post(serde_json::json!({
        "model": "mock/model-a", "stream": false, "tool_choice": "required",
        "tools": [{"type": "function", "function": {"name": "run"}}],
        "messages": [{"role": "user", "content": "go"}]
    }))
    .await;

    // Buffered JSON mode.
    post(serde_json::json!({
        "model": "mock/model-a", "stream": false,
        "response_format": {"type": "json_object"},
        "messages": [{"role": "user", "content": "as json"}]
    }))
    .await;

    // Unknown upstream finish_reason must collapse to "other".
    mock.state.push(Behavior::OddFinish);
    post(serde_json::json!({
        "model": "mock/model-a", "stream": false,
        "messages": [{"role": "user", "content": "hi"}]
    }))
    .await;

    let metrics = metrics(&proxy).await;

    // Buffered quality extraction (from relay()).
    assert!(
        metrics
            .contains(r#"nimproxy_finish_reason_total{model="mock/model-a",reason="tool_calls"}"#),
        "buffered tool_calls finish recorded: {metrics}"
    );
    assert!(
        metrics.contains(r#"nimproxy_tool_calls_total{model="mock/model-a"}"#),
        "buffered tool-call count recorded"
    );
    assert!(
        metrics.contains(r#"nimproxy_reasoning_tokens_total{model="mock/model-a"}"#),
        "buffered reasoning tokens recorded"
    );
    assert!(
        metrics.contains(r#"nimproxy_upstream_seconds_count{model="mock/model-a"}"#),
        "upstream latency recorded on the buffered path"
    );

    // Edge cases.
    assert!(
        metrics.contains(r#"nimproxy_tool_choice_total{mode="required"}"#),
        "non-auto tool_choice mode recorded"
    );
    assert!(
        metrics.contains(r#"nimproxy_json_mode_total{client="local"}"#),
        "JSON mode recorded"
    );
    assert!(
        metrics.contains(r#"nimproxy_finish_reason_total{model="mock/model-a",reason="other"}"#),
        "unknown finish_reason collapsed to other: {metrics}"
    );
    assert!(
        !metrics.contains(r#"reason="banana""#),
        "raw upstream finish_reason never becomes a label"
    );
}

// ---------- correctness & security hardening (PR 6a) ----------

/// A malformed percent-escape with a multibyte char (`%€`) in the login body
/// must not panic the pre-auth handler (it used to slice a &str on a non-char
/// boundary). The request should come back as a normal failed-login page.
#[tokio::test]
async fn login_handles_malformed_urlencoded_without_panic() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    let resp = client()
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("username=root&password=%a\u{20ac}")
        .send()
        .await
        .unwrap();
    // No panic / connection reset: a clean 401 login page with the error.
    assert_eq!(resp.status(), 401);
    assert!(resp
        .text()
        .await
        .unwrap()
        .contains("Incorrect username or password"));
}

/// Repeated failed logins trip the throttle: a burst past the failure cap
/// returns 429 + Retry-After, even for a subsequently-correct password.
#[tokio::test]
async fn login_throttles_after_repeated_failures() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    // The cap is 10 failures per window; 11 wrong attempts trips it. Every
    // attempt names a real user so the throttle (not a parse path) is what fires.
    for _ in 0..11 {
        let r = client()
            .post(proxy.url("/login"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body("username=root&password=wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401); // wrong password re-renders the form (401)
    }
    // Now throttled: even the correct password is refused with 429 + Retry-After.
    let r = client()
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("username=root&password={TEST_PASSWORD}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
    assert_eq!(r.headers().get("retry-after").unwrap(), "60");
}

/// A buffered request against an upstream that sends headers then stalls the
/// body must not hang forever holding an in-flight slot — the request timeout
/// surfaces a gateway error instead.
#[tokio::test]
async fn buffered_request_times_out_on_hung_upstream() {
    let mock = start_mock().await;
    mock.state.push(Behavior::Hang);
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            request_timeout_secs: 1,
            ..Default::default()
        },
        &[],
    )
    .await;

    let started = Instant::now();
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502, "hung body surfaces as bad_gateway");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "returned promptly, did not hang"
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "bad_gateway");
}

/// Past the in-flight cap the proxy sheds load with 503 instead of growing the
/// queue unbounded.
#[tokio::test]
async fn overloaded_requests_are_shed_with_503() {
    let mock = start_mock().await;
    mock.state.push(Behavior::Hang);
    let proxy = std::sync::Arc::new(
        start_proxy_with(
            &mock.url,
            StoreOpts {
                max_inflight: 1,
                request_timeout_secs: 30,
                ..Default::default()
            },
            &[],
        )
        .await,
    );

    // Occupy the single in-flight slot with a buffered request whose body hangs.
    let hog = {
        let proxy = proxy.clone();
        tokio::spawn(async move {
            let _ = client()
                .post(proxy.url("/v1/chat/completions"))
                .json(&chat_body("hog", false))
                .send()
                .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("shed-me", false))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        503,
        "second request shed at the in-flight cap"
    );
    assert_eq!(resp.headers().get("retry-after").unwrap(), "5");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "overloaded");
    hog.abort();
}

/// An unreachable upstream exercises the connection-error arm: the lane is
/// benched with status "connect" and the request fails fast at the deadline.
#[tokio::test]
async fn upstream_connection_error_is_benched() {
    // Nothing listens on port 1 → every connect attempt fails.
    let proxy = start_proxy_with(
        "http://127.0.0.1:1",
        StoreOpts {
            max_wait_secs: 2,
            ..Default::default()
        },
        &[],
    )
    .await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 504, "connect failures exhaust to a 504");

    let metrics = metrics(&proxy).await;
    assert!(
        metrics.contains(r#"nimproxy_lane_benched_total{lane="0",status="connect"}"#),
        "connection error benched the lane: {metrics}"
    );
}

#[tokio::test]
async fn history_records_snapshots_and_survives_restart() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[("HISTORY_SAMPLE_SECS", "1")]).await;

    // Drive traffic so snapshots have metric series in them.
    let r = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    tokio::time::sleep(Duration::from_millis(2500)).await;

    // Snapshots land on disk at DATA_DIR/history.jsonl (harness-managed dir).
    let jsonl = proxy.data_dir.join("history.jsonl");
    let raw = std::fs::read_to_string(&jsonl).expect("history.jsonl written");
    let lines: Vec<&str> = raw.lines().filter(|l| !l.is_empty()).collect();
    assert!(lines.len() >= 2, "sampler ran: {} snapshots", lines.len());
    assert!(
        lines.last().unwrap().contains("nimproxy"),
        "snapshots carry metrics: {}",
        lines.last().unwrap()
    );
    let before = lines.len();

    // Restart on the SAME data dir: history reloads from disk and is served
    // through the (now auth-gated) /api/history endpoint.
    let proxy = restart(proxy, &[("HISTORY_SAMPLE_SECS", "1")]).await;
    let cookie = login(&proxy).await;
    let points: Vec<serde_json::Value> = client()
        .get(proxy.url("/api/history"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        points.len() >= before,
        "history persisted across restart ({} >= {before})",
        points.len()
    );
}

#[tokio::test]
async fn sigterm_shuts_down_cleanly() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let status = proxy.terminate();
    assert!(status.success(), "clean exit on SIGTERM, got {status:?}");
}

#[tokio::test]
async fn dashboard_and_config_are_served_to_authenticated_users() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let cookie = login(&proxy).await;

    let dash = client()
        .get(proxy.url("/"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(dash.status(), 200);
    assert!(dash.text().await.unwrap().contains("NIM"));

    let cfg: serde_json::Value = client()
        .get(proxy.url("/dash/config.json"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cfg["lanes"], 3);
    assert_eq!(cfg["auth"], false, "open /v1 mode reports auth=false");
}

// ---------- boot posture & the setup wizard ----------

/// With no store, the proxy boots healthy but claimably closed: /v1 answers
/// 503 setup_required, browsers land on /setup, and /setup serves the wizard.
#[tokio::test]
async fn fresh_boot_enters_setup_mode() {
    let proxy = start_proxy_fresh().await;
    let nr = no_redirect_client();

    // Health stays public so orchestrators can probe a not-yet-claimed proxy.
    assert_eq!(
        client()
            .get(proxy.url("/health"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // /v1 is closed until setup completes.
    let api = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(api.status(), 503);
    let body: serde_json::Value = api.json().await.unwrap();
    assert_eq!(body["error"]["code"], "setup_required");

    // Browsers are steered to the wizard, from both the dashboard and /login.
    let dash = nr
        .get(proxy.url("/"))
        .header("accept", "text/html")
        .send()
        .await
        .unwrap();
    assert_eq!(dash.status(), 302);
    assert_eq!(dash.headers()["location"], "/setup");

    let login = nr.get(proxy.url("/login")).send().await.unwrap();
    assert_eq!(login.status(), 302);
    assert_eq!(login.headers()["location"], "/setup");

    let setup = client().get(proxy.url("/setup")).send().await.unwrap();
    assert_eq!(setup.status(), 200);
    assert!(setup.text().await.unwrap().contains("setup"));
}

/// A corrupt or future-version store is a hard boot error, never a silent
/// fall-through to setup mode (which would discard credentials and keys).
#[tokio::test]
async fn corrupt_or_future_store_refuses_to_start() {
    let corrupt = scratch_data_dir();
    std::fs::write(corrupt.join("config.json"), "{ not json").unwrap();
    expect_refuses_to_start(corrupt).await;

    let future = scratch_data_dir();
    std::fs::write(future.join("config.json"), r#"{"version": 2}"#).unwrap();
    expect_refuses_to_start(future).await;
}

/// The wizard's single POST claims the proxy: creates the superuser, writes a
/// 0600 store, mints a session, closes /setup (404), and opens /v1.
#[tokio::test]
async fn setup_wizard_claims_the_proxy() {
    let mock = start_mock().await;
    let proxy = start_proxy_fresh().await;

    complete_setup(
        &proxy,
        "admin",
        "hunter2hunter2",
        &mock.url,
        &[("nvapi-key", 40)],
    )
    .await;

    // Credentials file is owner-only.
    let mode = std::fs::metadata(proxy.data_dir.join("config.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "config store must be 0600");

    // The wizard is gone once the proxy is claimed.
    assert_eq!(
        client()
            .get(proxy.url("/setup"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let post_setup = client()
        .post(proxy.url("/setup"))
        .json(&serde_json::json!({"username": "x", "password": "yyyyyyyyyy"}))
        .send()
        .await
        .unwrap();
    assert_eq!(post_setup.status(), 404, "POST /setup 404 after claim");
    let post_validate = client()
        .post(proxy.url("/setup/validate-key"))
        .json(&serde_json::json!({"key": "k"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        post_validate.status(),
        404,
        "POST /setup/validate-key 404 after claim"
    );

    // The /v1 setup gate has lifted: it no longer answers 503 setup_required.
    // A wizard-created store is keyed (see setup.html: "create client keys in
    // Settings"), so with no client key yet it fails closed with 401.
    let r = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "keyed /v1 with no client key fails closed");
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "unauthorized");
}

/// The claim persists: after a restart on the same data dir, the created user
/// can log in and the setup-provided key is still in the pool.
#[tokio::test]
async fn setup_claim_survives_restart() {
    let mock = start_mock().await;
    let proxy = start_proxy_fresh().await;
    // TEST_PASSWORD so `login_as` (which uses it) works after the restart.
    complete_setup(
        &proxy,
        "admin",
        TEST_PASSWORD,
        &mock.url,
        &[("nvapi-key", 40)],
    )
    .await;

    let proxy = restart(proxy, &[]).await;

    // Session auth works against the persisted user.
    let cookie = login_as(&proxy, "admin").await;
    // The persisted store rehydrated: one lane (the setup key), keyed /v1.
    let cfg: serde_json::Value = client()
        .get(proxy.url("/dash/config.json"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cfg["lanes"], 1, "setup key survived the restart");
    assert_eq!(cfg["auth"], true, "keyed /v1 mode persisted");

    // /v1 is live behind auth (not the pre-setup 503).
    let r = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "keyed /v1 fails closed, no longer 503");
}

/// Lockout recovery: a store whose users were hand-emptied on the volume (its
/// keys left with dangling owners) boots into setup mode; the new superuser
/// adopts the orphan keys, so /v1 works without re-supplying them.
#[tokio::test]
async fn recovery_store_adopts_orphan_keys() {
    let mock = start_mock().await;
    let dir = scratch_data_dir();
    let fixture = serde_json::json!({
        "version": 1,
        "upstream": {
            "base_url": mock.url,
            "nim_keys": [{"key": "orphan-key", "owner": "ghost", "enabled": true, "rpm": 40}],
        },
        // Open /v1 so the test can observe the adopted key reaching upstream
        // (a wizard-created store would be keyed; this recovery store predates it).
        "client_auth": {"mode": "open"},
        "users": [],
    });
    std::fs::write(
        dir.join("config.json"),
        serde_json::to_string_pretty(&fixture).unwrap(),
    )
    .unwrap();

    let proxy = start_proxy_in(dir, &[]).await;
    // No superuser -> setup mode despite the store existing.
    assert_eq!(
        client()
            .get(proxy.url("/setup"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // Claim with an empty key list: the orphan is re-owned by the superuser.
    complete_setup(&proxy, "admin", TEST_PASSWORD, &mock.url, &[]).await;

    let r = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "adopted key serves /v1");
    assert_eq!(mock.state.hit_keys(), vec!["orphan-key".to_owned()]);
}

/// The wizard rejects a password shorter than 10 characters up front.
#[tokio::test]
async fn setup_rejects_weak_password() {
    let proxy = start_proxy_fresh().await;
    let resp = client()
        .post(proxy.url("/setup"))
        .json(&serde_json::json!({
            "username": "admin", "password": "short", "nim_keys": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "weak_password");
}

/// The wizard's pre-auth key probe reports how many models an upstream key can
/// see (the mock exposes exactly one).
#[tokio::test]
async fn setup_validate_key_probes_upstream() {
    let mock = start_mock().await;
    let proxy = start_proxy_fresh().await;
    let resp = client()
        .post(proxy.url("/setup/validate-key"))
        .json(&serde_json::json!({"key": "nvapi-probe", "base_url": mock.url}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["models"], 1, "{body}");
}

// ---------- security hardening ----------

/// Post-setup, the operator surface (dashboard, metrics, history) always
/// requires auth — there is no insecure mode. Health stays public.
#[tokio::test]
async fn operator_surface_always_requires_auth() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    // Health stays public (load balancers / Docker probe).
    assert_eq!(
        client()
            .get(proxy.url("/health"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // Metrics require creds; Bearer <user>:<pass> works (Prometheus scrape path).
    assert_eq!(
        client()
            .get(proxy.url("/metrics"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let ok = client()
        .get(proxy.url("/metrics"))
        .header(
            "authorization",
            format!("Bearer {}:{TEST_PASSWORD}", support::TEST_USER),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);

    // History requires creds too.
    assert_eq!(
        client()
            .get(proxy.url("/api/history"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );

    // Browser hitting the dashboard without a session is redirected to /login.
    let nr = no_redirect_client();
    let redir = nr
        .get(proxy.url("/"))
        .header("accept", "text/html")
        .send()
        .await
        .unwrap();
    assert_eq!(redir.status(), 302);
    assert_eq!(redir.headers()["location"], "/login");
    assert_eq!(
        nr.get(proxy.url("/login")).send().await.unwrap().status(),
        200
    );

    // Wrong password is rejected; correct password sets a hardened session cookie.
    let bad = nr
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("username=root&password=wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);

    let good = nr
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "username={}&password={TEST_PASSWORD}",
            support::TEST_USER
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(good.status(), 303);
    let cookie = good.headers()["set-cookie"].to_str().unwrap().to_owned();
    assert!(cookie.contains("nimproxy_session="));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));

    // The session cookie then opens the dashboard.
    let session = cookie.split(';').next().unwrap();
    let dash = nr
        .get(proxy.url("/"))
        .header("accept", "text/html")
        .header("cookie", session)
        .send()
        .await
        .unwrap();
    assert_eq!(dash.status(), 200);
    assert!(dash.text().await.unwrap().contains("NIM"));
}

#[tokio::test]
async fn model_label_is_sanitized_in_metrics() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;

    // A malicious model id carrying Prometheus/HTML/log injection payloads.
    let evil = "<img src=x onerror=alert(1)>\"} pwn 1\nmeta";
    let body = serde_json::json!({
        "model": evil,
        "messages": [{"role": "user", "content": "hi"}]
    });
    let r = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let metrics = metrics(&proxy).await;
    // The sanitized label keeps only safe chars; none of the injection
    // characters survive, and no spurious `pwn` series was created.
    // The model label value (after `model="`) must contain only safe chars —
    // no `<`, `>`, quote, brace, or newline that could break the exposition
    // format, inject a series, or become HTML. The payload collapses to one
    // harmless alphanumeric token on a single line.
    let req_line = metrics
        .lines()
        .find(|l| l.starts_with("nimproxy_requests_total"))
        .expect("requests_total present");
    let value = req_line
        .split("model=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("model label present");
    assert!(
        value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':')),
        "unsafe chars in model label: {value:?}"
    );
    // No injected standalone series (the `\n... pwn 1` part of the payload).
    assert!(
        !metrics.lines().any(|l| l.trim_start().starts_with("pwn")),
        "injected metric series present"
    );
}

#[tokio::test]
async fn dashboard_sends_security_headers() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    // The dashboard now requires a session; assert the CSP on an authenticated
    // 200 (the hardening headers wrap every response, success or redirect).
    let cookie = login(&proxy).await;
    let resp = client()
        .get(proxy.url("/"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let h = resp.headers();
    let csp = h["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(
        csp.contains("connect-src 'self'"),
        "blocks cross-origin exfil"
    );
    assert!(
        csp.contains("font-src https://fonts.gstatic.com"),
        "dashboard webfonts are allowed, and only from Google's font host"
    );
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["x-frame-options"], "DENY");
}

#[tokio::test]
async fn worker_exhaustion_governs_the_model_and_spares_the_lane() {
    let mock = start_mock().await;
    mock.state.push(Behavior::WorkerExhausted);
    let proxy = start_proxy(&mock.url, &[]).await;

    let started = Instant::now();
    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "hello world");
    assert_eq!(mock.state.hit_count(), 2, "one exhausted try, one success");
    // The retry waited out the governor's ~2s drain gap, not the 10s default
    // lane bench a plain 429-without-Retry-After would have earned.
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "retry took {:?} — looks like a lane bench, not a model drain gap",
        started.elapsed()
    );

    let metrics = metrics(&proxy).await;
    assert!(
        metrics.contains(r#"nimproxy_worker_exhausted_total{model="mock/model-a"} 1"#),
        "exhaustion counted: {metrics}"
    );
    assert!(
        !metrics.contains("nimproxy_lane_benched_total"),
        "worker exhaustion must never bench a lane: {metrics}"
    );
    assert!(
        metrics.contains(r#"nimproxy_model_limit{model="mock/model-a"} 1"#),
        "governor engaged at max(1, inflight/2) = 1: {metrics}"
    );
    assert!(
        metrics.contains(r#"nimproxy_model_inflight{model="mock/model-a"} 0"#),
        "permit released after completion: {metrics}"
    );
}

#[tokio::test]
async fn worker_exhaustion_streaming_retries_inside_the_stream() {
    let mock = start_mock().await;
    mock.state.push(Behavior::WorkerExhausted);
    let proxy = start_proxy(&mock.url, &[]).await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "stream commits to 200 before retrying");
    let body = read_sse(resp).await;
    assert!(body.contains(": retrying"), "retry notice sent: {body}");
    assert!(body.contains("hello"), "content delivered: {body}");
    assert!(body.contains("data: [DONE]"), "stream completed: {body}");

    let metrics = metrics(&proxy).await;
    assert!(
        metrics.contains(r#"nimproxy_worker_exhausted_total{model="mock/model-a"} 1"#),
        "exhaustion counted: {metrics}"
    );
    assert!(
        !metrics.contains("nimproxy_lane_benched_total"),
        "worker exhaustion must never bench a lane: {metrics}"
    );
}

// ---------------------------------------------------------------------------
// Settings API: role filtering, ownership, invariants, live application.
// ---------------------------------------------------------------------------

async fn api_config(proxy: &support::Proxy, cookie: &str) -> serde_json::Value {
    client()
        .get(proxy.url("/api/config"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn post_json(
    proxy: &support::Proxy,
    cookie: &str,
    path: &str,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client()
        .post(proxy.url(path))
        .header("cookie", cookie)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let v = resp.json().await.unwrap_or_default();
    (status, v)
}

#[tokio::test]
async fn api_config_is_filtered_by_role_before_serialization() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        extra_users: vec![("alice".into(), "user".into())],
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;

    // Admin view: server settings, users, and every key row (owner-labeled).
    let root = support::login(&proxy).await;
    let admin_view = api_config(&proxy, &root).await;
    assert_eq!(admin_view["role"], "superuser");
    assert!(admin_view["server"].is_object(), "{admin_view}");
    assert_eq!(admin_view["users"].as_array().unwrap().len(), 2);
    assert_eq!(admin_view["nim_keys"].as_array().unwrap().len(), 3);

    // User view: the raw JSON body simply has no server/users sections and
    // no foreign key rows — CSS tampering can reveal nothing.
    let alice = support::login_as(&proxy, "alice").await;
    let user_view = api_config(&proxy, &alice).await;
    assert_eq!(user_view["role"], "user");
    assert!(user_view.get("server").is_none(), "{user_view}");
    assert!(user_view.get("users").is_none(), "{user_view}");
    assert_eq!(
        user_view["nim_keys"].as_array().unwrap().len(),
        0,
        "alice owns no keys and must not see root's: {user_view}"
    );
    // The pool aggregate stays visible to everyone.
    assert_eq!(user_view["pool"]["enabled"], 3);
}

#[tokio::test]
async fn user_role_is_denied_server_settings_and_foreign_keys() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        extra_users: vec![("alice".into(), "user".into())],
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let root = support::login(&proxy).await;
    let alice = support::login_as(&proxy, "alice").await;

    for (path, body) in [
        (
            "/api/settings/upstream",
            serde_json::json!({"base_url": "http://x"}),
        ),
        ("/api/settings/history", serde_json::json!({"days": 1})),
        (
            "/api/settings/users",
            serde_json::json!({"add": {"username": "eve", "password": "long-enough-pw", "role": "user"}}),
        ),
        ("/api/settings/clients", serde_json::json!({"mode": "open"})),
    ] {
        let (status, v) = post_json(&proxy, &alice, path, body).await;
        assert_eq!(status, 403, "{path} should be admin-only: {v}");
    }

    // Removing / disabling someone else's NIM key is also forbidden.
    let fp = api_config(&proxy, &root).await["nim_keys"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, v) = post_json(
        &proxy,
        &alice,
        "/api/settings/nim-keys",
        serde_json::json!({"remove": fp}),
    )
    .await;
    assert_eq!(status, 403, "{v}");
    let (status, _) = post_json(
        &proxy,
        &alice,
        "/api/settings/nim-keys",
        serde_json::json!({"set": {"fingerprint": fp, "enabled": false}}),
    )
    .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn superuser_is_undeletable_and_the_pool_floor_holds() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        nim_keys: vec![("only-key".into(), 40)],
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let root = support::login(&proxy).await;

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/users",
        serde_json::json!({"remove": support::TEST_USER}),
    )
    .await;
    assert_eq!(status, 403, "superuser must be undeletable: {v}");

    // The superuser's last enabled key is the pool floor: neither removable
    // nor disableable, and the config marks it guarded for the padlock UI.
    let cfg = api_config(&proxy, &root).await;
    assert_eq!(cfg["nim_keys"][0]["guarded"], true, "{cfg}");
    let fp = cfg["nim_keys"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/nim-keys",
        serde_json::json!({"remove": &fp}),
    )
    .await;
    assert_eq!(status, 400, "{v}");
    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/nim-keys",
        serde_json::json!({"set": {"fingerprint": &fp, "enabled": false}}),
    )
    .await;
    assert_eq!(status, 400, "{v}");
}

#[tokio::test]
async fn deleting_a_user_pulls_their_keys_and_kills_their_session() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        extra_users: vec![("alice".into(), "user".into())],
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let root = support::login(&proxy).await;
    let alice = support::login_as(&proxy, "alice").await;

    // Any role may contribute a key to the shared pool.
    let (status, v) = post_json(
        &proxy,
        &alice,
        "/api/settings/nim-keys",
        serde_json::json!({"add": {"key": "alice-key", "rpm": 10}}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(api_config(&proxy, &root).await["pool"]["enabled"], 4);

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/users",
        serde_json::json!({"remove": "alice"}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let cfg = api_config(&proxy, &root).await;
    assert_eq!(
        cfg["pool"]["enabled"], 3,
        "alice's key left the pool: {cfg}"
    );
    assert_eq!(cfg["users"].as_array().unwrap().len(), 1);

    // Her session dies on the next lookup.
    let resp = client()
        .get(proxy.url("/api/config"))
        .header("cookie", &alice)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn client_key_lifecycle_mints_once_and_revokes() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        open: false, // keyed, no keys yet: /v1 rejects everyone
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let root = support::login(&proxy).await;

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/clients",
        serde_json::json!({"add": {"name": "opencode"}}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let secret = v["secret"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("npk_"), "{secret}");

    // The minted secret works on /v1; the stored config never returns it.
    let ok = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth(&secret)
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let cfg = api_config(&proxy, &root).await;
    assert_eq!(cfg["client_keys"][0]["name"], "opencode");
    assert!(
        !serde_json::to_string(&cfg).unwrap().contains(&secret),
        "secret must never be served back"
    );

    // Revoke: the same bearer stops working on the next request.
    let (status, _) = post_json(
        &proxy,
        &root,
        "/api/settings/clients",
        serde_json::json!({"remove": "opencode"}),
    )
    .await;
    assert_eq!(status, 200);
    let denied = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth(&secret)
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401);

    // Flipping to open mode admits keyless clients again (admin-only).
    let (status, _) = post_json(
        &proxy,
        &root,
        "/api/settings/clients",
        serde_json::json!({"mode": "open"}),
    )
    .await;
    assert_eq!(status, 200);
    let open = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(open.status(), 200);
}

#[tokio::test]
async fn rpm_raise_applies_to_the_live_pool_immediately() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        nim_keys: vec![("solo".into(), 1)],
        max_wait_secs: 2,
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let root = support::login(&proxy).await;

    let first = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    let second = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 504, "rpm 1 is spent for the window");

    // Raising the key's rpm rebuilds the pool with carried state — the new
    // headroom serves requests immediately, no restart, no window reset.
    let fp = api_config(&proxy, &root).await["nim_keys"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/nim-keys",
        serde_json::json!({"set": {"fingerprint": fp, "rpm": 5}}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let third = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(third.status(), 200, "raised rpm applies live");
    assert_eq!(mock.state.hit_count(), 2);
}

#[tokio::test]
async fn password_change_requires_current_and_rotates_other_sessions() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let session_a = support::login(&proxy).await;
    let session_b = support::login(&proxy).await;

    let (status, v) = post_json(
        &proxy,
        &session_a,
        "/api/settings/account",
        serde_json::json!({"current_password": "wrong", "new_password": "a-brand-new-pw"}),
    )
    .await;
    assert_eq!(
        status, 403,
        "re-auth is required regardless of session: {v}"
    );

    let resp = client()
        .post(proxy.url("/api/settings/account"))
        .header("cookie", &session_a)
        .json(&serde_json::json!({
            "current_password": support::TEST_PASSWORD,
            "new_password": "a-brand-new-pw",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // The change response re-mints THIS session; every other one dies.
    let fresh = resp.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let alive = client()
        .get(proxy.url("/api/config"))
        .header("cookie", &fresh)
        .send()
        .await
        .unwrap();
    assert_eq!(alive.status(), 200);
    let dead = client()
        .get(proxy.url("/api/config"))
        .header("cookie", &session_b)
        .send()
        .await
        .unwrap();
    assert_eq!(
        dead.status(),
        401,
        "old sessions bind the old password hash"
    );
}

#[tokio::test]
async fn base_url_change_flushes_the_models_cache() {
    let mock_a = start_mock().await;
    let mock_b = start_mock().await;
    let proxy = start_proxy(&mock_a.url, &[]).await;
    let root = support::login(&proxy).await;

    // Prime the (10-minute-TTL) catalog cache from upstream A.
    client()
        .get(proxy.url("/v1/models"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        mock_a
            .state
            .models_hits
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/upstream",
        serde_json::json!({"base_url": mock_b.url}),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    client()
        .get(proxy.url("/v1/models"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        mock_b
            .state
            .models_hits
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "catalog refetches from the new upstream, not the stale cache"
    );
}

#[tokio::test]
async fn admin_cannot_reset_or_takeover_the_superuser() {
    let mock = start_mock().await;
    let opts = support::StoreOpts {
        extra_users: vec![("adm".into(), "admin".into())],
        ..Default::default()
    };
    let proxy = start_proxy_with(&mock.url, opts, &[]).await;
    let adm = support::login_as(&proxy, "adm").await;

    // An admin resetting the superuser's password would be account takeover
    // (the change kills the real superuser's sessions). Must be refused.
    let (status, v) = post_json(
        &proxy,
        &adm,
        "/api/settings/users",
        serde_json::json!({"reset_password": {"username": support::TEST_USER, "new_password": "attacker-chosen-pw"}}),
    )
    .await;
    assert_eq!(
        status, 403,
        "admin must not reset the superuser's password: {v}"
    );

    // The superuser can still log in with the original password afterwards.
    let su = support::login(&proxy).await;
    assert!(!su.is_empty());

    // A normal reset of a peer admin still works.
    let (status, v) = post_json(
        &proxy,
        &su,
        "/api/settings/users",
        serde_json::json!({"reset_password": {"username": "adm", "new_password": "brand-new-admin-pw"}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "resetting a non-superuser must still work: {v}"
    );
}

#[tokio::test]
async fn authenticated_key_validation_ignores_caller_supplied_base_url() {
    // The configured upstream is the mock; a caller-supplied base_url must be
    // ignored so the endpoint can't be turned into an SSRF probe.
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let root = support::login(&proxy).await;

    let (status, v) = post_json(
        &proxy,
        &root,
        "/api/settings/validate-key",
        serde_json::json!({"key": "nvapi-x", "base_url": "http://169.254.169.254"}),
    )
    .await;
    assert_eq!(status, 200);
    // It probed the real (mock) upstream, which answers with model-a — not the
    // attacker's target (which would have errored "cannot reach upstream").
    assert_eq!(
        v["ok"], true,
        "validated against the configured upstream: {v}"
    );
    assert_eq!(v["models"], 1, "{v}");
}

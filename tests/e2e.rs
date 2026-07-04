//! End-to-end tests: the real proxy binary against a scriptable mock NIM.

mod support;

use std::time::{Duration, Instant};

use support::{chat_body, expect_refuses_to_start, read_sse, start_mock, start_proxy, Behavior};

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

#[tokio::test]
async fn local_mode_needs_no_auth() {
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
async fn auth_mode_rejects_bad_tokens_and_accepts_good_ones() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[("PROXY_API_KEYS", "alice:sekrit")]).await;

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
    let proxy = start_proxy(&mock.url, &[("NIM_API_KEYS", "only-key")]).await;

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
    let proxy = start_proxy(&mock.url, &[("STRICT_PASSTHROUGH", "true")]).await;

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
    let proxy = start_proxy(
        &mock.url,
        &[
            ("NIM_API_KEYS", "only-key"),
            ("RPM_PER_KEY", "2"),
            ("MAX_WAIT_SECS", "2"),
        ],
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
    assert_eq!(third.status(), 504, "no slot within MAX_WAIT_SECS");
    let v: serde_json::Value = third.json().await.unwrap();
    assert_eq!(v["error"]["code"], "rate_limited");
    assert_eq!(
        mock.state.hit_count(),
        2,
        "pacer let exactly RPM_PER_KEY through"
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
    let proxy = start_proxy(&mock.url, &[("PROXY_API_KEYS", "alice:sekrit")]).await;

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

    // STRICT_PASSTHROUGH disables injection entirely.
    let mock3 = start_mock().await;
    let proxy3 = start_proxy(&mock3.url, &[("STRICT_PASSTHROUGH", "true")]).await;
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
    let proxy = start_proxy(&mock.url, &[("STREAM_IDLE_SECS", "1")]).await;

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
    let proxy = start_proxy(&mock.url, &[("PROXY_API_KEYS", "alice:sekrit")]).await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .bearer_auth("sekrit")
        .json(&chat_body("hi", true))
        .send()
        .await
        .unwrap();
    read_sse(resp).await;

    let metrics = client()
        .get(proxy.url("/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
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

    let metrics = client()
        .get(proxy.url("/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    // Request shape (labeled by client — harness behavior).
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

    let metrics = client()
        .get(proxy.url("/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

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
/// boundary). The request should come back as a normal "incorrect password".
#[tokio::test]
async fn login_handles_malformed_urlencoded_without_panic() {
    let mock = start_mock().await;
    let proxy = start_proxy(
        &mock.url,
        &[
            ("INSECURE_NO_AUTH", "false"),
            ("ADMIN_PASSWORD", "s3cret"),
            ("PROXY_API_KEYS", "k"),
        ],
    )
    .await;

    let resp = client()
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("password=%a\u{20ac}")
        .send()
        .await
        .unwrap();
    // No panic / connection reset: a clean 401 login page with the error.
    assert_eq!(resp.status(), 401);
    assert!(resp.text().await.unwrap().contains("Incorrect password"));
}

/// Repeated failed logins trip the throttle: a burst past the failure cap
/// returns 429 + Retry-After, even for a subsequently-correct password.
#[tokio::test]
async fn login_throttles_after_repeated_failures() {
    let mock = start_mock().await;
    let proxy = start_proxy(
        &mock.url,
        &[
            ("INSECURE_NO_AUTH", "false"),
            ("ADMIN_PASSWORD", "s3cret"),
            ("PROXY_API_KEYS", "k"),
        ],
    )
    .await;

    // The cap is 10 failures per window; 11 wrong attempts trips it.
    for _ in 0..11 {
        let r = client()
            .post(proxy.url("/login"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body("password=wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401); // wrong password re-renders the form (401)
    }
    // Now throttled: even the correct password is refused with 429 + Retry-After.
    let r = client()
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("password=s3cret")
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
    let proxy = start_proxy(&mock.url, &[("REQUEST_TIMEOUT_SECS", "1")]).await;

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
        start_proxy(
            &mock.url,
            &[("MAX_INFLIGHT", "1"), ("REQUEST_TIMEOUT_SECS", "30")],
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
    let proxy = start_proxy("http://127.0.0.1:1", &[("MAX_WAIT_SECS", "2")]).await;

    let resp = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 504, "connect failures exhaust to a 504");

    let metrics = client()
        .get(proxy.url("/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains(r#"nimproxy_lane_benched_total{lane="0",status="connect"}"#),
        "connection error benched the lane: {metrics}"
    );
}

#[tokio::test]
async fn history_records_snapshots_and_survives_restart() {
    let dir = std::env::temp_dir().join(format!("nimproxy-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mock = start_mock().await;
    let envs = [
        ("DATA_DIR", dir.to_str().unwrap()),
        ("HISTORY_SAMPLE_SECS", "1"),
    ];

    let proxy = start_proxy(&mock.url, &envs).await;
    // Drive traffic so snapshots have metric series in them.
    let r = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("hi", false))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let points: Vec<serde_json::Value> = client()
        .get(proxy.url("/api/history"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(points.len() >= 2, "sampler ran: {} points", points.len());
    let last = points.last().unwrap()["m"].as_str().unwrap();
    assert!(last.contains("nimproxy"), "snapshots carry metrics: {last}");
    let before = points.len();
    drop(proxy);

    // Restart on the same DATA_DIR: history is reloaded from disk.
    let proxy = start_proxy(&mock.url, &envs).await;
    let points: Vec<serde_json::Value> = client()
        .get(proxy.url("/api/history"))
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
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sigterm_shuts_down_cleanly() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let status = proxy.terminate();
    assert!(status.success(), "clean exit on SIGTERM, got {status:?}");
}

#[tokio::test]
async fn dashboard_and_config_are_served() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, &[]).await;
    let dash = client().get(proxy.url("/")).send().await.unwrap();
    assert_eq!(dash.status(), 200);
    assert!(dash.text().await.unwrap().contains("NIM"));
    let cfg: serde_json::Value = client()
        .get(proxy.url("/dash/config.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cfg["lanes"], 3);
    assert_eq!(cfg["auth"], false);
}

// ---------- security hardening ----------

#[tokio::test]
async fn refuses_to_start_without_auth() {
    let mock = start_mock().await;
    // Nothing set: no INSECURE flag, no admin password, no API keys.
    expect_refuses_to_start(&mock.url, &[]).await;
    // Partial config is still refused (only a password, no API key).
    expect_refuses_to_start(&mock.url, &[("ADMIN_PASSWORD", "pw")]).await;
    // Only API keys, no admin password: still refused.
    expect_refuses_to_start(&mock.url, &[("PROXY_API_KEYS", "k")]).await;
}

#[tokio::test]
async fn secure_mode_gates_dashboard_metrics_and_history() {
    let mock = start_mock().await;
    let proxy = start_proxy(
        &mock.url,
        &[
            ("INSECURE_NO_AUTH", "false"),
            ("ADMIN_PASSWORD", "s3cret"),
            ("PROXY_API_KEYS", "api-key-1"),
        ],
    )
    .await;

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

    // Metrics require creds; Bearer <password> works (Prometheus scrape path).
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
        .bearer_auth("s3cret")
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    assert_eq!(
        client()
            .get(proxy.url("/api/history"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client()
            .get(proxy.url("/api/history"))
            .bearer_auth("s3cret")
            .send()
            .await
            .unwrap()
            .status(),
        200
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

    // Wrong password is rejected; correct password sets a session cookie.
    let bad = nr
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("password=wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);

    let good = nr
        .post(proxy.url("/login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("password=s3cret")
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

    // API still requires a valid key.
    assert_eq!(
        client()
            .post(proxy.url("/v1/chat/completions"))
            .json(&chat_body("hi", false))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client()
            .post(proxy.url("/v1/chat/completions"))
            .bearer_auth("api-key-1")
            .json(&chat_body("hi", false))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
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

    let metrics = client()
        .get(proxy.url("/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
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
    let resp = client().get(proxy.url("/")).send().await.unwrap();
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

    let metrics = client()
        .get(proxy.url("/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
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

    let metrics = client()
        .get(proxy.url("/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains(r#"nimproxy_worker_exhausted_total{model="mock/model-a"} 1"#),
        "exhaustion counted: {metrics}"
    );
    assert!(
        !metrics.contains("nimproxy_lane_benched_total"),
        "worker exhaustion must never bench a lane: {metrics}"
    );
}

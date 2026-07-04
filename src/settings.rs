//! Setup & settings handlers — the config store's only writers. Every write
//! runs the same pipeline under the store mutex: build candidate → validate
//! → persist → swap the runtime snapshot → side effects. A failed disk write
//! applies nothing; concurrent saves serialize on the mutex.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::config::{self, NimKey, Role, StoredConfig, User};
use crate::{auth, AppState};

/// Commit a candidate store: validate, persist, swap the runtime config and
/// pool (with rate-state carryover), retune history retention, and only then
/// publish the candidate as the store's truth. Callers hold the store lock
/// (`guard`), which is the save-mutex.
pub fn commit(
    state: &AppState,
    guard: &mut StoredConfig,
    candidate: StoredConfig,
) -> Result<(), String> {
    config::validate(&candidate)?;
    config::save(&state.data_dir, &candidate)
        .map_err(|e| format!("cannot write the config store: {e}"))?;
    *state.cfg.write().unwrap() = Arc::new(candidate.runtime());
    {
        let mut pool = state.pool.write().unwrap();
        *pool = Arc::new(pool.rebuild(candidate.pool_keys()));
    }
    state.history.set_days(candidate.history.days);
    *guard = candidate;
    Ok(())
}

fn json_error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    let body = serde_json::json!({
        "error": { "message": message.into(), "type": "proxy_error", "code": code }
    });
    (status, axum::Json(body)).into_response()
}

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

/// `GET /setup` — the first-run wizard (404 once setup is complete).
pub async fn setup_page(State(state): State<Arc<AppState>>) -> Response {
    if !state.setup_required.load(Ordering::SeqCst) {
        return not_found();
    }
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("setup.html"),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct SetupReq {
    username: String,
    password: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    nim_keys: Vec<SetupKey>,
}

#[derive(Deserialize)]
pub struct SetupKey {
    key: String,
    rpm: Option<usize>,
}

/// `POST /setup` — one atomic claim: create the superuser, record the
/// initial NIM keys, persist, and mint a session. No half-configured server
/// state exists at any point; an abandoned wizard leaves nothing behind.
pub async fn setup_submit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<SetupReq>,
) -> Response {
    if !state.setup_required.load(Ordering::SeqCst) {
        return not_found();
    }
    if req.password.len() < 10 {
        return json_error(
            StatusCode::BAD_REQUEST,
            "weak_password",
            "password must be at least 10 characters",
        );
    }
    let password = req.password.clone();
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .expect("hashing task");

    let result = {
        let mut guard = state.store.lock().unwrap();
        if guard.superuser().is_some() {
            // Two claimers raced; the store lock made the first win whole.
            return json_error(
                StatusCode::CONFLICT,
                "already_configured",
                "setup was just completed by someone else",
            );
        }
        let mut cand = guard.clone();
        cand.users = vec![User {
            username: req.username.clone(),
            password_hash: hash.clone(),
            role: Role::Superuser,
        }];
        // Lockout recovery: keys already in the store belonged to hand-deleted
        // users; the new superuser adopts any orphans, restoring both the
        // ownership rule and the pool-floor invariant.
        for k in &mut cand.upstream.nim_keys {
            if k.owner != req.username {
                k.owner = req.username.clone();
            }
        }
        for c in &mut cand.client_auth.keys {
            if c.owner != req.username {
                c.owner = req.username.clone();
            }
        }
        if let Some(b) = &req.base_url {
            cand.upstream.base_url = b.trim().trim_end_matches('/').to_owned();
        }
        for k in &req.nim_keys {
            cand.upstream.nim_keys.push(NimKey {
                key: k.key.trim().to_owned(),
                owner: req.username.clone(),
                enabled: true,
                rpm: k.rpm.unwrap_or(40),
            });
        }
        commit(&state, &mut guard, cand)
    };
    match result {
        Ok(()) => {
            state.setup_required.store(false, Ordering::SeqCst);
            tracing::info!(user = %req.username, "first-time setup complete; superuser created");
            let cookie = auth::mint_session_cookie(&state, &headers, &req.username, &hash);
            (
                StatusCode::OK,
                [(header::SET_COOKIE, cookie)],
                axum::Json(serde_json::json!({"ok": true})),
            )
                .into_response()
        }
        Err(e) => json_error(StatusCode::BAD_REQUEST, "invalid_config", e),
    }
}

#[derive(Deserialize)]
pub struct ValidateKeyReq {
    key: String,
    #[serde(default)]
    base_url: Option<String>,
}

/// `POST /setup/validate-key` — pre-auth key probe for the wizard, bounded
/// by the shared login throttle plus a fixed delay (probe abuse control).
pub async fn setup_validate_key(
    State(state): State<Arc<AppState>>,
    axum::Json(req): axum::Json<ValidateKeyReq>,
) -> Response {
    if !state.setup_required.load(Ordering::SeqCst) {
        return not_found();
    }
    if state.admin.is_throttled() {
        return json_error(
            StatusCode::TOO_MANY_REQUESTS,
            "throttled",
            "too many validation attempts, try again shortly",
        );
    }
    state.admin.note_failure(); // every pre-auth probe burns throttle budget
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let base = req
        .base_url
        .clone()
        .unwrap_or_else(|| state.store.lock().unwrap().upstream.base_url.clone());
    let base = base.trim().trim_end_matches('/').to_owned();
    axum::Json(match probe_key(&state.http, &base, req.key.trim()).await {
        Ok(models) => serde_json::json!({"ok": true, "models": models}),
        Err(e) => serde_json::json!({"ok": false, "error": e}),
    })
    .into_response()
}

/// Probe a NIM key against an upstream: does `/v1/models` answer for it?
/// Bypasses the pool and the models cache — this is an explicit-key check.
pub async fn probe_key(http: &reqwest::Client, base_url: &str, key: &str) -> Result<usize, String> {
    match crate::proxy::fetch_models(http, base_url, key).await {
        Ok(resp) if resp.status().is_success() => {
            let body = resp
                .bytes()
                .await
                .map_err(|e| format!("upstream sent an unreadable model list: {e}"))?;
            let v: serde_json::Value = serde_json::from_slice(&body)
                .map_err(|e| format!("upstream sent an unreadable model list: {e}"))?;
            Ok(v["data"].as_array().map(|a| a.len()).unwrap_or(0))
        }
        Ok(resp) => Err(format!("upstream rejected the key ({})", resp.status())),
        Err(e) => Err(format!("cannot reach upstream: {e}")),
    }
}

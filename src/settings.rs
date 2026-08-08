//! Setup & settings handlers — the config store's only writers. Every write
//! runs the same pipeline under the store mutex: build candidate → validate
//! → persist → swap the runtime snapshot → side effects. A failed disk write
//! applies nothing; concurrent saves serialize on the mutex.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::{FromRequest, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::de::{self, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use utoipa::ToSchema;

use crate::api::{
    ApiError, ApiJson, ClientKeyRow, ClientsResponse, ConfigResponse, HistorySettings,
    MintedClientKey, NimKeyRow, OkResponse, PoolSummary, ServerSettings, SetupResponse, UserRow,
    ValidateKeyResponse,
};
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
        *pool = Arc::new(pool.rebuild(candidate.pool_specs()));
    }
    if candidate.history.days != guard.history.days {
        state
            .history
            .clone()
            .reconfigure_retention(candidate.history.days, crate::unix_now());
    }
    *guard = candidate;
    state.config_revision.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

fn json_error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (status, axum::Json(ApiError::new(code, message))).into_response()
}

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

fn setup_complete() -> Response {
    json_error(
        StatusCode::CONFLICT,
        "setup_complete",
        "setup is already complete",
    )
}

/// `GET /setup` — the first-run wizard (404 once setup is complete).
pub async fn setup_page(State(state): State<Arc<AppState>>) -> Response {
    if !state.setup_required.load(Ordering::SeqCst) {
        return not_found();
    }
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        crate::presentation::page(crate::presentation::Page::Setup),
    )
        .into_response()
}

#[derive(Deserialize, ToSchema)]
pub struct SetupReq {
    username: String,
    password: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    nim_keys: Vec<SetupKey>,
    /// Mint a first client key with the claim (the wizard sends this by
    /// default) so a fresh keyed-mode proxy can serve /v1 immediately.
    #[serde(default)]
    create_client_key: Option<CreateClientKey>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateClientKey {
    name: String,
}

#[derive(Deserialize, ToSchema)]
pub struct SetupKey {
    key: String,
    rpm: Option<usize>,
}

/// `POST /setup` — one atomic claim: create the superuser, record the
/// initial NIM keys, persist, and mint a session. No half-configured server
/// state exists at any point; an abandoned wizard leaves nothing behind.
#[utoipa::path(
    post,
    path = "/setup",
    tag = "setup",
    request_body = SetupReq,
    security(),
    responses(
        (status = 200, description = "Claimed. Sets the session cookie; `client_key` carries \
            the minted secret, which is never retrievable again.", body = SetupResponse),
        (status = 400, description = "Weak password or a config the store rejects", body = ApiError),
        (status = 409, description = "Setup is already complete or another caller claimed the \
            proxy first", body = ApiError),
    ),
)]
pub async fn setup_submit(State(state): State<Arc<AppState>>, req: Request) -> Response {
    if !state.setup_required.load(Ordering::SeqCst) {
        return setup_complete();
    }
    let headers = req.headers().clone();
    let ApiJson(req) = match ApiJson::<SetupReq>::from_request(req, &state).await {
        Ok(req) => req,
        Err(response) => return response,
    };
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

    let mut minted: Option<(String, String)> = None; // (name, secret)
    let result = {
        let mut guard = state.store.lock().unwrap();
        if guard.superuser().is_some() {
            // Two claimers raced; the store lock made the first win whole.
            return setup_complete();
        }
        let mut cand = guard.clone();
        cand.users = vec![User {
            username: req.username.clone(),
            password_hash: hash.clone(),
            role: Role::Superuser,
            locale: None,
        }];
        // Lockout recovery: keys already in the store belonged to hand-deleted
        // users; the new superuser adopts any orphans, restoring both the
        // ownership rule and the pool-floor invariant.
        for k in &mut cand.upstream.nim_keys {
            if k.owner != req.username {
                k.owner.clone_from(&req.username);
            }
        }
        for c in &mut cand.client_auth.keys {
            if c.owner != req.username {
                c.owner.clone_from(&req.username);
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
        if let Some(ck) = &req.create_client_key {
            let secret = mint_client_secret();
            cand.client_auth.keys.push(ClientKey {
                name: ck.name.trim().to_owned(),
                secret_sha256: auth::sha256_hex(&secret),
                last4: last4(&secret),
                owner: req.username.clone(),
            });
            minted = Some((ck.name.trim().to_owned(), secret));
        }
        commit(&state, &mut guard, cand)
    };
    match result {
        Ok(()) => {
            state.setup_required.store(false, Ordering::SeqCst);
            tracing::info!(user = %req.username, "first-time setup complete; superuser created");
            let cookie = auth::mint_session_cookie(&state, &headers, &req.username, &hash);
            let body = SetupResponse {
                // The one and only time this secret leaves the server.
                client_key: minted.map(|(name, secret)| MintedClientKey { name, secret }),
                ok: true,
            };
            (
                StatusCode::OK,
                [(header::SET_COOKIE, cookie)],
                axum::Json(body),
            )
                .into_response()
        }
        Err(e) => json_error(StatusCode::BAD_REQUEST, "invalid_config", e),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct ValidateKeyReq {
    /// The NIM key to probe.
    key: String,
    /// Only honored by `/setup/validate-key` (pre-claim, no upstream is
    /// configured yet). The authenticated twin ignores it — see
    /// [`validate_key`].
    #[serde(default)]
    base_url: Option<String>,
}

/// `POST /setup/validate-key` — pre-auth key probe for the wizard, bounded
/// by the shared login throttle plus a fixed delay (probe abuse control).
#[utoipa::path(
    post,
    path = "/setup/validate-key",
    tag = "setup",
    request_body = ValidateKeyReq,
    security(),
    responses(
        (status = 200, description = "Probe finished; `ok` says whether the key worked",
            body = ValidateKeyResponse),
        (status = 400, description = "`base_url` is not an allowed upstream", body = ApiError),
        (status = 409, description = "Setup is already complete", body = ApiError),
        (status = 429, description = "Probe throttle (shared with the login throttle)",
            body = ApiError),
    ),
)]
pub async fn setup_validate_key(State(state): State<Arc<AppState>>, req: Request) -> Response {
    if !state.setup_required.load(Ordering::SeqCst) {
        return setup_complete();
    }
    let ApiJson(req) = match ApiJson::<ValidateKeyReq>::from_request(req, &state).await {
        Ok(req) => req,
        Err(response) => return response,
    };
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
    // The wizard's advanced path lets an unauthenticated pre-claim caller
    // supply base_url, so guard it against the link-local metadata range
    // before probing (loopback/RFC1918 stay allowed for self-hosted NIM).
    if let Err(e) = config::check_base_url(&base) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_base_url", e);
    }
    axum::Json(ValidateKeyResponse::probed(
        probe_key(&state.http, &base, req.key.trim()).await,
    ))
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

// ---------------------------------------------------------------------------
// Authenticated settings API. Every handler runs the same shape: resolve the
// caller's role from the live store, check authorization, build a candidate,
// and push it through `commit` (validate → persist → swap → side effects).
// ---------------------------------------------------------------------------

use axum::Extension;

use crate::auth::Identity;
use crate::config::{ClientKey, Mode};

/// First 8 hex chars of SHA-256(key): the stable public identifier for a
/// stored NIM key (the value itself is never sent back to a browser).
fn fingerprint(key: &str) -> String {
    auth::sha256_hex(key)[..8].to_owned()
}

/// A fresh client-key secret: `npk_` + 128 bits of OS randomness. Only the
/// SHA-256 digest is ever stored; the caller shows the secret exactly once.
fn mint_client_secret() -> String {
    let mut raw = [0u8; 16];
    getrandom::getrandom(&mut raw).expect("OS RNG for client secret");
    format!("npk_{}", auth::hex(&raw))
}

fn last4(s: &str) -> String {
    s.chars()
        .skip(s.chars().count().saturating_sub(4))
        .collect()
}

fn forbidden(msg: &str) -> Response {
    json_error(StatusCode::FORBIDDEN, "forbidden", msg)
}

fn bad_request(msg: impl Into<String>) -> Response {
    json_error(StatusCode::BAD_REQUEST, "invalid_config", msg)
}

/// The uniform "it applied" answer every settings write shares.
fn ok_json() -> Response {
    axum::Json(OkResponse::new()).into_response()
}

/// The caller's role, or None if their user was deleted mid-session
/// (answered with [`stale_session`]).
fn role_of(sc: &StoredConfig, username: &str) -> Option<Role> {
    sc.user(username).map(|u| u.role)
}

fn stale_session() -> Response {
    json_error(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "your user no longer exists",
    )
}

/// `GET /api/config` — the Settings page's data source, filtered by role
/// BEFORE serialization: a user-role response simply contains no server
/// settings and no other users' key rows, so DOM tampering reveals nothing.
#[utoipa::path(
    get,
    path = "/api/config",
    tag = "settings",
    responses(
        (status = 200, description = "Settings visible to the caller's role", body = ConfigResponse),
        (status = 401, description = "No session, or the caller's user was deleted",
            body = ApiError),
    ),
)]
pub async fn api_config(
    State(state): State<Arc<AppState>>,
    Extension(Identity(username)): Extension<Identity>,
) -> Response {
    let sc = state.store.lock().unwrap().clone();
    let Some(role) = role_of(&sc, &username) else {
        return stale_session();
    };
    let admin_view = role.is_admin();

    // Live lane state, keyed by the key string (enabled keys only).
    let pool = state.pool();
    let stats: std::collections::HashMap<String, (usize, usize, u64)> = pool
        .lane_stats()
        .into_iter()
        .enumerate()
        .map(|(lane, s)| (s.key.clone(), (lane, s.in_window, s.cooldown_ms)))
        .collect();

    // The padlocked key: the superuser's only enabled key (the pool floor).
    let su = sc
        .superuser()
        .map(|u| u.username.clone())
        .unwrap_or_default();
    let su_enabled: Vec<&str> = sc
        .upstream
        .nim_keys
        .iter()
        .filter(|k| k.enabled && k.owner == su)
        .map(|k| k.key.as_str())
        .collect();
    let guarded_key = (su_enabled.len() == 1).then(|| su_enabled[0].to_owned());

    let nim_keys: Vec<NimKeyRow> = sc
        .upstream
        .nim_keys
        .iter()
        .filter(|k| admin_view || k.owner == username)
        .map(|k| {
            let lane = stats.get(&k.key);
            NimKeyRow {
                cooldown_ms: lane.map(|(_, _, c)| *c),
                enabled: k.enabled,
                fingerprint: fingerprint(&k.key),
                guarded: guarded_key.as_deref() == Some(k.key.as_str()),
                in_window: lane.map(|(_, w, _)| *w),
                lane: lane.map(|(i, _, _)| *i),
                last4: last4(&k.key),
                owner: k.owner.clone(),
                rpm: k.rpm,
            }
        })
        .collect();

    let client_keys: Vec<ClientKeyRow> = sc
        .client_auth
        .keys
        .iter()
        .filter(|c| admin_view || c.owner == username)
        .map(|c| ClientKeyRow {
            last4: c.last4.clone(),
            name: c.name.clone(),
            owner: c.owner.clone(),
        })
        .collect();

    // Role filtering happens HERE, by building or not building the section —
    // never by omitting it later or by trusting the client to hide it.
    let (server, users) = if admin_view {
        let history = state.history.status();
        let server = ServerSettings {
            base_url: sc.upstream.base_url.clone(),
            dashboard: sc.dashboard.clone(),
            default_locale: sc.default_locale.clone(),
            governor: sc.governor.clone(),
            history: HistorySettings {
                available_from: history.available_from,
                compaction_pending: history.compaction_pending,
                days: sc.history.days,
                file_bytes: history.file_bytes,
                persistence: history.persistence,
            },
            limits: sc.limits.clone(),
        };
        let users = sc
            .users
            .iter()
            .map(|u| UserRow {
                client_keys: sc
                    .client_auth
                    .keys
                    .iter()
                    .filter(|c| c.owner == u.username)
                    .count(),
                nim_keys: sc
                    .upstream
                    .nim_keys
                    .iter()
                    .filter(|k| k.owner == u.username)
                    .count(),
                role: u.role,
                username: u.username.clone(),
            })
            .collect();
        (Some(server), Some(users))
    } else {
        (None, None)
    };

    axum::Json(ConfigResponse {
        client_keys,
        locale: sc.user(&username).and_then(|user| user.locale.clone()),
        mode: sc.client_auth.mode,
        nim_keys,
        pool: PoolSummary {
            capacity_rpm: pool.capacity_rpm(),
            enabled: pool.len(),
        },
        role,
        server,
        username,
        users,
    })
    .into_response()
}

/// Exactly one of `add` / `remove` / `set` per request.
#[derive(Deserialize, ToSchema)]
pub struct NimKeysReq {
    add: Option<AddNimKey>,
    /// Fingerprint of the key to remove.
    remove: Option<String>,
    set: Option<SetNimKey>,
}

#[derive(Deserialize, ToSchema)]
pub struct AddNimKey {
    key: String,
    /// Requests per minute for this key's lane; defaults to 40 (NIM's free
    /// tier).
    rpm: Option<usize>,
}

#[derive(Deserialize, ToSchema)]
pub struct SetNimKey {
    fingerprint: String,
    enabled: Option<bool>,
    rpm: Option<usize>,
}

/// `POST /api/settings/nim-keys` — any role may add keys (owner = caller)
/// and manage their OWN keys; admins manage any key. The superuser's last
/// enabled key is protected by `validate` (the pool-floor invariant).
#[utoipa::path(
    post,
    path = "/api/settings/nim-keys",
    tag = "settings",
    request_body = NimKeysReq,
    responses(
        (status = 200, description = "Applied; the pool was rebuilt with rate-state carryover",
            body = OkResponse),
        (status = 400, description = "No such key, more than one action, or a store the \
            validator rejects (e.g. removing the pool floor)", body = ApiError),
        (status = 401, description = "No session, or the caller's user was deleted", body = ApiError),
        (status = 403, description = "Not the key's owner and not an admin", body = ApiError),
    ),
)]
pub async fn nim_keys(
    State(state): State<Arc<AppState>>,
    Extension(Identity(username)): Extension<Identity>,
    ApiJson(req): ApiJson<NimKeysReq>,
) -> Response {
    let mut guard = state.store.lock().unwrap();
    let Some(role) = role_of(&guard, &username) else {
        return stale_session();
    };
    let mut cand = guard.clone();
    match (req.add, req.remove, req.set) {
        (Some(add), None, None) => {
            cand.upstream.nim_keys.push(NimKey {
                key: add.key.trim().to_owned(),
                owner: username,
                enabled: true,
                rpm: add.rpm.unwrap_or(40),
            });
        }
        (None, Some(fp), None) => {
            let Some(pos) = cand
                .upstream
                .nim_keys
                .iter()
                .position(|k| fingerprint(&k.key) == fp)
            else {
                return bad_request("no such key");
            };
            if !role.is_admin() && cand.upstream.nim_keys[pos].owner != username {
                return forbidden("you can only remove your own keys");
            }
            cand.upstream.nim_keys.remove(pos);
        }
        (None, None, Some(set)) => {
            let Some(k) = cand
                .upstream
                .nim_keys
                .iter_mut()
                .find(|k| fingerprint(&k.key) == set.fingerprint)
            else {
                return bad_request("no such key");
            };
            if !role.is_admin() && k.owner != username {
                return forbidden("you can only change your own keys");
            }
            if let Some(e) = set.enabled {
                k.enabled = e;
            }
            if let Some(rpm) = set.rpm {
                k.rpm = rpm;
            }
        }
        _ => return bad_request("send exactly one of add / remove / set"),
    }
    match commit(&state, &mut guard, cand) {
        Ok(()) => ok_json(),
        Err(e) => bad_request(e),
    }
}

/// Exactly one of `add` / `remove` / `mode` per request.
#[derive(Deserialize, ToSchema)]
pub struct ClientsReq {
    add: Option<AddClient>,
    /// Name of the client key to revoke.
    remove: Option<String>,
    /// `open` or `keyed` — whether `/v1` requires a client key (admin only).
    mode: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct AddClient {
    name: String,
}

/// `POST /api/settings/clients` — client-key create/revoke for any role
/// (own keys; admins revoke any); the open/keyed mode toggle is admin-only.
/// A created secret is returned exactly once and stored only as a digest.
#[utoipa::path(
    post,
    path = "/api/settings/clients",
    tag = "settings",
    request_body = ClientsReq,
    responses(
        (status = 200, description = "Applied. On `add`, `secret` is the plaintext key — the \
            only time it is ever served.", body = ClientsResponse),
        (status = 400, description = "No such client key, more than one action, or an \
            unknown mode", body = ApiError),
        (status = 401, description = "No session, or the caller's user was deleted", body = ApiError),
        (status = 403, description = "Revoking someone else's key, or a non-admin changing \
            the mode", body = ApiError),
    ),
)]
pub async fn clients(
    State(state): State<Arc<AppState>>,
    Extension(Identity(username)): Extension<Identity>,
    ApiJson(req): ApiJson<ClientsReq>,
) -> Response {
    let mut guard = state.store.lock().unwrap();
    let Some(role) = role_of(&guard, &username) else {
        return stale_session();
    };
    let mut cand = guard.clone();
    let mut minted: Option<String> = None;
    match (req.add, req.remove, req.mode) {
        (Some(add), None, None) => {
            let secret = mint_client_secret();
            cand.client_auth.keys.push(ClientKey {
                name: add.name.trim().to_owned(),
                secret_sha256: auth::sha256_hex(&secret),
                last4: last4(&secret),
                owner: username,
            });
            minted = Some(secret);
        }
        (None, Some(name), None) => {
            let Some(pos) = cand.client_auth.keys.iter().position(|c| c.name == name) else {
                return bad_request("no such client key");
            };
            if !role.is_admin() && cand.client_auth.keys[pos].owner != username {
                return forbidden("you can only revoke your own client keys");
            }
            cand.client_auth.keys.remove(pos);
        }
        (None, None, Some(mode)) => {
            if !role.is_admin() {
                return forbidden("changing the API access mode requires an admin");
            }
            cand.client_auth.mode = match mode.as_str() {
                "open" => Mode::Open,
                "keyed" => Mode::Keyed,
                _ => return bad_request("mode must be \"open\" or \"keyed\""),
            };
        }
        _ => return bad_request("send exactly one of add / remove / mode"),
    }
    match commit(&state, &mut guard, cand) {
        Ok(()) => axum::Json(ClientsResponse {
            ok: true,
            secret: minted,
        })
        .into_response(),
        Err(e) => bad_request(e),
    }
}

/// Admin-only settings sections share one skeleton: role check, mutate the
/// candidate, commit. The OpenAPI operation is generated from the same
/// invocation, so a section can't exist without being documented.
macro_rules! admin_section {
    ($fn_name:ident, $req:ident, $path:literal, $doc:literal, $apply:expr) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = $path,
            tag = "settings",
            request_body = $req,
            responses(
                (status = 200, description = "Applied, persisted, and swapped into the live \
                    runtime config", body = OkResponse),
                (status = 400, description = "The store validator rejected the result",
                    body = ApiError),
                (status = 401, description = "No session, or the caller's user was deleted",
                    body = ApiError),
                (status = 403, description = "Server settings require an admin", body = ApiError),
                (status = 422, description = "A field is missing or the wrong type — a partial \
                    body is never a silent reset", body = ApiError),
            ),
        )]
        pub async fn $fn_name(
            State(state): State<Arc<AppState>>,
            Extension(Identity(username)): Extension<Identity>,
            ApiJson(req): ApiJson<$req>,
        ) -> Response {
            let mut guard = state.store.lock().unwrap();
            match role_of(&guard, &username) {
                Some(r) if r.is_admin() => {}
                Some(_) => return forbidden("server settings require an admin"),
                None => return stale_session(),
            }
            let mut cand = guard.clone();
            #[allow(clippy::redundant_closure_call)]
            ($apply)(&mut cand, req);
            match commit(&state, &mut guard, cand) {
                Ok(()) => ok_json(),
                Err(e) => bad_request(e),
            }
        }
    };
}

#[derive(Deserialize, ToSchema)]
pub struct UpstreamReq {
    /// `http(s)://host[:port]`; link-local addresses are refused (cloud
    /// metadata endpoints have no legitimate NIM use).
    base_url: String,
}

/// `POST /api/settings/upstream` (admin) — also flushes the model-catalog
/// cache and the per-model no-inject memory, which are upstream-specific.
#[utoipa::path(
    post,
    path = "/api/settings/upstream",
    tag = "settings",
    request_body = UpstreamReq,
    responses(
        (status = 200, description = "Applied; the model catalog cache and the per-model \
            no-inject memory were flushed", body = OkResponse),
        (status = 400, description = "Not an http(s) URL, or a link-local address", body = ApiError),
        (status = 401, description = "No session, or the caller's user was deleted", body = ApiError),
        (status = 403, description = "Server settings require an admin", body = ApiError),
    ),
)]
pub async fn upstream(
    State(state): State<Arc<AppState>>,
    Extension(Identity(username)): Extension<Identity>,
    ApiJson(req): ApiJson<UpstreamReq>,
) -> Response {
    let result = {
        let mut guard = state.store.lock().unwrap();
        match role_of(&guard, &username) {
            Some(r) if r.is_admin() => {}
            Some(_) => return forbidden("server settings require an admin"),
            None => return stale_session(),
        }
        let mut cand = guard.clone();
        cand.upstream.base_url = req.base_url.trim().trim_end_matches('/').to_owned();
        commit(&state, &mut guard, cand)
    };
    match result {
        Ok(()) => {
            *state.models_cache.lock().await = None;
            state.no_inject.lock().unwrap().clear();
            ok_json()
        }
        Err(e) => bad_request(e),
    }
}

/// Mirror of `config::Limits` WITHOUT serde defaults: a partial body is a
/// 422, never a silent reset of the omitted fields.
#[derive(Deserialize, ToSchema)]
pub struct LimitsReq {
    max_wait_secs: u64,
    heartbeat_secs: u64,
    models_ttl_secs: u64,
    stream_idle_secs: u64,
    request_timeout_secs: u64,
    max_inflight: usize,
    strict_passthrough: bool,
}

admin_section!(
    limits,
    LimitsReq,
    "/api/settings/limits",
    "`POST /api/settings/limits` (admin) — patience, timeouts and the \
     in-flight cap. Every field is required.",
    |cand: &mut StoredConfig, req: LimitsReq| {
        cand.limits = crate::config::Limits {
            heartbeat_secs: req.heartbeat_secs,
            max_inflight: req.max_inflight,
            max_wait_secs: req.max_wait_secs,
            models_ttl_secs: req.models_ttl_secs,
            request_timeout_secs: req.request_timeout_secs,
            stream_idle_secs: req.stream_idle_secs,
            strict_passthrough: req.strict_passthrough,
        };
    }
);

#[derive(Deserialize, ToSchema)]
pub struct HistoryReq {
    /// Retention in days; 0 = keep forever. Must be 0 or >= the default
    /// window.
    days: u64,
    default_window_days: u64,
    slo_target_percent: f64,
}

admin_section!(
    history,
    HistoryReq,
    "/api/settings/history",
    "`POST /api/settings/history` (admin) — retention plus the dashboard's \
     default window and SLO target. Shrinking retention prunes on disk.",
    |cand: &mut StoredConfig, req: HistoryReq| {
        cand.history.days = req.days;
        cand.dashboard.default_window_days = req.default_window_days;
        cand.dashboard.slo_target_percent = req.slo_target_percent;
    }
);

#[derive(Deserialize, ToSchema)]
pub struct GovernorReq {
    enabled: Option<bool>,
    set_override: Option<GovernorOverride>,
    /// Model id whose pinned cap to drop.
    remove_override: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct GovernorOverride {
    model: String,
    /// Max in-flight requests for this model, 1-10000.
    cap: usize,
}

admin_section!(
    governor_cfg,
    GovernorReq,
    "/api/settings/governor",
    "`POST /api/settings/governor` (admin) — the per-model concurrency gate: \
     the adaptive switch and operator-pinned caps. Fields are independent; \
     omitted ones are left alone.",
    |cand: &mut StoredConfig, req: GovernorReq| {
        if let Some(e) = req.enabled {
            cand.governor.enabled = e;
        }
        if let Some(o) = req.set_override {
            cand.governor.overrides.insert(o.model, o.cap);
        }
        if let Some(m) = req.remove_override {
            cand.governor.overrides.remove(&m);
        }
    }
);

/// Exactly one of `add` / `remove` / `reset_password` / `set_role`.
#[derive(Deserialize, ToSchema)]
pub struct UsersReq {
    add: Option<AddUser>,
    /// Username to delete. Their NIM keys leave the pool and their client
    /// keys are revoked.
    remove: Option<String>,
    reset_password: Option<ResetPassword>,
    set_role: Option<SetRole>,
}

#[derive(Deserialize, ToSchema)]
pub struct AddUser {
    username: String,
    /// At least 10 characters.
    password: String,
    /// `admin` or `user` — `superuser` is never assignable.
    role: String,
}

#[derive(Deserialize, ToSchema)]
pub struct ResetPassword {
    username: String,
    new_password: String,
}

#[derive(Deserialize, ToSchema)]
pub struct SetRole {
    username: String,
    /// `admin` or `user`.
    role: String,
}

fn parse_role(s: &str) -> Option<Role> {
    match s {
        "admin" => Some(Role::Admin),
        "user" => Some(Role::User),
        _ => None, // superuser is never assignable
    }
}

/// `POST /api/settings/users` (admin) — create/delete users, reset
/// passwords, change roles. Deleting a user pulls their NIM keys from the
/// pool and revokes their client keys — their harnesses stop; that's the
/// point. The superuser can never be deleted or demoted.
#[utoipa::path(
    post,
    path = "/api/settings/users",
    tag = "settings",
    request_body = UsersReq,
    responses(
        (status = 200, description = "Applied", body = OkResponse),
        (status = 400, description = "Weak password, unknown user, bad role, or more than \
            one action", body = ApiError),
        (status = 401, description = "No session, or the caller's user was deleted", body = ApiError),
        (status = 403, description = "Not an admin, or a write against the superuser \
            (undeletable, immutable role, password only via /account)", body = ApiError),
    ),
)]
pub async fn users(
    State(state): State<Arc<AppState>>,
    Extension(Identity(username)): Extension<Identity>,
    ApiJson(req): ApiJson<UsersReq>,
) -> Response {
    // Hash outside the store lock (PBKDF2 is deliberately slow).
    let new_hash = match (&req.add, &req.reset_password) {
        (Some(a), None) => {
            if a.password.len() < 10 {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "weak_password",
                    "password must be at least 10 characters",
                );
            }
            let p = a.password.clone();
            Some(
                tokio::task::spawn_blocking(move || auth::hash_password(&p))
                    .await
                    .expect("hashing task"),
            )
        }
        (None, Some(r)) => {
            if r.new_password.len() < 10 {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "weak_password",
                    "password must be at least 10 characters",
                );
            }
            let p = r.new_password.clone();
            Some(
                tokio::task::spawn_blocking(move || auth::hash_password(&p))
                    .await
                    .expect("hashing task"),
            )
        }
        _ => None,
    };

    let mut guard = state.store.lock().unwrap();
    match role_of(&guard, &username) {
        Some(r) if r.is_admin() => {}
        Some(_) => return forbidden("user management requires an admin"),
        None => return stale_session(),
    }
    let mut cand = guard.clone();
    match (req.add, req.remove, req.reset_password, req.set_role) {
        (Some(add), None, None, None) => {
            let Some(role) = parse_role(&add.role) else {
                return bad_request("role must be \"admin\" or \"user\"");
            };
            cand.users.push(User {
                username: add.username.trim().to_owned(),
                password_hash: new_hash.expect("hashed above"),
                role,
                locale: None,
            });
        }
        (None, Some(target), None, None) => {
            let Some(user) = cand.user(&target) else {
                return bad_request("no such user");
            };
            if user.role == Role::Superuser {
                return forbidden("the superuser can never be deleted");
            }
            cand.users.retain(|u| u.username != target);
            cand.upstream.nim_keys.retain(|k| k.owner != target);
            cand.client_auth.keys.retain(|c| c.owner != target);
        }
        (None, None, Some(reset), None) => {
            let Some(user) = cand.users.iter_mut().find(|u| u.username == reset.username) else {
                return bad_request("no such user");
            };
            // The superuser is inviolable: an admin resetting it would be
            // account takeover + lockout (the change invalidates the real
            // owner's sessions). The superuser rotates its own password via
            // /account (current-password re-auth); a forgotten one is the
            // documented volume-edit recovery.
            if user.role == Role::Superuser {
                return forbidden("the superuser's password can only be changed by the superuser (Account settings)");
            }
            user.password_hash = new_hash.expect("hashed above");
        }
        (None, None, None, Some(set)) => {
            let Some(role) = parse_role(&set.role) else {
                return bad_request("role must be \"admin\" or \"user\"");
            };
            let Some(user) = cand.users.iter_mut().find(|u| u.username == set.username) else {
                return bad_request("no such user");
            };
            if user.role == Role::Superuser {
                return forbidden("the superuser's role is immutable");
            }
            user.role = role;
        }
        _ => return bad_request("send exactly one of add / remove / reset_password / set_role"),
    }
    match commit(&state, &mut guard, cand) {
        Ok(()) => {
            // Header-credential memo may hold a deleted/reset user.
            state.admin.clear_scraper_memo();
            ok_json()
        }
        Err(e) => bad_request(e),
    }
}

pub(crate) struct AccountPasswordReq {
    current_password: String,
    /// At least 10 characters.
    new_password: String,
}

pub(crate) enum AccountBody {
    Password(AccountPasswordReq),
    Locale(Option<String>),
    InvalidLocale,
    InvalidAction,
}

impl<'de> Deserialize<'de> for AccountBody {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AccountVisitor;

        impl<'de> Visitor<'de> for AccountVisitor {
            type Value = AccountBody;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a password change or locale preference object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut current_password = None;
                let mut new_password = None;
                let mut action = None;
                let mut locale = None;
                let mut current_password_seen = false;
                let mut new_password_seen = false;
                let mut action_seen = false;
                let mut locale_seen = false;
                let mut unknown_seen = false;

                while let Some(field) = map.next_key::<String>()? {
                    match field.as_str() {
                        "current_password" => {
                            if current_password_seen {
                                return Err(de::Error::duplicate_field("current_password"));
                            }
                            current_password_seen = true;
                            current_password = Some(map.next_value()?);
                        }
                        "new_password" => {
                            if new_password_seen {
                                return Err(de::Error::duplicate_field("new_password"));
                            }
                            new_password_seen = true;
                            new_password = Some(map.next_value()?);
                        }
                        "action" => {
                            if action_seen {
                                return Err(de::Error::duplicate_field("action"));
                            }
                            action_seen = true;
                            action = Some(map.next_value::<serde_json::Value>()?);
                        }
                        "locale" => {
                            if locale_seen {
                                return Err(de::Error::duplicate_field("locale"));
                            }
                            locale_seen = true;
                            locale = Some(map.next_value::<serde_json::Value>()?);
                        }
                        _ => {
                            unknown_seen = true;
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }

                if current_password_seen || new_password_seen {
                    return Ok(AccountBody::Password(AccountPasswordReq {
                        current_password: current_password
                            .ok_or_else(|| de::Error::missing_field("current_password"))?,
                        new_password: new_password
                            .ok_or_else(|| de::Error::missing_field("new_password"))?,
                    }));
                }
                if action_seen || locale_seen {
                    if unknown_seen
                        || !action_seen
                        || !locale_seen
                        || action.as_ref().and_then(serde_json::Value::as_str) != Some("locale")
                    {
                        return Ok(AccountBody::InvalidAction);
                    }
                    return match locale.expect("locale field checked above") {
                        serde_json::Value::Null => Ok(AccountBody::Locale(None)),
                        serde_json::Value::String(locale) => Ok(AccountBody::Locale(Some(locale))),
                        _ => Ok(AccountBody::InvalidLocale),
                    };
                }
                Err(de::Error::missing_field("current_password"))
            }
        }

        deserializer.deserialize_map(AccountVisitor)
    }
}

/// OpenAPI union for the unchanged password body and the locale set/clear
/// action. utoipa 5.5 collapses an untagged mixed enum to its first variant,
/// so this small schema-only type spells the two native JSON object shapes.
pub struct AccountReq;

impl utoipa::PartialSchema for AccountReq {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{AdditionalProperties, Object, OneOf, SchemaType, Type};

        let string = || Object::builder().schema_type(Type::String);
        let password = Object::builder()
            .property("current_password", string())
            .required("current_password")
            .property(
                "new_password",
                string().description(Some("At least 10 characters.")),
            )
            .required("new_password");
        let action = string().enum_values(Some(["locale"]));
        let nullable_locale =
            Object::builder().schema_type(SchemaType::from_iter([Type::String, Type::Null]));
        let locale = Object::builder()
            .property("action", action)
            .required("action")
            .property("locale", nullable_locale)
            .required("locale")
            .additional_properties(Some(AdditionalProperties::FreeForm(false)));

        OneOf::builder().item(password).item(locale).into()
    }
}

impl ToSchema for AccountReq {}

/// Why an own-password change could not be applied to the candidate store.
#[derive(Debug, PartialEq)]
enum PasswordChangeErr {
    UserGone,
    HashRotated,
}

/// Apply an own-password change, refusing unless the stored hash is still
/// the one the caller's current password was verified against. The verify
/// runs outside the store lock (PBKDF2 is deliberately slow), so a hash
/// rotation — an admin reset landing in that window — must win, not be
/// silently overwritten by a change authorized against the old password.
fn apply_password_change(
    cand: &mut StoredConfig,
    username: &str,
    verified_hash: &str,
    new_hash: &str,
) -> Result<(), PasswordChangeErr> {
    let Some(user) = cand.users.iter_mut().find(|u| u.username == username) else {
        return Err(PasswordChangeErr::UserGone);
    };
    if user.password_hash != verified_hash {
        return Err(PasswordChangeErr::HashRotated);
    }
    user.password_hash = new_hash.to_owned();
    Ok(())
}

/// `POST /api/settings/account` — change the caller's password or locale preference.
/// Password changes always re-verify the current password and return a fresh
/// cookie; locale changes preserve the existing session.
#[utoipa::path(
    post,
    path = "/api/settings/account",
    tag = "settings",
    request_body = AccountReq,
    responses(
        (status = 200, description = "Password changed with a fresh session cookie and other \
            sessions invalidated, or locale preference applied without changing the session.",
            body = OkResponse),
        (status = 400, description = "Request failed with weak_password, invalid_action, \
            invalid_locale, locale_not_installed, or invalid_config", body = ApiError),
        (status = 401, description = "No session, or the caller's user was deleted", body = ApiError),
        (status = 403, description = "The current password is wrong", body = ApiError),
        (status = 409, description = "An admin reset the password while this request was in \
            flight; the reset wins", body = ApiError),
        (status = 422, description = "Invalid JSON request body", body = ApiError),
    ),
)]
pub async fn account(
    State(state): State<Arc<AppState>>,
    Extension(Identity(username)): Extension<Identity>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<AccountBody>,
) -> Response {
    let req = match req {
        AccountBody::InvalidAction => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_action",
                "account action must be locale",
            )
        }
        AccountBody::InvalidLocale => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_locale",
                "locale is not valid",
            )
        }
        AccountBody::Locale(locale) => {
            let locale = match locale {
                None => None,
                Some(locale) => match crate::presentation::installed_locale(&locale) {
                    Ok(locale) => Some(locale),
                    Err(error) => return locale_error_response(error),
                },
            };
            let result = {
                let mut guard = state.store.lock().unwrap();
                let Some(user) = guard.users.iter().find(|user| user.username == username) else {
                    return stale_session();
                };
                let mut cand = guard.clone();
                cand.user_mut(&user.username)
                    .expect("candidate cloned from store")
                    .locale = locale;
                commit(&state, &mut guard, cand)
            };
            return match result {
                Ok(()) => ok_json(),
                Err(e) => bad_request(e),
            };
        }
        AccountBody::Password(req) => req,
    };
    if req.new_password.len() < 10 {
        return json_error(
            StatusCode::BAD_REQUEST,
            "weak_password",
            "password must be at least 10 characters",
        );
    }
    let stored_hash = {
        let guard = state.store.lock().unwrap();
        match guard.user(&username) {
            Some(u) => u.password_hash.clone(),
            None => {
                return json_error(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "your user no longer exists",
                )
            }
        }
    };
    let current = req.current_password.clone();
    let verify_hash = stored_hash.clone();
    let ok = tokio::task::spawn_blocking(move || auth::verify_password(&current, &verify_hash))
        .await
        .expect("verify task");
    if !ok {
        return json_error(
            StatusCode::FORBIDDEN,
            "wrong_password",
            "current password is incorrect",
        );
    }
    let new = req.new_password.clone();
    let new_hash = tokio::task::spawn_blocking(move || auth::hash_password(&new))
        .await
        .expect("hashing task");
    let result = {
        let mut guard = state.store.lock().unwrap();
        let mut cand = guard.clone();
        match apply_password_change(&mut cand, &username, &stored_hash, &new_hash) {
            Ok(()) => {}
            Err(PasswordChangeErr::UserGone) => {
                return json_error(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "your user no longer exists",
                )
            }
            Err(PasswordChangeErr::HashRotated) => {
                return json_error(
                    StatusCode::CONFLICT,
                    "password_changed",
                    "your password was changed while this request was in flight; log in again",
                )
            }
        }
        commit(&state, &mut guard, cand)
    };
    match result {
        Ok(()) => {
            state.admin.clear_scraper_memo();
            let cookie = auth::mint_session_cookie(&state, &headers, &username, &new_hash);
            (
                StatusCode::OK,
                [(header::SET_COOKIE, cookie)],
                axum::Json(OkResponse::new()),
            )
                .into_response()
        }
        Err(e) => bad_request(e),
    }
}

fn locale_error_response(error: crate::presentation::LocaleError) -> Response {
    match error {
        crate::presentation::LocaleError::Invalid => json_error(
            StatusCode::BAD_REQUEST,
            "invalid_locale",
            "locale is not valid",
        ),
        crate::presentation::LocaleError::NotInstalled(locale) => json_error(
            StatusCode::BAD_REQUEST,
            "locale_not_installed",
            format!("locale {locale} is not installed"),
        ),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct SetServerLocale {
    pub locale: String,
}

/// `POST /api/settings/locale` — change the persisted server default. Admin
/// and superuser roles share the existing server-settings authority.
#[utoipa::path(
    post,
    path = "/api/settings/locale",
    tag = "settings",
    request_body = SetServerLocale,
    responses(
        (status = 200, description = "Applied", body = OkResponse),
        (status = 400, description = "Invalid or uninstalled locale", body = ApiError),
        (status = 401, description = "No session, or the caller's user was deleted", body = ApiError),
        (status = 403, description = "Server settings require an admin", body = ApiError),
        (status = 422, description = "Invalid JSON request body", body = ApiError),
    ),
)]
pub async fn locale(
    State(state): State<Arc<AppState>>,
    Extension(Identity(username)): Extension<Identity>,
    req: Request,
) -> Response {
    {
        let guard = state.store.lock().unwrap();
        match role_of(&guard, &username) {
            Some(role) if role.is_admin() => {}
            Some(_) => return forbidden("server settings require an admin"),
            None => return stale_session(),
        }
    }
    let ApiJson(req) = match ApiJson::<SetServerLocale>::from_request(req, &state).await {
        Ok(req) => req,
        Err(response) => return response,
    };
    let locale = match crate::presentation::installed_locale(&req.locale) {
        Ok(locale) => locale,
        Err(error) => return locale_error_response(error),
    };
    let result = {
        let mut guard = state.store.lock().unwrap();
        match role_of(&guard, &username) {
            Some(role) if role.is_admin() => {}
            Some(_) => return forbidden("server settings require an admin"),
            None => return stale_session(),
        }
        let mut cand = guard.clone();
        cand.default_locale = locale;
        commit(&state, &mut guard, cand)
    };
    match result {
        Ok(()) => ok_json(),
        Err(e) => bad_request(e),
    }
}

/// `POST /api/settings/validate-key` — authenticated twin of the setup
/// probe. The upstream is ALWAYS the configured `base_url`, never a
/// caller-supplied one: a request-supplied target would let any logged-in
/// user turn the proxy into an SSRF probe of internal hosts (the response
/// distinguishes reachable/rejected/unreachable). An admin testing a new
/// upstream saves it first, then validates. `req.base_url` is ignored.
#[utoipa::path(
    post,
    path = "/api/settings/validate-key",
    tag = "settings",
    request_body = ValidateKeyReq,
    responses(
        (status = 200, description = "Probe finished against the CONFIGURED upstream; any \
            `base_url` in the body is ignored (SSRF guard)", body = ValidateKeyResponse),
        (status = 401, description = "No session", body = ApiError),
    ),
)]
pub async fn validate_key(
    State(state): State<Arc<AppState>>,
    ApiJson(req): ApiJson<ValidateKeyReq>,
) -> Response {
    let base = state.store.lock().unwrap().upstream.base_url.clone();
    let base = base.trim().trim_end_matches('/').to_owned();
    axum::Json(ValidateKeyResponse::probed(
        probe_key(&state.http, &base, req.key.trim()).await,
    ))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_user(username: &str, hash: &str) -> StoredConfig {
        let mut sc = StoredConfig::default();
        sc.users.push(User {
            username: username.into(),
            password_hash: hash.into(),
            role: Role::User,
            locale: None,
        });
        sc
    }

    #[test]
    fn parse_role_maps_admin_and_user_but_never_superuser() {
        assert_eq!(parse_role("admin"), Some(Role::Admin));
        assert_eq!(parse_role("user"), Some(Role::User));
        // Superuser is intentionally NOT assignable through the users API.
        assert_eq!(parse_role("superuser"), None);
        assert_eq!(parse_role("root"), None);
        assert_eq!(parse_role(""), None);
    }

    #[test]
    fn password_change_applies_when_the_verified_hash_is_current() {
        let mut sc = store_with_user("alice", "old-hash");
        assert_eq!(
            apply_password_change(&mut sc, "alice", "old-hash", "new-hash"),
            Ok(())
        );
        assert_eq!(sc.users[0].password_hash, "new-hash");
    }

    #[test]
    fn password_change_refuses_when_the_hash_rotated_since_verify() {
        // An admin reset landed between this caller's verify and commit: the
        // stored hash is no longer the one the current password matched.
        let mut sc = store_with_user("alice", "reset-by-admin");
        assert_eq!(
            apply_password_change(&mut sc, "alice", "old-hash", "new-hash"),
            Err(PasswordChangeErr::HashRotated)
        );
        assert_eq!(sc.users[0].password_hash, "reset-by-admin", "unchanged");
    }

    #[test]
    fn password_change_refuses_when_the_user_was_deleted() {
        let mut sc = StoredConfig::default();
        assert_eq!(
            apply_password_change(&mut sc, "ghost", "h", "n"),
            Err(PasswordChangeErr::UserGone)
        );
    }
}

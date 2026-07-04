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
        *pool = Arc::new(pool.rebuild(candidate.pool_specs()));
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
    // The wizard's advanced path lets an unauthenticated pre-claim caller
    // supply base_url, so guard it against the link-local metadata range
    // before probing (loopback/RFC1918 stay allowed for self-hosted NIM).
    if let Err(e) = config::check_base_url(&base) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_base_url", e);
    }
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

fn ok_json(v: serde_json::Value) -> Response {
    axum::Json(v).into_response()
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

    let nim_keys: Vec<serde_json::Value> = sc
        .upstream
        .nim_keys
        .iter()
        .filter(|k| admin_view || k.owner == username)
        .map(|k| {
            let lane = stats.get(&k.key);
            serde_json::json!({
                "fingerprint": fingerprint(&k.key),
                "last4": last4(&k.key),
                "owner": k.owner,
                "enabled": k.enabled,
                "rpm": k.rpm,
                "lane": lane.map(|(i, _, _)| i),
                "in_window": lane.map(|(_, w, _)| w),
                "cooldown_ms": lane.map(|(_, _, c)| c),
                "guarded": guarded_key.as_deref() == Some(k.key.as_str()),
            })
        })
        .collect();

    let client_keys: Vec<serde_json::Value> = sc
        .client_auth
        .keys
        .iter()
        .filter(|c| admin_view || c.owner == username)
        .map(|c| serde_json::json!({"name": c.name, "last4": c.last4, "owner": c.owner}))
        .collect();

    let mut body = serde_json::json!({
        "username": username,
        "role": match role { Role::Superuser => "superuser", Role::Admin => "admin", Role::User => "user" },
        "mode": match sc.client_auth.mode { Mode::Open => "open", Mode::Keyed => "keyed" },
        "pool": {
            "enabled": pool.len(),
            "capacity_rpm": pool.capacity_rpm(),
        },
        "nim_keys": nim_keys,
        "client_keys": client_keys,
    });
    if admin_view {
        body["server"] = serde_json::json!({
            "base_url": sc.upstream.base_url,
            "limits": sc.limits,
            "pricing": sc.pricing,
            "history": sc.history,
            "governor": sc.governor,
        });
        body["users"] = serde_json::json!(sc
            .users
            .iter()
            .map(|u| {
                serde_json::json!({
                    "username": u.username,
                    "role": match u.role { Role::Superuser => "superuser", Role::Admin => "admin", Role::User => "user" },
                    "nim_keys": sc.upstream.nim_keys.iter().filter(|k| k.owner == u.username).count(),
                    "client_keys": sc.client_auth.keys.iter().filter(|c| c.owner == u.username).count(),
                })
            })
            .collect::<Vec<_>>());
    }
    ok_json(body)
}

#[derive(Deserialize)]
pub struct NimKeysReq {
    add: Option<AddNimKey>,
    remove: Option<String>, // fingerprint
    set: Option<SetNimKey>,
}

#[derive(Deserialize)]
pub struct AddNimKey {
    key: String,
    rpm: Option<usize>,
}

#[derive(Deserialize)]
pub struct SetNimKey {
    fingerprint: String,
    enabled: Option<bool>,
    rpm: Option<usize>,
}

/// `POST /api/settings/nim-keys` — any role may add keys (owner = caller)
/// and manage their OWN keys; admins manage any key. The superuser's last
/// enabled key is protected by `validate` (the pool-floor invariant).
pub async fn nim_keys(
    State(state): State<Arc<AppState>>,
    Extension(Identity(username)): Extension<Identity>,
    axum::Json(req): axum::Json<NimKeysReq>,
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
                owner: username.clone(),
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
        Ok(()) => ok_json(serde_json::json!({"ok": true})),
        Err(e) => bad_request(e),
    }
}

#[derive(Deserialize)]
pub struct ClientsReq {
    add: Option<AddClient>,
    remove: Option<String>, // name
    mode: Option<String>,
}

#[derive(Deserialize)]
pub struct AddClient {
    name: String,
}

/// `POST /api/settings/clients` — client-key create/revoke for any role
/// (own keys; admins revoke any); the open/keyed mode toggle is admin-only.
/// A created secret is returned exactly once and stored only as a digest.
pub async fn clients(
    State(state): State<Arc<AppState>>,
    Extension(Identity(username)): Extension<Identity>,
    axum::Json(req): axum::Json<ClientsReq>,
) -> Response {
    let mut guard = state.store.lock().unwrap();
    let Some(role) = role_of(&guard, &username) else {
        return stale_session();
    };
    let mut cand = guard.clone();
    let mut minted: Option<String> = None;
    match (req.add, req.remove, req.mode) {
        (Some(add), None, None) => {
            let mut raw = [0u8; 16];
            getrandom::getrandom(&mut raw).expect("OS RNG for client secret");
            let secret = format!("npk_{}", auth::hex(&raw));
            cand.client_auth.keys.push(ClientKey {
                name: add.name.trim().to_owned(),
                secret_sha256: auth::sha256_hex(&secret),
                last4: last4(&secret),
                owner: username.clone(),
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
        Ok(()) => ok_json(match minted {
            Some(secret) => serde_json::json!({"ok": true, "secret": secret}),
            None => serde_json::json!({"ok": true}),
        }),
        Err(e) => bad_request(e),
    }
}

/// Admin-only settings sections share one skeleton: role check, mutate the
/// candidate, commit.
macro_rules! admin_section {
    ($fn_name:ident, $req:ty, $apply:expr) => {
        pub async fn $fn_name(
            State(state): State<Arc<AppState>>,
            Extension(Identity(username)): Extension<Identity>,
            axum::Json(req): axum::Json<$req>,
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
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => bad_request(e),
            }
        }
    };
}

#[derive(Deserialize)]
pub struct UpstreamReq {
    base_url: String,
}

/// `POST /api/settings/upstream` (admin) — also flushes the model-catalog
/// cache and the per-model no-inject memory, which are upstream-specific.
pub async fn upstream(
    State(state): State<Arc<AppState>>,
    Extension(Identity(username)): Extension<Identity>,
    axum::Json(req): axum::Json<UpstreamReq>,
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
            ok_json(serde_json::json!({"ok": true}))
        }
        Err(e) => bad_request(e),
    }
}

/// Mirror of `config::Limits` WITHOUT serde defaults: a partial body is a
/// 422, never a silent reset of the omitted fields.
#[derive(Deserialize)]
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
    |cand: &mut StoredConfig, req: LimitsReq| {
        cand.limits = crate::config::Limits {
            max_wait_secs: req.max_wait_secs,
            heartbeat_secs: req.heartbeat_secs,
            models_ttl_secs: req.models_ttl_secs,
            stream_idle_secs: req.stream_idle_secs,
            request_timeout_secs: req.request_timeout_secs,
            max_inflight: req.max_inflight,
            strict_passthrough: req.strict_passthrough,
        };
    }
);

#[derive(Deserialize)]
pub struct PricingReq {
    ref_price_in: f64,
    ref_price_out: f64,
}

admin_section!(
    pricing,
    PricingReq,
    |cand: &mut StoredConfig, req: PricingReq| {
        cand.pricing.ref_price_in = req.ref_price_in;
        cand.pricing.ref_price_out = req.ref_price_out;
    }
);

#[derive(Deserialize)]
pub struct HistoryReq {
    days: u64,
}

admin_section!(
    history,
    HistoryReq,
    |cand: &mut StoredConfig, req: HistoryReq| {
        cand.history.days = req.days;
    }
);

#[derive(Deserialize)]
pub struct GovernorReq {
    enabled: Option<bool>,
    set_override: Option<GovernorOverride>,
    remove_override: Option<String>,
}

#[derive(Deserialize)]
pub struct GovernorOverride {
    model: String,
    cap: usize,
}

admin_section!(
    governor_cfg,
    GovernorReq,
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

#[derive(Deserialize)]
pub struct UsersReq {
    add: Option<AddUser>,
    remove: Option<String>,
    reset_password: Option<ResetPassword>,
    set_role: Option<SetRole>,
}

#[derive(Deserialize)]
pub struct AddUser {
    username: String,
    password: String,
    role: String,
}

#[derive(Deserialize)]
pub struct ResetPassword {
    username: String,
    new_password: String,
}

#[derive(Deserialize)]
pub struct SetRole {
    username: String,
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
pub async fn users(
    State(state): State<Arc<AppState>>,
    Extension(Identity(username)): Extension<Identity>,
    axum::Json(req): axum::Json<UsersReq>,
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
            ok_json(serde_json::json!({"ok": true}))
        }
        Err(e) => bad_request(e),
    }
}

#[derive(Deserialize)]
pub struct AccountReq {
    current_password: String,
    new_password: String,
}

/// `POST /api/settings/account` — own password change; always re-verifies
/// the current password regardless of the session. Existing sessions die
/// (the cookie binds a password-hash fragment); the response carries a fresh
/// cookie so THIS session survives.
pub async fn account(
    State(state): State<Arc<AppState>>,
    Extension(Identity(username)): Extension<Identity>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<AccountReq>,
) -> Response {
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
    let ok = tokio::task::spawn_blocking(move || auth::verify_password(&current, &stored_hash))
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
        let Some(user) = cand.users.iter_mut().find(|u| u.username == username) else {
            return json_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "your user no longer exists",
            );
        };
        user.password_hash = new_hash.clone();
        commit(&state, &mut guard, cand)
    };
    match result {
        Ok(()) => {
            state.admin.clear_scraper_memo();
            let cookie = auth::mint_session_cookie(&state, &headers, &username, &new_hash);
            (
                StatusCode::OK,
                [(header::SET_COOKIE, cookie)],
                axum::Json(serde_json::json!({"ok": true})),
            )
                .into_response()
        }
        Err(e) => bad_request(e),
    }
}

/// `POST /api/settings/validate-key` — authenticated twin of the setup
/// probe. The upstream is ALWAYS the configured `base_url`, never a
/// caller-supplied one: a request-supplied target would let any logged-in
/// user turn the proxy into an SSRF probe of internal hosts (the response
/// distinguishes reachable/rejected/unreachable). An admin testing a new
/// upstream saves it first, then validates. `req.base_url` is ignored.
pub async fn validate_key(
    State(state): State<Arc<AppState>>,
    axum::Json(req): axum::Json<ValidateKeyReq>,
) -> Response {
    let base = state.store.lock().unwrap().upstream.base_url.clone();
    let base = base.trim().trim_end_matches('/').to_owned();
    axum::Json(match probe_key(&state.http, &base, req.key.trim()).await {
        Ok(models) => serde_json::json!({"ok": true, "models": models}),
        Err(e) => serde_json::json!({"ok": false, "error": e}),
    })
    .into_response()
}

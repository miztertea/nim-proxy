//! Authentication: store-backed user sessions for the dashboard and
//! observability endpoints, plus the primitives (constant-time compare,
//! PBKDF2 password hashing, HMAC-signed session cookies, a failed-attempt
//! throttle) the rest of the app uses. The `/v1/*` API keeps its own
//! client-key check in `proxy.rs`; this module protects the operator surface.
//!
//! A session token binds three things: an expiry, the username, and a short
//! fragment of the user's *current* password hash. The fragment means a
//! password change (or admin reset) invalidates that user's outstanding
//! sessions instantly, and the username lookup happens against the live
//! config store on every request — deleting a user kills their session the
//! same moment.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;

use crate::config::StoredConfig;
use crate::AppState;

const COOKIE: &str = "nimproxy_session";
const SESSION_TTL_SECS: u64 = 12 * 3600;

/// Constant-time byte equality (avoids leaking content via timing). `subtle`
/// short-circuits only on a *length* mismatch — that leaks the secret's length,
/// which is acceptable; the bytes themselves are always compared in full.
pub fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

pub fn sha256_hex(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

/// PBKDF2-HMAC-SHA256 iteration count for newly minted hashes (OWASP's
/// recommendation). Every stored hash encodes its own count, so this can be
/// raised later without invalidating existing credentials.
const PBKDF2_ITERS: u32 = 600_000;

/// One PBKDF2-HMAC-SHA256 block (dkLen = 32, one SHA-256 output — all a
/// password hash needs). Hand-rolled over the installed hmac/sha2 pair,
/// pinned by the RFC 7914 §11 test vectors below.
fn pbkdf2_sha256(password: &[u8], salt: &[u8], iters: u32) -> [u8; 32] {
    // Key the HMAC once and clone the initialized state per iteration —
    // rekeying every round would double the SHA-256 compressions.
    let keyed = Hmac::<Sha256>::new_from_slice(password).expect("HMAC accepts any key length");
    let prf = |data: &[u8], extra: &[u8]| {
        let mut m = keyed.clone();
        m.update(data);
        m.update(extra);
        m.finalize().into_bytes()
    };
    let mut u = prf(salt, &1u32.to_be_bytes()); // U1 = PRF(P, S || INT(1))
    let mut out: [u8; 32] = u.into();
    for _ in 1..iters {
        u = prf(&u, &[]);
        for (o, b) in out.iter_mut().zip(u.iter()) {
            *o ^= b;
        }
    }
    out
}

/// Hash a password for storage: `pbkdf2-sha256$<iters>$<salt>$<hash>` (hex).
pub fn hash_password(password: &str) -> String {
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt).expect("OS RNG for salt");
    let dk = pbkdf2_sha256(password.as_bytes(), &salt, PBKDF2_ITERS);
    format!("pbkdf2-sha256${PBKDF2_ITERS}${}${}", hex(&salt), hex(&dk))
}

/// Verify a password against a stored hash string; malformed strings fail
/// closed. Honors the hash's own iteration count. CPU-bound (~hundreds of
/// ms by design) — call inside `spawn_blocking` on request paths.
pub fn verify_password(password: &str, stored: &str) -> bool {
    let mut parts = stored.split('$');
    let (Some("pbkdf2-sha256"), Some(iters), Some(salt), Some(hash), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return false;
    };
    let Ok(iters @ 1..) = iters.parse::<u32>() else {
        return false;
    };
    let Some(salt) = unhex(salt) else {
        return false;
    };
    let dk = pbkdf2_sha256(password.as_bytes(), &salt, iters);
    ct_eq(&hex(&dk), hash)
}

/// First 8 hex chars of SHA-256(password_hash): enough to bind a session to
/// a password *generation* (invalidation on change), too short to help brute
/// force the hash itself.
fn pw_fragment(password_hash: &str) -> String {
    sha256_hex(password_hash)[..8].to_owned()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Session/throttle state: a random per-boot signing key (sessions don't
/// survive restarts — deliberate, see the auth-posture ADR), the login
/// throttle, and a memo of the last verified scraper credential so
/// Prometheus polls don't pay PBKDF2 every 15 seconds.
pub struct Admin {
    signing_key: [u8; 32],
    trust_proxy: bool,
    throttle: Mutex<Throttle>,
    /// (HMAC(signing_key, "user:pass"), username) of the last verified
    /// header credential. Cleared whenever users change.
    scraper_memo: Mutex<Option<([u8; 32], String)>>,
}

/// Fixed-window failed-login limiter (per process, not per IP — a reverse
/// proxy should do IP-level limiting; this is a cheap backstop).
struct Throttle {
    window_start: u64,
    failures: u32,
}

const THROTTLE_WINDOW_SECS: u64 = 60;
const THROTTLE_MAX_FAILURES: u32 = 10;

impl Admin {
    pub fn new(trust_proxy: bool) -> Self {
        let mut signing_key = [0u8; 32];
        getrandom::getrandom(&mut signing_key).expect("OS RNG for session key");
        Self {
            signing_key,
            trust_proxy,
            throttle: Mutex::new(Throttle {
                window_start: now(),
                failures: 0,
            }),
            scraper_memo: Mutex::new(None),
        }
    }

    fn mac(&self, parts: &[&[u8]]) -> [u8; 32] {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.signing_key).expect("HMAC accepts any key length");
        for p in parts {
            mac.update(&(p.len() as u64).to_be_bytes()); // length-prefix each part
            mac.update(p);
        }
        mac.finalize().into_bytes().into()
    }

    /// Mint a session token for `user`:
    /// `hex(expiry).hex(username).pw_fragment.hex(hmac)`.
    pub fn sign_session(&self, expiry: u64, username: &str, password_hash: &str) -> String {
        let frag = pw_fragment(password_hash);
        let tag = self.mac(&[&expiry.to_be_bytes(), username.as_bytes(), frag.as_bytes()]);
        format!(
            "{expiry:x}.{}.{frag}.{}",
            hex(username.as_bytes()),
            hex(&tag)
        )
    }

    /// Verify a session token against the live store: signature intact, not
    /// expired, user still exists, password unchanged since minting.
    /// Returns the authenticated username.
    pub fn verify_session(&self, token: &str, sc: &StoredConfig) -> Option<String> {
        let mut parts = token.split('.');
        let (Some(exp_hex), Some(user_hex), Some(frag), Some(tag_hex), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return None;
        };
        let expiry = u64::from_str_radix(exp_hex, 16).ok()?;
        if expiry < now() {
            return None;
        }
        let username = String::from_utf8(unhex(user_hex)?).ok()?;
        let expected = self.mac(&[&expiry.to_be_bytes(), username.as_bytes(), frag.as_bytes()]);
        if !ct_eq(tag_hex, &hex(&expected)) {
            return None;
        }
        let user = sc.user(&username)?;
        ct_eq(frag, &pw_fragment(&user.password_hash)).then_some(username)
    }

    /// Record a failed attempt; returns true if the caller is now throttled.
    pub fn note_failure(&self) -> bool {
        let mut t = self.throttle.lock().unwrap();
        let n = now();
        if n.saturating_sub(t.window_start) >= THROTTLE_WINDOW_SECS {
            t.window_start = n;
            t.failures = 0;
        }
        t.failures += 1;
        t.failures > THROTTLE_MAX_FAILURES
    }

    pub fn is_throttled(&self) -> bool {
        let t = self.throttle.lock().unwrap();
        now().saturating_sub(t.window_start) < THROTTLE_WINDOW_SECS
            && t.failures > THROTTLE_MAX_FAILURES
    }

    /// Forget the memoized scraper credential. Call on any change to users
    /// (password change/reset, user removal) so revocation is immediate.
    pub fn clear_scraper_memo(&self) {
        *self.scraper_memo.lock().unwrap() = None;
    }

    fn memo_hit(&self, cred: &str) -> Option<String> {
        let tag = self.mac(&[cred.as_bytes()]);
        let memo = self.scraper_memo.lock().unwrap();
        let (t, user) = memo.as_ref()?;
        bool::from(tag.ct_eq(t)).then(|| user.clone())
    }

    fn memoize(&self, cred: &str, username: &str) {
        *self.scraper_memo.lock().unwrap() =
            Some((self.mac(&[cred.as_bytes()]), username.to_owned()));
    }

    fn cookie(&self, headers: &HeaderMap, token: &str, max_age: i64) -> String {
        let secure = self.trust_proxy
            && headers
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|p| p.eq_ignore_ascii_case("https"));
        let secure_attr = if secure { "; Secure" } else { "" };
        format!(
            "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}{secure_attr}"
        )
    }
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    s.as_bytes()
        .chunks(2)
        .map(|c| Some(hex_val(c[0])? << 4 | hex_val(c[1])?))
        .collect()
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{COOKIE}=")) {
            return Some(v.to_owned());
        }
    }
    None
}

/// Resolve the request's identity: a valid session cookie, or scraper-style
/// header credentials (`Authorization: Bearer user:pass` or HTTP Basic)
/// verified against the store. Header verification pays PBKDF2 once, then
/// hits an HMAC memo on subsequent polls. Returns the username.
pub async fn identify(state: &Arc<AppState>, headers: &HeaderMap) -> Option<String> {
    if let Some(tok) = cookie_token(headers) {
        let sc = state.store.lock().unwrap();
        if let Some(user) = state.admin.verify_session(&tok, &sc) {
            return Some(user);
        }
    }
    let auth = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let cred = if let Some(bearer) = auth.strip_prefix("Bearer ") {
        bearer.trim().to_owned()
    } else if let Some(basic) = auth.strip_prefix("Basic ") {
        String::from_utf8(base64_decode(basic.trim())?).ok()?
    } else {
        return None;
    };
    if let Some(user) = state.admin.memo_hit(&cred) {
        return Some(user);
    }
    let (username, password) = cred.split_once(':')?;
    let stored_hash = {
        let sc = state.store.lock().unwrap();
        sc.user(username)?.password_hash.clone()
    };
    let password = password.to_owned();
    let ok = tokio::task::spawn_blocking(move || verify_password(&password, &stored_hash))
        .await
        .unwrap_or(false);
    if !ok {
        return None;
    }
    state.admin.memoize(&cred, username);
    Some(username.to_owned())
}

/// Minimal base64 decoder (standard alphabet, optional padding) — avoids a
/// dependency for the one place we need it (HTTP Basic).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = s
        .bytes()
        .filter(|&c| c != b'=' && !c.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let mut acc = 0u32;
        for &c in chunk {
            acc = (acc << 6) | val(c)?;
        }
        acc <<= 6 * (4 - chunk.len());
        let bytes = match chunk.len() {
            4 => 3,
            3 => 2,
            2 => 1,
            _ => return None,
        };
        for i in 0..bytes {
            out.push((acc >> (16 - i * 8)) as u8);
        }
    }
    Some(out)
}

/// axum middleware: gate the operator surface. Pre-setup everything routes
/// to the wizard (browsers) or a 503 (API clients); post-setup a session is
/// required. The authenticated username is stored in request extensions for
/// downstream role checks.
pub async fn require_session(
    State(state): State<Arc<AppState>>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if state
        .setup_required
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return if wants_html(req.headers()) {
            redirect("/setup")
        } else {
            setup_required_json()
        };
    }
    match identify(&state, req.headers()).await {
        Some(username) => {
            req.extensions_mut().insert(Identity(username));
            next.run(req).await
        }
        None if wants_html(req.headers()) => redirect_found("/login"),
        None => unauthorized_json(),
    }
}

/// The authenticated username, inserted by [`require_session`] for the
/// settings handlers' role and ownership checks.
#[derive(Clone)]
#[allow(dead_code)] // read by the settings API (next phase)
pub struct Identity(pub String);

fn wants_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"))
}

fn unauthorized_json() -> Response {
    let body = serde_json::json!({
        "error": {
            "message": "authentication required (session cookie, or Authorization: Bearer <username>:<password>)",
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

pub fn setup_required_json() -> Response {
    let body = serde_json::json!({
        "error": {
            "message": "first-time setup has not been completed; open the dashboard to create the superuser",
            "type": "proxy_error",
            "code": "setup_required"
        }
    });
    (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body)).into_response()
}

/// `GET /login` — serve the form, bounce to `/setup` pre-setup, or to `/`
/// if already authed.
pub async fn login_page(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if state
        .setup_required
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return redirect("/setup");
    }
    if identify(&state, &headers).await.is_some() {
        return redirect_found("/");
    }
    login_html(false)
}

/// `POST /login` — verify username + password, set the session cookie.
pub async fn login_submit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if state
        .setup_required
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return redirect("/setup");
    }
    let admin = &state.admin;
    if admin.is_throttled() {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        return too_many();
    }
    let username = form_field(&body, "username").unwrap_or_default();
    let password = form_field(&body, "password").unwrap_or_default();
    let stored_hash = {
        let sc = state.store.lock().unwrap();
        sc.user(&username).map(|u| u.password_hash.clone())
    };
    // Verify even for unknown users (against a burner hash) so the response
    // time doesn't reveal which usernames exist.
    let hash = stored_hash.clone().unwrap_or_else(|| {
        "pbkdf2-sha256$600000$00000000000000000000000000000000$0000000000000000000000000000000000000000000000000000000000000000".to_owned()
    });
    let pw = password.clone();
    let ok = tokio::task::spawn_blocking(move || verify_password(&pw, &hash))
        .await
        .unwrap_or(false)
        && stored_hash.is_some();
    if ok {
        let hash = stored_hash.expect("checked above");
        let cookie = mint_session_cookie(&state, &headers, &username, &hash);
        return Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(header::LOCATION, "/")
            .header(header::SET_COOKIE, cookie)
            .body(Body::empty())
            .unwrap();
    }
    metrics::counter!("nimproxy_login_failures_total").increment(1);
    admin.note_failure();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    login_html(true)
}

/// Mint a Set-Cookie value for a just-verified user — shared by login and
/// the setup wizard's completion.
pub fn mint_session_cookie(
    state: &AppState,
    headers: &HeaderMap,
    username: &str,
    password_hash: &str,
) -> String {
    let expiry = now() + SESSION_TTL_SECS;
    let token = state.admin.sign_session(expiry, username, password_hash);
    state.admin.cookie(headers, &token, SESSION_TTL_SECS as i64)
}

/// `POST /logout` — clear the cookie.
pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let cookie = state.admin.cookie(&headers, "", 0);
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, "/login")
        .header(header::SET_COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

fn redirect(to: &str) -> Response {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, to)
        .body(Body::empty())
        .unwrap()
}

fn redirect_found(to: &str) -> Response {
    redirect(to)
}

fn too_many() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "60")],
        "too many failed attempts, try again shortly\n",
    )
        .into_response()
}

/// Parse a single field from an application/x-www-form-urlencoded body.
pub fn form_field(body: &str, field: &str) -> Option<String> {
    for pair in body.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == field {
                return Some(url_decode(v));
            }
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let src = s.replace('+', " ");
    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // Decode a "%XX" escape purely from bytes — never re-slice the &str,
        // which would panic if the window lands inside a multibyte char.
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let (hi, lo) = (bytes[i + 1], bytes[i + 2]);
            if let (Some(h), Some(l)) = (hex_val(hi), hex_val(lo)) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Value of a single ASCII hex digit, or None.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn login_html(error: bool) -> Response {
    let err = if error {
        r#"<p class="err">Incorrect username or password.</p>"#
    } else {
        ""
    };
    let html = format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>NIM Proxy — Sign in</title>
<style>
:root {{ color-scheme: light dark; }}
body {{ margin:0; min-height:100vh; display:grid; place-items:center;
  font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif; background:#f9f9f7; color:#0b0b0b; }}
@media (prefers-color-scheme:dark) {{ body {{ background:#0d0d0d; color:#fff; }} }}
.card {{ background:Canvas; border:1px solid rgba(128,128,128,.25); border-radius:14px;
  padding:28px 26px; width:300px; box-shadow:0 6px 24px rgba(0,0,0,.12); }}
h1 {{ font-size:17px; margin:0 0 4px; }}
p.sub {{ color:#898781; font-size:13px; margin:0 0 18px; }}
input {{ width:100%; box-sizing:border-box; font:inherit; padding:9px 11px; border-radius:9px;
  border:1px solid rgba(128,128,128,.4); background:Field; color:inherit; margin-bottom:12px; }}
button {{ width:100%; font:inherit; font-weight:600; padding:9px; border:0; border-radius:9px;
  background:#2a78d6; color:#fff; cursor:pointer; }}
.err {{ color:#d03b3b; font-size:13px; margin:0 0 12px; }}
</style></head><body>
<form class="card" method="post" action="/login">
  <h1>NIM&nbsp;Proxy</h1><p class="sub">Sign in to the dashboard.</p>
  {err}
  <input type="text" name="username" placeholder="Username" autofocus autocomplete="username">
  <input type="password" name="password" placeholder="Password" autocomplete="current-password">
  <button type="submit">Sign in</button>
</form></body></html>"##
    );
    let status = if error {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::OK
    };
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Role, User};

    fn admin() -> Admin {
        Admin::new(false)
    }

    fn store_with(username: &str, password_hash: &str) -> StoredConfig {
        StoredConfig {
            users: vec![User {
                username: username.into(),
                password_hash: password_hash.into(),
                role: Role::Superuser,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn pbkdf2_matches_rfc7914_vectors() {
        // RFC 7914 §11 PBKDF2-HMAC-SHA256 vectors (first 32 of dkLen=64 —
        // blocks are independent, and we only ever derive one).
        assert_eq!(
            hex(&pbkdf2_sha256(b"passwd", b"salt", 1)),
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc"
        );
        assert_eq!(
            hex(&pbkdf2_sha256(b"Password", b"NaCl", 80_000)),
            "4ddcd8f60b98be21830cee5ef22701f9641a4418d04c0414aeff08876b34ab56"
        );
    }

    #[test]
    fn password_hash_round_trips_and_rejects_wrong_password() {
        // Manually built low-iteration hash: the count is read back from the
        // string, so verification honors it (this is also what keeps test
        // fixtures cheap without a prod knob).
        let dk = pbkdf2_sha256(b"hunter22", b"\x01\x02\x03\x04", 1_000);
        let stored = format!("pbkdf2-sha256$1000$01020304${}", hex(&dk));
        assert!(verify_password("hunter22", &stored));
        assert!(!verify_password("hunter23", &stored));
    }

    #[test]
    fn malformed_hash_strings_fail_closed() {
        for bad in [
            "",
            "plaintext",
            "pbkdf2-sha256$0$aa$bb",       // zero iterations
            "pbkdf2-sha256$x$aa$bb",       // non-numeric iterations
            "pbkdf2-sha256$1000$zz$bb",    // bad salt hex
            "pbkdf2-sha256$1000$aa",       // missing field
            "pbkdf2-sha256$1000$aa$bb$cc", // extra field
            "scrypt$1000$aa$bb",           // unknown scheme
        ] {
            assert!(!verify_password("x", bad), "accepted: {bad}");
        }
    }

    #[test]
    fn hash_password_emits_current_format() {
        let h = hash_password("correct horse");
        assert!(h.starts_with("pbkdf2-sha256$600000$"), "{h}");
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong horse", &h));
        // Distinct salts: hashing the same password twice differs.
        assert_ne!(h, hash_password("correct horse"));
    }

    #[test]
    fn ct_eq_matches_std_eq() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("", "x"));
    }

    #[test]
    fn valid_session_round_trips_and_carries_identity() {
        let a = admin();
        let sc = store_with("alice", "hash-v1");
        let tok = a.sign_session(now() + 100, "alice", "hash-v1");
        assert_eq!(a.verify_session(&tok, &sc).as_deref(), Some("alice"));
    }

    #[test]
    fn expired_session_rejected() {
        let a = admin();
        let sc = store_with("alice", "hash-v1");
        let tok = a.sign_session(now() - 1, "alice", "hash-v1");
        assert!(a.verify_session(&tok, &sc).is_none());
    }

    #[test]
    fn tampered_session_rejected() {
        let a = admin();
        let sc = store_with("alice", "hash-v1");
        let tok = a.sign_session(now() + 100, "alice", "hash-v1");
        let mut chars: Vec<char> = tok.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'a' { 'b' } else { 'a' };
        let tampered: String = chars.into_iter().collect();
        assert!(a.verify_session(&tampered, &sc).is_none());
    }

    #[test]
    fn foreign_key_session_rejected() {
        let a = admin();
        let b = admin(); // different random signing key
        let sc = store_with("alice", "hash-v1");
        let tok = a.sign_session(now() + 100, "alice", "hash-v1");
        assert!(b.verify_session(&tok, &sc).is_none());
    }

    #[test]
    fn password_change_invalidates_existing_sessions() {
        let a = admin();
        let tok = a.sign_session(now() + 100, "alice", "hash-v1");
        let rotated = store_with("alice", "hash-v2");
        assert!(
            a.verify_session(&tok, &rotated).is_none(),
            "session minted against the old password hash must die on change"
        );
    }

    #[test]
    fn deleted_user_session_rejected() {
        let a = admin();
        let tok = a.sign_session(now() + 100, "alice", "hash-v1");
        let sc = store_with("bob", "hash-v1");
        assert!(a.verify_session(&tok, &sc).is_none());
    }

    #[test]
    fn session_username_is_authenticated_not_just_parsed() {
        // Re-labeling the username segment without re-signing must fail.
        let a = admin();
        let sc = StoredConfig {
            users: vec![
                User {
                    username: "alice".into(),
                    password_hash: "h".into(),
                    role: Role::User,
                },
                User {
                    username: "admin".into(),
                    password_hash: "h".into(),
                    role: Role::Superuser,
                },
            ],
            ..Default::default()
        };
        let tok = a.sign_session(now() + 100, "alice", "h");
        let mut parts: Vec<&str> = tok.split('.').collect();
        let admin_hex = hex(b"admin");
        parts[1] = &admin_hex;
        let forged = parts.join(".");
        assert!(a.verify_session(&forged, &sc).is_none());
    }

    #[test]
    fn scraper_memo_round_trips_and_clears() {
        let a = admin();
        assert!(a.memo_hit("alice:pw").is_none());
        a.memoize("alice:pw", "alice");
        assert_eq!(a.memo_hit("alice:pw").as_deref(), Some("alice"));
        assert!(a.memo_hit("alice:other").is_none());
        a.clear_scraper_memo();
        assert!(a.memo_hit("alice:pw").is_none());
    }

    #[test]
    fn basic_auth_decodes() {
        // base64("alice:hunter2")
        let decoded = base64_decode("YWxpY2U6aHVudGVyMg==").unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "alice:hunter2");
    }

    #[test]
    fn form_field_parses() {
        assert_eq!(
            form_field("password=hunter2", "password").as_deref(),
            Some("hunter2")
        );
        assert_eq!(
            form_field("a=1&password=p%40ss", "password").as_deref(),
            Some("p@ss")
        );
    }

    #[test]
    fn url_decode_survives_multibyte_and_malformed_escapes() {
        // A multibyte char right after '%' must not panic (a '%XX' window that
        // lands on a non-char-boundary of the original &str). Reachable pre-auth
        // via POST /login, so this must never crash the handler.
        assert_eq!(url_decode("%\u{20ac}"), "%\u{20ac}"); // "%€"
        assert_eq!(url_decode("%a\u{20ac}"), "%a\u{20ac}"); // "%a€"
        assert_eq!(url_decode("caf\u{e9}%20x"), "caf\u{e9} x"); // valid escape amid UTF-8
                                                                // Malformed / truncated escapes pass through untouched.
        assert_eq!(url_decode("%"), "%");
        assert_eq!(url_decode("%z"), "%z");
        assert_eq!(url_decode("%zz"), "%zz");
        assert_eq!(url_decode("100%"), "100%");
        // Well-formed escapes still decode, and '+' still becomes space.
        assert_eq!(url_decode("p%40ss+word"), "p@ss word");
    }
}

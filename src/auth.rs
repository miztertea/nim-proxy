//! Authentication: a shared-password admin gate for the dashboard and
//! observability endpoints, plus the primitives (constant-time compare,
//! HMAC-signed session cookies, a failed-attempt throttle) the rest of the
//! app uses. The `/v1/*` API keeps its own Bearer-key check in `proxy.rs`;
//! this module is about protecting the operator surface.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::Arc;
use subtle::ConstantTimeEq;

use crate::AppState;

const COOKIE: &str = "nimproxy_session";
const SESSION_TTL_SECS: u64 = 12 * 3600;

/// Constant-time byte equality (avoids leaking content via timing). `subtle`
/// short-circuits only on a *length* mismatch — that leaks the secret's length,
/// which is acceptable; the bytes themselves are always compared in full.
pub fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Admin auth state: the shared password and a random per-boot signing key.
/// `None` password means insecure mode (no admin gate).
pub struct Admin {
    password: Option<String>,
    signing_key: [u8; 32],
    trust_proxy: bool,
    throttle: Mutex<Throttle>,
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
    pub fn new(password: Option<String>, trust_proxy: bool) -> Self {
        let mut signing_key = [0u8; 32];
        getrandom::getrandom(&mut signing_key).expect("OS RNG for session key");
        Self {
            password,
            signing_key,
            trust_proxy,
            throttle: Mutex::new(Throttle {
                window_start: now(),
                failures: 0,
            }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.password.is_some()
    }

    /// Sign an expiry into a session token: `hex(expiry).hex(hmac)`.
    fn sign(&self, expiry: u64) -> String {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.signing_key).expect("HMAC accepts any key length");
        mac.update(&expiry.to_be_bytes());
        let tag = mac.finalize().into_bytes();
        format!("{expiry:x}.{}", hex(&tag))
    }

    /// Verify a session token: HMAC matches (constant-time) and not expired.
    fn verify(&self, token: &str) -> bool {
        let Some((exp_hex, tag_hex)) = token.split_once('.') else {
            return false;
        };
        let Ok(expiry) = u64::from_str_radix(exp_hex, 16) else {
            return false;
        };
        if expiry < now() {
            return false;
        }
        // Recompute and compare in constant time.
        let expected = self.sign(expiry);
        let Some((_, expected_tag)) = expected.split_once('.') else {
            return false;
        };
        ct_eq(tag_hex, expected_tag)
    }

    fn password_matches(&self, candidate: &str) -> bool {
        self.password
            .as_deref()
            .is_some_and(|p| ct_eq(candidate, p))
    }

    /// Record a failed attempt; returns true if the caller is now throttled.
    fn note_failure(&self) -> bool {
        let mut t = self.throttle.lock().unwrap();
        let n = now();
        if n.saturating_sub(t.window_start) >= THROTTLE_WINDOW_SECS {
            t.window_start = n;
            t.failures = 0;
        }
        t.failures += 1;
        t.failures > THROTTLE_MAX_FAILURES
    }

    fn is_throttled(&self) -> bool {
        let t = self.throttle.lock().unwrap();
        now().saturating_sub(t.window_start) < THROTTLE_WINDOW_SECS
            && t.failures > THROTTLE_MAX_FAILURES
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

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
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

/// True if the request carries valid admin credentials: a valid session
/// cookie, `Authorization: Bearer <password>`, or HTTP Basic with the
/// password. All secret comparisons are constant-time.
pub fn authorized(admin: &Admin, headers: &HeaderMap) -> bool {
    if !admin.enabled() {
        return true; // insecure mode: no admin gate
    }
    if let Some(tok) = cookie_token(headers) {
        if admin.verify(&tok) {
            return true;
        }
    }
    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(bearer) = auth.strip_prefix("Bearer ") {
            if admin.password_matches(bearer.trim()) {
                return true;
            }
        }
        if let Some(basic) = auth.strip_prefix("Basic ") {
            // "user:pass" base64; accept any user, match the password half.
            if let Some(pass) = decode_basic_password(basic.trim()) {
                if admin.password_matches(&pass) {
                    return true;
                }
            }
        }
    }
    false
}

fn decode_basic_password(b64: &str) -> Option<String> {
    let decoded = base64_decode(b64)?;
    let s = String::from_utf8(decoded).ok()?;
    Some(s.split_once(':').map(|(_, p)| p).unwrap_or(&s).to_owned())
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

/// axum middleware: gate the admin surface. HTML clients get a redirect to
/// the login page; API clients (and scrapers) get a 401.
pub async fn require_admin(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if authorized(&state.admin, req.headers()) {
        return next.run(req).await;
    }
    let wants_html = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"));
    if wants_html {
        Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, "/login")
            .body(Body::empty())
            .unwrap()
    } else {
        unauthorized_json()
    }
}

fn unauthorized_json() -> Response {
    let body = serde_json::json!({
        "error": {
            "message": "admin authentication required (session cookie or Authorization: Bearer <password>)",
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

/// `GET /login` — serve the form, or bounce to `/` if already authed.
pub async fn login_page(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if authorized(&state.admin, &headers) {
        return redirect("/");
    }
    login_html(false)
}

/// `POST /login` — verify the password, set the session cookie.
pub async fn login_submit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let admin = &state.admin;
    if admin.is_throttled() {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        return too_many();
    }
    let password = form_field(&body, "password").unwrap_or_default();
    if admin.password_matches(&password) {
        let expiry = now() + SESSION_TTL_SECS;
        let token = admin.sign(expiry);
        let cookie = admin.cookie(&headers, &token, SESSION_TTL_SECS as i64);
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

fn too_many() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "60")],
        "too many failed attempts, try again shortly\n",
    )
        .into_response()
}

/// Parse a single field from an application/x-www-form-urlencoded body.
fn form_field(body: &str, field: &str) -> Option<String> {
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
        r#"<p class="err">Incorrect password.</p>"#
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
  <h1>NIM&nbsp;Proxy</h1><p class="sub">Enter the dashboard password.</p>
  {err}
  <input type="password" name="password" placeholder="Password" autofocus autocomplete="current-password">
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

    fn admin() -> Admin {
        Admin::new(Some("hunter2".into()), false)
    }

    #[test]
    fn ct_eq_matches_std_eq() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("", "x"));
    }

    #[test]
    fn valid_session_round_trips() {
        let a = admin();
        let tok = a.sign(now() + 100);
        assert!(a.verify(&tok));
    }

    #[test]
    fn expired_session_rejected() {
        let a = admin();
        let tok = a.sign(now() - 1);
        assert!(!a.verify(&tok));
    }

    #[test]
    fn tampered_session_rejected() {
        let a = admin();
        let tok = a.sign(now() + 100);
        // Flip the last hex nibble of the tag.
        let mut chars: Vec<char> = tok.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'a' { 'b' } else { 'a' };
        let tampered: String = chars.into_iter().collect();
        assert!(!a.verify(&tampered));
    }

    #[test]
    fn foreign_key_session_rejected() {
        let a = admin();
        let b = admin(); // different random signing key
        let tok = a.sign(now() + 100);
        assert!(!b.verify(&tok), "token signed by a must not verify under b");
    }

    #[test]
    fn basic_auth_password_extracted() {
        // base64("alice:hunter2")
        let b64 = "YWxpY2U6aHVudGVyMg==";
        assert_eq!(decode_basic_password(b64).as_deref(), Some("hunter2"));
    }

    #[test]
    fn bearer_and_basic_authorize() {
        let a = admin();
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, "Bearer hunter2".parse().unwrap());
        assert!(authorized(&a, &h));
        h.insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert!(!authorized(&a, &h));
    }

    #[test]
    fn insecure_mode_allows_all() {
        let a = Admin::new(None, false);
        assert!(authorized(&a, &HeaderMap::new()));
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

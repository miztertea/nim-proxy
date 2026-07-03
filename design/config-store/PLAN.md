# In-app configuration & first-launch setup — implementation plan

## Context

All configuration currently lives in env vars, read once at boot (`knowledge/ops/configure-env.md` even states "no config files, no reload" as philosophy — this feature deliberately supersedes that, via ADR). We're moving app-level config into the application: a Settings page in the dashboard, a persistent store owned by the app, and a first-launch wizard (create superuser → add ≥1 NIM key → validate upstream → land on Overview, logged in). Env shrinks to container-level concerns only — which also makes running outside a container natural.

**Scope decisions (confirmed by maintainer, 2026-07-03):**
- **Multi-user with three roles.** `superuser` = an admin that cannot be deleted (safety: you can never delete all admins); `admin` = can manage users and tune server settings; `user` = views all dashboards, manages only their own account, their own client API keys, and their own NIM keys. Dashboards are identical for every role — only the Settings surface differs. Motivating case: a friend adds their NIM key to the shared pool safely; nobody else can see it.
- **No migration.** There are no deployments to migrate (tests only). No seed-from-env; a one-line boot warning lists any set-but-now-ignored legacy vars. Tests pre-write a store fixture.
- **`INSECURE_NO_AUTH` retires.** Its replacement — the open-vs-keyed toggle — lives in app config and affects **only `/v1` harness calls**. Every dashboard/UI/admin surface always requires a logged-in session post-setup; pre-setup, only `/health`, `/login` (redirects), and `/setup` are reachable.

**Explicitly NOT building (YAGNI):** per-role dashboard variants (all roles see the same observability tabs), audit log, secrets encryption at rest (the `/data` volume is the trust boundary — NIM keys already sit in `history.jsonl`'s directory and `.env` in plaintext; 0600 + atomic writes is the proportionate hardening), config-migration framework (a `version` field + `#[serde(default)]` is the entire story), file-watch live reload (the UI is the only writer), a new session system (reuse the existing HMAC cookie machinery, payload extended to carry the username), CSRF token subsystem (SameSite=Strict + `form-action 'self'` + JSON POSTs match existing posture), `ADMIN_RESET_TOKEN` backdoor (recovery = documented volume edit), username rename (password change only — usernames are the ownership key for NIM/client keys; renaming would need cascade for no real value), force-password-change-on-first-login flags, per-user API rate quotas, arc-swap/parking_lot deps (std RwLock).

## 1. Config inventory — what lives where

**Env (container-level only):** `HOST`, `PORT`, `DATA_DIR`, `RUST_LOG`, `TRUST_PROXY` (deployment topology: marks cookies Secure behind TLS-terminating proxy), `HISTORY_SAMPLE_SECS` (undocumented test knob).

**Store (UI-managed, all hot-apply):**

| Section | Fields (today's env equivalent) |
|---|---|
| upstream | `base_url` (NIM_BASE_URL), `nim_keys: Vec<{key, owner, enabled, rpm}>` (NIM_API_KEYS + RPM_PER_KEY, now **per-key**: default 40 in the add form; covers paid tiers/self-hosted NIM; `enabled` toggles a key in/out of the pool live with rate state preserved) |
| client_auth | `mode: open\|keyed` (replaces INSECURE_NO_AUTH posture), `keys: Vec<{name, secret_sha256, owner}>` (PROXY_API_KEYS) — client secrets stored as **SHA-256 digests** (server-generated 128-bit secrets need no slow KDF; verification hashes the inbound bearer and ct_eqs against digests; a leaked store never leaks usable bearer tokens) |
| limits | `max_wait_secs`, `heartbeat_secs`, `models_ttl_secs`, `stream_idle_secs`, `request_timeout_secs`, `max_inflight`, `strict_passthrough` |
| pricing | `ref_price_in`, `ref_price_out` |
| history | `days` (HISTORY_DAYS) |
| users | `[{username, password_hash, role: superuser\|admin\|user}]` (replaces ADMIN_PASSWORD) |

**Ownership:** each NIM key and each client key carries `owner: <username>`. Full secret values are never displayed to anyone after entry (masked `last4` + fingerprint only); non-admin users don't even see other users' key rows; admins see all rows (masked, owner-labeled) so they can manage the shared pool, but never values.

**Permission matrix:**

| Capability | user | admin | superuser |
|---|---|---|---|
| View all dashboard tabs | ✓ | ✓ | ✓ |
| Own account password | ✓ | ✓ | ✓ |
| Add NIM keys to the pool / remove·toggle·set-rpm on **own** keys | ✓ | ✓ | ✓ |
| Create / revoke **own** client API keys | ✓ | ✓ | ✓ |
| Remove/toggle **any** NIM key, revoke **any** client key | | ✓ | ✓ |
| Server settings (upstream, limits, pricing, history, open/keyed API mode, governor overrides) | | ✓ | ✓ |
| Create/delete users, reset passwords, set roles | | ✓ | ✓ |
| Deletable | ✓ | ✓ | **never** |

Deleting a user removes their NIM keys from the pool (live rebuild) and revokes their client keys — their harnesses stop; that's the point of deletion. Superuser is otherwise an admin; any admin may reset any password including the superuser's (no escalation exists — superuser is only a deletion guard).

**Module boundaries (the contracts):**
- `config.rs` — schema, validation, atomic persistence. No HTTP, no auth logic.
- `auth.rs` — identity: sessions, hashing, throttle. No storage; reads user records from config snapshots.
- Settings handlers (`main.rs`/`settings.rs`) — the **only config writer**: authz check → build candidate → validate → persist → swap → side effects, all serialized through one save-mutex (two admins saving concurrently can't lose updates).
- `pool.rs` — rate mechanics + `rebuild` with state carryover. Knows nothing about users; receives `(key, rpm)` pairs for enabled keys only.
- `proxy.rs` — data plane: takes one config snapshot per request, **never writes config**.
- `dashboard.html` — presentation only; receives role-filtered data (see visibility note below).

**Observability visibility contract (deliberate):** all roles see identical dashboard tabs, which means all authenticated users can read `/metrics` and `/api/history` — including each other's client names and token counts. That's the shared-pool-among-friends model, chosen explicitly.

## 2. Store (`src/config.rs`, new)

- **File**: `DATA_DIR/config.json` (sibling of `history.jsonl`; the compose volume already covers it — zero compose changes).
- **Typed via serde derive**: `serde = {version="1", features=["derive"]}` adds **zero new crates** — serde and the proc-macro stack (syn/quote/proc-macro2) are already in Cargo.lock via serde_json/tokio-macros. `#[derive(Serialize, Deserialize)] StoredConfig` with `#[serde(default)]` per section = forward-compatible field additions. `version: u32 = 1`; `version > 1` → refuse to boot cleanly.
- **Atomic writes**: serialize → write `config.json.tmp` opened with `.mode(0o600)` (`#[cfg(unix)]` via `OpenOptionsExt`) → `sync_all()` → `fs::rename` → fsync dir (`#[cfg(unix)]`). Delete stale `.tmp` at boot. (History today has none of this — fine for telemetry, required for credentials.)
- **Unwritable/corrupt store = hard boot error** (unlike history's degrade-to-memory). Rationale: silently degrading would let wizard-created credentials vanish on restart and reopen the setup-claim window — a fail-open regression. Corruption never falls through to setup mode (that would discard keys); operator restores or deliberately deletes the file.
- One shared `validate()` used by the wizard and every settings endpoint (bounds: per-key rpm 1–10000, heartbeat < max_wait, password ≥ 10 chars, client names globally unique + existing `sanitize_label` charset (they're metric labels), base_url http(s) etc.).

**Why JSON and not SQLite (directional decision, recorded in the ADR):** the store is kilobytes, read-mostly, single-writer (the settings handlers), and fully snapshot-cached in memory — a JSON file keeps recovery a text edit on the volume, backup a file copy, and adds zero binary weight; SQLite would add ~1MB of C to the 3.5MB static binary plus the schema-migration discipline we deliberately avoided, and buys nothing at friends-pool scale. The seam is already right: every consumer goes through `config.rs`/`StoredConfig`, so storage can swap without touching them. **Revisit triggers** (any one flips the answer to SQLite): per-user usage accounting or quotas (per-request writes), audit logging, hundreds of users, or a second concurrent writer.

## 3. Runtime application model

- `AppState.cfg` becomes `std::sync::RwLock<Arc<Config>>` with `fn cfg(&self) -> Arc<Config>` snapshot accessor; `proxy::handle` takes one snapshot per request (no torn reads; lock never held across await; arc-swap dep rejected — read path is nanoseconds vs seconds of network I/O). `Config` absorbs `clients` map, `max_inflight`, `rpm`, `user`.
- **Pool swap**: `pub type PoolHandle = Arc<RwLock<Arc<Pool>>>` + `Pool::rebuild(keys, rpm, old)` that carries over per-lane rate state (`sent` window, `cooldown_until`) by key-string match — a kept key can never be double-spent across a swap, and a lowered rpm is honored automatically (`try_take` checks `sent.len() < rpm` live). Race-free by construction: the dispatcher is the only caller of `reserve` (verified) and takes the read lock per iteration; rebuild happens under the write lock.
- **Grants carry their pool**: dispatcher replies `Slot {pool: Arc<Pool>, lane, key}` so bench/penalize/release route through the pool that granted them — no index-out-of-bounds after a swap; late ops on a retired pool are benign. Bounds-guard the affinity `prefer` index against the new lane count.
- `History.days` → `AtomicU64` (read per append already); `Arc<History>` moves into `AppState`.
- `/dash/config.json` becomes a live handler (today a boot-built string) — same JSON shape, dashboard JS unchanged.
- Side effects on save: nim-keys/rpm → pool rebuild; base_url → flush `models_cache` + clear `no_inject`.

## 4. Setup & auth flow

- **Phase = one `AtomicBool setup_required`**, true iff the store has **no superuser**. This doubles as total-lockout recovery: stop the container, delete only the superuser's entry from `config.json` on the volume, restart → the wizard re-creates the superuser while other users, keys, and settings survive. (Partial lockout needs no recovery — any admin resets any password.)
- **Pre-setup**: `/health` public (probe unaffected); `/v1/*` → 503 `{"code":"setup_required"}` (fail-closed preserved — nothing proxies); browsers → 302 `/setup`; `/login` → `/setup`. Post-setup: setup routes 404 (gated on the AtomicBool; axum routers are immutable).
- **Wizard**: client-side screens, **one atomic server POST** (`/setup {username, password, base_url?, nim_keys}`) — no half-configured server state, no abandonment cleanup. The created account is the **superuser**; its username owns the initial NIM keys. Per-key `POST /setup/validate-key` probes upstream pre-submit; success mints a session via the existing `Admin::sign`/`cookie` and 303s to Overview. If a store exists with keys but an emptied `users` array (lockout recovery), the wizard shows only the create-account screen.
- **Key validation probe**: factor `probe_key(http, base_url, key) -> Result<model_count, ProbeError>` out of `models()` (proxy.rs:776) — explicit key, bypasses pool/cache; `models()` keeps its pool path and calls it for the HTTP leg. Pre-auth probe abuse bounded by reusing the existing `Admin` throttle + 500ms sleep.
- **Password hashing**: PBKDF2-HMAC-SHA256, 600k iterations (OWASP), 16-byte getrandom salt, stored as `pbkdf2-sha256$600000$<salt>$<hash>` (iterations read back from the string — tunable without migration). Implemented as a ~15-line loop over the **already-installed** `hmac`+`sha2` (repo precedent: hand-rolled base64/urlencoding to avoid deps), pinned by RFC 7914 §11 test vectors; trivially swappable for the `pbkdf2` crate if review prefers. Verify in `spawn_blocking` (~200ms).
- **Sessions carry identity**: the cookie's signed payload extends from `expiry` to `expiry || hex(username) || first8(sha256(password_hash))` (same HMAC machinery). Role is **looked up from the config snapshot on every request**, never baked into the token — role changes and user deletion take effect immediately (a deleted user's cookie fails the lookup → 401/redirect), and the password-hash fragment means **changing or resetting a password invalidates that user's existing sessions** instantly.
- **Login**: page gains a username field; throttle/cookie flow unchanged. Prometheus scraper auth becomes `Bearer <username>:<password>` (or Basic) verified against the store; kept fast via an HMAC memo of the last verified credential (3 lines, cleared on password change) instead of 300ms PBKDF2 per scrape. `Admin` drops `password` entirely (credentials live in the store, hashed); sessions stay per-boot (existing ADR).
- **Role enforcement**: `require_admin` (rename to `require_session`) gates all UI/API surfaces for any logged-in user; server-setting and user-management endpoints additionally check `role != user`; ownership checks compare the session username against the key's `owner` (admins bypass). One small helper, not a framework.

## 5. Admin API (all behind `require_admin`)

Section-scoped POSTs (not PUT-the-world — avoids the masked-secret round-trip problem; one clear side effect per save). Uniform pipeline: **build candidate → validate() → save to disk → swap snapshot → side effects** (disk failure = nothing applied).

- `GET /api/config` — config **filtered by role**: users get server settings read-only-relevant bits (for display) plus only their OWN nim/client key rows; admins get all rows, owner-labeled. Secrets always masked (`{fingerprint, last4, owner}`; fingerprint = first 8 hex of SHA-256(key), no new id field), never password hashes.
- `POST /api/settings/nim-keys` — any role; `{add: {key, rpm?}}` records `owner = session user` (rpm defaults 40, bounds 1–10000); `{remove: fingerprint}`, `{set: {fingerprint, enabled?, rpm?}}` allowed for own keys (any role) or any key (admin+). Any change triggers pool rebuild (enabled keys only feed the pool; disabled keys keep their stored state and re-enable warm via key-string carryover).
- **Invariant: the superuser always has ≥1 enabled NIM key** (established by the wizard, enforced by `validate()`). This pins the pool floor to the one undeletable account and trickles down: every other user (and admin) can freely remove/disable ALL of their own keys with zero special cases; user deletion can never empty the pool. Removing/disabling the superuser's last enabled key → 400. Recovery-wizard completion reassigns any orphan-owned keys (dangling owner after a volume-edit) to the new superuser, restoring the invariant.
- `POST /api/settings/clients` — key add/revoke: any role, own keys (admins: any); `owner` recorded; secret generated server-side (getrandom), returned **exactly once**. Mode toggle (`open|keyed`): admin+.
- `POST /api/settings/upstream|limits|pricing|history` — **admin+**; per §1 sections (rpm is now per-key, not here).
- **Dashboard ripple from per-key rpm**: `/dash/config.json` (live) serves `capacity_rpm` = Σ enabled-lane rpms + per-lane `{rpm}` list; the dashboard's four `lanes × rpm` sites, lane-utilization normalization, and the "keys for peak" tile switch to it ("keys for peak" becomes an rpm-shortfall readout: `peak − capacity`, phrased as "≈ N more keys @ 40").
- `POST /api/settings/users` — **admin+**: `{add: {username, password, role: admin|user}}`, `{remove: username}` (never the superuser; removing a user drops their NIM keys from the pool + revokes their client keys), `{reset_password: {username, new_password}}`, `{set_role: {username, role}}` (superuser's role immutable).
- `POST /api/settings/account` — any role: own password change; **requires current_password re-auth** regardless of session.
- `POST /api/settings/validate-key` — any role; authed twin of the setup probe.

## 6. Settings page IA + wizard screens (handoff to the design pass)

New **Settings** sidebar entry (gear icon, after Capacity), same tab machinery/esc()/CSP discipline. Cards, each with its own Save mapping 1:1 to an endpoint, inline success/error. **Role-conditional rendering**: users see cards 1–3 (own-keys scope) + 7; admins additionally see 4–6, 8, and owner columns. The dashboard tabs themselves are identical for every role.

*Visible to every role:*
1. **My NIM Keys** — per-key row: masked key (`nvapi-••••abcd`), **enabled toggle**, **rpm** (inline-editable number, default 40), lane index, live in-window/cooldown state, Validate (inline ✓ n models / ✗ status) and Remove (confirm; toggle+remove blocked when it's the pool's last *enabled* key). Add form: password-style input with reveal + rpm field, "Validate & add" (+"Add anyway" if upstream down). Callout: "Keys you add join the shared pool as your keys; only you (and admins) see they exist. Changes apply live — kept keys keep their rate windows; disabled keys re-enable warm." For admins the card is **NIM Keys** and lists every key with an owner column.
2. **My API Keys** (client keys) — list: name, masked secret (last-4), Remove. Add → name + Generate → modal shows the secret exactly once with copy button + "you won't see this again". Admin view adds owner column + revoke-any.
3. **Connection help** (read-only mini-card) — the base URL clients should point at + current mode (open/keyed), so a user can self-serve their harness setup.
7. **Account** — username (display only), current password (always required), new password + confirm.

*Admin-only:*
4. **API Access mode** — Open/Keyed toggle (Open = "anyone who can reach /v1 — localhost/VPN only"; switching to Keyed with zero client keys prompts to create one).
5. **Upstream & Limits** — `base_url` (help: saving clears the model-catalog cache); max_wait, heartbeat, stream_idle, request_timeout, models_ttl, max_inflight, strict_passthrough toggle. "All limits apply to new requests immediately." (rpm lives on each key row, not here.)
6. **Pricing & History** — ref_price_in/out ($/1M, cosmetic, feeds "dollars saved"); history retention days (0 = forever; ≈35 MB/30d).
8. **Users** — table: username, role chip (superuser/admin/user), key counts (n NIM · n client), actions: Reset password, Change role, Delete (never shown for superuser; confirm dialog warns "their NIM keys leave the pool and their API keys stop working"). Add form: username + initial password + role.

**Wizard** (`/setup`, login-card styling, 3 client-side screens → one POST): ① Create your account (username/password/confirm; copy: "This is the superuser — it can never be deleted") ② Add NIM keys (same add/validate UX as Card 1; Advanced disclosure for base_url; Continue needs ≥1 key; screen skipped in lockout-recovery when the store already has keys) ③ Review & finish → POST → Overview, logged in.

## 6b. Model-pressure governor (new — from field testing against Nemotron 3 Super 120B)

**Problem observed**: `ResourceExhausted: Worker local total request limit reached (32/32)` during agentic workloads — NIM's serving stack has a worker-concurrency cap **orthogonal to** the 40 RPM key limit. At 40 RPM with 45–90s generations, steady-state in-flight is 30–60: you saturate the worker pool while fully RPM-legal.

**Key insight — scope**: worker exhaustion is **model-scoped, shared across all keys** (and all of NIM's users). The current proxy would misclassify it as a retryable status, bench the lane, and fail over to another key — which can't help (same saturated workers) and burns healthy key capacity. Fix the classification first, then govern.

**Mechanism**:
- `classify(status, body)` in the retry path: worker-exhaustion signature (status + `Worker local total request limit` body sniff, available on the pre-stream error response) routes to per-model backoff — **never a lane bench**. Plain 429/5xx behavior unchanged.
- A per-model admission gate beside the RPM dispatcher: a request needs an RPM slot AND a model permit (`governor.rs`: per-model `{limit: AtomicUsize, inflight: AtomicUsize, last_exhausted, stable_since}`). Waiting requests ride the existing heartbeat machinery — no new client-facing behavior.
- **Adaptive-by-default (zero config)**: each model starts ungoverned; on first worker-exhausted error the governor engages at `max(1, inflight_at_error / 2)`, climbs +1 per stable minute, and dissolves back to ungoverned after a long clean period (e.g. 30 min). Rationale: the worker pool is shared infrastructure — other tenants' load moves the real ceiling, so static caps are wrong in both directions; AIMD tracks reality. Store gains an optional `models: {overrides: {model_id: max_concurrency}, governor_enabled: bool}` for operators who know better (admin-only Settings sub-card under Upstream & Limits).
- **Metrics**: `nimproxy_worker_exhausted_total{model}`, gauges `nimproxy_model_inflight{model}` / `nimproxy_model_limit{model}` (0 = ungoverned). Reliability tab can surface "model pressure" later (not in this epic's UI scope beyond the settings card).
- **Test scaffolding**: `mock_nim.py --worker-slots N` (per-model in-flight cap emitting the real error string) + a slow-generation load-test scenario asserting the governor converges (bounded exhaustion errors, no thrash, no client-visible failures) alongside the existing zero-RPM-violation gate.

## 7. Compat & test strategy (no migration — confirmed: no deployments exist)

- **No seed-from-env.** Legacy app-level env vars are simply ignored; one boot log line lists any that are set ("ignoring legacy env vars — settings live in the dashboard"). `.env.example` shrinks to the 5 container vars.
- **E2E fixture = pre-written store**: `tests/support` gains `write_store(dir, users, nim_keys, mode, ...)` that serializes a `StoredConfig` into a tempdir `DATA_DIR/config.json` before boot (each proxy gets its own tempdir, cleaned on Drop). Test password hashes use a low iteration count — the count is encoded in the hash string, so `verify_password` honors it with zero prod impact. `expect_refuses_to_start` retargets to exit-nonzero for unwritable DATA_DIR / corrupt store / `version>1`; a new `setup_mode` helper asserts the claimably-closed state (healthy, `/v1`→503, `/`→302 `/setup`). `start_proxy_fresh` + `complete_setup()` (drives POST /setup, returns a cookie'd client) cover wizard tests; role tests boot with a 3-user fixture.

## 8. Phases (5 PRs)

1. **Store + runtime plumbing, zero UX change**: `src/config.rs` (schema incl. users/roles/ownership, load/save/validate, atomic write, 0600), serde derive feature, `RwLock<Arc<Config>>` snapshots through proxy.rs, `PoolHandle`+`rebuild`+`Slot`, `History.days` atomic, `probe_key` factoring, live `/dash/config.json`, PBKDF2 (RFC vectors) — env boot path **kept** this PR (reads env into the same structs) so e2e churn is mechanical (tempdir DATA_DIR + store-precedence tests). *Verify*: PBKDF2 vectors; atomic-save crash sim; rebuild state-carryover unit tests; full e2e; **load test with mid-run pool rebuilds — zero-violation invariant**.
2. **Model-pressure governor** (independent of the UX PRs; wants config snapshots from PR 1): `governor.rs` + `classify()` + metrics; `mock_nim.py --worker-slots`; slow-generation load scenario. *Verify*: misclassification test (worker-exhausted never benches a lane), AIMD convergence under the mock, zero RPM violations still hold, no governor engagement on clean upstreams.
3. **Setup phase + wizard + store-only boot (env app-vars retired, INSECURE_NO_AUTH deleted)**: AtomicBool gate, 503 pre-setup, `/setup` page (include_str, login-card style) + endpoints + throttled probe, username+password login, sessions carrying username, role lookup per request. *Verify*: fresh boot claimably-closed; wizard happy path → logged-in superuser on Overview, store 0600; setup 404s post-setup; lockout-recovery path.
4. **Settings admin API + roles**: all endpoints incl. `/api/settings/users`, ownership + role gating, masking, persist→swap→side-effects, account re-auth, scraper-credential memo. *Verify*: per-endpoint e2e incl. role denials (user hitting admin endpoints → 403; user removing another's key → 403; superuser undeletable; user deletion drops their keys from pool + revokes clients), remove-last-key 400, base_url cache flush, rpm applies next request; load test again.
5. **Settings UI + wizard polish + docs/knowledge**: dashboard Settings tab per §6 with role-conditional cards; new ADR `ui-managed-config-store.md` (covers multi-user model + ownership + no-encryption rationale + claim-risk acceptance); amend `auth-posture` ADR; rewrite `ops/configure-env.md` (5 container vars); update `.env.example`, README (quickstart → first-run wizard; sharing-with-friends flow now "create them a user"), `deploy-docker.md` (volume backup now includes config.json), `client-auth.md`, `ops/sharing-with-friends.md`, `examples/README.md`, CHANGELOG, log.md. *Verify*: Playwright walkthrough as superuser/admin/user (each sees the right cards) + restart round-trip; binary-size check vs ~3.5MB budget (expect +100–200KB, measured and recorded).

## 9. Risks

- **Pool swap concurrency** — structurally eliminated (§3); late bench/release on retired pool benign.
- **Setup-phase claiming** — exposed unconfigured instance claimable by first visitor. Accepted (matches Grafana/Portainer): /v1 closed pre-setup, compose binds loopback, loud "SETUP REQUIRED — first visitor becomes admin" boot log, docs say finish setup immediately; upgraders bypass via seed. No claim token.
- **Lockout** — stop container, delete `users` entry from `config.json` on the volume (scratch image has no shell: `docker run --rm -v nimproxy_history:/data alpine vi /data/config.json`), restart → account-only wizard, keys intact. Documented, not tooled.
- **Store corruption** — near-impossible via atomic writes; hard error, never silent setup fallback.
- **Role/ownership bypass** — enforcement is server-side in every endpoint; `GET /api/config` filters **before serialization**, so a user-role session's response simply contains no server-settings values and no other users' key rows — DOM/CSS tampering reveals empty containers, not data. E2E asserts this by diffing the raw JSON bodies per role. Key values are never returned by any endpoint after creation, so "no one else can see it" holds even against an admin.
- **Lost updates from concurrent admins** — all config writes serialize through one save-mutex (build→validate→persist→swap is a critical section); last-writer-wins within a section is acceptable at this scale (no ETag machinery — YAGNI).
- **Login-throttle DoS** — the failed-login limiter stays per-process (10/min shared): one attacker can throttle all users' logins for 60s. Unchanged posture (docs: reverse proxy does per-IP limiting); revisit only if it bites.
- **Zero enabled keys** — structurally impossible: the superuser-key invariant guarantees the pool floor.

## Verification (end-to-end, final)

Full `cargo test` + clippy/fmt; `mock_nim.py --enforce` + `loadtest.py` with settings-driven rebuilds mid-run (zero upstream violations is the hard gate); Playwright: fresh-install wizard walkthrough (bad password, failed key validation, happy path), every Settings card save + container-restart round-trip, generate-secret-shown-once, key removal during an in-flight stream.

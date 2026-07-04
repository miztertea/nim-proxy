---
type: Decision
title: UI-managed config store with a first-run setup wizard
description: App-level config moves from env vars into a JSON store owned by the app, edited from the dashboard, claimed by a first-run wizard; multi-user with per-key ownership.
tags: [configuration, auth, multi-user, security, storage]
timestamp: 2026-07-04T00:00:00Z
---

# UI-managed config store with a first-run setup wizard

## Context

Every app-level setting used to live in a flat env var read once at boot —
[configure-env](../ops/configure-env.md) even stated "no config files, no
reload" as philosophy. That was fine for one operator with a `.env`, but it
made three things awkward: a friend contributing a NIM key meant editing the
server owner's `.env` and restarting; nobody could see who owned which key or
how much each was consuming; and every knob change was a redeploy. v0.6.0
supersedes that philosophy deliberately: app-level config moves *into* the
application — a persistent store the app owns, a Settings surface in the
dashboard that writes it, and a first-launch wizard that claims a fresh
install. Env shrinks to container-level concerns only
([configure-env](../ops/configure-env.md)), which also makes running outside a
container natural.

There are no deployments to migrate (tests only), so there is **no
seed-from-env and no migration framework** — legacy app-level env vars are
ignored with a one-line boot warning listing any still set.

## Options

Three independent decisions had to be locked before implementation.

### (a) Storage format — SQLite vs. a JSON file

1. **SQLite** — a real embedded database with schema migrations.
2. **A single JSON file** (chosen).

### (b) Access model — single operator vs. multi-user

1. **One shared admin** (status quo: `ADMIN_PASSWORD`).
2. **Multi-user with three roles and per-key ownership** (chosen).

### (c) Secrets at rest — encrypt or not

1. **Encrypt the store** (a passphrase / KMS envelope).
2. **Plaintext file, 0600, atomic writes; client secrets as digests** (chosen).

And one risk to accept or design around: an unconfigured instance is
**claimable by its first visitor** during the setup window.

## Choice

### JSON file, not SQLite

`DATA_DIR/config.json` (sibling of `history.jsonl` — the compose volume
already covers it, zero compose changes). The store is **kilobytes,
read-mostly, single-writer** (the settings handlers), and fully
**snapshot-cached in memory** (`RwLock<Arc<Config>>`, one snapshot per
request). A JSON file keeps recovery a text edit on the volume, backup a file
copy, and adds **zero binary weight** — serde derive pulls in no new crates
(the proc-macro stack is already in `Cargo.lock` via serde_json). SQLite would
add ~1MB of C to the ~3.5MB static binary plus the schema-migration discipline
we deliberately avoid, and buys nothing at friends-pool scale. The seam is
right regardless: every consumer goes through `config.rs`/`StoredConfig`, so
storage can swap without touching them.

Forward compatibility is a `version: u32` field (`1` today) plus
`#[serde(default)]` on every section — new fields land without a migration; a
`version` the build doesn't understand refuses to boot rather than silently
dropping unknown keys.

**Revisit triggers** (any one flips the answer to SQLite): per-user usage
accounting or quotas (per-request writes), audit logging, hundreds of users,
or a second concurrent writer.

### Multi-user with ownership

Three roles: **superuser** = an admin that can never be deleted (so you can't
delete the last admin — a deletion guard only, no extra powers); **admin** =
server settings + user management; **user** = own account, own client API
keys, own NIM keys. Dashboards are identical for every role — only the
Settings surface differs, and it is **filtered server-side** in
`GET /api/config` (hidden sections are absent from the payload, not hidden by
CSS), so DOM tampering reveals empty containers, not data.

Each NIM key and each client key carries `owner: <username>`. Users see only
their own key rows; admins see all rows (masked, owner-labeled) to manage the
shared pool, but never key values. This is the motivating case: a friend adds
their NIM key to the shared pool safely — nobody else, not even an admin, can
read it back. **Invariant**: the superuser always owns ≥1 enabled NIM key (the
pool floor), so user deletion or a user disabling all their own keys can never
empty the pool; removing/disabling the superuser's last enabled key is a 400.

Sessions reuse the existing HMAC-cookie machinery, payload extended to carry
the username plus a fragment of the password hash; role is looked up from the
config snapshot on every request, never baked into the token. See the
[auth-posture amendment](auth-posture-and-dashboard-password.md) for the
identity mechanics.

### No encryption at rest

The **`/data` volume is the trust boundary.** NIM keys already sat there in
`history.jsonl`'s directory and in `.env` in plaintext; anyone who can read the
volume already had the keys. Encrypting the store would need a key to live
*somewhere* — another env var, another file on the same volume — which moves
the problem without solving it. The proportionate hardening is **0600
permissions + atomic writes** (tmp + `fsync` + `rename` + dir `fsync`), which
the credential store needs anyway (unlike telemetry, a torn write here loses
logins). Client API-key secrets get defense-in-depth for free: they're
server-generated 128-bit tokens stored only as **SHA-256 digests** (+ last-4
for display), so a leaked store leaks no usable bearer tokens — and no slow KDF
is needed because the secret has full entropy (unlike passwords, which use
PBKDF2). An unreadable or corrupt store is a **hard boot error**, never a
silent fall-through to setup mode — degrading would discard keys and reopen the
claim window (see below).

### Setup-claim risk: accepted

A fresh install has no superuser, so `setup_required` is true: `/health` stays
public, `/v1/*` answers `503 {"code":"setup_required"}` (fail-closed — nothing
proxies), and browsers land on `/setup`, a wizard whose single atomic POST
creates the superuser, records ≥1 validated NIM key, and mints a session. The
window between first boot and completing that wizard is claimable by whoever
reaches it first. **Accepted** — it matches Grafana / Portainer first-run: the
data plane is closed pre-setup, compose binds loopback, and boot logs a loud
`SETUP REQUIRED — the FIRST VISITOR becomes the superuser` line telling the
operator to finish setup immediately. No claim token — it would be one more
secret to route to the operator for a window measured in seconds, with no
deployments upgrading into it. The one pre-auth surface in that window, the
wizard's key-validation probe (`/setup/validate-key`, which fetches the
operator-supplied upstream to confirm a key), rejects link-local targets
(`169.254.0.0/16`, `fe80::/10`) so it can't be turned into a cloud-metadata
SSRF oracle — loopback and RFC1918 stay allowed because local and LAN
self-hosted NIM are real upstreams (`config::check_base_url`).

## Consequences

- App-level env vars are **retired**; only `HOST`, `PORT`, `DATA_DIR`,
  `RUST_LOG`, `TRUST_PROXY` remain (container-level). Setting a retired var
  logs a one-line boot warning and is otherwise ignored. See
  [configure-env](../ops/configure-env.md).
- The `/data` volume now holds **credentials** (`config.json`, 0600). Backups
  of the volume now contain secrets — treat them accordingly
  ([deploy-docker](../ops/deploy-docker.md)).
- **Total-lockout recovery is a documented volume edit**, not a tool: stop the
  container, empty the `users` array in `config.json` on the volume (the
  scratch image has no shell —
  `docker run --rm -it -v <volume>:/data alpine vi /data/config.json`),
  restart → the wizard re-creates the superuser while keys/settings survive and
  the new superuser adopts any orphaned keys. Partial lockout needs no recovery
  — any admin resets any password.
- All config writes serialize through one save-mutex (build candidate →
  `validate()` → persist → swap snapshot → side effects); a disk failure
  applies nothing, and two admins saving concurrently can't lose updates.
  Last-writer-wins *within a section* is accepted at this scale (no ETag
  machinery — YAGNI).
- `INSECURE_NO_AUTH` retires; its replacement is the store's `open|keyed` API
  mode, which affects **only `/v1`** — every dashboard/observability surface
  always requires a logged-in session. See the
  [auth-posture amendment](auth-posture-and-dashboard-password.md).

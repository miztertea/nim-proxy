#!/usr/bin/env node
/*
 * Render the embedded dashboard against Rust-generated API fixtures, drive it
 * the way an operator does, and fail on any uncaught page exception.
 *
 * This exists because `cargo test` asserts on served HTML *text* and never
 * parses or executes the page JavaScript, and `node --check` proves only that
 * the syntax parses. Every page bug this project has shipped got past both.
 * The specific bug that motivated this file threw on chart hover, and because
 * the poll loop's catch treats any throw as "connection lost", a healthy proxy
 * rendered a red "Disconnected" badge and froze most of the tab.
 *
 * Stdlib only, matching scripts/formatter_fixture.js: no package.json, no npm
 * install, no Playwright. Node 22 ships a WebSocket client, so the Chrome
 * DevTools Protocol is reachable with nothing added to the repo.
 *
 * Usage:
 *   node scripts/render_check.js              # fail on page errors
 *   node scripts/render_check.js --locale en-XA   # also report untranslated runs
 *   CHROME=/path/to/chrome node scripts/render_check.js
 */

'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const vm = require('vm');
const net = require('net');
const { spawn, execFileSync } = require('child_process');

const ROOT = path.resolve(__dirname, '..');
const FIXTURES = path.join(ROOT, 'tests', 'fixtures', 'ui');

const args = process.argv.slice(2);
const PRESENTATION_CSP = "default-src 'none'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/*
 * Task 8's browser matrix is data before it is driver code. Every row names
 * one operator-observable result and, for mutations, the exact request that
 * must leave the page. The RED phase intentionally has no observations for
 * these rows; --all-states reaches the real served application first and then
 * reports each missing row by stable id.
 */
const INTERACTION_ROWS = [
  /* Mutation caught by the observation-quality state rows below: rendering
     absent/invalid observations as zero, omitting a closed result state, or
     coupling that state to history-window completeness. The expected health
     row text is literal catalog English, not a dashboard helper result. */
  {
    id: 'startup-dashboard-healthy',
    category: 'startup',
    state: 'healthy',
    action: 'navigate to / and resolve bootstrap, config, catalog, and dashboard fixtures',
    visible: '#tab-overview is visible after catalog and dashboard fixtures resolve',
    dom: [{ selector: '#tab-overview', property: 'hidden', equals: false }],
    visual: [{
      case: 'dashboard-healthy', locales: ['en-US', 'en-XA'], roles: ['superuser'],
      surfaces: ['dashboard-tabs', 'settings-panels'],
    }],
  },
  {
    id: 'startup-setup-healthy',
    category: 'startup',
    state: 'healthy',
    action: 'navigate to /setup and resolve bootstrap and the public catalog',
    visible: '#step1 is visible after the public catalog resolves',
    dom: [{ selector: '#step1', property: 'hidden', equals: false }],
    visual: [
      { case: 'setup-step1', locales: ['en-US', 'en-XA'], roles: ['public'], surfaces: ['setup-step1'] },
      { case: 'setup-client-validation-error', locales: ['en-US'], roles: ['public'], surfaces: ['setup-client-validation-error'] },
    ],
  },
  {
    id: 'startup-login-healthy',
    category: 'startup',
    state: 'healthy',
    action: 'navigate to /login and resolve bootstrap and the public catalog',
    visible: 'the login form is visible after the public catalog resolves',
    dom: [{ selector: 'form.login-card', property: 'exists', equals: true }],
    visual: [{ case: 'login-healthy', locales: ['en-US', 'en-XA'], roles: ['public'], surfaces: ['login-healthy'] }],
  },
  {
    id: 'login-invalid-credentials',
    category: 'startup',
    state: 'invalid-credentials',
    action: 'submit invalid credentials and follow the server-rendered invalid_credentials state',
    visible: 'the catalog-backed invalid-credentials error is visible',
    dom: [
      { selector: '#login-error', property: 'hidden', equals: false },
      { selector: '#login-error', property: 'textContent', equals: 'Incorrect username or password.' },
    ],
    visual: [{ case: 'login-invalid-credentials', locales: ['en-US'], roles: ['public'], surfaces: ['login-invalid-credentials'] }],
  },
  {
    id: 'navigation-dashboard-tabs',
    category: 'navigation',
    state: 'healthy',
    action: 'activate every #side nav tab button',
    visible: 'each selected dashboard tab is the only visible main section',
    dom: [{
      selector: 'main > section:not([hidden])',
      property: 'id-sequence',
      equals: ['tab-overview', 'tab-models', 'tab-clients', 'tab-reliability', 'tab-capacity', 'tab-settings'],
    }],
  },
  {
    id: 'navigation-settings-role-panels',
    category: 'navigation',
    state: 'roles-superuser-admin-user',
    action: 'enter Settings and activate every role-authorized subnavigation button',
    visible: 'Settings subnavigation exposes only role-authorized panels',
    dom: [{
      selector: '#setnav button[aria-current="page"]',
      property: 'data-sub-sequence-by-role',
      equals: {
        admin: ['access', 'server', 'users', 'account'],
        superuser: ['access', 'server', 'users', 'account'],
        user: ['access', 'account'],
      },
    }],
  },
  {
    id: 'sorting-table-ascending',
    category: 'sorting',
    state: 'healthy',
    action: 'activate a sortable table header once',
    visible: 'the selected column is ascending and its row order changes',
    dom: [{
      selector: '#table-models tbody tr',
      property: 'first-cell-sequence',
      equals: ['fixture/alpha', 'fixture/zeta'],
    }],
  },
  {
    id: 'sorting-table-descending',
    category: 'sorting',
    state: 'healthy',
    action: 'activate the same sortable table header a second time',
    visible: 'the second header activation reverses the row order',
    dom: [{
      selector: '#table-models tbody tr',
      property: 'first-cell-sequence',
      equals: ['fixture/zeta', 'fixture/alpha'],
    }],
  },
  {
    id: 'sorting-table-persists-rerender',
    category: 'sorting',
    state: 'healthy',
    action: 'fulfill the next dashboard-now poll after sorting descending',
    visible: 'the descending sort order survives the dashboard rerender',
    dom: [{
      selector: '#table-models tbody tr',
      property: 'first-cell-sequence',
      equals: ['fixture/zeta', 'fixture/alpha'],
    }],
    request: {
      method: 'GET',
      path: '/api/dashboard/now',
      query: {},
      body: null,
    },
  },
  {
    id: 'filtering-settings-role',
    category: 'filtering',
    state: 'role-user',
    action: 'load Settings from an ordinary-user ConfigResponse',
    visible: 'Server and Users are absent for an ordinary-user response',
    dom: [
      { selector: '#setnav [data-sub="server"]', property: 'exists', equals: false },
      { selector: '#setnav [data-sub="users"]', property: 'exists', equals: false },
    ],
  },
  {
    id: 'refresh-dashboard-now',
    category: 'refresh',
    state: 'healthy',
    action: 'fulfill the next dashboard-now poll with changed values',
    visible: 'a changed now fixture updates the Now values',
    dom: [{ selector: '#uptime', property: 'textContent-changed', equals: true }],
    request: {
      method: 'GET',
      path: '/api/dashboard/now',
      query: {},
      body: null,
    },
  },
  {
    id: 'refresh-settings-access',
    category: 'refresh',
    state: 'healthy',
    action: 'hold focus in Access and fulfill the next config refresh with changed lane state',
    visible: 'the five-second access refresh updates lane state without losing focus',
    dom: [
      { selector: '[data-ksfp="f17e0001"]', property: 'textContent', equals: '1 / 40 in window' },
      { selector: 'document', property: 'activeElement', equals: '#nk-key' },
    ],
    request: {
      method: 'GET',
      path: '/api/config',
      query: {},
      body: null,
    },
  },
  {
    id: 'history-window-preset',
    category: 'history-window',
    state: 'healthy',
    action: 'activate the 1h preset button',
    visible: 'the selected preset and range summary update',
    dom: [
      { selector: '#ranges [data-range="3600"]', property: 'aria-pressed', equals: 'true' },
      { selector: '#rangeinfo', property: 'textContent-includes', equals: '1h' },
    ],
    request: {
      method: 'GET',
      path: '/api/dashboard',
      query: { from: '1700000000', points: '288', to: '1700003600' },
      body: null,
    },
  },
  {
    id: 'history-window-custom',
    category: 'history-window',
    state: 'healthy',
    action: 'enter fixed from/to values and activate Apply',
    visible: 'the applied absolute range is shown',
    dom: [
      { selector: '#ranges [data-range="custom"]', property: 'aria-pressed', equals: 'true' },
      { selector: '#rangeinfo', property: 'textContent-includes', equals: 'Absolute' },
    ],
    request: {
      method: 'GET',
      path: '/api/dashboard',
      query: { from: '1700000100', points: '288', to: '1700000200' },
      body: null,
    },
  },
  {
    id: 'history-window-freeze',
    category: 'history-window',
    state: 'healthy',
    action: 'activate the live control once',
    visible: 'freeze holds the selected window without requesting a new range',
    dom: [{ selector: '#liveText', property: 'textContent', equals: 'Absolute' }],
    requests: [],
  },
  {
    id: 'history-window-resume',
    category: 'history-window',
    state: 'healthy',
    action: 'activate the live control again after freezing',
    visible: 'resume follows the next fixed now boundary and requests that range once',
    dom: [{ selector: '#liveText', property: 'textContent', equals: 'Live' }],
    request: {
      method: 'GET',
      path: '/api/dashboard',
      query: { from: '1700000000', points: '288', to: '1700003600' },
      body: null,
    },
  },
  {
    id: 'dialog-client-secret-open',
    category: 'dialog',
    state: 'modal-client-secret',
    action: 'enter a client key name and activate Generate key',
    visible: 'the one-time client secret and Done control are visible',
    dom: [
      { selector: '#modal', property: 'class-contains', equals: 'show' },
      { selector: '#modal-done', property: 'exists', equals: true },
      { selector: '#modal-body .secretbox', property: 'textContent', equals: 'npk_fixture_secret' },
    ],
    request: {
      method: 'POST',
      path: '/api/settings/clients',
      body: { add: { name: 'fixture-client' } },
    },
  },
  {
    id: 'dialog-client-secret-close',
    category: 'dialog',
    state: 'modal-client-secret',
    action: 'activate Done in the one-time client-secret dialog',
    visible: 'the one-time secret dialog closes explicitly',
    dom: [{ selector: '#modal', property: 'class-contains-show', equals: false }],
    requests: [],
  },
  {
    id: 'dialog-client-secret-escape-focus-return',
    category: 'keyboard-focus',
    state: 'modal-client-secret',
    action: 'press Escape in the one-time client-secret dialog after Settings rerenders',
    visible: 'Escape closes the native dialog and returns focus to the current Generate key control',
    dom: [
      { selector: '#modal', property: 'class-contains-show', equals: false },
      { selector: 'document', property: 'activeElement', equals: '#ck-add' },
    ],
    requests: [],
  },
  {
    id: 'dialog-confirm-cancel-focus-return',
    category: 'keyboard-focus',
    state: 'modal-confirm',
    action: 'focus a destructive control, activate it, and dismiss confirm with Escape',
    visible: 'Escape cancels the destructive action and returns focus to its trigger',
    dom: [
      { selector: '[data-kdel="0"]', property: 'exists', equals: true },
      { selector: 'document', property: 'activeElement', equals: '[data-kdel="0"]' },
    ],
  },
  {
    id: 'mutation-nim-key-create',
    category: 'create',
    state: 'mutation-success',
    action: 'enter a NIM key and activate Validate and add',
    visible: 'the added NIM key row is visible after validation',
    dom: [{ selector: '[data-ksfp="f17e0001"]', property: 'exists', equals: true }],
    requests: [
      {
        method: 'POST',
        path: '/api/settings/validate-key',
        query: {},
        body: { key: 'nvapi-ui-fixture' },
      },
      {
        method: 'POST',
        path: '/api/settings/nim-keys',
        query: {},
        body: { add: { key: 'nvapi-ui-fixture', rpm: 40 } },
      },
    ],
  },
  {
    id: 'mutation-nim-key-rpm',
    category: 'edit',
    state: 'mutation-success',
    action: 'change the NIM key rpm input to 41',
    visible: 'the NIM key row shows the updated rpm',
    dom: [{ selector: '[data-rpm="0"]', property: 'value', equals: '41' }],
    request: {
      method: 'POST',
      path: '/api/settings/nim-keys',
      body: { set: { fingerprint: 'f17e0001', rpm: 41 } },
    },
  },
  {
    id: 'mutation-nim-key-toggle',
    category: 'edit',
    state: 'mutation-success',
    action: 'activate the NIM key enabled switch',
    visible: 'the NIM key row shows disabled state',
    dom: [{ selector: '[data-tog="0"]', property: 'aria-pressed', equals: 'false' }],
    request: {
      method: 'POST',
      path: '/api/settings/nim-keys',
      body: { set: { enabled: false, fingerprint: 'f17e0001' } },
    },
  },
  {
    id: 'mutation-nim-key-delete',
    category: 'delete',
    state: 'mutation-success',
    action: 'activate Remove key and accept confirm',
    visible: 'the removed NIM key row is absent',
    dom: [{ selector: '[data-ksfp="f17e0001"]', property: 'exists', equals: false }],
    request: {
      method: 'POST',
      path: '/api/settings/nim-keys',
      body: { remove: 'f17e0001' },
    },
  },
  {
    id: 'mutation-client-key-delete',
    category: 'delete',
    state: 'mutation-success',
    action: 'activate Revoke and accept confirm',
    visible: 'the revoked client key row is absent',
    dom: [{ selector: '[data-ckdel]', property: 'fixture-client-present', equals: false }],
    request: {
      method: 'POST',
      path: '/api/settings/clients',
      body: { remove: 'fixture-client' },
    },
  },
  {
    id: 'mutation-client-auth-mode',
    category: 'edit',
    state: 'mutation-success',
    action: 'activate API key required in Server settings',
    visible: 'API key required is the selected client-auth mode',
    dom: [{ selector: '[data-mode="keyed"]', property: 'aria-pressed', equals: 'true' }],
    request: {
      method: 'POST',
      path: '/api/settings/clients',
      body: { mode: 'keyed' },
    },
  },
  {
    id: 'mutation-server-limits',
    category: 'edit',
    state: 'mutation-success',
    action: 'change the upstream base URL and activate Save in Upstream and limits',
    visible: 'the saved limit values remain visible',
    dom: [
      { selector: '#sv-base', property: 'value', equals: 'https://fixture.invalid/v1' },
      { selector: '#limits-err', property: 'textContent', equals: 'Saved.' },
    ],
    requests: [
      {
        method: 'POST',
        path: '/api/settings/upstream',
        query: {},
        body: { base_url: 'https://fixture.invalid/v1' },
      },
      {
        method: 'POST',
        path: '/api/settings/limits',
        query: {},
        body: {
          heartbeat_secs: 10,
          max_inflight: 512,
          max_wait_secs: 900,
          models_ttl_secs: 600,
          request_timeout_secs: 300,
          stream_idle_secs: 300,
          strict_passthrough: false,
        },
      },
    ],
  },
  {
    id: 'mutation-server-history',
    category: 'edit',
    state: 'mutation-success',
    action: 'change the history fields and activate Save',
    visible: 'Saved is visible beside History and dashboard',
    request: {
      method: 'POST',
      path: '/api/settings/history',
      body: {
        days: 30,
        default_window_days: 7,
        slo_target_percent: 99.9,
      },
    },
  },
  {
    id: 'mutation-governor-toggle',
    category: 'edit',
    state: 'mutation-success',
    action: 'activate the governor enabled switch',
    visible: 'the governor switch reflects the saved state',
    dom: [{ selector: '#gov-tog', property: 'aria-pressed', equals: 'false' }],
    request: {
      method: 'POST',
      path: '/api/settings/governor',
      body: { enabled: false },
    },
  },
  {
    id: 'mutation-governor-override-create',
    category: 'create',
    state: 'mutation-success',
    action: 'enter a model and cap and activate override',
    visible: 'the model override chip is visible',
    dom: [{ selector: '.ochip', property: 'textContent-includes', equals: 'fixture/model' }],
    request: {
      method: 'POST',
      path: '/api/settings/governor',
      body: { set_override: { cap: 8, model: 'fixture/model' } },
    },
  },
  {
    id: 'mutation-governor-override-delete',
    category: 'delete',
    state: 'mutation-success',
    action: 'activate Remove override on fixture/model',
    visible: 'the removed model override chip is absent',
    dom: [{ selector: '.ochip', property: 'textContent-excludes', equals: 'fixture/model' }],
    request: {
      method: 'POST',
      path: '/api/settings/governor',
      body: { remove_override: 'fixture/model' },
    },
  },
  {
    id: 'mutation-user-create',
    category: 'create',
    state: 'mutation-success',
    action: 'enter username/password/role and activate Add user',
    visible: 'the created user row is visible',
    dom: [{ selector: '#setbody .krow', property: 'textContent-includes', equals: 'fixture-user' }],
    request: {
      method: 'POST',
      path: '/api/settings/users',
      body: {
        add: {
          password: 'fixture-password',
          role: 'user',
          username: 'fixture-user',
        },
      },
    },
  },
  {
    id: 'mutation-user-role',
    category: 'edit',
    state: 'mutation-success',
    action: 'change fixture-user role to admin',
    visible: 'the user row shows admin',
    dom: [{ selector: '#setbody .krow', property: 'fixture-user-role', equals: 'admin' }],
    request: {
      method: 'POST',
      path: '/api/settings/users',
      body: { set_role: { role: 'admin', username: 'fixture-user' } },
    },
  },
  {
    id: 'mutation-user-password-reset',
    category: 'account',
    state: 'modal-prompt',
    action: 'activate Reset password and submit replacement-password',
    visible: 'the password-reset success note names the user',
    dom: [{ selector: '#u-err', property: 'textContent', equals: 'Password reset for fixture-user.' }],
    request: {
      method: 'POST',
      path: '/api/settings/users',
      body: {
        reset_password: {
          new_password: 'replacement-password',
          username: 'fixture-user',
        },
      },
    },
  },
  {
    id: 'dialog-user-password-prompt-cancel',
    category: 'keyboard-focus',
    state: 'modal-prompt',
    action: 'activate Reset password and cancel the prompt',
    visible: 'cancel sends no request and restores focus to Reset password',
    dom: [{ selector: 'document', property: 'activeElement', equals: '[data-urp="1"]' }],
    requests: [],
  },
  {
    id: 'mutation-user-delete',
    category: 'delete',
    state: 'mutation-success',
    action: 'activate Delete for fixture-user and accept confirm',
    visible: 'the deleted user row is absent',
    dom: [{ selector: '#setbody .krow', property: 'textContent-excludes', equals: 'fixture-user' }],
    request: {
      method: 'POST',
      path: '/api/settings/users',
      body: { remove: 'fixture-user' },
    },
  },
  {
    id: 'account-password-validation',
    category: 'account',
    state: 'validation-error',
    action: 'enter mismatched new-password values and activate Update password',
    visible: 'Passwords do not match is visible and no request is sent',
    dom: [{ selector: '#a-err', property: 'textContent', equals: 'Passwords do not match.' }],
    requests: [],
  },
  {
    id: 'account-password-update',
    category: 'account',
    state: 'mutation-success',
    action: 'enter the current and matching replacement passwords and activate Update password',
    visible: 'the password-updated success note is visible and inputs are clear',
    dom: [
      { selector: '#a-err', property: 'textContent', equals: 'Password updated. Other sessions were signed out.' },
      { selector: '#a-cur,#a-new,#a-conf', property: 'value-sequence', equals: ['', '', ''] },
    ],
    request: {
      method: 'POST',
      path: '/api/settings/account',
      body: {
        current_password: 'current-password',
        new_password: 'replacement-password',
      },
    },
  },
  {
    id: 'locale-actions-remain-dormant',
    category: 'locale',
    state: 'roles-superuser-admin-user',
    action: 'inspect every role-specific Settings panel and captured request',
    visible: 'no locale mutation control is present for any operator role',
    dom: [{ selector: '[data-locale-action],#locale-select,#server-locale', property: 'exists', equals: false }],
    forbiddenRequests: [
      { method: 'POST', path: '/api/settings/locale' },
      { method: 'POST', path: '/api/settings/account', bodyContains: { action: 'locale' } },
    ],
  },
  {
    id: 'error-dashboard-range',
    category: 'error',
    state: 'api-error',
    action: 'activate a range control and fulfill the range request with ApiError',
    visible: 'Failed to load time range is visible',
    dom: [{ selector: '#rangeinfo', property: 'textContent', equals: 'Failed to load time range' }],
    request: {
      method: 'GET',
      path: '/api/dashboard',
      query: { from: '1700000000', points: '288', to: '1700003600' },
      body: null,
    },
    visual: [{ case: 'dashboard-range-api-error', locales: ['en-US'], roles: ['superuser'], surfaces: ['dashboard-tabs'] }],
  },
  {
    id: 'error-dashboard-now',
    category: 'error',
    state: 'api-error',
    action: 'fulfill a dashboard-now poll with ApiError',
    visible: 'the disconnected state is visible without an uncaught error',
    dom: [{ selector: '#liveText', property: 'textContent', equals: 'Disconnected' }],
    request: {
      method: 'GET',
      path: '/api/dashboard/now',
      body: null,
    },
  },
  {
    id: 'error-settings-load',
    category: 'error',
    state: 'api-error',
    action: 'enter Settings and fulfill config with ApiError',
    visible: 'the settings load error is visible',
    dom: [{ selector: '#setbody .empty', property: 'textContent-includes', equals: 'Could not load Settings. Reload the page and sign in again.' }],
    request: {
      method: 'GET',
      path: '/api/config',
      body: null,
    },
    visual: [{ case: 'settings-load-api-error', locales: ['en-US'], roles: ['superuser'], surfaces: ['settings-load-error'] }],
  },
  {
    id: 'error-settings-mutation',
    category: 'error',
    state: 'mutation-error',
    action: 'submit locally valid history settings and fulfill with ApiError',
    visible: 'the typed API error message is visible beside the triggering control',
    dom: [{ selector: '#history-err', property: 'textContent', equals: 'invalid history fixture' }],
    request: {
      method: 'POST',
      path: '/api/settings/history',
      body: {
        days: 30,
        default_window_days: 7,
        slo_target_percent: 99.9,
      },
    },
  },
  {
    id: 'error-settings-fallback',
    category: 'error',
    state: 'malformed-api-error',
    action: 'submit locally valid history settings and fulfill with an API failure without a usable error message',
    visible: 'the catalog-owned status fallback is visible beside the triggering control',
    dom: [{ selector: '#history-err', property: 'textContent', equals: 'Request failed (HTTP 400).' }],
    request: {
      method: 'POST',
      path: '/api/settings/history',
      body: {
        days: 30,
        default_window_days: 7,
        slo_target_percent: 99.9,
      },
    },
  },
  {
    id: 'error-nim-key-validation',
    category: 'error',
    state: 'mutation-error',
    action: 'activate Validate and add and fulfill key validation with an error',
    visible: 'the raw API validation error is visible without a stale success state, and Add anyway is available',
    dom: [
      { selector: '#nk-err', property: 'textContent', equals: 'validation fixture rejected' },
      { selector: '#nk-force', property: 'hidden', equals: false },
    ],
    request: {
      method: 'POST',
      path: '/api/settings/validate-key',
      body: { key: 'nvapi-invalid-fixture' },
    },
  },
  {
    id: 'error-setup-key-validation',
    category: 'error',
    state: 'mutation-error',
    action: 'enter the default upstream base URL, activate setup Validate and add, and fulfill with an error',
    visible: 'the setup validation error is visible',
    dom: [{ selector: '#err', property: 'textContent-includes', equals: 'validation' }],
    request: {
      method: 'POST',
      path: '/setup/validate-key',
      body: {
        base_url: 'https://integrate.api.nvidia.com',
        key: 'nvapi-invalid-fixture',
      },
    },
    visual: [{ case: 'setup-key-validation-api-error', locales: ['en-US'], roles: ['public'], surfaces: ['setup-key-validation-api-error'] }],
  },
  {
    id: 'setup-key-validation-success',
    category: 'create',
    state: 'mutation-success',
    action: 'enter the default upstream base URL, activate setup Validate and add, and fulfill with ValidateKeyResponse',
    visible: 'the validated NIM key appears in the setup key list',
    dom: [{ selector: '#keylist > li', property: 'textContent-includes', equals: '40 rpm' }],
    request: {
      method: 'POST',
      path: '/setup/validate-key',
      query: {},
      body: {
        base_url: 'https://integrate.api.nvidia.com',
        key: 'nvapi-ui-fixture',
      },
    },
    visual: [
      { case: 'setup-step2', locales: ['en-US', 'en-XA'], roles: ['public'], surfaces: ['setup-step2'] },
      { case: 'setup-step3', locales: ['en-US', 'en-XA'], roles: ['public'], surfaces: ['setup-step3'] },
    ],
  },
  {
    id: 'error-setup-submit',
    category: 'error',
    state: 'mutation-error',
    action: 'enter the default upstream base URL, activate setup Finish, and fulfill with ApiError',
    visible: 'the setup submit error is visible and Finish is enabled',
    dom: [
      { selector: '#err', property: 'textContent', equals: 'setup fixture rejected' },
      { selector: '#finish', property: 'disabled', equals: false },
    ],
    request: {
      method: 'POST',
      path: '/setup',
      body: {
        create_client_key: { name: 'default' },
        base_url: 'https://integrate.api.nvidia.com',
        nim_keys: [{ key: 'nvapi-ui-fixture', rpm: 40 }],
        password: 'fixture-password',
        username: 'fixture-user',
      },
    },
    visual: [{ case: 'setup-submit-api-error', locales: ['en-US'], roles: ['public'], surfaces: ['setup-submit-api-error'] }],
  },
  {
    id: 'setup-submit-success',
    category: 'create',
    state: 'mutation-success',
    action: 'enter the default upstream base URL, activate setup Finish, and fulfill with SetupResponse containing MintedClientKey',
    visible: 'the connection screen shows the once-only client secret',
    dom: [
      { selector: '#step4', property: 'hidden', equals: false },
      { selector: '#csecret', property: 'textContent', equals: 'npk_fixture_secret' },
    ],
    request: {
      method: 'POST',
      path: '/setup',
      query: {},
      body: {
        base_url: 'https://integrate.api.nvidia.com',
        create_client_key: { name: 'default' },
        nim_keys: [{ key: 'nvapi-ui-fixture', rpm: 40 }],
        password: 'fixture-password',
        username: 'fixture-user',
      },
    },
    visual: [{ case: 'setup-step4', locales: ['en-US', 'en-XA'], roles: ['public'], surfaces: ['setup-step4'] }],
  },
  {
    id: 'state-empty',
    category: 'state',
    state: 'empty',
    action: 'fulfill dashboard Now and range requests from empty Rust-owned fixtures',
    visible: 'empty-state labels render without invented metric zeros',
    dom: [
      { selector: '#tab-overview .empty', property: 'exists', equals: true },
      { selector: '#rangeinfo', property: 'textContent', equals: 'No data yet' },
    ],
  },
  {
    id: 'state-loading',
    category: 'state',
    state: 'loading',
    action: 'hold one required fixture response before fulfillment',
    visible: 'the page stays stable while a fixture response is held',
    dom: [{ selector: 'body', property: 'hidden', equals: true }],
  },
  {
    id: 'state-partial-incomplete',
    category: 'state',
    state: 'partial-incomplete',
    action: 'fulfill dashboard requests from the partial/incomplete Rust-owned fixture',
    visible: 'partial data is distinguishable from complete data',
    dom: [
      { selector: '#rangeinfo', property: 'textContent-includes', equals: 'Partial history ·' },
      { selector: '#rangeinfo', property: 'effective-bound-summary', equals: true },
    ],
    visual: [{ case: 'dashboard-partial-incomplete', locales: ['en-US', 'en-XA'], roles: ['superuser'], surfaces: ['dashboard-tabs'] }],
  },
  {
    id: 'state-observation-measured',
    category: 'state',
    state: 'observation-measured',
    action: 'fulfill the dashboard range from the measured-observation Rust fixture',
    visible: 'the health row says Measured without a numeric quality zero',
  },
  {
    id: 'state-observation-estimated',
    category: 'state',
    state: 'observation-estimated',
    action: 'fulfill the dashboard range from the estimated-observation Rust fixture',
    visible: 'the health row says Estimated without a numeric quality zero',
  },
  {
    id: 'state-observation-unavailable',
    category: 'state',
    state: 'observation-unavailable',
    action: 'fulfill the dashboard range from the unavailable-observation Rust fixture',
    visible: 'the health row says Unavailable rather than zero',
  },
  {
    id: 'state-observation-invalid',
    category: 'state',
    state: 'observation-invalid',
    action: 'fulfill the dashboard range from the invalid-observation Rust fixture',
    visible: 'the health row says Invalid rather than zero',
  },
  {
    id: 'state-observation-incomplete-history',
    category: 'state',
    state: 'observation-measured-history-incomplete',
    action: 'fulfill the dashboard range from measured observations with incomplete history',
    visible: 'Measured observation quality and Partial history remain independently visible',
  },
  {
    id: 'state-observation-zero',
    category: 'state',
    state: 'observation-zero',
    action: 'fulfill the dashboard range from the zero-observation Rust fixture',
    visible: 'recognized zero observations say No observations rather than an unavailable quality',
  },
  {
    id: 'state-observation-absent',
    category: 'state',
    state: 'observation-absent',
    action: 'fulfill the dashboard range with no recognized observation rows',
    visible: 'absent observation rows say Unavailable rather than zero',
  },
  {
    id: 'state-observation-tie-invalid',
    category: 'state',
    state: 'observation-tie-invalid',
    action: 'fulfill tied observation results including invalid from the Rust fixture',
    visible: 'Invalid wins a positive result tie',
  },
  {
    id: 'state-observation-tie-unavailable',
    category: 'state',
    state: 'observation-tie-unavailable',
    action: 'fulfill tied observation results including unavailable from the Rust fixture',
    visible: 'Unavailable wins a positive result tie without invalid',
  },
  {
    id: 'state-observation-tie-estimated',
    category: 'state',
    state: 'observation-tie-estimated',
    action: 'fulfill tied estimated and measured observations from the Rust fixture',
    visible: 'Estimated wins a positive result tie without invalid or unavailable',
  },
  {
    id: 'state-observation-live-tail',
    category: 'state',
    state: 'observation-live-tail',
    action: 'append a valid observation row and an ignored negative row through the live dashboard tail',
    visible: 'the valid live-tail result renders while the negative row does not become Invalid',
  },
  {
    id: 'state-history-unavailable',
    category: 'state',
    state: 'history-unavailable',
    action: 'select the unavailable fixture’s recovered-gap range',
    visible: 'the selected range is unavailable without inventing history values',
    dom: [{ selector: '#rangeinfo', property: 'textContent', equals: 'History unavailable for selected time range' }],
    visual: [{ case: 'dashboard-history-unavailable', locales: ['en-US', 'en-XA'], roles: ['superuser'], surfaces: ['dashboard-tabs'] }],
  },
  {
    id: 'state-history-outside-before',
    category: 'state',
    state: 'history-outside-before',
    action: 'select a range wholly before globally available history',
    visible: 'an ordinary range before retained history remains a no-data range',
    dom: [{ selector: '#rangeinfo', property: 'textContent', equals: 'No data in selected time range' }],
  },
  {
    id: 'state-history-outside-after',
    category: 'state',
    state: 'history-outside-after',
    action: 'select a range wholly after globally available history',
    visible: 'an ordinary range after retained history remains a no-data range',
    dom: [{ selector: '#rangeinfo', property: 'textContent', equals: 'No data in selected time range' }],
  },
  {
    id: 'state-extreme-numeric',
    category: 'state',
    state: 'extreme-numeric',
    action: 'fulfill dashboard requests from the extreme-numeric Rust-owned fixture',
    visible: 'extreme numeric values render without a page error',
    dom: [{ selector: '#tab-overview', property: 'finite-layout', equals: true }],
    visual: [{ case: 'dashboard-extreme-numeric', locales: ['en-US'], roles: ['superuser'], surfaces: ['dashboard-tabs'] }],
  },
  {
    id: 'state-settings-exact-large-values',
    category: 'state',
    state: 'exact-large-settings-values',
    action: 'enter Access with a test-only high-value transform of the Rust-owned ConfigResponse',
    visible: 'Settings shows locale-grouped, non-compact key and pool values above 10,000',
    dom: [
      { selector: '#pool-note', property: 'textContent', equals: 'Pool: 12,345 enabled · Total: 67,890 rpm' },
      { selector: '[data-ksfp="f17e0001"]', property: 'textContent', equals: '12,345 / 67,890 in window' },
    ],
    visual: [{ case: 'settings-exact-large-values', locales: ['en-US'], roles: ['superuser'], surfaces: ['settings-access'] }],
  },
  {
    id: 'state-long-machine-values',
    category: 'state',
    state: 'long-machine-values',
    action: 'fulfill dashboard/config requests from long-machine-value Rust-owned fixtures',
    visible: 'long model, client, publisher, and user values remain byte-identical',
    dom: [{ selector: '#tab-models,#tab-clients,#setbody', property: 'machine-value-byte-match', equals: true }],
    visual: [{
      case: 'long-machine-values', locales: ['en-US'], roles: ['superuser'],
      surfaces: ['dashboard-tabs', 'settings-access', 'settings-users'],
    }],
  },
];

const VISUAL_VIEWPORTS = ['390x844', '768x1024', '900x1000', '1440x1000'];
const DASHBOARD_VISUAL_SURFACES = [
  { id: 'dashboard-overview', root: '#tab-overview', reach: 'dashboard:overview', layoutRoot: '.main', layoutBoundary: '.main' },
  { id: 'dashboard-models', root: '#tab-models', reach: 'dashboard:models', layoutRoot: '.main', layoutBoundary: '.main' },
  { id: 'dashboard-clients', root: '#tab-clients', reach: 'dashboard:clients', layoutRoot: '.main', layoutBoundary: '.main' },
  { id: 'dashboard-reliability', root: '#tab-reliability', reach: 'dashboard:reliability', layoutRoot: '.main', layoutBoundary: '.main' },
  { id: 'dashboard-capacity', root: '#tab-capacity', reach: 'dashboard:capacity', layoutRoot: '.main', layoutBoundary: '.main' },
];
const SETTINGS_VISUAL_SURFACES = [
  { id: 'settings-access', root: '#nk-key', reach: 'settings:access', layoutRoot: '.main', layoutBoundary: '.main' },
  { id: 'settings-server', root: '#sv-base', reach: 'settings:server', layoutRoot: '.main', layoutBoundary: '.main' },
  { id: 'settings-users', root: '#u-add', reach: 'settings:users', layoutRoot: '.main', layoutBoundary: '.main' },
  { id: 'settings-account', root: '#a-cur', reach: 'settings:account', layoutRoot: '.main', layoutBoundary: '.main' },
];
const VISUAL_SURFACE_FAMILIES = {
  'dashboard-tabs': DASHBOARD_VISUAL_SURFACES,
  'settings-panels': SETTINGS_VISUAL_SURFACES,
};
const VISUAL_SURFACES = new Map([
  ...DASHBOARD_VISUAL_SURFACES,
  ...SETTINGS_VISUAL_SURFACES,
  { id: 'settings-load-error', root: '#setbody .empty', reach: 'settings:load-error', layoutRoot: '.main', layoutBoundary: '.main' },
  { id: 'setup-step1', root: '#step1', reach: 'setup:step1', layoutRoot: '#step1', layoutBoundary: 'main' },
  { id: 'setup-step2', root: '#step2', reach: 'setup:step2', layoutRoot: '#step2', layoutBoundary: 'main' },
  { id: 'setup-step3', root: '#step3', reach: 'setup:review-after-successful-key', layoutRoot: '#step3', layoutBoundary: 'main' },
  { id: 'setup-step4', root: '#step4', reach: 'setup:completion', layoutRoot: '#step4', layoutBoundary: 'main' },
  { id: 'setup-client-validation-error', root: '#err', reach: 'setup:client-validation-error', layoutRoot: '#step1', layoutBoundary: 'main' },
  { id: 'setup-key-validation-api-error', root: '#err', reach: 'setup:key-validation-api-error', layoutRoot: '#step2', layoutBoundary: 'main' },
  { id: 'setup-submit-api-error', root: '#err', reach: 'setup:submit-api-error', layoutRoot: '#step3', layoutBoundary: 'main' },
  { id: 'login-healthy', root: 'form.login-card', reach: 'login:healthy', layoutRoot: 'main', layoutBoundary: 'main' },
  { id: 'login-invalid-credentials', root: '#login-error', reach: 'login:invalid-credentials', layoutRoot: 'main', layoutBoundary: 'main' },
].map(surface => [surface.id, surface]));

function visualIdentity(item) {
  return [item.rowId, item.caseId, item.surfaceId, item.locale, item.role, item.viewport].join(':');
}

function visualArtifactPath(item) {
  return [item.rowId, item.caseId, item.surfaceId, item.locale, item.role, item.viewport].join('__') + '.png';
}

function resolveVisualExpectations(rows = INTERACTION_ROWS) {
  const items = [], problems = [];
  for (const row of rows) {
    for (const declaration of row.visual || []) {
      if (!declaration.case || !Array.isArray(declaration.locales) || !Array.isArray(declaration.roles)
          || !Array.isArray(declaration.surfaces)) {
        problems.push(`visual-coverage:${row.id}:invalid-declaration`);
        continue;
      }
      const surfaces = declaration.surfaces.flatMap(name => {
        if (VISUAL_SURFACE_FAMILIES[name]) return VISUAL_SURFACE_FAMILIES[name];
        const surface = VISUAL_SURFACES.get(name);
        if (surface) return [surface];
        problems.push(`visual-coverage:${row.id}:${declaration.case}:unknown-surface:${name}`);
        return [];
      });
      for (const surface of surfaces) {
        for (const locale of declaration.locales) {
          for (const role of declaration.roles) {
            for (const viewport of VISUAL_VIEWPORTS) {
              const item = {
                rowId: row.id,
                caseId: declaration.case,
                surfaceId: surface.id,
                locale,
                role,
                viewport,
                root: surface.root,
                reach: declaration.reach || surface.reach,
                layoutRoot: surface.layoutRoot,
                layoutBoundary: surface.layoutBoundary,
              };
              item.identity = visualIdentity(item);
              item.path = visualArtifactPath(item);
              items.push(item);
            }
          }
        }
      }
    }
  }
  return { items, problems };
}

function completeVisualCapture(item) {
  const [width, height] = item.viewport.split('x').map(Number);
  const contentHeight = Math.max(height, 1200);
  return {
    kind: 'document-vertical-v1',
    requestedViewport: { width, height },
    cssLayoutViewport: { pageX: 0, pageY: 0, clientWidth: width, clientHeight: height },
    cssContentSize: { x: 0, y: 0, width, height: contentHeight },
    clip: { x: 0, y: 0, width, height: contentHeight, scale: 1 },
    captureBeyondViewport: true,
    png: { width, height: contentHeight },
    internalVerticalScrollers: [],
  };
}

function visualCaptureProblems(item, capture) {
  const prefix = `visual-coverage:${item.identity}:`;
  const [width, height] = item.viewport.split('x').map(Number);
  if (!capture || capture.kind !== 'document-vertical-v1'
      || !capture.requestedViewport || !capture.cssLayoutViewport || !capture.cssContentSize
      || !capture.clip || !capture.png || !Array.isArray(capture.internalVerticalScrollers)) {
    return [`${prefix}capture-metadata-missing`];
  }
  const viewport = capture.requestedViewport;
  const layoutViewport = capture.cssLayoutViewport;
  if (viewport.width !== width || viewport.height !== height
      || layoutViewport.pageX !== 0 || layoutViewport.pageY !== 0
      || layoutViewport.clientWidth !== width || layoutViewport.clientHeight !== height) {
    return [`${prefix}capture-viewport-mismatch`];
  }
  const content = capture.cssContentSize;
  const expectedHeight = Math.max(height, Math.ceil(content.height));
  if (capture.captureBeyondViewport !== true || capture.clip.height < expectedHeight) {
    return [`${prefix}capture-not-full-document`];
  }
  const clip = capture.clip;
  if (content.x !== 0 || content.y !== 0 || !Number.isFinite(content.width) || !Number.isFinite(content.height)
      || clip.x !== 0 || clip.y !== 0 || clip.width !== width || clip.height !== expectedHeight || clip.scale !== 1) {
    return [`${prefix}capture-clip-mismatch`];
  }
  if (capture.png.width !== clip.width || capture.png.height !== clip.height) {
    return [`${prefix}png-dimensions-mismatch`];
  }
  return capture.internalVerticalScrollers.flatMap(scroller => {
    if (scroller?.intentionalTable === true && scroller.directChild === 'table'
        && typeof scroller.selector === 'string' && scroller.selector.includes('.scroll')) return [];
    return [`${prefix}internal-vertical-scroller:${canonicalJson(scroller)}`];
  });
}

function completeVisualObservations(rows = INTERACTION_ROWS) {
  return resolveVisualExpectations(rows).items.map(item => ({
    ...item,
    artifactWritten: true,
    reached: true,
    rootVisible: true,
    renderedLocale: item.locale,
    capture: completeVisualCapture(item),
    layoutProblems: [],
    pageErrors: [],
    consoleErrors: [],
    promiseRejections: [],
  }));
}

function visualCoverageProblems(observations, rows = INTERACTION_ROWS) {
  const { items: expected, problems } = resolveVisualExpectations(rows);
  const duplicateProblems = (items, field, label) => {
    const seen = new Set();
    for (const item of items) {
      if (seen.has(item[field])) problems.push(`visual-coverage:${item.identity}:${label}:${item[field]}`);
      seen.add(item[field]);
    }
  };
  duplicateProblems(expected, 'identity', 'duplicate-identity');
  duplicateProblems(expected, 'path', 'duplicate-path');
  duplicateProblems(observations, 'identity', 'duplicate-identity');
  duplicateProblems(observations, 'path', 'duplicate-path');

  const observationDetails = (observed, item, field, label) => {
    const details = Array.isArray(observed[field]) ? observed[field] : [{ missing: field }];
    for (const detail of details) {
      problems.push(`visual-coverage:${item.identity}:${label}:${canonicalJson(detail)}`);
    }
  };
  const actual = new Map(observations.map(item => [item.identity, item]));
  const expectedIds = new Set(expected.map(item => item.identity));
  for (const item of observations) {
    if (!expectedIds.has(item.identity)) problems.push(`visual-coverage:${item.identity}:unexpected`);
  }
  for (const item of expected) {
    const observed = actual.get(item.identity);
    if (!observed) {
      problems.push(`visual-coverage:${item.identity}:missing`);
      continue;
    }
    if (observed.path !== item.path) problems.push(`visual-coverage:${item.identity}:path-mismatch`);
    if (observed.artifactWritten !== true) problems.push(`visual-coverage:${item.identity}:artifact-missing`);
    if (observed.root !== item.root || observed.reach !== item.reach) {
      problems.push(`visual-coverage:${item.identity}:root-mismatch`);
    } else if (!observed.reached || !observed.rootVisible) {
      problems.push(`visual-coverage:${item.identity}:root-unreached`);
    }
    if (!observed.renderedLocale) {
      problems.push(`visual-coverage:${item.identity}:locale-provenance-missing`);
    } else if (observed.renderedLocale !== item.locale) {
      problems.push(`visual-coverage:${item.identity}:locale-provenance-mismatch:${item.locale}:${observed.renderedLocale}`);
    }
    problems.push(...visualCaptureProblems(item, observed.capture));
    observationDetails(observed, item, 'layoutProblems', 'layout');
    observationDetails(observed, item, 'pageErrors', 'page-error');
    observationDetails(observed, item, 'consoleErrors', 'console-error');
    observationDetails(observed, item, 'promiseRejections', 'promise-rejection');
  }
  return problems;
}

function visualCoverageSelftest() {
  const failures = [];
  const complete = completeVisualObservations();
  if (visualCoverageProblems(complete).length) failures.push('visual complete observation set did not pass');
  if (complete.filter(item => item.surfaceId.startsWith('dashboard-') || item.surfaceId.startsWith('settings-'))
    .some(item => item.layoutRoot !== '.main')) {
    failures.push('operator visual layout root is not .main');
  }
  const target = complete.find(item => item.identity
    === 'state-long-machine-values:long-machine-values:settings-users:en-US:superuser:390x844');
  if (!target) {
    failures.push('long-machine Settings Users visual case is missing');
    return failures;
  }
  const expectOnly = (name, observations, expected) => {
    const problems = visualCoverageProblems(observations);
    if (problems.length !== 1 || problems[0] !== expected) failures.push(`${name} did not fire ${expected}`);
  };
  expectOnly('long-machine Settings Users missing mutation',
    complete.filter(item => item.identity !== target.identity),
    `visual-coverage:${target.identity}:missing`);
  expectOnly('hidden visual root mutation',
    complete.map(item => item.identity === target.identity ? { ...item, rootVisible: false } : item),
    `visual-coverage:${target.identity}:root-unreached`);
  const expectProblem = (name, observations, expected) => {
    if (!visualCoverageProblems(observations).includes(expected)) {
      failures.push(`${name} did not fire ${expected}`);
    }
  };
  expectProblem('artifact missing mutation',
    complete.map(item => item.identity === target.identity ? { ...item, artifactWritten: false } : item),
    `visual-coverage:${target.identity}:artifact-missing`);
  expectOnly('viewport-only capture mutation',
    complete.map(item => item.identity === target.identity ? {
      ...item,
      capture: {
        ...item.capture,
        captureBeyondViewport: false,
        clip: { ...item.capture.clip, height: 844 },
        png: { ...item.capture.png, height: 844 },
      },
    } : item),
    `visual-coverage:${target.identity}:capture-not-full-document`);
  expectOnly('PNG dimension mutation',
    complete.map(item => item.identity === target.identity ? {
      ...item,
      capture: { ...item.capture, png: { ...item.capture.png, height: 1199 } },
    } : item),
    `visual-coverage:${target.identity}:png-dimensions-mismatch`);
  const localeTarget = complete.find(item => item.identity
    === 'startup-dashboard-healthy:dashboard-healthy:dashboard-overview:en-XA:superuser:390x844');
  if (!localeTarget) {
    failures.push('en-XA dashboard visual case is missing');
  } else {
    expectOnly('en-XA locale provenance mutation',
      complete.map(item => item.identity === localeTarget.identity
        ? { ...item, renderedLocale: 'en-US' }
        : item),
      `visual-coverage:${localeTarget.identity}:locale-provenance-mismatch:en-XA:en-US`);
  }
  const unexpectedScroller = {
    selector: '#unexpected-scroll-pane',
    geometry: { left: 0, top: 0, width: 120, height: 120, scrollHeight: 240, clientHeight: 120 },
    intentionalTable: false,
    directChild: 'div',
  };
  expectOnly('unexpected vertical scroller mutation',
    complete.map(item => item.identity === target.identity ? {
      ...item,
      capture: { ...item.capture, internalVerticalScrollers: [unexpectedScroller] },
    } : item),
    `visual-coverage:${target.identity}:internal-vertical-scroller:${canonicalJson(unexpectedScroller)}`);
  expectProblem('layout problem mutation',
    complete.map(item => item.identity === target.identity ? { ...item, layoutProblems: ['layout fixture'] } : item),
    `visual-coverage:${target.identity}:layout:${canonicalJson('layout fixture')}`);
  expectProblem('page error mutation',
    complete.map(item => item.identity === target.identity ? { ...item, pageErrors: ['page fixture'] } : item),
    `visual-coverage:${target.identity}:page-error:${canonicalJson('page fixture')}`);
  expectProblem('console error mutation',
    complete.map(item => item.identity === target.identity ? { ...item, consoleErrors: ['console fixture'] } : item),
    `visual-coverage:${target.identity}:console-error:${canonicalJson('console fixture')}`);
  expectProblem('promise rejection mutation',
    complete.map(item => item.identity === target.identity ? { ...item, promiseRejections: ['promise fixture'] } : item),
    `visual-coverage:${target.identity}:promise-rejection:${canonicalJson('promise fixture')}`);
  const collisionTarget = complete.find(item => item.identity !== target.identity);
  const collision = complete.map(item => item.identity === collisionTarget.identity
    ? { ...item, path: target.path }
    : item);
  const collisionProblems = visualCoverageProblems(collision);
  const expectedCollision = `visual-coverage:${target.identity}:duplicate-path:${target.path}`;
  if (!collisionProblems.includes(expectedCollision)) {
    failures.push(`duplicate visual artifact path mutation did not fire ${expectedCollision}`);
  }
  return failures;
}

const domContract = (check, expression, expected, collect) => ({
  check,
  expression,
  expected,
  ...(collect ? { collect } : {}),
});
const DOM_CONTRACTS = {
  'startup-dashboard-healthy': [
    domContract('overview-visible', `document.querySelector('#tab-overview')?.hidden ?? null`, false),
  ],
  'startup-setup-healthy': [
    domContract('step-one-visible', `document.querySelector('#step1')?.hidden ?? null`, false),
  ],
  'startup-login-healthy': [
    domContract('login-form-present', `!!document.querySelector('form.login-card')`, true),
  ],
  'login-invalid-credentials': [
    domContract('error-visible', `document.querySelector('#login-error')?.hidden ?? null`, false),
    domContract('error-text', `document.querySelector('#login-error')?.textContent.trim() ?? null`, 'Incorrect username or password.'),
  ],
  'navigation-dashboard-tabs': [
    domContract('visible-section-after-each-activation', `document.querySelector('main > section:not([hidden])')?.id ?? null`,
      ['tab-overview', 'tab-models', 'tab-clients', 'tab-reliability', 'tab-capacity', 'tab-settings'],
      {
        kind: 'sequence',
        steps: [
          '#side [data-tab="overview"]',
          '#side [data-tab="models"]',
          '#side [data-tab="clients"]',
          '#side [data-tab="reliability"]',
          '#side [data-tab="capacity"]',
          '#side [data-tab="settings"]',
        ],
      }),
  ],
  'navigation-settings-role-panels': [
    domContract('authorized-subnavigation-activates-visible-panel-by-role',
      `(() => {
        const selected = document.querySelector('#setnav [aria-current="page"]')?.dataset.sub ?? null;
        const marker = {
          access: '#nk-key',
          account: '#a-cur',
          server: '#sv-base',
          users: '#u-add',
        }[selected];
        return { selected, visible: marker ? !!document.querySelector(marker) : false };
      })()`,
      {
        admin: [
          { selected: 'access', visible: true },
          { selected: 'server', visible: true },
          { selected: 'users', visible: true },
          { selected: 'account', visible: true },
        ],
        superuser: [
          { selected: 'access', visible: true },
          { selected: 'server', visible: true },
          { selected: 'users', visible: true },
          { selected: 'account', visible: true },
        ],
        user: [
          { selected: 'access', visible: true },
          { selected: 'account', visible: true },
        ],
      },
      {
        kind: 'context-sequence',
        contexts: {
          admin: [
            '#setnav [data-sub="access"]',
            '#setnav [data-sub="server"]',
            '#setnav [data-sub="users"]',
            '#setnav [data-sub="account"]',
          ],
          superuser: [
            '#setnav [data-sub="access"]',
            '#setnav [data-sub="server"]',
            '#setnav [data-sub="users"]',
            '#setnav [data-sub="account"]',
          ],
          user: [
            '#setnav [data-sub="access"]',
            '#setnav [data-sub="account"]',
          ],
        },
      }),
  ],
  'sorting-table-ascending': [
    domContract('ascending-model-order',
      `Array.from(document.querySelectorAll('#table-models tbody tr')).map(row => row.cells[0]?.textContent.trim())`,
      ['fixture/alpha', 'fixture/zeta']),
  ],
  'sorting-table-descending': [
    domContract('descending-model-order',
      `Array.from(document.querySelectorAll('#table-models tbody tr')).map(row => row.cells[0]?.textContent.trim())`,
      ['fixture/zeta', 'fixture/alpha']),
  ],
  'sorting-table-persists-rerender': [
    domContract('descending-model-order-after-rerender',
      `Array.from(document.querySelectorAll('#table-models tbody tr')).map(row => row.cells[0]?.textContent.trim())`,
      ['fixture/zeta', 'fixture/alpha']),
  ],
  'filtering-settings-role': [
    domContract('server-nav-absent', `!document.querySelector('#setnav [data-sub="server"]')`, true),
    domContract('users-nav-absent', `!document.querySelector('#setnav [data-sub="users"]')`, true),
    domContract('server-controls-absent', `!document.querySelector('#sv-base')`, true),
    domContract('user-controls-absent', `!document.querySelector('#u-add')`, true),
  ],
  'refresh-dashboard-now': [
    domContract('changed-fixture-uptime', `document.querySelector('#uptime')?.textContent.trim() ?? null`, 'up 2h 0m'),
  ],
  'refresh-settings-access': [
    domContract('lane-state-updated', `document.querySelector('[data-ksfp="f17e0001"]')?.textContent.trim() ?? null`, '1 / 40 in window'),
    domContract('focus-preserved', `document.activeElement?.matches('#nk-key') ?? false`, true),
  ],
  'history-window-preset': [
    domContract('one-hour-selected', `document.querySelector('#ranges [data-range="3600"]')?.getAttribute('aria-pressed') ?? null`, 'true'),
    domContract('one-hour-summary', `document.querySelector('#rangeinfo')?.textContent.includes('1h') ?? false`, true),
  ],
  'history-window-custom': [
    domContract('custom-selected', `document.querySelector('#ranges [data-range="custom"]')?.getAttribute('aria-pressed') ?? null`, 'true'),
    domContract('absolute-summary', `document.querySelector('#rangeinfo')?.textContent.includes('Absolute') ?? false`, true),
  ],
  'history-window-freeze': [
    domContract('frozen-label', `document.querySelector('#liveText')?.textContent.trim() ?? null`, 'Absolute'),
  ],
  'history-window-resume': [
    domContract('following-label', `document.querySelector('#liveText')?.textContent.trim() ?? null`, 'Live'),
  ],
  'dialog-client-secret-open': [
    domContract('dialog-open', `document.querySelector('#modal')?.open ?? false`, true),
    domContract('done-control-present', `!!document.querySelector('#modal-done')`, true),
    domContract('dialog-focus-contained', `document.activeElement?.matches('#modal-copy') ?? false`, true),
    domContract('secret-byte-exact', `document.querySelector('#modal-body .secretbox')?.textContent ?? null`, 'npk_fixture_secret'),
  ],
  'dialog-client-secret-close': [
    domContract('dialog-closed', `document.querySelector('#modal')?.open ?? null`, false),
    domContract('dialog-focus-returned', `document.activeElement?.matches('#ck-add') ?? false`, true),
  ],
  'dialog-client-secret-escape-focus-return': [
    domContract('dialog-closed', `document.querySelector('#modal')?.open ?? null`, false),
    domContract('current-add-control-retained', `!!document.querySelector('#ck-add')`, true),
    domContract('dialog-focus-returned', `document.activeElement?.matches('#ck-add') ?? false`, true),
  ],
  'dialog-confirm-cancel-focus-return': [
    domContract('destructive-control-retained', `!!document.querySelector('[data-kdel="0"]')`, true),
    domContract('focus-returned', `document.activeElement?.matches('[data-kdel="0"]') ?? false`, true),
  ],
  'mutation-nim-key-create': [
    domContract('added-key-present', `!!document.querySelector('[data-ksfp="f17e0001"]')`, true),
  ],
  'mutation-nim-key-rpm': [
    domContract('rpm-value', `document.querySelector('[data-rpm="0"]')?.value ?? null`, '41'),
  ],
  'mutation-nim-key-toggle': [
    domContract('disabled-switch', `document.querySelector('[data-tog="0"]')?.getAttribute('aria-pressed') ?? null`, 'false'),
  ],
  'mutation-nim-key-delete': [
    domContract('removed-key-absent', `!document.querySelector('[data-ksfp="f17e0001"]')`, true),
  ],
  'mutation-client-key-delete': [
    domContract('revoked-client-absent',
      `!Array.from(document.querySelectorAll('#setbody .krow')).some(row => row.textContent.includes('fixture-client'))`,
      true),
  ],
  'mutation-client-auth-mode': [
    domContract('keyed-mode-selected', `document.querySelector('[data-mode="keyed"]')?.getAttribute('aria-pressed') ?? null`, 'true'),
  ],
  'mutation-server-limits': [
    domContract('saved-server-values',
      `[
        document.querySelector('#sv-base')?.value ?? null,
        document.querySelector('#sv-maxwait')?.value ?? null,
        document.querySelector('#sv-heartbeat')?.value ?? null,
        document.querySelector('#sv-idle')?.value ?? null,
        document.querySelector('#sv-timeout')?.value ?? null,
        document.querySelector('#sv-ttl')?.value ?? null,
        document.querySelector('#sv-inflight')?.value ?? null,
        document.querySelector('#sv-strict')?.getAttribute('aria-pressed') ?? null,
      ]`,
      ['https://fixture.invalid/v1', '900', '10', '300', '300', '600', '512', 'false']),
    domContract('limits-saved', `document.querySelector('#limits-err')?.textContent.trim() ?? null`, 'Saved.'),
  ],
  'mutation-server-history': [
    domContract('saved-history-values',
      `[
        document.querySelector('#sv-retention-days')?.value ?? null,
        document.querySelector('#sv-default-days')?.value ?? null,
        document.querySelector('#sv-slo')?.value ?? null,
      ]`,
      ['30', '7', '99.9']),
    domContract('history-saved', `document.querySelector('#history-err')?.textContent.trim() ?? null`, 'Saved.'),
    domContract('history-persistence-degraded', `document.querySelector('[data-i18n="settings.server.history.persistence_degraded"]')?.textContent.trim() ?? null`, 'History persistence is degraded. Restart the proxy to retry canonical storage.'),
  ],
  'mutation-governor-toggle': [
    domContract('governor-disabled', `document.querySelector('#gov-tog')?.getAttribute('aria-pressed') ?? null`, 'false'),
  ],
  'mutation-governor-override-create': [
    domContract('override-present', `Array.from(document.querySelectorAll('.ochip')).some(node => node.textContent.includes('fixture/model'))`, true),
  ],
  'mutation-governor-override-delete': [
    domContract('override-absent', `!Array.from(document.querySelectorAll('.ochip')).some(node => node.textContent.includes('fixture/model'))`, true),
  ],
  'mutation-user-create': [
    domContract('created-user-present',
      `Array.from(document.querySelectorAll('#setbody .krow')).some(row => row.textContent.includes('fixture-user'))`,
      true),
  ],
  'mutation-user-role': [
    domContract('fixture-user-admin',
      `Array.from(document.querySelectorAll('#setbody .krow')).find(row => row.textContent.includes('fixture-user'))?.querySelector('.rbadge')?.textContent.trim() ?? null`,
      'admin'),
  ],
  'mutation-user-password-reset': [
    domContract('password-reset-note', `document.querySelector('#u-err')?.textContent.trim() ?? null`, 'Password reset for fixture-user.'),
  ],
  'dialog-user-password-prompt-cancel': [
    domContract('prompt-focus-returned', `document.activeElement?.matches('[data-urp="1"]') ?? false`, true),
  ],
  'mutation-user-delete': [
    domContract('deleted-user-absent',
      `!Array.from(document.querySelectorAll('#setbody .krow')).some(row => row.textContent.includes('fixture-user'))`,
      true),
  ],
  'account-password-validation': [
    domContract('mismatch-note', `document.querySelector('#a-err')?.textContent.trim() ?? null`, 'Passwords do not match.'),
  ],
  'account-password-update': [
    domContract('password-updated-note',
      `document.querySelector('#a-err')?.textContent.trim() ?? null`,
      'Password updated. Other sessions were signed out.'),
    domContract('password-fields-cleared',
      `['a-cur','a-new','a-conf'].map(id => document.getElementById(id)?.value ?? null)`,
      ['', '', '']),
  ],
  'locale-actions-remain-dormant': [
    domContract('no-locale-mutation-control',
      `!document.querySelector('[data-locale-action],#locale-select,#server-locale')`,
      {
        admin: [true, true, true, true, true],
        superuser: [true, true, true, true, true],
        user: [true, true, true],
      },
      {
        kind: 'context-sequence',
        contexts: {
          admin: [
            '#side [data-tab="settings"]',
            '#setnav [data-sub="access"]',
            '#setnav [data-sub="server"]',
            '#setnav [data-sub="users"]',
            '#setnav [data-sub="account"]',
          ],
          superuser: [
            '#side [data-tab="settings"]',
            '#setnav [data-sub="access"]',
            '#setnav [data-sub="server"]',
            '#setnav [data-sub="users"]',
            '#setnav [data-sub="account"]',
          ],
          user: [
            '#side [data-tab="settings"]',
            '#setnav [data-sub="access"]',
            '#setnav [data-sub="account"]',
          ],
        },
      }),
  ],
  'error-dashboard-range': [
    domContract('range-error', `document.querySelector('#rangeinfo')?.textContent.trim() ?? null`, 'Failed to load time range'),
  ],
  'error-dashboard-now': [
    domContract('disconnected-label', `document.querySelector('#liveText')?.textContent.trim() ?? null`, 'Disconnected'),
  ],
  'error-settings-load': [
    domContract('settings-load-error',
      `document.querySelector('#setbody .empty')?.textContent.includes('Could not load Settings. Reload the page and sign in again.') ?? false`,
      true),
  ],
  'error-settings-mutation': [
    domContract('typed-api-error', `document.querySelector('#history-err')?.textContent.trim() ?? null`, 'invalid history fixture'),
  ],
  'error-settings-fallback': [
    domContract('catalog-request-fallback',
      `document.querySelector('#history-err')?.textContent.trim() ?? null`,
      'Request failed (HTTP 400).'),
  ],
  'error-nim-key-validation': [
    domContract('raw-validation-error',
      `document.querySelector('#nk-err')?.textContent.trim() ?? null`,
      'validation fixture rejected'),
    domContract('validation-error-not-green',
      `document.querySelector('#nk-err')?.classList.contains('ok') ?? null`, false),
    domContract('add-anyway-visible', `document.querySelector('#nk-force')?.hidden ?? null`, false),
  ],
  'error-setup-key-validation': [
    domContract('setup-validation-error', `document.querySelector('#err')?.textContent.toLowerCase().includes('validation') ?? false`, true),
  ],
  'setup-key-validation-success': [
    domContract('setup-key-added',
      `Array.from(document.querySelectorAll('#keylist > li')).some(row => row.textContent.includes('40 rpm'))`,
      true),
  ],
  'error-setup-submit': [
    domContract('setup-api-error', `document.querySelector('#err')?.textContent.trim() ?? null`, 'setup fixture rejected'),
    domContract('finish-reenabled', `document.querySelector('#finish')?.disabled ?? null`, false),
  ],
  'setup-submit-success': [
    domContract('connection-step-visible', `document.querySelector('#step4')?.hidden ?? null`, false),
    domContract('minted-secret-byte-exact', `document.querySelector('#csecret')?.textContent ?? null`, 'npk_fixture_secret'),
  ],
  'state-empty': [
    domContract('empty-state-present', `!!document.querySelector('#tab-overview .empty')`, true),
    domContract('empty-range-message', `document.querySelector('#rangeinfo')?.textContent.trim() ?? null`, 'No data yet'),
  ],
  'state-loading': [
    domContract('catalog-gate-held', `document.body.hidden`, true),
  ],
  'state-partial-incomplete': [
    domContract('partial-window-marker', `document.querySelector('#rangeinfo')?.textContent.includes('Partial history ·') ?? false`, true),
    domContract('partial-window-uses-effective-bounds',
      `(() => {
        const text = document.querySelector('#rangeinfo')?.textContent ?? '';
        const format = new Intl.DateTimeFormat(document.documentElement.lang, { month: 'short', day: 'numeric' });
        return text.includes(format.format(1699913600 * 1000))
          && text.includes(format.format(1700003600 * 1000));
      })()`,
      true),
  ],
  'state-observation-measured': [
    domContract('measured-observation-quality',
      `(() => { const rows = Array.from(document.querySelectorAll('#o-health .kv')); const row = rows.filter(row => row.textContent.trim() === 'Observation qualityMeasured'); const queued = rows.findIndex(row => row.textContent.trim() === 'Queued1'); return queued >= 0 && row.length === 1 && rows[queued + 1] === row[0] && row[0].className === 'kv'; })()`,
      true),
  ],
  'state-observation-estimated': [
    domContract('estimated-observation-quality',
      `(() => { const rows = Array.from(document.querySelectorAll('#o-health .kv')); const row = rows.filter(row => row.textContent.trim() === 'Observation qualityEstimated'); const queued = rows.findIndex(row => row.textContent.trim() === 'Queued1'); return queued >= 0 && row.length === 1 && rows[queued + 1] === row[0] && row[0].className === 'kv warn'; })()`,
      true),
  ],
  'state-observation-unavailable': [
    domContract('unavailable-observation-quality-is-not-zero',
      `(() => { const rows = Array.from(document.querySelectorAll('#o-health .kv')); const row = rows.filter(row => row.textContent.trim() === 'Observation qualityUnavailable'); const queued = rows.findIndex(row => row.textContent.trim() === 'Queued1'); return queued >= 0 && row.length === 1 && rows[queued + 1] === row[0] && row[0].className === 'kv warn'; })()`,
      true),
  ],
  'state-observation-invalid': [
    domContract('invalid-observation-quality-is-not-zero',
      `(() => { const rows = Array.from(document.querySelectorAll('#o-health .kv')); const row = rows.filter(row => row.textContent.trim() === 'Observation qualityInvalid'); const queued = rows.findIndex(row => row.textContent.trim() === 'Queued1'); return queued >= 0 && row.length === 1 && rows[queued + 1] === row[0] && row[0].className === 'kv crit'; })()`,
      true),
  ],
  'state-observation-incomplete-history': [
    domContract('measured-quality-survives-incomplete-history',
      `(() => { const rows = Array.from(document.querySelectorAll('#o-health .kv')); const row = rows.filter(row => row.textContent.trim() === 'Observation qualityMeasured'); const queued = rows.findIndex(row => row.textContent.trim() === 'Queued1'); return queued >= 0 && row.length === 1 && rows[queued + 1] === row[0] && row[0].className === 'kv'; })()`,
      true),
    domContract('incomplete-history-remains-separate',
      `document.querySelector('#rangeinfo')?.textContent.includes('Partial history ·') ?? false`,
      true),
  ],
  'state-observation-zero': [
    domContract('zero-observation-quality-is-no-observations',
      `(() => { const rows = Array.from(document.querySelectorAll('#o-health .kv')); const row = rows.filter(row => row.textContent.trim() === 'Observation qualityNo observations'); const queued = rows.findIndex(row => row.textContent.trim() === 'Queued1'); return queued >= 0 && row.length === 1 && rows[queued + 1] === row[0] && row[0].className === 'kv zero'; })()`,
      true),
  ],
  'state-observation-absent': [
    domContract('absent-observation-quality-is-unavailable',
      `(() => { const rows = Array.from(document.querySelectorAll('#o-health .kv')); const row = rows.filter(row => row.textContent.trim() === 'Observation qualityUnavailable'); const queued = rows.findIndex(row => row.textContent.trim() === 'Queued1'); return queued >= 0 && row.length === 1 && rows[queued + 1] === row[0] && row[0].className === 'kv warn'; })()`,
      true),
  ],
  'state-observation-tie-invalid': [
    domContract('invalid-wins-observation-quality-tie',
      `(() => { const rows = Array.from(document.querySelectorAll('#o-health .kv')); const row = rows.filter(row => row.textContent.trim() === 'Observation qualityInvalid'); const queued = rows.findIndex(row => row.textContent.trim() === 'Queued1'); return queued >= 0 && row.length === 1 && rows[queued + 1] === row[0] && row[0].className === 'kv crit'; })()`,
      true),
  ],
  'state-observation-tie-unavailable': [
    domContract('unavailable-wins-observation-quality-tie',
      `(() => { const rows = Array.from(document.querySelectorAll('#o-health .kv')); const row = rows.filter(row => row.textContent.trim() === 'Observation qualityUnavailable'); const queued = rows.findIndex(row => row.textContent.trim() === 'Queued1'); return queued >= 0 && row.length === 1 && rows[queued + 1] === row[0] && row[0].className === 'kv warn'; })()`,
      true),
  ],
  'state-observation-tie-estimated': [
    domContract('estimated-wins-observation-quality-tie',
      `(() => { const rows = Array.from(document.querySelectorAll('#o-health .kv')); const row = rows.filter(row => row.textContent.trim() === 'Observation qualityEstimated'); const queued = rows.findIndex(row => row.textContent.trim() === 'Queued1'); return queued >= 0 && row.length === 1 && rows[queued + 1] === row[0] && row[0].className === 'kv warn'; })()`,
      true),
  ],
  'state-observation-live-tail': [
    domContract('live-tail-observation-quality-ignores-negative-row',
      `(() => { const rows = Array.from(document.querySelectorAll('#o-health .kv')); const row = rows.filter(row => row.textContent.trim() === 'Observation qualityEstimated'); const queued = rows.findIndex(row => row.textContent.trim() === 'Queued1'); return queued >= 0 && row.length === 1 && rows[queued + 1] === row[0] && row[0].className === 'kv warn'; })()`,
      true),
  ],
  'state-history-unavailable': [
    domContract('unavailable-range-message', `document.querySelector('#rangeinfo')?.textContent.trim() ?? null`, 'History unavailable for selected time range'),
  ],
  'state-history-outside-before': [
    domContract('outside-before-range-message', `document.querySelector('#rangeinfo')?.textContent.trim() ?? null`, 'No data in selected time range'),
  ],
  'state-history-outside-after': [
    domContract('outside-after-range-message', `document.querySelector('#rangeinfo')?.textContent.trim() ?? null`, 'No data in selected time range'),
  ],
  'state-extreme-numeric': [
    domContract('finite-layout',
      `Number.isFinite(document.documentElement.scrollWidth) && !/(?:NaN|Infinity)/.test(document.body.textContent)`,
      true),
  ],
  'state-settings-exact-large-values': [
    domContract('grouped-pool-capacity',
      `document.querySelector('#pool-note')?.textContent.trim() ?? null`,
      'Pool: 12,345 enabled · Total: 67,890 rpm'),
    domContract('grouped-key-state',
      `document.querySelector('[data-ksfp="f17e0001"]')?.textContent.trim() ?? null`,
      '12,345 / 67,890 in window'),
  ],
  'state-long-machine-values': [
    domContract('machine-values-byte-exact-by-owning-surface',
      `(() => {
        const section = document.querySelector('main > section:not([hidden])')?.id;
        const client = 'client-fixture-' + 'c'.repeat(49);
        const publisher = 'publisher-fixture-' + 'p'.repeat(20);
        const model = publisher + '/model-' + 'm'.repeat(19);
        const user = 'user-fixture-' + 'u'.repeat(19);
        if (section === 'tab-models') {
          return {
            model: Array.from(document.querySelectorAll('.mname')).some(node => node.title === model),
            publisher: Array.from(document.querySelectorAll('.mpub')).some(node => node.textContent === publisher),
            surface: 'models',
          };
        }
        if (section === 'tab-clients') {
          return {
            client: Array.from(document.querySelectorAll('#table-harness tbody td:first-child'))
              .some(node => node.textContent.trim() === client),
            surface: 'clients',
          };
        }
        if (section === 'tab-settings' && document.querySelector('#u-add')) {
          return {
            surface: 'users',
            user: Array.from(document.querySelectorAll('#setbody .krow'))
              .some(node => node.textContent.includes(user)),
          };
        }
        return {
          client: document.querySelector('#setbody')?.textContent.includes(client) ?? false,
          surface: 'access',
        };
      })()`,
      [
        { model: true, publisher: true, surface: 'models' },
        { client: true, surface: 'clients' },
        { client: true, surface: 'access' },
        { surface: 'users', user: true },
      ],
      {
        kind: 'sequence',
        steps: [
          '#side [data-tab="models"]',
          '#side [data-tab="clients"]',
          '#side [data-tab="settings"]',
          '#setnav [data-sub="users"]',
        ],
      }),
  ],
};

for (const row of INTERACTION_ROWS) {
  row.dom = DOM_CONTRACTS[row.id];
}

const getRequest = (path, query = {}) => ({
  method: 'GET',
  path,
  query,
  body: null,
});
// Startup request order begins after the initial document's static CSS/script
// fetches. STARTUP_ASSETS separately proves every required asset was served;
// this sequence proves the ordered application fetch/load chain.
const EXPLICIT_REQUEST_SEQUENCES = {
  'startup-dashboard-healthy': [
    getRequest('/api/locale-bootstrap'),
    getRequest('/api/config'),
    getRequest('/assets/operator/locales/en-US.json'),
    getRequest('/assets/operator/settings.js'),
    getRequest('/assets/operator/dashboard.js'),
    getRequest('/api/dashboard/now'),
    getRequest('/api/dashboard', {
      from: '1697411600',
      points: '288',
      to: '1700003600',
    }),
  ],
  'startup-setup-healthy': [
    getRequest('/api/locale-bootstrap'),
    getRequest('/assets/public/locales/en-US.json'),
  ],
  'startup-login-healthy': [
    getRequest('/api/locale-bootstrap'),
    getRequest('/assets/public/locales/en-US.json'),
  ],
  'login-invalid-credentials': [
    {
      method: 'POST',
      path: '/login',
      query: {},
      body: 'username=fixture-user&password=incorrect-password',
    },
    getRequest('/api/locale-bootstrap'),
    getRequest('/assets/public/locales/en-US.json'),
  ],
  'navigation-dashboard-tabs': [
    getRequest('/api/config'),
  ],
  'navigation-settings-role-panels': [
    getRequest('/api/config'),
    getRequest('/api/config'),
    getRequest('/api/config'),
  ],
  'filtering-settings-role': [
    getRequest('/api/config'),
  ],
  'locale-actions-remain-dormant': [
    getRequest('/api/config'),
    getRequest('/api/config'),
    getRequest('/api/config'),
  ],
  'state-empty': [
    getRequest('/api/dashboard/now'),
    getRequest('/api/dashboard', {
      from: '1697411600',
      points: '288',
      to: '1700003600',
    }),
  ],
  'state-loading': [
    getRequest('/api/locale-bootstrap'),
  ],
  'state-partial-incomplete': [
    getRequest('/api/dashboard', {
      from: '1697411600',
      points: '288',
      to: '1700003600',
    }),
  ],
  'state-observation-measured': [
    getRequest('/api/dashboard', {
      from: '1697411600', points: '288', to: '1700003600',
    }),
  ],
  'state-observation-estimated': [
    getRequest('/api/dashboard', {
      from: '1697411600', points: '288', to: '1700003600',
    }),
  ],
  'state-observation-unavailable': [
    getRequest('/api/dashboard', {
      from: '1697411600', points: '288', to: '1700003600',
    }),
  ],
  'state-observation-invalid': [
    getRequest('/api/dashboard', {
      from: '1697411600', points: '288', to: '1700003600',
    }),
  ],
  'state-observation-incomplete-history': [
    getRequest('/api/dashboard', {
      from: '1697411600', points: '288', to: '1700003600',
    }),
  ],
  'state-observation-zero': [
    getRequest('/api/dashboard', {
      from: '1697411600', points: '288', to: '1700003600',
    }),
  ],
  'state-observation-absent': [
    getRequest('/api/dashboard', {
      from: '1697411600', points: '288', to: '1700003600',
    }),
  ],
  'state-observation-tie-invalid': [
    getRequest('/api/dashboard', {
      from: '1697411600', points: '288', to: '1700003600',
    }),
  ],
  'state-observation-tie-unavailable': [
    getRequest('/api/dashboard', {
      from: '1697411600', points: '288', to: '1700003600',
    }),
  ],
  'state-observation-tie-estimated': [
    getRequest('/api/dashboard', {
      from: '1697411600', points: '288', to: '1700003600',
    }),
  ],
  'state-observation-live-tail': [
    getRequest('/api/dashboard', {
      from: '1697411600', points: '288', to: '1700003600',
    }),
  ],
  'state-history-unavailable': [
    getRequest('/api/dashboard', {
      from: '1699000000',
      points: '288',
      to: '1699003600',
    }),
  ],
  'state-history-outside-before': [
    getRequest('/api/dashboard', {
      from: '1697000000',
      points: '288',
      to: '1697003600',
    }),
  ],
  'state-history-outside-after': [
    getRequest('/api/dashboard', {
      from: '1700100000',
      points: '288',
      to: '1700103600',
    }),
  ],
  'state-extreme-numeric': [
    getRequest('/api/dashboard', {
      from: '1697411600',
      points: '288',
      to: '1700003600',
    }),
  ],
  'state-long-machine-values': [
    getRequest('/api/dashboard', {
      from: '1697411600',
      points: '288',
      to: '1700003600',
    }),
    getRequest('/api/config'),
  ],
};
for (const row of INTERACTION_ROWS) {
  if (EXPLICIT_REQUEST_SEQUENCES[row.id]) {
    row.requests = EXPLICIT_REQUEST_SEQUENCES[row.id];
  }
}

const STARTUP_ASSETS = {
  'startup-dashboard-healthy': [
    '/assets/operator/operator.css',
    '/assets/operator/shared.js',
    '/assets/operator/locales/en-US.json',
    '/assets/operator/settings.js',
    '/assets/operator/dashboard.js',
  ],
  'startup-setup-healthy': [
    '/assets/public/public.css',
    '/assets/public/setup.js',
    '/assets/public/locales/en-US.json',
  ],
  'startup-login-healthy': [
    '/assets/public/public.css',
    '/assets/public/login.js',
    '/assets/public/locales/en-US.json',
  ],
  'login-invalid-credentials': [
    '/assets/public/public.css',
    '/assets/public/login.js',
    '/assets/public/locales/en-US.json',
  ],
};
for (const row of INTERACTION_ROWS) {
  row.assets = STARTUP_ASSETS[row.id] || [];
}

const dashboardRecipe = (overrides = {}) => ({
  page: 'dashboard',
  role: 'superuser',
  locale: 'locale-bootstrap.json',
  config: 'config-superuser.json',
  now: 'dashboard-now-initial.json',
  range: 'dashboard-healthy.json',
  response: null,
  boundary: 'after-ready',
  requires: [],
  ...overrides,
});
const setupRecipe = (overrides = {}) => ({
  page: 'setup',
  role: 'public',
  locale: 'locale-bootstrap.json',
  config: null,
  now: null,
  range: null,
  response: null,
  boundary: 'after-ready',
  requires: [],
  ...overrides,
});
const loginRecipe = (overrides = {}) => ({
  page: 'login',
  role: 'public',
  locale: 'locale-bootstrap.json',
  config: null,
  now: null,
  range: null,
  response: null,
  boundary: 'after-ready',
  requires: [],
  ...overrides,
});

const FIXTURE_SCENARIOS = {
  'startup-dashboard-healthy': dashboardRecipe({ boundary: 'startup-application' }),
  'startup-setup-healthy': setupRecipe({ boundary: 'startup-application' }),
  'startup-login-healthy': loginRecipe({ boundary: 'startup-application' }),
  'login-invalid-credentials': loginRecipe(),
  'navigation-settings-role-panels': dashboardRecipe({
    role: ['superuser', 'admin', 'user'],
    config: {
      admin: 'config-admin.json',
      superuser: 'config-superuser.json',
      user: 'config-user.json',
    },
  }),
  'navigation-dashboard-tabs': dashboardRecipe(),
  'sorting-table-ascending': dashboardRecipe(),
  'sorting-table-descending': dashboardRecipe({
    requires: ['sorting-table-ascending'],
  }),
  'sorting-table-persists-rerender': dashboardRecipe({
    response: 'dashboard-now-changed.json',
    requires: ['sorting-table-descending'],
  }),
  'filtering-settings-role': dashboardRecipe({
    role: 'user',
    config: 'config-user.json',
  }),
  'refresh-dashboard-now': dashboardRecipe({
    now: {
      after: 'dashboard-now-changed.json',
      before: 'dashboard-now-initial.json',
    },
  }),
  'refresh-settings-access': dashboardRecipe({
    config: {
      after: 'scenarios.json#access-refreshed',
      before: 'scenarios.json#access-before',
    },
  }),
  'history-window-preset': dashboardRecipe({
    response: 'scenarios.json#dashboard-one-hour-window',
  }),
  'history-window-custom': dashboardRecipe({
    response: 'scenarios.json#dashboard-custom-window',
  }),
  'history-window-freeze': dashboardRecipe({
    range: 'scenarios.json#dashboard-one-hour-window',
  }),
  'history-window-resume': dashboardRecipe({
    response: 'scenarios.json#dashboard-one-hour-window',
    requires: ['history-window-preset', 'history-window-freeze'],
  }),
  'dialog-client-secret-open': dashboardRecipe({
    config: {
      after: 'scenarios.json#client-created-after',
      before: 'scenarios.json#client-create-before',
    },
    response: 'clients-secret-present.json',
  }),
  'dialog-client-secret-close': dashboardRecipe({
    config: 'scenarios.json#client-created-after',
    requires: ['dialog-client-secret-open'],
  }),
  'dialog-client-secret-escape-focus-return': dashboardRecipe({
    config: 'scenarios.json#client-created-after',
    requires: ['dialog-client-secret-open'],
  }),
  'dialog-confirm-cancel-focus-return': dashboardRecipe({
    config: 'scenarios.json#access-before',
  }),
  'mutation-nim-key-create': dashboardRecipe({
    config: {
      after: 'scenarios.json#nim-created-after',
      before: 'scenarios.json#nim-create-before',
    },
    now: 'scenarios.json#dashboard-now-one-key',
    response: ['validate-success.json', 'ok.json'],
  }),
  'mutation-nim-key-rpm': dashboardRecipe({
    config: {
      after: 'scenarios.json#nim-rpm-after',
      before: 'scenarios.json#access-before',
    },
    response: 'ok.json',
  }),
  'mutation-nim-key-toggle': dashboardRecipe({
    config: {
      after: 'scenarios.json#nim-disabled-after',
      before: 'scenarios.json#access-before',
    },
    response: 'ok.json',
  }),
  'mutation-nim-key-delete': dashboardRecipe({
    config: {
      after: 'scenarios.json#nim-deleted-after',
      before: 'scenarios.json#access-before',
    },
    response: 'ok.json',
  }),
  'mutation-client-key-delete': dashboardRecipe({
    config: {
      after: 'scenarios.json#client-deleted-after',
      before: 'scenarios.json#client-created-after',
    },
    response: 'ok.json',
  }),
  'mutation-client-auth-mode': dashboardRecipe({
    config: {
      after: 'scenarios.json#auth-keyed-after',
      before: 'scenarios.json#auth-open-before',
    },
    now: 'scenarios.json#dashboard-now-open-auth',
    response: 'ok.json',
  }),
  'mutation-server-limits': dashboardRecipe({
    config: {
      after: 'scenarios.json#limits-after',
      before: 'scenarios.json#limits-before',
    },
    response: ['ok.json', 'ok.json'],
  }),
  'mutation-server-history': dashboardRecipe({
    config: {
      after: 'scenarios.json#history-after',
      before: 'scenarios.json#history-before',
    },
    response: 'ok.json',
  }),
  'mutation-governor-toggle': dashboardRecipe({
    config: {
      after: 'scenarios.json#governor-disabled-after',
      before: 'scenarios.json#governor-enabled-before',
    },
    response: 'ok.json',
  }),
  'mutation-governor-override-create': dashboardRecipe({
    config: {
      after: 'scenarios.json#override-created-after',
      before: 'scenarios.json#override-create-before',
    },
    response: 'ok.json',
  }),
  'mutation-governor-override-delete': dashboardRecipe({
    config: {
      after: 'scenarios.json#override-deleted-after',
      before: 'scenarios.json#override-delete-before',
    },
    response: 'ok.json',
  }),
  'mutation-user-create': dashboardRecipe({
    config: {
      after: 'scenarios.json#users-created-after',
      before: 'scenarios.json#users-create-before',
    },
    now: 'scenarios.json#dashboard-now-one-key',
    response: 'ok.json',
  }),
  'mutation-user-role': dashboardRecipe({
    config: {
      after: 'scenarios.json#users-role-admin-after',
      before: 'scenarios.json#users-role-before',
    },
    response: 'ok.json',
  }),
  'mutation-user-password-reset': dashboardRecipe({
    config: 'scenarios.json#users-role-before',
    response: 'ok.json',
  }),
  'dialog-user-password-prompt-cancel': dashboardRecipe({
    config: 'scenarios.json#users-role-before',
  }),
  'mutation-user-delete': dashboardRecipe({
    config: {
      after: 'scenarios.json#users-deleted-after',
      before: 'scenarios.json#users-delete-before',
    },
    response: 'ok.json',
  }),
  'account-password-validation': dashboardRecipe({
    role: 'user',
    config: 'config-user.json',
  }),
  'account-password-update': dashboardRecipe({
    role: 'user',
    config: 'config-user.json',
    response: 'ok.json',
  }),
  'locale-actions-remain-dormant': dashboardRecipe({
    role: ['superuser', 'admin', 'user'],
    config: {
      admin: 'config-admin.json',
      superuser: 'config-superuser.json',
      user: 'config-user.json',
    },
  }),
  'error-dashboard-range': dashboardRecipe({ response: 'api-error.json' }),
  'error-dashboard-now': dashboardRecipe({ response: 'api-error.json' }),
  'error-settings-load': dashboardRecipe({
    config: {
      action: 'api-error.json',
      before: 'config-superuser.json',
    },
  }),
  'error-settings-mutation': dashboardRecipe({ response: 'api-error.json' }),
  'error-settings-fallback': dashboardRecipe({ response: 'api-error.json' }),
  'error-nim-key-validation': dashboardRecipe({ response: 'validate-failure.json' }),
  'error-setup-key-validation': setupRecipe({ response: 'validate-failure.json' }),
  'setup-key-validation-success': setupRecipe({ response: 'validate-success.json' }),
  'error-setup-submit': setupRecipe({ response: 'scenarios.json#setup-error' }),
  'setup-submit-success': setupRecipe({ response: 'setup-minted-client-key.json' }),
  'state-empty': dashboardRecipe({
    now: 'scenarios.json#dashboard-now-empty',
    range: 'dashboard-empty.json',
    boundary: 'startup-dashboard-data',
  }),
  'state-loading': dashboardRecipe({
    boundary: 'startup-held-bootstrap',
  }),
  'state-partial-incomplete': dashboardRecipe({
    now: 'scenarios.json#dashboard-now-partial',
    range: 'dashboard-partial.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-observation-measured': dashboardRecipe({
    range: 'dashboard-observation-measured.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-observation-estimated': dashboardRecipe({
    range: 'dashboard-observation-estimated.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-observation-unavailable': dashboardRecipe({
    range: 'dashboard-observation-unavailable.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-observation-invalid': dashboardRecipe({
    range: 'dashboard-observation-invalid.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-observation-incomplete-history': dashboardRecipe({
    now: 'scenarios.json#dashboard-now-partial',
    range: 'dashboard-observation-incomplete.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-observation-zero': dashboardRecipe({
    range: 'dashboard-observation-zero.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-observation-absent': dashboardRecipe({
    range: 'dashboard-observation-absent.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-observation-tie-invalid': dashboardRecipe({
    range: 'dashboard-observation-tie-invalid.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-observation-tie-unavailable': dashboardRecipe({
    range: 'dashboard-observation-tie-unavailable.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-observation-tie-estimated': dashboardRecipe({
    range: 'dashboard-observation-tie-estimated.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-observation-live-tail': dashboardRecipe({
    now: 'dashboard-now-observation-tail.json',
    range: 'dashboard-observation-absent.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-history-unavailable': dashboardRecipe({
    range: 'dashboard-unavailable.json',
    boundary: 'after-ready',
  }),
  'state-history-outside-before': dashboardRecipe({
    range: 'dashboard-outside-before.json',
    boundary: 'after-ready',
  }),
  'state-history-outside-after': dashboardRecipe({
    range: 'dashboard-outside-after.json',
    boundary: 'after-ready',
  }),
  'state-extreme-numeric': dashboardRecipe({
    range: 'dashboard-extreme.json',
    boundary: 'startup-dashboard-range',
  }),
  'state-settings-exact-large-values': dashboardRecipe({
    config: 'scenarios.json#access-before',
  }),
  'state-long-machine-values': dashboardRecipe({
    config: 'scenarios.json#long-values',
    range: 'dashboard-long.json',
    boundary: 'startup-range-through-action',
  }),
};
for (const row of INTERACTION_ROWS) {
  if (FIXTURE_SCENARIOS[row.id]) row.fixtures = FIXTURE_SCENARIOS[row.id];
}

function nestedFixtureReferences(value) {
  if (typeof value === 'string') return [value];
  if (Array.isArray(value)) return value.flatMap(nestedFixtureReferences);
  if (value && typeof value === 'object')
    return Object.values(value).flatMap(nestedFixtureReferences);
  return [];
}

function fixtureReferences(recipe) {
  return ['locale', 'config', 'now', 'range', 'response']
    .flatMap(field => nestedFixtureReferences(recipe[field]));
}

function generatedFixtureManifest() {
  const directory = path.join(ROOT, 'tests', 'fixtures', 'ui');
  const scenariosPath = path.join(directory, 'scenarios.json');
  if (!fs.existsSync(scenariosPath)) return null;
  const files = fs.readdirSync(directory)
    .filter(file => file.endsWith('.json'));
  const manifest = new Map(files.map(file => [file, null]));
  const scenarios = JSON.parse(fs.readFileSync(scenariosPath, 'utf8'));
  manifest.set('scenarios.json', new Set(Object.keys(scenarios)));
  return manifest;
}

function validFixtureReference(reference, manifest) {
  const [file, key, extra] = reference.split('#');
  if (extra !== undefined || !manifest.has(file)) return false;
  return file === 'scenarios.json'
    ? Boolean(key) && manifest.get(file)?.has(key)
    : key === undefined;
}

function fixtureReferenceProblems(rows, manifest) {
  if (!manifest) return ['generated fixture manifest is unavailable'];
  const problems = [];
  for (const row of rows) {
    for (const reference of fixtureReferences(row.fixtures)) {
      if (!validFixtureReference(reference, manifest)) {
        problems.push(`${row.id}: fixture-reference-drift ${reference}`);
      }
    }
  }
  return problems;
}

const FIXTURE_RECIPE_KEYS = [
  'boundary', 'config', 'locale', 'now', 'page',
  'range', 'requires', 'response', 'role',
];

function fixtureRecipeProblems(rows) {
  const problems = [];
  const byId = new Map(rows.map(row => [row.id, row]));
  const rowIndexes = new Map(rows.map((row, index) => [row.id, index]));
  for (const row of rows) {
    const recipe = row.fixtures;
    if (!recipe || typeof recipe !== 'object' || Array.isArray(recipe)) {
      problems.push(`${row.id}: missing complete fixture recipe`);
      continue;
    }
    const keys = Object.keys(recipe).sort();
    if (canonicalJson(keys) !== canonicalJson(FIXTURE_RECIPE_KEYS)) {
      problems.push(`${row.id}: incomplete fixture recipe keys ${canonicalJson(keys)}`);
    }
    if (!['dashboard', 'login', 'setup'].includes(recipe.page)) {
      problems.push(`${row.id}: invalid fixture page ${recipe.page}`);
    }
    const roleContexts = Array.isArray(recipe.role)
      && canonicalJson([...recipe.role].sort())
        === canonicalJson(['admin', 'superuser', 'user']);
    if (!['public', 'superuser', 'admin', 'user'].includes(recipe.role)
        && !roleContexts) {
      problems.push(`${row.id}: invalid fixture role/context ${canonicalJson(recipe.role)}`);
    }
    if (recipe.page === 'dashboard'
        && (recipe.config == null || recipe.now == null || recipe.range == null)) {
      problems.push(`${row.id}: dashboard recipe is missing config/now/range`);
    }
    if (recipe.page !== 'dashboard'
        && (recipe.role !== 'public'
          || recipe.config !== null || recipe.now !== null || recipe.range !== null)) {
      problems.push(`${row.id}: public-page recipe exposes operator fixtures`);
    }
    if (!['after-ready', 'startup-application', 'startup-dashboard-data',
      'startup-dashboard-range', 'startup-held-bootstrap',
      'startup-range-through-action'].includes(recipe.boundary)) {
      problems.push(`${row.id}: invalid observation boundary ${recipe.boundary}`);
    }
    if (!Array.isArray(recipe.requires)
        || new Set(recipe.requires).size !== recipe.requires.length) {
      problems.push(`${row.id}: prerequisites must be a unique array`);
    } else {
      for (const dependency of recipe.requires) {
        if (!byId.has(dependency) || dependency === row.id) {
          problems.push(`${row.id}: invalid prerequisite ${dependency}`);
        } else if (rowIndexes.get(dependency) >= rowIndexes.get(row.id)) {
          problems.push(`${row.id}: prerequisite ${dependency} must precede the row`);
        }
      }
    }
    const requests = expectedRequestSequence(row);
    const paths = requests.map(request => request.path);
    if (recipe.boundary === 'startup-application'
        && paths[0] !== '/api/locale-bootstrap') {
      problems.push(`${row.id}: startup boundary must begin at locale bootstrap`);
    }
    if (recipe.boundary === 'startup-held-bootstrap'
        && canonicalJson(paths) !== canonicalJson(['/api/locale-bootstrap'])) {
      problems.push(`${row.id}: held-bootstrap boundary must observe only bootstrap`);
    }
    if (recipe.boundary === 'startup-dashboard-data'
        && canonicalJson(paths)
          !== canonicalJson(['/api/dashboard/now', '/api/dashboard'])) {
      problems.push(`${row.id}: dashboard-data boundary must observe Now then range`);
    }
    if (recipe.boundary === 'startup-dashboard-range'
        && canonicalJson(paths) !== canonicalJson(['/api/dashboard'])) {
      problems.push(`${row.id}: dashboard-range boundary must observe exactly one range`);
    }
    if (recipe.boundary === 'startup-range-through-action'
        && canonicalJson(paths) !== canonicalJson(['/api/dashboard', '/api/config'])) {
      problems.push(`${row.id}: range-through-action boundary must observe range then config`);
    }
  }
  const visiting = new Set();
  const visited = new Set();
  const visit = (id) => {
    if (visiting.has(id)) {
      problems.push(`${id}: fixture prerequisite cycle`);
      return;
    }
    if (visited.has(id)) return;
    visiting.add(id);
    for (const dependency of byId.get(id)?.fixtures?.requires || []) visit(dependency);
    visiting.delete(id);
    visited.add(id);
  };
  for (const row of rows) visit(row.id);
  return problems;
}

const SETTINGS_MUTATIONS_THAT_RELOAD_CONFIG = new Set([
  'dialog-client-secret-open',
  'mutation-nim-key-create',
  'mutation-nim-key-rpm',
  'mutation-nim-key-toggle',
  'mutation-nim-key-delete',
  'mutation-client-key-delete',
  'mutation-client-auth-mode',
  'mutation-server-limits',
  'mutation-server-history',
  'mutation-governor-toggle',
  'mutation-governor-override-create',
  'mutation-governor-override-delete',
  'mutation-user-create',
  'mutation-user-role',
  'mutation-user-delete',
]);
for (const row of INTERACTION_ROWS) {
  if (!SETTINGS_MUTATIONS_THAT_RELOAD_CONFIG.has(row.id)) continue;
  const mutations = (row.requests || [row.request]).map(normalizeRequest);
  row.requests = [
    ...mutations,
    { method: 'GET', path: '/api/config', query: {}, body: null },
  ];
  delete row.request;
}

function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (value && typeof value === 'object') {
    return `{${Object.keys(value).sort().map(key =>
      `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(',')}}`;
  }
  return JSON.stringify(value);
}

const CLEAN_RUN_FIELDS = [
  'pageErrors',
  'consoleErrors',
  'promiseRejections',
  'failedAssets',
  'unexpectedRequests',
  'unconsumedFixtures',
];

function normalizeRequest(request) {
  return {
    method: request.method,
    path: request.path,
    query: request.query || {},
    body: Object.prototype.hasOwnProperty.call(request, 'body') ? request.body : null,
  };
}

function expectedRequestSequence(row) {
  if (row.requests) return row.requests.map(normalizeRequest);
  if (row.request) return [normalizeRequest(row.request)];
  return [];
}

function requestMatches(request, matcher) {
  if (matcher.method && request.method !== matcher.method) return false;
  if (matcher.path && request.path !== matcher.path) return false;
  if (matcher.query
      && canonicalJson(request.query || {}) !== canonicalJson(matcher.query)) return false;
  if (matcher.bodyContains) {
    if (!request.body || typeof request.body !== 'object') return false;
    for (const [key, value] of Object.entries(matcher.bodyContains)) {
      if (canonicalJson(request.body[key]) !== canonicalJson(value)) return false;
    }
  }
  return true;
}

function interactionCoverageProblems(observations) {
  const problems = [];
  for (const detail of fixtureRecipeProblems(INTERACTION_ROWS)) {
    problems.push({ check: 'ui-fixture-recipe-drift', detail });
  }
  const generatedManifest = generatedFixtureManifest();
  if (generatedManifest) {
    for (const detail of fixtureReferenceProblems(INTERACTION_ROWS, generatedManifest)) {
      problems.push({ check: 'ui-fixture-reference-drift', detail });
    }
  }
  for (const row of INTERACTION_ROWS) {
    const observed = observations.get(row.id);
    if (!observed) {
      problems.push({
        check: `ui-interaction:${row.id}`,
        detail: `missing ${row.category} observation for state ${row.state}`,
      });
      continue;
    }
    const actualDom = new Map((observed.dom || []).map(assertion =>
      [assertion.check, assertion]));
    for (const assertion of row.dom) {
      const actual = actualDom.get(assertion.check);
      if (!actual
          || actual.expression !== assertion.expression
          || canonicalJson(actual.collect) !== canonicalJson(assertion.collect)
          || canonicalJson(actual.result) !== canonicalJson(assertion.expected)) {
        problems.push({
          check: `ui-interaction:${row.id}`,
          detail: `DOM ${assertion.check} result ${canonicalJson(actual)} != ${canonicalJson(assertion)}`,
        });
      }
    }
    if ((observed.dom || []).length !== row.dom.length) {
      problems.push({
        check: `ui-interaction:${row.id}`,
        detail: `DOM assertion count ${(observed.dom || []).length} != ${row.dom.length}`,
      });
    }
    if (canonicalJson(observed.fixtures) !== canonicalJson(row.fixtures)) {
      problems.push({
        check: `ui-interaction:${row.id}`,
        detail: `fixture recipe ${canonicalJson(observed.fixtures)} != ${canonicalJson(row.fixtures)}`,
      });
    }
    const actualAssets = [...(observed.assets || [])].sort();
    const expectedAssets = [...row.assets].sort();
    if (canonicalJson(actualAssets) !== canonicalJson(expectedAssets)) {
      problems.push({
        check: `ui-interaction:${row.id}`,
        detail: `served assets ${canonicalJson(actualAssets)} != ${canonicalJson(expectedAssets)}`,
      });
    }
    const actualRequests = (observed.requests || []).map(normalizeRequest);
    const expectedRequests = expectedRequestSequence(row);
    if (canonicalJson(actualRequests) !== canonicalJson(expectedRequests)) {
      problems.push({
        check: `ui-interaction:${row.id}`,
        detail: `ordered requests ${canonicalJson(actualRequests)} != ${canonicalJson(expectedRequests)}`,
      });
    }
    if (row.forbiddenRequests?.some(matcher =>
      actualRequests.some(request => requestMatches(request, matcher)))) {
      problems.push({
        check: `ui-interaction:${row.id}`,
        detail: 'dormant locale preference emitted a forbidden mutation request',
      });
    }
    for (const field of CLEAN_RUN_FIELDS) {
      const values = observed.run?.[field];
      if (!Array.isArray(values) || values.length !== 0) {
        problems.push({
          check: `ui-interaction:${row.id}`,
          detail: `${field} must be an observed empty array; got ${canonicalJson(values)}`,
        });
      }
    }
  }
  return problems;
}

function completeObservation(row) {
  return {
    assets: [...row.assets],
    dom: row.dom.map(assertion => ({
      check: assertion.check,
      expression: assertion.expression,
      ...(assertion.collect ? { collect: assertion.collect } : {}),
      result: assertion.expected,
    })),
    fixtures: row.fixtures,
    requests: expectedRequestSequence(row),
    run: Object.fromEntries(CLEAN_RUN_FIELDS.map(field => [field, []])),
  };
}

function cloneObservations(observations) {
  return new Map([...observations].map(([id, observation]) =>
    [id, JSON.parse(JSON.stringify(observation))]));
}

function interactionSelftest() {
  const ids = new Set();
  const categories = new Set();
  const failures = [
    ...fixtureRecipeProblems(INTERACTION_ROWS),
    ...fixtureReferenceProblems(INTERACTION_ROWS, generatedFixtureManifest()),
  ];
  const generatedManifest = generatedFixtureManifest();
  for (const row of INTERACTION_ROWS) {
    if (ids.has(row.id)) failures.push(`duplicate row id ${row.id}`);
    ids.add(row.id);
    categories.add(row.category);
    if (!row.action) failures.push(`${row.id}: missing interaction action`);
    if (!Array.isArray(row.assets)
        || row.assets.some(asset => typeof asset !== 'string' || !asset.startsWith('/assets/'))) {
      failures.push(`${row.id}: invalid served-asset contract`);
    }
    if (!row.fixtures) failures.push(`${row.id}: missing Rust-owned fixture scenario`);
    for (const reference of fixtureReferences(row.fixtures)) {
      if (!generatedManifest || !validFixtureReference(reference, generatedManifest)) {
        failures.push(`${row.id}: invalid Rust-owned fixture reference ${reference}`);
      }
    }
    if (!Array.isArray(row.dom) || row.dom.length === 0) {
      failures.push(`${row.id}: missing executable DOM contract`);
    }
    for (const assertion of row.dom || []) {
      if (!assertion.check || !assertion.expression
          || !Object.prototype.hasOwnProperty.call(assertion, 'expected')) {
        failures.push(`${row.id}: incomplete DOM assertion`);
      } else {
        try {
          new vm.Script(assertion.expression);
        } catch (error) {
          failures.push(`${row.id}:${assertion.check}: invalid DOM expression: ${error.message}`);
        }
      }
      if (assertion.collect) {
        const sequence = assertion.collect.kind === 'sequence'
          && Array.isArray(assertion.collect.steps)
          && assertion.collect.steps.length > 0;
        const contextSequence = assertion.collect.kind === 'context-sequence'
          && assertion.collect.contexts
          && Object.keys(assertion.collect.contexts).length > 0
          && Object.values(assertion.collect.contexts)
            .every(steps => Array.isArray(steps) && steps.length > 0);
        if (!sequence && !contextSequence) {
          failures.push(`${row.id}:${assertion.check}: invalid DOM collection contract`);
        } else if (sequence
            && (!Array.isArray(assertion.expected)
              || assertion.expected.length !== assertion.collect.steps.length)) {
          failures.push(`${row.id}:${assertion.check}: sequence result shape does not match steps`);
        } else if (contextSequence) {
          const expected = assertion.expected;
          const contextNames = Object.keys(assertion.collect.contexts).sort();
          const expectedNames = expected && typeof expected === 'object' && !Array.isArray(expected)
            ? Object.keys(expected).sort()
            : [];
          if (canonicalJson(contextNames) !== canonicalJson(expectedNames)
              || contextNames.some(context =>
                !Array.isArray(expected[context])
                || expected[context].length !== assertion.collect.contexts[context].length)) {
            failures.push(`${row.id}:${assertion.check}: context-sequence result shape does not match steps`);
          }
        }
      }
    }
    if (row.request && row.requests) {
      failures.push(`${row.id}: request and requests contracts are mutually exclusive`);
    }
    if (row.request && (!row.request.method || !row.request.path
        || !Object.prototype.hasOwnProperty.call(row.request, 'body'))) {
      failures.push(`${row.id}: incomplete request contract`);
    }
    for (const request of row.requests || []) {
      if (!request.method || !request.path
          || !Object.prototype.hasOwnProperty.call(request, 'query')
          || !Object.prototype.hasOwnProperty.call(request, 'body')) {
        failures.push(`${row.id}: incomplete ordered request contract`);
      }
    }
  }
  for (const category of [
    'startup', 'navigation', 'sorting', 'filtering', 'refresh',
    'history-window', 'dialog', 'create', 'edit', 'delete', 'account',
    'locale', 'keyboard-focus', 'error', 'state',
  ]) {
    if (!categories.has(category)) failures.push(`missing category ${category}`);
  }

  const complete = new Map(INTERACTION_ROWS.map(row =>
    [row.id, completeObservation(row)]));
  if (interactionCoverageProblems(complete).length) {
    failures.push('complete observation set did not pass');
  }

  const expectStableFailure = (name, observations, rowId) => {
    const checks = interactionCoverageProblems(observations)
      .map(problem => problem.check);
    if (!checks.includes(`ui-interaction:${rowId}`)) {
      failures.push(`${name} did not fire ui-interaction:${rowId}`);
    }
  };
  const mutate = (name, rowId, change) => {
    const observations = cloneObservations(complete);
    change(observations.get(rowId));
    expectStableFailure(name, observations, rowId);
  };

  mutate('DOM result mutation', 'startup-dashboard-healthy', observation => {
    observation.dom[0].result = true;
  });
  mutate('DOM expression mutation', 'startup-dashboard-healthy', observation => {
    observation.dom[0].expression = 'true';
  });
  mutate('DOM sequence mutation', 'navigation-dashboard-tabs', observation => {
    observation.dom[0].collect.steps.pop();
  });
  mutate('DOM context mutation', 'locale-actions-remain-dormant', observation => {
    observation.dom[0].collect.contexts.user.pop();
  });
  mutate('DOM context-sequence mutation', 'navigation-settings-role-panels', observation => {
    observation.dom[0].collect.contexts.user.pop();
  });
  mutate('served asset mutation', 'startup-dashboard-healthy', observation => {
    observation.assets.pop();
  });
  {
    const row = INTERACTION_ROWS.find(candidate =>
      candidate.id === 'startup-dashboard-healthy');
    const expected = row.dom[0].expected;
    row.dom[0].expected = !expected;
    expectStableFailure('DOM expected mutation', complete, row.id);
    row.dom[0].expected = expected;
  }
  mutate('request method mutation', 'mutation-nim-key-rpm', observation => {
    observation.requests[0].method = 'DELETE';
  });
  mutate('request path mutation', 'mutation-nim-key-rpm', observation => {
    observation.requests[0].path = '/api/settings/not-nim-keys';
  });
  mutate('request query mutation', 'history-window-preset', observation => {
    observation.requests[0].query.points = '999';
  });
  mutate('request body mutation', 'mutation-user-create', observation => {
    observation.requests[0].body.add.role = 'admin';
  });
  mutate('request sequence mutation', 'mutation-server-limits', observation => {
    observation.requests.reverse();
  });
  mutate('startup request sequence mutation', 'startup-dashboard-healthy', observation => {
    [observation.requests[0], observation.requests[1]] =
      [observation.requests[1], observation.requests[0]];
  });
  mutate('request multiplicity mutation', 'mutation-server-limits', observation => {
    observation.requests.push(observation.requests[1]);
  });
  mutate('request-free row mutation', 'history-window-freeze', observation => {
    observation.requests.push(getRequest('/api/dashboard/now'));
  });
  mutate('fixture prerequisite mutation', 'sorting-table-descending', observation => {
    observation.fixtures.requires.pop();
  });
  {
    const cyclicRows = INTERACTION_ROWS.map(row => ({
      ...row,
      fixtures: {
        ...row.fixtures,
        requires: [...row.fixtures.requires],
      },
    }));
    cyclicRows.find(row => row.id === 'sorting-table-ascending')
      .fixtures.requires.push('sorting-table-descending');
    if (!fixtureRecipeProblems(cyclicRows)
      .some(problem => problem.includes('fixture prerequisite cycle'))) {
      failures.push('fixture prerequisite cycle negative probe did not fire');
    }
  }
  {
    const driftedManifest = generatedFixtureManifest();
    if (driftedManifest) {
      driftedManifest.get('scenarios.json').delete('long-values');
      if (!fixtureReferenceProblems(INTERACTION_ROWS, driftedManifest)
        .some(problem => problem.includes('fixture-reference-drift'))) {
        failures.push('fixture-reference-drift negative probe did not fire');
      }
    }
  }

  const dormant = INTERACTION_ROWS.find(row => row.id === 'locale-actions-remain-dormant');
  mutate('dormant locale body mutation', dormant.id, observation => {
    observation.requests.push({
      method: 'POST',
      path: '/api/settings/account',
      query: {},
      body: { action: 'locale', locale: 'en-US' },
    });
  });
  for (const field of CLEAN_RUN_FIELDS) {
    mutate(`${field} mutation`, 'startup-dashboard-healthy', observation => {
      observation.run[field].push(`${field} fixture`);
    });
  }
  const visualExpectationCount = resolveVisualExpectations().items.length;
  failures.push(...visualCoverageSelftest());

  if (failures.length) {
    for (const failure of failures) console.error(`[interaction-selftest] ${failure}`);
    return 1;
  }
  console.log(`interaction selftest ok — ${INTERACTION_ROWS.length} named rows, ${visualExpectationCount} visual applicability items; DOM, request, fixture-recipe/reference, locale, asset, and clean-run mutations observed`);
  return 0;
}

if (args.includes('--interaction-selftest')) process.exit(interactionSelftest());

const ASSET_CASES = [
  {
    name: 'external-script',
    files: { 'dashboard.html': '<script src="https://cdn.invalid/app.js"></script>' },
    want: 'external-script',
  },
  {
    name: 'external-stylesheet-font',
    files: {
      'dashboard.html': '<link rel="stylesheet" href="https://fonts.invalid/ui.css">',
      'operator.css': '@font-face { src: url(https://fonts.invalid/ui.woff2); }',
    },
    want: 'external-stylesheet-font',
  },
  {
    name: 'external-image',
    files: { 'dashboard.html': '<img src="https://images.invalid/model.svg">' },
    want: 'external-image',
  },
  {
    name: 'external-css-url',
    files: { 'operator.css': '.logo { background: url(http://images.invalid/logo.svg); }' },
    want: 'external-css-url',
  },
  {
    name: 'external-script-protocol-relative-unquoted',
    files: { 'dashboard.html': '<script defer src=//cdn.invalid/app.js></script>' },
    want: 'external-script',
  },
  {
    name: 'external-stylesheet-reordered-attributes',
    files: {
      'dashboard.html': '<link href="//fonts.invalid/ui.css" media="screen" rel="stylesheet">',
    },
    want: 'external-stylesheet-font',
  },
  {
    name: 'external-image-srcset-protocol-relative',
    files: {
      'dashboard.html': '<img alt="model" srcset="//images.invalid/model.svg 1x, /local.svg 2x">',
    },
    want: 'external-image',
  },
  {
    name: 'external-css-quoted-import',
    files: { 'operator.css': '@import "//fonts.invalid/ui.css";' },
    want: 'external-stylesheet-font',
  },
  {
    name: 'external-css-url-protocol-relative',
    files: { 'operator.css': '.logo { background: url("//images.invalid/logo.svg"); }' },
    want: 'external-css-url',
  },
  {
    name: 'local-only',
    files: {
      'dashboard.html': '<link rel="stylesheet" href="/assets/operator/operator.css"><script src="/assets/operator/dashboard.js"></script>',
      'operator.css': '.logo { background: none; }',
    },
    want: null,
  },
];

function isExternalUrl(value) {
  return /^(?:https?:)?\/\//i.test(String(value || '').trim());
}

function srcsetHasExternalUrl(value) {
  return String(value || '').split(',').some(candidate =>
    isExternalUrl(candidate.trim().split(/\s+/, 1)[0]));
}

/* This is deliberately a small context parser, not a spelling regex. It
   tokenizes start tags and attributes so attribute order, quote style and
   unquoted values cannot change what resource-loading context is inspected. */
function htmlElements(source) {
  const elements = [];
  const tags = /<([A-Za-z][A-Za-z0-9:-]*)(\s(?:[^"'<>]|"[^"]*"|'[^']*')*)?>/g;
  for (const match of source.matchAll(tags)) {
    const attrs = new Map();
    const attrSource = match[2] || '';
    const attributes = /([^\s"'=<>`]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+)))?/g;
    for (const attr of attrSource.matchAll(attributes)) {
      attrs.set(attr[1].toLowerCase(), attr[2] ?? attr[3] ?? attr[4] ?? '');
    }
    elements.push({
      tag: match[1].toLowerCase(),
      attrs,
      end: (match.index || 0) + match[0].length,
    });
  }
  return elements;
}

function cssUrls(source) {
  const urls = [];
  const pattern = /url\(\s*(?:"([^"]*)"|'([^']*)'|([^)\s]+))\s*\)/gi;
  for (const match of source.matchAll(pattern)) {
    urls.push(match[1] ?? match[2] ?? match[3] ?? '');
  }
  return urls;
}

function cssImports(source) {
  const urls = [];
  const pattern = /@import\s+(?:url\(\s*)?(?:"([^"]*)"|'([^']*)'|([^;\s)]+))/gi;
  for (const match of source.matchAll(pattern)) {
    urls.push(match[1] ?? match[2] ?? match[3] ?? '');
  }
  return urls;
}

function assetProblems(files) {
  const problems = [];
  for (const [filename, source] of Object.entries(files)) {
    const markup = filename.endsWith('.html') || filename.endsWith('.svg');
    if (markup) {
      for (const element of htmlElements(source)) {
        const { tag, attrs } = element;
        if (tag === 'script' && isExternalUrl(attrs.get('src'))) {
          problems.push({ check: 'external-script', detail: filename });
        }
        if (tag === 'link'
            && isExternalUrl(attrs.get('href'))
            && attrs.get('rel')?.toLowerCase().split(/\s+/)
              .some(rel => ['stylesheet', 'preconnect', 'dns-prefetch', 'icon'].includes(rel))) {
          problems.push({ check: 'external-stylesheet-font', detail: filename });
        }
        if (['img', 'image', 'source', 'video'].includes(tag)) {
          const direct = ['src', 'href', 'xlink:href', 'poster']
            .some(attr => isExternalUrl(attrs.get(attr)));
          if (direct || srcsetHasExternalUrl(attrs.get('srcset'))) {
            problems.push({ check: 'external-image', detail: filename });
          }
        }
        if (!filename.endsWith('.html')) continue;
        if (tag === 'style' || attrs.has('style')) {
          problems.push({ check: 'inline-style', detail: filename });
        }
        if ([...attrs.keys()].some(attr => /^on[a-z]+$/i.test(attr))) {
          problems.push({ check: 'inline-event-handler', detail: filename });
        }
        if (tag === 'script' && !attrs.has('src')
            && attrs.get('type')?.toLowerCase() !== 'application/json') {
          const closing = source.toLowerCase().indexOf('</script', element.end);
          if (closing < 0 || source.slice(element.end, closing).trim()) {
            problems.push({ check: 'inline-script', detail: filename });
          }
        }
      }
    }
    if (filename.endsWith('.css')) {
      const importsExternal = cssImports(source).some(isExternalUrl);
      const fontBlocks = [...source.matchAll(/@font-face\s*\{([^}]*)\}/gi)]
        .some(match => cssUrls(match[1]).some(isExternalUrl));
      if (importsExternal || fontBlocks) {
        problems.push({ check: 'external-stylesheet-font', detail: filename });
      }
      if (cssUrls(source).some(isExternalUrl) && !fontBlocks) {
        problems.push({ check: 'external-css-url', detail: filename });
      }
    }
  }
  return problems;
}

function assetSelftest() {
  const failures = [];
  for (const { name, files, want } of ASSET_CASES) {
    const problems = assetProblems(files);
    const got = problems[0] ? problems[0].check : null;
    if (got !== want) {
      failures.push(`${name}: expected check ${want || 'nothing'}, got ${got || 'nothing'}`);
    } else {
      console.log(`  ok  ${name}: ${got || 'no problem'}`);
    }
  }
  if (failures.length) {
    for (const failure of failures) console.error(failure);
    return 1;
  }
  console.log('asset selftest ok — every external-origin check observed');
  return 0;
}

function sourceFilesUnder(dir) {
  if (!fs.existsSync(dir)) return [];
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...sourceFilesUnder(full));
    else out.push(full);
  }
  return out;
}

function assetsOnly() {
  const web = path.join(ROOT, 'src', 'web');
  const paths = sourceFilesUnder(web);
  if (!paths.length) {
    console.error('[asset-directory] src/web has no split presentation sources');
    return 1;
  }
  const files = Object.fromEntries(paths.map((filename) => [
    path.relative(web, filename),
    fs.readFileSync(filename, 'utf8'),
  ]));
  const problems = assetProblems(files);
  if (problems.length) {
    for (const { check, detail } of problems) console.error(`[${check}] ${detail}`);
    return 1;
  }
  console.log(`presentation assets OK — ${paths.length} local sources; no external origins or inline executable/style contexts`);
  return 0;
}

if (args.includes('--asset-selftest')) process.exit(assetSelftest());
if (args.includes('--assets-only')) process.exit(assetsOnly());

const SYNTAX_CASES = [
  { name: 'valid', html: '<script>const ok = 1;</script>', want: null },
  { name: 'invalid', html: '<script>const = ;</script>', want: 'syntax' },
  { name: 'missing', html: '<main></main>', want: 'script-block' },
  {
    name: 'multiple',
    html: '<script>const a = 1;</script><script>const b = 2;</script>',
    want: 'script-block',
  },
];

function syntaxProblems(html, filename) {
  const blocks = [...html.matchAll(/<script>(?:\r?\n)?([\s\S]*?)<\/script>/g)];
  if (blocks.length !== 1) {
    return [{
      check: 'script-block',
      detail: `${filename}: expected one plain <script>, found ${blocks.length}`,
    }];
  }
  try {
    new vm.Script(blocks[0][1], { filename });
    return [];
  } catch (err) {
    return [{ check: 'syntax', detail: `${filename}: ${err.message}` }];
  }
}

function syntaxSelftest() {
  const failures = [];
  for (const { name, html, want } of SYNTAX_CASES) {
    const problems = syntaxProblems(html, `${name}.html`);
    const got = problems[0] ? problems[0].check : null;
    if (got !== want) {
      failures.push(`${name}: expected check ${want || 'nothing'}, got ${got || 'nothing'}`);
    } else {
      console.log(`  ok  ${name}: ${got || 'no problem'}`);
    }
  }
  if (failures.length) {
    for (const failure of failures) console.error(failure);
    return 1;
  }
  console.log('syntax selftest ok — every check observed');
  return 0;
}

if (args.includes('--syntax-selftest')) process.exit(syntaxSelftest());

function syntaxOnly() {
  const scripts = ['shared.js', 'dashboard.js', 'settings.js', 'setup.js', 'login.js'];
  const problems = [];
  for (const script of scripts) {
    const filename = path.join('src', 'web', script);
    const full = path.join(ROOT, filename);
    if (!fs.existsSync(full)) {
      problems.push({ check: 'script-file', detail: `${filename}: missing` });
      continue;
    }
    try {
      new vm.Script(fs.readFileSync(full, 'utf8'), { filename });
    } catch (err) {
      problems.push({ check: 'syntax', detail: `${filename}: ${err.message}` });
    }
  }
  if (problems.length) {
    for (const { check, detail } of problems) console.error(`[${check}] ${detail}`);
    return 1;
  }
  console.log('presentation script syntax OK — all split sources parse');
  return 0;
}

if (args.includes('--syntax-only')) process.exit(syntaxOnly());

function servedPageSelftest() {
  const failures = [];
  const runSource = String(main);
  const catalogResponseSource = typeof handleCatalogResponseForProbe === 'function'
    ? String(handleCatalogResponseForProbe)
    : '';
  const responseSources = `${runSource}\n${catalogResponseSource}\n${String(responseBody)}`;
  if (!responseSources.includes("'Fetch.getResponseBody'")) {
    failures.push('probe does not read the real response body');
  }
  if (!runSource.includes('serverPageResponses.add')) {
    failures.push('probe does not record server response provenance');
  }
  if (!catalogResponseSource.includes('serverCatalogResponses.add')) {
    failures.push('probe does not record server catalog response provenance');
  }
  if (typeof handleCatalogResponseForProbe !== 'function') {
    failures.push('catalog probe does not mutate a server response');
  }
  if (typeof probeCatalogHtml === 'function') {
    failures.push('catalog probe still mutates server HTML');
  }
  if (runSource.includes('probePageHtml()')) {
    failures.push('test-only page assembly remains reachable');
  }
  if (failures.length) {
    for (const failure of failures) console.error(`[served-page] ${failure}`);
    return 1;
  }
  console.log('served-page selftest ok — probe derives from and tracks the real response');
  return 0;
}

if (args.includes('--served-page-selftest')) process.exit(servedPageSelftest());

function operatorConfigStartupFailures({
  isDashboard,
  startupProbe,
  requestTimeline,
  catalogPath,
}) {
  const configIndexes = requestTimeline
    .map((row, index) => row.path === '/api/config' ? index : -1)
    .filter(index => index >= 0);
  const configRequired = isDashboard
    && !['bootstrap-failure', 'css-failure'].includes(startupProbe);
  if (!configRequired) {
    return configIndexes.length ? ['operator-config-forbidden'] : [];
  }
  if (configIndexes.length !== 1) return ['operator-config-count'];
  const bootstrapIndex = requestTimeline.findIndex(row =>
    row.path === '/api/locale-bootstrap');
  const catalogIndex = requestTimeline.findIndex(row =>
    row.path === catalogPath);
  return bootstrapIndex < configIndexes[0] && configIndexes[0] < catalogIndex
    ? []
    : ['operator-config-order'];
}

function catalogStartupSelftest() {
  const failures = [];
  const runSource = String(main);
  const pausedSource = String(handleCatalogResponseForStartupProbe);
  const probeSource = typeof handleCatalogResponseForProbe === 'function'
    ? String(handleCatalogResponseForProbe)
    : '';
  const appAssetSource =
    typeof handleApplicationScriptResponseForStartupProbe === 'function'
      ? String(handleApplicationScriptResponseForStartupProbe)
      : '';
  const responseSource = String(responseBody);
  const required = [
    ['catalog-response-stage', pausedSource.includes('responseBody(')
      && responseSource.includes("'Fetch.getResponseBody'")],
    ['catalog-probe-response-stage', probeSource.includes('responseBody(')
      && probeSource.includes('fulfillPausedResponse(')],
    ['catalog-delay', pausedSource.includes('catalogHeldState')],
    ['bootstrap-before-catalog', runSource.includes('bootstrap-before-catalog')],
    ['catalog-before-reveal', runSource.includes('catalog-before-reveal')],
    ['catalog-before-api', runSource.includes('catalog-before-api')],
    ['css-before-bootstrap', runSource.includes('css-before-bootstrap')],
    ['css-failure-request-stage', runSource.includes("'Fetch.failRequest'")
      && runSource.includes("'css-failure'")],
    ['app-asset-response-stage', appAssetSource.includes('responseBody(')
      && appAssetSource.includes('startupState.secret')
      && appAssetSource.includes('fulfillPausedResponse(')],
    ['app-asset-emergency-only', runSource.includes('app-asset-emergency-only')],
    ['emergency-only', runSource.includes('emergency-only')],
    ['no-response-body-disclosure', runSource.includes('no-response-body-disclosure')],
  ];
  for (const [name, observed] of required) {
    if (!observed) failures.push(`${name}: startup proof is missing`);
  }
  if (typeof operatorConfigStartupFailures !== 'function') {
    failures.push('operator-config-order: executable startup-order proof is missing');
  } else {
    const configOrderCases = [
      {
        name: 'valid-delayed-catalog',
        input: ['dashboard', 'delayed-catalog', ['bootstrap', 'config', 'catalog']],
        expected: [],
      },
      {
        name: 'duplicate-config',
        input: ['dashboard', 'delayed-catalog', ['bootstrap', 'config', 'config', 'catalog']],
        expected: ['operator-config-count'],
      },
      {
        name: 'config-before-bootstrap',
        input: ['dashboard', 'delayed-catalog', ['config', 'bootstrap', 'catalog']],
        expected: ['operator-config-order'],
      },
      {
        name: 'bootstrap-failure-config',
        input: ['dashboard', 'bootstrap-failure', ['bootstrap', 'config']],
        expected: ['operator-config-forbidden'],
      },
      {
        name: 'css-failure-config',
        input: ['dashboard', 'css-failure', ['config']],
        expected: ['operator-config-forbidden'],
      },
      {
        name: 'public-page-config',
        input: ['login', 'delayed-catalog', ['bootstrap', 'config', 'catalog']],
        expected: ['operator-config-forbidden'],
      },
    ];
    for (const { name, input: [page, probe, order], expected } of configOrderCases) {
      const paths = {
        bootstrap: '/api/locale-bootstrap',
        config: '/api/config',
        catalog: '/assets/operator/locales/en-US.json',
      };
      const observed = operatorConfigStartupFailures({
        isDashboard: page === 'dashboard',
        startupProbe: probe,
        requestTimeline: order.map((item, at) => ({ at, path: paths[item] })),
        catalogPath: paths.catalog,
      });
      if (JSON.stringify(observed) !== JSON.stringify(expected)) {
        failures.push(
          `operator-config-order:${name}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(observed)}`,
        );
      }
    }
  }
  if (typeof probeCatalogHtml === 'function') {
    failures.push('catalog-probe-response-stage: inline-HTML catalog mutator survives');
  }
  for (const page of ['dashboard', 'setup', 'login']) {
    const html = fs.readFileSync(path.join(ROOT, 'src', 'web', `${page}.html`), 'utf8');
    if (/id=["']i18n-catalog["']/.test(html)) {
      failures.push(`${page}-inline-catalog: inline catalog survives`);
    }
    if (!/<body\b[^>]*\bhidden(?:\s|=|>)/i.test(html)) {
      failures.push(`${page}-hidden-first-paint: body is not hidden`);
    }
    if (!/<link\b[^>]*\bid=["']presentation-stylesheet["'][^>]*>/i.test(html)) {
      failures.push(`${page}-css-before-bootstrap: stylesheet readiness is not addressable`);
    }
    const script = fs.readFileSync(
      path.join(
        ROOT,
        'src',
        'web',
        page === 'dashboard' ? 'shared.js' : `${page}.js`,
      ),
      'utf8',
    );
    if (!script.includes('presentationStylesheetReady')
        || !/if\s*\(\s*!presentationStylesheetReady\(\)\s*\)\s*return/.test(script)) {
      failures.push(`${page}-css-before-bootstrap: boot does not stop before bootstrap`);
    }
  }
  if (failures.length) {
    for (const failure of failures) console.error(`[catalog-startup] ${failure}`);
    return 1;
  }
  console.log('catalog startup selftest ok — all pages and response/request-stage probes are wired');
  return 0;
}

if (args.includes('--catalog-startup-selftest')) {
  process.exit(catalogStartupSelftest());
}

function renderTempdirs() {
  return new Set(fs.readdirSync(os.tmpdir())
    .filter(name => name.startsWith('nimproxy-render-'))
    .map(name => path.join(os.tmpdir(), name)));
}

function processDescendants(rootPid) {
  if (process.platform === 'win32') return [];
  const rows = execFileSync('ps', ['-eo', 'pid=,ppid='], { encoding: 'utf8' })
    .trim().split('\n').map(line => line.trim().split(/\s+/).map(Number));
  const found = [];
  const pending = [rootPid];
  while (pending.length) {
    const parent = pending.pop();
    for (const [pid, ppid] of rows) {
      if (ppid !== parent || found.includes(pid)) continue;
      found.push(pid);
      pending.push(pid);
    }
  }
  return found;
}

async function cleanupSelftest() {
  const realChrome = findChrome();
  if (!realChrome) {
    console.error('cleanup selftest needs Chromium');
    return 2;
  }
  const probeRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'nimproxy-cleanup-probe-'));
  const wrapper = path.join(probeRoot, 'chrome-wrapper.js');
  const pidFile = path.join(probeRoot, 'pids.json');
  const writerSource = [
    '"use strict";',
    'const fs = require("fs");',
    'const path = require("path");',
    'const profile = process.argv[1];',
    'let n = 0;',
    'setInterval(() => {',
    '  try {',
    '    fs.mkdirSync(profile, { recursive: true });',
    '    fs.writeFileSync(path.join(profile, "cleanup-race-" + (n++ % 8)), "x");',
    '  } catch (_) {}',
    '}, 5);',
  ].join('\n');
  const wrapperSource = [
    '#!/usr/bin/env node',
    '"use strict";',
    'const fs = require("fs");',
    'const { spawn } = require("child_process");',
    'const args = process.argv.slice(2);',
    'const profileArg = args.find(value => value.startsWith("--user-data-dir="));',
    'const profile = profileArg.slice("--user-data-dir=".length);',
    'const chrome = spawn(process.env.NIMPROXY_REAL_CHROME, args,',
    '  { stdio: ["ignore", "pipe", "pipe"] });',
    'chrome.stdout.pipe(process.stdout);',
    'chrome.stderr.pipe(process.stderr);',
    `const writer = spawn(process.execPath, ["-e", ${JSON.stringify(writerSource)}, profile],`,
    '  { stdio: "ignore" });',
    'fs.writeFileSync(process.env.NIMPROXY_CLEANUP_PID_FILE,',
    '  JSON.stringify({ chrome: chrome.pid, writer: writer.pid }));',
    'chrome.on("exit", code => { process.exitCode = code || 0; });',
  ].join('\n');
  fs.writeFileSync(wrapper, wrapperSource, { mode: 0o700 });

  const before = renderTempdirs();
  let stdout = '';
  let stderr = '';
  const child = spawn(process.execPath, [__filename, '--page', 'setup'], {
    cwd: ROOT,
    env: {
      ...process.env,
      CHROME: wrapper,
      NIMPROXY_CLEANUP_PID_FILE: pidFile,
      NIMPROXY_REAL_CHROME: realChrome,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout.on('data', chunk => { stdout += chunk.toString(); });
  child.stderr.on('data', chunk => { stderr += chunk.toString(); });
  const result = await Promise.race([
    new Promise(resolve => child.once('exit', code => resolve({ code, timedOut: false }))),
    sleep(30000).then(() => ({ code: null, timedOut: true })),
  ]);
  if (result.timedOut) child.kill('SIGKILL');
  await sleep(300);
  const leaked = [...renderTempdirs()].filter(dir => !before.has(dir));

  let pids = [];
  try {
    const recorded = JSON.parse(fs.readFileSync(pidFile, 'utf8'));
    pids = [recorded.chrome, recorded.writer].filter(Number.isInteger);
  } catch (_) {}
  const descendants = pids.flatMap(processDescendants);
  for (const pid of [...new Set([...descendants.reverse(), ...pids])]) {
    try { process.kill(pid, 'SIGKILL'); } catch (_) {}
  }
  await sleep(300);
  for (const dir of leaked) {
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }

  const failures = [];
  if (result.timedOut) failures.push('render process stayed alive after reporting its result');
  else if (result.code !== 0) failures.push(`render process exited ${result.code}`);
  if (leaked.length) failures.push(`left ${leaked.length} nimproxy-render temp director${leaked.length === 1 ? 'y' : 'ies'}`);
  if (/note: left .*nimproxy-render-/.test(stderr)) failures.push('cleanup failure was downgraded to a note');

  const startupMarker = path.join(probeRoot, 'proxy-exit.json');
  const startupBefore = renderTempdirs();
  let startupStderr = '';
  const startup = spawn(process.execPath, [__filename, '--page', 'setup'], {
    cwd: ROOT,
    env: {
      ...process.env,
      CHROME: realChrome,
      NIMPROXY_RENDER_HEALTH_PATH: '/cleanup-selftest-never-ready',
      NIMPROXY_RENDER_HEALTH_TIMEOUT_MS: '200',
      NIMPROXY_RENDER_PROXY_EXIT_MARKER: startupMarker,
    },
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  startup.stderr.on('data', chunk => { startupStderr += chunk.toString(); });
  const startupResult = await Promise.race([
    new Promise(resolve => startup.once('exit', code => resolve({ code, timedOut: false }))),
    sleep(10000).then(() => ({ code: null, timedOut: true })),
  ]);
  if (startupResult.timedOut) startup.kill('SIGKILL');
  await sleep(100);
  const startupLeaked = [...renderTempdirs()].filter(dir => !startupBefore.has(dir));
  let startupExit = null;
  try {
    startupExit = JSON.parse(fs.readFileSync(startupMarker, 'utf8'));
  } catch (_) {}
  if (startupResult.timedOut) failures.push('startup-timeout render process stayed alive');
  else if (startupResult.code !== 2) failures.push(`startup-timeout render process exited ${startupResult.code}`);
  if (!startupStderr.includes('proxy did not become healthy')) {
    failures.push('startup-timeout did not report the health failure');
  }
  if (!startupExit?.runDirectoryExisted) {
    failures.push('startup-timeout removed the run directory before the proxy exit was observed');
  }
  if (startupLeaked.length) {
    failures.push(`startup-timeout left ${startupLeaked.length} nimproxy-render temp director${startupLeaked.length === 1 ? 'y' : 'ies'}`);
  }

  const localeBefore = renderTempdirs();
  let localeStderr = '';
  const locale = spawn(
    process.execPath,
    [__filename, '--locale', 'cleanup-selftest-missing'],
    {
      cwd: ROOT,
      env: { ...process.env, CHROME: realChrome },
      stdio: ['ignore', 'ignore', 'pipe'],
    },
  );
  locale.stderr.on('data', chunk => { localeStderr += chunk.toString(); });
  const localeResult = await Promise.race([
    new Promise(resolve => locale.once('exit', code => resolve({ code, timedOut: false }))),
    sleep(30000).then(() => ({ code: null, timedOut: true })),
  ]);
  if (localeResult.timedOut) locale.kill('SIGKILL');
  await sleep(100);
  const localeLeaked = [...renderTempdirs()].filter(dir => !localeBefore.has(dir));
  if (localeResult.timedOut) failures.push('missing-locale render process stayed alive');
  else if (localeResult.code !== 1) failures.push(`missing-locale render process exited ${localeResult.code}`);
  if (!localeStderr.includes('missing locale locales/cleanup-selftest-missing.json')) {
    failures.push('missing-locale failure hid its originating diagnostic');
  }
  if (localeLeaked.length) {
    failures.push(`missing-locale left ${localeLeaked.length} nimproxy-render temp director${localeLeaked.length === 1 ? 'y' : 'ies'}`);
  }

  for (const dir of [...startupLeaked, ...localeLeaked]) {
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
  fs.rmSync(probeRoot, { recursive: true, force: true });

  if (failures.length) {
    for (const failure of failures) console.error(`[cleanup] ${failure}`);
    if (stderr.trim()) console.error(stderr.trim());
    return 1;
  }
  console.log('cleanup selftest ok — browser/proxy lifecycle and failure diagnostics passed');
  return 0;
}
// Which embedded page to drive. The dashboard renders from Rust-owned
// fixtures; the wizard is driven by filling and clicking.
// Both were being proved by hand-built one-off harnesses, which is more work
// than one committed check and leaves nothing behind.
const pageArg = (() => {
  const i = args.indexOf('--page');
  return i >= 0 ? args[i + 1] : 'dashboard';
})();
if (!['dashboard', 'setup', 'login'].includes(pageArg)) {
  console.error(`unknown --page ${pageArg} (expected: dashboard, setup, login)`);
  process.exit(2);
}
const IS_SETUP = pageArg === 'setup';
const IS_LOGIN = pageArg === 'login';
const IS_DASHBOARD = pageArg === 'dashboard';
const allStates = args.includes('--all-states');
const visualMatrix = args.includes('--visual-matrix');
const semanticSelftest = args.includes('--semantic-selftest');
const layoutReport = args.includes('--layout-report');
const layoutSelftest = args.includes('--layout-selftest');
const layoutStateIndex = args.indexOf('--layout-state');
const layoutState = layoutStateIndex >= 0 ? args[layoutStateIndex + 1] : 'healthy';
const LAYOUT_RANGE_FIXTURES = {
  healthy: 'dashboard-healthy.json',
  long: 'dashboard-long.json',
  extreme: 'dashboard-extreme.json',
  incomplete: 'dashboard-partial.json',
  error: 'api-error.json',
};
if (layoutReport && IS_DASHBOARD && !LAYOUT_RANGE_FIXTURES[layoutState]) {
  throw new Error(`unknown --layout-state ${JSON.stringify(layoutState)}`);
}
const PAGE_REL = path.join('src', 'web', `${pageArg}.html`);
const localeArg = (() => {
  const i = args.indexOf('--locale');
  if (i >= 0) return args[i + 1];
  return args.includes('--pseudolocale') ? 'en-XA' : null;
})();
// Catalog values are plain Unicode text. The probe proves ordinary text and
// allowed attributes render literally, while fixed-markup HTML builders
// perform their one context-appropriate escape at the sink.
const escapeProbe = args.includes('--escape-probe');
const startupProbe = (() => {
  const i = args.indexOf('--startup-probe');
  return i >= 0 ? args[i + 1] : null;
})();
const precedenceCase = (() => {
  const i = args.indexOf('--precedence-case');
  return i >= 0 ? args[i + 1] : 'override';
})();
const STARTUP_PROBES = new Set([
  'app-script-failure',
  'delayed-catalog',
  'bootstrap-failure',
  'catalog-failure',
  'css-failure',
  'locale-precedence',
  'malformed-catalog',
]);
if (startupProbe && !STARTUP_PROBES.has(startupProbe)) {
  console.error(`unknown --startup-probe ${startupProbe}`);
  process.exit(2);
}
if (startupProbe === 'app-script-failure' && !IS_DASHBOARD) {
  console.error('app-script-failure is a dashboard-only startup probe');
  process.exit(2);
}
if (!['override', 'default'].includes(precedenceCase)) {
  console.error(`unknown --precedence-case ${precedenceCase} (expected: override, default)`);
  process.exit(2);
}
if (args.includes('--precedence-case') && startupProbe !== 'locale-precedence') {
  console.error('--precedence-case requires --startup-probe locale-precedence');
  process.exit(2);
}
// Appended to every catalog value under --escape-probe. See the mutation below.
const PROBE_MARKER = 'Ampersand';
// The `"` is not decoration. An unescaped value interpolated into a quoted
// attribute inside an innerHTML string breaks OUT of the attribute, and the
// parser then reads the rest as further attribute names — so a `<b` or `"`
// shows up as an ATTRIBUTE NAME, which is unambiguous. Scanning attribute
// VALUES cannot work: getAttribute() decodes entities, so a correctly-escaped
// value and an unescaped one are byte-identical by the time the DOM has them.
const PROBE_SUFFIX = ' Ampersand & Quote\' <b>Tag</b> DQ"';
const PROBE_TAG_TEXT = 'Tag';

/* ---------- locate a browser ---------------------------------------------- */

function findChrome() {
  // CHROME is a hint, not a promise: if it does not exist, keep looking rather
  // than spawning a path that is not there.
  const candidates = [
    process.env.CHROME,
    '/opt/pw-browsers/chromium/chrome-linux/chrome',
    ...expandGlob('/opt/pw-browsers/chromium-*/chrome-linux/chrome'),
    ...expandGlob('/opt/pw-browsers/chromium_headless_shell-*/chrome-linux/headless_shell'),
    '/usr/bin/google-chrome-stable',
    '/usr/bin/google-chrome',
    '/usr/bin/chromium-browser',
    '/usr/bin/chromium',
  ];
  for (const c of candidates) if (c && fs.existsSync(c)) return c;
  return null;
}

function expandGlob(pattern) {
  const dir = path.dirname(path.dirname(path.dirname(pattern)));
  const rest = pattern.slice(dir.length + 1).split('/');
  if (!fs.existsSync(dir)) return [];
  return fs
    .readdirSync(dir)
    .filter((n) => new RegExp('^' + rest[0].replace(/\*/g, '.*') + '$').test(n))
    .map((n) => path.join(dir, n, ...rest.slice(1)));
}

/* ---------- minimal CDP client over Node's built-in WebSocket -------------- */

class CDP {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.listeners = [];
    ws.addEventListener('message', (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result);
      } else if (msg.method) {
        for (const fn of this.listeners) fn(msg);
      }
    });
  }

  static connect(url) {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url);
      ws.addEventListener('open', () => resolve(new CDP(ws)));
      ws.addEventListener('error', (e) => reject(new Error('ws: ' + e.message)));
    });
  }

  on(fn) {
    this.listeners.push(fn);
    return () => {
      const index = this.listeners.indexOf(fn);
      if (index >= 0) this.listeners.splice(index, 1);
    };
  }

  send(method, params = {}, sessionId) {
    const id = ++this.id;
    const payload = { id, method, params };
    if (sessionId) payload.sessionId = sessionId;
    this.ws.send(JSON.stringify(payload));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }
}

let LINE_OFFSET = 0;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function evaluateRaw(browser, sessionId, expression) {
  const r = await browser.send(
    'Runtime.evaluate',
    { expression, returnByValue: true, awaitPromise: true },
    sessionId,
  );
  if (r.exceptionDetails) throw new Error('evaluate threw: ' + r.exceptionDetails.text);
  return r.result.value;
}

/* ---------- production server and captured API responses ------------------ */

function loadFixtures() {
  const dashboardFixture = LAYOUT_RANGE_FIXTURES[layoutState] || 'dashboard-healthy.json';
  const need = IS_DASHBOARD
    ? ['config-superuser.json', dashboardFixture, 'dashboard-now-initial.json']
    : IS_SETUP
      ? ['setup-minted-client-key.json', 'validate-success.json']
      : [];
  for (const f of need) {
    const p = path.join(FIXTURES, f);
    if (!fs.existsSync(p)) {
      const error = new Error(
        `missing fixture ${path.relative(ROOT, p)} — regenerate with UPDATE_UI_FIXTURES=1`,
      );
      error.exitCode = 2;
      throw error;
    }
  }
  return Object.fromEntries(
    need.map((f) => [f, JSON.parse(fs.readFileSync(path.join(FIXTURES, f), 'utf8'))]),
  );
}

function loadTestLocaleCatalog(locale) {
  if (locale !== 'en-XA') {
    const error = new Error(`missing locale locales/${locale}.json`);
    error.exitCode = 2;
    throw error;
  }
  return JSON.parse(execFileSync(
    'python3',
    [path.join(ROOT, 'scripts', 'gen_pseudolocale.py'), '--stdout'],
    { cwd: ROOT, encoding: 'utf8' },
  ));
}

function publicCatalogProjection(catalog) {
  return {
    locale: catalog.locale,
    messages: Object.fromEntries(
      Object.entries(catalog.messages).filter(([id]) =>
        id === 'common.app_name'
        || id.startsWith('login.')
        || id.startsWith('setup.')),
    ),
  };
}

async function responseBody(browser, sessionId, requestId) {
  const response = await browser.send(
    'Fetch.getResponseBody',
    { requestId },
    sessionId,
  );
  return response.base64Encoded
    ? Buffer.from(response.body, 'base64').toString('utf8')
    : response.body;
}

async function fulfillPausedResponse(browser, sessionId, params, body, statusCode) {
  const headers = (params.responseHeaders || []).filter(header =>
    !['content-length', 'content-encoding', 'transfer-encoding']
      .includes(header.name.toLowerCase()));
  await browser.send('Fetch.fulfillRequest', {
    requestId: params.requestId,
    responseCode: statusCode ?? params.responseStatusCode,
    responseHeaders: headers,
    body: Buffer.from(body).toString('base64'),
  }, sessionId);
}

function loadUiFixture(reference, consumed) {
  const [file, key] = reference.split('#');
  const fixturePath = path.join(FIXTURES, file);
  const manifest = generatedFixtureManifest();
  if (!manifest || !validFixtureReference(reference, manifest)) {
    throw new Error(`invalid UI fixture reference ${reference}`);
  }
  let value = JSON.parse(fs.readFileSync(fixturePath, 'utf8'));
  if (key) value = value[key];
  if (value === undefined) throw new Error(`missing UI fixture value ${reference}`);
  if (consumed) consumed.push(reference);
  return JSON.parse(JSON.stringify(value));
}

function selectedFixtureReference(value, phase, role) {
  if (typeof value === 'string' || value == null) return value;
  if (Array.isArray(value)) return value;
  if (value[role]) return value[role];
  if (phase === 'action') return value.action ?? value.after ?? value.before;
  return value.before ?? value.action ?? value.after;
}

class UiFixturePlan {
  constructor(row, role) {
    this.row = row;
    this.recipe = row.fixtures;
    this.role = role;
    this.phase = 'before';
    this.consumed = [];
    this.responseReferences = this.recipe.response == null
      ? []
      : Array.isArray(this.recipe.response)
        ? [...this.recipe.response]
        : [this.recipe.response];
    this.responseIndex = 0;
  }

  enterAction() {
    this.phase = 'action';
  }

  value(reference) {
    if (!reference) return null;
    this.lastReference = reference;
    return loadUiFixture(reference, this.consumed);
  }

  locale() {
    return this.value(this.recipe.locale);
  }

  config() {
    return this.value(selectedFixtureReference(this.recipe.config, this.phase, this.role));
  }

  now() {
    return this.value(selectedFixtureReference(this.recipe.now, this.phase, this.role));
  }

  range() {
    const reference = this.phase === 'action' && this.recipe.response
      ? this.nextResponseReference()
      : selectedFixtureReference(this.recipe.range, this.phase, this.role);
    return this.value(reference);
  }

  response() {
    return this.value(this.nextResponseReference());
  }

  nextResponseReference() {
    const reference = this.responseReferences[this.responseIndex];
    if (!reference) throw new Error(`${this.row.id}: action consumed too many response fixtures`);
    this.responseIndex += 1;
    return reference;
  }

  unconsumed() {
    return this.responseReferences.slice(this.responseIndex);
  }
}

async function handleCatalogResponseForProbe({
  browser,
  sessionId,
  params,
  pathname,
  catalogPath,
  serverCatalogResponses,
}) {
  const originalBody = await responseBody(browser, sessionId, params.requestId);
  let body = originalBody;
  let statusCode = params.responseStatusCode;
  if (pathname === '/api/locale-bootstrap') {
    const bootstrap = JSON.parse(body);
    bootstrap.installed_locales = [localeArg];
    bootstrap.server_default = localeArg;
    body = JSON.stringify(bootstrap);
  } else if (pathname === catalogPath) {
    let catalog = localeArg
      ? loadTestLocaleCatalog(localeArg)
      : JSON.parse(body);
    if (!IS_DASHBOARD) catalog = publicCatalogProjection(catalog);
    if (escapeProbe) {
      for (const id of Object.keys(catalog.messages)) {
        catalog.messages[id] += PROBE_SUFFIX;
      }
    }
    body = JSON.stringify(catalog);
    if (localeArg) statusCode = 200;
    serverCatalogResponses.add(pathname);
  }
  await fulfillPausedResponse(browser, sessionId, params, body, statusCode);
}

async function handleCatalogResponseForStartupProbe({
  browser,
  sessionId,
  params,
  pathname,
  catalogPath,
  startupState,
}) {
  const originalBody = await responseBody(browser, sessionId, params.requestId);
  let body = originalBody;
  let statusCode = params.responseStatusCode;
  if (pathname === '/api/locale-bootstrap'
      && startupProbe === 'locale-precedence') {
    startupState.bootstrapOriginal = JSON.parse(originalBody);
    const bootstrap = JSON.parse(originalBody);
    bootstrap.installed_locales = ['en-US', 'de-DE', 'fr-FR'];
    bootstrap.server_default = 'fr-FR';
    body = JSON.stringify(bootstrap);
    startupState.bootstrapFromServer = true;
  }
  if (pathname === '/api/locale-bootstrap'
      && startupProbe === 'bootstrap-failure') {
    statusCode = 503;
    body = startupState.secret;
    startupState.failureReleasedAt = Date.now();
  }
  if (pathname === catalogPath && startupProbe === 'delayed-catalog') {
    await sleep(400);
    startupState.catalogHeldState = await evaluateRaw(browser, sessionId, `JSON.stringify({
      hidden: !document.body || document.body.hidden,
      text: document.body ? document.body.innerText.trim() : '',
    })`).then(JSON.parse);
    const catalog = JSON.parse(body);
    const probeId = IS_DASHBOARD
      ? 'dashboard.nav.tab.overview'
      : IS_SETUP
        ? 'setup.wizard.continue'
        : 'login.submit';
    startupState.readyText = `${catalog.messages[probeId]} · catalog response`;
    catalog.messages[probeId] = startupState.readyText;
    body = JSON.stringify(catalog);
  }
  if (pathname === catalogPath && startupProbe === 'locale-precedence') {
    const authoringCatalog = JSON.parse(fs.readFileSync(
      path.join(ROOT, 'src', 'web', 'locales', 'en-US.json'),
      'utf8',
    ));
    const catalog = {
      locale: startupState.expectedLocale,
      messages: Object.fromEntries(
        Object.entries(authoringCatalog.messages)
          .map(([id, message]) => [id, message.en]),
      ),
    };
    if (Object.keys(catalog).sort().join(',') !== 'locale,messages'
        || Object.values(catalog.messages)
          .some(message => typeof message !== 'string')) {
      throw new Error('locale-precedence: synthetic response is not a valid wire catalog');
    }
    startupState.readyText = precedenceCase === 'override'
      ? 'Override locale de-DE selected'
      : 'Server locale fr-FR selected';
    catalog.messages['dashboard.nav.tab.overview'] = startupState.readyText;
    body = JSON.stringify(catalog);
    statusCode = 200;
    startupState.selectedCatalogPath = pathname;
  }
  if (pathname === catalogPath && startupProbe === 'catalog-failure') {
    statusCode = 503;
    body = startupState.secret;
    startupState.failureReleasedAt = Date.now();
  }
  if (pathname === catalogPath && startupProbe === 'malformed-catalog') {
    body = '{"locale":"en-US","messages":[]}';
    startupState.disclosureMarkers.add(body);
    startupState.failureReleasedAt = Date.now();
  }
  if (pathname === catalogPath) startupState.catalogReleasedAt = Date.now();
  await fulfillPausedResponse(browser, sessionId, params, body, statusCode);
}

async function handleConfigResponseForLocalePrecedence({
  browser,
  sessionId,
  params,
  startupState,
}) {
  const originalBody = await responseBody(browser, sessionId, params.requestId);
  startupState.configOriginal = JSON.parse(originalBody);
  const config = JSON.parse(originalBody);
  config.locale = precedenceCase === 'override' ? 'de-DE' : null;
  startupState.configFromServer = true;
  await fulfillPausedResponse(
    browser,
    sessionId,
    params,
    JSON.stringify(config),
    params.responseStatusCode,
  );
}

async function handleApplicationScriptResponseForStartupProbe({
  browser,
  sessionId,
  params,
  startupState,
}) {
  await responseBody(browser, sessionId, params.requestId);
  startupState.failureReleasedAt = Date.now();
  await fulfillPausedResponse(
    browser,
    sessionId,
    params,
    startupState.secret,
    503,
  );
}

function configuredStore() {
  const rootUser = {
    username: 'root',
    password_hash: 'pbkdf2-sha256$1000$00000000000000000000000000000000$dd5fe0be04ca7f9e24642561a5d4635c52c40be82cbd7587b5eddc913ad3c7a7',
    role: 'superuser',
  };
  if (!(startupProbe === 'locale-precedence' && precedenceCase === 'default')) {
    rootUser.locale = 'en-US';
  }
  return {
    version: 1,
    default_locale: 'en-US',
    upstream: {
      base_url: 'http://127.0.0.1:9',
      nim_keys: [{ key: 'render-fixture-key', owner: 'root', enabled: true, rpm: 40 }],
    },
    client_auth: { mode: 'open', keys: [] },
    limits: { heartbeat_secs: 1 },
    users: [rootUser],
  };
}

async function freePort() {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const port = server.address().port;
      server.close((error) => error ? reject(error) : resolve(port));
    });
  });
}

async function startProxy(tmpdir, setupRequired = IS_SETUP) {
  const binary = path.join(ROOT, 'target', 'debug', 'nim-proxy');
  execFileSync('cargo', ['build', '--quiet', '--bin', 'nim-proxy'], {
    cwd: ROOT,
    stdio: 'inherit',
  });
  const dataDir = path.join(tmpdir, 'data');
  fs.mkdirSync(dataDir);
  if (!setupRequired) {
    fs.writeFileSync(
      path.join(dataDir, 'config.json'),
      JSON.stringify(configuredStore(), null, 2),
    );
  }
  const port = await freePort();
  const proc = spawn(binary, [], {
    cwd: tmpdir,
    env: {
      DATA_DIR: dataDir,
      HISTORY_SAMPLE_SECS: '3600',
      HOST: '127.0.0.1',
      PORT: String(port),
      RUST_LOG: 'nim_proxy=warn',
    },
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  if (process.env.NIMPROXY_RENDER_PROXY_EXIT_MARKER) {
    proc.once('exit', () => {
      fs.writeFileSync(
        process.env.NIMPROXY_RENDER_PROXY_EXIT_MARKER,
        JSON.stringify({
          pid: proc.pid,
          runDirectoryExisted: fs.existsSync(tmpdir),
        }),
      );
    });
  }
  const origin = `http://127.0.0.1:${port}`;
  let stderr = '';
  proc.stderr.on('data', chunk => { stderr += chunk.toString(); });
  const healthPath = process.env.NIMPROXY_RENDER_HEALTH_PATH || '/health';
  const healthTimeoutMs = Number(process.env.NIMPROXY_RENDER_HEALTH_TIMEOUT_MS || 20000);
  const deadline = Date.now() + healthTimeoutMs;
  while (Date.now() < deadline) {
    if (proc.exitCode !== null) throw new Error(`proxy exited early: ${proc.exitCode}\n${stderr}`);
    try {
      const response = await fetch(origin + healthPath);
      if (response.ok) return { proc, origin };
    } catch (_) {}
    await sleep(50);
  }
  if (!await stopChild(proc, 'SIGKILL')) {
    throw new Error(`proxy did not become healthy and did not exit\n${stderr}`);
  }
  throw new Error(`proxy did not become healthy\n${stderr}`);
}

/* ---------- the run -------------------------------------------------------- */


const isGeneratedFrozenP95 = text =>
  /^\[p95 (?:–|(?:0|[1-9]\d{0,2}(?:,\d{3})*)\.\d sec|(?:0|[1-9]\d{0,2}(?:,\d{3})*) (?:ms|min)) e\]$/.test(text);
const isIntlDurationRun = text =>
  /^(?:(?:0|[1-9]\d{0,2}(?:,\d{3})*) ms|(?:0|[1-9]\d{0,2}(?:,\d{3})*)\.\d sec|(?:0|[1-9]\d{0,2}(?:,\d{3})*) min)$/.test(text);

// A run of ASCII prose with no accented character, under a locale where every
// translated message is accented, is a string the catalog never reached.
const SCAN_UNTRANSLATED = `
  (() => {
    const out = [];
    const pseudoPad = 'escamotable';
    const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let n = walk.nextNode(); n; n = walk.nextNode()) {
      const p = n.parentNode.nodeName;
      if (p === 'SCRIPT' || p === 'STYLE') continue;
      const t = n.textContent.trim();
      if (t.length < 3 || !/[a-zA-Z]{3}/.test(t)) continue;
      const padding = t.endsWith(']') ? t.slice(0, -1) : t;
      if (padding && [...padding].every((ch, i) => ch === pseudoPad[i % pseudoPad.length]))
        continue;
      if (${isGeneratedFrozenP95.toString()}(t)) continue;
      if (/[\\u00C0-\\u024F]/.test(t)) continue;   // accented: the catalog reached it
      out.push(t);
    }
    return Array.from(new Set(out));
  })()`;

// Values that originate in the API response, computed by the page's own
// helpers so this cannot drift from what it actually renders.
const dataLabelValues = (() => {
  // Data means values that arrived in the payload — nothing else. Scraping
  // rendered elements misclassifies our own generated names: "Slot 1" comes
  // from a template we wrote, not from the API.
  const out = new Set();
  for (const f of ['dashboard-healthy.json', 'dashboard-now-initial.json']) {
    const p = path.join(FIXTURES, f);
    if (!fs.existsSync(p)) continue;
    const doc = JSON.parse(fs.readFileSync(p, 'utf8'));
    for (const bucket of ['totals', 'latest']) {
      for (const r of doc[bucket] || []) {
        for (const k of ['model', 'client', 'mode']) {
          const v = (r.labels || {})[k];
          if (v && v !== 'none') out.add(v);
        }
      }
    }
  }
  return [...out];
})();

// Expand each payload value through the page's own helpers, so the exclusion
// set matches what it actually renders rather than what I assume it renders.
const SCAN_DATA_DERIVED = `
  (() => {
    const ids = ${JSON.stringify(dataLabelValues)};
    const out = new Set(ids);
    for (const id of ids) {
      try { out.add(prettyName(id)); } catch (e) {}
      try { out.add(publisher(id).name); } catch (e) {}
    }
    // Anything Intl produces is CLDR data, not catalog text. Ask the page's
    // own cached formatters rather than accepting a text-shaped heuristic that
    // could hide an owned label such as "Key 12".
    try { for (const d of DAYS) out.add(d); } catch (e) {}
    try {
      for (let month = 0; month < 12; month++) for (let day = 1; day <= 31; day++) {
        const ms = Date.UTC(2024, month, day);
        if (new Date(ms).getUTCMonth() === month) out.add(DAY_SHORT.format(ms));
      }
    } catch (e) {}
    try {
      for (let h = 0; h < 24; h++) {
        out.add(HOUR_ONLY.format(Date.UTC(2024, 0, 1, h)));
        out.add(HOUR_ONLY.formatRange(Date.UTC(2024, 0, 1, h), Date.UTC(2024, 0, 1, h + 1)));
      }
    } catch (e) {}
    return [...out].filter(Boolean);
  })()`;

const DASH_TABS = ['overview', 'models', 'clients', 'reliability', 'capacity'];
/* The wizard has no tabs and no payloads: it is a form, so it has to be filled
   and clicked. Each step returns true once the panel it should have revealed is
   visible, so a step that silently does nothing fails loudly instead of letting
   the scan measure step 1 four times — the same mistake the dashboard's
   hash-vs-click bug caused. `errors` deliberately trips the validation paths,
   which is where eight of the wizard's messages live. */
const SETUP_STEPS = {
  errors: `(() => {
    $('username').value = 'op'; $('password').value = 'short';
    $('confirm').value = 'mismatch'; $('to2').click();
    const shown = ($('err').textContent || '').trim();
    return shown.length > 0 && !$('err').hidden;
  })()`,
  step1: `(() => {
    $('username').value = 'fixture-user'; $('password').value = 'probe-password-1';
    $('confirm').value = 'probe-password-1'; $('to2').click();
    return !$('step2').hidden;
  })()`,
  step2: `(async () => {
    $('newkey').value = 'nvapi-probe-key';
    $('addkey').click();
    for (let i = 0; i < 40 && $('to3').disabled; i++) await new Promise(r => setTimeout(r, 50));
    return !$('to3').disabled && !!$('keylist').querySelector('li');
  })()`,
  step3: `(() => {
    $('to3').click();
    const reviewed = ($('review').textContent || '').length > 0;
    // Toggle both ways: apiAccessMessageId() has two branches and both feed the
    // ID-taking text sink. Leave it checked so step4 reaches showConnect
    // rather than navigating to /.
    $('mintkey').checked = false;
    $('mintkey').dispatchEvent(new Event('change'));
    $('mintkey').checked = true;
    $('mintkey').dispatchEvent(new Event('change'));
    return !$('step3').hidden && reviewed;
  })()`,
  step4: `(async () => {
    $('finish').click();
    for (let i = 0; i < 60 && $('step4').hidden; i++) await new Promise(r => setTimeout(r, 50));
    return !$('step4').hidden;
  })()`,
};
const TABS = IS_SETUP ? Object.keys(SETUP_STEPS) : DASH_TABS;

// The never-translate list has exactly one definition, in check_i18n.py. A JS
// copy would drift, and this list decides which "untranslated" runs are
// actually correct.
function frozenTokens() {
  const out = execFileSync('python3', ['-c',
    'import sys,json; sys.path.insert(0,"scripts"); import check_i18n; '
    + 'print(json.dumps(sorted(check_i18n.NEVER_TRANSLATE)))'],
    { cwd: ROOT, encoding: 'utf8' });
  return JSON.parse(out);
}

function waitForProcessExit(proc, timeoutMs) {
  if (!proc || proc.exitCode !== null || proc.signalCode !== null) return Promise.resolve(true);
  return new Promise(resolve => {
    let settled = false;
    const finish = value => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      proc.removeListener('exit', onExit);
      resolve(value);
    };
    const onExit = () => finish(true);
    const timer = setTimeout(() => finish(false), timeoutMs);
    proc.once('exit', onExit);
    if (proc.exitCode !== null || proc.signalCode !== null) finish(true);
  });
}

async function stopChild(proc, firstSignal = 'SIGTERM') {
  if (!proc || proc.exitCode !== null || proc.signalCode !== null) return true;
  proc.kill(firstSignal);
  if (await waitForProcessExit(proc, 2000)) return true;
  if (firstSignal !== 'SIGKILL') proc.kill('SIGKILL');
  return firstSignal === 'SIGKILL' ? false : await waitForProcessExit(proc, 2000);
}

function browserGroupAlive(pid) {
  if (!pid || process.platform === 'win32') return false;
  try {
    process.kill(-pid, 0);
    return true;
  } catch (error) {
    return error.code !== 'ESRCH';
  }
}

async function waitForBrowserGroupExit(pid, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (browserGroupAlive(pid) && Date.now() < deadline) await sleep(25);
  return !browserGroupAlive(pid);
}

async function shutdownRun(browser, browserProc, proxyProc, tmpdir) {
  const failures = [];
  if (browser) {
    try {
      await Promise.race([
        browser.send('Browser.close'),
        sleep(2000).then(() => { throw new Error('Browser.close timed out'); }),
      ]);
    } catch (_) {
      // The socket commonly closes before CDP can acknowledge Browser.close.
      // Process and group exit below are the authoritative result.
    }
  }

  const browserExited = await waitForProcessExit(browserProc, 2000);
  let browserTreeExited = process.platform === 'win32'
    ? browserExited
    : await waitForBrowserGroupExit(browserProc?.pid, browserExited ? 250 : 0);
  if (!browserExited || !browserTreeExited) {
    try {
      if (process.platform === 'win32') browserProc?.kill('SIGKILL');
      else if (browserProc?.pid) process.kill(-browserProc.pid, 'SIGKILL');
    } catch (error) {
      if (error.code !== 'ESRCH') failures.push(`browser termination: ${error.message}`);
    }
    const forcedParentExit = await waitForProcessExit(browserProc, 2000);
    browserTreeExited = process.platform === 'win32'
      ? forcedParentExit
      : await waitForBrowserGroupExit(browserProc?.pid, 2000);
    if (!forcedParentExit || !browserTreeExited) {
      failures.push('browser process tree did not exit');
    }
  }
  try { browser?.ws.close(); } catch (_) {}

  if (!await stopChild(proxyProc)) {
    failures.push('proxy process did not exit');
  }

  if (tmpdir) {
    try {
      fs.rmSync(tmpdir, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
    } catch (error) {
      failures.push(`remove ${tmpdir}: ${error.code || error.message}`);
    }
    if (fs.existsSync(tmpdir)) failures.push(`run directory still exists: ${tmpdir}`);
  }
  if (failures.length) throw new Error(`render cleanup failed: ${failures.join('; ')}`);
}

function reportedFailure() {
  const error = new Error('render proof failed');
  error.reported = true;
  error.exitCode = 1;
  return error;
}

async function waitForMatrixCondition(browser, sessionId, expression, label, timeoutMs = 10000) {
  const deadline = Date.now() + timeoutMs;
  let last;
  while (Date.now() < deadline) {
    try {
      last = await evaluateRaw(browser, sessionId, expression);
      if (last) return;
    } catch (error) {
      last = error.message;
    }
    await sleep(25);
  }
  throw new Error(`${label} timed out (last=${canonicalJson(last)})`);
}

function normalizedPausedRequest(request) {
  const url = new URL(request.url);
  const query = {};
  for (const [key, value] of url.searchParams) {
    if (Object.prototype.hasOwnProperty.call(query, key)) {
      throw new Error(`duplicate query key ${key} in ${url.pathname}`);
    }
    query[key] = value;
  }
  let body = null;
  if (request.postData) {
    try { body = JSON.parse(request.postData); }
    catch { body = request.postData; }
  }
  return { method: request.method, path: url.pathname, query, body };
}

function isExpectedMatrixApiResourceError(message) {
  return /^Failed to load resource: the server responded with a status of [45]\d\d/.test(message);
}

const MATRIX_TIMER_SHIM = `
  (() => {
    const intervals = [];
    window.__matrixIntervals = intervals;
    window.setInterval = (fn, delay, ...args) => {
      const entry = { fn, delay: Number(delay), args, active: true };
      intervals.push(entry);
      return 100000 + intervals.length;
    };
    window.clearInterval = id => {
      const entry = intervals[id - 100001];
      if (entry) entry.active = false;
    };
    window.__runMatrixIntervals = async delay => {
      for (const entry of intervals) {
        if (entry.active && entry.delay === delay) await entry.fn(...entry.args);
      }
    };
    window.__matrixPageErrors = [];
    window.__matrixPromiseRejections = [];
    addEventListener('error', event => {
      window.__matrixPageErrors.push(String(event.error?.stack || event.message));
    });
    addEventListener('unhandledrejection', event => {
      window.__matrixPromiseRejections.push(String(event.reason?.stack || event.reason));
    });
  })();
`;

function matrixActionExpression(id) {
  const openSettings = `document.querySelector('#side [data-tab="settings"]').click()`;
  const setValue = (selector, value) =>
    `document.querySelector(${JSON.stringify(selector)}).value=${JSON.stringify(value)}`;
  const click = selector => `document.querySelector(${JSON.stringify(selector)}).click()`;
  const expressions = {
    'navigation-settings-role-panels': openSettings,
    'sorting-table-ascending': `${click('#side [data-tab="models"]')};${click('#table-models thead th[data-i="0"]')}`,
    'sorting-table-descending': `${click('#table-models thead th[data-i="0"]')}`,
    'sorting-table-persists-rerender': `await pollNow()`,
    'filtering-settings-role': openSettings,
    'refresh-dashboard-now': `await pollNow()`,
    'refresh-settings-access': `document.querySelector('#nk-key').focus();await window.__runMatrixIntervals(5000)`,
    'history-window-preset': click('#ranges [data-range="3600"]'),
    'history-window-custom': `(() => {
      const local = seconds => {
        const d = new Date(seconds * 1000), p = n => String(n).padStart(2, '0');
        return d.getFullYear()+'-'+p(d.getMonth()+1)+'-'+p(d.getDate())+'T'+p(d.getHours())+':'+p(d.getMinutes())+':'+p(d.getSeconds());
      };
      ${click('#ranges [data-range="custom"]')};
      document.querySelector('#from').value=local(1700000100);
      document.querySelector('#to').value=local(1700000200);
      ${click('#applyRange')};
    })()`,
    'state-history-unavailable': `(() => {
      const local = seconds => {
        const d = new Date(seconds * 1000), p = n => String(n).padStart(2, '0');
        return d.getFullYear()+'-'+p(d.getMonth()+1)+'-'+p(d.getDate())+'T'+p(d.getHours())+':'+p(d.getMinutes())+':'+p(d.getSeconds());
      };
      ${click('#ranges [data-range="custom"]')};
      document.querySelector('#from').value=local(1699000000);
      document.querySelector('#to').value=local(1699003600);
      ${click('#applyRange')};
    })()`,
    'state-history-outside-before': `(() => {
      const local = seconds => {
        const d = new Date(seconds * 1000), p = n => String(n).padStart(2, '0');
        return d.getFullYear()+'-'+p(d.getMonth()+1)+'-'+p(d.getDate())+'T'+p(d.getHours())+':'+p(d.getMinutes())+':'+p(d.getSeconds());
      };
      ${click('#ranges [data-range="custom"]')};
      document.querySelector('#from').value=local(1697000000);
      document.querySelector('#to').value=local(1697003600);
      ${click('#applyRange')};
    })()`,
    'state-history-outside-after': `(() => {
      const local = seconds => {
        const d = new Date(seconds * 1000), p = n => String(n).padStart(2, '0');
        return d.getFullYear()+'-'+p(d.getMonth()+1)+'-'+p(d.getDate())+'T'+p(d.getHours())+':'+p(d.getMinutes())+':'+p(d.getSeconds());
      };
      ${click('#ranges [data-range="custom"]')};
      document.querySelector('#from').value=local(1700100000);
      document.querySelector('#to').value=local(1700103600);
      ${click('#applyRange')};
    })()`,
    'history-window-freeze': click('#live'),
    'history-window-resume': click('#live'),
    'dialog-client-secret-open': `${setValue('#ck-name', 'fixture-client')};${click('#ck-add')}`,
    'dialog-client-secret-close': click('#modal-done'),
    'dialog-confirm-cancel-focus-return': `document.querySelector('[data-kdel="0"]').focus();${click('[data-kdel="0"]')}`,
    'mutation-nim-key-create': `${setValue('#nk-key', 'nvapi-ui-fixture')};${click('#nk-add')}`,
    'mutation-nim-key-rpm': `${setValue('[data-rpm="0"]', '41')};document.querySelector('[data-rpm="0"]').dispatchEvent(new Event('change'))`,
    'mutation-nim-key-toggle': click('[data-tog="0"]'),
    'mutation-nim-key-delete': `document.querySelector('[data-kdel="0"]').click()`,
    'mutation-client-key-delete': `document.querySelector('[data-ckdel="0"]').click()`,
    'mutation-client-auth-mode': click('[data-mode="keyed"]'),
    'mutation-server-limits': `${setValue('#sv-base', 'https://fixture.invalid/v1')};${setValue('#sv-maxwait', '900')};${setValue('#sv-heartbeat', '10')};${setValue('#sv-idle', '300')};${setValue('#sv-timeout', '300')};${setValue('#sv-ttl', '600')};${setValue('#sv-inflight', '512')};${click('#save-limits')}`,
    'mutation-server-history': `${setValue('#sv-retention-days', '30')};${setValue('#sv-default-days', '7')};${setValue('#sv-slo', '99.9')};${click('#save-history')}`,
    'mutation-governor-toggle': click('#gov-tog'),
    'mutation-governor-override-create': `${setValue('#gov-model', 'fixture/model')};${setValue('#gov-cap', '8')};${click('#gov-add')}`,
    'mutation-governor-override-delete': click('[data-govdel="0"]'),
    'mutation-user-create': `${setValue('#u-name', 'fixture-user')};${setValue('#u-pass', 'fixture-password')};${click('#u-add')}`,
    'mutation-user-role': `document.querySelector('[data-urole="1"]').value='admin';document.querySelector('[data-urole="1"]').dispatchEvent(new Event('change'))`,
    'mutation-user-password-reset': click('[data-urp="1"]'),
    'dialog-user-password-prompt-cancel': `document.querySelector('[data-urp="1"]').focus();${click('[data-urp="1"]')}`,
    'mutation-user-delete': click('[data-udel="1"]'),
    'account-password-validation': `${setValue('#a-cur', 'current-password')};${setValue('#a-new', 'replacement-password')};${setValue('#a-conf', 'different-password')};${click('#a-save')}`,
    'account-password-update': `${setValue('#a-cur', 'current-password')};${setValue('#a-new', 'replacement-password')};${setValue('#a-conf', 'replacement-password')};${click('#a-save')}`,
    'error-dashboard-range': click('#ranges [data-range="3600"]'),
    'error-dashboard-now': `await pollNow()`,
    'error-settings-load': openSettings,
    'error-settings-mutation': `${setValue('#sv-retention-days', '30')};${setValue('#sv-default-days', '7')};${setValue('#sv-slo', '99.9')};${click('#save-history')}`,
    'error-settings-fallback': `${setValue('#sv-retention-days', '30')};${setValue('#sv-default-days', '7')};${setValue('#sv-slo', '99.9')};${click('#save-history')}`,
    'error-nim-key-validation': `${setValue('#nk-key', 'nvapi-invalid-fixture')};${click('#nk-add')}`,
    'error-setup-key-validation': `${setValue('#newkey', 'nvapi-invalid-fixture')};${click('#addkey')}`,
    'setup-key-validation-success': `${setValue('#newkey', 'nvapi-ui-fixture')};${click('#addkey')}`,
    'error-setup-submit': `${click('#finish')}`,
    'setup-submit-success': `${click('#finish')}`,
  };
  return expressions[id] || null;
}

const SETTINGS_PRESTATE_PANEL = new Map([
  ['refresh-settings-access', 'access'],
  ['dialog-client-secret-open', 'access'],
  ['dialog-confirm-cancel-focus-return', 'access'],
  ['mutation-nim-key-create', 'access'],
  ['mutation-nim-key-rpm', 'access'],
  ['mutation-nim-key-toggle', 'access'],
  ['mutation-nim-key-delete', 'access'],
  ['mutation-client-key-delete', 'access'],
  ['mutation-client-auth-mode', 'server'],
  ['mutation-server-limits', 'server'],
  ['mutation-server-history', 'server'],
  ['mutation-governor-toggle', 'server'],
  ['mutation-governor-override-create', 'server'],
  ['mutation-governor-override-delete', 'server'],
  ['mutation-user-create', 'users'],
  ['mutation-user-role', 'users'],
  ['mutation-user-password-reset', 'users'],
  ['dialog-user-password-prompt-cancel', 'users'],
  ['mutation-user-delete', 'users'],
  ['account-password-validation', 'account'],
  ['account-password-update', 'account'],
  ['error-settings-mutation', 'server'],
  ['error-settings-fallback', 'server'],
  ['state-settings-exact-large-values', 'access'],
  ['error-nim-key-validation', 'access'],
]);

async function runMatrixContext({
  browser, row, role, configuredProxy, setupProxy, visualCapture = null,
  visualLocale = null, skipDomAssertions = false,
}) {
  const origin = row.fixtures.page === 'setup' ? setupProxy.origin : configuredProxy.origin;
  const pagePath = row.fixtures.page === 'dashboard'
    ? '/'
    : `/${row.fixtures.page}`;
  const { targetId } = await browser.send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await browser.send(
    'Target.attachToTarget',
    { targetId, flatten: true },
  );
  const plan = new UiFixturePlan(row, role);
  let activePlan = plan;
  let capture = !['after-ready', 'startup-range-through-action']
    .includes(row.fixtures.boundary);
  let strictCapture = ['startup-application', 'startup-held-bootstrap']
    .includes(row.fixtures.boundary);
  let heldBootstrapRelease = null;
  let dialog = null;
  let requestActivity = 0;
  const pendingFetch = new Set();
  const pausedRequests = new Set();
  const requests = [];
  const assets = new Set();
  const run = Object.fromEntries(CLEAN_RUN_FIELDS.map(field => [field, []]));
  const visualCatalogPath = visualLocale
    ? `/assets/${row.fixtures.page === 'dashboard' ? 'operator' : 'public'}/locales/${visualLocale}.json`
    : null;
  const expectedPaths = new Set(expectedRequestSequence(row).map(request => request.path));
  if (visualCatalogPath) {
    expectedPaths.delete(`/assets/${row.fixtures.page === 'dashboard' ? 'operator' : 'public'}/locales/en-US.json`);
    expectedPaths.add(visualCatalogPath);
  }
  const requestUrls = new Map();

  const drain = async () => {
    let stable = 0;
    let previous = -1;
    const deadline = Date.now() + 10000;
    while (Date.now() < deadline) {
      await Promise.all([...pendingFetch]);
      if (pendingFetch.size === 0 && requestActivity === previous) {
        stable += 1;
        if (stable >= 2) return;
      } else {
        stable = 0;
        previous = requestActivity;
      }
      await sleep(25);
    }
    throw new Error(`${row.id}: network did not settle`);
  };

  const routeFixture = normalized => {
    const { method, path: pathname } = normalized;
    if (method === 'GET' && pathname === '/api/locale-bootstrap') {
      return activePlan.locale();
    }
    if (method === 'GET' && pathname === '/api/config') {
      return activePlan.config();
    }
    if (method === 'GET' && pathname === '/api/dashboard/now') {
      if (activePlan.phase === 'action'
          && typeof activePlan.recipe.now === 'string'
          && activePlan.recipe.response) {
        return activePlan.response();
      }
      return activePlan.now();
    }
    if (method === 'GET' && pathname === '/api/dashboard') {
      return activePlan.range();
    }
    if (method === 'POST' && (pathname === '/setup/validate-key'
        || pathname === '/setup'
        || pathname.startsWith('/api/settings/'))) {
      return activePlan.response();
    }
    return undefined;
  };

  const handlePaused = async params => {
    pausedRequests.add(params.requestId);
    const normalized = normalizedPausedRequest(params.request);
    const pathname = normalized.path;
    requestActivity += 1;
    // The long-value row observes the initial range response, then explicitly
    // enters Settings to observe its reload. Startup config/Now are setup
    // prerequisites, not the row's target boundary.
    if ((row.fixtures.boundary === 'startup-dashboard-data'
          && pathname === '/api/dashboard/now')
        || (['startup-dashboard-range', 'startup-range-through-action']
          .includes(row.fixtures.boundary)
          && pathname === '/api/dashboard')) {
      capture = true;
      strictCapture = true;
    }
    if (capture && (expectedPaths.has(pathname)
        || (strictCapture && (normalized.method !== 'GET'
          || pathname.startsWith('/api/')
          || pathname.startsWith('/setup/'))))) {
      requests.push(normalized);
      if (!expectedPaths.has(pathname)) {
        run.unexpectedRequests.push(
          `${normalized.method} ${pathname}`,
        );
      }
    }
    if (row.fixtures.boundary === 'startup-held-bootstrap'
        && pathname === '/api/locale-bootstrap'
        && !heldBootstrapRelease) {
      await new Promise(resolve => { heldBootstrapRelease = resolve; });
    }
    let value = routeFixture(normalized);
    if (visualLocale && pathname === '/api/locale-bootstrap') {
      value.installed_locales = [visualLocale];
      value.server_default = visualLocale;
    } else if (visualLocale && pathname === visualCatalogPath) {
      value = loadTestLocaleCatalog(visualLocale);
      if (row.fixtures.page !== 'dashboard') value = publicCatalogProjection(value);
    }
    if (row.id === 'error-settings-fallback'
        && normalized.method === 'POST'
        && pathname === '/api/settings/history') {
      value.error = undefined;
      value.ok = false;
    }
    if (row.id === 'state-settings-exact-large-values'
        && normalized.method === 'GET'
        && pathname === '/api/config') {
      value.pool.enabled = 12345;
      value.pool.capacity_rpm = 67890;
      value.nim_keys[0].lane = 12344;
      value.nim_keys[0].in_window = 12345;
      value.nim_keys[0].rpm = 67890;
    }
    if (value !== undefined) {
      const reference = activePlan.lastReference;
      const status = reference === 'validate-failure.json'
        ? 200
        : reference?.startsWith('api-error')
            || reference === 'scenarios.json#setup-error'
          ? 400
          : value?.ok === false && value?.error?.code
            ? 400
            : 200;
      await browser.send('Fetch.fulfillRequest', {
        requestId: params.requestId,
        responseCode: status,
        responseHeaders: [
          { name: 'Content-Type', value: 'application/json' },
          { name: 'Cache-Control', value: 'no-store' },
        ],
        body: Buffer.from(JSON.stringify(value)).toString('base64'),
      }, sessionId);
    } else {
      await browser.send('Fetch.continueRequest', { requestId: params.requestId }, sessionId);
    }
    pausedRequests.delete(params.requestId);
  };

  const unsubscribe = browser.on(msg => {
    if (msg.sessionId !== sessionId) return;
    if (msg.method === 'Fetch.requestPaused') {
      let operation;
      operation = handlePaused(msg.params)
        .catch(async error => {
          run.pageErrors.push(error.message);
          try {
            await browser.send('Fetch.failRequest', {
              requestId: msg.params.requestId,
              errorReason: 'Failed',
            }, sessionId);
          } catch (_) {}
          pausedRequests.delete(msg.params.requestId);
        })
        .finally(() => pendingFetch.delete(operation));
      pendingFetch.add(operation);
    } else if (msg.method === 'Runtime.exceptionThrown') {
      const details = msg.params.exceptionDetails;
      run.pageErrors.push(
        details.exception?.description || details.text,
      );
    } else if (msg.method === 'Runtime.consoleAPICalled'
        && msg.params.type === 'error') {
      const message = msg.params.args
        .map(arg => arg.value ?? arg.description)
        .join(' ');
      if (!isExpectedMatrixApiResourceError(message)) run.consoleErrors.push(message);
    } else if (msg.method === 'Log.entryAdded'
        && msg.params.entry.level === 'error') {
      if (!isExpectedMatrixApiResourceError(msg.params.entry.text)) {
        run.consoleErrors.push(msg.params.entry.text);
      }
    } else if (msg.method === 'Page.javascriptDialogOpening') {
      if (!dialog) {
        run.pageErrors.push(`unexpected ${msg.params.type} dialog`);
        dialog = { accept: false };
      }
      browser.send('Page.handleJavaScriptDialog', {
        accept: dialog.accept,
        ...(dialog.promptText === undefined ? {} : { promptText: dialog.promptText }),
      }, sessionId).catch(error => run.pageErrors.push(error.message));
      dialog = null;
    } else if (msg.method === 'Network.requestWillBeSent') {
      requestUrls.set(msg.params.requestId, msg.params.request.url);
      if (msg.params.request.url.startsWith('http')
          && !msg.params.request.url.startsWith(origin)) {
        run.failedAssets.push(`external request ${msg.params.request.url}`);
      }
    } else if (msg.method === 'Network.responseReceived') {
      const response = msg.params.response;
      if (!response.url.startsWith(origin)) return;
      const pathname = new URL(response.url).pathname;
      if (!pathname.startsWith('/assets/')) return;
      if (response.status >= 400) run.failedAssets.push(`${response.status} ${pathname}`);
      else if (row.assets.includes(pathname)) assets.add(pathname);
    } else if (msg.method === 'Network.loadingFailed') {
      const requestUrl = requestUrls.get(msg.params.requestId);
      if (requestUrl?.startsWith(origin)
          && new URL(requestUrl).pathname.startsWith('/assets/')) {
        run.failedAssets.push(`${msg.params.errorText} ${new URL(requestUrl).pathname}`);
      }
    }
  });

  const evaluate = expression =>
    evaluateRaw(browser, sessionId, `(async()=>{
      const __wait = async selector => {
        for (let index = 0; index < 400; index++) {
          const node = document.querySelector(selector);
          if (node) return node;
          await new Promise(resolve => setTimeout(resolve, 25));
        }
        throw new Error('matrix selector did not appear: ' + selector);
      };
      ${expression}
    })()`);
  const resetObservation = () => {
    requests.length = 0;
    if (row.assets.length === 0) assets.clear();
    run.unexpectedRequests.length = 0;
    capture = true;
    strictCapture = true;
  };

  const prepareSetup = async actionId => {
    if (!actionId.startsWith('error-setup')
        && actionId !== 'setup-key-validation-success'
        && actionId !== 'setup-submit-success') return;
    await evaluate(`
      document.querySelector('#username').value='fixture-user';
      document.querySelector('#password').value='fixture-password';
      document.querySelector('#confirm').value='fixture-password';
      document.querySelector('#baseurl').value='https://integrate.api.nvidia.com';
      document.querySelector('#to2').click();
    `);
    await waitForMatrixCondition(
      browser, sessionId,
      `document.querySelector('#step2')?.hidden === false`,
      `${row.id}: setup step two`,
    );
    if (actionId.includes('submit') || actionId === 'setup-submit-success') {
      await evaluate(`
        keys=[{key:'nvapi-ui-fixture',rpm:40,models:3}];
        renderKeys();
        document.querySelector('#to3').click();
      `);
      await waitForMatrixCondition(
        browser, sessionId,
        `document.querySelector('#step3')?.hidden === false`,
        `${row.id}: setup review`,
      );
    }
  };

  const performAction = async (actionRow, actionPlan) => {
    activePlan = actionPlan;
    const prestatePanel = SETTINGS_PRESTATE_PANEL.get(actionRow.id);
    if (prestatePanel) {
      await evaluate(`
        document.querySelector('#side [data-tab="settings"]').click();
        await __wait('#setnav button');
        document.querySelector('#setnav [data-sub=${JSON.stringify(prestatePanel)}]').click();
      `);
      await drain();
      // Opening Settings is the precondition that consumes the `before`
      // ConfigResponse. The named row begins at its actual control/POST.
      resetObservation();
    }
    activePlan.enterAction();
    if ([
      'mutation-nim-key-delete',
      'mutation-client-key-delete',
      'mutation-user-delete',
    ].includes(actionRow.id)) dialog = { accept: true };
    if (actionRow.id === 'dialog-confirm-cancel-focus-return'
        || actionRow.id === 'dialog-user-password-prompt-cancel') {
      dialog = { accept: false };
    }
    if (actionRow.id === 'mutation-user-password-reset') {
      dialog = { accept: true, promptText: 'replacement-password' };
    }
    await prepareSetup(actionRow.id);
    const expression = matrixActionExpression(actionRow.id);
    if (expression) {
      try {
        await evaluate(expression);
      } catch (error) {
        throw new Error(`${actionRow.id}: ${error.message}`);
      }
    }
    if (actionRow.id === 'dialog-client-secret-escape-focus-return') {
      const key = {
        key: 'Escape',
        code: 'Escape',
        windowsVirtualKeyCode: 27,
        nativeVirtualKeyCode: 27,
      };
      await browser.send('Input.dispatchKeyEvent', { type: 'keyDown', ...key }, sessionId);
      await browser.send('Input.dispatchKeyEvent', { type: 'keyUp', ...key }, sessionId);
    }
    await drain();
    const unused = activePlan.unconsumed();
    if (unused.length) {
      run.unconsumedFixtures.push(
        ...unused.map(reference => `${actionRow.id}:${reference}`),
      );
    }
  };

  let unsubscribeLoad = () => {};
  try {
    await browser.send('Runtime.enable', {}, sessionId);
    await browser.send('Log.enable', {}, sessionId);
    await browser.send('Page.enable', {}, sessionId);
    await browser.send('Network.enable', {}, sessionId);
    await browser.send('Network.setCacheDisabled', { cacheDisabled: true }, sessionId);
    await browser.send('Fetch.enable', {
      patterns: [{ urlPattern: '*', requestStage: 'Request' }],
    }, sessionId);
    if (row.fixtures.page === 'dashboard') {
      await browser.send('Network.setExtraHTTPHeaders', {
        headers: { Authorization: 'Bearer root:test-password-1' },
      }, sessionId);
    }
    await browser.send('Page.addScriptToEvaluateOnNewDocument', {
      source: MATRIX_TIMER_SHIM,
    }, sessionId);

    let loadedResolve;
    let loaded = new Promise(resolve => { loadedResolve = resolve; });
    unsubscribeLoad = browser.on(msg => {
      if (msg.sessionId === sessionId && msg.method === 'Page.loadEventFired') {
        loadedResolve();
      }
    });
    await browser.send('Page.navigate', { url: origin + pagePath }, sessionId);
    if (row.fixtures.boundary === 'startup-held-bootstrap') {
      const deadline = Date.now() + 10000;
      while (!heldBootstrapRelease && Date.now() < deadline) await sleep(25);
      if (!heldBootstrapRelease) {
        throw new Error(`${row.id}: bootstrap request was not captured for holding`);
      }
      await waitForMatrixCondition(
        browser, sessionId,
        `document.body?.hidden === true`,
        `${row.id}: held startup body`,
      );
    } else {
      await Promise.race([
        loaded,
        sleep(15000).then(() => { throw new Error(`${row.id}: page load timed out`); }),
      ]);
      await waitForMatrixCondition(
        browser, sessionId,
        row.fixtures.page === 'dashboard'
          ? `document.body.hidden === false && typeof nowData !== 'undefined' && nowData !== null && typeof rangeData !== 'undefined' && rangeData !== null`
          : `document.body.hidden === false`,
        `${row.id}: application ready`,
      );
      await drain();
    }
    if (row.id === 'login-invalid-credentials') {
      resetObservation();
      loaded = new Promise(resolve => { loadedResolve = resolve; });
      await evaluate(`
        document.querySelector('input[name="username"]').value='fixture-user';
        document.querySelector('input[name="password"]').value='incorrect-password';
        document.querySelector('form').requestSubmit();
      `);
      await Promise.race([
        loaded,
        sleep(15000).then(() => { throw new Error(`${row.id}: invalid login timed out`); }),
      ]);
      await waitForMatrixCondition(
        browser, sessionId,
        `document.querySelector('#login-error')?.hidden === false`,
        `${row.id}: invalid login message`,
      );
      await drain();
    } else if (row.fixtures.boundary === 'after-ready') {
      resetObservation();
    }

    const dependencies = [];
    const addDependency = dependencyId => {
      const dependency = INTERACTION_ROWS.find(candidate => candidate.id === dependencyId);
      for (const nested of dependency.fixtures.requires) addDependency(nested);
      if (!dependencies.some(candidate => candidate.id === dependency.id)) {
        dependencies.push(dependency);
      }
    };
    for (const dependencyId of row.fixtures.requires) addDependency(dependencyId);
    for (const dependency of dependencies) {
      const dependencyPlan = new UiFixturePlan(dependency, role);
      await performAction(dependency, dependencyPlan);
    }
    if (row.fixtures.requires.length) resetObservation();
    activePlan = plan;
    if (row.fixtures.boundary === 'startup-held-bootstrap') {
      // The held state is the observation. Release the request only so the
      // target has no paused Fetch operation during teardown.
    } else {
      await performAction(row, plan);
    }

    const dom = [];
    for (const assertion of skipDomAssertions ? [] : row.dom) {
      if (assertion.collect?.kind === 'sequence') {
        const result = [];
        for (const selector of assertion.collect.steps) {
          await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
          await drain();
          result.push(await evaluateRaw(browser, sessionId, assertion.expression));
        }
        dom.push({
          check: assertion.check,
          expression: assertion.expression,
          collect: assertion.collect,
          result,
        });
      } else if (assertion.collect?.kind === 'context-sequence') {
        const result = [];
        for (const selector of assertion.collect.contexts[role]) {
          await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
          await drain();
          result.push(await evaluateRaw(browser, sessionId, assertion.expression));
        }
        dom.push({
          check: assertion.check,
          expression: assertion.expression,
          collect: assertion.collect,
          result: { [role]: result },
        });
      } else {
        await waitForMatrixCondition(
          browser,
          sessionId,
          `(() => {
            const canonical = value => {
              if (Array.isArray(value))
                return '[' + value.map(canonical).join(',') + ']';
              if (value && typeof value === 'object')
                return '{' + Object.keys(value).sort().map(key =>
                  JSON.stringify(key) + ':' + canonical(value[key])).join(',') + '}';
              return JSON.stringify(value);
            };
            return canonical(${assertion.expression})
              === ${JSON.stringify(canonicalJson(assertion.expected))};
          })()`,
          `${row.id}:${assertion.check}`,
        );
        dom.push({
          check: assertion.check,
          expression: assertion.expression,
          result: await evaluateRaw(browser, sessionId, assertion.expression),
        });
      }
    }
    if (visualCapture) {
      await visualCapture({
        browser,
        sessionId,
        row,
        role,
        plan,
        run,
        requests,
        assets,
        evaluate,
        drain,
      });
    }
    run.pageErrors.push(...await evaluateRaw(
      browser,
      sessionId,
      'window.__matrixPageErrors || []',
    ));
    run.promiseRejections.push(...await evaluateRaw(
      browser,
      sessionId,
      'window.__matrixPromiseRejections || []',
    ));
    if (heldBootstrapRelease) {
      // The held boundary owns only bootstrap. Resume without capture so the
      // target can settle and close, but its later startup work is not folded
      // into this loading-state observation.
      const release = heldBootstrapRelease;
      heldBootstrapRelease = null;
      capture = false;
      strictCapture = false;
      release();
    }
    await drain();
    // Every before/action/after fixture named by the target recipe must have
    // crossed the intercepted HTTP boundary. This catches a matrix row that
    // merely names a Rust response without actually exercising it.
    const requiredFixtures = new Set([plan.recipe.locale]);
    for (const field of ['config', 'now', 'range']) {
      requiredFixtures.add(selectedFixtureReference(plan.recipe[field], 'before', role));
      requiredFixtures.add(selectedFixtureReference(plan.recipe[field], 'action', role));
    }
    for (const reference of plan.responseReferences) requiredFixtures.add(reference);
    for (const reference of requiredFixtures) {
      if (reference && !plan.consumed.includes(reference)) {
        run.unconsumedFixtures.push(reference);
      }
    }
    if (plan.unconsumed().length) {
      run.unconsumedFixtures.push(...plan.unconsumed());
    }
    return {
      assets: [...assets],
      dom,
      fixtures: row.fixtures,
      requests,
      run,
    };
  } finally {
    if (heldBootstrapRelease) heldBootstrapRelease();
    for (const requestId of pausedRequests) {
      try {
        await browser.send('Fetch.continueRequest', { requestId }, sessionId);
      } catch (_) {}
    }
    unsubscribeLoad();
    unsubscribe();
    try { await browser.send('Target.closeTarget', { targetId }); } catch (_) {}
  }
}

function mergeMatrixContextObservations(row, observations) {
  if (observations.length === 1) return observations[0];
  const merged = {
    assets: [...new Set(observations.flatMap(observation => observation.assets))],
    dom: row.dom.map(assertion => ({
      check: assertion.check,
      expression: assertion.expression,
      ...(assertion.collect ? { collect: assertion.collect } : {}),
      result: {},
    })),
    fixtures: row.fixtures,
    requests: observations.flatMap(observation => observation.requests),
    run: Object.fromEntries(CLEAN_RUN_FIELDS.map(field => [
      field,
      observations.flatMap(observation => observation.run[field]),
    ])),
  };
  for (const observation of observations) {
    for (const [index, assertion] of observation.dom.entries()) {
      Object.assign(merged.dom[index].result, assertion.result);
    }
  }
  return merged;
}

async function runInteractionMatrix({ browser, configuredProxy, tmpdir }) {
  const setupDir = path.join(tmpdir, 'matrix-setup');
  fs.mkdirSync(setupDir);
  const setupProxy = await startProxy(setupDir, true);
  const observations = new Map();
  try {
    for (const row of INTERACTION_ROWS) {
      const roles = Array.isArray(row.fixtures.role) ? row.fixtures.role : [row.fixtures.role];
      const contexts = [];
      for (const role of roles) {
        contexts.push(await runMatrixContext({
          browser,
          row,
          role,
          configuredProxy,
          setupProxy,
        }));
      }
      observations.set(row.id, mergeMatrixContextObservations(row, contexts));
      if (process.env.DEBUG) console.log(`[matrix] ${row.id} observed`);
    }
  } finally {
    if (!await stopChild(setupProxy.proc)) {
      throw new Error('interaction setup proxy did not exit');
    }
  }
  return observations;
}

function prepareVisualArtifactDir() {
  const artifactDir = process.env.NIMPROXY_LAYOUT_ARTIFACT_DIR
    || fs.mkdtempSync(path.join(os.tmpdir(), 'nim-proxy-task9-visual-'));
  const resolved = path.resolve(artifactDir);
  const allowedPrefix = path.join(os.tmpdir(), 'nim-proxy-task9-visual-');
  if (!resolved.startsWith(allowedPrefix)) {
    throw new Error('visual artifacts must stay in /tmp/nim-proxy-task9-visual-*');
  }
  fs.mkdirSync(resolved, { recursive: true });
  if (fs.readdirSync(resolved).length) {
    throw new Error(`visual artifact directory must be empty: ${resolved}`);
  }
  return resolved;
}

function visualRootVisibleExpression(selector) {
  return `(() => {
    const node = document.querySelector(${JSON.stringify(selector)});
    if (!node || node.closest('[hidden]')) return false;
    const style = getComputedStyle(node);
    const rect = node.getBoundingClientRect();
    return style.display !== 'none' && style.visibility !== 'hidden'
      && rect.width > 0 && rect.height > 0;
  })()`;
}

function pngDimensions(bytes) {
  const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  if (bytes.length < 24 || !bytes.subarray(0, 8).equals(signature)
      || bytes.readUInt32BE(8) !== 13 || bytes.toString('ascii', 12, 16) !== 'IHDR') {
    throw new Error('visual artifact was not a PNG with an IHDR header');
  }
  const width = bytes.readUInt32BE(16);
  const height = bytes.readUInt32BE(20);
  if (!width || !height) throw new Error('visual artifact PNG had zero dimensions');
  return { width, height };
}

async function visibleInternalVerticalScrollers(context) {
  return evaluateRaw(context.browser, context.sessionId, `(() => {
    const visible = node => {
      if (node.closest('[hidden]')) return false;
      const style = getComputedStyle(node);
      const rect = node.getBoundingClientRect();
      return style.display !== 'none' && style.visibility !== 'hidden' && rect.width > 0 && rect.height > 0;
    };
    const selector = node => {
      if (node.id) return '#' + node.id;
      if (node.classList.contains('scroll') && node.parentElement?.id) return '#' + node.parentElement.id + ' > .scroll';
      return node.tagName.toLowerCase() + (node.classList.length ? '.' + node.classList[0] : '');
    };
    return [...document.querySelectorAll('*')].flatMap(node => {
      const style = getComputedStyle(node);
      if (!visible(node) || !/(auto|scroll|overlay)/.test(style.overflowY)
          || node.scrollHeight <= node.clientHeight + 1) return [];
      const rect = node.getBoundingClientRect();
      const directChild = node.firstElementChild?.tagName.toLowerCase() || null;
      return [{
        selector: selector(node),
        geometry: {
          left: rect.left, top: rect.top, width: rect.width, height: rect.height,
          scrollHeight: node.scrollHeight, clientHeight: node.clientHeight,
        },
        intentionalTable: node.classList.contains('scroll') && node.children.length === 1 && directChild === 'table',
        directChild,
      }];
    });
  })()`);
}

async function reachVisualSurface(item, context) {
  const waitFor = (expression, label) => waitForMatrixCondition(
    context.browser,
    context.sessionId,
    expression,
    `${item.identity}: ${label}`,
  );
  if (item.reach.startsWith('dashboard:')) {
    const tab = item.reach.slice('dashboard:'.length);
    await context.evaluate(`document.querySelector('#side [data-tab=${JSON.stringify(tab)}]').click()`);
    await context.drain();
    await waitFor(`(() => {
      const button = document.querySelector('#side [data-tab=${JSON.stringify(tab)}]');
      const visible = Array.from(document.querySelectorAll('main > section:not([hidden])'));
      return button?.getAttribute('aria-current') === 'page'
        && visible.length === 1 && visible[0]?.id === ${JSON.stringify(`tab-${tab}`)};
    })()`, 'dashboard surface');
    return;
  }
  if (item.reach === 'settings:load-error') {
    await waitFor(`document.querySelector('#tab-settings')?.hidden === false && ${visualRootVisibleExpression(item.root)}`, 'Settings error surface');
    return;
  }
  if (item.reach.startsWith('settings:')) {
    const panel = item.reach.slice('settings:'.length);
    if (await evaluateRaw(context.browser, context.sessionId, `document.querySelector('#tab-settings')?.hidden !== false`)) {
      await context.evaluate(`document.querySelector('#side [data-tab="settings"]').click()`);
      await context.drain();
    }
    await waitFor(`!!document.querySelector('#setnav button')`, 'Settings navigation');
    await context.evaluate(`document.querySelector('#setnav [data-sub=${JSON.stringify(panel)}]').click()`);
    await context.drain();
    await waitFor(`document.querySelector('#setnav [data-sub=${JSON.stringify(panel)}]')?.getAttribute('aria-current') === 'page' && ${visualRootVisibleExpression(item.root)}`, 'Settings panel');
    return;
  }
  if (item.reach === 'setup:review-after-successful-key') {
    await context.evaluate(`document.querySelector('#to3').click()`);
    await context.drain();
    await waitFor(`${visualRootVisibleExpression(item.root)} && document.querySelector('#review')?.childElementCount > 0`, 'setup review');
    return;
  }
  if (item.reach === 'setup:client-validation-error') {
    await context.evaluate(`
      document.querySelector('#username').value='fixture-user';
      document.querySelector('#password').value='short';
      document.querySelector('#confirm').value='different';
      document.querySelector('#to2').click();
    `);
    await context.drain();
    await waitFor(`${visualRootVisibleExpression(item.root)} && document.querySelector('#step1')?.hidden === false`, 'setup validation error');
    return;
  }
  if ([
    'setup:step1',
    'setup:step2',
    'setup:completion',
    'setup:key-validation-api-error',
    'setup:submit-api-error',
    'login:healthy',
    'login:invalid-credentials',
  ].includes(item.reach)) {
    await waitFor(visualRootVisibleExpression(item.root), 'preserved row surface');
    return;
  }
  throw new Error(`unknown visual reach: ${item.reach}`);
}

async function captureCurrentVisualItems({ items, artifactDir, drafts }, context) {
  const bySurface = new Map();
  for (const item of items) {
    const surfaceItems = bySurface.get(item.surfaceId) || [];
    surfaceItems.push(item);
    bySurface.set(item.surfaceId, surfaceItems);
  }
  for (const surfaceItems of bySurface.values()) {
    await reachVisualSurface(surfaceItems[0], context);
    for (const item of surfaceItems) {
    const [width, height] = item.viewport.split('x').map(Number);
    const captureErrors = [];
    let artifactWritten = false;
    let absolutePath = path.resolve(artifactDir, item.path);
    let reached = false;
    let rootVisible = false;
    let renderedLocale = '';
    let layoutProblemsForItem = [];
    let layoutMeasurements = null;
    let capture = null;
    try {
      if (path.relative(artifactDir, absolutePath).startsWith('..')) {
        throw new Error(`visual artifact path escapes its directory: ${item.path}`);
      }
      await context.browser.send('Emulation.setDeviceMetricsOverride', {
        width,
        height,
        deviceScaleFactor: 1,
        mobile: false,
      }, context.sessionId);
      await context.evaluate(`
        document.scrollingElement.scrollTop = 0;
        window.scrollTo(0, 0);
        document.querySelector('.main')?.scrollTo(0, 0);
        for (let node = document.querySelector(${JSON.stringify(item.layoutRoot)})?.parentElement; node; node = node.parentElement) {
          if (node.scrollHeight > node.clientHeight) node.scrollTop = 0;
        }
      `);
      await sleep(75);
      const state = await evaluateRaw(context.browser, context.sessionId, `(() => {
        const visible = node => {
          if (!node || node.closest('[hidden]')) return false;
          const style = getComputedStyle(node);
          const rect = node.getBoundingClientRect();
          return style.display !== 'none' && style.visibility !== 'hidden'
            && rect.width > 0 && rect.height > 0;
        };
        const root = document.querySelector(${JSON.stringify(item.root)});
        const owner = document.querySelector(${JSON.stringify(item.layoutRoot)});
        return {
          rootVisible: visible(root),
          ownerVisible: visible(owner),
          renderedLocale: document.documentElement.lang,
        };
      })()`);
      rootVisible = state.rootVisible;
      renderedLocale = state.renderedLocale;
      reached = state.rootVisible && state.ownerVisible;
      if (!reached) continue;
      const geometry = await evaluateRaw(context.browser, context.sessionId, `(() => {
        ${LAYOUT_CHECKER}
        const root = document.querySelector(${JSON.stringify(item.layoutRoot)});
        const boundary = document.querySelector(${JSON.stringify(item.layoutBoundary)});
        const measure = node => node ? {
          scrollWidth: node.scrollWidth,
          scrollHeight: node.scrollHeight,
          clientWidth: node.clientWidth,
          clientHeight: node.clientHeight,
        } : null;
        return {
          problems: [
            ...layoutProblems(${JSON.stringify(item.layoutRoot)}, ${JSON.stringify(item.layoutBoundary)}, ${JSON.stringify(item.identity)}),
            ...primaryAddRowProblems(${JSON.stringify(item.layoutRoot)}, ${JSON.stringify(item.identity)}, ${width}),
          ],
          root: measure(root),
          boundary: measure(boundary),
        };
      })()`);
      layoutProblemsForItem = geometry.problems;
      layoutMeasurements = { root: geometry.root, boundary: geometry.boundary };
      if (fs.existsSync(absolutePath)) {
        captureErrors.push(`artifact path already exists: ${item.path}`);
      } else {
        await context.evaluate(`
          document.scrollingElement.scrollTo(0, 0);
          window.scrollTo(0, 0);
          document.querySelector('.main')?.scrollTo(0, 0);
        `);
        await sleep(75);
        const metrics = await context.browser.send('Page.getLayoutMetrics', {}, context.sessionId);
        const viewport = metrics.cssLayoutViewport;
        const content = metrics.cssContentSize;
        const clip = {
          x: 0,
          y: 0,
          width: viewport.clientWidth,
          height: Math.max(height, Math.ceil(content.height)),
          scale: 1,
        };
        const internalVerticalScrollers = await visibleInternalVerticalScrollers(context);
        const screenshot = await context.browser.send(
          'Page.captureScreenshot',
          { format: 'png', fromSurface: true, captureBeyondViewport: true, clip },
          context.sessionId,
        );
        const bytes = Buffer.from(screenshot.data, 'base64');
        if (!bytes.length) {
          captureErrors.push(`artifact was empty: ${item.path}`);
        } else {
          const png = pngDimensions(bytes);
          capture = {
            kind: 'document-vertical-v1',
            requestedViewport: { width, height },
            cssLayoutViewport: {
              pageX: viewport.pageX,
              pageY: viewport.pageY,
              clientWidth: viewport.clientWidth,
              clientHeight: viewport.clientHeight,
            },
            cssContentSize: { x: content.x, y: content.y, width: content.width, height: content.height },
            clip,
            captureBeyondViewport: true,
            png,
            internalVerticalScrollers,
          };
          fs.writeFileSync(absolutePath, bytes, { flag: 'wx' });
          artifactWritten = fs.statSync(absolutePath).size > 0;
          if (!artifactWritten) captureErrors.push(`artifact was empty after write: ${item.path}`);
        }
      }
    } catch (error) {
      captureErrors.push(error.message);
    } finally {
      try {
        await context.browser.send('Emulation.clearDeviceMetricsOverride', {}, context.sessionId);
      } catch (error) {
        captureErrors.push(error.message);
      }
    }
    if (!reached) continue;
    drafts.push({
      ...item,
      absolutePath,
      artifactWritten,
      reached,
      rootVisible,
      renderedLocale,
      capture,
      layoutProblems: layoutProblemsForItem,
      layoutMeasurements,
      captureErrors,
      run: context.run,
      fixturesConsumed: context.plan.consumed,
      requests: context.requests,
      assets: context.assets,
    });
    }
  }
}

function visualObservationFromDraft(draft) {
  return {
    ...draft,
    fixturesConsumed: [...draft.fixturesConsumed],
    requests: [...draft.requests],
    requestedAssets: [...draft.assets],
    pageErrors: [...draft.run.pageErrors, ...draft.captureErrors],
    consoleErrors: [...draft.run.consoleErrors],
    promiseRejections: [...draft.run.promiseRejections],
    failedAssets: [...draft.run.failedAssets],
    unexpectedRequests: [...draft.run.unexpectedRequests],
    unconsumedFixtures: [...draft.run.unconsumedFixtures],
    run: undefined,
    captureErrors: undefined,
    assets: undefined,
  };
}

function visualIntegrityProblems(observations) {
  const problems = [];
  for (const observation of observations) {
    for (const field of ['failedAssets', 'unexpectedRequests', 'unconsumedFixtures']) {
      for (const detail of observation[field] || []) {
        problems.push(`visual-integrity:${observation.identity}:${field}:${canonicalJson(detail)}`);
      }
    }
  }
  return problems;
}

async function runVisualMatrix({ browser, configuredProxy, tmpdir, artifactDir }) {
  const startedAt = Date.now();
  const expected = resolveVisualExpectations().items;
  const drafts = [];
  const setupDir = path.join(tmpdir, 'matrix-setup');
  fs.mkdirSync(setupDir);
  const setupProxy = await startProxy(setupDir, true);
  let matrixError = null;
  try {
    for (const row of INTERACTION_ROWS) {
      for (const declaration of row.visual || []) {
        for (const role of declaration.roles) {
          for (const locale of declaration.locales) {
            const items = expected.filter(item => item.rowId === row.id
              && item.caseId === declaration.case
              && item.locale === locale
              && item.role === role);
            if (!items.length) continue;
            await runMatrixContext({
              browser,
              row,
              role,
              configuredProxy,
              setupProxy,
              visualLocale: locale === 'en-US' ? null : locale,
              skipDomAssertions: locale !== 'en-US',
              visualCapture: async context => {
                await captureCurrentVisualItems({ items, artifactDir, drafts }, context);
              },
            });
          }
        }
      }
    }
  } catch (error) {
    matrixError = error;
  } finally {
    if (!await stopChild(setupProxy.proc) && !matrixError) {
      matrixError = new Error('visual setup proxy did not exit');
    }
  }
  const observations = drafts.map(visualObservationFromDraft);
  const coverageProblems = visualCoverageProblems(observations);
  const integrityProblems = visualIntegrityProblems(observations);
  const report = path.join(artifactDir, 'visual-matrix.json');
  fs.writeFileSync(report, JSON.stringify({
    expectedCount: expected.length,
    observedCount: observations.length,
    observations,
    coverageProblems,
    integrityProblems,
    elapsedMs: Date.now() - startedAt,
  }, null, 2) + '\n');
  if (matrixError) throw matrixError;
  return { observations, coverageProblems, integrityProblems, report };
}

const SEMANTIC_CHECKER = `
  function semanticProblems(doc, expectedData) {
    const problems = [];
    const controlDescription = control => {
      if (control.id) return '#' + control.id;
      for (const attribute of ['data-rpm', 'data-urole']) {
        if (control.hasAttribute(attribute))
          return control.tagName.toLowerCase() + '[' + attribute + '=' + JSON.stringify(control.getAttribute(attribute)) + ']';
      }
      return control.tagName.toLowerCase();
    };
    const hasAccessibleName = control => {
      if ((control.getAttribute('aria-label') || '').trim()) return true;
      const labelledBy = (control.getAttribute('aria-labelledby') || '').trim().split(/\\s+/).filter(Boolean);
      if (labelledBy.some(id => (doc.getElementById(id)?.textContent || '').trim())) return true;
      return Array.from(control.labels || []).some(label => label.textContent.trim());
    };
    const visibleControl = control => control.type !== 'hidden'
      && !control.closest('[hidden]')
      && getComputedStyle(control).display !== 'none'
      && getComputedStyle(control).visibility !== 'hidden';
    if (doc.querySelectorAll('main').length !== 1) problems.push('invalid-landmark');
    if (doc.querySelectorAll('h1').length !== 1) problems.push('invalid-landmark');
    for (const control of doc.querySelectorAll('input,select,textarea')) {
      if (visibleControl(control) && !hasAccessibleName(control))
        problems.push('missing-accessible-name:' + controlDescription(control));
    }
    if (doc.querySelector('#modal') && doc.querySelector('#modal').tagName !== 'DIALOG') problems.push('invalid-landmark');
    if (doc.querySelector('[role="tablist"],[role="tab"],[role="switch"]')) problems.push('non-button-action');
    for (const action of doc.querySelectorAll('[data-tab],[data-range],[data-sub],[data-copy],[data-tog],[data-kdel],[data-ckdel],[data-govdel],[data-semantic-action]')) {
      if (action.tagName !== 'BUTTON') problems.push('non-button-action');
      const name = (action.getAttribute('aria-label') || action.getAttribute('title') || action.textContent || '').trim();
      if (!name) problems.push('missing-accessible-name');
    }
    for (const dialog of doc.querySelectorAll('dialog[open]')) {
      if (!dialog.contains(doc.activeElement)) problems.push('focus-escape');
    }
    for (const data of doc.querySelectorAll('[data-semantic-data]')) {
      if (data.textContent !== data.getAttribute('data-semantic-data')) problems.push('data-mutation');
    }
    for (const expected of expectedData || []) {
      if (![...doc.querySelectorAll(expected.selector)].some(node => node.textContent === expected.text))
        problems.push('data-mutation');
    }
    return [...new Set(problems)];
  }
`;

const LAYOUT_CHECKER = `
  function layoutProblems(rootSelector, boundarySelector, context) {
    const root = document.querySelector(rootSelector);
    const boundary = boundarySelector === '@viewport' ? null : document.querySelector(boundarySelector);
    if (!root || (!boundary && boundarySelector !== '@viewport')) return ['layout-unreachable:' + context + ':' + rootSelector + ':' + boundarySelector];
    const visible = node => {
      const style = getComputedStyle(node);
      const rect = node.getBoundingClientRect();
      return !node.closest('[hidden]')
        && style.display !== 'none' && style.visibility !== 'hidden' && rect.width > 0 && rect.height > 0;
    };
    const describe = node => {
      if (node.id) return '#' + node.id;
      for (const attribute of ['data-rpm', 'data-tog', 'data-kdel', 'data-ckdel']) {
        if (node.hasAttribute(attribute)) return node.tagName.toLowerCase() + '[' + attribute + '=' + JSON.stringify(node.getAttribute(attribute)) + ']';
      }
      return node.tagName.toLowerCase() + (node.classList.length ? '.' + node.classList[0] : '');
    };
    const boundaryRect = boundary ? boundary.getBoundingClientRect() : {
      left: 0, top: 0, right: innerWidth, bottom: innerHeight,
    };
    const problems = [];
    const seen = new Set();
    const candidates = [
      ...root.querySelectorAll('input,select,textarea,button'),
      ...root.querySelectorAll('*'),
    ].filter(node => {
      if (!visible(node)) return false;
      if (/^(INPUT|SELECT|TEXTAREA|BUTTON)$/.test(node.tagName)) return true;
      return node.children.length === 0 && node.textContent.trim();
    });
    for (const node of candidates) {
      if (seen.has(node)) continue;
      seen.add(node);
      const rect = node.getBoundingClientRect();
      const outside = rect.left < boundaryRect.left - 1 || rect.right > boundaryRect.right + 1
        || rect.top < boundaryRect.top - 1 || rect.bottom > boundaryRect.bottom + 1;
      const intrinsic = !/^(INPUT|SELECT|TEXTAREA)$/.test(node.tagName)
        && getComputedStyle(node).textOverflow !== 'ellipsis'
        && node.clientWidth > 0 && node.scrollWidth > node.clientWidth + 1;
      let maskedBy = '';
      let horizontallyReachable = false;
      for (let parent = node.parentElement; parent; parent = parent.parentElement) {
        const style = getComputedStyle(parent);
        const parentRect = parent.getBoundingClientRect();
        if (/(auto|scroll)/.test(style.overflowX) && parent.scrollWidth > parent.clientWidth + 1) {
          horizontallyReachable = true;
          break;
        }
        if (/(hidden|clip)/.test(style.overflowX)
            && (rect.left < parentRect.left - 1 || rect.right > parentRect.right + 1)) {
          maskedBy = describe(parent);
          break;
        }
        if (parent === boundary) break;
      }
      if (intrinsic || (!horizontallyReachable && outside && (maskedBy || boundarySelector === '@viewport'))) {
        problems.push('layout-clipped:' + context + ':' + describe(node)
          + ':' + (intrinsic ? 'intrinsic' : maskedBy ? 'masked-by-' + maskedBy : 'outside-viewport'));
      }
    }
    return problems;
  }

  function primaryAddRowProblems(rootSelector, context, viewportWidth = innerWidth) {
    const root = document.querySelector(rootSelector);
    if (!root) return ['layout-unreachable:' + context + ':' + rootSelector];
    if (viewportWidth > 500) return [];
    return [...root.querySelectorAll('.addrow > input:not([type="number"])')]
      .filter(node => {
        const style = getComputedStyle(node);
        const rect = node.getBoundingClientRect();
        return !node.closest('[hidden]')
          && style.display !== 'none' && style.visibility !== 'hidden' && rect.width > 0 && rect.height > 0;
      })
      .flatMap(node => {
        const row = node.parentElement.getBoundingClientRect();
        const field = node.getBoundingClientRect();
        if (field.left <= row.left + 1 && field.right >= row.right - 1) return [];
        const label = node.id ? '#' + node.id : node.tagName.toLowerCase();
        return ['layout-unusable:' + context + ':' + label + ':primary-not-own-row:'
          + Math.round(field.width) + '/' + Math.round(row.width)];
      });
  }
`;

async function main() {
  const chrome = findChrome();
  if (!chrome) {
    const error = new Error('no chromium found. Set CHROME=/path/to/chrome.');
    error.exitCode = 2;
    throw error;
  }

  const fixtures = loadFixtures();
  const tmpdir = fs.mkdtempSync(path.join(os.tmpdir(), 'nimproxy-render-'));
  let proxy = null;
  let browserProc = null;
  let browser = null;
  let cleanupPromise = null;
  const cleanupRun = () => {
    if (!cleanupPromise) {
      cleanupPromise = shutdownRun(browser, browserProc, proxy?.proc, tmpdir);
    }
    return cleanupPromise;
  };
  try {
    proxy = await startProxy(tmpdir);

    browserProc = spawn(chrome, [
    '--headless=new',
    '--disable-gpu',
    '--no-sandbox',
    '--hide-scrollbars',
    '--window-size=1440,2400',
    '--remote-debugging-port=0',
    `--user-data-dir=${path.join(tmpdir, 'profile')}`,
    'about:blank',
    ], {
      detached: process.platform !== 'win32',
      stdio: ['ignore', 'ignore', 'pipe'],
    });

  const wsUrl = await new Promise((resolve, reject) => {
    let buf = '';
    const timer = setTimeout(() => reject(new Error('browser did not report a debug port')), 20000);
    browserProc.stderr.on('data', (d) => {
      buf += d.toString();
      const m = buf.match(/ws:\/\/[^\s]+/);
      if (m) {
        clearTimeout(timer);
        resolve(m[0]);
      }
    });
    browserProc.on('exit', (c) => reject(new Error('browser exited early: ' + c)));
  });

  browser = await CDP.connect(wsUrl);
  const { targetId } = await browser.send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await browser.send('Target.attachToTarget', { targetId, flatten: true });
  const S = sessionId;

  if (visualMatrix) {
    await browser.send('Target.closeTarget', { targetId });
    const artifactDir = prepareVisualArtifactDir();
    const startedAt = Date.now();
    const matrix = await runVisualMatrix({
      browser,
      configuredProxy: proxy,
      tmpdir,
      artifactDir,
    });
    console.log(`[visual-matrix] artifact-dir=${artifactDir} report=${matrix.report} observed=${matrix.observations.length} expected=${resolveVisualExpectations().items.length} elapsed-ms=${Date.now() - startedAt}`);
    if (matrix.coverageProblems.length || matrix.integrityProblems.length) {
      const first = matrix.coverageProblems[0] || matrix.integrityProblems[0];
      console.error(`[visual-matrix] ${first}`);
      throw reportedFailure();
    }
    return;
  }

  const errors = [];
  const consoleErrors = [];
  const consoleMessages = [];
  const requestUrls = new Map();
  const initialAssetErrors = [];
  const requestedPresentation = new Set();
  const servedPresentation = new Set();
  const serverPageResponses = new Set();
  const serverCatalogResponses = new Set();
  const fetchOperations = new Set();
  const fetched = [];
  const requestTimeline = [];
  const startupState = {
    bootstrapFromServer: false,
    bootstrapOriginal: null,
    catalogHeldState: null,
    catalogReleasedAt: null,
    configFromServer: false,
    configOriginal: null,
    expectedLocale: null,
    failureReleasedAt: null,
    pageSourceProblems: [],
    readyText: null,
    selectedCatalogPath: null,
    secret: 'CATALOG_RESPONSE_BODY_MUST_NOT_BE_DISCLOSED',
    disclosureMarkers: new Set(),
  };
  startupState.disclosureMarkers.add(startupState.secret);
  const pagePath = IS_SETUP ? '/setup' : IS_LOGIN ? '/login' : '/';
  const selectedLocale = startupProbe === 'locale-precedence'
    ? precedenceCase === 'override' ? 'de-DE' : 'fr-FR'
    : localeArg || 'en-US';
  startupState.expectedLocale = selectedLocale;
  const catalogPath = IS_DASHBOARD
    ? `/assets/operator/locales/${selectedLocale}.json`
    : `/assets/public/locales/${selectedLocale}.json`;
  const stylesheetPath = IS_DASHBOARD
    ? '/assets/operator/operator.css'
    : '/assets/public/public.css';
  const applicationScriptPath = '/assets/operator/dashboard.js';
  const presentationPath = value =>
    value === '/' || value === '/setup' || value === '/login'
      || value.startsWith('/assets/');
  const headerValue = (headers, name) =>
    (headers || []).find(header => header.name.toLowerCase() === name)?.value;
  async function handleFetchPaused(params) {
    const { request } = params;
    const url = new URL(request.url);
    if (params.responseStatusCode !== undefined) {
      const headers = params.responseHeaders || [];
      if (startupProbe === 'app-script-failure'
          && url.pathname === applicationScriptPath) {
        await handleApplicationScriptResponseForStartupProbe({
          browser,
          sessionId: S,
          params,
          startupState,
        });
        return;
      }
      if (startupProbe === 'locale-precedence'
          && url.pathname === '/api/config') {
        await handleConfigResponseForLocalePrecedence({
          browser,
          sessionId: S,
          params,
          startupState,
        });
        return;
      }
      if (startupProbe
          && ['/api/locale-bootstrap', catalogPath].includes(url.pathname)) {
        await handleCatalogResponseForStartupProbe({
          browser,
          sessionId: S,
          params,
          pathname: url.pathname,
          catalogPath,
          startupState,
        });
        return;
      }
      if ((escapeProbe || localeArg)
          && [catalogPath, '/api/locale-bootstrap'].includes(url.pathname)) {
        await handleCatalogResponseForProbe({
          browser,
          sessionId: S,
          params,
          pathname: url.pathname,
          catalogPath,
          serverCatalogResponses,
        });
        return;
      }
      if (url.pathname !== pagePath || request.method !== 'GET') {
        throw new Error(
          `unexpected response-stage interception: ${request.method} ${url.pathname}`,
        );
      }
      if (params.responseStatusCode !== 200) {
        throw new Error(`server page response was ${params.responseStatusCode} ${url.pathname}`);
      }
      if (headerValue(headers, 'content-type') !== 'text/html; charset=utf-8'
          || headerValue(headers, 'cache-control') !== 'no-store'
          || headerValue(headers, 'content-security-policy') !== PRESENTATION_CSP) {
        throw new Error(`server page response headers violated the presentation contract: ${url.pathname}`);
      }
      const serverHtml = await responseBody(browser, S, params.requestId);
      if (startupProbe) {
        if (/id=["']i18n-catalog["']/.test(serverHtml)) {
          startupState.pageSourceProblems.push('inline catalog survives');
        }
        if (!/<body\b[^>]*\bhidden(?:\s|=|>)/i.test(serverHtml)) {
          startupState.pageSourceProblems.push('body is not hidden in served HTML');
        }
      }
      serverPageResponses.add(url.pathname);
      await fulfillPausedResponse(browser, S, params, serverHtml);
      return;
    }
    requestTimeline.push({
      at: Date.now(),
      method: request.method,
      path: url.pathname,
    });
    if (startupProbe === 'css-failure' && url.pathname === stylesheetPath) {
      startupState.failureReleasedAt = Date.now();
      await browser.send('Fetch.failRequest', {
        requestId: params.requestId,
        errorReason: 'Failed',
      }, S);
      return;
    }
    let body = null;
    if (url.pathname === '/api/config' && startupProbe !== 'locale-precedence')
      body = fixtures['config-superuser.json'];
    else if (url.pathname === '/api/dashboard/now')
      body = fixtures['dashboard-now-initial.json'];
    else if (url.pathname === '/api/dashboard')
      body = fixtures[LAYOUT_RANGE_FIXTURES[layoutState] || 'dashboard-healthy.json'];
    else if (url.pathname === '/setup/validate-key' && request.method === 'POST')
      body = fixtures['validate-success.json'];
    else if (url.pathname === '/setup' && request.method === 'POST')
      body = fixtures['setup-minted-client-key.json'];
    if (body !== null) {
      fetched.push(url.pathname);
      await browser.send('Fetch.fulfillRequest', {
        requestId: params.requestId,
        responseCode: 200,
        responseHeaders: [{ name: 'Content-Type', value: 'application/json' }],
        body: Buffer.from(JSON.stringify(body)).toString('base64'),
      }, S);
    } else {
      await browser.send('Fetch.continueRequest', { requestId: params.requestId }, S);
    }
  }
  browser.on((msg) => {
    if (msg.sessionId !== S) return;
    if (msg.method === 'Runtime.exceptionThrown') {
      const d = msg.params.exceptionDetails;
      errors.push({
        text: (d.exception && d.exception.description) || d.text,
        line: d.lineNumber + 1,
        col: d.columnNumber,
      });
    }
    if (msg.method === 'Log.entryAdded') {
      const entry = msg.params.entry;
      const rendered = '[' + entry.source + '] ' + entry.text
        + (entry.url ? ` (${entry.url})` : '');
      consoleMessages.push(rendered);
      if (entry.level === 'error') consoleErrors.push(rendered);
    }
    if (msg.method === 'Runtime.consoleAPICalled') {
      const rendered = msg.params.args
        .map((a) => a.value ?? a.description)
        .join(' ');
      consoleMessages.push(rendered);
      if (msg.params.type === 'error') consoleErrors.push(rendered);
    }
    if (msg.method === 'Network.requestWillBeSent') {
      const url = msg.params.request.url;
      requestUrls.set(msg.params.requestId, url);
      if (url.startsWith('http') && !url.startsWith(proxy.origin)) {
        initialAssetErrors.push(`external request ${url}`);
      }
      if (url.startsWith(proxy.origin)) {
        const pathname = new URL(url).pathname;
        if (presentationPath(pathname)) requestedPresentation.add(pathname);
      }
    }
    if (msg.method === 'Network.responseReceived') {
      const { response } = msg.params;
      const url = new URL(response.url);
      if (response.url.startsWith(proxy.origin)
          && presentationPath(url.pathname)
          && response.status >= 400) {
        initialAssetErrors.push(`${response.status} ${url.pathname}`);
      } else if (response.url.startsWith(proxy.origin)
          && presentationPath(url.pathname)) {
        servedPresentation.add(url.pathname);
      }
    }
    if (msg.method === 'Network.loadingFailed') {
      const url = requestUrls.get(msg.params.requestId) || 'unknown request';
      if (url.startsWith(proxy.origin)) {
        initialAssetErrors.push(`${msg.params.errorText} ${url}`);
      }
    }
    if (msg.method === 'Fetch.requestPaused') {
      const operation = handleFetchPaused(msg.params)
        .catch(async error => {
          errors.push({ text: error.message, line: 0, col: 0 });
          try {
            await browser.send('Fetch.failRequest', {
              requestId: msg.params.requestId,
              errorReason: 'Failed',
            }, S);
          } catch (_) {}
        })
        .finally(() => fetchOperations.delete(operation));
      fetchOperations.add(operation);
    }
  });

  await browser.send('Runtime.enable', {}, S);
  await browser.send('Log.enable', {}, S);
  await browser.send('Page.enable', {}, S);
  await browser.send('DOM.enable', {}, S);
  await browser.send('Network.enable', {}, S);
  const fetchPatterns = [{ urlPattern: '*', requestStage: 'Request' }];
  if (escapeProbe || localeArg || startupProbe) {
    fetchPatterns.push({
      urlPattern: proxy.origin + pagePath,
      requestStage: 'Response',
    });
  }
  if (startupProbe || escapeProbe || localeArg) {
    const responsePaths = ['/api/locale-bootstrap', catalogPath];
    if (startupProbe === 'locale-precedence') responsePaths.push('/api/config');
    for (const responsePath of responsePaths) {
      if (!startupProbe && !localeArg
          && responsePath === '/api/locale-bootstrap') continue;
      fetchPatterns.push({
        urlPattern: proxy.origin + responsePath,
        requestStage: 'Response',
      });
    }
  }
  if (startupProbe === 'app-script-failure') {
    fetchPatterns.push({
      urlPattern: proxy.origin + applicationScriptPath,
      requestStage: 'Response',
    });
  }
  await browser.send('Fetch.enable', { patterns: fetchPatterns }, S);
  if (IS_DASHBOARD) {
    await browser.send('Network.setExtraHTTPHeaders', {
      headers: { Authorization: 'Bearer root:test-password-1' },
    }, S);
  }
  await browser.send('Page.addScriptToEvaluateOnNewDocument', {
    source: `
      window.__pageErrors = [];
      window.addEventListener('error', function (e) {
        window.__pageErrors.push({ kind: 'error', msg: e.message,
          line: e.lineno, col: e.colno, stack: e.error && e.error.stack });
      });
      window.addEventListener('unhandledrejection', function (e) {
        var r = e.reason;
        window.__pageErrors.push({ kind: 'unhandledrejection',
          msg: String((r && r.message) || r), stack: r && r.stack });
      });
    `,
  }, S);

  const loaded = new Promise((resolve) => {
    browser.on((m) => {
      if (m.sessionId === S && m.method === 'Page.loadEventFired') resolve();
    });
  });
  await browser.send('Page.navigate', {
    url: proxy.origin + pagePath,
  }, S);
  await Promise.race([loaded, sleep(20000)]);

  const state = await evaluateRaw(browser, S, 'document.readyState');
  if (state !== 'complete') {
    console.error(`FAIL — page never finished loading (readyState=${state}).`);
    throw reportedFailure();
  }
  // let the first poll land and paint
  await sleep(1500);
  await Promise.all([...fetchOperations]);
  if (startupProbe) {
    const startupFailures = [...startupState.pageSourceProblems];
    const bootstrapIndex = requestTimeline.findIndex(row =>
      row.path === '/api/locale-bootstrap');
    const catalogIndex = requestTimeline.findIndex(row =>
      row.path === catalogPath);
    const configIndex = requestTimeline.findIndex(row =>
      row.path === '/api/config');
    const operatorConfigRequired = IS_DASHBOARD
      && !['bootstrap-failure', 'css-failure'].includes(startupProbe);
    for (const failure of operatorConfigStartupFailures({
      isDashboard: IS_DASHBOARD,
      startupProbe,
      requestTimeline,
      catalogPath,
    })) {
      const detail = {
        'operator-config-forbidden': 'config read is forbidden for this startup path',
        'operator-config-count': 'expected exactly one config read',
        'operator-config-order': 'expected bootstrap < config < catalog',
      }[failure];
      startupFailures.push(`${failure}: ${detail}`);
    }
    if (startupProbe === 'css-failure') {
      if (!requestTimeline.some(row => row.path === stylesheetPath)) {
        startupFailures.push('css-before-bootstrap: stylesheet failure was not forced');
      }
      if (bootstrapIndex >= 0) {
        startupFailures.push('css-before-bootstrap: bootstrap was requested after CSS failed');
      }
      if (catalogIndex >= 0) {
        startupFailures.push('css-before-bootstrap: catalog was requested after CSS failed');
      }
    } else if (startupProbe === 'app-script-failure'
        && !requestTimeline.some(row => row.path === applicationScriptPath)) {
      startupFailures.push(
        'app-asset-emergency-only: operator application-script failure was not forced',
      );
    } else if (bootstrapIndex < 0) {
      startupFailures.push(
        'bootstrap-before-catalog: bootstrap was not requested',
      );
    } else if (startupProbe === 'bootstrap-failure') {
      if (catalogIndex >= 0) {
        startupFailures.push(
          'bootstrap-before-catalog: catalog was requested after bootstrap failed',
        );
      }
    } else if (catalogIndex < 0 || bootstrapIndex >= catalogIndex) {
      startupFailures.push(
        'bootstrap-before-catalog: catalog was not requested after bootstrap',
      );
    }
    if (startupProbe === 'locale-precedence') {
      const persistedOverride = precedenceCase === 'override' ? 'en-US' : null;
      if (!startupState.bootstrapFromServer
          || JSON.stringify(startupState.bootstrapOriginal)
            !== JSON.stringify({
              installed_locales: ['en-US'],
              server_default: 'en-US',
            })) {
        startupFailures.push(
          `locale-precedence: real bootstrap provenance was not the production en-US-only response: ${JSON.stringify(startupState.bootstrapOriginal)}`,
        );
      }
      if (configIndex < 0) {
        startupFailures.push(
          'locale-precedence: authenticated /api/config was not requested',
        );
      } else if (!(bootstrapIndex < configIndex && configIndex < catalogIndex)) {
        startupFailures.push(
          `locale-precedence: expected bootstrap < config < catalog, got ${bootstrapIndex} < ${configIndex} < ${catalogIndex}`,
        );
      }
      if (!startupState.configFromServer
          || startupState.configOriginal?.locale !== persistedOverride) {
        startupFailures.push(
          `locale-precedence: real /api/config did not preserve the ${precedenceCase} persisted-source shape; expected locale=${JSON.stringify(persistedOverride)} got ${JSON.stringify(startupState.configOriginal?.locale)}`,
        );
      }
      if (fetched.includes('/api/config')) {
        startupFailures.push(
          'locale-precedence: /api/config used a hand-authored fixture instead of the real server response',
        );
      }
      const localeCatalogRequests = requestTimeline
        .filter(row => row.path.startsWith('/assets/operator/locales/'))
        .map(row => row.path);
      if (localeCatalogRequests.length !== 1
          || localeCatalogRequests[0] !== catalogPath
          || startupState.selectedCatalogPath !== catalogPath) {
        startupFailures.push(
          `locale-precedence: ${precedenceCase} must select exactly ${catalogPath} at the catalog boundary, got requests=${JSON.stringify(localeCatalogRequests)} response=${JSON.stringify(startupState.selectedCatalogPath)}`,
        );
      }
    }
    if (startupProbe === 'delayed-catalog') {
      if (!startupState.catalogHeldState?.hidden) {
        startupFailures.push(
          'catalog-before-reveal: body was visible while the catalog response was held',
        );
      }
      if (!startupState.catalogHeldState?.hidden
          && startupState.catalogHeldState?.text) {
        startupFailures.push(
          'catalog-before-reveal: stale English was visible while the catalog response was held',
        );
      }
    }
    const appRequests = requestTimeline.filter(row =>
      row.path.startsWith('/api/') && row.path !== '/api/locale-bootstrap');
    if (startupProbe === 'css-failure' && appRequests.length) {
      startupFailures.push(
        `css-before-bootstrap: application API work started after CSS failed: ${appRequests.map(row => row.path).join(', ')}`,
      );
    }
    const beforeCatalog = appRequests.filter(row =>
      !(operatorConfigRequired && row.path === '/api/config')
      && (!startupState.catalogReleasedAt || row.at < startupState.catalogReleasedAt));
    if (beforeCatalog.length) {
      startupFailures.push(
        `catalog-before-api: ${beforeCatalog.map(row => row.path).join(', ')} requested before the catalog resolved`,
      );
    }

    const hardHiddenExpected = startupProbe === 'css-failure';
    const emergencyExpected = !['delayed-catalog', 'locale-precedence'].includes(startupProbe)
      && !hardHiddenExpected;
    const deadline = Date.now() + 3000;
    let visible = null;
    do {
      visible = await evaluateRaw(browser, S, `JSON.stringify({
        hidden: !document.body || document.body.hidden,
        text: document.body ? document.body.innerText.trim() : '',
        stylesheetRules: (() => {
          const sheet = document.getElementById('presentation-stylesheet')?.sheet;
          try { return sheet ? sheet.cssRules.length : null; }
          catch { return 'unreadable'; }
        })(),
      })`).then(JSON.parse);
      if ((hardHiddenExpected && visible.hidden)
          || (!hardHiddenExpected && !emergencyExpected && !visible.hidden)
          || (emergencyExpected && visible.text)) break;
      await sleep(50);
    } while (Date.now() < deadline);

    if (hardHiddenExpected) {
      if (!visible.hidden) {
        startupFailures.push(
          `css-before-bootstrap: page became visible after CSS failed with stylesheetRules=${JSON.stringify(visible.stylesheetRules)} text=${JSON.stringify(visible.text)}`,
        );
      }
    } else if (emergencyExpected) {
      if (visible.hidden
          || visible.text !== 'NIM Proxy interface failed to load.') {
        const check = startupProbe === 'app-script-failure'
          ? 'app-asset-emergency-only'
          : 'emergency-only';
        startupFailures.push(
          `${check}: got hidden=${visible.hidden} text=${JSON.stringify(visible.text)}`,
        );
      }
      await sleep(IS_DASHBOARD ? 3500 : 500);
      const failureAt = startupState.failureReleasedAt || 0;
      const afterFailure = requestTimeline.filter(row =>
        row.at > failureAt
        && row.path.startsWith('/api/')
        && row.path !== '/api/locale-bootstrap');
      if (afterFailure.length) {
        startupFailures.push(
          `catalog-before-api: requests continued after startup failure: ${afterFailure.map(row => row.path).join(', ')}`,
        );
      }
    } else {
      const readyExpression = IS_DASHBOARD
        ? `Boolean(
            !document.body.hidden
            && document.getElementById('o-traffic')?.textContent.trim()
            && document.getElementById('pagetitle')?.textContent.trim()
              === ${JSON.stringify(startupState.readyText)}
          )`
        : IS_SETUP
          ? `Boolean(
              !document.body.hidden
              && document.getElementById('wiz')
              && document.getElementById('to2')?.textContent.trim()
                === ${JSON.stringify(startupState.readyText)}
            )`
          : `Boolean(
              !document.body.hidden
              && document.querySelector('.login-card')
              && document.querySelector('input[name="username"]')?.placeholder === 'Username'
              && document.querySelector('button[type="submit"]')?.textContent.trim()
                === ${JSON.stringify(startupState.readyText)}
            )`;
      let ready = false;
      const readyDeadline = Date.now() + 3000;
      do {
        ready = await evaluateRaw(browser, S, readyExpression);
        if (ready) break;
        await sleep(50);
      } while (Date.now() < readyDeadline);
      if (!ready) {
        startupFailures.push(
          'catalog-before-reveal: catalog resolved but the page did not reach its ready state',
        );
      }
      if (startupProbe === 'locale-precedence') {
        const renderedMarker = await evaluateRaw(
          browser,
          S,
          'document.getElementById("pagetitle")?.textContent.trim() || null',
        );
        if (renderedMarker !== startupState.readyText) {
          startupFailures.push(
            `locale-precedence: ${precedenceCase} rendered marker ${JSON.stringify(renderedMarker)}, expected exactly ${JSON.stringify(startupState.readyText)}`,
          );
        }
      }
      const afterCatalog = appRequests.filter(row =>
        row.at >= startupState.catalogReleasedAt);
      if (IS_DASHBOARD && !afterCatalog.some(row =>
        row.path === '/api/dashboard' || row.path === '/api/dashboard/now')) {
        startupFailures.push(
          'catalog-before-api: dashboard did not start its API work after catalog resolution',
        );
      }
      if (!IS_DASHBOARD && appRequests.length) {
        startupFailures.push(
          `catalog-before-api: ${pageArg} made unexpected application API requests`,
        );
      }
      if (IS_LOGIN && requestTimeline.some(row =>
        row.path.startsWith('/assets/operator/'))) {
        startupFailures.push(
          'login-boundary: anonymous login requested an operator asset',
        );
      }
    }

    const staleEnglish = IS_DASHBOARD
      ? 'Capacity & reliability'
      : IS_SETUP
        ? 'First-time setup'
        : 'Sign in to the dashboard';
    const disclosed = [...consoleMessages, ...errors.map(error => error.text)]
      .some(value => [...startupState.disclosureMarkers, staleEnglish]
        .some(marker => String(value).includes(marker)));
    if (disclosed) {
      startupFailures.push(
        'no-response-body-disclosure: response data or stale English reached console diagnostics',
      );
    }
    const unexpectedConsoleErrors = consoleErrors.filter(message =>
      !/Failed to load resource: the server responded with a status of 503/.test(message)
      && !(['css-failure', 'app-script-failure'].includes(startupProbe)
        && /Failed to load resource: net::ERR_FAILED/.test(message)));
    if (errors.length || unexpectedConsoleErrors.length) {
      startupFailures.push(
        `startup-errors: ${errors.length} page error(s), ${unexpectedConsoleErrors.length} unexpected console error(s)`,
      );
    }
    if (startupFailures.length) {
      console.error(
        `FAIL — catalog startup ${startupProbe} on ${pageArg} violated ${startupFailures.length} contract(s)`,
      );
      for (const failure of startupFailures) console.error(`  ${failure}`);
      throw reportedFailure();
    }
    console.log(`catalog startup ${startupProbe} ok — ${pageArg}`);
    return;
  }
  const requiredPresentation = IS_SETUP
    ? ['/setup', '/assets/public/public.css', '/assets/public/setup.js', catalogPath]
    : IS_LOGIN
      ? ['/login', '/assets/public/public.css', '/assets/public/login.js', catalogPath]
      : ['/', '/assets/operator/operator.css', '/assets/operator/shared.js',
         '/assets/operator/dashboard.js', '/assets/operator/settings.js', catalogPath];
  for (const resource of requiredPresentation) {
    if (!requestedPresentation.has(resource)) initialAssetErrors.push(`not requested ${resource}`);
    if (!servedPresentation.has(resource)) initialAssetErrors.push(`not served ${resource}`);
  }
  if ((escapeProbe || localeArg) && !serverPageResponses.has(pagePath)) {
    initialAssetErrors.push(`probe did not derive from the server page response ${pagePath}`);
  }
  if ((escapeProbe || localeArg) && !serverCatalogResponses.has(catalogPath)) {
    initialAssetErrors.push(`probe did not mutate the server catalog response ${catalogPath}`);
  }
  if (initialAssetErrors.length) {
    console.error('FAIL — initial presentation asset load was incomplete or external');
    for (const error of new Set(initialAssetErrors)) console.error('  ' + error);
    for (const cause of new Set(errors.map(error => error.text))) {
      console.error('  cause: ' + cause);
    }
    throw reportedFailure();
  }

  if (process.env.DEBUG) {
    const diag = await browser.send('Runtime.evaluate', {
      expression: `JSON.stringify({
        fetches: ${JSON.stringify(fetched)}.slice(0, 8),
        svgAny: document.querySelectorAll('svg').length,
        svgBig: Array.from(document.querySelectorAll('svg')).filter(s => {
          const r = s.getBoundingClientRect(); return r.width > 80 && r.height > 40; }).length,
        sections: Array.from(document.querySelectorAll('section[id], .tabpane, [data-tab]')).map(e => e.id).slice(0, 10),
        visibleH: document.body.getBoundingClientRect().height,
        traffic: !!document.getElementById('o-traffic'),
        trafficHTML: (document.getElementById('o-traffic') || {}).innerHTML ? 'populated' : 'EMPTY',
        hash: location.hash,
        stubRan: (typeof window.__pageErrors !== 'undefined'),
        readyState: document.readyState,
        booted: (typeof POLL_MS !== 'undefined'),
      })`,
      returnByValue: true,
    }, S);
    console.log('DEBUG ' + diag.result.value);
  }

  const evaluate = async (expression) => {
    const r = await browser.send(
      'Runtime.evaluate',
      { expression, returnByValue: true, awaitPromise: true },
      S,
    );
    if (r.exceptionDetails) {
      throw new Error('evaluate threw: ' + JSON.stringify(r.exceptionDetails.text));
    }
    return r.result.value;
  };

  if (layoutReport) {
    await browser.send('Emulation.setDeviceMetricsOverride', {
      width: 390, height: 844, deviceScaleFactor: 1, mobile: false,
    }, S);
  }

  if (IS_LOGIN) {
    const selectedLoginMessages = localeArg
      ? publicCatalogProjection(loadTestLocaleCatalog(localeArg)).messages
      : null;
    const expectedAppName = selectedLoginMessages?.['common.app_name'] ?? 'NIM Proxy';
    const expectedPrompt = selectedLoginMessages?.['login.prompt'] ?? 'Sign in to the dashboard.';
    const loginFailure = await evaluate(`
      (() => {
        if (document.body.hidden) return 'body stayed hidden after catalog resolution';
        if (!document.querySelector('form.login-card')) return 'login form is missing';
        if (document.querySelector('script[src^="/assets/operator/"]'))
          return 'login loaded an operator script';
        if (document.querySelector('h1')?.textContent !== ${JSON.stringify(expectedAppName)})
          return 'catalog did not render the product name';
        if (document.querySelector('.login-sub')?.textContent !== ${JSON.stringify(expectedPrompt)})
          return 'catalog did not render the login prompt';
        return '';
      })()`);
    const inPage = await evaluate(
      'JSON.stringify(window.__pageErrors || [])',
    ).then(JSON.parse);
    for (const error of inPage) {
      errors.push({ text: `${error.kind}: ${error.msg}`, line: error.line || 0, col: error.col || 0 });
    }
    const operatorRequests = [...requestedPresentation]
      .filter(pathname => pathname.startsWith('/assets/operator/'));
    const semanticProblems = semanticSelftest
      ? await evaluate(`(() => { ${SEMANTIC_CHECKER}; return semanticProblems(document, []); })()`)
      : [];
    const loginArtifacts = [];
    if (layoutReport) {
      const artifactDir = process.env.NIMPROXY_LAYOUT_ARTIFACT_DIR
        || fs.mkdtempSync(path.join(os.tmpdir(), 'nim-proxy-task9-layout-'));
      if (!path.resolve(artifactDir).startsWith(path.join(os.tmpdir(), 'nim-proxy-task9-')))
        throw new Error('layout artifacts must stay in /tmp/nim-proxy-task9-*');
      fs.mkdirSync(artifactDir, { recursive: true });
      for (const [width, height] of [[390, 844], [768, 1024], [900, 1000], [1440, 1000]]) {
        await browser.send('Emulation.setDeviceMetricsOverride', {
          width, height, deviceScaleFactor: 1, mobile: false,
        }, S);
        await sleep(150);
        const screenshot = await browser.send('Page.captureScreenshot', { format: 'png' }, S);
        const locale = localeArg ? `-${localeArg}` : '';
        const artifact = path.join(artifactDir, `login-healthy${locale}-${width}x${height}.png`);
        fs.writeFileSync(artifact, Buffer.from(screenshot.data, 'base64'));
        loginArtifacts.push({ viewport: `${width}x${height}`, artifact });
      }
      await browser.send('Emulation.clearDeviceMetricsOverride', {}, S);
    }
    await cleanupRun();
    if (loginFailure || operatorRequests.length || semanticProblems.length || errors.length || consoleErrors.length) {
      console.error('FAIL — login render violated its public catalog boundary');
      if (loginFailure) console.error(`  ${loginFailure}`);
      if (semanticProblems.length) console.error(`  semantic: ${semanticProblems.join(', ')}`);
      for (const pathname of operatorRequests) {
        console.error(`  requested operator asset ${pathname}`);
      }
      for (const error of errors) console.error(`  ${error.text}`);
      for (const error of consoleErrors) console.error(`  console: ${error}`);
      throw reportedFailure();
    }
    if (semanticSelftest) console.log('semantic served-page check ok — login landmark and labeled native form');
    for (const artifact of loginArtifacts) {
      console.log(`[layout-report] state=login-healthy viewport=${artifact.viewport} path=${artifact.artifact}`);
    }
    console.log('rendered anonymous login from the public catalog');
    console.log('render ok — no uncaught page errors');
    return;
  }

  /* Generated metric geometry changes on every poll. Prove the CSSOM bridge
     bounds its cache/rules without invalidating a live node during compaction. */
  if (!IS_SETUP) {
    const dynamicStyleFailure = await evaluate(`
      (() => {
        if (typeof MAX_DYNAMIC_STYLE_RULES !== 'number')
          return 'MAX_DYNAMIC_STYLE_RULES is not defined';
        const anchor = document.createElement('div');
        anchor.dataset.style = 'position:absolute;width:37px;height:11px';
        document.body.appendChild(anchor);
        applyDynamicStyles(anchor);
        const before = anchor.getBoundingClientRect();
        const compactionsBefore = dynamicStyleCompactions;
        for (let i = 0; i < MAX_DYNAMIC_STYLE_RULES * 2; i++) {
          const node = document.createElement('div');
          node.dataset.style = 'position:absolute;width:' + (1000 + i) + 'px';
          document.body.appendChild(node);
          applyDynamicStyles(node);
          node.remove();
        }
        const after = anchor.getBoundingClientRect();
        const sheet = [...document.styleSheets].find(candidate =>
          candidate.href?.endsWith('/assets/operator/operator.css'));
        const rules = [...sheet.cssRules].filter(rule =>
          rule.selectorText?.startsWith('.dynamic-style-')).length;
        const problem =
          dynamicStyleCompactions <= compactionsBefore
            ? 'probe did not exercise live-node compaction'
            : dynamicStyleClasses.size > MAX_DYNAMIC_STYLE_RULES
            ? 'dynamic style cache exceeded its bound'
            : rules > MAX_DYNAMIC_STYLE_RULES
              ? 'dynamic stylesheet exceeded its bound'
              : rules !== dynamicStyleClasses.size
                ? 'dynamic style cache and stylesheet disagree'
              : before.width !== after.width || before.height !== after.height
                ? 'compaction changed live-node geometry'
                : '';
        anchor.remove();
        return problem;
      })()`);
    if (dynamicStyleFailure) {
      console.error(`[dynamic-style-bound] FAIL — ${dynamicStyleFailure}`);
      throw reportedFailure();
    }
  }

  /* Exercise the page's real ID/descriptor runtime. Static self-tests cover
     source-level forbidden contexts; this probe proves the helpers use native
     DOM sinks, preserve literal Unicode/entities/markup, and reject every
     non-text attribute. */
  if (escapeProbe) {
    const sinkContext = await evaluate(`
      (() => {
      const required = ['setMessageText', 'setMessageAttr'];
      for (const name of required) {
        if (typeof globalThis[name] !== 'function') return name + ' is not reachable';
      }
      if (typeof message !== 'function') return 'lexical message resolver is not reachable';
      if (typeof globalThis.message !== 'undefined')
        return 'raw message resolver is exposed on globalThis';
      if (!${JSON.stringify(IS_SETUP)} &&
          (typeof catalogMessage !== 'function' || typeof escapeHtml !== 'function'))
        return 'catalog descriptor or HTML sink is not reachable';
      if (typeof I18N_TEXT_ATTRS === 'undefined') return 'I18N_TEXT_ATTRS is not defined';
      const allowedAttrs = [...I18N_TEXT_ATTRS].sort().join(',');
      if (allowedAttrs !== 'alt,aria-label,placeholder,title')
        return 'I18N_TEXT_ATTRS is not exactly the four approved attributes';
      for (const old of ['rawMsg', 't', 'tRaw', 'tHtml']) {
        if (typeof globalThis[old] !== 'undefined') return old + ' escaped/plain compatibility helper survives';
      }
      const ids = Object.keys(MSG);
      if (!ids.length) return 'catalog has no probe id';
      const id = ids[0];
      const expected = message(id);
      if (!expected.includes(${JSON.stringify(PROBE_MARKER)})) return 'message() did not return the hostile probe';
      if (!expected.includes('<b>${PROBE_TAG_TEXT}</b>')) return 'message() did not return literal markup text';

      const host = document.createElement('div');
      document.body.appendChild(host);
      const problems = [];

      if (!${JSON.stringify(IS_SETUP)}) {
        const descriptor = catalogMessage(id);
        if (!Object.isFrozen(descriptor)) problems.push('catalog descriptor is mutable');
        let coercionError = '';
        try { String(descriptor); } catch (e) { coercionError = e.message; }
        if (coercionError !== 'catalog descriptor requires escapeHtml')
          problems.push('catalog descriptor did not refuse ordinary coercion');
        const escaped = expected.replace(/[&<>"']/g, c =>
          ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
        if (escapeHtml(descriptor) !== escaped)
          problems.push('HTML sink did not resolve and escape the catalog descriptor');
        for (const attempt of [
          d => { document.createElement('a').href = d; },
          d => { document.createElement('div').style.cssText = d; },
          d => { document.createElement('div').setAttribute('title', d); },
        ]) {
          let error = '';
          try { attempt(descriptor); } catch (e) { error = e.message; }
          if (error !== 'catalog descriptor requires escapeHtml')
            problems.push('catalog descriptor did not refuse a coercive non-HTML sink');
        }
      }

      const text = document.createElement('span');
      host.appendChild(text);
      setMessageText(text, id);
      if (text.textContent !== expected) problems.push('element text changed catalog bytes');
      if (text.children.length) problems.push('element text parsed catalog markup');
      const textHost = document.createElement('span');
      const textNode = document.createTextNode('');
      textHost.appendChild(textNode);
      host.appendChild(textHost);
      setMessageText(textNode, id);
      if (textNode.textContent !== expected)
        problems.push('ordinary parented text node changed catalog bytes');

      for (const node of [
        document.createElement('script'),
        document.createElement('style'),
        document.createElementNS('http://www.w3.org/2000/svg', 'svg'),
      ]) {
        let error = '';
        try { setMessageText(node, id); }
        catch (e) { error = e.message; }
        if (error !== 'forbidden catalog text target')
          problems.push(node.nodeName + ' did not throw the stable text-target refusal');
        if (node.textContent) problems.push(node.nodeName + ' received catalog text');
      }
      for (const parent of [
        document.createElement('script'),
        document.createElement('style'),
        document.createElementNS('http://www.w3.org/2000/svg', 'svg'),
      ]) {
        const textNode = document.createTextNode('');
        parent.appendChild(textNode);
        let error = '';
        try { setMessageText(textNode, id); }
        catch (e) { error = e.message; }
        if (error !== 'forbidden catalog text target')
          problems.push(parent.nodeName + ' parented text node was accepted');
        if (textNode.textContent) problems.push(parent.nodeName + ' parent received catalog text');
      }

      for (const attr of ['title', 'placeholder', 'aria-label', 'alt']) {
        const node = document.createElement(attr === 'alt' ? 'img' : 'input');
        host.appendChild(node);
        try { setMessageAttr(node, attr, id); }
        catch (e) { problems.push('refused allowlisted attribute ' + attr + ': ' + e.message); continue; }
        if (node.getAttribute(attr) !== expected) problems.push(attr + ' changed catalog bytes');
      }
      const accessibleSvg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
      setMessageAttr(accessibleSvg, 'aria-label', id);
      if (accessibleSvg.getAttribute('aria-label') !== expected)
        problems.push('SVG aria-label changed catalog bytes');

      for (const attr of ['href', 'src', 'style', 'onclick']) {
        const node = document.createElement('a');
        host.appendChild(node);
        let error = '';
        try { setMessageAttr(node, attr, id); }
        catch (e) { error = e.message; }
        if (error !== 'forbidden catalog attribute: ' + attr)
          problems.push(attr + ' did not throw the stable refusal');
        if (node.hasAttribute(attr)) problems.push(attr + ' was set from a catalog id');
      }
      if (typeof setMessageWithNodes === 'function') {
        const id = 'setup.step3.mintkey';
        const original = MSG[id];
        const key = document.createElement('code');
        key.textContent = 'KEY';
        try {
          MSG[id] = 'A {key} B {key}';
          setMessageWithNodes(host, id, [['{key}', key]]);
          if (host.textContent !== 'A KEY B KEY')
            problems.push('structured replacement did not preserve repeated placeholders');
          if (host.querySelectorAll('code').length !== 2)
            problems.push('structured replacement did not create one fixed node per placeholder');
        } finally {
          MSG[id] = original;
        }
        for (const node of [
          document.createElement('script'),
          document.createElement('style'),
          document.createElementNS('http://www.w3.org/2000/svg', 'svg'),
        ]) {
          let error = '';
          try { setMessageWithNodes(node, id, [['{key}', key]]); }
          catch (e) { error = e.message; }
          if (error !== 'forbidden catalog text target')
            problems.push(node.nodeName + ' structured helper did not refuse its target');
          if (node.textContent) problems.push(node.nodeName + ' received structured catalog text');
        }
        for (const replacement of [
          document.createElement('script'),
          document.createElement('style'),
          document.createElementNS('http://www.w3.org/2000/svg', 'svg'),
          (() => {
            const wrapper = document.createElement('span');
            wrapper.appendChild(document.createElement('script'));
            return wrapper;
          })(),
        ]) {
          let error = '';
          try { setMessageWithNodes(host, id, [['{key}', replacement]]); }
          catch (e) { error = e.message; }
          if (error !== 'forbidden catalog text target')
            problems.push(replacement.nodeName + ' structured replacement was accepted');
        }
      }
      if (typeof setEmphasizedMessage === 'function') {
        const id = 'setup.step3.mintwarn';
        for (const node of [
          document.createElement('script'),
          document.createElement('style'),
          document.createElementNS('http://www.w3.org/2000/svg', 'svg'),
        ]) {
          let error = '';
          try { setEmphasizedMessage(node, id, document.createElement('b')); }
          catch (e) { error = e.message; }
          if (error !== 'forbidden catalog text target')
            problems.push(node.nodeName + ' emphasis helper did not refuse its target');
          if (node.textContent) problems.push(node.nodeName + ' received emphasized catalog text');
        }
        for (const emphasis of [
          document.createElement('script'),
          document.createElement('style'),
          document.createElementNS('http://www.w3.org/2000/svg', 'svg'),
          (() => {
            const wrapper = document.createElement('b');
            wrapper.appendChild(document.createElement('script'));
            return wrapper;
          })(),
        ]) {
          let error = '';
          try { setEmphasizedMessage(host, id, emphasis); }
          catch (e) { error = e.message; }
          if (error !== 'forbidden catalog text target')
            problems.push(emphasis.nodeName + ' emphasis replacement was accepted');
        }
      }
      host.remove();
      return problems.length ? problems.join('; ') : '';
      })()`);
    if (sinkContext) {
      console.error(`[sink-context] FAIL — ${PAGE_REL}: ${sinkContext}`);
      throw reportedFailure();
    }
  }

  const untranslatedByTab = new Map();
  const dataDerived = new Set();
  const hovered = [];
  const semanticSurfaceProblems = [];
  const layoutSurfaceProblems = [];
  const layoutSurfaceEvidence = [];
  const captureSemanticSurface = async context => {
    if (!semanticSelftest) return;
    semanticSurfaceProblems.push(...await evaluate(`(() => {
      ${SEMANTIC_CHECKER}
      return semanticProblems(document, []).map(problem => ${JSON.stringify('surface:')} + ${JSON.stringify(context)} + ':' + problem);
    })()`));
  };
  const captureLayoutSurface = async (context, rootSelector, boundarySelector = '.main') => {
    if (!layoutReport) return;
    const result = await evaluate(`(() => {
      ${LAYOUT_CHECKER}
      const root = document.querySelector(${JSON.stringify(rootSelector)});
      const boundary = ${JSON.stringify(boundarySelector)} === '@viewport'
        ? document.documentElement : document.querySelector(${JSON.stringify(boundarySelector)});
      return {
        problems: [
          ...layoutProblems(${JSON.stringify(rootSelector)}, ${JSON.stringify(boundarySelector)}, ${JSON.stringify(context)}),
          ...primaryAddRowProblems(${JSON.stringify(rootSelector)}, ${JSON.stringify(context)}),
        ],
        visible: root ? Array.from(root.querySelectorAll('*')).filter(node => {
          const style = getComputedStyle(node), rect = node.getBoundingClientRect();
          return !node.closest('[hidden]') && style.display !== 'none' && style.visibility !== 'hidden' && rect.width > 0 && rect.height > 0;
        }).length : 0,
        root: root ? { scrollWidth: root.scrollWidth, clientWidth: root.clientWidth } : null,
        boundary: boundary ? { scrollWidth: boundary.scrollWidth, clientWidth: boundary.clientWidth } : null,
      };
    })()`);
    layoutSurfaceProblems.push(...result.problems);
    layoutSurfaceEvidence.push({ context, rootSelector, boundarySelector, ...result });
  };
  for (const tab of TABS) {
    // Tabs switch on click. `location.hash` is assigned BY that handler, and
    // the hash-to-click bridge runs once at load, so setting the hash after
    // load silently does nothing and every "tab" measures Overview again.
    const switched = await evaluate(IS_SETUP ? SETUP_STEPS[tab] : `
      (() => {
        const b = document.querySelector('#side nav button[data-tab="${tab}"]');
        if (!b) return false;
        b.click();
        return document.querySelector('#tab-${tab}') ? !document.querySelector('#tab-${tab}').hidden : false;
      })()`);
    if (!switched) {
      const detail = IS_SETUP
        ? await evaluate(`JSON.stringify({
          error: $('err')?.textContent || '', step1Hidden: $('step1')?.hidden,
          step2Hidden: $('step2')?.hidden,
        })`)
        : '';
      console.error(`FAIL — could not reach ${tab} in ${PAGE_REL}; the gate would measure the wrong panels ${detail}`);
      throw reportedFailure();
    }
    await sleep(1200);
    await captureSemanticSurface(IS_SETUP ? `setup-${tab}` : `dashboard-${tab}`);
    if (IS_SETUP) {
      const surface = tab === 'errors' ? 'step1' : tab === 'step1' ? 'step2' : tab;
      await captureLayoutSurface(`setup-${surface}`, '#wiz', '@viewport');
    }

    // Real pointer input, at the real coordinates of each rendered chart.
    const rects = await evaluate(`
      (() => Array.from(document.querySelectorAll('svg'))
        .map(s => { const r = s.getBoundingClientRect();
          return { id: (s.closest('[id]') || {}).id || '', w: r.width, h: r.height,
                   x: r.left + r.width / 2, y: r.top + r.height / 2 }; })
        .filter(r => r.w > 80 && r.h > 40))()`);

    for (const r of rects) {
      for (const frac of [0.3, 0.5, 0.75]) {
        await browser.send('Input.dispatchMouseEvent', {
          type: 'mouseMoved',
          x: Math.round(r.x + (frac - 0.5) * r.w * 0.8),
          y: Math.round(r.y),
          buttons: 0,
        }, S);
        await sleep(40);
      }
      hovered.push(`${tab}/${r.id}`);
    }

    if (localeArg && localeArg.startsWith('en-X')) {
      // Data is never translated: model ids, their prettified forms, publisher
      // names and client names come from the API, and localizing them would be
      // manipulating data rather than labelling it. Ask the page which strings
      // those are instead of guessing from the fixtures.
      if (IS_SETUP) {
        // The wizard has no API payload to scrape, but it does have inputs —
        // and every data value on screen is one THIS driver typed, plus the
        // secret the stub returned. Naming them keeps the report honest:
        // otherwise the gate calls a masked key and a URL "untranslated" and
        // the count stops meaning anything.
        for (const d of await evaluate(`
          (() => [
            ($('username') && $('username').value) || '',
            ($('newkey') && $('newkey').value) || '',
            (document.querySelector('#keylist code') || {}).textContent || '',
            ($('baseurl') && $('baseurl').value) || 'https://integrate.api.nvidia.com',
            ($('cbase') && $('cbase').textContent) || '',
            ($('csecret') && $('csecret').textContent) || '',
            'PROXY',
          ].filter(Boolean))()`)) dataDerived.add(d);
      } else {
        for (const d of await evaluate(SCAN_DATA_DERIVED)) dataDerived.add(d);
      }
      // Per tab, so each run is attributed to the tab it was found on. Tab
      // switching only toggles `hidden`, so a later scan would still SEE this
      // markup — but it would blame the wrong tab, and hover tooltips really
      // are transient, so anything hover-only has to be read here or not at all.
      for (const run of await evaluate(SCAN_UNTRANSLATED)) {
        if (!untranslatedByTab.has(run)) untranslatedByTab.set(run, tab);
      }
    }
  }

  if (layoutReport && IS_DASHBOARD) {
    await evaluate(`document.querySelector('#side nav button[data-tab="settings"]')?.click()`);
    await sleep(200);
    await captureLayoutSurface('settings-access', '#setbody');
  }

  if (semanticSelftest && IS_DASHBOARD) {
    const customRangeVisible = await evaluate(`(() => {
      document.querySelector('#ranges [data-range="custom"]')?.click();
      return document.querySelector('#custom')?.classList.contains('show') ?? false;
    })()`);
    if (!customRangeVisible) semanticSurfaceProblems.push('surface:dashboard-custom-range:unreachable');
    else await captureSemanticSurface('dashboard-custom-range');

    await evaluate(`document.querySelector('#side nav button[data-tab="settings"]')?.click()`);
    await sleep(200);
    const settingsPanels = await evaluate(`Array.from(document.querySelectorAll('#setnav button[data-sub]')).map(button => button.dataset.sub)`);
    if (!settingsPanels.length) semanticSurfaceProblems.push('surface:settings:unreachable');
    for (const panel of settingsPanels) {
      const reached = await evaluate(`(() => {
        const button = document.querySelector('#setnav button[data-sub=${JSON.stringify(panel)}]');
        button?.click();
        return !!button && document.querySelector('#setnav button[data-sub=${JSON.stringify(panel)}]')?.getAttribute('aria-current') === 'page';
      })()`);
      if (!reached) semanticSurfaceProblems.push('surface:settings-' + panel + ':unreachable');
      else {
        await sleep(100);
        await captureSemanticSurface('settings-' + panel);
      }
    }
  }

  // Task 9's browser checks deliberately inspect the served DOM rather than
  // source spelling. The checker is also fed isolated DOM mutations below so
  // its five failure ids cannot go green merely because this page happens to
  // avoid them today.
  if (semanticSelftest) {
    const semanticSelftestFailures = await evaluate(`(() => {
      ${SEMANTIC_CHECKER}
      const base = () => {
        const doc = document.implementation.createHTMLDocument('semantic-selftest');
        doc.body.innerHTML = '<main><h1>Heading</h1></main><button data-semantic-action aria-label="Action"></button>';
        return doc;
      };
      const cases = [];
      { const doc = base(); doc.querySelector('button').removeAttribute('aria-label'); cases.push(['missing-accessible-name', semanticProblems(doc).some(problem => problem.startsWith('missing-accessible-name'))]); }
      { const doc = base(); doc.body.insertAdjacentHTML('beforeend', '<input id="visible-control">'); cases.push(['visible-control-name', semanticProblems(doc).some(problem => problem === 'missing-accessible-name:#visible-control')]); }
      { const doc = base(); doc.body.insertAdjacentHTML('beforeend', '<input id="hidden-control" hidden>'); cases.push(['hidden-control-excluded', !semanticProblems(doc).some(problem => problem === 'missing-accessible-name:#hidden-control')]); }
      { const doc = base(); doc.querySelector('main').remove(); cases.push(['invalid-landmark', semanticProblems(doc).includes('invalid-landmark')]); }
      { const doc = base(); const action = doc.querySelector('button'); const replacement = doc.createElement('div'); replacement.setAttribute('data-semantic-action', ''); replacement.setAttribute('aria-label', 'Action'); action.replaceWith(replacement); cases.push(['non-button-action', semanticProblems(doc).includes('non-button-action')]); }
      { const doc = base(); const dialog = doc.createElement('dialog'); dialog.open = true; doc.body.append(dialog); cases.push(['focus-escape', semanticProblems(doc).includes('focus-escape')]); }
      { const doc = base(); const data = doc.createElement('span'); data.setAttribute('data-semantic-data', 'alpha'); data.textContent = 'Alpha'; doc.body.append(data); cases.push(['data-mutation', semanticProblems(doc).includes('data-mutation')]); }
      return cases.filter(([, observed]) => !observed).map(([name]) => name);
    })()`);
    if (semanticSelftestFailures.length) {
      console.error('[semantic-selftest] checker missed: ' + semanticSelftestFailures.join(', '));
      throw reportedFailure();
    }
    console.log('semantic selftest ok — missing-accessible-name, invalid-landmark, non-button-action, focus-escape, data-mutation observed');
  }

  if (layoutSelftest) {
    const layoutSelftestFailures = await evaluate(`(() => {
      ${LAYOUT_CHECKER}
      const boundary = document.createElement('div');
      boundary.id = 'layout-selftest-boundary';
      boundary.style.cssText = 'position:fixed;left:0;top:0;width:120px;height:120px;overflow:hidden';
      const clipped = document.createElement('span');
      clipped.id = 'layout-selftest-clipped';
      clipped.style.cssText = 'display:block;width:40px;height:10px';
      clipped.textContent = 'clipped';
      const input = document.createElement('input');
      input.id = 'layout-selftest-input';
      input.style.cssText = 'display:block;width:20px;box-sizing:border-box';
      input.value = 'a deliberately long editable value';
      const ellipsis = document.createElement('span');
      ellipsis.id = 'layout-selftest-ellipsis';
      ellipsis.style.cssText = 'display:block;width:20px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis';
      ellipsis.textContent = 'a deliberately long display value';
      const scroll = document.createElement('div');
      scroll.id = 'layout-selftest-scroll';
      scroll.style.cssText = 'display:block;width:120px;overflow:auto';
      const scrollable = document.createElement('span');
      scrollable.id = 'layout-selftest-scrollable';
      scrollable.style.cssText = 'display:block;width:240px;height:10px';
      scrollable.textContent = 'reachable through horizontal scrolling';
      scroll.append(scrollable);
      const addrow = document.createElement('div');
      addrow.className = 'addrow';
      addrow.style.cssText = 'display:flex;width:100px';
      const addInput = document.createElement('input');
      addInput.id = 'layout-selftest-add-input';
      addInput.style.cssText = 'width:20px;box-sizing:border-box';
      addrow.append(addInput);
      boundary.append(clipped);
      boundary.append(input, ellipsis, scroll, addrow);
      document.body.append(boundary);
      const problems = layoutProblems('#layout-selftest-boundary', '#layout-selftest-boundary', 'layout-selftest');
      const widePrimary = primaryAddRowProblems('#layout-selftest-boundary', 'layout-selftest-wide', 768);
      const narrowPrimary = primaryAddRowProblems('#layout-selftest-boundary', 'layout-selftest-narrow', 390);
      boundary.remove();
      const failures = [];
      if (!problems.some(problem => problem.startsWith('layout-clipped:layout-selftest:#layout-selftest-clipped')))
        failures.push('clipped-element');
      if (problems.some(problem => problem.includes('#layout-selftest-input:intrinsic')))
        failures.push('input-intrinsic-exemption');
      if (problems.some(problem => problem.includes('#layout-selftest-ellipsis:intrinsic')))
        failures.push('ellipsis-intrinsic-exemption');
      if (problems.some(problem => problem.includes('#layout-selftest-scrollable')))
        failures.push('horizontal-scroll-reachability-exemption');
      if (widePrimary.some(problem => problem.includes('#layout-selftest-add-input:primary-not-own-row')))
        failures.push('wide-primary-row-exemption');
      if (!narrowPrimary.some(problem => problem.includes('#layout-selftest-add-input:primary-not-own-row')))
        failures.push('narrow-primary-row-preserved');
      return failures;
    })()`);
    if (layoutSelftestFailures.length) {
      console.error('[layout-selftest] checker missed: ' + layoutSelftestFailures.join(', '));
      throw reportedFailure();
    }
    console.log('layout selftest ok — clipped element observed');
  }

  let task9SemanticProblems = [];
  let task9LayoutProblems = [...layoutSurfaceProblems];
  let task9LayoutMeasurements = [];
  let task9Artifacts = [];
  if (semanticSelftest) {
    task9SemanticProblems = [
      ...semanticSurfaceProblems,
      ...await evaluate(`(() => { ${SEMANTIC_CHECKER}; return semanticProblems(document, ${IS_DASHBOARD ? "[{ selector: '#m-toolcalls .bname', text: 'alpha' }]" : '[]'}); })()`),
    ];
  }
  if ((semanticSelftest || layoutReport) && IS_DASHBOARD) {
    await evaluate(`document.querySelector('#side nav button[data-tab="models"]').click()`);
    await sleep(200);
    if (layoutReport && layoutState === 'extreme')
      await captureLayoutSurface('dashboard-extreme-models', '#m-kpis');
    task9LayoutProblems = [...layoutSurfaceProblems];
    if (semanticSelftest || layoutState === 'healthy') {
      for (const [width, height] of [[1440, 1000], [900, 1000]]) {
        await browser.send('Emulation.setDeviceMetricsOverride', {
          width, height, deviceScaleFactor: 1, mobile: false,
        }, S);
        await sleep(150);
        const measurement = await evaluate(`(() => {
          const value = document.querySelector('#m-toolcalls .bval');
          if (!value) return { missing: true };
          return { text: value.textContent, scrollWidth: value.scrollWidth, clientWidth: value.clientWidth, scrollHeight: value.scrollHeight, clientHeight: value.clientHeight };
        })()`);
        task9LayoutMeasurements.push({ width, height, ...measurement });
      }
    }
    await browser.send('Emulation.clearDeviceMetricsOverride', {}, S);
  }

  if (layoutReport) {
    const artifactDir = process.env.NIMPROXY_LAYOUT_ARTIFACT_DIR
      || fs.mkdtempSync(path.join(os.tmpdir(), 'nim-proxy-task9-layout-'));
    if (!path.resolve(artifactDir).startsWith(path.join(os.tmpdir(), 'nim-proxy-task9-')))
      throw new Error('layout artifacts must stay in /tmp/nim-proxy-task9-*');
    fs.mkdirSync(artifactDir, { recursive: true });
    const captures = IS_DASHBOARD ? [['models', 'dashboard'], ['settings', 'settings']] : [[null, pageArg]];
    for (const [tab, surface] of captures) {
      if (tab) {
        await evaluate(`document.querySelector('#side nav button[data-tab=${JSON.stringify(tab)}]').click()`);
        await sleep(150);
      }
      for (const [width, height] of [[390, 844], [768, 1024], [900, 1000], [1440, 1000]]) {
        await browser.send('Emulation.setDeviceMetricsOverride', {
          width, height, deviceScaleFactor: 1, mobile: false,
        }, S);
        await sleep(150);
        const screenshot = await browser.send('Page.captureScreenshot', { format: 'png' }, S);
        const locale = localeArg ? `-${localeArg}` : '';
        const filename = `${surface}-${layoutState}${locale}-${width}x${height}.png`;
        const artifact = path.join(artifactDir, filename);
        fs.writeFileSync(artifact, Buffer.from(screenshot.data, 'base64'));
        const measurement = await evaluate(`(() => {
          const value = document.querySelector('#m-toolcalls .bval');
          return value ? { text: value.textContent, scrollWidth: value.scrollWidth, clientWidth: value.clientWidth, scrollHeight: value.scrollHeight, clientHeight: value.clientHeight } : { missing: true };
        })()`);
        task9Artifacts.push({ viewport: `${width}x${height}`, state: `${surface}-${layoutState}${locale}`, artifact, ...measurement });
      }
    }
    await browser.send('Emulation.clearDeviceMetricsOverride', {}, S);
    const report = path.join(artifactDir, 'layout-report.json');
    fs.writeFileSync(report, JSON.stringify({ page: pageArg, measurements: task9Artifacts }, null, 2) + '\n');
    task9Artifacts.push({ report });
  }

  // The poll loop re-applies the last hover on every live re-render, which is
  // how a hover throw escalates from "no tooltip" to "the tab stops updating".
  await sleep(3500);
  if (process.env.DEBUG) console.log('DEBUG hovered:', JSON.stringify(hovered));
  if (IS_DASHBOARD && !layoutReport) {
    const expectedHovered = [
      'overview/o-kpis',
      'overview/o-kpis',
      'overview/o-kpis',
      'overview/o-traffic',
      'models/chart-modeltok',
      'models/chart-ttft',
      'models/chart-tps',
      'models/chart-tpot',
      'models/chart-upstream',
      'reliability/chart-reqrate',
      'reliability/chart-outcomes',
      'reliability/chart-load',
      'reliability/chart-outcome-stack',
      'reliability/chart-qwait',
      'reliability/heatmap',
      'reliability/chart-exhaust',
      'capacity/chart-cooldowns',
    ];
    if (canonicalJson(hovered) !== canonicalJson(expectedHovered)) {
      console.error(
        `FAIL — dashboard hover inventory ${canonicalJson(hovered)} `
        + `!= ${canonicalJson(expectedHovered)}`,
      );
      throw reportedFailure();
    }
  }

  let doubleEscaped = [];
  if (escapeProbe) {
    doubleEscaped = await evaluate(`
      (() => {
        const bad = [];
        const ENTITY = /&amp;|&#39;|&quot;|&lt;|&gt;/;
        const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
        for (let n = walk.nextNode(); n; n = walk.nextNode()) {
          if (n.parentNode.nodeName === 'SCRIPT' || n.parentNode.nodeName === 'STYLE') continue;
          const t = n.textContent;
          if (ENTITY.test(t)) {
            bad.push({ dir: 'double', text: t.trim().slice(0, 70),
                       el: n.parentNode.className || n.parentNode.nodeName });
          }
        }
        // Attribute sinks. deltaChip's title=, ringGauge's aria-label=,
        // barList/leaderList/segbar title= and applyStatic's setAttribute path
        // are all outside SHOW_TEXT, so a double-escape in any of them shipped
        // green and surfaced as &#39; in a tooltip on the first translation.
        for (const el of document.querySelectorAll('[title],[aria-label],[placeholder],[alt]')) {
          for (const a of ['title', 'aria-label', 'placeholder', 'alt']) {
            const v = el.getAttribute(a);
            if (v && ENTITY.test(v)) {
              bad.push({ dir: 'double', text: v.trim().slice(0, 70),
                         el: (el.className || el.nodeName) + '[' + a + ']' });
            }
          }
        }
        // The other direction: the probe suffix ends in <b>Tag</b>. If a catalog
        // value reached a raw-HTML sink WITHOUT being escaped, that parsed into
        // a real element. Any <b> holding exactly the probe text is a sink that
        // does not escape when it must.
        // The <b> text alone is not enough to accuse: metricRow() and the
        // settings modal both put dynamic values inside <b>, so a value that
        // happened to read "Tag" would be a false accusation. Require the rest
        // of the probe suffix in the same host — it is appended to the SAME
        // catalog value, so an unescaped one always carries both.
        for (const b of document.querySelectorAll('b')) {
          if (b.textContent.trim() !== ${JSON.stringify(PROBE_TAG_TEXT)}) continue;
          const host = b.parentNode;
          const hostText = (host && host.textContent) || '';
          if (!hostText.includes(${JSON.stringify(PROBE_MARKER)})) continue;
          bad.push({ dir: 'missing', text: hostText.trim().slice(0, 70),
                     el: host.className || host.nodeName });
        }
        // Attribute-sink under-escaping: the probe's double quote closed the
        // attribute early, so the parser turned the remainder into attribute
        // NAMES. A real attribute name can never contain <, ", ' or =.
        // (No backticks in this comment — it lives inside a template literal.)
        for (const el of document.querySelectorAll('*')) {
          for (const a of el.getAttributeNames()) {
            if (/[<>"'=]/.test(a)) {
              bad.push({ dir: 'missing',
                         text: 'attribute-name breakout: ' + a.slice(0, 40),
                         el: (el.className || el.nodeName) });
            }
          }
        }
        return bad;
      })()`);
  }

  /* The `status` label is whatever the upstream returned, so "succeeded" is
     not "=== '200'". The captured fixtures only contain 200/429/504/disconnect,
     so replaying them can never observe the disagreement; the predicates
     themselves can, and they are module-scope for exactly this reason. */
  const predicateFailures = IS_SETUP ? [] : await evaluate(`
    (() => {
      if (typeof IS_2XX !== 'function' || typeof IS_ERR !== 'function')
        return ['IS_2XX / IS_ERR are not reachable at module scope'];
      const cases = [
        ['200', true, false], ['201', true, false], ['204', true, false],
        ['299', true, false], ['400', false, true], ['429', false, true],
        ['504', false, true], ['300', false, true], ['2', false, true],
        ['disconnect', false, false], ['stall', false, true],
      ];
      const bad = [];
      for (const [s, ok, err] of cases) {
        if (IS_2XX(s) !== ok) bad.push(\`IS_2XX(\${JSON.stringify(s)}) = \${IS_2XX(s)}, expected \${ok}\`);
        if (IS_ERR(s) !== err) bad.push(\`IS_ERR(\${JSON.stringify(s)}) = \${IS_ERR(s)}, expected \${err}\`);
      }
      return bad;
    })()`);

  const inPage = await evaluate('JSON.stringify(window.__pageErrors || [])').then(JSON.parse);
  for (const e of inPage) {
    const first = (e.stack || '').split('\n').find((l) => /:\d+:\d+/.test(l)) || '';
    const m = first.match(/:(\d+):(\d+)/);
    errors.push({
      text: `${e.kind}: ${e.msg}`,
      line: e.line || (m ? Number(m[1]) : 0),
      col: e.col || (m ? Number(m[2]) : 0),
    });
  }

  let matrixObservations = null;
  if (allStates) {
    await browser.send('Target.closeTarget', { targetId });
    matrixObservations = await runInteractionMatrix({
      browser,
      configuredProxy: proxy,
      tmpdir,
    });
  }

  await cleanupRun();

  /* ---------- report ------------------------------------------------------ */

  console.log(IS_SETUP
    ? `drove ${TABS.length} wizard steps in ${PAGE_REL}`
    : `rendered ${TABS.length} tabs, hovered ${hovered.length} charts`);

  if (allStates) {
    const problems = interactionCoverageProblems(matrixObservations);
    if (problems.length) {
      console.error(`\nFAIL — ${problems.length} browser interaction row(s) disagreed`);
      for (const { check, detail } of problems) {
        console.error(`  [${check}] ${detail}`);
      }
      throw reportedFailure();
    }
    console.log(`interaction matrix ok — ${matrixObservations.size} named rows observed`);
  }

  if (untranslatedByTab.size) {
    const frozen = frozenTokens();
    // Exact match only. Substring matching against data values excludes real
    // labels: a "Mo" monogram swallowed the heatmap's "Mon", and any run
    // containing "rpm" swallowed "0 / 24 rpm · 3 keys", whose "keys" is ours.
    const isData = (t) => dataDerived.has(t) || [...dataDerived].some(value =>
      t.startsWith(value + ' ') && /^[0-9.,%]+$/.test(t.slice(value.length + 1)));
    // A run is correctly untranslated only if NOTHING of ours is left once the
    // frozen tokens, digits and punctuation are removed. "tok/s" goes; "24 rpm
    // available" stays, because "available" is a word we wrote.
    const isFrozen = (t) => {
      // This is the exact trio of already-localized output shapes from secs().
      // A catalog must not own dynamic numeric data.
      if (isIntlDurationRun(t)) return true;
      let rest = t;
      for (const f of [...frozen].sort((a, b) => b.length - a.length)) rest = rest.split(f).join(' ');
      return !/[a-zA-Z]{2,}/.test(rest);
    };
    const real = [...untranslatedByTab].filter(([t]) => !isFrozen(t) && !isData(t));
    if (process.env.DEBUG) {
      const dropped = [...untranslatedByTab].filter(([t]) => isFrozen(t) || isData(t));
      console.log('DEBUG excluded:', JSON.stringify(dropped.map(([t]) => t)));
    }
    const correct = untranslatedByTab.size - real.length;
    console.log(`\nuntranslated runs under ${localeArg}: ${real.length} actionable`
      + ` (${correct} correctly untranslated: frozen tokens and data from the API)`);
    for (const [text, tab] of real) console.log(`   [${tab}] ${JSON.stringify(text)}`);
    if (real.length) {
      console.error(`FAIL — ${real.length} repository-owned run(s) bypassed the catalog`);
      throw reportedFailure();
    }
  }

  if (predicateFailures.length) {
    console.error(`\nFAIL — ${predicateFailures.length} status-predicate disagreement(s)`);
    for (const f of predicateFailures) console.error('  ' + f);
    throw reportedFailure();
  }

  if (doubleEscaped.length) {
    const dbl = doubleEscaped.filter((d) => d.dir !== 'missing');
    const miss = doubleEscaped.filter((d) => d.dir === 'missing');
    console.error(`\nFAIL — contextual catalog sink violated: ${dbl.length} entity-escaped, ${miss.length} parsed as markup`);
    if (dbl.length) console.error('  a text/attribute sink rendered escaped entities:');
    const seen = new Set();
    for (const d of dbl) {
      if (seen.has(d.el)) continue;
      seen.add(d.el);
      console.error(`    .${d.el}  ${JSON.stringify(d.text)}`);
    }
    if (miss.length) console.error('  a catalog value reached a raw-HTML sink unescaped:');
    for (const d of miss) {
      if (seen.has(d.el)) continue;
      seen.add(d.el);
      console.error(`    .${d.el}  ${JSON.stringify(d.text)}`);
    }
    throw reportedFailure();
  }

  if (task9SemanticProblems.length) {
    console.error('\nFAIL — semantic DOM checks disagreed: ' + task9SemanticProblems.join(', '));
    throw reportedFailure();
  }

  if (task9LayoutProblems.length) {
    console.error('\nFAIL — layout DOM checks disagreed: ' + task9LayoutProblems.join(', '));
    throw reportedFailure();
  }

  if (layoutReport) {
    for (const evidence of layoutSurfaceEvidence) {
      console.log(`[layout-report] geometry=${evidence.context} visible=${evidence.visible} `
        + `root=${evidence.root?.scrollWidth}x${evidence.root?.clientWidth} `
        + `boundary=${evidence.boundary?.scrollWidth}x${evidence.boundary?.clientWidth}`);
    }
    for (const artifact of task9Artifacts) {
      if (artifact.report) console.log(`[layout-report] report=${artifact.report}`);
      else console.log(`[layout-report] state=${artifact.state} viewport=${artifact.viewport} path=${artifact.artifact} `
        + `scroll=${artifact.scrollWidth}x${artifact.scrollHeight} client=${artifact.clientWidth}x${artifact.clientHeight}`);
    }
  }

  if (task9LayoutMeasurements.length) {
    const layoutProblems = task9LayoutMeasurements.filter(measurement =>
      measurement.missing
      || measurement.scrollWidth > measurement.clientWidth
      || measurement.scrollHeight > measurement.clientHeight);
    if (layoutProblems.length) {
      console.error('\nFAIL — tool-call value does not fit without wrapping');
      for (const measurement of layoutProblems) {
        console.error(`  [layout-toolcall-${measurement.width}] ${measurement.text || 'missing'} `
          + `scroll=${measurement.scrollWidth}x${measurement.scrollHeight} `
          + `client=${measurement.clientWidth}x${measurement.clientHeight}`);
      }
      throw reportedFailure();
    }
  }

  if (errors.length || consoleErrors.length) {
    console.error(`\nFAIL — ${errors.length} uncaught page error(s), ${consoleErrors.length} console error(s)`);
    const seen = new Set();
    for (const e of errors) {
      const key = e.text.split('\n')[0] + ':' + (e.line - LINE_OFFSET);
      if (seen.has(key)) continue;
      seen.add(key);
      console.error(`  ${PAGE_REL}:${Math.max(1, e.line - LINE_OFFSET)}:${e.col}  ${e.text.split('\n')[0]}`);
    }
    for (const c of new Set(consoleErrors)) console.error('  console: ' + c);
    throw reportedFailure();
  }

  console.log('render ok — no uncaught page errors');
  } finally {
    await cleanupRun();
  }
}

if (args.includes('--pseudolocale-selftest')) {
  const failures = [];
  if (!isGeneratedFrozenP95('[p95 25.0 sec e]'))
    failures.push('generated p95 duration was not recognized');
  if (isGeneratedFrozenP95('[p95 arbitrary English e]'))
    failures.push('arbitrary bracketed English was accepted');
  for (const value of ['25 ms', '25.0 sec', '4 min'])
    if (!isIntlDurationRun(value)) failures.push(`Intl duration ${JSON.stringify(value)} was not recognized`);
  for (const value of ['25 sec', '25.00 sec', '25 min later'])
    if (isIntlDurationRun(value)) failures.push(`non-Intl duration ${JSON.stringify(value)} was accepted`);
  if (failures.length) {
    for (const failure of failures) console.error(`[pseudolocale] ${failure}`);
    process.exitCode = 1;
  } else {
    console.log('pseudolocale selftest ok — exact generated and Intl-format exemptions only');
  }
} else if (args.includes('--cleanup-selftest')) {
  cleanupSelftest().then(code => process.exit(code)).catch((e) => {
    console.error('cleanup selftest failed to run: ' + e.message);
    process.exit(2);
  });
} else {
  main().catch((e) => {
    if (!e.reported) console.error('render_check failed to run: ' + e.message);
    process.exitCode = e.exitCode || 2;
  });
}

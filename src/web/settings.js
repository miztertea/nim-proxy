/* ===== settings — config-driven, not metrics-driven: fetched from
   /api/config on tab entry and after every save (the server filters by role
   BEFORE serialization, so sections render purely from data presence). Only
   the lane-state chips ride a 5s in-place refresh. ===== */
let SET = null, setSub = 'access';

async function sPost(path, body) {
  const r = await fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
  let j = {}; try { j = await r.json(); } catch {}
  const apiError = typeof j.error === 'string'
    ? j.error
    : (j.error && typeof j.error.message === 'string' ? j.error.message : '');
  if (!r.ok || j.ok === false)
    throw new Error(apiError || message('settings.error.request_failed', { status: r.status }));
  return j;
}
/* inline feedback next to the control that caused it (textContent — never markup) */
const note = (id, msg, ok) => { const el = $(id); if (el) { el.textContent = msg || ''; el.classList.toggle('ok', !!ok); } };
const noteMessage = (id, messageId, params = {}, ok = false) => {
  const el = $(id);
  if (!el) return;
  setMessageText(el, messageId, params);
  el.classList.toggle('ok', ok);
};

async function loadSettings() {
  try {
    const j = await (await fetch('/api/config')).json();
    if (!j.nim_keys) throw 0;
    SET = j;
  } catch { $('setbody').innerHTML = `<div class="empty">${escapeHtml(catalogMessage('settings.error.load'))}</div>`; return; }
  renderSettings();
}

const TRASH = '<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M2.5 4h11M6.5 4V2.5h3V4M4 4l.8 9.5h6.4L12 4M6.5 7v4M9.5 7v4"/></svg>';
const LOCK = '<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="3.5" y="7" width="9" height="6.5" rx="1.5"/><path d="M5.5 7V5a2.5 2.5 0 015 0v2"/></svg>';
const SICONS = {
  access: '<svg viewBox="0 0 16 16"><circle cx="11" cy="5" r="2.5"/><path d="M9.2 6.8L2.5 13.5M4.8 11.2l1.5 1.5"/></svg>',
  server: '<svg viewBox="0 0 16 16"><rect x="2" y="2.5" width="12" height="4.5" rx="1.5"/><rect x="2" y="9" width="12" height="4.5" rx="1.5"/><path d="M4.6 4.75h.01M4.6 11.25h.01"/></svg>',
  users: '<svg viewBox="0 0 16 16"><circle cx="6" cy="5.5" r="2.2"/><path d="M2 13.5c.5-2.4 2-3.6 4-3.6s3.5 1.2 4 3.6M10.4 3.6a2.2 2.2 0 010 3.8M11.8 10c1.3.5 2 1.6 2.3 3.5"/></svg>',
  account: '<svg viewBox="0 0 16 16"><circle cx="8" cy="5" r="2.5"/><path d="M3 14c.6-2.8 2.4-4.2 5-4.2s4.4 1.4 5 4.2"/></svg>',
};

function renderSettings() {
  const subs = [['access', 'settings.nav.access'], ['server', 'settings.nav.server'], ['users', 'settings.nav.users'], ['account', 'settings.nav.account']]
    .filter(([id]) => id === 'access' || id === 'account' || (id === 'server' ? !!SET.server : !!SET.users));
  if (!subs.some(([id]) => id === setSub)) setSub = 'access';
  $('setnav').innerHTML = subs.map(([id, label]) =>
    `<button data-sub="${id}"${id === setSub ? ' aria-current="page"' : ''}><span class="nbar"></span>${SICONS[id]}<span data-i18n="${label}"></span></button>`).join('');
  applyStatic($('setnav'));
  for (const b of $('setnav').querySelectorAll('button'))
    b.addEventListener('click', () => { setSub = b.dataset.sub; renderSettings(); });
  ({ access: renderAccess, server: renderServer, users: renderUsers, account: renderAccount })[setSub]();
}

/* live lane state for one key row (also patched in place by the 5s refresh) */
function keyState(k) {
  if (!k.enabled) return { id: 'settings.key.state.disabled', cls: 'kstate dim' };
  if (k.cooldown_ms > 0) return { id: 'settings.key.state.cooldown', params: { seconds: NUM_GROUPED.format(Math.ceil(k.cooldown_ms / 1000)) }, cls: 'kstate warnc' };
  if (k.in_window != null) return { id: 'settings.key.state.in_window', params: { in_window: NUM_GROUPED.format(+k.in_window), rpm: NUM_GROUPED.format(+k.rpm) }, cls: 'kstate' };
  return { id: 'settings.key.state.unassigned', cls: 'kstate dim' };
}
const clampInt = (v, lo, hi, dflt) => { const n = Math.round(+v); return isFinite(n) ? Math.max(lo, Math.min(hi, n)) : dflt; };

async function copyText(text, btn) {
  const old = btn.textContent;
  try { await navigator.clipboard.writeText(text); setMessageText(btn, 'settings.copy.copied'); }
  catch { setMessageText(btn, 'settings.copy.failed'); }
  setTimeout(() => { btn.textContent = old; }, 1500);
}

/* show-once client-key secret: modal only closes on an explicit Done, so a
   stray re-render can't eat the one chance to copy it */
function showSecret(name, secret) {
  const modal = $('modal');
  $('modal-body').innerHTML =
    `<p data-style="margin:0 0 12px;color:var(--ink-2);font-size:13px">${escapeHtml(catalogMessage('settings.secret.ready', { name }))} <b data-style="color:var(--amber-lt)" data-i18n="settings.secret.warning"></b></p>
    <div class="secretbox">${escapeHtml(secret)}</div>
    <div data-style="display:flex;gap:10px;justify-content:flex-end;margin-top:16px">
      <button class="pbtn" id="modal-copy" data-i18n="settings.secret.copy"></button>
      <button class="gbtn" id="modal-done" data-i18n="settings.common.done"></button>
    </div>`;
  applyStatic($('modal-body'));
  $('modal-copy').addEventListener('click', () => copyText(secret, $('modal-copy')));
  modal.addEventListener('close', () => $('ck-add')?.focus(), { once: true });
  $('modal-done').addEventListener('click', () => modal.close());
  modal.showModal();
  $('modal-copy').focus();
}

function renderAccess() {
  const admin = !!SET.users;
  const ownerChip = o => admin ? ` <span class="tag">${escapeHtml(o)}</span>` : '';
  const validatedModelsId = n => {
    switch (PLURALS.select(n)) {
      case 'zero': return 'settings.validation.models_added.zero';
      case 'one': return 'settings.validation.models_added.one';
      case 'two': return 'settings.validation.models_added.two';
      case 'few': return 'settings.validation.models_added.few';
      case 'many': return 'settings.validation.models_added.many';
      default: return 'settings.validation.models_added.other';
    }
  };
  const keyRows = SET.nim_keys.map((k, i) => {
    const st = keyState(k);
    return `<div class="krow${k.enabled ? '' : ' koff'}">
      <div data-style="min-width:0">
        <div class="kmask">nvapi-••••${escapeHtml(k.last4)}${ownerChip(k.owner)}</div>
        <div class="kmeta">fp ${escapeHtml(String(k.fingerprint).slice(0, 8))} · ${k.lane != null ? escapeHtml(catalogMessage('settings.key.slot', { n: NUM_GROUPED.format(+k.lane + 1) })) : k.enabled ? escapeHtml(catalogMessage('settings.key.state.unassigned')) : escapeHtml(catalogMessage('settings.key.off'))}</div>
      </div>
      <span class="${st.cls}" data-ksfp="${escapeHtml(k.fingerprint)}">${escapeHtml(catalogMessage(st.id, st.params))}</span>
      <span class="rpmwrap"><input class="sin num" type="number" min="1" max="10000" value="${+k.rpm}" data-rpm="${i}" data-i18n-attr="aria-label:settings.key.rpm"><span class="unitl">rpm</span></span>
      <button class="tog" type="button" aria-pressed="${!!k.enabled}" data-tog="${i}" data-i18n-attr="title:${k.enabled ? 'settings.key.toggle.disable' : 'settings.key.toggle.enable'},aria-label:${k.enabled ? 'settings.key.toggle.disable' : 'settings.key.toggle.enable'}"></button>
      ${k.guarded
        ? `<span class="klock" data-i18n-attr="title:settings.key.guarded">${LOCK}</span>`
        : `<button class="dbtn icon" data-kdel="${i}" data-i18n-attr="title:settings.key.remove">${TRASH}</button>`}
    </div>`;
  }).join('');
  const ckRows = SET.client_keys.map((ck, i) =>
    `<div class="krow">
      <span data-style="font-weight:600;flex:0 1 160px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="${escapeHtml(ck.name)}">${escapeHtml(ck.name)}</span>
      <span class="kmask" data-style="color:var(--ink-25);font-weight:500">npk_••••••••${escapeHtml(ck.last4)}</span>
      ${admin ? `<span class="tag">${escapeHtml(ck.owner)}</span>` : ''}
      <button class="dbtn" data-style="margin-left:auto" data-ckdel="${i}" data-i18n="settings.client_key.revoke"></button>
    </div>`).join('');
  $('setbody').innerHTML = `
    <div class="card mb">
      <h2><span data-i18n="${admin ? 'settings.key.heading.all' : 'settings.key.heading.mine'}"></span> <span class="note" id="pool-note">${escapeHtml(catalogMessage('settings.key.pool_note', { enabled: NUM_GROUPED.format(+SET.pool.enabled), capacity: NUM_GROUPED.format(+SET.pool.capacity_rpm) }))}</span></h2>
      <p class="shint" data-i18n="${admin ? 'settings.key.notice.admin' : 'settings.key.notice.mine'}"></p>
      <div>${keyRows || '<div class="empty" data-i18n="settings.key.empty"></div>'}</div>
      <div class="addrow">
        <input id="nk-key" class="sin" data-style="flex:1;min-width:200px" type="password" data-i18n-attr="placeholder:settings.key.placeholder,aria-label:settings.key.placeholder" autocomplete="off" spellcheck="false">
        <span class="rpmwrap"><input id="nk-rpm" class="sin num" type="number" min="1" max="10000" value="40" data-i18n-attr="aria-label:settings.key.rpm"><span class="unitl">rpm</span></span>
        <button class="pbtn" id="nk-add" data-i18n="settings.key.validate_add"></button>
        <button class="gbtn" id="nk-force" hidden data-i18n="settings.key.add_anyway"></button>
      </div>
      <div class="serr" id="nk-err"></div>
    </div>
    <div class="card mb">
      <h2><span data-i18n="${admin ? 'settings.client_key.heading.all' : 'settings.client_key.heading.mine'}"></span> <span class="note" data-i18n="settings.client_key.note"></span></h2>
      <div>${ckRows || '<div class="empty" data-i18n="settings.client_key.empty"></div>'}</div>
      <div class="addrow">
        <input id="ck-name" class="sin" data-style="flex:1;min-width:200px" data-i18n-attr="placeholder:settings.client_key.placeholder,aria-label:settings.client_key.placeholder" maxlength="64" autocomplete="off" spellcheck="false">
        <button class="pbtn" id="ck-add" data-i18n="settings.client_key.generate"></button>
      </div>
      <div class="serr" id="ck-err"></div>
    </div>
    <div class="card">
      <h2 data-i18n="settings.connection.heading"></h2>
      <div class="congrid">
        <div class="conbox">
          <div class="slabel" data-i18n="settings.connection.base_url"></div>
          <div data-style="display:flex;align-items:center;gap:10px"><span class="kmask" data-style="flex:1;overflow:hidden;text-overflow:ellipsis">${escapeHtml(location.origin + '/v1')}</span><button class="gbtn" id="copy-base" data-i18n="settings.copy.copy"></button></div>
        </div>
        <div class="conbox">
          <div class="slabel" data-i18n="settings.mode.heading"></div>
          <div class="kmask" data-style="color:${SET.mode === 'keyed' ? 'var(--green-lt)' : 'var(--amber-lt)'}">● <span data-i18n="${SET.mode === 'keyed' ? 'settings.mode.keyed' : 'settings.mode.open'}"></span></div>
          <div class="kmeta" data-style="margin-top:4px" data-i18n="${SET.mode === 'keyed' ? 'settings.mode.keyed_note' : 'settings.mode.open_note'}"></div>
        </div>
      </div>
    </div>`;
  const body = $('setbody');
  applyStatic(body);
  for (const el of body.querySelectorAll('[data-rpm]')) el.addEventListener('change', async () => {
    const k = SET.nim_keys[+el.dataset.rpm];
    try {
      await sPost('/api/settings/nim-keys', { set: { fingerprint: k.fingerprint, rpm: clampInt(el.value, 1, 10000, +k.rpm) } });
      await loadSettings();
    } catch (e) { note('nk-err', e.message); }
  });
  for (const el of body.querySelectorAll('[data-tog]')) el.addEventListener('click', async () => {
    const k = SET.nim_keys[+el.dataset.tog];
    try {
      await sPost('/api/settings/nim-keys', { set: { fingerprint: k.fingerprint, enabled: !k.enabled } });
      await loadSettings();
    } catch (e) { note('nk-err', e.message); }
  });
  for (const el of body.querySelectorAll('[data-kdel]')) el.addEventListener('click', async () => {
    const k = SET.nim_keys[+el.dataset.kdel];
    if (!confirmMessage('settings.dialog.remove_key', { last4: k.last4 })) return;
    try { await sPost('/api/settings/nim-keys', { remove: k.fingerprint }); await loadSettings(); }
    catch (e) { note('nk-err', e.message); }
  });
  /* validate first; "Add anyway" appears only after a failed probe */
  const addKey = async () => {
    await sPost('/api/settings/nim-keys', { add: { key: $('nk-key').value.trim(), rpm: clampInt($('nk-rpm').value, 1, 10000, 40) } });
    await loadSettings();
  };
  $('nk-add').addEventListener('click', async () => {
    const key = $('nk-key').value.trim();
    if (!key) return noteMessage('nk-err', 'settings.validation.nim_key_required');
    $('nk-add').disabled = true; setMessageText($('nk-add'), 'settings.key.validating');
    try {
      const v = await sPost('/api/settings/validate-key', { key });
      const count = Array.isArray(v.models) ? v.models.length : +v.models;
      noteMessage('nk-err', validatedModelsId(count), { n: NUM_GROUPED.format(count) }, true);
      await addKey();
      return;
    } catch (e) {
      note('nk-err', e.message);
      $('nk-force').hidden = false;
    }
    $('nk-add').disabled = false; setMessageText($('nk-add'), 'settings.key.validate_add');
  });
  $('nk-force').addEventListener('click', async () => {
    if (!$('nk-key').value.trim()) return noteMessage('nk-err', 'settings.validation.nim_key_required');
    try { await addKey(); } catch (e) { note('nk-err', e.message); }
  });
  for (const el of body.querySelectorAll('[data-ckdel]')) el.addEventListener('click', async () => {
    const ck = SET.client_keys[+el.dataset.ckdel];
    if (!confirmMessage('settings.dialog.revoke_client_key', { name: ck.name })) return;
    try { await sPost('/api/settings/clients', { remove: ck.name }); await loadSettings(); }
    catch (e) { note('ck-err', e.message); }
  });
  $('ck-add').addEventListener('click', async () => {
    const name = $('ck-name').value.trim();
    if (!name) return noteMessage('ck-err', 'settings.validation.client_key_name');
    try {
      const j = await sPost('/api/settings/clients', { add: { name } });
      showSecret(name, j.secret);
      await loadSettings();
    } catch (e) { note('ck-err', e.message); }
  });
  $('copy-base').addEventListener('click', () => copyText(location.origin + '/v1', $('copy-base')));
}

function renderServer() {
  const sv = SET.server, L = sv.limits;
  const num = (id, label, val, unit, step) => `<div><label class="slabel" for="${id}" data-i18n="${label}"></label>
    <span class="rpmwrap" data-style="display:flex"><input id="${id}" class="sin" data-style="width:100%;text-align:right" type="number" min="0"${step ? ` step="${step}"` : ''} value="${+val}">${unit ? `<span class="unitl" data-i18n="${unit}"></span>` : ''}</span></div>`;
  const ovr = Object.entries(sv.governor.overrides || {});
  const retainedFrom = sv.history.available_from == null
    ? escapeHtml(catalogMessage('settings.server.history.no_data'))
    : escapeHtml(at(STAMP, +sv.history.available_from * 1000));
  const historyBytes = +sv.history.file_bytes || 0;
  const historyBytesMessage = () => {
    const params = { bytes: NUM_GROUPED.format(historyBytes) };
    switch (PLURALS.select(historyBytes)) {
      case 'zero': return catalogMessage('settings.server.history.bytes.zero', params);
      case 'one': return catalogMessage('settings.server.history.bytes.one', params);
      case 'two': return catalogMessage('settings.server.history.bytes.two', params);
      case 'few': return catalogMessage('settings.server.history.bytes.few', params);
      case 'many': return catalogMessage('settings.server.history.bytes.many', params);
      default: return catalogMessage('settings.server.history.bytes.other', params);
    }
  };
  $('setbody').innerHTML = `
    <div class="card mb">
      <h2><span data-i18n="settings.mode.heading"></span>
        <span class="pills" data-style="margin-left:auto">
          <button data-mode="open" aria-pressed="${SET.mode === 'open'}" data-i18n="settings.mode.open"></button>
          <button data-mode="keyed" aria-pressed="${SET.mode === 'keyed'}" data-i18n="settings.mode.keyed"></button>
        </span></h2>
      <p class="shint" data-i18n="settings.mode.help"></p>
      ${SET.mode === 'keyed' && !SET.client_keys.length ? '<p class="shint" data-style="color:var(--amber-lt)" data-i18n="settings.mode.no_client_keys"></p>' : ''}
      <div class="serr" id="mode-err"></div>
    </div>
    <div class="card mb">
      <h2><span data-i18n="settings.server.upstream_limits"></span> <button class="pbtn" id="save-limits" data-style="margin-left:auto" data-i18n="settings.common.save"></button></h2>
      <div class="slabel" data-i18n="settings.server.base_url_note"></div>
      <input id="sv-base" class="sin" data-style="width:100%" value="${escapeHtml(sv.base_url)}" data-i18n-attr="aria-label:settings.server.base_url" spellcheck="false">
      <div class="limgrid">
        ${num('sv-maxwait', 'settings.server.limit.max_wait', L.max_wait_secs, 'settings.server.unit.seconds')}
        ${num('sv-heartbeat', 'settings.server.limit.heartbeat', L.heartbeat_secs, 'settings.server.unit.seconds')}
        ${num('sv-idle', 'settings.server.limit.stream_idle', L.stream_idle_secs, 'settings.server.unit.seconds')}
        ${num('sv-timeout', 'settings.server.limit.request_timeout', L.request_timeout_secs, 'settings.server.unit.seconds')}
        ${num('sv-ttl', 'settings.server.limit.models_ttl', L.models_ttl_secs, 'settings.server.unit.seconds')}
        ${num('sv-inflight', 'settings.server.limit.max_inflight', L.max_inflight, '')}
      </div>
      <div data-style="display:flex;align-items:center;gap:10px;margin-top:16px">
        <button class="tog" type="button" id="sv-strict" aria-pressed="${!!L.strict_passthrough}" data-i18n-attr="aria-label:settings.server.strict_passthrough"></button>
        <span data-style="font-size:12.5px"><span data-i18n="settings.server.strict_passthrough"></span> <span class="kmeta" data-style="display:inline" data-i18n="settings.server.strict_note"></span></span>
      </div>
      <div class="serr" id="limits-err"></div>
    </div>
    <div class="card mb">
      <h2><span data-i18n="settings.server.model_limits"></span>
        <span data-style="margin-left:auto;display:inline-flex;align-items:center;gap:8px"><span class="kmeta" data-i18n="settings.server.adaptive"></span>
        <button class="tog" type="button" id="gov-tog" aria-pressed="${!!sv.governor.enabled}" data-i18n-attr="aria-label:settings.server.adaptive"></button></span></h2>
      <p class="shint" data-i18n="settings.server.model_limits_help"></p>
      <div>${ovr.map(([m, cap], i) =>
        `<span class="ochip"><span title="${escapeHtml(m)}">${escapeHtml(m)}</span><b>${+cap} <span data-i18n="settings.server.unit.limit"></span></b><button data-govdel="${i}" data-i18n-attr="title:settings.server.remove_override">×</button></span>`).join('')
        || '<span class="kmeta" data-i18n="settings.server.no_overrides"></span>'}</div>
      <div class="addrow">
        <input id="gov-model" class="sin" data-style="flex:1;min-width:200px" data-i18n-attr="placeholder:settings.server.model_placeholder,aria-label:settings.server.model_placeholder" autocomplete="off" spellcheck="false">
        <span class="rpmwrap"><input id="gov-cap" class="sin num" type="number" min="1" max="10000" value="8" data-i18n-attr="aria-label:settings.server.model_concurrency"><span class="unitl" data-i18n="settings.server.unit.limit"></span></span>
        <button class="gbtn" id="gov-add" data-i18n="settings.server.add_override"></button>
      </div>
      <div class="serr" id="gov-err"></div>
    </div>
    <div class="card">
      <h2><span data-i18n="settings.server.history.heading"></span> <button class="pbtn" id="save-history" data-style="margin-left:auto" data-i18n="settings.common.save"></button></h2>
      <div class="limgrid" data-style="margin-top:6px">
        <div><label class="slabel" for="sv-default-days" data-i18n="settings.server.history.default_range"></label>
          <span class="rpmwrap" data-style="display:flex"><input id="sv-default-days" class="sin" data-style="width:100%;text-align:right" type="number" min="1" step="1" value="${+sv.dashboard.default_window_days}"><span class="unitl" data-i18n="settings.server.days"></span></span></div>
        <div><label class="slabel" for="sv-retention-days" data-i18n="settings.server.history.retention"></label>
          <span class="rpmwrap" data-style="display:flex"><input id="sv-retention-days" class="sin" data-style="width:100%;text-align:right" type="number" min="0" step="1" value="${+sv.history.days}"><span class="unitl" data-i18n="settings.server.days"></span></span></div>
        <div><label class="slabel" for="sv-slo" data-i18n="settings.server.history.slo"></label>
          <span class="rpmwrap" data-style="display:flex"><input id="sv-slo" class="sin" data-style="width:100%;text-align:right" type="number" min="0.1" max="100" step="0.1" value="${+sv.dashboard.slo_target_percent}"><span class="unitl">%</span></span></div>
      </div>
      <div class="congrid" data-style="margin-top:16px">
        <div class="conbox"><div class="slabel" data-i18n="settings.server.history.oldest"></div><div class="kmeta" data-style="display:block">${retainedFrom}</div></div>
        <div class="conbox"><div class="slabel" data-i18n="settings.server.history.data_file"></div><div class="kmeta" data-style="display:block">${escapeHtml(historyBytesMessage())}</div></div>
      </div>
      ${sv.history.compaction_pending ? '<p class="shint" data-style="color:var(--amber-lt)" data-i18n="settings.server.history.compaction_pending"></p>' : ''}
      ${sv.history.persistence === 'degraded' ? '<p class="shint" data-style="color:var(--amber-lt)" data-i18n="settings.server.history.persistence_degraded"></p>' : ''}
      <div class="serr" id="history-err"></div>
    </div>`;
  applyStatic($('setbody'));
  for (const b of $('setbody').querySelectorAll('[data-mode]')) b.addEventListener('click', async () => {
    if (b.dataset.mode === SET.mode) return;
    try { await sPost('/api/settings/clients', { mode: b.dataset.mode }); await loadSettings(); }
    catch (e) { note('mode-err', e.message); }
  });
  $('sv-strict').addEventListener('click', () =>
    $('sv-strict').setAttribute('aria-pressed', $('sv-strict').getAttribute('aria-pressed') !== 'true'));
  $('save-limits').addEventListener('click', async () => {
    const base = $('sv-base').value.trim();
    const nums = {
      max_wait_secs: 'sv-maxwait', heartbeat_secs: 'sv-heartbeat', models_ttl_secs: 'sv-ttl',
      stream_idle_secs: 'sv-idle', request_timeout_secs: 'sv-timeout', max_inflight: 'sv-inflight',
    };
    const limits = { strict_passthrough: $('sv-strict').getAttribute('aria-pressed') === 'true' };
    for (const [field, id] of Object.entries(nums)) {
      const n = Math.round(+$(id).value);
      if (!isFinite(n) || n < 0) return noteMessage('limits-err', 'settings.validation.limits');
      limits[field] = n;
    }
    try {
      if (base !== sv.base_url) await sPost('/api/settings/upstream', { base_url: base });
      await sPost('/api/settings/limits', limits);
      await loadSettings();
      noteMessage('limits-err', 'settings.validation.saved', {}, true);
    } catch (e) { note('limits-err', e.message); }
  });
  $('gov-tog').addEventListener('click', async () => {
    try { await sPost('/api/settings/governor', { enabled: !sv.governor.enabled }); await loadSettings(); }
    catch (e) { note('gov-err', e.message); }
  });
  for (const b of $('setbody').querySelectorAll('[data-govdel]')) b.addEventListener('click', async () => {
    try { await sPost('/api/settings/governor', { remove_override: ovr[+b.dataset.govdel][0] }); await loadSettings(); }
    catch (e) { note('gov-err', e.message); }
  });
  $('gov-add').addEventListener('click', async () => {
    const model = $('gov-model').value.trim(), cap = Math.round(+$('gov-cap').value);
    if (!model || !(cap >= 1)) return noteMessage('gov-err', 'settings.validation.governor');
    try { await sPost('/api/settings/governor', { set_override: { model, cap } }); await loadSettings(); }
    catch (e) { note('gov-err', e.message); }
  });
  $('save-history').addEventListener('click', async () => {
    const defaultDays = Math.round(+$('sv-default-days').value);
    const retention = Math.round(+$('sv-retention-days').value);
    const slo = +$('sv-slo').value;
    if (!isFinite(defaultDays) || defaultDays < 1)
      return noteMessage('history-err', 'settings.validation.history_default');
    if (!isFinite(retention) || retention < 0)
      return noteMessage('history-err', 'settings.validation.history_retention');
    if (!isFinite(slo) || slo <= 0 || slo > 100)
      return noteMessage('history-err', 'settings.validation.slo');
    try {
      await sPost('/api/settings/history', {
        days: retention,
        default_window_days: defaultDays,
        slo_target_percent: slo,
      });
      await loadSettings();
      noteMessage('history-err', 'settings.validation.saved', {}, true);
    } catch (e) { note('history-err', e.message); }
  });
}

function renderUsers() {
  const RCLS = { superuser: 'superuser', admin: 'admin', user: 'user' };
  const nimKeyCount = n => {
    const params = { n: NUM_GROUPED.format(n) };
    switch (PLURALS.select(n)) {
      case 'zero': return catalogMessage('settings.users.nim_key_count.zero', params);
      case 'one': return catalogMessage('settings.users.nim_key_count.one', params);
      case 'two': return catalogMessage('settings.users.nim_key_count.two', params);
      case 'few': return catalogMessage('settings.users.nim_key_count.few', params);
      case 'many': return catalogMessage('settings.users.nim_key_count.many', params);
      default: return catalogMessage('settings.users.nim_key_count.other', params);
    }
  };
  const clientKeyCount = n => {
    const params = { n: NUM_GROUPED.format(n) };
    switch (PLURALS.select(n)) {
      case 'zero': return catalogMessage('settings.users.client_key_count.zero', params);
      case 'one': return catalogMessage('settings.users.client_key_count.one', params);
      case 'two': return catalogMessage('settings.users.client_key_count.two', params);
      case 'few': return catalogMessage('settings.users.client_key_count.few', params);
      case 'many': return catalogMessage('settings.users.client_key_count.many', params);
      default: return catalogMessage('settings.users.client_key_count.other', params);
    }
  };
  const rows = SET.users.map((u, i) => `<div class="krow">
    <span class="umono">${escapeHtml((u.username[0] || '?').toUpperCase())}</span>
    <div data-style="min-width:0">
      <div data-style="font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${escapeHtml(u.username)}</div>
      <div class="kmeta">${escapeHtml(nimKeyCount(+u.nim_keys))} · ${escapeHtml(clientKeyCount(+u.client_keys))}</div>
    </div>
    <span class="rbadge ${RCLS[u.role] || 'user'}" data-style="margin-left:auto">${escapeHtml(String(u.role))}</span>
    ${u.role !== 'superuser' ? `<button class="gbtn" data-urp="${i}" data-i18n="settings.users.reset_password"></button>
      <select class="sin" data-urole="${i}" data-i18n-attr="title:settings.users.role,aria-label:settings.users.role">
        <option value="admin"${u.role === 'admin' ? ' selected' : ''}>admin</option>
        <option value="user"${u.role === 'user' ? ' selected' : ''}>user</option>
      </select>
      <button class="dbtn" data-udel="${i}" data-i18n="settings.users.delete"></button>`
      : `<span class="note" data-i18n-attr="title:settings.users.superuser_note" data-i18n="settings.users.self_managed"></span>`}
  </div>`).join('');
  $('setbody').innerHTML = `<div class="card">
    <h2><span data-i18n="settings.users.heading"></span> <span class="note" data-i18n="settings.users.note"></span></h2>
    <div>${rows}</div>
    <div class="addrow">
      <input id="u-name" class="sin" data-style="flex:1;min-width:150px" data-i18n-attr="placeholder:settings.users.username_placeholder,aria-label:settings.users.username_placeholder" autocomplete="off" spellcheck="false">
      <input id="u-pass" class="sin" data-style="flex:1;min-width:150px" type="password" data-i18n-attr="placeholder:settings.users.password_placeholder,aria-label:settings.users.password_placeholder" autocomplete="new-password">
      <span class="pills"><button data-urolepick="user" aria-pressed="true">user</button><button data-urolepick="admin" aria-pressed="false">admin</button></span>
      <button class="pbtn" id="u-add" data-i18n="settings.users.add"></button>
    </div>
    <div class="serr" id="u-err"></div>
  </div>`;
  const body = $('setbody');
  applyStatic(body);
  for (const b of body.querySelectorAll('[data-urp]')) b.addEventListener('click', async () => {
    const u = SET.users[+b.dataset.urp];
    const pw = promptMessage('settings.dialog.reset_password', { username: u.username });
    if (pw == null) return;
    if (pw.length < 10) return noteMessage('u-err', 'settings.validation.password');
    try {
      await sPost('/api/settings/users', { reset_password: { username: u.username, new_password: pw } });
      noteMessage('u-err', 'settings.validation.password_reset', { username: u.username }, true);
    } catch (e) { note('u-err', e.message); }
  });
  for (const s of body.querySelectorAll('[data-urole]')) s.addEventListener('change', async () => {
    const u = SET.users[+s.dataset.urole];
    try { await sPost('/api/settings/users', { set_role: { username: u.username, role: s.value } }); await loadSettings(); }
    catch (e) { note('u-err', e.message); }
  });
  for (const b of body.querySelectorAll('[data-udel]')) b.addEventListener('click', async () => {
    const u = SET.users[+b.dataset.udel];
    if (!confirmMessage('settings.dialog.delete_user', { username: u.username })) return;
    try { await sPost('/api/settings/users', { remove: u.username }); await loadSettings(); }
    catch (e) { note('u-err', e.message); }
  });
  let newRole = 'user';
  for (const b of body.querySelectorAll('[data-urolepick]')) b.addEventListener('click', () => {
    newRole = b.dataset.urolepick;
    for (const o of body.querySelectorAll('[data-urolepick]')) o.setAttribute('aria-pressed', o === b);
  });
  $('u-add').addEventListener('click', async () => {
    const username = $('u-name').value.trim(), password = $('u-pass').value;
    if (!username) return noteMessage('u-err', 'settings.validation.username');
    if (password.length < 10) return noteMessage('u-err', 'settings.validation.initial_password');
    try { await sPost('/api/settings/users', { add: { username, password, role: newRole } }); await loadSettings(); }
    catch (e) { note('u-err', e.message); }
  });
}

function renderAccount() {
  $('setbody').innerHTML = `<div class="card" data-style="max-width:560px">
    <h2 data-i18n="settings.account.heading"></h2>
    <div class="slabel" data-i18n="settings.account.username"></div>
    <input class="sin" data-style="width:100%" value="${escapeHtml(SET.username)}" data-i18n-attr="aria-label:settings.account.username" disabled>
    <div class="slabel" data-style="margin-top:14px" data-i18n="settings.account.current_password"></div>
    <input id="a-cur" class="sin" data-style="width:100%" type="password" data-i18n-attr="aria-label:settings.account.current_password" autocomplete="current-password">
    <div class="slabel" data-style="margin-top:14px" data-i18n="settings.account.new_password"></div>
    <input id="a-new" class="sin" data-style="width:100%" type="password" data-i18n-attr="aria-label:settings.account.new_password" autocomplete="new-password">
    <div class="slabel" data-style="margin-top:14px" data-i18n="settings.account.confirm_password"></div>
    <input id="a-conf" class="sin" data-style="width:100%" type="password" data-i18n-attr="aria-label:settings.account.confirm_password" autocomplete="new-password">
    <p class="shint" data-style="margin-top:14px" data-i18n="settings.account.note"></p>
    <button class="pbtn" id="a-save" data-i18n="settings.account.update"></button>
    <div class="serr" id="a-err"></div>
  </div>`;
  applyStatic($('setbody'));
  $('a-save').addEventListener('click', async () => {
    const nw = $('a-new').value;
    if (nw.length < 10) return noteMessage('a-err', 'settings.validation.new_password');
    if (nw !== $('a-conf').value) return noteMessage('a-err', 'settings.validation.password_mismatch');
    $('a-save').disabled = true; // password hashing is deliberately slow
    try {
      await sPost('/api/settings/account', { current_password: $('a-cur').value, new_password: nw });
      for (const id of ['a-cur', 'a-new', 'a-conf']) $(id).value = '';
      noteMessage('a-err', 'settings.validation.password_updated', {}, true);
    } catch (e) { note('a-err', e.message); }
    $('a-save').disabled = false;
  });
}

/* keep the lane-state chips honest while the page sits on Access & keys —
   patch text in place so inputs and focus survive */
setInterval(async () => {
  if (activeTab !== 'settings' || setSub !== 'access' || !SET) return;
  try {
    const j = await (await fetch('/api/config')).json();
    if (!j.nim_keys) return;
    SET.pool = j.pool;
    const byFp = new Map(j.nim_keys.map(k => [k.fingerprint, k]));
    for (const el of document.querySelectorAll('[data-ksfp]')) {
      const k = byFp.get(el.dataset.ksfp);
      if (!k) continue;
      const st = keyState(k);
      setMessageText(el, st.id, st.params);
      el.className = st.cls;
    }
    const pn = $('pool-note');
    if (pn) setMessageText(pn, 'settings.key.pool_note', { enabled: NUM_GROUPED.format(+SET.pool.enabled), capacity: NUM_GROUPED.format(+SET.pool.capacity_rpm) });
  } catch {}
}, 5000);

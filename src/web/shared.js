"use strict";
const $ = id => document.getElementById(id);
const css = v => getComputedStyle(document.documentElement).getPropertyValue(v).trim();
/* Generated charts need numeric geometry and fixed palette values. Keep those
   values out of serialized style attributes: inert data is promoted through a
   narrow CSSOM allowlist after the node enters the document. */
const DYNAMIC_STYLE_PROPERTIES = new Set([
  'align-items', 'background', 'border-radius', 'border-top', 'color', 'display',
  'flex', 'font', 'font-size', 'font-weight', 'gap', 'height', 'inset',
  'justify-content', 'left', 'letter-spacing', 'margin', 'margin-bottom',
  'margin-left', 'margin-top', 'max-height', 'max-width', 'min-width',
  'overflow', 'padding', 'padding-top', 'position', 'text-align',
  'text-overflow', 'white-space', 'width',
]);
const MAX_DYNAMIC_STYLE_RULES = 512;
const dynamicStyleClasses = new Map();
const dynamicStyleKeysByClass = new Map();
let dynamicStyleSequence = 0;
let dynamicStyleCompactions = 0;
function operatorStyleSheet() {
  const sheet = [...document.styleSheets].find(candidate =>
    candidate.href?.endsWith('/assets/operator/operator.css'));
  if (!sheet) throw new Error('operator stylesheet is unavailable');
  return sheet;
}
function insertDynamicStyle(key) {
  if (dynamicStyleClasses.size >= MAX_DYNAMIC_STYLE_RULES)
    throw new Error('dynamic style live set exceeds its fixed bound');
  const name = `dynamic-style-${++dynamicStyleSequence}`;
  const sheet = operatorStyleSheet();
  sheet.insertRule(`.${name}{${key}}`, sheet.cssRules.length);
  dynamicStyleClasses.set(key, name);
  dynamicStyleKeysByClass.set(name, key);
  return name;
}
function compactDynamicStyles() {
  const live = new Map();
  for (const node of document.querySelectorAll('[class*="dynamic-style-"]')) {
    for (const name of [...node.classList].filter(value => value.startsWith('dynamic-style-'))) {
      const key = dynamicStyleKeysByClass.get(name);
      if (!key) continue;
      if (!live.has(key)) live.set(key, []);
      live.get(key).push(node);
    }
  }
  // The pending declaration also needs a slot. Refuse it without touching the
  // existing sheet when every bounded slot is still owned by live DOM.
  if (live.size >= MAX_DYNAMIC_STYLE_RULES)
    throw new Error('dynamic style live set exceeds its fixed bound');
  dynamicStyleCompactions++;
  const sheet = operatorStyleSheet();
  for (let i = sheet.cssRules.length - 1; i >= 0; i--) {
    if (sheet.cssRules[i].selectorText?.startsWith('.dynamic-style-')) sheet.deleteRule(i);
  }
  dynamicStyleClasses.clear();
  dynamicStyleKeysByClass.clear();
  dynamicStyleSequence = 0;
  for (const [key, nodes] of live) {
    const name = insertDynamicStyle(key);
    for (const node of nodes) {
      for (const old of [...node.classList].filter(value => value.startsWith('dynamic-style-')))
        node.classList.remove(old);
      node.classList.add(name);
    }
  }
}
function dynamicStyleClass(declarations) {
  const key = declarations.join(';');
  if (dynamicStyleClasses.has(key)) return dynamicStyleClasses.get(key);
  if (dynamicStyleClasses.size >= MAX_DYNAMIC_STYLE_RULES) compactDynamicStyles();
  return insertDynamicStyle(key);
}
function applyDynamicStyles(root) {
  const nodes = [];
  if (root.nodeType === Node.ELEMENT_NODE && root.hasAttribute('data-style')) nodes.push(root);
  root.querySelectorAll?.('[data-style]').forEach(node => nodes.push(node));
  for (const node of nodes) {
    const declarations = [];
    for (const declaration of node.dataset.style.split(';')) {
      const colon = declaration.indexOf(':');
      if (colon < 1) continue;
      const property = declaration.slice(0, colon).trim();
      const value = declaration.slice(colon + 1).trim();
      if (!DYNAMIC_STYLE_PROPERTIES.has(property))
        throw new Error(`forbidden dynamic style property: ${property}`);
      if (!value || /[{}@]/.test(value) || /url\s*\(/i.test(value) || !CSS.supports(property, value))
        throw new Error(`forbidden dynamic style value for ${property}`);
      declarations.push(`${property}:${value}`);
    }
    const name = dynamicStyleClass(declarations);
    for (const old of [...node.classList].filter(value => value.startsWith('dynamic-style-')))
      node.classList.remove(old);
    node.classList.add(name);
    node.removeAttribute('data-style');
  }
}
const styleObserver = new MutationObserver(records => {
  for (const record of records)
    for (const node of record.addedNodes) applyDynamicStyles(node);
});
styleObserver.observe(document.documentElement, { childList: true, subtree: true });
applyDynamicStyles(document);
/* ---------- i18n ----------
   Catalog values remain plain Unicode text. Native DOM sinks own ordinary
   element and text-bearing attribute contexts. A branded catalog descriptor
   is inert until the fixed-markup HTML sink resolves and escapes it. */
const EMERGENCY_MESSAGE = 'NIM Proxy interface failed to load.';
let I18N;
let MSG;
function failInterface() {
  document.body.replaceChildren(document.createTextNode(EMERGENCY_MESSAGE));
  document.body.hidden = false;
}
function presentationStylesheetReady() {
  const sheet = document.getElementById('presentation-stylesheet')?.sheet;
  try { return Boolean(sheet?.cssRules.length); }
  catch { return false; }
}
function validBootstrap(value) {
  return value
    && Object.keys(value).sort().join(',') === 'installed_locales,server_default'
    && Array.isArray(value.installed_locales)
    && value.installed_locales.length > 0
    && value.installed_locales.every(locale => typeof locale === 'string')
    && typeof value.server_default === 'string'
    && value.installed_locales.includes(value.server_default);
}
function validCatalog(value, locale) {
  return value
    && Object.keys(value).sort().join(',') === 'locale,messages'
    && value.locale === locale
    && value.messages
    && !Array.isArray(value.messages)
    && typeof value.messages === 'object'
    && Object.values(value.messages).every(value => typeof value === 'string');
}
async function responseJson(path) {
  const response = await fetch(path, { headers: { Accept: 'application/json' } });
  if (!response.ok) throw new Error('startup request failed');
  return response.json();
}
function loadScript(path) {
  return new Promise((resolve, reject) => {
    const script = document.createElement('script');
    script.src = path;
    script.onload = resolve;
    script.onerror = () => reject(new Error('application asset failed'));
    document.body.appendChild(script);
  });
}
const catalogReady = (async () => {
  if (!presentationStylesheetReady()) return;
  const bootstrap = await responseJson('/api/locale-bootstrap');
  if (!validBootstrap(bootstrap)) throw new Error('invalid locale bootstrap');
  const config = await responseJson('/api/config');
  if (!config
      || !Object.prototype.hasOwnProperty.call(config, 'locale')
      || (config.locale !== null && typeof config.locale !== 'string'))
    throw new Error('invalid locale preference');
  const locale = [
    config.locale,
    bootstrap.server_default,
    'en-US',
  ].find(candidate => bootstrap.installed_locales.includes(candidate));
  if (!locale) throw new Error('no installed startup locale');
  const catalog = await responseJson(
    `/assets/operator/locales/${encodeURIComponent(locale)}.json`,
  );
  if (!validCatalog(catalog, locale)) throw new Error('invalid locale catalog');
  I18N = catalog;
  MSG = catalog.messages;
  document.documentElement.lang = locale;
  initializeFormatters();
  await loadScript('/assets/operator/settings.js');
  await loadScript('/assets/operator/dashboard.js');
})().catch(() => {
  failInterface();
});
const message = (id, params = {}) => {
  let text = MSG[id] === undefined ? id : MSG[id];
  for (const key in params)
    text = text.split('{' + key + '}').join(String(params[key]));
  return text;
};
const CATALOG_MESSAGE = Symbol('catalog-message');
const catalogMessage = (id, params = {}) =>
  Object.freeze({
    type: CATALOG_MESSAGE,
    id,
    params: Object.freeze({ ...params }),
    [Symbol.toPrimitive]() {
      throw new TypeError('catalog descriptor requires escapeHtml');
    },
  });
function escapeHtml(value) {
  const text = value && value.type === CATALOG_MESSAGE
    ? message(value.id, value.params)
    : String(value);
  return text.replace(/[&<>"']/g, c =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}
function assertMessageTextTarget(node) {
  const context = node.nodeType === Node.TEXT_NODE ? node.parentElement : node;
  if (!context ||
      context instanceof HTMLScriptElement ||
      context instanceof HTMLStyleElement ||
      context instanceof SVGElement ||
      context.closest?.('script,style,svg'))
    throw new Error('forbidden catalog text target');
}
const I18N_TEXT_ATTRS = new Set(['title', 'placeholder', 'aria-label', 'alt']);
function setMessageText(node, id, params = {}) {
  assertMessageTextTarget(node);
  node.textContent = message(id, params);
}
function setMessageAttr(node, attr, id, params = {}) {
  if (!I18N_TEXT_ATTRS.has(attr))
    throw new Error(`forbidden catalog attribute: ${attr}`);
  node.setAttribute(attr, message(id, params));
}
function confirmMessage(id, params = {}) {
  return confirm(message(id, params));
}
function promptMessage(id, params = {}) {
  return prompt(message(id, params));
}
/* Static markup carries data-i18n="<id>" for content and
   data-i18n-attr="<attr>:<id>[,<attr>:<id>]" for attributes. */
function applyStatic(root) {
  root.querySelectorAll('[data-i18n]').forEach(el => {
    setMessageText(el, el.dataset.i18n);
  });
  /* Replace an element's own first text node, leaving sibling markup alone.
     Assigning to a text node cannot introduce markup, so this path needs no
     escaping — and it keeps headings that mix a text node with an icon and a
     .note span structurally identical. */
  root.querySelectorAll('[data-i18n-text]').forEach(el => {
    for (const n of el.childNodes) {
      if (n.nodeType === 3 && n.textContent.trim()) {
        setMessageText(n, el.dataset.i18nText);
        break;
      }
    }
  });
  root.querySelectorAll('[data-i18n-attr]').forEach(el => {
    el.dataset.i18nAttr.split(',').forEach(pair => {
      const i = pair.indexOf(':');
      const attr = pair.slice(0, i);
      setMessageAttr(el, attr, pair.slice(i + 1));
    });
  });
}

// Sequential green ramp for the heatmap (dark → bright = more).
const RAMP = ['#141A0E','#233312','#33501A','#4E7A0F','#76B900','#A7D65A'];
const MED = '#A7D65A', P95 = '#6F7767';   // quantile pair: filled median, muted p95

/* ---------- formatting ----------
   Numbers, durations and dates follow the CATALOG's locale, not the browser's.
   A dashboard rendered in German should not group thousands the American way
   because the operator's OS happens to be en-US. Formatters are constructed
   once — Intl constructors are expensive and these run inside render loops.

   Thresholds are deliberately unchanged from the hand-rolled versions. Intl's
   own compact notation would kick in at 1,000 ("1K"); this dashboard shows
   exact counts up to 10,000 because request counts in the hundreds are
   meaningful and "1.2K" is not. */
/* A malformed tag (e.g. the POSIX spelling "en_US") makes Intl constructors
   throw. Fail catalog startup instead of formatting under the wrong locale. */
let LOCALE;
const NF = (opts) => new Intl.NumberFormat(LOCALE, opts);
const DTF = (opts) => new Intl.DateTimeFormat(LOCALE, opts);
let NUM_COMPACT;
let NUM_SCIENTIFIC;
let NUM_GROUPED;
let NUM_1DP;
let NUM_2DP;
let DUR_MS;
let DUR_S;
let DUR_MIN;
let AGO_D;
let AGO_H;
let AGO_M;
let AGO_S;
let DAY_SHORT;
let TIME_HM;
/* Weekday names and the hour axis are CLDR DATA, not labels: putting them in
   the catalog would ask a translator to re-key what the platform already has
   correct for 200+ locales. Formatted in UTC against a reference week so a
   timezone offset cannot shift which day a name belongs to. */
let WEEKDAY_SHORT;
let HOUR_ONLY;
/* CLDR first day of week: 1=Mon .. 7=Sun. en-US, ja-JP and pt-BR start Sunday;
   ar-EG and fa-IR start Saturday. getWeekInfo is a method in Chrome and a
   getter in Firefox; neither exists in older Safari, hence the fallback. */
let FIRST_DAY;
/* JS getDay() is 0=Sun..6=Sat; CLDR is 1=Mon..7=Sun. One conversion, used by
   BOTH the axis labels and the row bucketing so they cannot drift apart. */
let FIRST_DAY_JS;
const dayRow = jsDay => (jsDay - FIRST_DAY_JS + 7) % 7;
/* Explicit components rather than dateStyle:'short', which abbreviates the
   year to two digits — ambiguous on a dashboard that retains 30+ days. */
let STAMP;
let SCOPE_TIME;
/* Percentages shown to the operator. NOT for CSS: `data-style="width:12.3%"` must
   stay locale-independent, because a comma-decimal locale would emit
   `width:12,3%`, which is invalid CSS and silently collapses the element.
   Every toFixed() that remains is either inside a data-style= attribute or SVG
   path/geometry — both machine formats, never read by a human. */
let PCT_0;
let PCT_1;
let PCT_2;
let PCT_3;
/* Trend chips carry an explicit sign; Intl places it per locale. */
let PCT_SIGNED_0;
let PCT_SIGNED_1;
let DAYS;
let PLURALS;
function initializeFormatters() {
  const tag = I18N.locale || 'en-US';
  try {
    new Intl.NumberFormat(tag);
    LOCALE = tag;
  } catch {
    throw new Error('invalid catalog locale');
  }
  NUM_COMPACT = NF({ notation: 'compact', maximumFractionDigits: 1 });
  NUM_SCIENTIFIC = NF({ notation: 'scientific', maximumSignificantDigits: 3 });
  NUM_GROUPED = NF({ maximumFractionDigits: 0 });
  NUM_1DP = NF({ maximumFractionDigits: 1 });
  NUM_2DP = NF({ maximumFractionDigits: 2 });
  DUR_MS = NF({ style: 'unit', unit: 'millisecond', unitDisplay: 'short', maximumFractionDigits: 0 });
  DUR_S = NF({ style: 'unit', unit: 'second', unitDisplay: 'short', minimumFractionDigits: 1, maximumFractionDigits: 1 });
  DUR_MIN = NF({ style: 'unit', unit: 'minute', unitDisplay: 'short', maximumFractionDigits: 0 });
  AGO_D = NF({ style: 'unit', unit: 'day', unitDisplay: 'narrow', maximumFractionDigits: 0 });
  AGO_H = NF({ style: 'unit', unit: 'hour', unitDisplay: 'narrow', maximumFractionDigits: 0 });
  AGO_M = NF({ style: 'unit', unit: 'minute', unitDisplay: 'narrow', maximumFractionDigits: 0 });
  AGO_S = NF({ style: 'unit', unit: 'second', unitDisplay: 'narrow', maximumFractionDigits: 0 });
  DAY_SHORT = DTF({ month: 'short', day: 'numeric' });
  TIME_HM = DTF({ hour: '2-digit', minute: '2-digit' });
  WEEKDAY_SHORT = DTF({ weekday: 'short', timeZone: 'UTC' });
  HOUR_ONLY = DTF({ hour: 'numeric', timeZone: 'UTC' });
  try {
    const locale = new Intl.Locale(LOCALE);
    FIRST_DAY = (
      typeof locale.getWeekInfo === 'function'
        ? locale.getWeekInfo()
        : locale.weekInfo
    )?.firstDay ?? 1;
  } catch {
    FIRST_DAY = 1;
  }
  FIRST_DAY_JS = FIRST_DAY % 7;
  STAMP = DTF({
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
    second: '2-digit',
  });
  SCOPE_TIME = DTF({
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
  PCT_0 = NF({ style: 'percent', maximumFractionDigits: 0 });
  PCT_1 = NF({ style: 'percent', maximumFractionDigits: 1 });
  PCT_2 = NF({ style: 'percent', maximumFractionDigits: 2 });
  PCT_3 = NF({ style: 'percent', maximumFractionDigits: 3 });
  PCT_SIGNED_0 = NF({
    style: 'percent',
    maximumFractionDigits: 0,
    signDisplay: 'exceptZero',
  });
  PCT_SIGNED_1 = NF({
    style: 'percent',
    maximumFractionDigits: 1,
    signDisplay: 'exceptZero',
  });
  PLURALS = new Intl.PluralRules(LOCALE);
  DAYS = Array.from({ length: 7 }, (_, i) =>
    WEEKDAY_SHORT.format(
      Date.UTC(2024, 0, 1 + ((FIRST_DAY - 1 + i) % 7)),
    ));
}
/* ratio in 0..1 */
const pctOf = (r, digits) => (digits >= 3 ? PCT_3 : digits === 2 ? PCT_2 : digits === 1 ? PCT_1 : PCT_0).format(r);
const NO_VALUE = '–';
/* Chart X-axis label. Shared by both chart builders so there is exactly one
   threshold to get wrong, and so the fixture can pin the real one. */
const axisLabel = (ms, spanMs) => (spanMs > 36e5 * 26 ? DAY_SHORT : TIME_HM).format(ms);
/* Intl.DateTimeFormat throws RangeError on a non-finite time, where
   toLocaleString() used to return "Invalid Date". A throw here escapes the
   template it sits in and blanks a whole panel, so degrade to a placeholder. */
const at = (fmtr, ms) => (Number.isFinite(ms) ? fmtr.format(ms) : NO_VALUE);

const fmt = n => !isFinite(n) ? NO_VALUE
  : Math.abs(n) >= 1e4 ? (() => {
    const compact = NUM_COMPACT.format(n);
    return compact.length > 12 ? NUM_SCIENTIFIC.format(n) : compact;
  })()
  : Math.abs(n) >= 100 ? NUM_GROUPED.format(n)
  : Math.abs(n) >= 10 ? NUM_1DP.format(n)
  : NUM_2DP.format(n);
const secs = s => !isFinite(s) ? NO_VALUE
  : s < 1 ? DUR_MS.format(Math.round(s * 1000))
  : s < 90 ? DUR_S.format(s)
  : DUR_MIN.format(Math.round(s / 60));
/* Compound elapsed time ("2d 3h"). Intl has no compound-duration formatter, so
   each component is formatted separately in narrow unit style, which is what
   produced the hand-written d/h/m/s suffixes in the first place. */
const ago = s => { s = Math.max(0, Math.round(s));
  const d = Math.floor(s/86400), h = Math.floor(s%86400/3600), m = Math.floor(s%3600/60);
  return d ? `${AGO_D.format(d)} ${AGO_H.format(h)}`
    : h ? `${AGO_H.format(h)} ${AGO_M.format(m)}`
    : m ? AGO_M.format(m) : AGO_S.format(s); };

/* ---------- typed dashboard rows ---------- */
const rowKey = r => r.metric + '\0' +
  Object.entries(r.labels || {}).sort(([a], [b]) => a.localeCompare(b))
    .map(([k, v]) => `${k}=${v}`).join('\0');
const asRow = r => ({ name: r.metric, labels: r.labels || {}, value: +r.value });
const sum = (rows, name, f) => rows.filter(r => r.name === name && (!f || f(r.labels))).reduce((a,r) => a + r.value, 0);
const groups = (rows, name, key, f) => {
  const out = new Map();
  for (const r of rows) if (r.name === name && (!f || f(r.labels)))
    out.set(r.labels[key] ?? '', (out.get(r.labels[key] ?? '') || 0) + r.value);
  return out;
};
/* One pair of predicates for "this request succeeded" / "this request failed",
   shared by every panel. The `status` label is whatever the upstream returned
   (src/proxy.rs record_request), so it is not always the literal '200'. Eight
   sites used to spell this out by hand and they disagreed: a 204 counted as
   Success in the outcome chart, as an error in the per-model error rate, and
   as an "Other" error row in the reliability table, all at once. */
const IS_2XX = s => /^2\d\d$/.test(s);
const IS_ERR = s => !IS_2XX(s) && s !== 'disconnect';
/* cumulative histogram buckets [{le, count}], summed across label filter */
function buckets(rows, name, f) {
  const m = new Map();
  for (const r of rows) if (r.name === name + '_bucket' && (!f || f(r.labels))) {
    const le = r.labels.le === '+Inf' ? Infinity : parseFloat(r.labels.le);
    m.set(le, (m.get(le) || 0) + r.value);
  }
  return [...m.entries()].sort((a,b) => a[0]-b[0]).map(([le, count]) => ({ le, count }));
}
/* quantile from windowed (delta) buckets via linear interpolation */
function quantile(bs, q) {
  const total = bs.length ? bs[bs.length-1].count : 0;
  if (total <= 0) return NaN;
  const target = total * q;
  let prev = { le: 0, count: 0 };
  for (const b of bs) {
    if (b.count >= target) {
      const hi = b.le === Infinity ? prev.le || 1 : b.le;
      return prev.le + (hi - prev.le) * (target - prev.count) / Math.max(1, b.count - prev.count);
    }
    prev = b;
  }
  return prev.le;
}
const deltaBuckets = (cur, prev) => cur.map((b, i) => ({ le: b.le, count: b.count - (prev[i]?.count || 0) }));

/* ---------- state: one selected range plus a lightweight current snapshot ---------- */
const POLL_MS = 3000;
let samples = [];              // synthetic cumulative [{t: milliseconds, rows}]
let rangeData = null;
let nowData = null;
let nowRows = [];
let mode = { kind: 'following', preset: 'default', paused: false, from: 0, to: 0 };
let cfg = {
  lanes: 0, rpm: 40, rpms: [], capacity_rpm: 0,
  started: Date.now()/1000, version: '', auth: false, default_window_days: 30,
  retention_days: 30, slo_target_percent: 99.9,
};
let previousNow = null, nowRpm = 0, nowLaneRates = new Map();
let rangeRequestGeneration = 0;
let frozenHasTraffic = false;

/* Adapt normalized counter deltas back into the cumulative rows all existing
   renderers consume. Gauges are replaced, never accumulated; the final
   historical sample is overwritten from exact range totals. */
function rangeSamples(range, tail) {
  const gaugeKeys = new Set((range.latest || []).map(rowKey));
  let counters = new Map();
  const gauges = new Map();
  const seedCounter = r => {
    const key = rowKey(r);
    if (!gaugeKeys.has(key) && !counters.has(key))
      counters.set(key, { metric: r.metric, labels: r.labels || {}, value: 0 });
  };
  for (const r of range.totals || []) seedCounter(r);
  for (const point of range.points || [])
    for (const r of point.values || []) seedCounter(r);

  const rows = () => [...counters.values(), ...gauges.values()].map(asRow);
  const baseline = range.window.effective_from ?? range.window.requested_from;
  const out = [{ t: baseline * 1000, rows: rows() }];
  const points = [...(range.points || [])].sort((a, b) => a.to - b.to);
  for (const point of points) {
    for (const r of point.values || []) {
      const key = rowKey(r);
      if (gaugeKeys.has(key)) {
        gauges.set(key, { metric: r.metric, labels: r.labels || {}, value: +r.value });
      } else {
        const current = counters.get(key) || { metric: r.metric, labels: r.labels || {}, value: 0 };
        counters.set(key, { ...current, value: current.value + (+r.value || 0) });
      }
    }
    out.push({ t: point.to * 1000, rows: rows() });
  }

  counters = new Map((range.totals || []).map(r =>
    [rowKey(r), { metric: r.metric, labels: r.labels || {}, value: +r.value }]));
  for (const r of range.latest || [])
    gauges.set(rowKey(r), { metric: r.metric, labels: r.labels || {}, value: +r.value });
  if (out.length > 1) out[out.length - 1].rows = rows();

  if (tail && tail.base_history_revision === range.history_revision) {
    for (const r of tail.totals || []) {
      const key = rowKey(r);
      const current = counters.get(key) || { metric: r.metric, labels: r.labels || {}, value: 0 };
      counters.set(key, { ...current, value: current.value + (+r.value || 0) });
    }
    out.push({ t: tail.to * 1000, rows: rows() });
  }
  return out;
}

function hasSelectedRequestTraffic(selectedSamples) {
  return selectedSamples.some(sample => sample.rows.some(
    row => row.name === 'nimproxy_requests_total' && +row.value > 0));
}

/* pool capacity from per-lane rpms when the server provides them (keys can
   have different limits); the uniform `rpm` field is the fallback */
const poolCapacity = () => Math.max(0, cfg.capacity_rpm ?? cfg.lanes * cfg.rpm);
const laneRpm = i => (cfg.rpms && cfg.rpms[i]) ?? cfg.rpm;

function rate(cur, prev, curT, prevT) {
  if (prev === undefined || cur < prev || curT <= prevT) return 0;
  return (cur - prev) / ((curT - prevT) / 60000);
}
/* pairwise series over samples from a per-sample extractor */
const rateSeries = get => samples.slice(1).map((s, i) =>
  ({ t: s.t, v: rate(get(s), get(samples[i]), s.t, samples[i].t) }));

/* every selected range begins at a synthetic zero-counter baseline */
const windowed = get => {
  if (samples.length < 2) return 0;
  return Math.max(0, get(samples[samples.length-1]) - get(samples[0]));
};
/* windowed per-key map: delta over the selected view */
function wgroups(name, key, f) {
  const cur = groups(samples[samples.length-1].rows, name, key, f);
  const base = groups(samples[0].rows, name, key, f);
  for (const [k, v] of cur) cur.set(k, Math.max(0, v - (base.get(k) || 0)));
  return cur;
}
/* windowed histogram buckets: delta over the selected view */
function wbuckets(metric, f) {
  const cur = buckets(samples[samples.length-1].rows, metric, f);
  if (samples.length < 2) return cur;
  return deltaBuckets(cur, buckets(samples[0].rows, metric, f));
}
/* pairwise windowed average of a histogram's sum/count — a trend line for
   "avg X per poll interval" (TTFT, speed, …) */
const avgSeries = (sumName, cntName, f) => samples.slice(1).map((s, i) => {
  const dS = sum(s.rows, sumName, f) - sum(samples[i].rows, sumName, f);
  const dN = sum(s.rows, cntName, f) - sum(samples[i].rows, cntName, f);
  return dN > 0 && dS >= 0 ? { t: s.t, v: dS / dN } : null;
}).filter(Boolean);

/* quantile median/p95 series from windowed bucket deltas over `samples`;
   `filter` narrows the source buckets (e.g. source="usage"). */
const quantSeries = (metric, filter) => [[0.5, 'dashboard.chart.median', MED], [0.95, 'dashboard.chart.p95', P95]].map(([q, nameId, color]) => ({
  nameId, color,
  pts: samples.slice(1).map((s, i) => {
    const bA = s2 => buckets(s2.rows, metric, filter);
    return { t: s.t, v: quantile(deltaBuckets(bA(s), bA(samples[i])), q) };
  }).filter(p => isFinite(p.v)),
}));

/* ---------- tooltip ---------- */
const tip = $('tooltip');
function showTip(x, y, html) {
  tip.innerHTML = html; tip.style.display = 'block';
  const w = tip.offsetWidth, h = tip.offsetHeight;
  tip.style.left = Math.min(x + 14, innerWidth - w - 8) + 'px';
  tip.style.top = Math.max(8, Math.min(y - h - 12, innerHeight - h - 8)) + 'px';
}
const hideTip = () => tip.style.display = 'none';
/* last mouse position, so hover survives the 3s live re-render */
let mouse = { x: -1, y: -1 };
addEventListener('mousemove', ev => { mouse.x = ev.clientX; mouse.y = ev.clientY; }, { passive: true });

/* ---------- line chart: full-bleed plot, overlay y-labels, hover crosshair
   + snapped per-series dots + tooltip card ---------- */
function lineChart(el, series, unitFmt, opts = {}) {
  const H = opts.height || 190;
  el.classList.add('chart');
  const live = series.filter(s => s.pts.length > 1);
  if (!live.length) { el.innerHTML = `<div class="empty">${escapeHtml(catalogMessage('dashboard.common.empty.no_data_in_range'))}</div>`; return; }
  const W = el.clientWidth || 500, PT = 16, PB = 16;
  const t0 = Math.min(...live.map(s => s.pts[0].t));
  const t1 = Math.max(...live.map(s => s.pts[s.pts.length-1].t));
  let vmax = Math.max(1e-6, ...live.flatMap(s => s.pts.map(p => p.v)));
  if (vmax <= 1e-6) vmax = 1; // all-zero series: show a 0..1 scale
  if (vmax >= 0.5) { const step = Math.pow(10, Math.floor(Math.log10(vmax))); vmax = Math.ceil(vmax / (step/2)) * (step/2); }
  const X = ms => W * (ms - t0) / Math.max(1, t1 - t0);
  const Y = v => PT + (H-PT-PB) * (1 - Math.min(v, vmax) / vmax);
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
  svg.setAttribute('width', '100%'); svg.setAttribute('height', H);
  let g = '';
  for (let i = 0; i <= 3; i++) {
    const v = vmax * i / 3, y = Y(v);
    g += `<line x1="0" y1="${y}" x2="${W}" y2="${y}" stroke="${css('--hairline')}" stroke-width="1"/>`;
    if (i > 0) g += `<text x="${W-3}" y="${y-4}" text-anchor="end">${unitFmt(v)}</text>`;
  }
  const span = t1 - t0;
  const tlab = ms => axisLabel(ms, span);
  g += `<text x="0" y="${H-4}" fill="${css('--ink-4')}">${tlab(t0)}</text><text x="${W}" y="${H-4}" text-anchor="end" fill="${css('--ink-4')}">${tlab(t1)}</text>`;
  for (const s of live) {
    const d = s.pts.map((p,i) => `${i?'L':'M'}${X(p.t).toFixed(1)},${Y(p.v).toFixed(1)}`).join('');
    if (s.area) g += `<path d="${d}L${X(s.pts[s.pts.length-1].t).toFixed(1)},${H-PB}L${X(s.pts[0].t).toFixed(1)},${H-PB}Z" fill="${s.area === true ? 'url(#gGreen)' : s.area}"/>`;
    g += `<path d="${d}" fill="none" stroke="${s.color}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>`;
    const e = s.pts[s.pts.length-1];
    g += `<circle cx="${X(e.t)}" cy="${Y(e.v)}" r="3.5" fill="${s.color}" stroke="${css('--bg')}" stroke-width="1.5"/>`;
  }
  g += `<g id="hoverg" visibility="hidden"><line id="xh" x1="0" y1="${PT}" x2="0" y2="${H-PB}" stroke="rgba(255,255,255,0.28)" stroke-width="1"/></g>`;
  svg.innerHTML = g;
  el.innerHTML = ''; el.appendChild(svg);
  const hoverg = svg.querySelector('#hoverg'), xh = svg.querySelector('#xh');
  const hover = (clientX, clientY) => {
    const r = svg.getBoundingClientRect();
    const tAt = t0 + (clientX - r.left) / Math.max(1, r.width) * (t1 - t0);
    // snap to the nearest sample of the densest series
    const ref = live.reduce((a, s) => s.pts.length > a.pts.length ? s : a);
    let best = ref.pts[0];
    for (const p of ref.pts) if (Math.abs(p.t - tAt) < Math.abs(best.t - tAt)) best = p;
    const mx = X(best.t);
    xh.setAttribute('x1', mx); xh.setAttribute('x2', mx);
    let dots = '';
    let html = `<div class="t">${at(STAMP, best.t)}</div>`;
    for (const s of live) {
      let p = s.pts[0];
      for (const q of s.pts) if (Math.abs(q.t - best.t) < Math.abs(p.t - best.t)) p = q;
      dots += `<circle cx="${X(p.t)}" cy="${Y(p.v)}" r="4" fill="${s.color}" stroke="${css('--bg')}" stroke-width="1.5"/>`;
      html += `<div class="row"><span><i class="sw" data-style="background:${s.color}"></i>${escapeHtml(s.nameId ? catalogMessage(s.nameId) : s.name)}</span><b>${unitFmt(p.v)}</b></div>`;
    }
    hoverg.innerHTML = xh.outerHTML + dots;
    hoverg.setAttribute('visibility', 'visible');
    showTip(clientX, clientY, html);
  };
  svg.addEventListener('mousemove', ev => hover(ev.clientX, ev.clientY));
  svg.addEventListener('mouseleave', () => { hoverg.setAttribute('visibility', 'hidden'); hideTip(); });
  // re-apply hover if the pointer is already over this chart (live re-render)
  const r = svg.getBoundingClientRect();
  if (mouse.x >= r.left && mouse.x <= r.right && mouse.y >= r.top && mouse.y <= r.bottom) hover(mouse.x, mouse.y);
}

/* ---------- stacked area chart over time: bands sum to a running total on a
   shared time axis. Series carry raw (un-stacked) values at aligned sample
   times; hover shows each band's own value plus the stacked total. Same grid,
   axes, crosshair and tooltip idiom as lineChart. ---------- */
function stackChart(el, series, unitFmt, opts = {}) {
  const H = opts.height || 190;
  el.classList.add('chart');
  let bands = series.filter(s => s.pts.length > 1);
  const n = bands.length ? Math.max(...bands.map(s => s.pts.length)) : 0;
  // Bands pair positionally on one shared time axis; a series with a
  // different sample count (e.g. a null-dropping average) would draw against
  // wrong timestamps, so it is dropped rather than drawn wrong.
  bands = bands.filter(s => s.pts.length === n);
  const ts = (bands[0] || { pts: [] }).pts.map(p => p.t);
  const bandAt = (s, i) => Math.max(0, s.pts[i] ? s.pts[i].v : 0);
  const totals = ts.map((_, i) => bands.reduce((a, s) => a + bandAt(s, i), 0));
  if (ts.length < 2 || !totals.some(v => v > 0)) { el.innerHTML = `<div class="empty">${escapeHtml(catalogMessage('dashboard.common.empty.no_data_in_range'))}</div>`; return; }
  const W = el.clientWidth || 500, PT = 16, PB = 16;
  const t0 = ts[0], t1 = ts[ts.length-1];
  let vmax = Math.max(1e-6, ...totals);
  if (vmax >= 0.5) { const step = Math.pow(10, Math.floor(Math.log10(vmax))); vmax = Math.ceil(vmax / (step/2)) * (step/2); }
  const X = ms => W * (ms - t0) / Math.max(1, t1 - t0);
  const Y = v => PT + (H-PT-PB) * (1 - Math.min(v, vmax) / vmax);
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
  svg.setAttribute('width', '100%'); svg.setAttribute('height', H);
  let g = '';
  for (let i = 0; i <= 3; i++) {
    const v = vmax * i / 3, y = Y(v);
    g += `<line x1="0" y1="${y}" x2="${W}" y2="${y}" stroke="${css('--hairline')}" stroke-width="1"/>`;
    if (i > 0) g += `<text x="${W-3}" y="${y-4}" text-anchor="end">${unitFmt(v)}</text>`;
  }
  const span = t1 - t0;
  const tlab = ms => axisLabel(ms, span);
  g += `<text x="0" y="${H-4}" fill="${css('--ink-4')}">${tlab(t0)}</text><text x="${W}" y="${H-4}" text-anchor="end" fill="${css('--ink-4')}">${tlab(t1)}</text>`;
  // Cumulative bands drawn top→bottom: each fills to the baseline, painting
  // over the shorter (lower) bands, so band k occupies [cum(k-1), cum(k)].
  const run = ts.map(() => 0), cum = [];
  for (const s of bands) { for (let i = 0; i < ts.length; i++) run[i] += bandAt(s, i); cum.push(run.slice()); }
  for (let k = bands.length - 1; k >= 0; k--) {
    const d = cum[k].map((v, i) => `${i?'L':'M'}${X(ts[i]).toFixed(1)},${Y(v).toFixed(1)}`).join('');
    g += `<path d="${d}L${X(t1).toFixed(1)},${H-PB}L${X(t0).toFixed(1)},${H-PB}Z" fill="${bands[k].color}" fill-opacity="0.82"/>`;
    g += `<path d="${d}" fill="none" stroke="${bands[k].color}" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round"/>`;
  }
  g += `<g id="hoverg" visibility="hidden"><line id="xh" x1="0" y1="${PT}" x2="0" y2="${H-PB}" stroke="rgba(255,255,255,0.28)" stroke-width="1"/></g>`;
  svg.innerHTML = g;
  el.innerHTML = ''; el.appendChild(svg);
  const hoverg = svg.querySelector('#hoverg'), xh = svg.querySelector('#xh');
  const hover = (clientX, clientY) => {
    const r = svg.getBoundingClientRect();
    const tAt = t0 + (clientX - r.left) / Math.max(1, r.width) * (t1 - t0);
    let bi = 0;
    for (let i = 0; i < ts.length; i++) if (Math.abs(ts[i] - tAt) < Math.abs(ts[bi] - tAt)) bi = i;
    const mx = X(ts[bi]);
    xh.setAttribute('x1', mx); xh.setAttribute('x2', mx);
    let dots = '';
    for (let k = bands.length - 1; k >= 0; k--)
      dots += `<circle cx="${mx}" cy="${Y(cum[k][bi])}" r="3" fill="${bands[k].color}" stroke="${css('--bg')}" stroke-width="1.5"/>`;
    let html = `<div class="t">${at(STAMP, ts[bi])}</div>`;
    for (const s of bands)
      html += `<div class="row"><span><i class="sw" data-style="background:${s.color}"></i>${escapeHtml(s.name)}</span><b>${unitFmt(bandAt(s, bi))}</b></div>`;
    html += `<div class="row" data-style="border-top:1px solid var(--hairline);margin-top:3px;padding-top:4px"><span>total</span><b>${unitFmt(totals[bi])}</b></div>`;
    hoverg.innerHTML = xh.outerHTML + dots;
    hoverg.setAttribute('visibility', 'visible');
    showTip(clientX, clientY, html);
  };
  svg.addEventListener('mousemove', ev => hover(ev.clientX, ev.clientY));
  svg.addEventListener('mouseleave', () => { hoverg.setAttribute('visibility', 'hidden'); hideTip(); });
  const rr = svg.getBoundingClientRect();
  if (mouse.x >= rr.left && mouse.x <= rr.right && mouse.y >= rr.top && mouse.y <= rr.bottom) hover(mouse.x, mouse.y);
}

function legend(el, series) {
  el.innerHTML = series.length > 1
    ? series.map(s => `<span><i data-style="background:${s.color}"></i>${escapeHtml(s.nameId ? catalogMessage(s.nameId) : s.name)}</span>`).join('')
    : '';
}

/* ---------- sortable growable table ----------
   cols: [{label, align?('l'), str?, best?('min'|'max'), fmt}], rows: [{vals, cells}]
   where vals are sortable primitives and cells pre-escaped HTML strings.
   Sort state persists across re-renders (Map by table id); scroll position is
   saved/restored around the innerHTML swap so the 3s live poll doesn't jump. */
const sortState = new Map();
function sortTable(el, id, cols, rows, opts = {}) {
  if (!rows.length) {
    el.innerHTML = `<div class="empty">${escapeHtml(opts.empty || catalogMessage('dashboard.common.empty.no_traffic'))}</div>`;
    return;
  }
  const st = sortState.get(id) || { idx: opts.defaultIdx ?? 1, asc: opts.defaultAsc ?? false };
  sortState.set(id, st);
  const sorted = [...rows].sort((a, b) => {
    const av = a.vals[st.idx], bv = b.vals[st.idx];
    if (typeof av === 'string' || typeof bv === 'string')
      return st.asc ? String(av).localeCompare(String(bv)) : String(bv).localeCompare(String(av));
    const an = isFinite(av) ? av : -Infinity, bn = isFinite(bv) ? bv : -Infinity;
    return st.asc ? an - bn : bn - an;
  });
  const bests = cols.map((c, i) => {
    if (!c.best || rows.length < 2) return null;
    const vs = rows.map(r => r.vals[i]).filter(v => typeof v === 'number' && isFinite(v));
    return vs.length ? (c.best === 'max' ? Math.max(...vs) : Math.min(...vs)) : null;
  });
  let h = `<div class="scroll" data-style="max-height:${opts.maxH || 420}px"><table data-style="min-width:${opts.minW || 0}px"><thead><tr>` +
    cols.map((c, i) =>
      `<th class="${c.align === 'l' ? 'l' : ''}${i === st.idx ? ' on' : ''}" data-i="${i}">${escapeHtml(c.label)}${i === st.idx ? (st.asc ? ' ↑' : ' ↓') : ''}</th>`
    ).join('') + '</tr></thead><tbody>' +
    sorted.map(r => '<tr>' + r.cells.map((cell, i) => {
      const best = bests[i] !== null && r.vals[i] === bests[i];
      return `<td class="${cols[i].align === 'l' ? 'l' : ''}${best ? ' best' : ''}">${cell}</td>`;
    }).join('') + '</tr>').join('') + '</tbody></table></div>';
  const prev = el.querySelector('.scroll');
  const top = prev ? prev.scrollTop : 0, left = prev ? prev.scrollLeft : 0;
  el.innerHTML = h;
  const sc = el.querySelector('.scroll');
  sc.scrollTop = top; sc.scrollLeft = left;
  for (const th of el.querySelectorAll('th')) th.addEventListener('click', () => {
    const i = +th.dataset.i;
    if (st.idx === i) st.asc = !st.asc;
    else { st.idx = i; st.asc = !!cols[i].str; }   // strings default A→Z, numbers big-first
    sortTable(el, id, cols, rows, opts);
  });
}

/* ---------- horizontal bar rows ----------
   Bars scale to the max entry, or to `maxV` when the metric has an absolute
   scale (percentages, utilization). */
function barList(el, entries, fmtV, maxV) {
  const vals = entries.filter(e => isFinite(e.v));
  if (!vals.length) { el.innerHTML = `<div class="empty">${escapeHtml(catalogMessage('dashboard.common.empty.no_data_yet'))}</div>`; return; }
  const max = maxV || Math.max(1e-9, ...vals.map(e => e.v));
  el.innerHTML = entries.map(e =>
    `<div class="brow"><span class="bname" title="${escapeHtml(e.name)}">${escapeHtml(e.label ?? e.name)}</span>` +
    `<span class="btrack">${isFinite(e.v) ? `<span data-style="width:${Math.max(2, e.v / max * 100).toFixed(0)}%;background:${e.color}"></span>` : ''}</span>` +
    `<span class="bval">${escapeHtml(e.vtext ?? (isFinite(e.v) ? fmtV(e.v) : '–'))}</span></div>`).join('');
}

/* leaderboard rows: chip square + name + value + thin progress bar */
function leaderList(el, entries, fmtV) {
  if (!entries.length) { el.innerHTML = `<div class="empty">${escapeHtml(catalogMessage('dashboard.common.empty.no_traffic'))}</div>`; return; }
  const max = Math.max(1e-9, ...entries.map(e => e.v));
  el.innerHTML = entries.map(e =>
    `<div class="lead">${e.chip}<div class="lbody">` +
    `<div class="lrow"><span class="lname" title="${escapeHtml(e.name)}">${escapeHtml(e.label ?? e.name)}</span><span class="lval">${fmtV(e.v)}</span></div>` +
    `<div class="ltrack"><span data-style="width:${Math.max(2, e.v / max * 100).toFixed(0)}%;background:${e.color}"></span></div>` +
    `</div></div>`).join('');
}

/* ---------- ring gauge ---------- */
function ringGauge(el, ratio, text, color, labelId, sub) {
  const C = 2 * Math.PI * 34, cl = Math.max(0, Math.min(1, ratio));
  const label = catalogMessage(labelId);
  el.innerHTML = `<div class="ring"><svg width="76" height="76" viewBox="0 0 84 84" role="img">
    <circle cx="42" cy="42" r="34" fill="none" stroke="rgba(255,255,255,0.08)" stroke-width="7"/>
    ${cl > 0.004 ? `<circle cx="42" cy="42" r="34" fill="none" stroke="${color}" stroke-width="7" stroke-linecap="round" stroke-dasharray="${(C*cl).toFixed(1)} ${C.toFixed(1)}" transform="rotate(-90 42 42)"/>` : ''}
    </svg><span class="rtext">${escapeHtml(text)}</span></div>
    <div class="rlabel">${escapeHtml(label)}</div><div class="rsub">${escapeHtml(sub)}</div>`;
  setMessageAttr(el.querySelector('svg'), 'aria-label', labelId);
}

/* ---------- KPI metric cards ---------- */
const ICONS = {
  pulse: '<path d="M1 8h3l2-5 3 11 2-6h4"/>',
  stack: '<path d="M8 1.5l6 3-6 3-6-3zM2 8l6 3 6-3M2 11.5l6 3 6-3"/>',
  check: '<path d="M2.5 8.5l3.5 3.5 7.5-8"/>',
  clock: '<path d="M8 4v4l2.5 1.5M8 1.5A6.5 6.5 0 108 14.5 6.5 6.5 0 008 1.5z"/>',
  bolt: '<path d="M9 1.5L3 9h4l-1 5.5L13 7H9z"/>',
  grid: '<path d="M2 8h5M2 4h9M2 12h7"/>',
};
function sparkSvg(pts, hero) {
  if (!pts || pts.length < 2) return '';
  const W = 300, H = hero ? 58 : 44, vs = pts.map(p => p.v);
  const vmax = Math.max(1e-9, ...vs);
  const X = i => W * i / (vs.length - 1), Y = v => 4 + (H - 8) * (1 - Math.min(v, vmax) / vmax);
  const d = vs.map((v, i) => (i ? 'L' : 'M') + X(i).toFixed(1) + ',' + Y(v).toFixed(1)).join('');
  return `<svg class="kpispark" viewBox="0 0 ${W} ${H}" preserveAspectRatio="none">` +
    `<path d="${d}L${W},${H}L0,${H}Z" fill="url(#gMuted)"/>` +
    `<path d="${d}" fill="none" stroke="#8E967F" stroke-width="1.5"/></svg>`;
}
/* trend chip: second half of the window vs the first half */
function deltaChip(pts, downIsGood) {
  if (!pts || pts.length < 6) return '';
  const mid = Math.floor(pts.length / 2);
  const a = pts.slice(0, mid).reduce((x, p) => x + p.v, 0) / mid;
  const b = pts.slice(mid).reduce((x, p) => x + p.v, 0) / (pts.length - mid);
  if (!(a > 0)) return '';
  const d = (b - a) / a * 100;
  if (!isFinite(d) || Math.abs(d) < 0.05) return '';
  const good = downIsGood ? d <= 0 : d >= 0;
  return `<span class="chip ${good ? 'good' : 'bad'}" title="${escapeHtml(catalogMessage('dashboard.common.kpi.delta_tooltip'))}">${(Math.abs(d) >= 10 ? PCT_SIGNED_0 : PCT_SIGNED_1).format(d / 100)}</span>`;
}
/* k: {icon, label, value, sub, delta?, pts?} */
function kpiCards(el, defs, hero) {
  el.innerHTML = defs.map(k =>
    `<div class="kpi${hero ? ' hero' : ''}">` +
    `<div class="kpihead"><span class="kpilabel"><svg viewBox="0 0 16 16">${ICONS[k.icon] || ''}</svg>${escapeHtml(k.label)}</span>${k.delta || ''}</div>` +
    `<div class="kpival">${escapeHtml(k.value)}</div>` +
    `<div class="kpisub">${escapeHtml(k.sub || '')}</div>` +
    sparkSvg(k.pts, hero) + `</div>`).join('');
}

/* one metric-list row: label, right-aligned mono value, optional tone */
const metricRow = (l, v, tone) => `<div class="kv${tone ? ' ' + tone : ''}"><span>${escapeHtml(l)}</span><b>${escapeHtml(v)}</b></div>`;

/* ---------- activity heatmap: weekday x hour, sequential green ramp ---------- */
function heatmap(el, matrix, vmax) {
  if (!vmax) { el.innerHTML = `<div class="empty">${escapeHtml(catalogMessage('dashboard.common.empty.no_data_in_range'))}</div>`; return; }
  const CW = 26, CH = 18, GAP = 3, PL = 36, PT = 18;
  const W = PL + 24*(CW+GAP), H = PT + 7*(CH+GAP);
  let g = '';
  for (let d = 0; d < 7; d++) {
    g += `<text x="${PL-8}" y="${PT + d*(CH+GAP) + CH - 4}" text-anchor="end">${DAYS[d]}</text>`;
    for (let h = 0; h < 24; h++) {
      const v = matrix[d][h];
      const color = v ? RAMP[Math.min(RAMP.length-1, Math.floor(v/vmax*(RAMP.length-0.001)))] : css('--thead');
      g += `<rect x="${PL + h*(CW+GAP)}" y="${PT + d*(CH+GAP)}" width="${CW}" height="${CH}" rx="3" fill="${color}" data-d="${d}" data-h="${h}"/>`;
    }
  }
  for (const h of [0, 6, 12, 18]) g += `<text x="${PL + h*(CW+GAP)}" y="${PT-6}">${HOUR_ONLY.format(Date.UTC(2024, 0, 1, h))}</text>`;
  el.innerHTML = `<svg viewBox="0 0 ${W} ${H}" width="100%" data-style="max-width:${W}px">${g}</svg>`;
  el.querySelector('svg').addEventListener('mousemove', ev => {
    const c = ev.target.closest('rect');
    if (!c) { hideTip(); return; }
    const d = +c.dataset.d, h = +c.dataset.h;
    showTip(ev.clientX, ev.clientY,
      `<div class="t">${DAYS[d]} ${HOUR_ONLY.formatRange(Date.UTC(2024, 0, 1, h), Date.UTC(2024, 0, 1, h + 1))}</div><div class="row"><span>${escapeHtml(catalogMessage('dashboard.common.col.requests'))}</span><b>${fmt(matrix[d][h])}</b></div>`);
  });
  el.querySelector('svg').addEventListener('mouseleave', hideTip);
}

/* ---------- entity identity: publishers, clients, colors ---------- */
const PUBLISHERS = {
  'meta': { name: 'Meta', slug: 'meta-color', color: '#0668E1' },
  'deepseek-ai': { name: 'DeepSeek', slug: 'deepseek-color', color: '#4D6BFE' },
  'moonshotai': { name: 'Moonshot AI', slug: 'moonshot', color: '#16B8C0' },
  'nvidia': { name: 'NVIDIA', slug: 'nvidia-color', color: '#76B900' },
  'qwen': { name: 'Qwen', slug: 'qwen-color', color: '#615CED' },
  'mistralai': { name: 'Mistral AI', slug: 'mistral-color', color: '#FA520F' },
  'google': { name: 'Google', slug: 'gemma-color', color: '#2E96FF' },
  'microsoft': { name: 'Microsoft', slug: 'microsoft-color', color: '#00A4EF' },
  'openai': { name: 'OpenAI', slug: 'openai', color: '#10A37F' },
  'ibm': { name: 'IBM', slug: 'ibm-color', color: '#0F62FE' },
  'ibm-granite': { name: 'IBM Granite', slug: 'ibm-color', color: '#0F62FE' },
  'upstage': { name: 'Upstage', slug: 'upstage-color', color: '#805CFB' },
  'zhipuai': { name: 'Zhipu AI', slug: 'zhipu-color', color: '#3859FF' },
  'z-ai': { name: 'Z.ai', slug: 'zhipu-color', color: '#3859FF' },
  'minimaxai': { name: 'MiniMax', slug: 'minimax-color', color: '#F23F5D' },
  'stepfun': { name: 'StepFun', slug: 'stepfun-color', color: '#EE6002' },
  'baichuan-inc': { name: 'Baichuan', slug: 'baichuan-color', color: '#FF6933' },
  '01-ai': { name: '01.AI', slug: 'yi-color', color: '#003425' },
  'writer': { name: 'Writer', slug: 'writer', color: '#160E1B' },
  'snowflake': { name: 'Snowflake', slug: 'snowflake-color', color: '#29B5E8' },
  'databricks': { name: 'Databricks', slug: 'databricks-color', color: '#FF3621' },
  'openclaw': { name: 'OpenClaw', slug: 'openclaw', color: '#4A5568' },
};
function publisher(model) {
  const ns = model.includes('/') ? model.split('/')[0] : '';
  const p = PUBLISHERS[ns.toLowerCase()];
  if (p) return p;
  const name = ns || 'unknown';
  return { name, slug: null, color: '#5A6150' };
}
function prettyName(model) {
  return model.includes('/') ? model.split('/').slice(1).join('/') : model;
}
const initialsOf = name => name.replace(/[^A-Za-z0-9 ]/g, ' ').trim().split(/\s+/)
  .map(w => w[0]).join('').slice(0, 2).toUpperCase() || '?';
/* Fixed local text primitive: publisher identity never requires the network. */
function chipHtml(pub) {
  const initials = initialsOf(pub.name);
  return `<span class="logo" data-style="background:${pub.color}">${escapeHtml(initials)}</span>`;
}
/* known-client colors, stable hash-to-hue for the rest */
const CLIENT_COLORS = {
  'claude-code': '#C15F3C', 'aider': '#2E7D5B', 'opencode': '#5A6150', 'cline': '#4D6BFE',
  'continue': '#615CED', 'cursor': '#2E96FF', 'roo-code': '#EE6002', 'zed': '#16B8C0',
  'codex': '#10A37F', 'n8n': '#EA4B71',
};
function hueFor(name) {
  let h = 0;
  for (const c of String(name)) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return `hsl(${h % 360} 58% 62%)`;
}
const clientColor = c => CLIENT_COLORS[String(c).toLowerCase()] || hueFor(c);
/* model line color: publisher brand, deduped per chart via hash fallback */
function seriesColors(models) {
  const used = new Set();
  return models.map(m => {
    let c = publisher(m).color;
    if (used.has(c)) c = hueFor(m);
    used.add(c);
    return c;
  });
}

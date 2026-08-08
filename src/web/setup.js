"use strict";
const $ = (id) => document.getElementById(id);
const setErrorText = (text) => { $("err").textContent = text || ""; };
const EMERGENCY_MESSAGE = "NIM Proxy interface failed to load.";
let MSG;
function failInterface() {
  document.body.replaceChildren(document.createTextNode(EMERGENCY_MESSAGE));
  document.body.hidden = false;
}
function presentationStylesheetReady() {
  const sheet = document.getElementById("presentation-stylesheet")?.sheet;
  try { return Boolean(sheet?.cssRules.length); }
  catch { return false; }
}
function validBootstrap(value) {
  return value
    && Object.keys(value).sort().join(",") === "installed_locales,server_default"
    && Array.isArray(value.installed_locales)
    && value.installed_locales.length > 0
    && value.installed_locales.every(locale => typeof locale === "string")
    && typeof value.server_default === "string"
    && value.installed_locales.includes(value.server_default);
}
function validCatalog(value, locale) {
  return value
    && Object.keys(value).sort().join(",") === "locale,messages"
    && value.locale === locale
    && value.messages
    && !Array.isArray(value.messages)
    && typeof value.messages === "object"
    && Object.values(value.messages).every(value => typeof value === "string");
}
async function responseJson(path) {
  const response = await fetch(path, { headers: { Accept: "application/json" } });
  if (!response.ok) throw new Error("startup request failed");
  return response.json();
}
const catalogReady = (async () => {
  if (!presentationStylesheetReady()) return;
  const bootstrap = await responseJson("/api/locale-bootstrap");
  if (!validBootstrap(bootstrap)) throw new Error("invalid locale bootstrap");
  const locale = bootstrap.server_default;
  const catalog = await responseJson(
    `/assets/public/locales/${encodeURIComponent(locale)}.json`,
  );
  if (!validCatalog(catalog, locale)) throw new Error("invalid locale catalog");
  MSG = catalog.messages;
  document.documentElement.lang = locale;
})().catch(() => {
  failInterface();
});

/* ---------- i18n ----------
   Catalog values remain plain Unicode text. Native DOM sinks own ordinary
   element and text-bearing attribute contexts. Structured messages splice
   fixed caller-created nodes between text nodes; catalog text is never HTML. */
const message = (id, params = {}) => {
  let text = MSG[id] === undefined ? id : MSG[id];
  for (const key in params)
    text = text.split("{" + key + "}").join(String(params[key]));
  return text;
};
const I18N_TEXT_ATTRS = new Set(["title", "placeholder", "aria-label", "alt"]);
function assertMessageTextTarget(node) {
  const context = node.nodeType === Node.TEXT_NODE ? node.parentElement : node;
  if (!context ||
      context instanceof HTMLScriptElement ||
      context instanceof HTMLStyleElement ||
      context instanceof SVGElement ||
      context.closest?.("script,style,svg"))
    throw new Error("forbidden catalog text target");
}
function setMessageText(node, id, params = {}) {
  assertMessageTextTarget(node);
  node.textContent = message(id, params);
}
function setMessageAttr(node, attr, id, params = {}) {
  if (!I18N_TEXT_ATTRS.has(attr))
    throw new Error(`forbidden catalog attribute: ${attr}`);
  node.setAttribute(attr, message(id, params));
}
function setMessageWithNodes(node, id, replacements) {
  assertMessageTextTarget(node);
  for (const [, replacement] of replacements) {
    assertMessageTextTarget(replacement);
    if (replacement.querySelector?.("script,style,svg"))
      throw new Error("forbidden catalog text target");
  }
  let text = message(id);
  const children = [];
  while (text) {
    let next = null;
    for (const [token, replacement] of replacements) {
      const at = text.indexOf(token);
      if (at >= 0 && (!next || at < next.at))
        next = { at, replacement, token };
    }
    if (!next) {
      children.push(document.createTextNode(text));
      break;
    }
    if (next.at) children.push(document.createTextNode(text.slice(0, next.at)));
    children.push(next.replacement.cloneNode(true));
    text = text.slice(next.at + next.token.length);
  }
  node.replaceChildren(...children);
}
function setEmphasizedMessage(node, id, emphasis) {
  assertMessageTextTarget(node);
  assertMessageTextTarget(emphasis);
  if (emphasis.querySelector?.("script,style,svg"))
    throw new Error("forbidden catalog text target");
  const text = message(id);
  const open = text.indexOf("{b}");
  const close = text.indexOf("{/b}", open + 3);
  if (open < 0 || close < 0)
    throw new Error(`invalid emphasis placeholders: ${id}`);
  emphasis.textContent = text.slice(open + 3, close);
  node.replaceChildren(
    document.createTextNode(text.slice(0, open)),
    emphasis,
    document.createTextNode(text.slice(close + 4)),
  );
}
function applyStatic(root) {
  root.querySelectorAll("[data-i18n]").forEach(el => {
    setMessageText(el, el.dataset.i18n);
  });
  // Own first text node — used for <title>, whose content is plain text.
  root.querySelectorAll("[data-i18n-text]").forEach(el => {
    for (const n of el.childNodes) {
      if (n.nodeType === 3 && n.textContent.trim()) { setMessageText(n, el.dataset.i18nText); break; }
    }
  });
  root.querySelectorAll("[data-i18n-attr]").forEach(el => {
    el.dataset.i18nAttr.split(",").forEach(pair => {
      const i = pair.indexOf(":");
      const attr = pair.slice(0, i);
      setMessageAttr(el, attr, pair.slice(i + 1));
    });
  });
}
function initializeSetup() {
  applyStatic(document);
  const keyLiteral = document.createElement("code");
  keyLiteral.textContent = "npk_\u2026";
  const endpointLiteral = document.createElement("code");
  endpointLiteral.textContent = "/v1";
  setMessageWithNodes($("mintkey-label"), "setup.step3.mintkey", [
    ["{key}", keyLiteral],
    ["{endpoint}", endpointLiteral],
  ]);
  setEmphasizedMessage(
    $("mintwarn"),
    "setup.step3.mintwarn",
    document.createElement("b"),
  );
  setEmphasizedMessage(
    $("setup-complete-intro"),
    "setup.step4.intro",
    document.createElement("b"),
  );
  document.body.hidden = false;
}
let keys = []; // {key, rpm, models}

function show(n) {
  [1,2,3].forEach(i => { $("step"+i).hidden = i !== n; $("s"+i).classList.toggle("on", i <= n); });
  setErrorText("");
}
function mask(k) { return k.length > 8 ? k.slice(0,6) + "••••" + k.slice(-4) : "••••"; }
function renderKeys() {
  const rows = keys.map((key, index) => {
    const row = document.createElement("li");
    const ok = document.createElement("span");
    ok.className = "ok";
    ok.textContent = "✓";
    const masked = document.createElement("code");
    masked.textContent = mask(key.key);
    const meta = document.createElement("span");
    meta.className = "meta";
    setMessageText(meta, "setup.step2.key_meta", {
      models: key.models,
      rpm: key.rpm,
    });
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "ghost";
    remove.textContent = "×";
    remove.onclick = () => { keys.splice(index, 1); renderKeys(); };
    row.append(ok, " ", masked, meta, remove);
    return row;
  });
  $("keylist").replaceChildren(...rows);
  $("to3").disabled = keys.length === 0;
}

$("to2").onclick = () => {
  const u = $("username").value.trim(), p = $("password").value;
  if (!/^[A-Za-z0-9._-]{1,32}$/.test(u)) return setMessageText($("err"), "setup.step1.error.username_charset");
  if (p.length < 10) return setMessageText($("err"), "setup.step1.error.password_length");
  if (p !== $("confirm").value) return setMessageText($("err"), "setup.step1.error.password_mismatch");
  show(2);
};
$("back1").onclick = () => show(1);
$("back2").onclick = () => show(2);

$("addkey").onclick = async () => {
  const key = $("newkey").value.trim(), rpm = Math.max(1, Math.min(10000, +$("newrpm").value || 40));
  if (!key) return setMessageText($("err"), "setup.step2.error.key_required");
  if (keys.some(k => k.key === key)) return setMessageText($("err"), "setup.step2.error.key_duplicate");
  setErrorText("");
  $("addkey").disabled = true; setMessageText($("addkey"), "setup.step2.validating");
  try {
    const body = { key };
    const base = $("baseurl").value.trim();
    if (base) body.base_url = base;
    const r = await fetch("/setup/validate-key", { method: "POST",
      headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
    const v = await r.json();
    if (v.ok) { keys.push({ key, rpm, models: v.models }); $("newkey").value = ""; renderKeys(); }
    else setMessageText($("err"), "setup.step2.error.validation_failed", { error: v.error || r.status });
  } catch (e) { setMessageText($("err"), "setup.step2.error.validation_request", { error: e }); }
  $("addkey").disabled = false; setMessageText($("addkey"), "setup.step2.addkey");
};

function apiAccessMessageId() {
  return $("mintkey").checked
    ? "setup.step3.api_access_keyed"
    : "setup.step3.api_access_open";
}
$("to3").onclick = () => {
  const base = $("baseurl").value.trim();
  const poolRpm = keys.reduce((a,k)=>a+k.rpm,0);
  const reviewRow = (labelId, value, valueId, messageId, params = {}) => {
    const row = document.createElement("div");
    const label = document.createElement("span");
    label.className = "k";
    setMessageText(label, labelId);
    const display = document.createElement("span");
    if (valueId) display.id = valueId;
    if (messageId) setMessageText(display, messageId, params);
    else display.textContent = value;
    row.append(label, display);
    return row;
  };
  $("review").replaceChildren(
    reviewRow(
      "setup.step3.review.superuser",
      $("username").value.trim(),
    ),
    reviewRow(
      "setup.step3.review.keys",
      "",
      "",
      "setup.step3.review.keys_value",
      {
        count: keys.length,
        rpm: poolRpm,
      },
    ),
    reviewRow(
      "setup.step3.review.upstream",
      base || "https://integrate.api.nvidia.com",
    ),
    reviewRow(
      "setup.step3.review.api_access",
      "",
      "apiline",
      apiAccessMessageId(),
    ),
  );
  show(3);
};
$("mintkey").onchange = () => {
  $("mintwarn").hidden = $("mintkey").checked;
  const line = document.getElementById("apiline");
  if (line) setMessageText(line, apiAccessMessageId());
};

// Final screen: base URL + the once-only secret, with copy buttons.
function showConnect(secret) {
  [1,2,3].forEach(i => { $("step"+i).hidden = true; $("s"+i).classList.add("on"); });
  setErrorText("");
  $("cbase").textContent = window.location.origin + "/v1";
  $("csecret").textContent = secret;
  $("step4").hidden = false;
}
document.querySelectorAll("[data-copy]").forEach(b => {
  b.onclick = async () => {
    // Never claim "Copied" when the write failed (no clipboard API on a
    // plain-http remote) — this is a once-only secret.
    try { await navigator.clipboard.writeText($(b.dataset.copy).textContent); setMessageText(b, "setup.step4.copied"); }
    catch { setMessageText(b, "setup.step4.select_copy"); }
    setTimeout(() => { setMessageText(b, "setup.step4.copy"); }, 1500);
  };
});
$("opendash").onclick = () => { window.location = "/"; };

$("wiz").onsubmit = async (ev) => {
  ev.preventDefault();
  $("finish").disabled = true;
  const body = {
    username: $("username").value.trim(),
    password: $("password").value,
    nim_keys: keys.map(k => ({ key: k.key, rpm: k.rpm })),
  };
  const base = $("baseurl").value.trim();
  if (base) body.base_url = base;
  if ($("mintkey").checked) body.create_client_key = { name: "default" };
  try {
    const r = await fetch("/setup", { method: "POST",
      headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
    if (r.ok) {
      const v = await r.json().catch(() => ({}));
      if (v.client_key && v.client_key.secret) { showConnect(v.client_key.secret); return; }
      window.location = "/";
      return;
    }
    const v = await r.json().catch(() => ({}));
    if (v.error && v.error.message) setErrorText(v.error.message);
    else setMessageText($("err"), "setup.step3.error.failed", { status: r.status });
  } catch (e) { setMessageText($("err"), "setup.step3.error.request", { error: e }); }
  $("finish").disabled = false;
};

catalogReady.then(() => {
  if (MSG) initializeSetup();
}).catch(() => {
  failInterface();
});

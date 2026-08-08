"use strict";

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

const message = (id, params = {}) => {
  let text = MSG[id] === undefined ? id : MSG[id];
  for (const key in params)
    text = text.split("{" + key + "}").join(String(params[key]));
  return text;
};
const I18N_TEXT_ATTRS = new Set(["title", "placeholder", "aria-label", "alt"]);

function assertMessageTextTarget(node) {
  const context = node.nodeType === Node.TEXT_NODE ? node.parentElement : node;
  if (!context
      || context instanceof HTMLScriptElement
      || context instanceof HTMLStyleElement
      || context instanceof SVGElement
      || context.closest?.("script,style,svg")) {
    throw new Error("forbidden catalog text target");
  }
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

function applyStatic(root) {
  root.querySelectorAll("[data-i18n]").forEach(node => {
    setMessageText(node, node.dataset.i18n);
  });
  root.querySelectorAll("[data-i18n-text]").forEach(node => {
    for (const child of node.childNodes) {
      if (child.nodeType === Node.TEXT_NODE && child.textContent.trim()) {
        setMessageText(child, node.dataset.i18nText);
        break;
      }
    }
  });
  root.querySelectorAll("[data-i18n-attr]").forEach(node => {
    for (const pair of node.dataset.i18nAttr.split(",")) {
      const separator = pair.indexOf(":");
      const attribute = pair.slice(0, separator);
      setMessageAttr(node, attribute, pair.slice(separator + 1));
    }
  });
}

(async () => {
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
  applyStatic(document);
  if (document.body.dataset.errorCode === "invalid_credentials") {
    const node = document.getElementById("login-error");
    setMessageText(node, "login.error.invalid_credentials");
    node.hidden = false;
  }
  document.body.hidden = false;
})().catch(() => {
  failInterface();
});

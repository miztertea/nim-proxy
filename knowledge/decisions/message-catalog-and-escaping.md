---
type: Decision
title: Route catalog ids and inert descriptors to context-owning sinks
description: Page code passes catalog ids to DOM sinks and branded inert descriptors to fixed-markup HTML builders; raw lookup is confined to canonical sink bodies.
tags: [i18n, dashboard, security, xss]
timestamp: 2026-07-30T00:00:00Z
---

# Route catalog ids and inert descriptors to context-owning sinks

## Context

The first catalog runtime escaped every message at lookup and exposed a second
plain lookup for `textContent` and `setAttribute`. Callers had to remember which
representation a helper accepted. One extra escape rendered entities; one
missing escape parsed markup. Structured setup copy also made HTML strings from
catalog placeholders.

An attempted replacement made `message()` public throughout page code and
tried to infer the eventual owner of every returned string. Independent review
repeatedly found fail-open shapes: string literals that spoofed helper names,
ASI-separated calls that inherited an earlier owner, blanket approval inside
named functions, fake canonical attribute writes, and script/style/SVG aliases.
Adding more partial JavaScript parsing failed the Ponytail ladder.

The owner approved a one-way replacement while the application is pre-1.0.
Runtime, both pages, validators, browser probes, guide invariant, and this
decision move atomically.

## Choice

Page code does not exchange ambiguous “catalog strings.” It uses two explicit
value flows:

1. DOM text, text-bearing attributes, and structured setup copy receive a
   catalog id plus parameters. Their canonical helper performs the lookup and
   immediately writes through its native DOM primitive.
   Native confirm/prompt dialogs are the one non-DOM text path: their exact
   canonical wrappers resolve an id and immediately pass it to the browser's
   text-only dialog primitive.
2. Fixed-markup dashboard builders receive a frozen, Symbol-branded descriptor
   from `catalogMessage(id, params)`. Its `Symbol.toPrimitive` throws on normal
   coercion, so a URL, style, or native attribute cannot silently stringify it.
   `escapeHtml()` is the sole named descriptor resolver and resolves and
   escapes in one operation.

`message(id, params)` still returns plain Unicode internally, but raw calls are
confined to the exact canonical sink bodies. There is no escaped/plain
compatibility lookup and no general plain catalog value in page code.

```js
function setMessageText(node, id, params = {}) {
  assertMessageTextTarget(node);
  node.textContent = message(id, params);
}

function setMessageAttr(node, attr, id, params = {}) {
  if (!I18N_TEXT_ATTRS.has(attr))
    throw new Error(`forbidden catalog attribute: ${attr}`);
  node.setAttribute(attr, message(id, params));
}

const CATALOG_MESSAGE = Symbol("catalog-message");
const catalogMessage = (id, params = {}) =>
  Object.freeze({
    type: CATALOG_MESSAGE,
    id,
    params: Object.freeze({ ...params }),
    [Symbol.toPrimitive]() {
      throw new TypeError("catalog descriptor requires escapeHtml");
    },
  });
```

`I18N_TEXT_ATTRS` is exactly `title`, `placeholder`, `aria-label`, and `alt`.
Text helpers validate the effective target before lookup, including a text
node's parent and active ancestors, and reject script, style, and SVG contexts.
Structured helpers also reject those node types, including descendants, in
every replacement/emphasis node before they resolve catalog text.

Structured messages never contain translator-authored HTML. Setup creates fixed
`<code>` or `<b>` nodes and splices them between catalog-owned text nodes with
`replaceChildren`. A replacement node is cloned for every placeholder
occurrence. The locale validator requires the source marker sequence, so a
locale may move emphasis but cannot drop, duplicate, or reorder its markers.

## Sink inventory

| Sink class | Accepted value | Owning primitive |
|---|---|---|
| Ordinary element or mixed text-node content | Catalog id + plain parameters | `setMessageText` → `textContent` |
| `title`, `placeholder`, `aria-label`, `alt` | Catalog id + plain parameters; `aria-label` is allowed on an ordinary SVG element | `setMessageAttr` → `setAttribute` |
| Structured setup copy | Catalog id + caller-created fixed nodes | text nodes + `replaceChildren` |
| Native confirm/prompt dialog | Catalog id + plain parameters | `confirmMessage` / `promptMessage` → browser dialog text |
| Fixed chart/table/card/list HTML | Branded catalog descriptor or plain machine data | `escapeHtml` → `innerHTML` |
| SVG geometry or raw SVG markup | Fixed markup, numeric geometry, fixed colors, and machine-formatted axis values | fixed builder or SVG DOM; catalog ids/descriptors forbidden except an allowlisted accessibility-text attribute after creation |
| URL-bearing values | Fixed URLs or validated machine/config data | catalog ids and descriptors forbidden |
| Style-bearing values | Fixed CSS tokens or bounded numeric layout values | catalog ids and descriptors forbidden |
| Event handlers | Repository functions | catalog ids and descriptors forbidden |
| Script, stylesheet text, raw SVG, native attribute bypass | Repository source or approved machine data | catalog ids and descriptors forbidden |

The inline `application/json` block is a transport container, not executable
script. Validators reject raw and entity-encoded markup before a catalog ships.
Task 5 may replace that transport without changing these value classes.

## Enforcement

- `locale_v1.py --selftest` distinguishes raw markup, entity-encoded markup,
  and invalid inline marker structure.
- `check_i18n.py --selftest` covers both allowed flows and hostile URL, style,
  event, script, CSS, SVG, native-attribute, raw-HTML, alias, string-spoof, and
  ASI mutations. It also sends descriptors directly to text, URL, style,
  native-attribute, and raw-SVG sinks while retaining the explicit
  `setMessageAttr(svg, "aria-label", id)` accessibility path.
- The resolver is a lexical `const`, not a `window` property. The normal source
  pass first blanks its declaration and the exact canonical raw-lookup bodies.
  Any remaining bare `message` identifier—including alias, `call`, or `bind`
  use—is `sink-context`. This intentionally bounded convention replaces owner
  inference.
- A settings request preserves a usable API `error` message verbatim. If an
  error response has no usable message, the exact `sPost` canonical body
  resolves the repository-owned `Request failed (HTTP {status}).` fallback;
  this prevents a hidden English template literal without rewriting API text.
- `render_check.js --escape-probe` mutates every value with hostile literal
  entity and markup text. It verifies descriptor inertness and HTML
  resolution, the four-attribute allowlist, literal text/attribute bytes,
  stable refusal of forbidden attributes and script/style/SVG text targets,
  repeated structured placeholders, and both pages' rendered DOM.

The source lint is a regression guard for the repository's direct conventions,
not a security verifier for an author deliberately obfuscating JavaScript with
computed properties, `Object.assign`, or native-method `.call`. Trusted source
can always route the plain string returned by `escapeHtml()` somewhere else.
Review remains responsible for such deliberate code. Runtime boundaries still
remove the accidental capability: the raw resolver is not global, descriptors
throw on coercion, and structured helpers validate both destinations and
replacement nodes.

See the [render gate](render-gate.md) and
[test strategy](../testing/test-strategy.md).

## Consequences

- Callers cannot accidentally choose an escaped versus plain catalog variant.
- Native DOM helpers own ordinary text and attributes.
- HTML builders receive a distinguishable value class and resolve it only at
  their escaping boundary.
- Catalog text cannot become URL, style, event, script, CSS, raw-SVG, or
  arbitrary native-attribute input.
- Machine data remains unlocalized. When fixed HTML displays model ids, client
  names, publisher names, metric values, API errors, endpoints, or credentials,
  `escapeHtml()` escapes that plain data without treating it as a descriptor.
- Adding another raw resolver reference or a helper that accepts an ambiguous
  string violates this decision.

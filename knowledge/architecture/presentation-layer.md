---
type: Component
title: Embedded presentation layer
description: Compile-time page and asset sources with explicit public/operator routing, strict same-origin CSP, and served-byte browser proof.
tags: [presentation, assets, dashboard, security, testing]
timestamp: 2026-07-30T00:00:00Z
---

# Embedded presentation layer

`src/presentation.rs` is the compile-time presentation boundary. `Page`
renders Dashboard, Login, or Setup HTML; `public_asset()` and
`operator_asset()` expose fixed byte slices with exact content types. The Rust
binary embeds every source under `src/web/`. There is no frontend package
manager, build step, runtime filesystem dependency, or network dependency.
The same module parses the one rich English catalog once and projects
ASCII-sorted plain-string public and operator wire catalogs from it.

## Source and route ownership

- Public assets are `/assets/public/public.css`, `setup.js`, `login.js`, and
  `locales/{locale}.json`.
  They are byte-identical before setup, after setup, and with a session. They
  contain no store values, credentials, user data, operator catalog, or
  `settings.*` ids.
- Operator assets are `/assets/operator/operator.css`, `shared.js`,
  `dashboard.js`, `settings.js`, and `locales/{locale}.json`. They are
  registered inside the same post-setup session gate as `/` and `/dash`; that
  gate runs before locale lookup.
- `GET /api/locale-bootstrap` is public and returns exactly the compiled
  production registry (`en-US`) plus the validated persisted server default.
  Public pages then fetch the public catalog
  projection—every `setup.*`, every `login.*`, and only
  `common.app_name`—while the operator page fetches the complete catalog.
  There is deliberately no icon route: the fixed local SVG is compiled into
  the Dashboard page.

Every page and asset response, including redirects and gate rejections,
carries `Cache-Control: no-store`. This avoids cross-version HTML/asset skew
without content hashing. Asset paths are intentionally absent from OpenAPI;
they are browser resources, not API contracts.

## Browser boundary

Pages contain no executable inline script, inline stylesheet, style attribute,
or event-handler attribute. Fonts use system stacks. CSP allows only
same-origin images, styles, scripts, and connections, plus data images.

Charts still need runtime widths, positions, and palette colors. Generated
markup carries inert `data-style` declarations. `shared.js` validates every
property and value, inserts a deduplicated class rule into the already-loaded
same-origin operator stylesheet, applies the generated class, and removes the
inert attribute. The cache and generated rule set are capped at 512 entries.
At the cap, the bridge compacts to declarations still owned by live DOM,
rewrites those nodes to the replacement classes, and then admits the pending
declaration. If the live set itself consumes the bound, it refuses the new
declaration before changing existing rules. Catalog values never enter this
path.

Login accepts only the fixed `invalid_credentials` error code.
`presentation.rs` maps unknown codes to no message; `login.js` selects
repository-owned text and writes it with `textContent`. GET/POST statuses,
redirects, and cookies remain owned by `auth.rs`.

Every page begins hidden and renders only after bootstrap and catalog schema
validation. Operator startup resolves bootstrap → authenticated `/api/config`
→ current-user override or server default or `en-US` → exactly one installed
operator catalog. It does not inspect browser languages or request headers.
Public setup/login remain on the server default. Dashboard application assets
and API polling start only after the operator catalog resolves. Once boot code
is running, bootstrap, config, catalog, or application-asset failure reveals only
`NIM Proxy interface failed to load.`, logs no response body, and starts no
later application request.

## Proof

`presentation_assets_are_gated` sends real requests in pre-setup, anonymous
configured, and authenticated states and pins status, content type, CSP,
`no-store`, public-byte stability, and private-sentinel absence. The 34-row
route inventory and real behavior matrix cover the nine asset routes.

`render_check.js --assets-only` rejects external origins and inline
active/style contexts with tag/attribute and CSS-context parsing rather than
attribute-order-sensitive spelling checks. Its self-test covers direct and
protocol-relative URLs, reordered/unquoted attributes, `srcset`, quoted
`@import`, font URLs, and ordinary CSS URLs.
`--served-page-selftest` rejects private source-file page assembly and requires
the probe flow to read and record the binary's page and catalog response
bodies.
Normal browser mode starts the current binary, requests its real page and
asset routes, fails any missing/error/external initial resource, and fulfills
only Rust-generated `tests/fixtures/ui/` API responses through CDP. The 69-row
`--all-states` matrix names file or `scenarios.json#scenario` consumption,
exact ordered requests, DOM observations, and clean browser observations; its
loading row holds the real bootstrap response. It covers these interactions,
not layout or every application path. It then drives every dashboard tab/chart
hover or setup step under the production CSP. It also forces more than twice
the dynamic-style bound, proves compaction happened, pins the rule/cache
limit, and verifies live-node geometry survived. Hostile and pseudolocale
probes first receive the real catalog-route response and then re-fulfill only
those response bytes.
Bootstrap/catalog failure, malformed-schema, and delayed-catalog probes pin the
startup ordering and emergency-only behavior; page HTML, scripts, and styles
remain production-served bytes.

Every browser result owns its process lifecycle. Shutdown first asks Chromium
to close through CDP, then uses a bounded process-group kill only if the
browser tree does not exit. The proxy is stopped, the run directory is removed
and verified absent, and cleanup failure fails the command rather than being
downgraded beneath a green page result. `--cleanup-selftest` uses real
processes and a continuing profile writer to force the fallback path and prove
no `nimproxy-render-*` directory survives. It separately forces a proxy
startup timeout so the pre-return child is observed stopped before directory
removal, and requires intercepted presentation failures to retain their
specific diagnostic.

See [dashboard](dashboard.md), [HTTP trust boundaries](http-trust-boundary-map.md),
[render gate](../decisions/render-gate.md), and
[test strategy](../testing/test-strategy.md).

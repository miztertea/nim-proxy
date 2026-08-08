---
type: Decision
title: Remove the estimated-savings metric and pricing configuration
description: "Dollars saved" needed a published per-model rate to be honest; one reference rate for all tokens is not a measurement. Deleted the metric, the config, and the route rather than fixing an unfixable number.
tags: [dashboard, metrics, settings, breaking-change]
timestamp: 2026-07-29T00:00:00Z
---

# Remove the estimated-savings metric and pricing configuration

## Context

The dashboard showed a **Dollars saved** KPI on Overview, plus a `Saved` column
in the Models, Clients, and Reliability tables. It multiplied prompt and
completion tokens by two operator-configured reference rates
(`ref_price_in`, `ref_price_out`) stored in a `Pricing` config block and edited
through `/api/settings/pricing`.

The number was never trustworthy. A meaningful "saved" figure is the difference
between what you paid and what *the same model* would have cost elsewhere, which
requires a published per-model rate for every model in the pool. Instead a
single pair of rates was applied to every model, so a run against a small model
and a run against a large one contributed identically. The figure moved with
traffic volume and looked precise — two decimal places and a currency symbol —
while measuring nothing in particular.

It also anchored the only currency in the product, which would have forced a
`Intl.NumberFormat` currency decision (which currency? whose locale? the
operator's or the viewer's?) during the localization work in this same release.

## Options

1. **Keep it, document the caveat.** The tooltip already said "vs reference
   pricing." Nobody reads a caveat on a number rendered in dollars.
2. **Fix it with per-model rates.** Correct, but requires a maintained rate
   table for every NIM model, kept current against someone else's pricing page.
   That is a product, and nobody asked for it.
3. **Make it token-denominated** — "tokens served" rather than dollars. But
   that is just `Tokens out`, which the dashboard already shows.
4. **Remove it.**

## Choice

**Option 4.** Deleted the KPI tile, the three `Saved` table columns, the
`money()` helper and its `$` prefix, the `Pricing` settings card, the
`Pricing` config struct with `ref_price_in` / `ref_price_out`, the
`/api/settings/pricing` route and handler, and the pricing validation branch.

`REF_PRICE_IN` and `REF_PRICE_OUT` stay in the legacy-env warning list in
`lib.rs`. An operator upgrading with them still set should be told they are
ignored — that list exists for exactly this.

## Consequences

- **`/api/settings/pricing` is gone** (404 after upgrade), and `/api/config` no
  longer carries a `server.pricing` object. `/api/dashboard/now` no longer
  emits `price_in` / `price_out`.
- **Existing config stores load unchanged.** `StoredConfig` has no
  `deny_unknown_fields`, so a `pricing` block written by 0.6.5 or earlier is
  ignored rather than rejected. No migration runs, and nothing rewrites the
  store until the next settings save. Covered by
  `config_with_a_legacy_pricing_block_still_loads`.
- The API surface shrinks from 13 `/api/*` routes to 12 before the OpenAPI spec
  is generated in the same release, so the spec never documents this route.
- The currency-formatting question disappears rather than being answered, which
  removes it from the `Intl` migration entirely.

---
type: Research Finding
title: /v1/models returns only the OpenAI minimum
description: id, created, object, owned_by — no descriptions, logos, or capability metadata.
tags: [nim, api-schema, dashboard]
timestamp: 2026-07-02T00:00:00Z
resource: https://docs.nvidia.com/nim/large-language-models/latest/reference/api-reference.html
---

# /v1/models returns only the OpenAI minimum

NIM's catalog endpoint follows the standard OpenAI schema exactly: each entry
carries `id`, `created`, `object: "model"`, and `owned_by` — nothing else. No
display names, descriptions, publisher logos, context lengths, or capability
flags.

Implications for the dashboard's model cards:

- Visual identity is derived **locally from the model id namespace**
  (`meta/…`, `deepseek-ai/…` → a static publisher map with display name and
  brand color).
- Real logos come from the LobeHub AI-icons CDN at render time in the
  *browser*, with the brand-colored monogram as offline fallback — the
  container itself makes no extra network calls.
- Model "descriptions" from build.nvidia.com would require scraping; rejected.

See [dashboard](../architecture/dashboard.md).

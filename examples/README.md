# Example configs

## `opencode.json` — GLM-5.2 via nim-proxy

A ready-to-use [OpenCode](https://opencode.ai) config pointed at a local
nim-proxy (`http://localhost:8000/v1`) and tuned for **GLM-5.2**, Z.ai's
long-context agentic-coding model on the [NVIDIA NIM catalog](https://build.nvidia.com/z-ai).

### Install

Copy it to your project root (or `~/.config/opencode/opencode.json` for
global use) and set the API key OpenCode sends to the proxy:

```sh
cp examples/opencode.json ./opencode.json
export NIM_PROXY_KEY=your-proxy-secret   # a client API key (npk_…) you minted in
                                         # Settings when the API is in keyed mode,
                                         # or any non-empty string in open mode
opencode
```

Confirm the model id your NIM account actually serves — the proxy is a strict
pass-through, so the key under `models` must match NIM exactly:

```sh
curl -s localhost:8000/v1/models | grep -i glm
```

If it returns a different id (e.g. `z-ai/glm-5.1`, or GLM-5.2 isn't on the
hosted free API yet), change the `models` key and the `model` reference to match.

### Why these settings

| Setting | Value | Rationale |
|---|---|---|
| `options.timeout` | `false` | **The important one.** nim-proxy holds the connection open with SSE heartbeats while it waits out NIM's 40 RPM limit. With a client-side timeout, OpenCode would abort mid-wait and defeat the proxy's whole purpose. Disable it and let the proxy pace. |
| `options.baseURL` | `http://localhost:8000/v1` | Points at the proxy, not NIM directly. Change the port if you set a non-default `PORT`. |
| `options.apiKey` | `{env:NIM_PROXY_KEY}` | The SDK requires a key. In keyed mode this is a client API key (`npk_…`) you generate in the dashboard Settings; in open mode any non-empty value works. |
| `limit.context` | `131072` (128k) | GLM-5.2's card advertises a 1M-token window, but NIM's **hosted** endpoint has historically served GLM at 128k. 128k is the safe floor and is plenty for agentic coding. Because OpenCode auto-compacts *below* this number, setting it conservatively means compaction fires before NIM can reject an over-length request. Raise it toward 1M only if testing confirms the hosted window is larger. |
| `limit.output` | `32768` | OpenCode silently caps `limit.output` at 32k ([issue #29363](https://github.com/anomalyco/opencode/issues/29363)), so this is the effective ceiling. Generous headroom for GLM-5.2's reasoning/"thinking" tokens, which count toward output. |
| `options.temperature` | `0.6` | Balanced for agentic coding — deterministic enough to follow tool schemas, loose enough to reason. Bump toward `1.0` (Z.ai's general-purpose default) for more exploratory work. |
| `options.top_p` | `0.95` | Z.ai's recommended nucleus-sampling value for GLM. |
| `compaction` | `auto`/`prune`/`reserved: 24000` | Auto-compact keeps long sessions under the window; `prune` drops stale tool outputs; `reserved` leaves ~24k tokens free so a compaction summary plus the next response never overflow. **Option names have changed across OpenCode releases** — if your version ignores this block, check `opencode.ai/docs/config`; the real lever is `limit.context` above, which every version honors. |
| `small_model` | GLM-5.2 | Title/summary generation stays on the same provider so you don't need a second key. It's cheap; the proxy answers `/v1/models` from cache so it costs no rate budget. |

### Notes

- **Rate budget**: one NIM key = 40 RPM. Long agentic runs on GLM-5.2 (which
  emits many reasoning tokens) go faster with more keys added in the dashboard
  Settings — the proxy load-balances across them. Watch utilization on the
  dashboard.
- **Thinking effort**: GLM-5.2 supports variable thinking effort. This example
  omits a `reasoning_effort` parameter because NIM may reject unknown fields
  with a 400; add it only after confirming your NIM endpoint accepts it.

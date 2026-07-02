---
type: Decision
title: Conversation affinity with least-loaded spillover
description: A conversation prefers one key every turn; when that lane is full the request spills to the least-loaded ready lane.
tags: [routing, prefix-cache, pool]
timestamp: 2026-07-02T00:00:00Z
---

# Conversation affinity with least-loaded spillover

## Context

NIM's serving stack supports [KV cache reuse](../research/nim-kv-cache-reuse.md)
(~2x faster TTFT on repeated prefixes — exactly the agent pattern of same
system prompt + growing history). If the hosted tier has it enabled, caches
are almost certainly scoped per account/key for isolation, so switching keys
mid-conversation forfeits hits. Crossing keys is always *correct* (the chat
API is stateless; full history rides every request) — only latency is at stake.

Separately, naive "first available lane" selection concentrated a cold-start
burst 40/10/0 across three keys, risking any undocumented per-key concurrency
ceiling.

## Options

1. Round-robin (cache-hostile, simple).
2. Least-loaded only (spreads well, cache-hostile).
3. **Sticky by conversation hash, spill to least-loaded when full.**

## Choice

Option 3. Conversation identity = hash of model + system prompt + first user
message (stable across every turn of a session; two messages, not one,
because all OpenCode sessions share a system prompt). Preferred lane wins
while it has capacity; otherwise least-loaded ready lane. Throughput beats
cache locality under saturation: a cold cache costs ~1s of TTFT, waiting for
the sticky lane could cost 60.

## Consequences

- Empirically observable: `nimproxy_affinity_total{result="sticky"|"spill"}`
  feeds the dashboard's "conversation stickiness" tile, which can also
  empirically answer whether hosted NIM prefix-caches at all (compare sticky
  vs spill TTFT).
- Burst spread verified in unit tests (9 requests → 3/3/3) and e2e (same
  conversation pins, distinct conversations spread).
- Context compaction in a harness changes the first user message and thus the
  lane — accepted, affinity is best-effort.

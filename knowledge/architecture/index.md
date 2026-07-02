---
type: Index
title: Architecture
description: How each component works and why it's shaped that way.
---

# Architecture

Request lifecycle: client → [client-auth](client-auth.md) →
[dispatcher](dispatcher.md) (FIFO slot queue) → [key-pool](key-pool.md)
(per-key sliding window) → upstream, with the
[streaming-pipeline](streaming-pipeline.md) keeping the client alive
throughout. Everything is measured into [metrics-history](metrics-history.md)
and rendered by the [dashboard](dashboard.md).

- [key-pool](key-pool.md)
- [dispatcher](dispatcher.md)
- [streaming-pipeline](streaming-pipeline.md)
- [metrics-history](metrics-history.md)
- [dashboard](dashboard.md)
- [client-auth](client-auth.md)

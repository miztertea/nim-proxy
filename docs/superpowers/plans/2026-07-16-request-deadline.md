# Explicit Request Deadline Implementation Plan

> **For agentic workers:** Implement inline with test-driven development; no subagents are needed for this maintenance change.

**Goal:** Add an opt-in `X-Nim-Proxy-Deadline-Ms` absolute deadline that cancels buffered and streaming work and reports a distinct outcome.

**Architecture:** Parse one checked absolute deadline in `proxy::handle`. Race the whole buffered request future and the spawned streaming workflow against that deadline so dropping the losing future releases existing RAII-owned resources. Keep all current behavior when the header is absent.

**Tech Stack:** Rust, Axum, Tokio, reqwest, existing real-binary e2e harness.

## Global Constraints

- Build from integration commit `28ed638`, which contains the `crossbeam-epoch` security fix.
- Do not change default patient behavior without the header.
- Do not add dependencies or change lane/governor policy.
- Invalid header details are returned only after normal client authentication.
- Update the OKF knowledge graph and log with the behavior change.

### Task 1: Deadline contract and lifecycle enforcement

**Files:**
- Modify: `tests/support/mod.rs`
- Modify: `tests/e2e.rs`
- Modify: `src/proxy.rs`

**Interfaces:**
- Consume request header `X-Nim-Proxy-Deadline-Ms` as canonical unsigned milliseconds.
- Produce buffered `400 invalid_deadline` or `504 deadline_exceeded` responses.
- Produce streaming SSE `deadline_exceeded` errors after the committed 200.
- Produce request status `deadline` and `nimproxy_deadline_exceeded_total`.

- [ ] Add deterministic mock behaviors for delayed headers and periodically active response bodies.
- [ ] Add focused e2e tests for invalid/duplicate input, buffered expiry and resource release, active-stream expiry, metrics, and header-free compatibility.
- [ ] Run each focused test and confirm it fails because deadline support is absent.
- [ ] Implement canonical parsing plus buffered and streaming deadline races in `src/proxy.rs`.
- [ ] Re-run focused tests until green, then run `cargo test`.

### Task 2: Operator contract and repository memory

**Files:**
- Modify: `README.md`
- Modify: `CHANGELOG.md`
- Create: `knowledge/decisions/explicit-request-deadline.md`
- Modify: `knowledge/architecture/streaming-pipeline.md`
- Modify: `knowledge/index.md`
- Modify: `knowledge/log.md`

- [x] Document the header, buffered/streaming outcomes, metric, cancellation semantics, and alternatives.
- [x] Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `git diff --check`, and the repository's knowledge-link checks if present.
- [x] Review the final diff for secret exposure, unrelated changes, and divergence from the approved design.

---
type: Runbook
title: Capture sanitized NIM response evidence
description: Bounded four-case capture, deterministic sanitization, human review, and exact raw cleanup.
tags: [nim, testing, privacy]
timestamp: 2026-08-01T00:00:00Z
---

# Capture sanitized NIM response evidence

This developer-only procedure records the response structure needed by the
side-band observer without committing credentials, endpoints, model ids,
prompts, completions, provider ids, or raw values. The raw directory always
stays outside the repository. Only the four sanitized fixtures and their
manifest may enter Git.

## Boundaries

- Run one invocation containing exactly `buffered-basic`, `streamed-basic`,
  `buffered-tools`, and `streamed-tools`, in that order. The tool sends one
  request per case and never retries.
- Each response stops at 2 MiB, 2,048 SSE events, or 60 seconds. Truncation is
  evidence; do not follow it with an exploratory request.
- The base URL is the exact OpenAI-compatible `/v1` service root. Direct
  endpoints require verified HTTPS. Plain HTTP is accepted only for a literal
  loopback host such as a local nim-proxy. A host root without `/v1` is
  rejected before the output directory or network is touched.
- Supply only `NIM_CAPTURE_BASE_URL`, `NIM_CAPTURE_BEARER_TOKEN`, and
  `NIM_CAPTURE_MODEL` through the process environment. Do not put their values
  in arguments, a repository file, shell tracing, logs, an issue, or a PR.
- A missing credential or unsupported case is a release blocker or an
  explicit `unavailable` evidence row. Never manufacture a response shape.

## Prepare an outside-repository directory

The capture target must not exist. Select an explicit operator-approved
absolute temporary base outside the repository; do not inherit `TMPDIR`.
Create an owner-only parent there and reserve its fixed child name without
creating that child:

```sh
NIM_CAPTURE_BASE=/tmp
NIM_REPOSITORY_ROOT="$(git rev-parse --show-toplevel)"
test "${NIM_CAPTURE_BASE#/}" != "$NIM_CAPTURE_BASE"
test -d "$NIM_CAPTURE_BASE"
test ! -L "$NIM_CAPTURE_BASE"
NIM_CAPTURE_PARENT="$(mktemp -d "$NIM_CAPTURE_BASE/nim-proxy-capture.XXXXXX")"
chmod 700 "$NIM_CAPTURE_PARENT"
NIM_CAPTURE_DIR="$NIM_CAPTURE_PARENT/raw"
test "${NIM_CAPTURE_DIR#/}" != "$NIM_CAPTURE_DIR"
test ! -e "$NIM_CAPTURE_DIR"
test ! -L "$NIM_CAPTURE_DIR"
case "$(realpath -m -- "$NIM_CAPTURE_DIR")/" in
  "$(realpath -- "$NIM_REPOSITORY_ROOT")/"*) exit 1 ;;
esac
```

Set the three required `NIM_CAPTURE_*` environment values without enabling
`set -x`. The base URL is the exact OpenAI-compatible service root, including
its `/v1` path (for example, `http://127.0.0.1:8000/v1` for a local default
instance). The bearer token is a deliberately authorized direct or local
client credential, and the model is the one bounded observation target.

## Capture the fixed set

```sh
python3 scripts/capture_nim.py --output-dir "$NIM_CAPTURE_DIR" \
  --case buffered-basic --case streamed-basic \
  --case buffered-tools --case streamed-tools
```

Success creates one mode-0700 directory with a mode-0600 version marker and
four mode-0600 raw envelopes. Do not print or open those envelopes during
normal operation. Validate only names, types, modes, and the public path:

```sh
test ! -L "$NIM_CAPTURE_DIR"
test "$(realpath -- "$NIM_CAPTURE_DIR")" = "$NIM_CAPTURE_DIR"
test "$(stat -c '%a' "$NIM_CAPTURE_DIR")" = 700
test "$(stat -c '%a' "$NIM_CAPTURE_DIR/.nim-capture-raw-v1")" = 600
test "$(stat -c '%a' "$NIM_CAPTURE_DIR/buffered-basic.raw.json")" = 600
test "$(stat -c '%a' "$NIM_CAPTURE_DIR/streamed-basic.raw.json")" = 600
test "$(stat -c '%a' "$NIM_CAPTURE_DIR/buffered-tools.raw.json")" = 600
test "$(stat -c '%a' "$NIM_CAPTURE_DIR/streamed-tools.raw.json")" = 600
```

The sanitizer independently rejects an extra, missing, symlinked, nonregular,
wrong-owner, or wrong-mode entry, so filename inspection is not the security
boundary.

## Sanitize and verify

```sh
python3 scripts/sanitize_nim_capture.py \
  --input-dir "$NIM_CAPTURE_DIR" \
  --output-dir tests/fixtures/nim-observations
python3 scripts/sanitize_nim_capture.py \
  --check tests/fixtures/nim-observations
```

The update builds and validates a complete same-parent staging set, replaces
fixtures with fixed temporary names, writes the manifest last, syncs, and
checks that the requested public path still names the updated directory. A
failed update is never accepted by `--check` as current evidence.

Inspect every sanitized fixture and `manifest.json` before staging. Confirm:

- the fixture fields are exactly `format`, `case`, `capture_date`, `transport`,
  `status`, `content_type`, `truncated`, and `body`;
- strings and ids are redacted placeholders, while only the approved JSON/SSE
  topology and equality relationships remain;
- no URL, credential prefix, prompt, completion prose, email-like value, long
  opaque id, raw model/account name, or provider identity remains;
- every manifest evidence row is either `captured` with fixture references or
  `unavailable` with `not_observed`, `request_rejected`, or
  `unsupported_by_model`.

`captured` means this four-request run directly contained the shape. It does
not claim every NIM model or future provider version has that shape.

## Current evidence set

The 2026-08-01 evidence set uses sanitizer version 1 and contains four
successful HTTP 200 observations: buffered basic, streamed basic, buffered
tools, and streamed tools. Both buffered responses contain usage. Both SSE
responses begin with a redacted comment, retain per-event `usage: null`, end
with a usage-only event, and then `[DONE]`. The basic stream progresses from a
null finish reason to `stop`; null is non-terminal and is not conflicting
evidence. The tool responses retain the buffered `message.tool_calls` and
streamed `delta.tool_calls` topology, one stable redacted id relationship, and
the `tool_calls` finish reason.

The manifest marks only directly observed shapes as captured. Additional or
partial usage fields, multi-line data, repeated tool fragments, multiple
choices, errors, malformed or truncated streams, unknown finish reasons, and
the unobserved standard finish reasons remain explicitly unavailable. The
model, provider, client credential, prompt, and generated content are not part
of the committed evidence.

## Remove the raw evidence exactly

Delete raw files only after sanitized review succeeds. The capture tool's
cleanup mode reopens the path component by component without following links,
retains the parent and raw-directory descriptors, validates the exact marker,
names, ownership, modes, and inode identities, opens entries nonblocking, and
unlinks only fixed names through the raw descriptor:

```sh
python3 scripts/capture_nim.py \
  --cleanup --output-dir "$NIM_CAPTURE_DIR"
```

Success means the held raw-directory descriptor was synced and verified empty.
The empty `NIM_CAPTURE_DIR` and `NIM_CAPTURE_PARENT` contain no evidence and are
deliberately outside the cleanup authority; system temporary-directory policy
may retire them. If validation or deletion fails, stop. A partial filesystem
failure is deliberately not auto-retried because the set is no longer exact;
do not use shell deletion, follow a changed path, or broaden the target. Retain
the path for deliberate descriptor-safe operator recovery.

## Reusable proof

```sh
python3 scripts/capture_nim.py --selftest
python3 scripts/sanitize_nim_capture.py --selftest
python3 scripts/sanitize_nim_capture.py \
  --check tests/fixtures/nim-observations
```

The self-tests are service-free adversarial boundary proof. The checked,
human-reviewed fixtures are real observation evidence. Neither substitutes for
the other.

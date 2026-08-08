#!/usr/bin/env python3
"""Sanitize bounded raw NIM captures into deterministic secret-free fixtures."""

import argparse
import base64
import contextlib
import datetime
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import tempfile


CASES = ("buffered-basic", "buffered-tools", "streamed-basic", "streamed-tools")
RAW_FIELDS = (
    "format", "case", "capture_date", "transport", "status", "content_type",
    "truncated", "body_b64",
)
FIXTURE_FIELDS = (
    "format", "case", "capture_date", "transport", "status", "content_type",
    "truncated", "body",
)
EVIDENCE = (
    "additional_usage_fields", "buffered_success", "buffered_tool_calls",
    "comments_keepalives", "conflicting_terminal_reasons",
    "known_finish_reason:content_filter", "known_finish_reason:function_call",
    "known_finish_reason:length", "known_finish_reason:stop", "known_finish_reason:tool_calls",
    "malformed_json_event", "missing_usage", "multi_line_data", "multiple_choices",
    "partial_usage", "repeated_tool_call_fragments", "standard_streamed_success",
    "streamed_tool_calls", "truncated_stream", "unknown_finish_reason",
    "upstream_json_error", "upstream_sse_error", "usage_only_final_sse_event",
)


class SanitizeError(ValueError):
    """A deliberately value-free diagnostic for untrusted capture input."""


def raw_envelope(body, **forbidden):
    envelope = {
        "format": "nim-capture-raw-v1",
        "case": "streamed-basic",
        "capture_date": "2026-08-01",
        "transport": "sse",
        "status": 200,
        "content_type": "text/event-stream",
        "truncated": False,
        "body_b64": base64.b64encode(body).decode("ascii"),
    }
    envelope.update(forbidden)
    return envelope


def write_raw(path, envelope):
    path.write_text(json.dumps(envelope, sort_keys=True), encoding="utf-8")


def parse_raw_capture(text):
    """Read only the allowlisted raw envelope fields, without exposing values."""
    try:
        raw = json.loads(text)
    except (TypeError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SanitizeError("capture-input-invalid") from error
    if not isinstance(raw, dict) or any(field not in raw for field in RAW_FIELDS):
        raise SanitizeError("capture-input-invalid")
    if raw["format"] != "nim-capture-raw-v1" or raw["case"] not in CASES:
        raise SanitizeError("capture-input-invalid")
    if not isinstance(raw["capture_date"], str) or raw["transport"] not in {"buffered", "sse"}:
        raise SanitizeError("capture-input-invalid")
    try:
        datetime.date.fromisoformat(raw["capture_date"])
    except (TypeError, ValueError) as error:
        raise SanitizeError("capture-input-invalid") from error
    if not isinstance(raw["status"], int) or isinstance(raw["status"], bool) or not 100 <= raw["status"] <= 599:
        raise SanitizeError("capture-input-invalid")
    if not isinstance(raw["content_type"], str) or not isinstance(raw["truncated"], bool):
        raise SanitizeError("capture-input-invalid")
    if not isinstance(raw["body_b64"], str):
        raise SanitizeError("capture-input-invalid")
    try:
        body = base64.b64decode(raw["body_b64"], validate=True)
        body.decode("utf-8")
    except (UnicodeDecodeError, ValueError) as error:
        raise SanitizeError("capture-input-invalid") from error
    return {field: raw[field] for field in RAW_FIELDS}, body


def raw_capture(raw_path):
    try:
        return parse_raw_capture(raw_path.read_text("utf-8"))
    except OSError as error:
        raise SanitizeError("capture-input-invalid") from error


def _number_placeholder(value, numbers):
    if isinstance(value, float) and (value != value or value in (float("inf"), float("-inf"))):
        raise SanitizeError("capture-body-invalid")
    signature = (number_class(value), repr(value))
    if signature in numbers:
        return numbers[signature]
    position = 1 + sum(1 for item in numbers if item[0] == signature[0])
    sign, kind = signature[0]
    if sign == "zero":
        replacement = 0 if kind == "integer" else 0.0
    elif sign == "positive":
        replacement = position if kind == "integer" else position + 0.5
    else:
        replacement = -position if kind == "integer" else -(position + 0.5)
    numbers[signature] = replacement
    return replacement


def _identifier_placeholder(value, identifiers):
    if value not in identifiers:
        identifiers[value] = f"id-{len(identifiers) + 1}"
    return identifiers[value]


def sanitize_json_value(value, key=None, numbers=None, identifiers=None, context="root"):
    """Keep only the JSON information the fixture contract permits."""
    numbers = {} if numbers is None else numbers
    identifiers = {} if identifiers is None else identifiers
    if isinstance(value, tuple) and value[0] == "object":
        if context == "error":
            return ("object", [])
        allowed = {
            "root": {"usage", "choices", "error"},
            "choice": {"index", "finish_reason", "message", "delta"},
            "message": {"tool_calls"},
            "delta": {"tool_calls"},
            "tool": {"index", "id", "tool_call_id", "type", "function"},
            "function": {"name", "arguments"},
        }.get(context)
        projected = []
        for name, item in value[1]:
            if allowed is not None and name not in allowed:
                continue
            child_context = {
                "usage": "usage", "choices": "choice", "error": "error",
                "message": "message", "delta": "delta", "tool_calls": "tool",
                "function": "function",
            }.get(name, context)
            projected.append((name, sanitize_json_value(
                item, name, numbers, identifiers, child_context
            )))
        return ("object", projected)
    if isinstance(value, list):
        return [sanitize_json_value(item, None, numbers, identifiers, context) for item in value]
    if value is None or isinstance(value, bool):
        return value
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return _number_placeholder(value, numbers)
    if isinstance(value, str):
        if key == "finish_reason":
            return value if value in KNOWN_FINISH else "__other__"
        if key in {"id", "tool_call_id"}:
            return _identifier_placeholder(value, identifiers)
        return "<redacted>"
    raise SanitizeError("capture-body-invalid")


def encode_json_value(value):
    """Encode pair-preserving JSON as deterministic, ASCII-only compact text."""
    if isinstance(value, tuple) and value[0] == "object":
        return "{" + ",".join(
            json.dumps(name, ensure_ascii=True, separators=(",", ":")) + ":" + encode_json_value(item)
            for name, item in value[1]
        ) + "}"
    if isinstance(value, list):
        return "[" + ",".join(encode_json_value(item) for item in value) + "]"
    return json.dumps(value, ensure_ascii=True, separators=(",", ":"), allow_nan=False)


def sanitize_json_body(body, numbers=None, identifiers=None):
    value = json_pairs(body)
    if value is None:
        raise SanitizeError("capture-body-invalid")
    return encode_json_value(sanitize_json_value(value, numbers=numbers, identifiers=identifiers))


def _line_prefix(line):
    name, separator, value = line.partition(":")
    return name, separator, value


def _repartition_data(text, count):
    """Retain data-line count while keeping joined JSON valid via trailing whitespace."""
    if count == 1:
        return [text]
    return [text] + [""] * (count - 1)


def sanitize_sse_body(body):
    try:
        source = body.decode("utf-8")
    except UnicodeDecodeError as error:
        raise SanitizeError("capture-body-invalid") from error
    newline = "\r\n" if "\r\n" in source else "\n"
    normalized = source.replace("\r\n", "\n")
    rendered_segments = []
    numbers, identifiers = {}, {}
    for segment in normalized.split("\n\n"):
        if not segment:
            rendered_segments.append("")
            continue
        lines = segment.split("\n")
        data_indexes = [index for index, line in enumerate(lines) if line.startswith("data:")]
        data = [lines[index].partition(":")[2].lstrip() for index in data_indexes]
        joined = "\n".join(data)
        if joined == "[DONE]":
            sanitized = "[DONE]" if len(data) == 1 else "<redacted>"
        else:
            parsed = json_pairs(joined.encode("utf-8"))
            sanitized = encode_json_value(sanitize_json_value(parsed, numbers=numbers, identifiers=identifiers)) if parsed is not None else "<redacted>"
        replacement_data = iter(_repartition_data(sanitized, len(data)))
        rendered = []
        for line in lines:
            name, separator, value = _line_prefix(line)
            if name == "data" and separator:
                rendered.append("data: " + next(replacement_data))
            elif name == "":
                rendered.append(": <redacted>")
            elif name in {"event", "id", "retry"} and separator:
                rendered.append(name + ": <redacted>")
            else:
                rendered.append("field: <redacted>")
        rendered_segments.append("\n".join(rendered))
    return ("\n\n".join(rendered_segments)).replace("\n", newline)


def sanitized_body_text(raw, body):
    """Choose the protected body representation from trusted capture metadata."""
    return sanitize_json_body(body) if raw["transport"] == "buffered" else sanitize_sse_body(body)


def candidate_from_raw(raw, body):
    """Sanitize a parsed allowlisted raw envelope into the stable fixture API."""
    body_text = sanitized_body_text(raw, body)
    fixture = {
        "format": "nim-observation-v1",
        "case": raw["case"],
        "capture_date": raw["capture_date"],
        "transport": raw["transport"],
        "status": raw["status"],
        "content_type": raw["content_type"],
        "truncated": raw["truncated"],
        "body": body_text,
    }
    return {"filename": raw["case"] + ".json", "fixture": fixture}


def candidate_sanitize(raw_path):
    """Sanitize one raw envelope into the stable fixture candidate API."""
    raw, body = raw_capture(raw_path)
    return candidate_from_raw(raw, body)


def fixture_bytes(fixture):
    return (json.dumps(fixture, ensure_ascii=True, sort_keys=True, separators=(",", ":")) + "\n").encode("ascii")


def _fixture_payloads(fixture):
    body = fixture["body"].encode("ascii")
    parsed = json_pairs(body)
    if parsed is not None:
        return [parsed]
    payloads = []
    normalized = body.replace(b"\r\n", b"\n")
    for segment in normalized.split(b"\n\n"):
        data = [line.partition(b":")[2].lstrip() for line in segment.split(b"\n") if line.startswith(b"data:")]
        value = json_pairs(b"\n".join(data))
        if value is not None:
            payloads.append(value)
    return payloads


def _key_strings(value, keys):
    if isinstance(value, tuple) and value[0] == "object":
        return [item for name, child in value[1] for item in (
            ([child] if name in keys and isinstance(child, str) else []) + _key_strings(child, keys)
        )]
    if isinstance(value, list):
        return [item for child in value for item in _key_strings(child, keys)]
    return []


def _observed_evidence(fixtures):
    """Return only facts directly present in sanitized fixture structure."""
    found = {name: set() for name in EVIDENCE}
    for filename, fixture in fixtures.items():
        body = fixture["body"].encode("ascii")
        payloads = _fixture_payloads(fixture)
        transport = fixture["transport"]
        has_error = any(pairs_get(payload, "error") for payload in payloads)
        if transport == "buffered" and fixture["status"] < 400 and payloads and not has_error:
            found["buffered_success"].add(filename)
        if transport == "sse" and fixture["status"] < 400 and payloads and not has_error:
            found["standard_streamed_success"].add(filename)
        signature = sse_signature(body) if transport == "sse" else None
        if signature is not None:
            _, terminated, events = signature
            if any(event[1] > 1 for event in events):
                found["multi_line_data"].add(filename)
            if any("" in event[0] for event in events):
                found["comments_keepalives"].add(filename)
            if not terminated or fixture["truncated"]:
                found["truncated_stream"].add(filename)
            non_empty_events = [event for event in events if not event[4]]
            if any(event[1] and event[3] is None and not event[2] for event in non_empty_events):
                found["malformed_json_event"].add(filename)
            json_events = [event[3] for event in non_empty_events if not event[2] and event[3] is not None]
            if json_events:
                final = json_events[-1]
                usage = pairs_get(final, "usage")
                choices = pairs_get(final, "choices")
                if usage and usage[0] != "null" and choices == [("array", ())]:
                    found["usage_only_final_sse_event"].add(filename)
        saw_usage = False
        terminal_reasons = {}
        fragments = set()
        for payload in payloads:
            usage = pairs_get(payload, "usage")
            choices = pairs_get(payload, "choices")
            errors = pairs_get(payload, "error")
            if usage:
                saw_usage = True
            if usage and isinstance(usage[0], tuple):
                names = [name for name, _ in usage[0][1]]
                if any(name not in {"prompt_tokens", "completion_tokens", "total_tokens"} for name in names):
                    found["additional_usage_fields"].add(filename)
                if any(name not in names for name in ("prompt_tokens", "completion_tokens", "total_tokens")):
                    found["partial_usage"].add(filename)
            if errors:
                found["upstream_sse_error" if transport == "sse" else "upstream_json_error"].add(filename)
            if choices and isinstance(choices[0], list):
                if len(choices[0]) > 1:
                    found["multiple_choices"].add(filename)
                for choice in choices[0]:
                    indexes = pairs_get(choice, "index")
                    choice_index = repr(indexes[0]) if indexes else "absent"
                    reasons = pairs_get(choice, "finish_reason")
                    terminal_reasons.setdefault(choice_index, set()).update(
                        reason for reason in reasons if reason is not None
                    )
                    for reason in reasons:
                        if reason in KNOWN_FINISH:
                            found[f"known_finish_reason:{reason}"].add(filename)
                        elif reason == "__other__":
                            found["unknown_finish_reason"].add(filename)
                    message = pairs_get(choice, "message")
                    if message and pairs_get(message[0], "tool_calls"):
                        found["buffered_tool_calls"].add(filename)
                    delta = pairs_get(choice, "delta")
                    if delta and pairs_get(delta[0], "tool_calls"):
                        found["streamed_tool_calls"].add(filename)
                        tools = pairs_get(delta[0], "tool_calls")
                        if tools and isinstance(tools[0], list):
                            for tool in tools[0]:
                                tool_indexes = pairs_get(tool, "index")
                                if tool_indexes:
                                    pair = (choice_index, repr(tool_indexes[0]))
                                    if pair in fragments:
                                        found["repeated_tool_call_fragments"].add(filename)
                                    fragments.add(pair)
        if not saw_usage and not has_error:
            found["missing_usage"].add(filename)
        if any(len(reasons) > 1 for reasons in terminal_reasons.values()):
            found["conflicting_terminal_reasons"].add(filename)
    return found


def evidence_rows(fixtures):
    observed = _observed_evidence(fixtures)
    rows = []
    for observation in sorted(EVIDENCE):
        references = sorted(observed[observation])
        if references:
            rows.append({"observation": observation, "status": "captured", "fixtures": references})
        else:
            rows.append({"observation": observation, "status": "unavailable", "reason": "not_observed"})
    return rows


def manifest_for(fixtures, fixture_contents):
    entries = []
    for filename in sorted(fixtures):
        fixture = fixtures[filename]
        entries.append({
            "case": fixture["case"],
            "file": filename,
            "sha256": hashlib.sha256(fixture_contents[filename]).hexdigest(),
            "capture_date": fixture["capture_date"],
            "transport": fixture["transport"],
        })
    return {
        "format": "nim-observation-manifest-v1",
        "sanitizer_version": 1,
        "fixtures": entries,
        "evidence": evidence_rows(fixtures),
    }


def _validate_fixture(filename, content):
    try:
        fixture = json.loads(content.decode("ascii"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SanitizeError("fixture-invalid") from error
    if not isinstance(fixture, dict) or tuple(sorted(fixture)) != tuple(sorted(FIXTURE_FIELDS)):
        raise SanitizeError("fixture-invalid")
    if fixture.get("format") != "nim-observation-v1" or fixture.get("case") + ".json" != filename:
        raise SanitizeError("fixture-invalid")
    if fixture.get("case") not in CASES or fixture.get("transport") not in {"buffered", "sse"}:
        raise SanitizeError("fixture-invalid")
    if not isinstance(fixture.get("capture_date"), str) or not isinstance(fixture.get("content_type"), str):
        raise SanitizeError("fixture-invalid")
    try:
        datetime.date.fromisoformat(fixture["capture_date"])
        fixture["body"].encode("ascii")
    except (AttributeError, UnicodeEncodeError, ValueError) as error:
        raise SanitizeError("fixture-invalid") from error
    if (not isinstance(fixture.get("status"), int) or isinstance(fixture["status"], bool)
            or not 100 <= fixture["status"] <= 599 or not isinstance(fixture.get("truncated"), bool)):
        raise SanitizeError("fixture-invalid")
    if fixture_bytes(fixture) != content:
        raise SanitizeError("fixture-stale")
    return fixture


def validate_candidate(raw, candidate):
    if candidate.get("filename") != raw["case"] + ".json":
        raise SanitizeError("fixture-invalid")
    fixture = candidate.get("fixture")
    if not isinstance(fixture, dict) or tuple(sorted(fixture)) != tuple(sorted(FIXTURE_FIELDS)):
        raise SanitizeError("fixture-invalid")
    if fixture.get("format") != "nim-observation-v1" or not isinstance(fixture.get("body"), str):
        raise SanitizeError("fixture-invalid")
    if any(fixture[field] != raw[field] for field in FIXTURE_FIELDS if field not in {"format", "body"}):
        raise SanitizeError("fixture-invalid")


def _walk_directory(path):
    """Open an absolute directory component by component without following links."""
    path = Path(path)
    if not path.is_absolute():
        path = path.absolute()
    descriptor = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for component in path.parts[1:]:
            if component in {"", ".", ".."}:
                raise SanitizeError("fixture-boundary")
            next_descriptor = os.open(
                component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor
            )
            os.close(descriptor)
            descriptor = next_descriptor
        return descriptor
    except (OSError, SanitizeError) as error:
        os.close(descriptor)
        if isinstance(error, SanitizeError):
            raise
        raise SanitizeError("fixture-boundary") from error


def _read_regular_at(directory_descriptor, name, expected_mode):
    descriptor = -1
    try:
        descriptor = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory_descriptor)
        metadata = os.fstat(descriptor)
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                or stat.S_IMODE(metadata.st_mode) != expected_mode):
            raise SanitizeError("capture-input-boundary")
        parts = []
        while True:
            part = os.read(descriptor, 64 * 1024)
            if not part:
                return b"".join(parts)
            parts.append(part)
    except SanitizeError:
        raise
    except OSError as error:
        raise SanitizeError("capture-input-boundary") from error
    finally:
        if descriptor != -1:
            os.close(descriptor)


def _write_all(descriptor, content):
    view = memoryview(content)
    while view:
        written = os.write(descriptor, view)
        if written <= 0:
            raise OSError("short fixture write")
        view = view[written:]


def _secure_raw_envelopes(input_dir):
    descriptor = _walk_directory(input_dir)
    try:
        metadata = os.fstat(descriptor)
        if (metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o700):
            raise SanitizeError("capture-input-boundary")
        names = set(os.listdir(descriptor))
        expected = {".nim-capture-raw-v1"} | {case + ".raw.json" for case in CASES}
        if names != expected:
            raise SanitizeError("capture-input-boundary")
        if _read_regular_at(descriptor, ".nim-capture-raw-v1", 0o600) != b"nim-capture-raw-v1\n":
            raise SanitizeError("capture-input-boundary")
        envelopes = []
        for case in CASES:
            content = _read_regular_at(descriptor, case + ".raw.json", 0o600)
            try:
                raw, body = parse_raw_capture(content.decode("utf-8"))
            except UnicodeDecodeError as error:
                raise SanitizeError("capture-input-invalid") from error
            if raw["case"] != case:
                raise SanitizeError("capture-input-invalid")
            envelopes.append((raw, body))
        return envelopes
    except OSError as error:
        raise SanitizeError("capture-input-boundary") from error
    finally:
        os.close(descriptor)


def _open_destination(destination, create=False):
    destination = Path(destination)
    parent = _walk_directory(destination.parent)
    descriptor = -1
    try:
        try:
            descriptor = os.open(
                destination.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent
            )
        except FileNotFoundError:
            if not create:
                return parent, None
            os.mkdir(destination.name, 0o700, dir_fd=parent)
            descriptor = os.open(
                destination.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent
            )
        return parent, descriptor
    except OSError as error:
        os.close(parent)
        raise SanitizeError("fixture-boundary") from error


def _is_public_destination_leaf(metadata):
    mode = stat.S_IMODE(metadata.st_mode)
    return bool(
        stat.S_ISREG(metadata.st_mode)
        and metadata.st_uid == os.getuid()
        and mode & stat.S_IRUSR
        and mode & stat.S_IWUSR
        and not mode & (stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH | stat.S_IWOTH)
    )


def _validate_destination_fd(descriptor, allow_stale_evidence=False, allow_checkout_modes=False):
    try:
        names = set(os.listdir(descriptor))
    except OSError as error:
        raise SanitizeError("fixture-set-invalid") from error
    expected_names = {case + ".json" for case in CASES} | {"manifest.json"}
    if names != expected_names:
        raise SanitizeError("fixture-set-invalid")
    contents, fixtures = {}, {}
    for filename in sorted(expected_names):
        if filename == "manifest.json":
            continue
        try:
            file_descriptor = os.open(filename, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=descriptor)
            metadata = os.fstat(file_descriptor)
            valid_leaf = (_is_public_destination_leaf(metadata) if allow_checkout_modes else (
                stat.S_ISREG(metadata.st_mode) and metadata.st_uid == os.getuid()
                and stat.S_IMODE(metadata.st_mode) in {0o600, 0o644}
            ))
            if not valid_leaf:
                raise SanitizeError("fixture-set-invalid")
            content_parts = []
            while True:
                part = os.read(file_descriptor, 64 * 1024)
                if not part:
                    break
                content_parts.append(part)
            content = b"".join(content_parts)
            os.close(file_descriptor)
        except (OSError, SanitizeError) as error:
            raise SanitizeError("fixture-set-invalid") from error
        contents[filename] = content
        fixtures[filename] = _validate_fixture(filename, content)
    try:
        manifest_descriptor = os.open("manifest.json", os.O_RDONLY | os.O_NOFOLLOW, dir_fd=descriptor)
        manifest_metadata = os.fstat(manifest_descriptor)
        valid_manifest = (_is_public_destination_leaf(manifest_metadata) if allow_checkout_modes else (
            stat.S_ISREG(manifest_metadata.st_mode) and manifest_metadata.st_uid == os.getuid()
            and stat.S_IMODE(manifest_metadata.st_mode) in {0o600, 0o644}
        ))
        if not valid_manifest:
            raise SanitizeError("manifest-invalid")
        manifest_content = b"".join(iter(lambda: os.read(manifest_descriptor, 64 * 1024), b""))
        os.close(manifest_descriptor)
        manifest = json.loads(manifest_content.decode("ascii"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, SanitizeError) as error:
        raise SanitizeError("manifest-invalid") from error
    if (not isinstance(manifest, dict) or set(manifest) != {"format", "sanitizer_version", "fixtures", "evidence"}
            or manifest.get("format") != "nim-observation-manifest-v1"
            or not isinstance(manifest.get("sanitizer_version"), int)
            or isinstance(manifest.get("sanitizer_version"), bool)
            or manifest["sanitizer_version"] != 1
            or not isinstance(manifest.get("fixtures"), list) or not isinstance(manifest.get("evidence"), list)):
        raise SanitizeError("manifest-invalid")
    expected_manifest = manifest_for(fixtures, contents)
    comparable_manifest = dict(manifest)
    if allow_stale_evidence:
        comparable_manifest["evidence"] = expected_manifest["evidence"]
    if comparable_manifest != expected_manifest:
        raise SanitizeError("manifest-stale")
    expected_content = (json.dumps(manifest, ensure_ascii=True, sort_keys=True, separators=(",", ":")) + "\n").encode("ascii")
    if manifest_content != expected_content:
        raise SanitizeError("manifest-stale")
    return fixtures, contents


def validate_destination(destination):
    parent, descriptor = _open_destination(destination)
    try:
        if descriptor is None:
            raise SanitizeError("fixture-set-invalid")
        _validate_destination_fd(descriptor, allow_checkout_modes=True)
    finally:
        if descriptor is not None:
            os.close(descriptor)
        os.close(parent)


def refresh_manifest(destination):
    parent, descriptor = _open_destination(destination)
    temporary_identity = None
    try:
        if descriptor is None:
            raise SanitizeError("fixture-set-invalid")
        fixtures, contents = _validate_destination_fd(
            descriptor, allow_stale_evidence=True, allow_checkout_modes=True
        )
        manifest = manifest_for(fixtures, contents)
        manifest_content = (json.dumps(
            manifest, ensure_ascii=True, sort_keys=True, separators=(",", ":")
        ) + "\n").encode("ascii")
        temporary_name = ".manifest.json.tmp"
        try:
            os.stat(temporary_name, dir_fd=descriptor, follow_symlinks=False)
            raise SanitizeError("fixture-write-failed")
        except FileNotFoundError:
            pass
        temporary_identity = _write_private_at(descriptor, temporary_name, manifest_content)
        os.replace(temporary_name, "manifest.json", src_dir_fd=descriptor, dst_dir_fd=descriptor)
        os.fsync(descriptor)
        if not _public_destination_matches(destination, parent, descriptor):
            raise SanitizeError("fixture-boundary")
    except (OSError, SanitizeError) as error:
        raise SanitizeError("fixture-write-failed") from error
    finally:
        if temporary_identity is not None:
            _unlink_private_if_matches_at(descriptor, temporary_name, temporary_identity)
        if descriptor is not None:
            os.close(descriptor)
        os.close(parent)


def build_output(input_dir):
    candidates = []
    for raw, body in _secure_raw_envelopes(input_dir):
        candidate = candidate_from_raw(raw, body)
        validate_candidate(raw, candidate)
        candidates.append(candidate)
    fixtures = {candidate["filename"]: candidate["fixture"] for candidate in candidates}
    if len(fixtures) != len(candidates) or sorted(fixtures) != [case + ".json" for case in CASES]:
        raise SanitizeError("capture-set-invalid")
    contents = {filename: fixture_bytes(fixture) for filename, fixture in fixtures.items()}
    return fixtures, contents, manifest_for(fixtures, contents)


def _create_staging_at(parent_descriptor):
    for attempt in range(128):
        name = f".nim-observations-stage-{os.getpid()}-{attempt}"
        try:
            os.mkdir(name, 0o700, dir_fd=parent_descriptor)
            return name, os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent_descriptor)
        except FileExistsError:
            continue
        except OSError as error:
            raise SanitizeError("fixture-write-failed") from error
    raise SanitizeError("fixture-write-failed")


def _unlink_private_if_matches_at(directory_descriptor, name, identity):
    try:
        metadata = os.stat(name, dir_fd=directory_descriptor, follow_symlinks=False)
    except FileNotFoundError:
        return
    if (metadata.st_dev, metadata.st_ino) == identity:
        os.unlink(name, dir_fd=directory_descriptor)


def _write_private_at(directory_descriptor, name, content):
    descriptor = -1
    identity = None
    try:
        descriptor = os.open(
            name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600,
            dir_fd=directory_descriptor,
        )
        metadata = os.fstat(descriptor)
        identity = (metadata.st_dev, metadata.st_ino)
        _write_all(descriptor, content)
        os.fsync(descriptor)
        return identity
    except OSError as error:
        if identity is not None:
            _unlink_private_if_matches_at(directory_descriptor, name, identity)
        raise SanitizeError("fixture-write-failed") from error
    finally:
        if descriptor != -1:
            os.close(descriptor)


def _public_destination_matches(destination, parent_descriptor, destination_descriptor):
    """Confirm the requested pathname still names the two held directory identities."""
    public_parent = -1
    public_destination = -1
    try:
        public_parent = _walk_directory(Path(destination).parent)
        expected_parent = os.fstat(parent_descriptor)
        observed_parent = os.fstat(public_parent)
        if (observed_parent.st_dev, observed_parent.st_ino) != (
            expected_parent.st_dev,
            expected_parent.st_ino,
        ):
            return False
        public_destination = os.open(
            Path(destination).name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
            dir_fd=public_parent,
        )
        expected_destination = os.fstat(destination_descriptor)
        observed_destination = os.fstat(public_destination)
        return (observed_destination.st_dev, observed_destination.st_ino) == (
            expected_destination.st_dev,
            expected_destination.st_ino,
        )
    except (OSError, SanitizeError):
        return False
    finally:
        if public_destination != -1:
            os.close(public_destination)
        if public_parent != -1:
            os.close(public_parent)


def write_output(input_dir, destination):
    fixtures, contents, manifest = build_output(input_dir)
    parent, existing = _open_destination(destination)
    destination_descriptor = -1
    staging_descriptor = -1
    staging_name = None
    try:
        if existing is not None:
            _validate_destination_fd(
                existing, allow_stale_evidence=True, allow_checkout_modes=True
            )
        staging_name, staging_descriptor = _create_staging_at(parent)
        for filename in sorted(contents):
            _write_private_at(staging_descriptor, filename, contents[filename])
        manifest_content = (json.dumps(manifest, ensure_ascii=True, sort_keys=True, separators=(",", ":")) + "\n").encode("ascii")
        _write_private_at(staging_descriptor, "manifest.json", manifest_content)
        _validate_destination_fd(staging_descriptor)
        if existing is None:
            os.mkdir(Path(destination).name, 0o700, dir_fd=parent)
            destination_descriptor = os.open(
                Path(destination).name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent
            )
        else:
            destination_descriptor = existing
            existing = None
        temporary_names = ["." + filename + ".tmp" for filename in sorted(contents)] + [".manifest.json.tmp"]
        for temporary_name in temporary_names:
            try:
                os.stat(temporary_name, dir_fd=destination_descriptor, follow_symlinks=False)
            except FileNotFoundError:
                continue
            raise SanitizeError("fixture-write-failed")
        for filename in sorted(contents):
            temporary_name = "." + filename + ".tmp"
            _write_private_at(destination_descriptor, temporary_name, contents[filename])
            os.replace(temporary_name, filename, src_dir_fd=destination_descriptor, dst_dir_fd=destination_descriptor)
        _write_private_at(destination_descriptor, ".manifest.json.tmp", manifest_content)
        os.replace(".manifest.json.tmp", "manifest.json", src_dir_fd=destination_descriptor, dst_dir_fd=destination_descriptor)
        os.fsync(destination_descriptor)
        # This is the truthful success linearization point.  A rename after
        # this check is outside this process; a prior swap cannot report
        # success for an unreachable requested destination.
        if not _public_destination_matches(destination, parent, destination_descriptor):
            raise SanitizeError("fixture-boundary")
    except (OSError, SanitizeError) as error:
        raise SanitizeError("fixture-write-failed") from error
    finally:
        if staging_descriptor != -1:
            for filename in [*contents, "manifest.json"]:
                try:
                    os.unlink(filename, dir_fd=staging_descriptor)
                except OSError:
                    pass
            os.close(staging_descriptor)
        if staging_name is not None:
            try:
                os.rmdir(staging_name, dir_fd=parent)
            except OSError:
                pass
        if destination_descriptor != -1:
            os.close(destination_descriptor)
        if existing is not None:
            os.close(existing)
        os.close(parent)


def body_bytes(value):
    try:
        return base64.b64decode(value, validate=True)
    except (TypeError, ValueError):
        return None


def has_sentinel(value, sentinel):
    """Inspect candidate structures recursively, decoding opaque byte channels."""
    if isinstance(value, dict):
        return any(has_sentinel(key, sentinel) or has_sentinel(item, sentinel) for key, item in value.items())
    if isinstance(value, list):
        return any(has_sentinel(item, sentinel) for item in value)
    if isinstance(value, bytes):
        return sentinel.encode() in value
    if isinstance(value, str):
        return sentinel in value or sentinel.encode() in (body_bytes(value) or b"")
    return False


def candidate_body(candidate):
    body = candidate.get("fixture", {}).get("body")
    return body.encode("utf-8") if isinstance(body, str) else None


def json_pairs(body):
    try:
        return json.loads(body, object_pairs_hook=lambda pairs: ("object", pairs))
    except (TypeError, UnicodeDecodeError, json.JSONDecodeError):
        return None


def pairs_get(value, key):
    if not (isinstance(value, tuple) and value[0] == "object"):
        return []
    return [item for name, item in value[1] if name == key]


def json_type(value):
    if value is None:
        return "null"
    if isinstance(value, tuple):
        return "object"
    if isinstance(value, list):
        return "array"
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, str):
        return "string"
    if isinstance(value, (int, float)):
        return "number"
    return "absent"


def number_class(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return json_type(value)
    return ("negative" if value < 0 else "zero" if value == 0 else "positive",
            "integer" if isinstance(value, int) else "fractional")


KNOWN_FINISH = {"content_filter", "function_call", "length", "stop", "tool_calls"}


def protected_json(value, key=None, numbers=None, identifiers=None):
    """Content-free JSON shape, with only observer-relevant equivalence classes."""
    numbers = {} if numbers is None else numbers
    identifiers = {} if identifiers is None else identifiers
    if isinstance(value, tuple) and value[0] == "object":
        return ("object", tuple(
            (name, protected_json(item, name, numbers, identifiers))
            for name, item in value[1]
        ))
    if isinstance(value, list):
        return ("array", tuple(protected_json(item, None, numbers, identifiers) for item in value))
    if value is None or isinstance(value, bool):
        return json_type(value)
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        signature = (number_class(value), repr(value))
        reference = numbers.setdefault(signature, len(numbers))
        return ("number", number_class(value), reference)
    if isinstance(value, str):
        if key == "finish_reason":
            return ("finish", value if value in KNOWN_FINISH else "__other__")
        if key in {"id", "tool_call_id"}:
            reference = identifiers.setdefault(value, len(identifiers))
            return ("identifier", reference)
        return ("redacted",)
    return ("absent",)


def contains_key(value, key):
    if isinstance(value, tuple) and value[0] == "object":
        return any(name == key or contains_key(item, key) for name, item in value[1])
    if isinstance(value, list):
        return any(contains_key(item, key) for item in value)
    return False


def sse_signature(body):
    if body is None:
        return None
    newline = "crlf" if b"\r\n" in body else "lf"
    normalized = body.replace(b"\r\n", b"\n")
    terminated = normalized.endswith(b"\n\n")
    segments = normalized.split(b"\n\n")
    numbers, identifiers = {}, {}
    parsed = []
    for segment in segments:
        lines = segment.split(b"\n") if segment else []
        kinds = [line.partition(b":")[0].decode("ascii", "replace") for line in lines]
        data = [line.partition(b":")[2].lstrip() for line in lines if line.startswith(b"data:")]
        joined = b"\n".join(data)
        parsed_json = json_pairs(joined)
        parsed.append((
            tuple(kinds),
            len(data),
            joined == b"[DONE]",
            protected_json(parsed_json, numbers=numbers, identifiers=identifiers) if parsed_json is not None else None,
            segment == b"",
        ))
    return (newline, terminated, tuple(parsed))


def protected_signature(field, body):
    if field.startswith("sse-") or field == "streamed-tool-calls":
        signature = sse_signature(body)
        if signature is None:
            return None
        newline, terminated, events = signature
        if field == "sse-event-order":
            return tuple(event[:4] for event in events)
        if field == "sse-line-kind":
            return tuple(event[0] for event in events)
        if field == "sse-data-count":
            return tuple(event[1] for event in events)
        if field == "sse-boundary":
            return (tuple(event[4] for event in events), terminated)
        if field == "sse-newline":
            return newline
        if field == "sse-done":
            return tuple(event[2] for event in events)
        if field == "sse-malformed":
            return tuple(event[3] is None for event in events)
        if field == "sse-truncated":
            return terminated
        return tuple(event[3] is not None and contains_key(event[3], "tool_calls") for event in events)

    root = json_pairs(body)
    if root is None:
        return None
    usage = pairs_get(root, "usage")
    choices = pairs_get(root, "choices")
    if field == "usage-presence":
        return bool(usage)
    if field == "usage-type":
        return json_type(usage[0]) if usage else "absent"
    if field == "usage-field-order":
        return tuple(name for name, _ in usage[0][1]) if usage and isinstance(usage[0], tuple) else ()
    if field in {"usage-equality", "usage-conflict"}:
        values = pairs_get(usage[0], "total_tokens") if usage else []
        return (len(values), len({repr(value) for value in values}) == 1)
    if field == "usage-number-class":
        values = pairs_get(usage[0], "completion_tokens") if usage else []
        return number_class(values[0]) if values else "absent"
    if field == "choice-order":
        return tuple(pairs_get(choice, "index")[0] for choice in choices[0] if pairs_get(choice, "index")) if choices else ()
    if field == "finish-reason":
        reasons = pairs_get(choices[0][0], "finish_reason") if choices and choices[0] else []
        return reasons[0] if reasons and reasons[0] in KNOWN_FINISH else "__other__" if reasons else "absent"
    if field == "buffered-tool-calls":
        message = pairs_get(choices[0][0], "message") if choices and choices[0] else []
        tools = pairs_get(message[0], "tool_calls") if message else []
        return json_type(tools[0]) if tools else "absent"
    if field == "error":
        error = pairs_get(root, "error")
        return json_type(error[0]) if error else "absent"
    raise AssertionError(field)


def shape_body(field):
    json_bodies = {
        "usage-presence": b'{"choices":[{"index":0,"finish_reason":"stop"}]}',
        "usage-type": b'{"usage":[],"choices":[]}',
        "usage-field-order": b'{"usage":{"completion_tokens":2,"prompt_tokens":1}}',
        "usage-equality": b'{"usage":{"total_tokens":3,"total_tokens":3}}',
        "usage-conflict": b'{"usage":{"total_tokens":3,"total_tokens":4}}',
        "usage-number-class": b'{"usage":{"completion_tokens":-1.5}}',
        "choice-order": b'{"choices":[{"index":1},{"index":0}]}',
        "finish-reason": b'{"choices":[{"index":0,"finish_reason":"provider-specific"}]}',
        "buffered-tool-calls": b'{"choices":[{"message":{"tool_calls":null}}]}',
        "error": b'{"error":{"code":"synthetic"}}',
    }
    if field in json_bodies:
        return json_bodies[field]
    return {
        "streamed-tool-calls": b'data: {"choices":[{"delta":{"tool_calls":[{}]}}]}\n\n',
        "sse-event-order": b'data: {"choices":[{"index":1}]}\n\ndata: {"choices":[{"index":0}]}\n\n',
        "sse-line-kind": b'retry: 10\ndata: {}\n\n',
        "sse-data-count": b'data: {"usage":\ndata: {"prompt_tokens":1}}\n\n',
        "sse-boundary": b'data: {}\ndata: [DONE]\n',
        "sse-newline": b'data: {}\r\n\r\n',
        "sse-done": b'data: {}\n\n',
        "sse-malformed": b'data: {not-json}\n\n',
        "sse-truncated": b'data: {"usage":{"prompt_tokens":1}}',
    }[field]


def secret_problems(candidate, channel, sentinel):
    fixture = candidate["fixture"]
    body = body_bytes(fixture.get("body_b64"))
    targets = {
        "envelope-authorization": fixture.get("authorization"),
        "envelope-request-headers": fixture.get("request_headers"),
        "envelope-request-body": fixture.get("request_body"),
        "envelope-url": fixture.get("url"),
        "envelope-model": fixture.get("model"),
        "envelope-provider-headers": fixture.get("provider_headers"),
        "envelope-timestamp": fixture.get("captured_at"),
        "nested-json-string": body,
        "sse-comment": body,
        "sse-data": body,
        "sse-id": body,
        "sse-prose": body,
        "opaque-id": body,
        "filename": candidate["filename"],
    }
    if not has_sentinel(targets[channel], sentinel):
        return []
    return [f"capture-secret-leak:{channel}"]


def shape_problems(candidate, field, raw_body):
    expected = protected_signature(field, raw_body)
    actual = protected_signature(field, candidate_body(candidate))
    if actual == expected:
        return []
    return [f"capture-shape-drift:{field}"]


def require_control(actual, expected):
    if actual != [expected]:
        raise AssertionError(f"oracle control {expected}: got {actual}")


def operational_raw_set(directory, secure=True):
    """Create a temporary four-case capture set for operational self-tests."""
    directory.mkdir(mode=0o700)
    if secure:
        os.chmod(directory, 0o700)
        marker = directory / ".nim-capture-raw-v1"
        marker.write_bytes(b"nim-capture-raw-v1\n")
        os.chmod(marker, 0o600)
    for case in CASES:
        streamed = case.startswith("streamed-")
        envelope = raw_envelope(b"data: {}\n\n" if streamed else b'{"choices":[]}')
        envelope.update({
            "case": case,
            "transport": "sse" if streamed else "buffered",
            "content_type": "text/event-stream" if streamed else "application/json",
        })
        path = directory / f"{case}.raw.json"
        write_raw(path, envelope)
        if secure:
            os.chmod(path, 0o600)


def operational_fixture(case, body, status=200):
    return {
        "format": "nim-observation-v1", "case": case,
        "capture_date": "2026-08-01", "transport": "sse" if case.startswith("streamed-") else "buffered",
        "status": status,
        "content_type": "text/event-stream" if case.startswith("streamed-") else "application/json",
        "truncated": False, "body": body,
    }


def evidence_status(rows, observation):
    return next((row["status"] for row in rows if row["observation"] == observation), None)


def operational_cases(sentinel):
    """Exercise publication and evidence behavior through temporary artifacts only."""
    checks = []
    with tempfile.TemporaryDirectory(prefix="nim-sanitize-operational-") as directory:
        root = Path(directory)

        # Each fixed publication temp must reject, rather than follow, a sibling symlink.
        input_dir = root / "raw-temp"
        operational_raw_set(input_dir)
        def temp_symlink_rejected(name):
            destination, target = root / ("destination-" + name), root / ("target-" + name)
            destination.mkdir()
            target.write_bytes(("target-" + name).encode("ascii"))
            (destination / name).symlink_to(target)
            before = target.read_bytes()
            rejected = False
            try:
                write_output(input_dir, destination)
            except SanitizeError:
                rejected = True
            return rejected and target.read_bytes() == before
        if not (temp_symlink_rejected(".buffered-basic.json.tmp")
                and temp_symlink_rejected(".manifest.json.tmp")):
            checks.append("sanitize-temp-nofollow")

        # A separate clean publication proves validation precedes replacement and manifest is last.
        destination = root / "destination-order"
        replaces, events = [], []
        original_replace, original_validate = os.replace, _validate_destination_fd
        def tracked_validate(descriptor):
            events.append("validate")
            return original_validate(descriptor)
        def tracked_replace(source, target, **kwargs):
            replaces.append(Path(source).name)
            events.append(Path(source).name)
            return original_replace(source, target, **kwargs)
        globals()["_validate_destination_fd"] = tracked_validate
        os.replace = tracked_replace
        try:
            write_output(input_dir, destination)
        finally:
            os.replace = original_replace
            globals()["_validate_destination_fd"] = original_validate
        if not events or events[0] != "validate" or not replaces or replaces[-1] != ".manifest.json.tmp":
            checks.append("sanitize-manifest-last")

        # Check rejects semantically stale evidence, while an explicit update
        # may refresh the same exact descriptor-contained five-file set.
        manifest_path = destination / "manifest.json"
        stale_manifest = json.loads(manifest_path.read_text("ascii"))
        stale_manifest["evidence"] = []
        manifest_path.write_text(
            json.dumps(stale_manifest, ensure_ascii=True, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="ascii",
        )
        stale_check_rejected = False
        try:
            validate_destination(destination)
        except SanitizeError:
            stale_check_rejected = True
        refresh_succeeded = True
        try:
            write_output(input_dir, destination)
            validate_destination(destination)
        except SanitizeError:
            refresh_succeeded = False
        if not stale_check_rejected or not refresh_succeeded:
            checks.append("sanitize-stale-destination-refresh")

        # A fixture-only evidence refresh preserves the four validated fixture
        # bytes, makes the stale destination strict-checkable, and rejects any
        # stale manifest content other than the evidence table.
        write_output(input_dir, destination)
        stale_manifest = json.loads(manifest_path.read_text("ascii"))
        stale_manifest["evidence"] = []
        manifest_path.write_text(
            json.dumps(stale_manifest, ensure_ascii=True, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="ascii",
        )
        fixture_hashes_before = {
            filename: hashlib.sha256((destination / filename).read_bytes()).hexdigest()
            for filename in sorted(case + ".json" for case in CASES)
        }
        try:
            refresh_result = main(["--refresh-manifest", str(destination)])
        except SystemExit as error:
            refresh_result = error.code
        fixture_hashes_after = {
            filename: hashlib.sha256((destination / filename).read_bytes()).hexdigest()
            for filename in sorted(case + ".json" for case in CASES)
        }
        if (refresh_result != 0 or main(["--check", str(destination)]) != 0
                or fixture_hashes_after != fixture_hashes_before):
            checks.append("sanitize-manifest-refresh")

        drifted_manifest = json.loads(manifest_path.read_text("ascii"))
        drifted_manifest["fixtures"][0]["case"] = "drifted"
        manifest_path.write_text(
            json.dumps(drifted_manifest, ensure_ascii=True, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="ascii",
        )
        drifted_before = manifest_path.read_bytes()
        try:
            drifted_result = main(["--refresh-manifest", str(destination)])
        except SystemExit as error:
            drifted_result = error.code
        if drifted_result == 0 or manifest_path.read_bytes() != drifted_before:
            checks.append("sanitize-manifest-refresh-non-evidence")

        # Private leaves from a failed write or refresh are removed only while
        # they retain the inode this invocation created.
        cleanup_destination = root / "destination-refresh-cleanup"
        write_output(input_dir, cleanup_destination)
        cleanup_parent, cleanup_descriptor = _open_destination(cleanup_destination)
        partial_name, partial_swap_name = ".partial.tmp", ".partial-swap.tmp"
        original_fsync = os.fsync
        def fail_fsync(_descriptor):
            raise OSError("forced private write failure")
        os.fsync = fail_fsync
        try:
            try:
                _write_private_at(cleanup_descriptor, partial_name, b"partial")
            except SanitizeError:
                partial_rejected = True
            else:
                partial_rejected = False
        finally:
            os.fsync = original_fsync
        def swap_then_fail_fsync(_descriptor):
            os.unlink(partial_swap_name, dir_fd=cleanup_descriptor)
            attacker_descriptor = os.open(
                partial_swap_name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                0o600, dir_fd=cleanup_descriptor,
            )
            try:
                os.write(attacker_descriptor, b"replacement")
            finally:
                os.close(attacker_descriptor)
            raise OSError("forced swapped private write failure")
        os.fsync = swap_then_fail_fsync
        try:
            try:
                _write_private_at(cleanup_descriptor, partial_swap_name, b"owned")
            except SanitizeError:
                partial_swap_rejected = True
            else:
                partial_swap_rejected = False
        finally:
            os.fsync = original_fsync
            os.close(cleanup_descriptor)
            os.close(cleanup_parent)

        partial_owned_remaining = (cleanup_destination / partial_name).exists()
        partial_swap_path = cleanup_destination / partial_swap_name
        partial_swap_survives = partial_swap_path.exists() and partial_swap_path.read_bytes() == b"replacement"
        cleanup_parent, cleanup_descriptor = _open_destination(cleanup_destination)
        try:
            for name in (partial_name, partial_swap_name):
                try:
                    os.unlink(name, dir_fd=cleanup_descriptor)
                except FileNotFoundError:
                    pass
        finally:
            os.close(cleanup_descriptor)
            os.close(cleanup_parent)

        refresh_temp = ".manifest.json.tmp"
        original_replace = os.replace
        def fail_replace(source, target, **kwargs):
            if source == refresh_temp:
                raise OSError("forced refresh replace failure")
            return original_replace(source, target, **kwargs)
        os.replace = fail_replace
        try:
            refresh_failure_result = main(["--refresh-manifest", str(cleanup_destination)])
        finally:
            os.replace = original_replace
        owned_refresh_cleaned = not (cleanup_destination / refresh_temp).exists()
        def swap_then_fail_replace(source, target, **kwargs):
            if source == refresh_temp:
                os.unlink(source, dir_fd=kwargs["src_dir_fd"])
                attacker_descriptor = os.open(
                    source, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                    0o600, dir_fd=kwargs["src_dir_fd"],
                )
                try:
                    os.write(attacker_descriptor, b"replacement")
                finally:
                    os.close(attacker_descriptor)
                raise OSError("forced swapped refresh replace failure")
            return original_replace(source, target, **kwargs)
        os.replace = swap_then_fail_replace
        try:
            refresh_swap_result = main(["--refresh-manifest", str(cleanup_destination)])
        finally:
            os.replace = original_replace
        replacement_path = cleanup_destination / refresh_temp
        replacement_survives = replacement_path.exists() and replacement_path.read_bytes() == b"replacement"
        cleanup_parent, cleanup_descriptor = _open_destination(cleanup_destination)
        try:
            for name in (refresh_temp,):
                try:
                    os.unlink(name, dir_fd=cleanup_descriptor)
                except FileNotFoundError:
                    pass
        finally:
            os.close(cleanup_descriptor)
            os.close(cleanup_parent)
        if (not partial_rejected or partial_owned_remaining
                or not partial_swap_rejected or not partial_swap_survives
                or refresh_failure_result == 0 or not owned_refresh_cleaned
                or refresh_swap_result == 0 or not replacement_survives):
            checks.append("sanitize-private-temp-cleanup")

        # Refresh refuses destination file mode drift and leaves its manifest
        # inode untouched when validation fails.
        mode_destination = root / "destination-refresh-mode"
        write_output(input_dir, mode_destination)
        mode_manifest = mode_destination / "manifest.json"
        fixture_path = mode_destination / "buffered-basic.json"
        os.chmod(fixture_path, 0o666)
        fixture_manifest_before = os.stat(mode_manifest).st_ino
        fixture_mode_result = main(["--refresh-manifest", str(mode_destination)])
        fixture_rejected = fixture_mode_result != 0 and os.stat(mode_manifest).st_ino == fixture_manifest_before
        os.chmod(fixture_path, 0o600)
        os.chmod(mode_manifest, 0o666)
        manifest_before = os.stat(mode_manifest).st_ino
        manifest_mode_result = main(["--refresh-manifest", str(mode_destination)])
        manifest_rejected = manifest_mode_result != 0 and os.stat(mode_manifest).st_ino == manifest_before
        os.chmod(mode_manifest, 0o600)
        if not fixture_rejected or not manifest_rejected:
            checks.append("sanitize-refresh-destination-mode")

        # A public fixture checkout may retain group write from a normal Git
        # umask. It stays safe when every leaf is current-owner, regular,
        # non-executable, and not world-writable; refresh validates that same
        # existing public set before replacing only its private temporary.
        checkout_destination = root / "destination-checkout-mode"
        write_output(input_dir, checkout_destination)
        checkout_leaves = [checkout_destination / (case + ".json") for case in CASES]
        checkout_leaves.append(checkout_destination / "manifest.json")
        for path in checkout_leaves:
            os.chmod(path, 0o664)
        checkout_parent, checkout_descriptor = _open_destination(checkout_destination)
        try:
            try:
                _validate_destination_fd(checkout_descriptor)
            except SanitizeError as error:
                checkout_default_error = str(error)
            else:
                checkout_default_error = None
        finally:
            os.close(checkout_descriptor)
            os.close(checkout_parent)
        try:
            validate_destination(checkout_destination)
        except SanitizeError as error:
            checkout_check_error = str(error)
        else:
            checkout_check_error = None
        checkout_refresh_result = main(["--refresh-manifest", str(checkout_destination)])
        try:
            validate_destination(checkout_destination)
        except SanitizeError as error:
            checkout_refresh_error = str(error)
        else:
            checkout_refresh_error = None

        world_writable_destination = root / "destination-world-writable"
        write_output(input_dir, world_writable_destination)
        world_writable_fixture = world_writable_destination / "buffered-basic.json"
        world_writable_manifest = world_writable_destination / "manifest.json"
        os.chmod(world_writable_fixture, 0o666)
        try:
            validate_destination(world_writable_destination)
        except SanitizeError as error:
            world_writable_fixture_error = str(error)
        else:
            world_writable_fixture_error = None
        os.chmod(world_writable_fixture, 0o600)
        os.chmod(world_writable_manifest, 0o666)
        try:
            validate_destination(world_writable_destination)
        except SanitizeError as error:
            world_writable_manifest_error = str(error)
        else:
            world_writable_manifest_error = None

        executable_destination = root / "destination-executable"
        write_output(input_dir, executable_destination)
        executable_fixture = executable_destination / "buffered-basic.json"
        executable_manifest = executable_destination / "manifest.json"
        os.chmod(executable_fixture, 0o744)
        try:
            validate_destination(executable_destination)
        except SanitizeError as error:
            executable_fixture_error = str(error)
        else:
            executable_fixture_error = None
        os.chmod(executable_fixture, 0o600)
        os.chmod(executable_manifest, 0o744)
        try:
            validate_destination(executable_destination)
        except SanitizeError as error:
            executable_manifest_error = str(error)
        else:
            executable_manifest_error = None
        if (checkout_default_error != "fixture-set-invalid" or checkout_check_error is not None
                or checkout_refresh_result != 0
                or checkout_refresh_error is not None
                or world_writable_fixture_error != "fixture-set-invalid"
                or executable_fixture_error != "fixture-set-invalid"
                or world_writable_manifest_error != "manifest-invalid"
                or executable_manifest_error != "manifest-invalid"):
            checks.append("sanitize-checkout-mode")

        # The destination must be a real directory, both for update and check.
        input_dir = root / "raw-destination"
        operational_raw_set(input_dir)
        target = root / "destination-target"
        target.mkdir()
        destination_alias = root / "destination-alias"
        destination_alias.symlink_to(target, target_is_directory=True)
        alias_updated = False
        try:
            write_output(input_dir, destination_alias)
            alias_updated = True
        except SanitizeError:
            pass
        alias_checked = main(["--check", str(destination_alias)]) == 0
        file_destination = root / "destination-file"
        file_destination.write_bytes(b"not-a-directory")
        file_rejected = False
        try:
            write_output(input_dir, file_destination)
        except SanitizeError:
            file_rejected = True
        if alias_updated or alias_checked or not file_rejected:
            checks.append("sanitize-destination-boundary")

        # Capture input is an exact owner-only directory with the capture marker.
        insecure_input = root / "raw-insecure"
        operational_raw_set(insecure_input, secure=False)
        malformed_marker = root / "raw-malformed-marker"
        operational_raw_set(malformed_marker)
        (malformed_marker / ".nim-capture-raw-v1").write_bytes(b"wrong-marker\n")
        directory_mode = root / "raw-directory-mode"
        operational_raw_set(directory_mode)
        os.chmod(directory_mode, 0o755)
        file_mode = root / "raw-file-mode"
        operational_raw_set(file_mode)
        os.chmod(file_mode / "buffered-basic.raw.json", 0o644)
        input_cases = (insecure_input, malformed_marker, directory_mode, file_mode)
        insecure_accepted = False
        for path in input_cases:
            try:
                build_output(path)
                insecure_accepted = True
            except SanitizeError:
                pass
        input_alias = root / "raw-alias"
        input_alias.symlink_to(insecure_input, target_is_directory=True)
        alias_accepted = True
        try:
            build_output(input_alias)
        except SanitizeError:
            alias_accepted = False
        ancestor_parent = root / "raw-ancestor-parent"
        ancestor_parent.symlink_to(root, target_is_directory=True)
        ancestor_accepted = True
        try:
            build_output(ancestor_parent / "raw-insecure")
        except SanitizeError:
            ancestor_accepted = False
        if insecure_accepted or alias_accepted or ancestor_accepted:
            checks.append("sanitize-input-boundary")

        # A successful update must leave a checkable exact destination set.
        input_dir, destination = root / "raw-extra", root / "destination-extra"
        operational_raw_set(input_dir)
        destination.mkdir()
        (destination / "extra.txt").write_bytes(b"extra")
        update_succeeded = True
        try:
            write_output(input_dir, destination)
        except SanitizeError:
            update_succeeded = False
        check_rejected = main(["--check", str(destination)]) != 0
        if update_succeeded and check_rejected:
            checks.append("sanitize-extra-destination")

        # JSON member names are content too: only schema names may survive.
        raw = root / "raw-key.json"
        write_raw(raw, raw_envelope(("{\"" + sentinel + "\":1}").encode()))
        if has_sentinel(candidate_sanitize(raw), sentinel):
            checks.append("sanitize-key-leak")

        # Declared SSE cannot be silently reclassified merely because bytes parse as JSON.
        raw = root / "raw-transport.json"
        write_raw(raw, raw_envelope(b"{}"))
        transport_candidate = candidate_sanitize(raw)
        if json_pairs(candidate_body(transport_candidate)) is not None:
            checks.append("sanitize-transport")

        streamed = "streamed-basic.json"
        usage_final = operational_fixture(
            "streamed-basic",
            'data: {"choices":[{"index":0}]}\n\ndata: {"usage":{"prompt_tokens":1}}\n\n',
        )
        if evidence_status(evidence_rows({streamed: usage_final}), "missing_usage") != "unavailable":
            checks.append("sanitize-evidence-missing-usage")

        usage_only_final_sse_event = operational_fixture(
            "streamed-basic",
            'data: {"choices":[{"index":0,"finish_reason":"stop"}],"usage":null}\n\ndata: {"choices":[],"usage":{"prompt_tokens":1}}\n\ndata: [DONE]\n\n',
        )
        if evidence_status(
            evidence_rows({streamed: usage_only_final_sse_event}),
            "usage_only_final_sse_event",
        ) != "captured":
            checks.append("sanitize-evidence-usage-only-final-sse")

        distinct = operational_fixture(
            "streamed-basic", 'data: {"choices":[{"index":0,"finish_reason":"stop"}]}\n\ndata: {"choices":[{"index":1,"finish_reason":"length"}]}\n\n'
        )
        repeated = operational_fixture(
            "streamed-basic", 'data: {"choices":[{"index":0,"finish_reason":"stop"}]}\n\ndata: {"choices":[{"index":0,"finish_reason":"length"}]}\n\n'
        )
        null_then_terminal = operational_fixture(
            "streamed-basic", 'data: {"choices":[{"index":0,"finish_reason":null}]}\n\ndata: {"choices":[{"index":0,"finish_reason":"stop"}]}\n\n'
        )
        if (evidence_status(evidence_rows({streamed: distinct}), "conflicting_terminal_reasons") != "unavailable"
                or evidence_status(evidence_rows({streamed: repeated}), "conflicting_terminal_reasons") != "captured"
                or evidence_status(evidence_rows({streamed: null_then_terminal}), "conflicting_terminal_reasons") != "unavailable"):
            checks.append("sanitize-evidence-conflict")

        same_fragment = operational_fixture(
            "streamed-basic", 'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"id-1"}]}}]}\n\ndata: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"id-1"}]}}]}\n\n'
        )
        different_fragment = operational_fixture(
            "streamed-basic", 'data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"id-1"}]}}]}\n\ndata: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"id-1"}]}}]}\n\n'
        )
        if (evidence_status(evidence_rows({streamed: same_fragment}), "repeated_tool_call_fragments") != "captured"
                or evidence_status(evidence_rows({streamed: different_fragment}), "repeated_tool_call_fragments") != "unavailable"):
            checks.append("sanitize-evidence-fragments")

        finish_fixture = operational_fixture(
            "buffered-basic", json.dumps({"choices": [{"finish_reason": reason} for reason in sorted(KNOWN_FINISH)]})
        )
        finish_rows = evidence_rows({"buffered-basic.json": finish_fixture})
        single_finish = operational_fixture(
            "buffered-basic", json.dumps({"choices": [{"finish_reason": "stop"}]})
        )
        single_finish_rows = evidence_rows({"buffered-basic.json": single_finish})
        if (any(evidence_status(finish_rows, f"known_finish_reason:{reason}") != "captured" for reason in KNOWN_FINISH)
                or evidence_status(single_finish_rows, "known_finish_reason:stop") != "captured"
                or any(evidence_status(single_finish_rows, f"known_finish_reason:{reason}") != "unavailable"
                       for reason in KNOWN_FINISH - {"stop"})):
            checks.append("sanitize-finish-coverage")

        fixtures = {case + ".json": operational_fixture(case, '{"choices":[]}', status=True) for case in CASES}
        contents = {filename: fixture_bytes(fixture) for filename, fixture in fixtures.items()}
        schema_destination = root / "destination-schema"
        schema_destination.mkdir()
        for filename, content in contents.items():
            (schema_destination / filename).write_bytes(content)
        schema_manifest = manifest_for(fixtures, contents)
        (schema_destination / "manifest.json").write_bytes(
            (json.dumps(schema_manifest, ensure_ascii=True, sort_keys=True, separators=(",", ":")) + "\n").encode("ascii")
        )
        schema_accepted = main(["--check", str(schema_destination)]) == 0
        if schema_accepted:
            checks.append("sanitize-fixture-schema")

        # Declared SSE remains SSE even if its whole byte body happens to be JSON.
        raw = root / "raw-declared-sse.json"
        write_raw(raw, raw_envelope(b'{"choices":[]}'))
        if json_pairs(candidate_body(candidate_sanitize(raw))) is not None:
            checks.append("sanitize-sse-declared-transport")

        # Equality classes span every SSE event, not each event in isolation.
        raw = root / "raw-sse-equality.json"
        write_raw(raw, raw_envelope(
            b'data: {"choices":[{"delta":{"tool_calls":[{"id":"first"}]}}]}\n\n'
            b'data: {"choices":[{"delta":{"tool_calls":[{"id":"second"}]}}]}\n\n'
        ))
        raw_body = base64.b64decode(json.loads(raw.read_text("utf-8"))["body_b64"], validate=True)
        if protected_signature("sse-event-order", candidate_body(candidate_sanitize(raw))) != protected_signature("sse-event-order", raw_body):
            checks.append("sanitize-sse-equality")

        # Error-bearing SSE replies neither establish successful streaming nor missing usage.
        error_fixture = operational_fixture("streamed-basic", 'data: {"error":{}}\n\n')
        error_rows = evidence_rows({"streamed-basic.json": error_fixture})
        if (evidence_status(error_rows, "standard_streamed_success") != "unavailable"
                or evidence_status(error_rows, "missing_usage") != "unavailable"):
            checks.append("sanitize-error-evidence")

        # A pathname create demonstrably follows the swapped public parent;
        # descriptor-relative staging must instead stay in the opened directory.
        parent, attacker, parked = root / "staging-parent", root / "staging-attacker", root / "staging-parked"
        parent.mkdir(); attacker.mkdir()
        os.rename(parent, parked)
        os.symlink(attacker, parent, target_is_directory=True)
        os.mkdir(parent / "pathname-stage")
        pathname_followed = (attacker / "pathname-stage").is_dir()
        os.rmdir(attacker / "pathname-stage")
        parent.unlink()
        os.rename(parked, parent)
        parent_descriptor = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        staging_name, staging_descriptor = None, -1
        try:
            os.rename(parent, parked)
            os.symlink(attacker, parent, target_is_directory=True)
            staging_name, staging_descriptor = _create_staging_at(parent_descriptor)
            descriptor_stayed = staging_name in os.listdir(parked) and staging_name not in os.listdir(attacker)
        finally:
            if staging_descriptor != -1:
                os.close(staging_descriptor)
            if staging_name is not None:
                os.rmdir(staging_name, dir_fd=parent_descriptor)
            os.close(parent_descriptor)
        if not pathname_followed or not descriptor_stayed:
            checks.append("sanitize-staging-parent")

        # Descriptor-relative publication must still refuse success when the
        # requested public parent disappears during a complete write.
        input_dir = root / "raw-publication-parent"
        operational_raw_set(input_dir)
        parent = root / "publication-parent"
        attacker = root / "publication-attacker"
        parked = root / "publication-parked"
        destination = parent / "observations"
        parent.mkdir()
        attacker.mkdir()
        original_create_staging = _create_staging_at

        def swap_parent_after_staging(parent_descriptor):
            name, descriptor = original_create_staging(parent_descriptor)
            os.rename(parent, parked)
            os.symlink(attacker, parent, target_is_directory=True)
            return name, descriptor

        returned_success = False
        try:
            globals()["_create_staging_at"] = swap_parent_after_staging
            write_output(input_dir, destination)
            returned_success = True
        except SanitizeError:
            pass
        finally:
            globals()["_create_staging_at"] = original_create_staging
        requested_destination_missing = not (parent / destination.name).exists()
        attacker_untouched = not (attacker / destination.name).exists()
        if returned_success or not requested_destination_missing or not attacker_untouched:
            checks.append("sanitize-publication-parent")
    return checks


def run_cases(sentinel):
    candidate_checks = []
    controls = 0
    private_event = b'data: {"choices":[{"index":0,"delta":{"content":"private-text"}}]}\n\n'
    redacted_event = b'data: {"choices":[{"index":0,"delta":{"content":"<redacted>"}}]}\n\n'
    if protected_signature("sse-event-order", private_event) != protected_signature("sse-event-order", redacted_event):
        raise AssertionError("capture-shape-drift:sse-redaction-control")
    one_boundary = b"data: {}\n\n"
    added_boundary = b"data: {}\n\n\n\n"
    if protected_signature("sse-boundary", one_boundary) == protected_signature("sse-boundary", added_boundary):
        raise AssertionError("capture-shape-drift:sse-boundary-control")
    buffered = json.dumps({"id": f"opaque-{sentinel}", "choices": [{"message": {"content": f"nested {sentinel}"}}]}).encode()
    forbidden = {
        "authorization": f"Bearer {sentinel}", "request_headers": {"x-secret": sentinel},
        "request_body": {"model": sentinel}, "url": f"https://{sentinel}.invalid/",
        "model": sentinel, "provider_headers": {"x-provider-secret": sentinel}, "captured_at": sentinel,
    }
    secret_cases = (
        ("envelope-authorization", "authorization"), ("envelope-request-headers", "request_headers"),
        ("envelope-request-body", "request_body"), ("envelope-url", "url"),
        ("envelope-model", "model"), ("envelope-provider-headers", "provider_headers"),
        ("envelope-timestamp", "captured_at"), ("nested-json-string", None),
        ("sse-comment", None), ("sse-data", None), ("sse-id", None),
        ("sse-prose", None), ("opaque-id", None), ("filename", None),
    )
    shape_fields = (
        "usage-presence", "usage-type", "usage-field-order", "usage-equality", "usage-conflict",
        "usage-number-class", "choice-order", "finish-reason", "buffered-tool-calls",
        "streamed-tool-calls", "error", "sse-event-order", "sse-line-kind", "sse-data-count",
        "sse-boundary", "sse-newline", "sse-done", "sse-malformed", "sse-truncated",
    )
    with tempfile.TemporaryDirectory(prefix="nim-sanitize-red-") as directory:
        root, raw = Path(directory), Path(directory) / "raw.json"
        original = raw_envelope(b"{}")
        for channel, envelope_field in secret_cases:
            write_raw(raw, original)  # restore before every negative fixture
            name = raw
            if envelope_field:
                envelope = raw_envelope(b"{}"); envelope[envelope_field] = forbidden[envelope_field]
            elif channel == "nested-json-string":
                envelope = raw_envelope(buffered)
            elif channel == "sse-comment":
                envelope = raw_envelope(f": {sentinel}\ndata: {{}}\n\n".encode())
            elif channel == "sse-data":
                envelope = raw_envelope(f"data: {{\"content\":\"{sentinel}\"}}\n\n".encode())
            elif channel == "sse-id":
                envelope = raw_envelope(f"id: {sentinel}\ndata: {{}}\n\n".encode())
            elif channel == "sse-prose":
                envelope = raw_envelope(f"data: prose {sentinel}\n\n".encode())
            elif channel == "opaque-id":
                envelope = raw_envelope(f"{{\"id\":\"opaque-{sentinel}\"}}".encode())
            else:
                name = root / f"fixture-{sentinel}.json"; envelope = original
            write_raw(name, envelope)
            expected = f"capture-secret-leak:{channel}"
            control = {"filename": name.name, "fixture": envelope}
            require_control(secret_problems(control, channel, sentinel), expected)
            controls += 1
            candidate_checks.extend(secret_problems(candidate_sanitize(name), channel, sentinel))
            if name != raw:
                name.unlink()
        for field in shape_fields:
            write_raw(raw, original)  # restore before every negative fixture
            body = shape_body(field)
            envelope = raw_envelope(body)
            if field not in {"streamed-tool-calls", "sse-event-order", "sse-line-kind", "sse-data-count", "sse-boundary", "sse-newline", "sse-done", "sse-malformed", "sse-truncated"}:
                envelope["transport"] = "buffered"
                envelope["content_type"] = "application/json"
            write_raw(raw, envelope)
            expected = f"capture-shape-drift:{field}"
            control = {"filename": "broken.json", "fixture": envelope}
            require_control(shape_problems(control, field, body), expected)
            controls += 1
            candidate_checks.extend(shape_problems(candidate_sanitize(raw), field, body))
    candidate_checks.extend(operational_cases(sentinel))
    return candidate_checks, controls


def selftest():
    sentinel = "SENTINEL-RAW-SECRET-9f2c"
    stdout, stderr = io.StringIO(), io.StringIO()
    exception_text = ""
    try:
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            checks, controls = run_cases(sentinel)
    except Exception as error:  # an implementation exception is itself inspected for leaks
        checks = []
        controls = 0
        exception_text = str(error)
    captured = stdout.getvalue() + stderr.getvalue() + exception_text
    if sentinel in captured:
        raise AssertionError("capture-secret-leak:diagnostics")
    expected_count = 14 + 19
    if controls != expected_count:
        raise AssertionError("capture-shape-drift:selftest-control-count")
    for check in checks:
        print(f"RED {check}")
    if checks:
        print(f"selftest RED — {len(checks)} candidate violations; {controls} oracle controls observed")
        return 1
    print(f"selftest ok — {controls} oracle controls observed; candidate compliant")
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--selftest", action="store_true", help="exercise sanitizer contract regressions"
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--input-dir", type=Path, help="directory holding raw NIM envelopes")
    mode.add_argument("--check", type=Path, help="validate an existing fixture directory")
    mode.add_argument("--refresh-manifest", type=Path, help="refresh evidence for validated fixtures")
    parser.add_argument("--output-dir", type=Path, help="destination for sanitized fixtures")
    args = parser.parse_args(argv)
    if args.selftest:
        if args.input_dir or args.check or args.refresh_manifest or args.output_dir:
            parser.error("--selftest cannot be combined with fixture modes")
        return selftest()
    if args.check:
        if args.output_dir:
            parser.error("--check cannot be combined with --output-dir")
        try:
            validate_destination(args.check)
        except SanitizeError as error:
            print(f"sanitize check failed: {error}")
            return 1
        return 0
    if args.refresh_manifest:
        if args.output_dir:
            parser.error("--refresh-manifest cannot be combined with --output-dir")
        try:
            refresh_manifest(args.refresh_manifest)
        except SanitizeError as error:
            print(f"sanitize refresh failed: {error}")
            return 1
        return 0
    if not args.input_dir or not args.output_dir:
        parser.error("--input-dir and --output-dir are required together")
    try:
        write_output(args.input_dir, args.output_dir)
    except SanitizeError as error:
        print(f"sanitize failed: {error}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

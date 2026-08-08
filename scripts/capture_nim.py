#!/usr/bin/env python3
"""Capture one bounded private NIM response per fixed case as raw evidence."""

import argparse
import base64
import contextlib
import copy
import datetime
import http.client
import io
import json
import os
import pathlib
import signal
import socket
import ssl
import stat
import sys
import tempfile
import threading
import time
import urllib.parse
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, HTTPServer


CASES = (
    "buffered-basic",
    "streamed-basic",
    "buffered-tools",
    "streamed-tools",
)
REQUIRED_ENVIRONMENT = (
    "NIM_CAPTURE_BASE_URL",
    "NIM_CAPTURE_BEARER_TOKEN",
    "NIM_CAPTURE_MODEL",
)
MAX_RESPONSE_BYTES = 2 * 1024 * 1024
MAX_SSE_EVENTS = 2_048
MAX_CAPTURE_SECONDS = 60
READ_CHUNK_BYTES = 64 * 1024
OWNER_DIRECTORY_MODE = 0o700
OWNER_FILE_MODE = 0o600
EXCLUSIVE_NOFOLLOW_FLAGS = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
FIXED_MAX_TOKENS = 32
FIXED_PROMPT = "Return one capture marker."
FIXED_TOOL = {
    "type": "function",
    "function": {
        "name": "record_capture_marker",
        "description": "Record the fixed capture marker.",
        "parameters": {
            "type": "object",
            "properties": {},
            "additionalProperties": False,
        },
    },
}


@dataclass(frozen=True, order=True)
class Problem:
    check: str


@dataclass(frozen=True)
class CandidateDecision:
    """One boundary decision exposed to the independent self-test oracles."""

    rejected: bool = False
    truncated: bool = False
    file_flags: int = 0


@dataclass(frozen=True)
class CaptureRequestPlan:
    """The only request body and its separate non-JSON execution policy."""

    body: dict
    attempts: int = 0
    retries: int = 0


@dataclass
class RawDirectory:
    """Descriptor-owned raw directory identity for safe failure cleanup."""

    directory_descriptor: int
    parent_descriptor: int
    leaf: str
    device: int
    inode: int


class CaptureError(Exception):
    """A stable, deliberately content-free capture failure."""

    def __init__(self, check: str):
        self.check = check
        super().__init__(check)


def candidate_environment(environment: dict[str, str]) -> CandidateDecision:
    """GREEN will inspect only the three approved environment names."""
    return CandidateDecision(
        rejected=any(
            not isinstance(environment.get(name), str) or not environment[name]
            for name in REQUIRED_ENVIRONMENT
        )
    )


def candidate_url(base_url: str) -> CandidateDecision:
    """GREEN will reject unsafe service roots before any transport opens."""
    try:
        parsed = urllib.parse.urlsplit(base_url)
        loopback = {"127.0.0.1", "::1"}
        transport_allowed = parsed.scheme == "https" or (
            parsed.scheme == "http" and parsed.hostname in loopback
        )
        rejected = not (
            transport_allowed
            and parsed.hostname
            and parsed.path.rstrip("/") == "/v1"
            and "@" not in parsed.netloc
            and "?" not in base_url
            and "#" not in base_url
        )
    except ValueError:
        rejected = True
    return CandidateDecision(rejected=rejected)


def candidate_path(
    output_dir: pathlib.Path, repository: pathlib.Path
) -> CandidateDecision:
    """GREEN will create only a new, resolved-outside-repository directory."""
    try:
        resolved_repository = repository.resolve()
        resolved_output = output_dir.resolve(strict=False)
        lexical_repository = repository.absolute()
        lexical_output = output_dir.absolute()
        has_symlink_component = output_dir.is_symlink() or any(
            parent.is_symlink() for parent in output_dir.parents
        )
        safe = (
            output_dir.is_absolute()
            and not output_dir.exists()
            and not has_symlink_component
            and not lexical_output.is_relative_to(lexical_repository)
            and not resolved_output.is_relative_to(resolved_repository)
        )
    except OSError:
        safe = False
    return CandidateDecision(
        rejected=not safe,
        file_flags=EXCLUSIVE_NOFOLLOW_FLAGS,
    )


def candidate_owner_mode(path: pathlib.Path, expected_mode: int) -> CandidateDecision:
    """GREEN will reject pre-existing owner-mode drift before writing."""
    try:
        rejected = stat.S_IMODE(path.stat().st_mode) != expected_mode
    except OSError:
        rejected = True
    return CandidateDecision(rejected=rejected)


def candidate_limit(
    response_bytes: int, event_count: int, elapsed_seconds: int
) -> CandidateDecision:
    """GREEN will stop and mark bounded truncation at any locked limit."""
    return CandidateDecision(
        truncated=(
            response_bytes > MAX_RESPONSE_BYTES
            or event_count > MAX_SSE_EVENTS
            or elapsed_seconds > MAX_CAPTURE_SECONDS
        )
    )


def candidate_diagnostic(channel: str, secret: str) -> str:
    """GREEN diagnostics name only a fixed channel, never the supplied data."""
    del secret
    return f"capture failure ({channel})"


def candidate_request_profile(case: str, model: str) -> CaptureRequestPlan:
    """GREEN will build exactly one fixed request profile for each case."""
    streamed = case.startswith("streamed-")
    tools = case.endswith("-tools")
    body = {
        "max_tokens": FIXED_MAX_TOKENS,
        "messages": [{"content": FIXED_PROMPT, "role": "user"}],
        "model": model,
        "stream": streamed,
    }
    if streamed:
        body["stream_options"] = {"include_usage": True}
    if tools:
        body["tools"] = [copy.deepcopy(FIXED_TOOL)]
        body["tool_choice"] = "required"
    return CaptureRequestPlan(body=body, attempts=1, retries=0)


def capture_environment() -> tuple[str, str, str]:
    """Read the three approved values and reject empty configuration."""
    environment = {name: os.environ.get(name, "") for name in REQUIRED_ENVIRONMENT}
    if candidate_environment(environment).rejected:
        raise CaptureError("capture-environment")
    return (
        environment["NIM_CAPTURE_BASE_URL"],
        environment["NIM_CAPTURE_BEARER_TOKEN"],
        environment["NIM_CAPTURE_MODEL"],
    )


def capture_endpoint(base_url: str) -> tuple[str, str, int | None, str]:
    """Validate a service root and append the one permitted API path."""
    if candidate_url(base_url).rejected:
        raise CaptureError("capture-url-boundary")
    parsed = urllib.parse.urlsplit(base_url)
    if parsed.hostname is None:
        raise CaptureError("capture-url-boundary")
    service_path = parsed.path.rstrip("/")
    try:
        port = parsed.port
    except ValueError as error:
        raise CaptureError("capture-url-boundary") from error
    return parsed.scheme, parsed.hostname, port, f"{service_path}/chat/completions"


def write_all(file_descriptor: int, content: bytes) -> None:
    """Write all bounded bytes without introducing a buffered file object."""
    view = memoryview(content)
    while view:
        written = os.write(file_descriptor, view)
        if written <= 0:
            raise OSError("short private artifact write")
        view = view[written:]


def write_private_at(directory_descriptor: int, name: str, content: bytes) -> None:
    """Create one exact private regular file beneath an already-safe directory."""
    descriptor = -1
    try:
        descriptor = os.open(
            name,
            EXCLUSIVE_NOFOLLOW_FLAGS,
            OWNER_FILE_MODE,
            dir_fd=directory_descriptor,
        )
        os.fchmod(descriptor, OWNER_FILE_MODE)
        write_all(descriptor, content)
        os.fsync(descriptor)
        file_stat = os.fstat(descriptor)
        if (
            not stat.S_ISREG(file_stat.st_mode)
            or file_stat.st_uid != os.getuid()
            or stat.S_IMODE(file_stat.st_mode) != OWNER_FILE_MODE
        ):
            raise CaptureError("capture-owner-mode")
        os.fsync(directory_descriptor)
    except CaptureError:
        raise
    except OSError as error:
        raise CaptureError("capture-private-artifact") from error
    finally:
        if descriptor != -1:
            os.close(descriptor)


def secure_parent_descriptor(output_dir: pathlib.Path) -> tuple[int, str]:
    """Walk an absolute output parent through no-follow directory descriptors."""
    if not output_dir.is_absolute() or output_dir.name in {"", ".", ".."}:
        raise CaptureError("capture-path-boundary")
    components = output_dir.parent.parts
    if not components or components[0] != os.sep:
        raise CaptureError("capture-path-boundary")
    descriptor = -1
    try:
        descriptor = os.open(os.sep, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        for component in components[1:]:
            if component in {"", ".", ".."}:
                raise CaptureError("capture-path-boundary")
            child = os.open(
                component,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=descriptor,
            )
            os.close(descriptor)
            descriptor = child
        return descriptor, output_dir.name
    except CaptureError:
        if descriptor != -1:
            os.close(descriptor)
        raise
    except OSError as error:
        if descriptor != -1:
            os.close(descriptor)
        raise CaptureError("capture-path-boundary") from error


def public_output_matches(
    output_dir: pathlib.Path,
    repository: pathlib.Path,
    directory_stat: os.stat_result,
) -> bool:
    """Reject a public path that moved after its descriptor-safe creation."""
    try:
        resolved_repository = repository.resolve()
        resolved_output = output_dir.resolve(strict=True)
        lexical_repository = repository.absolute()
        lexical_output = output_dir.absolute()
        public_stat = output_dir.stat()
    except OSError:
        return False
    return (
        not lexical_output.is_relative_to(lexical_repository)
        and not resolved_output.is_relative_to(resolved_repository)
        and public_stat.st_dev == directory_stat.st_dev
        and public_stat.st_ino == directory_stat.st_ino
    )


def close_raw_directory(directory: RawDirectory) -> None:
    """Close the descriptors held for one successfully created raw directory."""
    os.close(directory.directory_descriptor)
    os.close(directory.parent_descriptor)


def cleanup_raw_directory(directory: RawDirectory, names: tuple[str, ...]) -> None:
    """Remove only known artifacts in the exact directory created by this process."""
    try:
        if directory.directory_descriptor != -1:
            for name in names:
                try:
                    os.unlink(name, dir_fd=directory.directory_descriptor)
                except OSError:
                    pass
        try:
            current = os.stat(
                directory.leaf,
                dir_fd=directory.parent_descriptor,
                follow_symlinks=False,
            )
            if (
                stat.S_ISDIR(current.st_mode)
                and current.st_dev == directory.device
                and current.st_ino == directory.inode
            ):
                os.rmdir(directory.leaf, dir_fd=directory.parent_descriptor)
        except OSError:
            pass
    finally:
        if directory.directory_descriptor != -1:
            os.close(directory.directory_descriptor)
        os.close(directory.parent_descriptor)


def cleanup_capture_directory(output_dir: pathlib.Path, repository: pathlib.Path) -> None:
    """Clear one exact reviewed raw set through retained no-follow descriptors."""
    parent_descriptor = -1
    directory_descriptor = -1
    directory: RawDirectory | None = None
    marker = ".nim-capture-raw-v1"
    raw_names = tuple(f"{case}.raw.json" for case in CASES)
    expected_names = {marker, *raw_names}
    try:
        parent_descriptor, leaf = secure_parent_descriptor(output_dir)
        directory_descriptor = os.open(
            leaf,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
            dir_fd=parent_descriptor,
        )
        directory_stat = os.fstat(directory_descriptor)
        directory = RawDirectory(
            directory_descriptor,
            parent_descriptor,
            leaf,
            directory_stat.st_dev,
            directory_stat.st_ino,
        )
        directory_descriptor = -1
        parent_descriptor = -1
        if (
            not stat.S_ISDIR(directory_stat.st_mode)
            or directory_stat.st_uid != os.getuid()
            or stat.S_IMODE(directory_stat.st_mode) != OWNER_DIRECTORY_MODE
        ):
            raise CaptureError("capture-cleanup")
        if not public_output_matches(output_dir, repository, directory_stat):
            raise CaptureError("capture-path-boundary")
        if set(os.listdir(directory.directory_descriptor)) != expected_names:
            raise CaptureError("capture-cleanup")
        for name in (marker, *raw_names):
            descriptor = -1
            try:
                descriptor = os.open(
                    name,
                    os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW,
                    dir_fd=directory.directory_descriptor,
                )
                file_stat = os.fstat(descriptor)
                if (
                    not stat.S_ISREG(file_stat.st_mode)
                    or file_stat.st_uid != os.getuid()
                    or stat.S_IMODE(file_stat.st_mode) != OWNER_FILE_MODE
                ):
                    raise CaptureError("capture-cleanup")
                if name == marker and os.read(descriptor, 64) != b"nim-capture-raw-v1\n":
                    raise CaptureError("capture-cleanup")
            finally:
                if descriptor != -1:
                    os.close(descriptor)
        for name in raw_names:
            os.unlink(name, dir_fd=directory.directory_descriptor)
        os.unlink(marker, dir_fd=directory.directory_descriptor)
        os.fsync(directory.directory_descriptor)
        if os.listdir(directory.directory_descriptor):
            raise CaptureError("capture-cleanup")
        if not public_output_matches(output_dir, repository, directory_stat):
            raise CaptureError("capture-path-boundary")
    except CaptureError:
        raise
    except OSError as error:
        raise CaptureError("capture-cleanup") from error
    finally:
        if directory is not None:
            close_raw_directory(directory)
        else:
            if directory_descriptor != -1:
                os.close(directory_descriptor)
            if parent_descriptor != -1:
                os.close(parent_descriptor)


def create_raw_directory(
    output_dir: pathlib.Path, repository: pathlib.Path
) -> RawDirectory:
    """Create and mark a new raw-artifact directory before transport begins."""
    if candidate_path(output_dir, repository).rejected:
        raise CaptureError("capture-path-boundary")
    parent_descriptor = -1
    descriptor = -1
    created: RawDirectory | None = None
    try:
        parent_descriptor, leaf = secure_parent_descriptor(output_dir)
        os.mkdir(leaf, OWNER_DIRECTORY_MODE, dir_fd=parent_descriptor)
        created_stat = os.stat(leaf, dir_fd=parent_descriptor, follow_symlinks=False)
        created = RawDirectory(
            -1,
            parent_descriptor,
            leaf,
            created_stat.st_dev,
            created_stat.st_ino,
        )
        parent_descriptor = -1
        descriptor = os.open(
            leaf,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
            dir_fd=created.parent_descriptor,
        )
        created.directory_descriptor = descriptor
        descriptor = -1
        os.fchmod(created.directory_descriptor, OWNER_DIRECTORY_MODE)
        directory_stat = os.fstat(created.directory_descriptor)
        if (
            not stat.S_ISDIR(directory_stat.st_mode)
            or directory_stat.st_uid != os.getuid()
            or stat.S_IMODE(directory_stat.st_mode) != OWNER_DIRECTORY_MODE
        ):
            raise CaptureError("capture-owner-mode")
        if not public_output_matches(output_dir, repository, directory_stat):
            raise CaptureError("capture-path-boundary")
        write_private_at(
            created.directory_descriptor, ".nim-capture-raw-v1", b"nim-capture-raw-v1\n"
        )
        result = created
        created = None
        return result
    except CaptureError:
        raise
    except OSError as error:
        raise CaptureError("capture-path-boundary") from error
    finally:
        if descriptor != -1:
            os.close(descriptor)
        if created is not None:
            cleanup_raw_directory(created, (".nim-capture-raw-v1",))
        if parent_descriptor != -1:
            os.close(parent_descriptor)


def remaining_seconds(deadline: float) -> float:
    """Return the positive amount left in the one total monotonic deadline."""
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise CaptureError("capture-limit")
    return remaining


def truncate_stream(
    retained: bytearray, streamed: bool, complete_boundary: int, truncated: bool
) -> tuple[bytes, bool]:
    """Drop a partial next SSE event whenever a bounded read stops."""
    if streamed and len(retained) > complete_boundary:
        del retained[complete_boundary:]
        truncated = True
    return bytes(retained), truncated


def read_bounded_response(
    response: http.client.HTTPResponse,
    streamed: bool,
    deadline: float,
) -> tuple[bytes, bool]:
    """Retain at most the raw-byte and, for SSE, event limits across chunks."""
    retained = bytearray()
    current_line = bytearray()
    events = 0
    complete_boundary = 0
    while True:
        room = MAX_RESPONSE_BYTES - len(retained)
        if room == 0:
            return truncate_stream(retained, streamed, complete_boundary, True)
        try:
            remaining_seconds(deadline)
        except CaptureError:
            return truncate_stream(retained, streamed, complete_boundary, True)
        try:
            chunk = response.read(min(READ_CHUNK_BYTES, room))
        except CaptureError:
            return truncate_stream(retained, streamed, complete_boundary, True)
        except (OSError, TimeoutError) as error:
            if time.monotonic() >= deadline:
                return truncate_stream(retained, streamed, complete_boundary, True)
            raise CaptureError("capture-transport") from error
        if not chunk:
            return truncate_stream(retained, streamed, complete_boundary, False)
        for value in chunk:
            retained.append(value)
            if not streamed:
                continue
            current_line.append(value)
            if value != ord("\n"):
                continue
            if current_line in (b"\n", b"\r\n"):
                events += 1
                complete_boundary = len(retained)
                if events == MAX_SSE_EVENTS:
                    return bytes(retained), True
            current_line.clear()


def open_connection(
    scheme: str, host: str, port: int | None, timeout: float
) -> http.client.HTTPConnection:
    """Use verified TLS for HTTPS and permit HTTP only after URL validation."""
    if scheme == "https":
        return http.client.HTTPSConnection(
            host,
            port=port,
            timeout=timeout,
            context=ssl.create_default_context(),
        )
    return http.client.HTTPConnection(host, port=port, timeout=timeout)


class CaptureDeadline:
    """Linux process-wide alarm scoped to one capture's absolute deadline."""

    def __init__(self, seconds: int) -> None:
        self.seconds = seconds
        self.previous_handler: object | None = None
        self.previous_timer: tuple[float, float] = (0.0, 0.0)
        self.started = 0.0

    def __enter__(self) -> "CaptureDeadline":
        self.started = time.monotonic()
        self.previous_handler = signal.getsignal(signal.SIGALRM)
        signal.signal(signal.SIGALRM, self._expired)
        self.previous_timer = signal.setitimer(signal.ITIMER_REAL, self.seconds)
        return self

    def __exit__(self, exception_type, exception, traceback) -> bool:
        signal.setitimer(signal.ITIMER_REAL, 0.0)
        signal.signal(signal.SIGALRM, self.previous_handler)
        previous_delay, previous_interval = self.previous_timer
        if previous_delay:
            elapsed = max(0.0, time.monotonic() - self.started)
            signal.setitimer(
                signal.ITIMER_REAL,
                max(0.0, previous_delay - elapsed),
                previous_interval,
            )
        return False

    @staticmethod
    def _expired(signum: int, frame: object) -> None:
        del signum, frame
        raise CaptureError("capture-limit")


def close_quietly(value: object | None) -> None:
    """Close a stdlib transport without allowing cleanup errors to leak details."""
    close = getattr(value, "close", None)
    if callable(close):
        try:
            close()
        except OSError:
            pass


def retained_transport(transport: object) -> object:
    """Keep the original response socket; stdlib TLS sockets cannot be duplicated."""
    return transport


def raw_envelope(
    case: str,
    streamed: bool,
    response: http.client.HTTPResponse,
    body: bytes,
    truncated: bool,
) -> bytes:
    """Produce the deliberately minimal raw evidence record, without headers."""
    envelope = {
        "format": "nim-capture-raw-v1",
        "case": case,
        "capture_date": datetime.datetime.now(datetime.UTC).date().isoformat(),
        "transport": "sse" if streamed else "buffered",
        "status": int(response.status),
        "content_type": response.getheader("Content-Type") or "",
        "truncated": bool(truncated),
        "body_b64": base64.b64encode(body).decode("ascii"),
    }
    return json.dumps(envelope, separators=(",", ":"), ensure_ascii=True).encode("utf-8") + b"\n"


def candidate_capture(cases: tuple[str, ...] | list[str], output_dir: pathlib.Path) -> None:
    """Capture one canonical case set into one all-or-nothing raw directory."""
    if (
        not isinstance(cases, (tuple, list))
        or not cases
        or any(case not in CASES for case in cases)
        or (len(cases) > 1 and tuple(cases) != CASES)
    ):
        raise CaptureError("capture-case-list")
    raw_directory: RawDirectory | None = None
    completed = False
    try:
        base_url, bearer_token, model = capture_environment()
        scheme, host, port, endpoint = capture_endpoint(base_url)
        repository = pathlib.Path(__file__).resolve().parent.parent
        raw_directory = create_raw_directory(output_dir, repository)
        for case in cases:
            deadline = time.monotonic() + MAX_CAPTURE_SECONDS
            connection: http.client.HTTPConnection | None = None
            response: http.client.HTTPResponse | None = None
            transport: object | None = None
            with CaptureDeadline(MAX_CAPTURE_SECONDS):
                try:
                    plan = candidate_request_profile(case, model)
                    connection = open_connection(
                        scheme, host, port, remaining_seconds(deadline)
                    )
                    remaining_seconds(deadline)
                    connection.connect()
                    connection_transport = connection.sock
                    if connection_transport is None:
                        raise CaptureError("capture-transport")
                    transport = retained_transport(connection_transport)
                    body = json.dumps(
                        plan.body, separators=(",", ":"), ensure_ascii=False
                    ).encode("utf-8")
                    connection_transport.settimeout(remaining_seconds(deadline))
                    connection.request(
                        "POST",
                        endpoint,
                        body=body,
                        headers={
                            "Authorization": f"Bearer {bearer_token}",
                            "Content-Type": "application/json",
                            "Content-Length": str(len(body)),
                        },
                    )
                    connection_transport.settimeout(remaining_seconds(deadline))
                    response = connection.getresponse()
                    raw_body, truncated = read_bounded_response(
                        response, plan.body["stream"], deadline
                    )
                    write_private_at(
                        raw_directory.directory_descriptor,
                        f"{case}.raw.json",
                        raw_envelope(
                            case, plan.body["stream"], response, raw_body, truncated
                        ),
                    )
                finally:
                    close_quietly(response)
                    close_quietly(transport)
                    close_quietly(connection)
        if not public_output_matches(
            output_dir, repository, os.fstat(raw_directory.directory_descriptor)
        ):
            raise CaptureError("capture-path-boundary")
        completed = True
    except CaptureError:
        raise
    except (OSError, ValueError, http.client.HTTPException) as error:
        raise CaptureError("capture-transport") from error
    finally:
        if raw_directory is not None:
            if completed:
                close_raw_directory(raw_directory)
            else:
                cleanup_raw_directory(
                    raw_directory,
                    (".nim-capture-raw-v1", *(f"{case}.raw.json" for case in cases)),
                )


def exact_boundary_sets(
    required: bool, observed: bool, check: str
) -> tuple[set[Problem], set[Problem]]:
    """Translate an opaque candidate verdict into a stable oracle check id."""
    expected = {Problem(check)} if required else set()
    actual = {Problem(check)} if observed else set()
    return expected, actual


def oracle_environment(environment: dict[str, str]) -> bool:
    return not all(name in environment for name in REQUIRED_ENVIRONMENT)


def oracle_url(base_url: str) -> bool:
    parsed = urllib.parse.urlsplit(base_url)
    loopback = {"127.0.0.1", "::1"}
    safe = (
        parsed.scheme == "https"
        or (parsed.scheme == "http" and parsed.hostname in loopback)
    ) and not (
        parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
    ) and parsed.path.rstrip("/") == "/v1"
    return not safe


def oracle_path(
    output_dir: pathlib.Path,
    repository: pathlib.Path,
    require_exclusive_nofollow: bool,
) -> bool:
    resolved_repository = repository.resolve()
    resolved_output = output_dir.resolve(strict=False)
    lexical_repository = repository.absolute()
    lexical_output = output_dir.absolute()
    has_symlink_component = output_dir.is_symlink() or any(
        parent.is_symlink() for parent in output_dir.parents
    )
    outside_repository_lexically = not lexical_output.is_relative_to(
        lexical_repository
    )
    outside_repository = not resolved_output.is_relative_to(resolved_repository)
    safe = (
        output_dir.is_absolute()
        and not output_dir.exists()
        and not has_symlink_component
        and outside_repository_lexically
        and outside_repository
    )
    del require_exclusive_nofollow
    return not safe


def oracle_file_flags(decision: CandidateDecision) -> set[Problem]:
    if decision.file_flags == EXCLUSIVE_NOFOLLOW_FLAGS:
        return set()
    return {Problem("capture-path-boundary")}


def oracle_owner_mode(
    path: pathlib.Path,
    expected_mode: int,
) -> bool:
    actual_mode = stat.S_IMODE(path.stat().st_mode)
    return actual_mode != expected_mode


def oracle_limit(
    response_bytes: int,
    event_count: int,
    elapsed_seconds: int,
) -> bool:
    return (
        response_bytes > MAX_RESPONSE_BYTES
        or event_count > MAX_SSE_EVENTS
        or elapsed_seconds > MAX_CAPTURE_SECONDS
    )


def oracle_diagnostic(channel: str, diagnostic: str, secret: str) -> set[Problem]:
    if secret not in diagnostic:
        return set()
    return {Problem(f"capture-secret-leak:{channel}")}


def oracle_request_profile(
    case: str,
    model: str,
    plan: CaptureRequestPlan,
    prompt: object | None,
) -> set[Problem]:
    streamed = case.startswith("streamed-")
    tools = case.endswith("-tools")
    body = plan.body
    expected_keys = {"max_tokens", "messages", "model", "stream"}
    if streamed:
        expected_keys.add("stream_options")
    if tools:
        expected_keys.update({"tool_choice", "tools"})
    problems = set()
    if set(body) != expected_keys:
        problems.add(Problem(f"capture-request-profile:{case}"))
    if body.get("model") != model:
        problems.add(Problem(f"capture-request-profile:{case}"))
    if body.get("stream") is not streamed:
        problems.add(Problem(f"capture-request-profile:{case}"))
    if plan.attempts != 1 or plan.retries != 0:
        problems.add(Problem(f"capture-request-profile:{case}"))
    if not isinstance(body.get("max_tokens"), int) or body["max_tokens"] <= 0:
        problems.add(Problem(f"capture-request-profile:{case}"))
    if not isinstance(body.get("messages"), list) or not body["messages"]:
        problems.add(Problem(f"capture-request-profile:{case}"))
    elif prompt is not None and body["messages"] != prompt:
        problems.add(Problem(f"capture-request-profile:{case}"))
    if streamed:
        if body.get("stream_options") != {"include_usage": True}:
            problems.add(Problem(f"capture-request-profile:{case}"))
    elif "stream_options" in body:
        problems.add(Problem(f"capture-request-profile:{case}"))
    if tools:
        tool_list = body.get("tools")
        tool_choice = body.get("tool_choice")
        if tool_list != [FIXED_TOOL] or tool_choice != "required":
            problems.add(Problem(f"capture-request-profile:{case}"))
    elif "tools" in body or "tool_choice" in body:
        problems.add(Problem(f"capture-request-profile:{case}"))
    return problems


def oracle_control_plan(case: str, model: str) -> CaptureRequestPlan:
    """A synthetic complete plan used only to prove this oracle can fail."""
    body = {
        "max_tokens": 1,
        "messages": [{"content": "fixture", "role": "user"}],
        "model": model,
        "stream": case.startswith("streamed-"),
    }
    if case.startswith("streamed-"):
        body["stream_options"] = {"include_usage": True}
    if case.endswith("-tools"):
        body["tools"] = [copy.deepcopy(FIXED_TOOL)]
        body["tool_choice"] = "required"
    return CaptureRequestPlan(body=body, attempts=1, retries=0)


def checks(problems: set[Problem]) -> list[str]:
    return [problem.check for problem in sorted(problems)]


def expect_exact(
    name: str, expected: set[Problem], observed: set[Problem], failures: list[str]
) -> None:
    """GREEN expectation: candidate output has the exact fixed oracle checks."""
    if observed != expected:
        failures.append(
            f"{name}: expected {checks(expected)}; observed {checks(observed)}"
        )
    else:
        print(f"  ok  {name:34} candidate matches its fixed oracle")


def expect_operational_control(
    check: str, fulfilled: bool, failures: list[str]
) -> None:
    """Record a future-facing operational regression without exposing fixture data."""
    if fulfilled:
        print(f"  ok  {check:34} operational regression covered")
    else:
        failures.append(check)


def operational_total_deadline(root: pathlib.Path) -> bool:
    """Require a fresh remaining timeout at every blocking transport boundary."""
    original_environment = capture_environment
    original_endpoint = capture_endpoint
    original_open_connection = open_connection
    original_remaining_seconds = remaining_seconds
    original_monotonic = time.monotonic
    stages: dict[str, tuple[int, float]] = {}
    remaining_values: list[float] = []

    class Clock:
        calls = 0

        def __call__(self) -> float:
            self.calls += 1
            return float(self.calls - 1)

    class Socket:
        def settimeout(self, timeout: float) -> None:
            del timeout

    class Response:
        status = 200

        def __init__(self, clock: Clock) -> None:
            self.clock = clock
            self.reads = 0

        def getheader(self, name: str) -> str:
            del name
            return "application/json"

        def read(self, amount: int) -> bytes:
            del amount
            stages["body"] = (self.clock.calls, remaining_values[-1])
            self.reads += 1
            return b""

    class Connection:
        def __init__(self, clock: Clock) -> None:
            self.clock = clock
            self.sock = Socket()
            self.response = Response(clock)

        def connect(self) -> None:
            stages["connect"] = (self.clock.calls, remaining_values[-1])

        def request(self, *args, **kwargs) -> None:
            del args, kwargs
            stages["request"] = (self.clock.calls, remaining_values[-1])

        def getresponse(self) -> Response:
            stages["headers"] = (self.clock.calls, remaining_values[-1])
            return self.response

        def close(self) -> None:
            return None

    clock = Clock()

    def fake_open_connection(
        scheme: str, host: str, port: int | None, timeout: float
    ) -> Connection:
        del scheme, host, port, timeout
        return Connection(clock)

    def traced_remaining_seconds(deadline: float) -> float:
        value = original_remaining_seconds(deadline)
        remaining_values.append(value)
        return value

    try:
        globals()["capture_environment"] = lambda: ("https://fixture.invalid", "token", "model")
        globals()["capture_endpoint"] = lambda base_url: ("https", "fixture.invalid", None, "/v1/chat/completions")
        globals()["open_connection"] = fake_open_connection
        globals()["remaining_seconds"] = traced_remaining_seconds
        time.monotonic = clock
        candidate_capture(("buffered-basic",), root / "deadline-output")
        observed = [stages[name] for name in ("connect", "request", "headers", "body")]
        return all(
            later[0] > earlier[0] and later[1] < earlier[1]
            for earlier, later in zip(observed, observed[1:])
        )
    except Exception:
        return False
    finally:
        globals()["capture_environment"] = original_environment
        globals()["capture_endpoint"] = original_endpoint
        globals()["open_connection"] = original_open_connection
        globals()["remaining_seconds"] = original_remaining_seconds
        time.monotonic = original_monotonic


def operational_byte_cap() -> bool:
    """A full retained buffer must finish without a probing read that can block."""
    class Socket:
        def settimeout(self, timeout: float) -> None:
            del timeout

    class Response:
        def __init__(self) -> None:
            self.remaining = MAX_RESPONSE_BYTES
            self.requests: list[int] = []

        def read(self, amount: int) -> bytes:
            self.requests.append(amount)
            if amount > self.remaining:
                raise AssertionError("read asked for more than the retained room")
            if self.remaining == 0:
                raise AssertionError("a full response must not be read again")
            if self.remaining == READ_CHUNK_BYTES:
                size = READ_CHUNK_BYTES - 1
            else:
                size = min(amount, self.remaining)
            self.remaining -= size
            return b"x" * size

    response = Response()
    try:
        body, truncated = read_bounded_response(
            response, False, time.monotonic() + 1
        )
    except Exception:
        return False
    return (
        len(body) == MAX_RESPONSE_BYTES
        and truncated
        and response.remaining == 0
        and response.requests[-1] == 1
    )


def operational_event_cap() -> bool:
    """The 2049th SSE event is never retained, even when it has no terminator."""
    complete_events = b"data: {}\n\n" * MAX_SSE_EVENTS
    partial_event = b"data: partial"

    class Response:
        def __init__(self, chunks: list[bytes]) -> None:
            self.chunks = iter(chunks)

        def read(self, amount: int) -> bytes:
            del amount
            return next(self.chunks, b"")

    def retained_until(chunks: list[bytes], deadline: float) -> tuple[bytes, bool] | None:
        try:
            return read_bounded_response(Response(chunks), True, deadline)
        except Exception:
            return None

    eof = retained_until([complete_events + partial_event], time.monotonic() + 1)
    original_monotonic = time.monotonic
    ticks = iter((0.0, 2.0))
    try:
        time.monotonic = lambda: next(ticks)
        deadline = retained_until([complete_events + partial_event], 1.0)
    except StopIteration:
        deadline = None
    finally:
        time.monotonic = original_monotonic
    fill = MAX_RESPONSE_BYTES - len(complete_events)
    byte_cap = retained_until(
        [complete_events + partial_event + (b"x" * max(fill, 0)) + b"!"],
        time.monotonic() + 1,
    )
    return all(
        result == (complete_events, True) for result in (eof, deadline, byte_cap)
    )


def operational_closing_response_socket() -> bool:
    """The capture keeps its transport before a closing response disowns it."""
    original_environment = capture_environment
    original_endpoint = capture_endpoint
    original_open_connection = open_connection

    class Socket:
        def __init__(self) -> None:
            self.timeouts: list[float] = []

        def settimeout(self, timeout: float) -> None:
            self.timeouts.append(timeout)

    class Response:
        status = 200

        def __init__(self) -> None:
            self.reads = 0

        def getheader(self, name: str) -> str:
            del name
            return "application/json"

        def read(self, amount: int) -> bytes:
            del amount
            self.reads += 1
            return b"ok" if self.reads == 1 else b""

    class Connection:
        def __init__(self, socket: Socket) -> None:
            self.sock = socket

        def connect(self) -> None:
            return None

        def request(self, *args, **kwargs) -> None:
            del args, kwargs

        def getresponse(self) -> Response:
            self.sock = None
            return Response()

        def close(self) -> None:
            return None

    socket = Socket()
    try:
        globals()["capture_environment"] = lambda: ("https://fixture.invalid", "token", "model")
        globals()["capture_endpoint"] = lambda base_url: ("https", "fixture.invalid", None, "/v1/chat/completions")
        globals()["open_connection"] = lambda scheme, host, port, timeout: Connection(socket)
        with tempfile.TemporaryDirectory(prefix="nim-capture-closing-") as temporary:
            candidate_capture(("buffered-basic",), pathlib.Path(temporary) / "output")
    except Exception:
        return False
    finally:
        globals()["capture_environment"] = original_environment
        globals()["capture_endpoint"] = original_endpoint
        globals()["open_connection"] = original_open_connection
    return bool(socket.timeouts)


def operational_http10_buffered_capture(root: pathlib.Path) -> bool:
    """A real HTTP/1.0 close must not prevent bounded buffered publication."""
    original_environment = capture_environment
    requests: list[tuple[str, str, bytes]] = []
    response_body = b'{"choices":[]}'

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.0"

        def do_POST(self) -> None:
            content_length = int(self.headers["Content-Length"])
            requests.append((self.command, self.path, self.rfile.read(content_length)))
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(response_body)))
            self.end_headers()
            self.wfile.write(response_body)

        def log_message(self, format: str, *args: object) -> None:
            del format, args

    server = HTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    output = root / "http10-buffered-output"
    try:
        thread.start()
        globals()["capture_environment"] = lambda: (
            f"http://127.0.0.1:{server.server_port}/v1", "token", "model"
        )
        candidate_capture(("buffered-basic",), output)
    except Exception:
        return False
    finally:
        globals()["capture_environment"] = original_environment
        server.shutdown()
        server.server_close()
        thread.join()
    try:
        envelope = json.loads((output / "buffered-basic.raw.json").read_bytes())
        request = requests[0]
        return (
            len(requests) == 1
            and request[0:2] == ("POST", "/v1/chat/completions")
            and json.loads(request[2])["stream"] is False
            and {entry.name for entry in output.iterdir()}
            == {".nim-capture-raw-v1", "buffered-basic.raw.json"}
            and set(envelope)
            == {
                "format",
                "case",
                "capture_date",
                "transport",
                "status",
                "content_type",
                "truncated",
                "body_b64",
            }
            and envelope["format"] == "nim-capture-raw-v1"
            and envelope["case"] == "buffered-basic"
            and envelope["transport"] == "buffered"
            and envelope["status"] == 200
            and envelope["content_type"] == "application/json"
            and envelope["truncated"] is False
            and base64.b64decode(envelope["body_b64"], validate=True) == response_body
        )
    except (IndexError, KeyError, OSError, ValueError, json.JSONDecodeError):
        return False


def operational_publication_path_swap(root: pathlib.Path) -> bool:
    """A final parent swap must fail without writing the attacker pathname."""
    original_environment = capture_environment
    original_write_private_at = write_private_at
    parent = root / "publication-parent"
    parked = root / "publication-parked"
    attacker = root / "publication-attacker"
    parent.mkdir()
    attacker.mkdir()
    output = parent / "raw"
    requests: list[tuple[str, str, bytes]] = []
    response_body = b'{"choices":[]}'
    swapped = False

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.0"

        def do_POST(self) -> None:
            content_length = int(self.headers["Content-Length"])
            requests.append((self.command, self.path, self.rfile.read(content_length)))
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(response_body)))
            self.end_headers()
            self.wfile.write(response_body)

        def log_message(self, format: str, *args: object) -> None:
            del format, args

    def swap_after_marker(directory_descriptor: int, name: str, content: bytes) -> None:
        nonlocal swapped
        original_write_private_at(directory_descriptor, name, content)
        if name == ".nim-capture-raw-v1":
            parent.rename(parked)
            parent.symlink_to(attacker, target_is_directory=True)
            swapped = True

    server = HTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    failed = False
    try:
        thread.start()
        globals()["capture_environment"] = lambda: (
            f"http://127.0.0.1:{server.server_port}/v1", "token", "model"
        )
        globals()["write_private_at"] = swap_after_marker
        try:
            candidate_capture(("buffered-basic",), output)
        except CaptureError as error:
            failed = error.check == "capture-path-boundary"
    except Exception:
        return False
    finally:
        globals()["capture_environment"] = original_environment
        globals()["write_private_at"] = original_write_private_at
        server.shutdown()
        server.server_close()
        thread.join()
    return (
        swapped
        and failed
        and len(requests) == 1
        and requests[0][0:2] == ("POST", "/v1/chat/completions")
        and json.loads(requests[0][2])["stream"] is False
        and parent.is_symlink()
        and not output.exists()
        and not (attacker / output.name).exists()
        and not (parked / output.name).exists()
    )


def operational_parent_symlink_race(root: pathlib.Path) -> bool:
    """A post-validation parent swap must not redirect raw creation into the repo."""
    repository = root / "race-repository"
    repository.mkdir()
    parent = root / "race-parent"
    parent.mkdir()
    output = parent / "raw"
    original_mkdir = os.mkdir
    directory: RawDirectory | None = None
    swapped = False

    def swap_parent(path, mode=0o777, *, dir_fd=None):
        nonlocal swapped
        is_path_creation = pathlib.Path(path) == output
        is_dirfd_creation = dir_fd is not None and pathlib.Path(path).name == output.name
        if (is_path_creation or is_dirfd_creation) and not swapped:
            swapped = True
            parent.rmdir()
            parent.symlink_to(repository, target_is_directory=True)
        return original_mkdir(path, mode, dir_fd=dir_fd)

    try:
        os.mkdir = swap_parent
        directory = create_raw_directory(output, repository)
    except CaptureError:
        pass
    except OSError:
        pass
    finally:
        os.mkdir = original_mkdir
        if directory is not None:
            close_raw_directory(directory)
    return swapped and not (repository / "raw").exists()


def operational_publication_sync(root: pathlib.Path) -> bool:
    """Publishing a raw filename also syncs the directory entry that names it."""
    directory = root / "publication-directory"
    directory.mkdir()
    descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    original_fsync = os.fsync
    synced: list[int] = []

    def trace_fsync(file_descriptor: int) -> None:
        synced.append(file_descriptor)
        original_fsync(file_descriptor)

    try:
        os.fsync = trace_fsync
        write_private_at(descriptor, "buffered-basic.raw.json", b"{}")
    except Exception:
        return False
    finally:
        os.fsync = original_fsync
        os.close(descriptor)
    return bool(synced) and synced[-1] == descriptor


def operational_oracle_independence(root: pathlib.Path) -> bool:
    """Production decisions must not borrow the test-only oracle implementation."""
    names = (
        "oracle_url",
        "oracle_path",
        "oracle_owner_mode",
        "oracle_limit",
    )
    originals = {name: globals()[name] for name in names}
    outside = root / "independent-outside"
    owner_only = root / "independent-owner-only"
    owner_only.mkdir(mode=OWNER_DIRECTORY_MODE)
    os.chmod(owner_only, OWNER_DIRECTORY_MODE)

    def oracle_called(*args, **kwargs):
        del args, kwargs
        raise AssertionError("production predicate called a test oracle")

    try:
        for name in names:
            globals()[name] = oracle_called
        candidate_environment({name: "fixture" for name in REQUIRED_ENVIRONMENT})
        candidate_url("https://fixture.invalid/v1")
        candidate_path(outside, root / "repository")
        candidate_owner_mode(owner_only, OWNER_DIRECTORY_MODE)
        candidate_limit(0, 0, 0)
    except Exception:
        return False
    finally:
        for name, original in originals.items():
            globals()[name] = original
    return True


def operational_tls_transport() -> bool:
    """A real stdlib TLS socket must remain usable as the response transport."""
    client, peer = socket.socketpair()
    secure: ssl.SSLSocket | None = None
    try:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        context.check_hostname = False
        context.verify_mode = ssl.CERT_NONE
        secure = context.wrap_socket(
            client,
            server_hostname="loopback.invalid",
            do_handshake_on_connect=False,
        )
        return retained_transport(secure) is secure
    except (NotImplementedError, OSError, ValueError):
        return False
    finally:
        if secure is not None:
            secure.close()
        else:
            client.close()
        peer.close()


def operational_partial_cleanup(root: pathlib.Path) -> bool:
    """Marker and response-write failures leave no created raw directory behind."""
    repository = root / "cleanup-repository"
    repository.mkdir()
    marker_output = root / "cleanup-marker-output"
    original_write_all = write_all

    def fail_marker(file_descriptor: int, content: bytes) -> None:
        del content
        os.write(file_descriptor, b"partial")
        raise OSError("synthetic marker write failure")

    try:
        globals()["write_all"] = fail_marker
        try:
            create_raw_directory(marker_output, repository)
        except CaptureError:
            pass
        else:
            return False
    finally:
        globals()["write_all"] = original_write_all
    if marker_output.exists():
        return False

    response_output = root / "cleanup-response-output"
    original_environment = capture_environment
    original_endpoint = capture_endpoint
    original_open_connection = open_connection
    writes = 0

    class Transport:
        def settimeout(self, timeout: float) -> None:
            del timeout

        def close(self) -> None:
            return None

    class Response:
        status = 200

        def getheader(self, name: str) -> str:
            del name
            return "application/json"

        def read(self, amount: int) -> bytes:
            del amount
            return b""

        def close(self) -> None:
            return None

    class Connection:
        def __init__(self) -> None:
            self.sock = Transport()

        def connect(self) -> None:
            return None

        def request(self, *args, **kwargs) -> None:
            del args, kwargs

        def getresponse(self) -> Response:
            return Response()

        def close(self) -> None:
            return None

    def fail_response(file_descriptor: int, content: bytes) -> None:
        nonlocal writes
        writes += 1
        if writes == 2:
            os.write(file_descriptor, b"partial")
            raise OSError("synthetic response write failure")
        original_write_all(file_descriptor, content)

    try:
        globals()["capture_environment"] = lambda: (
            "https://fixture.invalid", "token", "model"
        )
        globals()["capture_endpoint"] = lambda base_url: (
            "https", "fixture.invalid", None, "/v1/chat/completions"
        )
        globals()["open_connection"] = lambda scheme, host, port, timeout: Connection()
        globals()["write_all"] = fail_response
        try:
            candidate_capture(("buffered-basic",), response_output)
        except CaptureError:
            pass
        else:
            return False
    finally:
        globals()["write_all"] = original_write_all
        globals()["capture_environment"] = original_environment
        globals()["capture_endpoint"] = original_endpoint
        globals()["open_connection"] = original_open_connection
    return writes == 2 and not response_output.exists()


def operational_competing_leaf(root: pathlib.Path) -> bool:
    """A racing pre-existing output leaf must survive the failed creation."""
    repository = root / "competing-repository"
    repository.mkdir()
    parent = root / "competing-parent"
    parent.mkdir()
    output = parent / "raw"
    original_mkdir = os.mkdir
    competed = False

    def create_competing_leaf(path, mode=0o777, *, dir_fd=None):
        nonlocal competed
        if dir_fd is not None and pathlib.Path(path).name == output.name and not competed:
            competed = True
            original_mkdir(path, mode, dir_fd=dir_fd)
            os.chmod(path, 0o755, dir_fd=dir_fd)
            raise FileExistsError("synthetic competing output")
        return original_mkdir(path, mode, dir_fd=dir_fd)

    try:
        os.mkdir = create_competing_leaf
        try:
            create_raw_directory(output, repository)
        except CaptureError:
            pass
        else:
            return False
    finally:
        os.mkdir = original_mkdir
    return competed and output.is_dir() and stat.S_IMODE(output.stat().st_mode) == 0o755


def cleanup_fixture(directory: pathlib.Path) -> None:
    """Create one complete private raw set for cleanup-only operational controls."""
    directory.mkdir(mode=OWNER_DIRECTORY_MODE)
    os.chmod(directory, OWNER_DIRECTORY_MODE)
    (directory / ".nim-capture-raw-v1").write_bytes(b"nim-capture-raw-v1\n")
    os.chmod(directory / ".nim-capture-raw-v1", OWNER_FILE_MODE)
    for case in CASES:
        path = directory / f"{case}.raw.json"
        path.write_bytes(b"{}")
        os.chmod(path, OWNER_FILE_MODE)


def operational_cleanup_exact_set(root: pathlib.Path) -> bool:
    """The cleanup CLI clears one exact raw set but retains its raw directory."""
    parent = root / "cleanup-exact-parent"
    parent.mkdir()
    output = parent / "raw"
    cleanup_fixture(output)
    output_stat = output.stat()
    original_fsync = os.fsync
    original_environment = capture_environment
    synced_raw = False

    def trace_fsync(file_descriptor: int) -> None:
        nonlocal synced_raw
        metadata = os.fstat(file_descriptor)
        if (metadata.st_dev, metadata.st_ino) == (output_stat.st_dev, output_stat.st_ino):
            synced_raw = True
        original_fsync(file_descriptor)

    try:
        os.fsync = trace_fsync
        globals()["capture_environment"] = lambda: (_ for _ in ()).throw(
            AssertionError("cleanup read credential environment")
        )
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(
            io.StringIO()
        ):
            try:
                status = main(["--cleanup", "--output-dir", str(output)])
            except SystemExit:
                return False
    finally:
        os.fsync = original_fsync
        globals()["capture_environment"] = original_environment
    return (
        status == 0
        and parent.is_dir()
        and output.is_dir()
        and not os.listdir(output)
        and synced_raw
    )


def operational_cleanup_rejects_hostile_sets(root: pathlib.Path) -> bool:
    """Malformed raw sets fail closed without deleting targets or local evidence."""
    repository = pathlib.Path(__file__).resolve().parent.parent
    names = (".nim-capture-raw-v1", *(f"{case}.raw.json" for case in CASES))

    def rejected(name: str, mutate) -> bool:
        directory = root / name
        cleanup_fixture(directory)
        target = root / f"{name}-target"
        target.write_bytes(b"target")
        mutate(directory, target)
        before = set(os.listdir(directory))
        try:
            cleanup_capture_directory(directory, repository)
        except CaptureError as error:
            return (
                error.check == "capture-cleanup"
                and directory.is_dir()
                and set(os.listdir(directory)) == before
                and target.read_bytes() == b"target"
            )
        except Exception:
            return False
        return False

    checks = []
    for index, file_name in enumerate(names):
        checks.append(
            rejected(
                f"cleanup-symlink-{index}",
                lambda directory, target, file_name=file_name: (
                    os.unlink(directory / file_name),
                    os.symlink(target, directory / file_name),
                ),
            )
        )
        checks.append(
            rejected(
                f"cleanup-mode-{index}",
                lambda directory, target, file_name=file_name: os.chmod(
                    directory / file_name, 0o644
                ),
            )
        )
    checks.append(
        rejected(
            "cleanup-extra",
            lambda directory, target: (directory / "extra").write_bytes(b"extra"),
        )
    )
    checks.append(
        rejected(
            "cleanup-missing",
            lambda directory, target: os.unlink(directory / f"{CASES[0]}.raw.json"),
        )
    )
    checks.append(
        rejected(
            "cleanup-marker-bytes",
            lambda directory, target: (directory / ".nim-capture-raw-v1").write_bytes(
                b"wrong-marker\n"
            ),
        )
    )
    checks.append(
        rejected(
            "cleanup-directory-mode",
            lambda directory, target: os.chmod(directory, 0o755),
        )
    )
    return all(checks)


def operational_cleanup_public_swap(root: pathlib.Path) -> bool:
    """A public parent or leaf swap cannot redirect cleanup or return success."""
    repository = pathlib.Path(__file__).resolve().parent.parent

    def swapped(kind: str) -> bool:
        parent = root / f"cleanup-{kind}-parent"
        parent.mkdir()
        output = parent / "raw"
        cleanup_fixture(output)
        attacker_parent = root / f"cleanup-{kind}-attacker"
        attacker_parent.mkdir()
        attacker = attacker_parent / "raw"
        cleanup_fixture(attacker)
        parked = root / f"cleanup-{kind}-parked"
        original_unlink = os.unlink
        did_swap = False
        attacker_before = {
            name: (attacker / name).read_bytes()
            for name in os.listdir(attacker)
        }

        def swap_then_unlink(path, *, dir_fd=None):
            nonlocal did_swap
            if not did_swap and dir_fd is not None and path == f"{CASES[0]}.raw.json":
                if kind == "parent":
                    parent.rename(parked)
                    parent.symlink_to(attacker_parent, target_is_directory=True)
                else:
                    output.rename(parked)
                    output.symlink_to(attacker, target_is_directory=True)
                did_swap = True
            return original_unlink(path, dir_fd=dir_fd)

        try:
            os.unlink = swap_then_unlink
            try:
                cleanup_capture_directory(output, repository)
            except CaptureError as error:
                failed = error.check == "capture-path-boundary"
            else:
                failed = False
        except Exception:
            return False
        finally:
            os.unlink = original_unlink
        return (
            did_swap
            and failed
            and attacker.is_dir()
            and {
                name: (attacker / name).read_bytes()
                for name in os.listdir(attacker)
            }
            == attacker_before
        )

    return swapped("parent") and swapped("leaf")


def operational_cleanup_replacement_leaf(root: pathlib.Path) -> bool:
    """A reviewed raw directory is never replaced by a pathname-based removal."""
    repository = pathlib.Path(__file__).resolve().parent.parent
    parent = root / "cleanup-replacement-parent"
    parent.mkdir()
    output = parent / "raw"
    cleanup_fixture(output)
    parked = root / "cleanup-replacement-parked"
    original_rmdir = os.rmdir
    attempted_removal = False

    def replace_before_rmdir(path, *, dir_fd=None):
        nonlocal attempted_removal
        if not attempted_removal and dir_fd is not None and path == output.name:
            attempted_removal = True
            output.rename(parked)
            output.mkdir(mode=OWNER_DIRECTORY_MODE)
            os.chmod(output, OWNER_DIRECTORY_MODE)
        return original_rmdir(path, dir_fd=dir_fd)

    try:
        os.rmdir = replace_before_rmdir
        try:
            cleanup_capture_directory(output, repository)
        except CaptureError:
            return False
    except Exception:
        return False
    finally:
        os.rmdir = original_rmdir
    return (
        not attempted_removal
        and output.is_dir()
        and not os.listdir(output)
        and not parked.exists()
    )


def operational_cleanup_fifo(root: pathlib.Path) -> bool:
    """A fixed-name FIFO rejects promptly and remains untouched."""
    repository = pathlib.Path(__file__).resolve().parent.parent
    directory = root / "cleanup-fifo"
    cleanup_fixture(directory)
    fifo = directory / f"{CASES[0]}.raw.json"
    os.unlink(fifo)
    os.mkfifo(fifo, OWNER_FILE_MODE)
    os.chmod(fifo, OWNER_FILE_MODE)
    before = set(os.listdir(directory))
    result: list[str] = []
    complete = threading.Event()

    def attempt_cleanup() -> None:
        try:
            cleanup_capture_directory(directory, repository)
        except CaptureError as error:
            result.append(error.check)
        except Exception:
            result.append("unexpected")
        finally:
            complete.set()

    thread = threading.Thread(target=attempt_cleanup, daemon=True)
    thread.start()
    if not complete.wait(0.2):
        return False
    return (
        result == ["capture-cleanup"]
        and stat.S_ISFIFO(os.stat(fifo, follow_symlinks=False).st_mode)
        and stat.S_IMODE(os.stat(fifo, follow_symlinks=False).st_mode) == OWNER_FILE_MODE
        and set(os.listdir(directory)) == before
    )


def operational_four_case_orchestration(root: pathlib.Path) -> bool:
    """The production CLI publishes one complete canonical raw set."""
    original_environment = {
        name: os.environ.get(name) for name in REQUIRED_ENVIRONMENT
    }
    requests: list[dict] = []
    response_body = b'{"choices":[]}'

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.0"

        def do_POST(self) -> None:
            content_length = int(self.headers["Content-Length"])
            request = json.loads(self.rfile.read(content_length))
            requests.append(request)
            body = b"data: [DONE]\n\n" if request["stream"] else response_body
            self.send_response(200)
            self.send_header(
                "Content-Type", "text/event-stream" if request["stream"] else "application/json"
            )
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, format: str, *args: object) -> None:
            del format, args

    def rejects_before_capture(cases: tuple[str, ...], name: str) -> bool:
        original_capture_environment = capture_environment
        original_create_raw_directory = create_raw_directory
        original_open_connection = open_connection
        created = 0
        opened = 0
        output = root / name

        def record_create(*args, **kwargs):
            nonlocal created
            del args, kwargs
            created += 1
            raise AssertionError("invalid case list reached raw-directory creation")

        def record_open(*args, **kwargs):
            nonlocal opened
            del args, kwargs
            opened += 1
            raise AssertionError("invalid case list reached transport creation")

        try:
            globals()["capture_environment"] = lambda: (
                "https://fixture.invalid", "token", "model"
            )
            globals()["create_raw_directory"] = record_create
            globals()["open_connection"] = record_open
            arguments = ["--output-dir", str(output)]
            for case in cases:
                arguments.extend(("--case", case))
            with contextlib.redirect_stderr(io.StringIO()):
                try:
                    status = main(arguments)
                except SystemExit as error:
                    status = int(error.code)
        finally:
            globals()["capture_environment"] = original_capture_environment
            globals()["create_raw_directory"] = original_create_raw_directory
            globals()["open_connection"] = original_open_connection
        return status != 0 and created == 0 and opened == 0 and not output.exists()

    server = HTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    output = root / "four-case-output"
    try:
        thread.start()
        os.environ.update(
            {
                "NIM_CAPTURE_BASE_URL": f"http://127.0.0.1:{server.server_port}/v1",
                "NIM_CAPTURE_BEARER_TOKEN": "token",
                "NIM_CAPTURE_MODEL": "model",
            }
        )
        arguments = ["--output-dir", str(output)]
        for case in CASES:
            arguments.extend(("--case", case))
        with contextlib.redirect_stdout(io.StringIO()):
            status = main(arguments)
    except Exception:
        return False
    finally:
        for name, value in original_environment.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        server.shutdown()
        server.server_close()
        thread.join()
    try:
        published = [
            json.loads((output / f"{case}.raw.json").read_bytes())["case"]
            for case in CASES
        ]
        valid = (
            status == 0
            and [(request["stream"], "tools" in request) for request in requests]
            == [(False, False), (True, False), (False, True), (True, True)]
            and {entry.name for entry in output.iterdir()}
            == {".nim-capture-raw-v1", *(f"{case}.raw.json" for case in CASES)}
            and published == list(CASES)
            and all(
                stat.S_ISREG((output / name).stat().st_mode)
                and stat.S_IMODE((output / name).stat().st_mode) == OWNER_FILE_MODE
                for name in (".nim-capture-raw-v1", *(f"{case}.raw.json" for case in CASES))
            )
        )
    except (OSError, KeyError, ValueError, json.JSONDecodeError):
        valid = False
    invalid = all(
        rejects_before_capture(cases, name)
        for cases, name in (
            (CASES[:-1], "incomplete-case-list"),
            (CASES[:-1] + (CASES[-2],), "duplicate-case-list"),
            ((CASES[1], CASES[0], *CASES[2:]), "out-of-order-case-list"),
        )
    )
    return valid and invalid


def operational_midset_failure_cleanup(root: pathlib.Path) -> bool:
    """A third-case transport failure leaves no partial run or attacker write."""
    original_environment = {
        name: os.environ.get(name) for name in REQUIRED_ENVIRONMENT
    }
    requests: list[dict] = []
    attacker = root / "midset-attacker"
    competing = root / "midset-competing"
    attacker.mkdir()
    competing.mkdir()
    (attacker / "sentinel").write_bytes(b"attacker")
    (competing / "sentinel").write_bytes(b"competing")

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.0"

        def do_POST(self) -> None:
            content_length = int(self.headers["Content-Length"])
            request = json.loads(self.rfile.read(content_length))
            requests.append(request)
            if len(requests) == 3:
                self.close_connection = True
                return
            body = b"data: [DONE]\n\n" if request["stream"] else b'{"choices":[]}'
            self.send_response(200)
            self.send_header(
                "Content-Type", "text/event-stream" if request["stream"] else "application/json"
            )
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, format: str, *args: object) -> None:
            del format, args

    server = HTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    output = root / "midset-output"
    try:
        thread.start()
        os.environ.update(
            {
                "NIM_CAPTURE_BASE_URL": f"http://127.0.0.1:{server.server_port}/v1",
                "NIM_CAPTURE_BEARER_TOKEN": "token",
                "NIM_CAPTURE_MODEL": "model",
            }
        )
        arguments = ["--output-dir", str(output)]
        for case in CASES:
            arguments.extend(("--case", case))
        with contextlib.redirect_stderr(io.StringIO()), contextlib.redirect_stdout(
            io.StringIO()
        ):
            status = main(arguments)
    except Exception:
        return False
    finally:
        for name, value in original_environment.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        server.shutdown()
        server.server_close()
        thread.join()
    return (
        status == 1
        and [(request["stream"], "tools" in request) for request in requests]
        == [(False, False), (True, False), (False, True)]
        and not output.exists()
        and {entry.name for entry in attacker.iterdir()} == {"sentinel"}
        and (attacker / "sentinel").read_bytes() == b"attacker"
        and {entry.name for entry in competing.iterdir()} == {"sentinel"}
        and (competing / "sentinel").read_bytes() == b"competing"
    )


def selftest() -> int:
    """Exercise frozen hostile inputs without touching a real environment or network."""
    failures: list[str] = []
    sentinel = "capture-secret-sentinel-not-a-credential"
    with tempfile.TemporaryDirectory(prefix="nim-capture-selftest-") as temporary:
        root = pathlib.Path(temporary)
        repository = root / "repository"
        repository.mkdir()
        existing_directory = root / "existing-directory"
        existing_directory.mkdir()
        existing_file = root / "existing-file"
        existing_file.write_bytes(b"fixture")
        alias = root / "repository-alias"
        alias.symlink_to(repository, target_is_directory=True)
        outside_parent = root / "outside-parent"
        outside_parent.mkdir()
        outside_alias = root / "outside-alias"
        outside_alias.symlink_to(outside_parent, target_is_directory=True)
        valid_directory = root / "new-owner-only-output"
        valid_mode_directory = root / "owner-only-directory"
        valid_mode_directory.mkdir(mode=OWNER_DIRECTORY_MODE)
        os.chmod(valid_mode_directory, OWNER_DIRECTORY_MODE)
        valid_mode_file = root / "owner-only-file"
        valid_mode_file.write_bytes(b"fixture")
        os.chmod(valid_mode_file, OWNER_FILE_MODE)
        drift_directory = root / "permission-drift-directory"
        drift_directory.mkdir(mode=0o755)
        drift_file = root / "permission-drift-file"
        drift_file.write_bytes(b"fixture")
        os.chmod(drift_file, 0o644)
        nofollow_target = root / "nofollow-target"
        nofollow_target.write_bytes(b"fixture")
        nofollow_alias = root / "nofollow-alias"
        nofollow_alias.symlink_to(nofollow_target)

        complete_environment = {
            name: "temporary-sentinel" for name in REQUIRED_ENVIRONMENT
        }
        boundary_cases = (
            (
                "complete-temporary-environment",
                *exact_boundary_sets(
                    oracle_environment(complete_environment),
                    candidate_environment(complete_environment).rejected,
                    "capture-environment",
                ),
            ),
            (
                "missing-environment",
                *exact_boundary_sets(
                    oracle_environment({}), candidate_environment({}).rejected,
                    "capture-environment",
                ),
            ),
            (
                "direct-https-service-root",
                *exact_boundary_sets(
                    oracle_url("https://capture.example.invalid/v1"),
                    candidate_url("https://capture.example.invalid/v1").rejected,
                    "capture-url-boundary",
                ),
            ),
            (
                "literal-loopback-http-service-root",
                *exact_boundary_sets(
                    oracle_url("http://127.0.0.1:8000/v1"),
                    candidate_url("http://127.0.0.1:8000/v1").rejected,
                    "capture-url-boundary",
                ),
            ),
            (
                "missing-v1-service-root",
                *exact_boundary_sets(
                    oracle_url("http://127.0.0.1:8000"),
                    candidate_url("http://127.0.0.1:8000").rejected,
                    "capture-url-boundary",
                ),
            ),
            (
                "forbidden-http-non-loopback",
                *exact_boundary_sets(
                    oracle_url("http://capture.example.invalid/v1"),
                    candidate_url("http://capture.example.invalid/v1").rejected,
                    "capture-url-boundary",
                ),
            ),
            (
                "forbidden-url-components",
                *exact_boundary_sets(
                    oracle_url("https://user:password@example.invalid/v1?query#fragment"),
                    candidate_url(
                        "https://user:password@example.invalid/v1?query#fragment"
                    ).rejected,
                    "capture-url-boundary",
                ),
            ),
        )

        path_cases = (
            ("relative-output-directory", pathlib.Path("relative-output"), False),
            ("new-absolute-outside-repository-output", valid_directory, False),
            ("existing-output-directory", existing_directory, False),
            ("existing-output-file", existing_file, False),
            ("inside-repository-output", repository / "raw", False),
            (
                "lexically-inside-repository-output",
                repository / ".." / "lexically-escaped-output",
                False,
            ),
            ("symlink-aliased-output", alias / "raw", False),
            ("outside-symlink-aliased-output", outside_alias / "raw", False),
            ("exclusive-nofollow-output-file", nofollow_alias, True),
        )
        mode_cases = (
            ("directory-permission-drift", drift_directory, OWNER_DIRECTORY_MODE),
            ("owner-only-directory", valid_mode_directory, OWNER_DIRECTORY_MODE),
            ("file-permission-drift", drift_file, OWNER_FILE_MODE),
            ("owner-only-file", valid_mode_file, OWNER_FILE_MODE),
        )
        limit_cases = (
            ("limits-below-bound", MAX_RESPONSE_BYTES, MAX_SSE_EVENTS, MAX_CAPTURE_SECONDS),
            ("response-byte-limit", MAX_RESPONSE_BYTES + 1, 0, 0),
            ("sse-event-limit", 0, MAX_SSE_EVENTS + 1, 0),
            ("wall-clock-limit", 0, 0, MAX_CAPTURE_SECONDS + 1),
        )
        for name, expected, observed in boundary_cases:
            expect_exact(name, expected, observed, failures)
        for name, output_dir, require_exclusive_nofollow in path_cases:
            decision = candidate_path(output_dir, repository)
            expected, observed = exact_boundary_sets(
                oracle_path(output_dir, repository, require_exclusive_nofollow),
                decision.rejected,
                "capture-path-boundary",
            )
            expect_exact(name, expected, observed, failures)
        expect_exact(
            "exclusive-nofollow-file-policy",
            set(),
            oracle_file_flags(candidate_path(valid_directory, repository)),
            failures,
        )
        for name, path, expected_mode in mode_cases:
            decision = candidate_owner_mode(path, expected_mode)
            expected, observed = exact_boundary_sets(
                oracle_owner_mode(path, expected_mode),
                decision.rejected,
                "capture-owner-mode",
            )
            expect_exact(name, expected, observed, failures)
        for name, response_bytes, event_count, elapsed_seconds in limit_cases:
            decision = candidate_limit(response_bytes, event_count, elapsed_seconds)
            expected, observed = exact_boundary_sets(
                oracle_limit(response_bytes, event_count, elapsed_seconds),
                decision.truncated,
                "capture-limit",
            )
            expect_exact(name, expected, observed, failures)

        for channel in ("stdout", "stderr", "exception"):
            expect_exact(
                f"secret-free-{channel}",
                set(),
                oracle_diagnostic(channel, candidate_diagnostic(channel, sentinel), sentinel),
                failures,
            )

        configured_model = "capture-model-sentinel"
        profiles = {
            case: candidate_request_profile(case, configured_model) for case in CASES
        }
        prompt = profiles["buffered-basic"].body.get("messages")
        for case, plan in profiles.items():
            expect_exact(
                f"request-profile-{case}",
                set(),
                oracle_request_profile(case, configured_model, plan, prompt),
                failures,
            )

        control_case = "buffered-basic"
        control_prompt = oracle_control_plan(control_case, configured_model).body["messages"]
        missing_model = oracle_control_plan(control_case, configured_model)
        missing_model.body.pop("model")
        wrong_model = oracle_control_plan(control_case, configured_model)
        wrong_model.body["model"] = "different-model-sentinel"
        policy_in_body = oracle_control_plan(control_case, configured_model)
        policy_in_body.body["attempts"] = 1
        policy_in_body.body["retries"] = 0
        changed_tool = oracle_control_plan("buffered-tools", configured_model)
        changed_tool.body["tools"][0]["function"]["parameters"] = {
            "type": "object",
            "properties": {"invented": {"type": "string"}},
        }
        mismatched_streamed_tool = oracle_control_plan(
            "streamed-tools", configured_model
        )
        mismatched_streamed_tool.body["tools"][0]["function"][
            "name"
        ] = "different_tool"
        for name, case, plan in (
            ("request-profile-missing-model-control", control_case, missing_model),
            ("request-profile-wrong-model-control", control_case, wrong_model),
            ("request-profile-policy-in-body-control", control_case, policy_in_body),
            ("request-profile-tool-schema-control", "buffered-tools", changed_tool),
            (
                "request-profile-streamed-tool-match-control",
                "streamed-tools",
                mismatched_streamed_tool,
            ),
        ):
            expect_exact(
                name,
                {Problem(f"capture-request-profile:{case}")},
                oracle_request_profile(case, configured_model, plan, control_prompt),
                failures,
            )

        expect_operational_control(
            "capture-total-deadline", operational_total_deadline(root), failures
        )
        expect_operational_control("capture-byte-cap", operational_byte_cap(), failures)
        expect_operational_control("capture-event-cap", operational_event_cap(), failures)
        expect_operational_control(
            "capture-closing-response-socket",
            operational_closing_response_socket(),
            failures,
        )
        expect_operational_control(
            "capture-http10-buffered-lifecycle",
            operational_http10_buffered_capture(root),
            failures,
        )
        expect_operational_control(
            "capture-publication-path-swap",
            operational_publication_path_swap(root),
            failures,
        )
        expect_operational_control(
            "capture-parent-symlink-race",
            operational_parent_symlink_race(root),
            failures,
        )
        expect_operational_control(
            "capture-publication-sync", operational_publication_sync(root), failures
        )
        expect_operational_control(
            "capture-oracle-independence",
            operational_oracle_independence(root),
            failures,
        )
        expect_operational_control(
            "capture-tls-transport", operational_tls_transport(), failures
        )
        expect_operational_control(
            "capture-partial-cleanup", operational_partial_cleanup(root), failures
        )
        expect_operational_control(
            "capture-competing-leaf", operational_competing_leaf(root), failures
        )
        expect_operational_control(
            "capture-four-case-orchestration",
            operational_four_case_orchestration(root),
            failures,
        )
        expect_operational_control(
            "capture-midset-failure-cleanup",
            operational_midset_failure_cleanup(root),
            failures,
        )
        expect_operational_control(
            "capture-cleanup-exact-set", operational_cleanup_exact_set(root), failures
        )
        expect_operational_control(
            "capture-cleanup-hostile-set",
            operational_cleanup_rejects_hostile_sets(root),
            failures,
        )
        expect_operational_control(
            "capture-cleanup-public-swap", operational_cleanup_public_swap(root), failures
        )
        expect_operational_control(
            "capture-cleanup-replacement-leaf",
            operational_cleanup_replacement_leaf(root),
            failures,
        )
        expect_operational_control(
            "capture-cleanup-fifo", operational_cleanup_fifo(root), failures
        )

    if failures:
        print("capture selftest RED — named contract failures remain:")
        for failure in failures:
            print(f"  RED {failure} (not-yet-implemented)")
        return 1
    print("capture selftest ok — every fixed candidate oracle passed")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--case",
        action="append",
        choices=CASES,
        help="repeat each fixed request profile once, in canonical order",
    )
    mode.add_argument("--cleanup", action="store_true", help="clear one reviewed raw capture")
    parser.add_argument("--output-dir", type=pathlib.Path, help="raw capture directory")
    mode.add_argument(
        "--selftest", action="store_true", help="exercise capture contract regressions"
    )
    args = parser.parse_args(argv)
    if args.selftest:
        return selftest()
    if args.cleanup:
        if args.output_dir is None:
            parser.error("--output-dir is required with --cleanup")
        try:
            cleanup_capture_directory(
                args.output_dir, pathlib.Path(__file__).resolve().parent.parent
            )
        except CaptureError as error:
            print(f"[{error.check}] capture cleanup failed", file=sys.stderr)
            return 1
        except Exception:
            print("[capture-failed] capture cleanup failed", file=sys.stderr)
            return 1
        print("evidence cleanup complete")
        return 0
    if args.case is None or args.output_dir is None:
        parser.error("--case and --output-dir are required unless --selftest is used")
    if tuple(args.case) != CASES:
        parser.error("--case must name every fixed request profile once in canonical order")
    try:
        candidate_capture(args.case, args.output_dir)
    except CaptureError as error:
        print(f"[{error.check}] capture failed", file=sys.stderr)
        return 1
    except Exception:
        print("[capture-failed] capture failed", file=sys.stderr)
        return 1
    print("capture complete")
    return 0


if __name__ == "__main__":
    sys.exit(main())

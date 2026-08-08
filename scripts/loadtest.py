#!/usr/bin/env python3
"""Load test for nim-proxy against the enforcing mock. Stdlib only.

Drives N concurrent clients (default 100, mixed streaming/buffered, several
models and proxy API keys) through the proxy while scripts/mock_nim.py
--enforce plays a strict 40-rpm-per-key NIM. Exits non-zero if any client
saw a failure or the upstream recorded a single rate-limit violation.

Typical run (three terminals or backgrounded):
  python3 scripts/mock_nim.py --enforce --rpm 40 --port 9999
  PORT=8000 DATA_DIR=/tmp/loadtest-data cargo run --release
  # then claim it (config lives in the store, not env):
  curl -X POST localhost:8000/setup -H 'content-type: application/json' -d \
    '{"username":"op","password":"loadtest-pw-1","base_url":"http://127.0.0.1:9999",
      "nim_keys":[{"key":"k1"},{"key":"k2"},{"key":"k3"}]}'
  # open /v1 (Settings -> API access mode, or POST /api/settings/clients
  # {"mode":"open"} with the session cookie), then:
  python3 scripts/loadtest.py --proxy http://127.0.0.1:8000 \
      --mock http://127.0.0.1:9999 --clients 100 --requests 3

Add --worker-slots N to the mock to exercise the model-pressure governor
(slow the streams with --token-ms 150+ so concurrency actually builds).

The request mix includes plain, tool-offering, and JSON-mode calls with
sampling params, so the v0.4.0 request-shape / response-quality code paths are
exercised under concurrency (not just the rate limiter).
"""
import argparse
import errno
import json
import os
from pathlib import Path
import stat
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

MODELS = [
    "moonshotai/kimi-k2-instruct",
    "deepseek-ai/deepseek-r1",
    "meta/llama-3.3-70b-instruct",
]

results = []
results_lock = threading.Lock()


class MeasurementError(Exception):
    def __init__(self, check_id):
        super().__init__(check_id)
        self.check_id = check_id


def require_measurement_options(args):
    supplied = [args.history_path, args.proxy_pid, args.report_json]
    if any(value is not None for value in supplied) and not all(value is not None for value in supplied):
        raise MeasurementError("loadtest:measurement-options:incomplete")


def history_bytes(path, ending=False):
    path = Path(path)
    if not path.is_absolute():
        raise MeasurementError("loadtest:history-path:relative")
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError as error:
        if error.errno == errno.ELOOP:
            raise MeasurementError("loadtest:history-path:not-file") from None
        raise MeasurementError("loadtest:history-path:disappeared" if ending else "loadtest:history-path:missing") from None
    try:
        history = os.fstat(descriptor)
        if not stat.S_ISREG(history.st_mode):
            return _history_not_file(path)
        return history.st_size
    finally:
        os.close(descriptor)


def _history_not_file(_path):
    raise MeasurementError("loadtest:history-path:not-file")


def parse_rss(pid, proc_root=Path("/proc")):
    if not isinstance(pid, int) or pid <= 0:
        raise MeasurementError("loadtest:proxy-pid:invalid")
    try:
        lines = (Path(proc_root) / str(pid) / "status").read_text(encoding="utf-8").splitlines()
    except FileNotFoundError:
        raise MeasurementError("loadtest:proxy-pid:dead") from None
    except (OSError, UnicodeError):
        raise MeasurementError("loadtest:proxy-pid:proc-malformed") from None
    values = [line.split() for line in lines if line.startswith("VmRSS:")]
    if len(values) != 1 or len(values[0]) != 3 or values[0][2] != "kB":
        raise MeasurementError("loadtest:proxy-pid:proc-malformed")
    try:
        kb = int(values[0][1])
    except ValueError:
        raise MeasurementError("loadtest:proxy-pid:rss-invalid") from None
    if kb < 0:
        raise MeasurementError("loadtest:proxy-pid:rss-invalid")
    return kb * 1024


class RssSampler:
    def __init__(self, pid, proc_root=Path("/proc"), interval=0.05):
        self.pid, self.proc_root, self.interval = pid, proc_root, max(interval, 0.05)
        self.stop = threading.Event()
        self.error = None
        self.peak = None
        self.thread = threading.Thread(target=self._run)

    def sample(self):
        value = parse_rss(self.pid, self.proc_root)
        self.peak = value if self.peak is None else max(self.peak, value)

    def _run(self):
        while not self.stop.wait(self.interval):
            try:
                self.sample()
            except MeasurementError as error:
                self.error = error
                return
            except Exception as error:
                self.error = MeasurementError("loadtest:proxy-pid:proc-malformed")
                self.error.__cause__ = error
                return

    def start(self):
        self.sample()
        self.thread.start()

    def finish(self):
        self.stop.set()
        self.thread.join()
        self.sample()
        if self.error:
            raise self.error
        if self.peak is None:
            raise MeasurementError("loadtest:proxy-pid:rss-invalid")
        return self.peak


def publish_report(path, report, replace=os.replace):
    path = Path(path)
    if not path.is_absolute():
        raise MeasurementError("loadtest:report-json:relative")
    temporary = None
    try:
        with tempfile.NamedTemporaryFile("w", encoding="utf-8", dir=path.parent,
                                         prefix=f".{path.name}.tmp-", delete=False) as output:
            temporary = Path(output.name)
            json.dump(report, output, sort_keys=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
    except Exception as error:
        if temporary is not None:
            try:
                temporary.unlink()
            except OSError:
                pass
        if isinstance(error, OSError):
            raise MeasurementError("loadtest:report-json:unwritable") from None
        raise
    try:
        replace(temporary, path)
    except Exception as error:
        try:
            temporary.unlink()
        except OSError:
            pass
        if isinstance(error, OSError):
            raise MeasurementError("loadtest:report-json:non-atomic") from None
        raise
    try:
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except OSError:
        raise MeasurementError("loadtest:report-json:non-atomic") from None


def run_selftest():
    cases = 0
    def expect(check_id, call):
        nonlocal cases
        cases += 1
        try:
            call()
        except MeasurementError as error:
            if error.check_id != check_id:
                raise AssertionError(f"{check_id}: got {error.check_id}")
            return
        raise AssertionError(check_id)

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        history = root / "history-v1.jsonl"
        history.write_bytes(b"abc")
        assert history_bytes(history) == 3
        expect("loadtest:history-path:missing", lambda: history_bytes(root / "missing"))
        expect("loadtest:history-path:relative", lambda: history_bytes(Path("history-v1.jsonl")))
        expect("loadtest:history-path:not-file", lambda: history_bytes(root))
        history_link = root / "history-link"
        history_link.symlink_to(history)
        expect("loadtest:history-path:not-file", lambda: history_bytes(history_link))
        history_fifo = root / "history-fifo"
        os.mkfifo(history_fifo)
        expect("loadtest:history-path:not-file", lambda: history_bytes(history_fifo))
        history.unlink()
        expect("loadtest:history-path:disappeared", lambda: history_bytes(history, ending=True))
        history.write_bytes(b"replacement")
        history.unlink()
        history.mkdir()
        expect("loadtest:history-path:not-file", lambda: history_bytes(history, ending=True))
        history.rmdir()
        expect("loadtest:proxy-pid:invalid", lambda: parse_rss(0, root))
        expect("loadtest:proxy-pid:dead", lambda: parse_rss(7, root))
        proc = root / "7"; proc.mkdir(); (proc / "status").write_text("VmRSS: nope kB\n")
        expect("loadtest:proxy-pid:rss-invalid", lambda: parse_rss(7, root))
        (proc / "status").write_text("VmRSS: 1 B\n")
        expect("loadtest:proxy-pid:proc-malformed", lambda: parse_rss(7, root))
        (proc / "status").write_text("Name:\ttest\n")
        expect("loadtest:proxy-pid:proc-malformed", lambda: parse_rss(7, root))
        (proc / "status").write_text("VmRSS:\t12 kB\nVmRSS:\t20 kB\n")
        expect("loadtest:proxy-pid:proc-malformed", lambda: parse_rss(7, root))
        (proc / "status").write_bytes(b"VmRSS:\t\xff kB\n")
        expect("loadtest:proxy-pid:proc-malformed", lambda: parse_rss(7, root))
        (proc / "status").write_text("VmRSS:\t12 kB\n")
        assert parse_rss(7, root) == 12 * 1024
        sampler = RssSampler(7, root); sampler.start()
        (proc / "status").write_text("VmRSS:\t20 kB\n")
        deadline = time.monotonic() + 1
        while sampler.peak != 20 * 1024 and time.monotonic() < deadline:
            time.sleep(0.01)
        assert sampler.finish() == 20 * 1024
        assert not sampler.thread.is_alive()
        (proc / "status").write_text("VmRSS:\t12 kB\n")
        sampler = RssSampler(7, root)
        sampler.start()
        original_parse_rss = parse_rss
        sampled = threading.Event()
        def unexpected_sample(*_args):
            sampled.set()
            raise RuntimeError("selftest")
        globals()["parse_rss"] = unexpected_sample
        try:
            assert sampled.wait(1)
        finally:
            globals()["parse_rss"] = original_parse_rss
        expect("loadtest:proxy-pid:proc-malformed", sampler.finish)
        def sampler_lifecycle_control(phase_error):
            sampler = RssSampler(7, root)
            sampler.start()
            try:
                if phase_error:
                    raise MeasurementError("loadtest:selftest:measurement-phase")
            finally:
                sampler.finish()
                assert not sampler.thread.is_alive()
            return sampler.peak
        assert sampler_lifecycle_control(False) == 12 * 1024
        expect("loadtest:selftest:measurement-phase", lambda: sampler_lifecycle_control(True))
        expect("loadtest:report-json:relative", lambda: publish_report(Path("report.json"), {}))
        blocked = root / "blocked"; blocked.write_text("x")
        expect("loadtest:report-json:unwritable", lambda: publish_report(blocked / "report.json", {}))
        report = root / "report.json"; publish_report(report, {"a": 1}); assert report.read_text() == '{"a": 1}\n'
        expect("loadtest:report-json:non-atomic", lambda: publish_report(root / "bad.json", {}, lambda *_: (_ for _ in ()).throw(OSError())))
        assert not list(root.glob(".bad.json.tmp-*"))
        try:
            publish_report(root / "programmer-error.json", {}, lambda *_: (_ for _ in ()).throw(RuntimeError("selftest")))
        except RuntimeError as error:
            assert str(error) == "selftest"
        else:
            raise AssertionError("replace RuntimeError was relabeled")
        assert not list(root.glob(".programmer-error.json.tmp-*"))
        old_report = b"old report\n"
        report.write_bytes(old_report)
        original_fsync = os.fsync
        os.fsync = lambda _fd: (_ for _ in ()).throw(OSError())
        try:
            expect("loadtest:report-json:unwritable", lambda: publish_report(report, {"a": 2}))
        finally:
            os.fsync = original_fsync
        assert report.read_bytes() == old_report
        assert not list(root.glob(".report.json.tmp-*"))
        fsync_calls = 0
        def fail_directory_sync(fd):
            nonlocal fsync_calls
            fsync_calls += 1
            if fsync_calls == 2:
                raise OSError()
            return original_fsync(fd)
        os.fsync = fail_directory_sync
        try:
            expect("loadtest:report-json:non-atomic", lambda: publish_report(report, {"a": 3}))
        finally:
            os.fsync = original_fsync
        assert report.read_text() == '{"a": 3}\n'
    incomplete = argparse.Namespace(history_path="x", proxy_pid=None, report_json="x")
    expect("loadtest:measurement-options:incomplete", lambda: require_measurement_options(incomplete))
    cases += 1
    incomplete_cli = subprocess.run(
        [sys.executable, __file__, "--selftest", "--history-path", str(root / "history-v1.jsonl")],
        capture_output=True, text=True)
    assert incomplete_cli.returncode != 0
    assert incomplete_cli.stdout == ""
    assert incomplete_cli.stderr == "loadtest:measurement-options:incomplete\n"
    print(f"loadtest selftest: {cases} cases passed")


def one_request(args, client_id, seq):
    model = MODELS[(client_id + seq) % len(MODELS)]
    stream = (client_id + seq) % 2 == 0
    payload = {
        "model": model,
        "stream": stream,
        "temperature": 0.2 + 0.1 * ((client_id + seq) % 8),
        "max_tokens": 1024,
        "messages": [
            {"role": "system", "content": "you are a load test"},
            # Stable per-client conversation identity exercises affinity
            # (messages are unchanged by the shape variety below).
            {"role": "user", "content": f"conversation for client {client_id}"},
        ],
    }
    # Exercise the v0.4.0 shape/quality paths under concurrency: every 3rd
    # request offers tools, every 5th asks for JSON output.
    if (client_id + seq) % 3 == 0:
        payload["tools"] = [{"type": "function",
                             "function": {"name": "search", "parameters": {}}}]
        payload["tool_choice"] = "auto"
    if (client_id + seq) % 5 == 0:
        payload["response_format"] = {"type": "json_object"}
    body = json.dumps(payload).encode()
    req = urllib.request.Request(
        f"{args.proxy}/v1/chat/completions", data=body,
        headers={"Content-Type": "application/json",
                 "Authorization": f"Bearer {args.tokens[client_id % len(args.tokens)]}"})
    start = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=args.timeout) as resp:
            payload = resp.read().decode(errors="replace")
            ok = resp.status == 200 and "proxy_error" not in payload
            if stream:
                ok = ok and "data: [DONE]" in payload
            return {"ok": ok, "status": resp.status,
                    "secs": time.monotonic() - start, "stream": stream}
    except Exception as e:
        return {"ok": False, "status": str(e)[:80],
                "secs": time.monotonic() - start, "stream": stream}


def client_loop(args, client_id):
    for seq in range(args.requests):
        r = one_request(args, client_id, seq)
        with results_lock:
            results.append(r)


def fetch_json(url):
    with urllib.request.urlopen(url, timeout=10) as resp:
        return json.loads(resp.read())


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--proxy", default="http://127.0.0.1:8000")
    p.add_argument("--mock", default="http://127.0.0.1:9999",
                   help="mock_nim.py base URL for the violation report")
    p.add_argument("--clients", type=int, default=100)
    p.add_argument("--requests", type=int, default=3,
                   help="requests per client (sequential, agent-style)")
    p.add_argument("--timeout", type=float, default=900)
    p.add_argument("--proxy-keys", default="",
                   help="comma-separated proxy bearer tokens (empty = local mode)")
    p.add_argument("--history-path")
    p.add_argument("--proxy-pid", type=int)
    p.add_argument("--report-json")
    p.add_argument("--selftest", action="store_true")
    args = p.parse_args()
    try:
        require_measurement_options(args)
    except MeasurementError as error:
        raise SystemExit(error.check_id)
    if args.selftest:
        run_selftest()
        return
    try:
        history_start = history_bytes(args.history_path) if args.history_path else None
        sampler = RssSampler(args.proxy_pid) if args.proxy_pid else None
    except MeasurementError as error:
        raise SystemExit(error.check_id)
    args.tokens = args.proxy_keys.split(",") if args.proxy_keys else [""]

    before = fetch_json(f"{args.mock}/control/stats")
    print(f"driving {args.clients} clients x {args.requests} requests "
          f"({args.clients * args.requests} total) ...", flush=True)
    started = time.monotonic()
    with results_lock:
        results.clear()
    sampler_started = False
    sampler_finished = False
    try:
        if sampler:
            sampler.start()
            sampler_started = True
        threads = [threading.Thread(target=client_loop, args=(args, i), daemon=True)
                   for i in range(args.clients)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        wall = time.monotonic() - started
        peak_rss = sampler.finish() if sampler else None
        sampler_finished = True
        history_end = history_bytes(args.history_path, ending=True) if args.history_path else None
    except MeasurementError as error:
        raise SystemExit(error.check_id)
    finally:
        if sampler_started and not sampler_finished:
            try:
                sampler.finish()
            except MeasurementError:
                pass

    after = fetch_json(f"{args.mock}/control/stats")
    violations = after["violations"] - before["violations"]
    upstream_hits = after["chat_requests"] - before["chat_requests"]
    exhausted = after.get("worker_exhausted", 0) - before.get("worker_exhausted", 0)
    per_key = {k: after["per_key"].get(k, 0) - before["per_key"].get(k, 0)
               for k in after["per_key"]}

    failed = [r for r in results if not r["ok"]]
    lat = sorted(r["secs"] for r in results)
    p50 = statistics.median(lat)
    p95 = lat[int(len(lat) * 0.95) - 1]

    if args.report_json:
        try:
            publish_report(args.report_json, {"client_failures": len(failed), "clients": args.clients,
                "completed_requests": len(results), "duration_seconds": wall,
                "history_end_bytes": history_end, "history_start_bytes": history_start,
                "peak_rss_bytes": peak_rss, "rate_violations": violations,
                "requests_per_client": args.requests, "upstream_requests": upstream_hits,
                "worker_exhaustions": exhausted})
        except MeasurementError as error:
            raise SystemExit(error.check_id)

    print(f"""
=== nim-proxy load test report ===
clients x requests   {args.clients} x {args.requests} = {len(results)} completed
wall clock           {wall:.1f}s  ({len(results) / wall * 60:.0f} req/min through proxy)
client failures      {len(failed)}
latency p50 / p95    {p50:.2f}s / {p95:.2f}s
latency max          {lat[-1]:.2f}s
upstream requests    {upstream_hits}
upstream by key      {json.dumps(per_key)}
worker exhaustions   {exhausted}  (per-model cap hits; governor should bound these)
peak workers/model   {json.dumps(after.get("max_workers", {}))}
RATE VIOLATIONS      {violations}  (upstream requests beyond the per-key rpm window)
""", flush=True)

    if failed:
        print(f"FAIL: {len(failed)} client-visible failures, e.g. {failed[:3]}")
        raise SystemExit(1)
    if violations:
        print(f"FAIL: proxy exceeded the upstream speed limit {violations} time(s)")
        raise SystemExit(1)
    if len(per_key) > 1 and min(per_key.values()) == 0:
        print("FAIL: a configured key was never used")
        raise SystemExit(1)
    print("PASS: zero failures, zero upstream rate violations")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Load test for nim-proxy against the enforcing mock. Stdlib only.

Drives N concurrent clients (default 100, mixed streaming/buffered, several
models and proxy API keys) through the proxy while scripts/mock_nim.py
--enforce plays a strict 40-rpm-per-key NIM. Exits non-zero if any client
saw a failure or the upstream recorded a single rate-limit violation.

Typical run (three terminals or backgrounded):
  python3 scripts/mock_nim.py --enforce --rpm 40 --port 9999
  NIM_API_KEYS=k1,k2,k3 NIM_BASE_URL=http://127.0.0.1:9999 PORT=8000 \
      INSECURE_NO_AUTH=true cargo run --release
  python3 scripts/loadtest.py --proxy http://127.0.0.1:8000 \
      --mock http://127.0.0.1:9999 --clients 100 --requests 3

The request mix includes plain, tool-offering, and JSON-mode calls with
sampling params, so the v0.4.0 request-shape / response-quality code paths are
exercised under concurrency (not just the rate limiter).
"""
import argparse
import json
import statistics
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
    args = p.parse_args()
    args.tokens = args.proxy_keys.split(",") if args.proxy_keys else [""]

    before = fetch_json(f"{args.mock}/control/stats")
    print(f"driving {args.clients} clients x {args.requests} requests "
          f"({args.clients * args.requests} total) ...", flush=True)
    started = time.monotonic()
    threads = [threading.Thread(target=client_loop, args=(args, i), daemon=True)
               for i in range(args.clients)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    wall = time.monotonic() - started

    after = fetch_json(f"{args.mock}/control/stats")
    violations = after["violations"] - before["violations"]
    upstream_hits = after["chat_requests"] - before["chat_requests"]
    per_key = {k: after["per_key"].get(k, 0) - before["per_key"].get(k, 0)
               for k in after["per_key"]}

    failed = [r for r in results if not r["ok"]]
    lat = sorted(r["secs"] for r in results)
    p50 = statistics.median(lat)
    p95 = lat[int(len(lat) * 0.95) - 1]

    print(f"""
=== nim-proxy load test report ===
clients x requests   {args.clients} x {args.requests} = {len(results)} completed
wall clock           {wall:.1f}s  ({len(results) / wall * 60:.0f} req/min through proxy)
client failures      {len(failed)}
latency p50 / p95    {p50:.2f}s / {p95:.2f}s
latency max          {lat[-1]:.2f}s
upstream requests    {upstream_hits}
upstream by key      {json.dumps(per_key)}
RATE VIOLATIONS      {violations}  (upstream requests beyond the per-key rpm window)
""", flush=True)

    if failed:
        print(f"FAIL: {len(failed)} client-visible failures, e.g. {failed[:3]}")
        raise SystemExit(1)
    if violations:
        print(f"FAIL: proxy exceeded the upstream speed limit {violations} time(s)")
        raise SystemExit(1)
    if len(per_key) > 1 and min(per_key.values()) == 0:
        print("FAIL: a configured key lane was never used")
        raise SystemExit(1)
    print("PASS: zero failures, zero upstream rate violations")


if __name__ == "__main__":
    main()

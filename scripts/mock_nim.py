#!/usr/bin/env python3
"""Mock NIM upstream for development and load testing. Stdlib only.

OpenAI-compatible surface: GET /v1/models, POST /v1/chat/completions
(streaming SSE with a usage chunk, or buffered JSON).

With --enforce it applies NIM's real semantics — a true sliding window of
--rpm requests per rolling 60s per API key, answering violations with
429 + Retry-After — and records every violation so a load test can assert
the proxy NEVER exceeded the speed limit. Stats at GET /control/stats.

Usage:
  python3 scripts/mock_nim.py --port 9999 --enforce --rpm 40 --delay-ms 50
"""
import argparse
import json
import threading
import time
from collections import defaultdict, deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ARGS = None
LOCK = threading.Lock()
WINDOWS = defaultdict(deque)  # key -> deque[timestamps]
STATS = {
    "chat_requests": 0,
    "models_requests": 0,
    "violations": 0,  # requests that arrived while the key's window was full
    "served_429": 0,
    "per_key": defaultdict(int),
}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _key(self):
        return self.headers.get("Authorization", "").replace("Bearer ", "")

    def _json(self, code, obj, extra=None):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/v1/models":
            with LOCK:
                STATS["models_requests"] += 1
            self._json(200, {"object": "list", "data": [
                {"id": m, "object": "model", "created": 0, "owned_by": m.split("/")[0]}
                for m in ARGS.models.split(",")]})
        elif self.path == "/control/stats":
            with LOCK:
                self._json(200, {**STATS, "per_key": dict(STATS["per_key"])})
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self):
        if self.path != "/v1/chat/completions":
            self._json(404, {"error": "not found"})
            return
        n = int(self.headers.get("Content-Length", 0))
        req = json.loads(self.rfile.read(n) or b"{}")
        key = self._key()
        now = time.monotonic()

        with LOCK:
            STATS["chat_requests"] += 1
            STATS["per_key"][key] += 1
            if ARGS.enforce:
                win = WINDOWS[key]
                while win and now - win[0] >= 60:
                    win.popleft()
                if len(win) >= ARGS.rpm:
                    # The proxy's job is to never let this happen.
                    STATS["violations"] += 1
                    STATS["served_429"] += 1
                    retry = max(1, int(60 - (now - win[0])) + 1)
                    self._json(429, {"error": "rate limited"},
                               {"Retry-After": str(retry)})
                    return
                win.append(now)

        if ARGS.delay_ms:
            time.sleep(ARGS.delay_ms / 1000)

        if req.get("stream"):
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            for tok in ["Hello", " from", " mock", " NIM"]:
                chunk = {"id": "c1", "object": "chat.completion.chunk", "choices": [
                    {"index": 0, "delta": {"content": tok}, "finish_reason": None}]}
                self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
                self.wfile.flush()
                time.sleep(ARGS.token_ms / 1000)
            final = {"id": "c1", "object": "chat.completion.chunk", "choices": [],
                     "usage": {"prompt_tokens": 120, "completion_tokens": 4}}
            self.wfile.write(f"data: {json.dumps(final)}\n\n".encode())
            self.wfile.write(b"data: [DONE]\n\n")
        else:
            self._json(200, {
                "id": "c1", "object": "chat.completion",
                "choices": [{"index": 0, "message": {"role": "assistant",
                             "content": "Hello from mock NIM"},
                             "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 120, "completion_tokens": 4}})


def main():
    global ARGS
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--port", type=int, default=9999)
    p.add_argument("--enforce", action="store_true",
                   help="enforce a per-key sliding-window rate limit")
    p.add_argument("--rpm", type=int, default=40)
    p.add_argument("--delay-ms", type=int, default=0,
                   help="pre-response latency to simulate inference queueing")
    p.add_argument("--token-ms", type=int, default=20,
                   help="delay between streamed tokens")
    p.add_argument("--models", default="moonshotai/kimi-k2-instruct,deepseek-ai/deepseek-r1,meta/llama-3.3-70b-instruct")
    ARGS = p.parse_args()
    srv = ThreadingHTTPServer(("127.0.0.1", ARGS.port), Handler)
    print(f"mock NIM on :{ARGS.port} enforce={ARGS.enforce} rpm={ARGS.rpm}", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()

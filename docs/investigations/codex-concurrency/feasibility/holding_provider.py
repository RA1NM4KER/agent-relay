"""Loopback-only Responses fixture; no inference, credentials, or tool execution.

Pinned Codex supplies the native identity in `thread-id`. Only that header and
bounded lifecycle metadata are retained; request bodies and other headers are not.
Open response streams make cancellation/concurrency observable independently of
terminal text. A disconnected HTTP stream is not an executor-quiescence receipt.
"""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import select
import socket
import threading
import time


class HoldingProvider:
    def __init__(self, evidence, lifetime=90):
        self.evidence = Path(evidence)
        self.file = self.evidence.open("x")
        self.evidence.chmod(0o600)
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.rows = []
        self.handlers = 0
        self.refusals = 0
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def setup(self):
                super().setup()
                self.connection.settimeout(2)
                with fixture.lock:
                    fixture.handlers += 1

            def finish(self):
                try:
                    super().finish()
                finally:
                    with fixture.lock:
                        fixture.handlers -= 1

            def log_message(self, *args):
                pass

            def do_GET(self):
                # The custom provider uses HTTP Responses. Refusing an upgrade
                # lets native transport selection perform its ordinary fallback.
                status = 426 if self.headers.get("Upgrade") else 404
                fixture.refuse("GET", status, self.path)
                self.send_response(status)
                self.send_header("Content-Length", "0")
                self.end_headers()
                self.close_connection = True

            def do_POST(self):
                thread = self.headers.get("thread-id", "")
                try:
                    length = int(self.headers.get("Content-Length", "0"))
                except ValueError:
                    length = -1
                if (self.path != "/v1/responses" or not 0 <= length <= 8 * 1024 * 1024
                        or not thread or len(thread) > 128
                        or any(c not in "0123456789abcdef-" for c in thread)):
                    fixture.refuse("POST", 400, self.path)
                    self.send_error(400)
                    return
                # Consume without decoding, persisting, or inspecting content.
                if len(self.rfile.read(length)) != length:
                    return
                with fixture.lock:
                    if len(fixture.rows) >= 512:
                        self.send_error(503)
                        return
                    row = dict(request=len(fixture.rows), thread=thread, state="open",
                               opened=time.monotonic(), heartbeats=0)
                    fixture.rows.append(row)
                    fixture.record("open", row)
                reason = "server_shutdown"
                try:
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Transfer-Encoding", "chunked")
                    self.send_header("Cache-Control", "no-cache")
                    self.end_headers()
                    created = {"type": "response.created", "response": {"id": f"lab-{row['request']}"}}
                    payload = ("event: response.created\ndata: " + json.dumps(created) + "\n\n").encode()
                    while not fixture.stop.is_set():
                        if time.monotonic() - row["opened"] > lifetime:
                            reason = "expired"
                            break
                        self.wfile.write(f"{len(payload):x}\r\n".encode() + payload + b"\r\n")
                        self.wfile.flush()
                        with fixture.lock:
                            row["heartbeats"] += 1
                        if select.select([self.connection], [], [], .1)[0]:
                            if not self.connection.recv(1, socket.MSG_PEEK):
                                reason = "peer_disconnect"
                                break
                            reason = "unexpected_peer_data"
                            break
                        payload = b": lab keepalive\n\n"
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    reason = "peer_disconnect"
                finally:
                    self.close_connection = True
                    with fixture.lock:
                        row.update(state="closed", closed=time.monotonic(), reason=reason)
                        fixture.record("close", row)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.server.server_port}/v1"

    def record(self, kind, row):
        self.file.write(json.dumps({"kind": kind, **row}) + "\n")
        self.file.flush()

    def refuse(self, method, status, path):
        with self.lock:
            self.refusals += 1
            if self.refusals <= 512:
                self.record("http_refused", {"method": method, "status": status,
                    "route": "responses" if path == "/v1/responses" else "other",
                    "time": time.monotonic()})
            else:
                self.stop.set()

    def snapshot(self):
        with self.lock:
            return [dict(row) for row in self.rows]

    def active(self, threads):
        rows = [r for r in self.snapshot() if r["state"] == "open" and r["thread"] in threads]
        if len({r["thread"] for r in rows}) != len(rows):
            raise AssertionError("multiple open model streams for one foreground root")
        return {r["thread"]: r for r in rows}

    def disconnected(self, threads):
        rows = self.snapshot()
        return bool(threads) and all(
            any(r["thread"] == thread for r in rows) and all(
                r["state"] == "closed" and r.get("reason") == "peer_disconnect"
                for r in rows if r["thread"] == thread) for thread in threads)

    def close(self):
        self.stop.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        # Request threads finish after noticing stop or a closed peer. Preserve
        # their final records before closing the evidence stream.
        deadline = time.monotonic() + 5
        while self.handlers and time.monotonic() < deadline:
            time.sleep(.02)
        if self.handlers or any(r["state"] == "open" for r in self.snapshot()):
            raise RuntimeError("fixture request cleanup incomplete")
        self.file.close()

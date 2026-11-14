"""A small localhost-only HTTP server shared by `review` and `try`."""

from __future__ import annotations

import json
import math
import re
import sys
import threading
import traceback
from collections.abc import Callable, Collection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from narrate import NarrateError

MAX_BODY = 64 * 1024
Response = tuple[int, str, bytes]
Handler = Callable[[object], Response]
_CLIP_RE = re.compile(r"/clips/([a-z0-9-]+)\.wav")


def _finite(value):
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, dict):
        return {k: _finite(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_finite(v) for v in value]
    return value


def json_response(obj, status: int = 200) -> Response:
    """Strict JSON (non-finite numbers become null)."""
    body = json.dumps(_finite(obj), ensure_ascii=False, allow_nan=False).encode("utf-8")
    return status, "application/json; charset=utf-8", body


def serve_clip(out_dir: Path, ids: Collection[str], request_path: str) -> Response:
    """The WAV for exactly `/clips/<id>.wav` with a known id; anything else is a 404."""
    m = _CLIP_RE.fullmatch(request_path)
    if m is None or m.group(1) not in ids:
        return json_response({"error": "not found"}, 404)
    path = Path(out_dir) / "clips" / f"{m.group(1)}.wav"
    try:
        return 200, "audio/wav", path.read_bytes()
    except OSError:
        return json_response({"error": "not found"}, 404)


class Routes:
    """Maps `(method, path)` to handlers; a handler gets the parsed JSON body (POST) or None."""

    def __init__(self) -> None:
        self._exact: dict[tuple[str, str], Handler] = {}
        self._prefix: list[tuple[str, str, Callable[[str], Response]]] = []

    def add(self, method: str, path: str, handler: Handler) -> None:
        """Route one exact path."""
        self._exact[(method, path)] = handler

    def add_prefix(self, method: str, prefix: str, handler: Callable[[str], Response]) -> None:
        """Route every path starting with `prefix`; the handler gets the full path."""
        self._prefix.append((method, prefix, handler))

    def find(self, method: str, path: str) -> Handler | None:
        """The handler for a request, or None."""
        handler = self._exact.get((method, path))
        if handler is not None:
            return handler
        for m, prefix, h in self._prefix:
            if m == method and path.startswith(prefix):
                return lambda _body, h=h: h(path)
        return None


class _RequestHandler(BaseHTTPRequestHandler):
    server: LocalServer

    def log_message(self, format, *args) -> None:  # noqa: A002 - silence request logging
        pass

    def _send(self, response: Response) -> None:
        status, ctype, body = response
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _read_body(self):
        """The parsed JSON body, or a `Response` when it is refused."""
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            length = -1
        if length < 0:
            return json_response({"error": "bad Content-Length"}, 400)
        if length > MAX_BODY:
            self.close_connection = True
            return json_response({"error": "body too large"}, 413)
        raw = self.rfile.read(length)
        try:
            return json.loads(raw.decode("utf-8")) if raw else {}
        except (ValueError, RecursionError):
            return json_response({"error": "invalid JSON"}, 400)

    def _host_ok(self) -> bool:
        """Only our own origin may talk to us (a DNS-rebinding guard)."""
        port = self.server.server_address[1]
        return self.headers.get("Host") in (f"127.0.0.1:{port}", f"localhost:{port}")

    def _dispatch(self, method: str) -> None:
        if not self._host_ok():
            self.close_connection = True
            self._send(json_response({"error": "misdirected request"}, 421))
            return
        path = self.path.split("?", 1)[0]
        handler = self.server.routes.find(method, path)
        if handler is None:
            self._send(json_response({"error": "not found"}, 404))
            return
        body = None
        if method == "POST":
            ctype = (self.headers.get("Content-Type") or "").split(";", 1)[0].strip().lower()
            if ctype != "application/json":
                self.close_connection = True
                self._send(json_response({"error": "Content-Type must be application/json"}, 415))
                return
            body = self._read_body()
            if isinstance(body, tuple):
                self._send(body)
                return
        try:
            response = handler(body)
        except NarrateError as e:
            response = json_response({"error": str(e)}, 400)
        except Exception:
            traceback.print_exc(file=sys.stderr)
            response = json_response({"error": "internal error"}, 500)
        self._send(response)

    def do_GET(self) -> None:  # noqa: N802
        """Handle a GET."""
        self._dispatch("GET")

    def do_POST(self) -> None:  # noqa: N802
        """Handle a POST."""
        self._dispatch("POST")


class LocalServer(ThreadingHTTPServer):
    """Binds 127.0.0.1 only; port 0 picks a free port."""

    daemon_threads = True

    def __init__(self, routes: Routes, port: int = 0) -> None:
        super().__init__(("127.0.0.1", port), _RequestHandler)
        self.routes = routes

    @property
    def url(self) -> str:
        """`http://127.0.0.1:<port>`."""
        return f"http://127.0.0.1:{self.server_address[1]}"


def start(server: LocalServer) -> threading.Thread:
    """Serve in a daemon thread; stop with `server.shutdown()` and `server.server_close()`."""
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return thread

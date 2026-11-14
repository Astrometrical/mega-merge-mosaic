"""The review API: state, approvals, rejections, notes and rebuilds (spec §5.5)."""

from __future__ import annotations

import threading
from collections.abc import Callable
from importlib import resources
from pathlib import Path

from narrate import NarrateError
from narrate._fs import is_sha256, sha256_file
from narrate.approved import ApprovedStore, approved_root
from narrate.backends import Backend
from narrate.build import BuildOptions, build, default_out
from narrate.cache import TakeStore, cache_root
from narrate.checkers import Checker
from narrate.lock import Lock, lock_path
from narrate.manifest import Manifest, ManifestLine, read_manifest
from narrate.script import load_script
from narrate.server import LocalServer, Response, Routes, json_response, serve_clip, start

BUILDING = "building — try again when the build finishes"
TAKE_CHANGED = "the take changed — reload"


def _page() -> bytes:
    return (resources.files("narrate") / "pages" / "review.html").read_bytes()


class _Conflict(Exception):
    """A mutation the state on disk no longer allows (HTTP 409)."""


class ReviewApp:
    """State on disk: the manifest is re-read after each build and, with the lock, before each mutation."""

    def __init__(
        self,
        script_path: Path,
        options: BuildOptions,
        backend: Backend | None = None,
        checker: Checker | None = None,
    ) -> None:
        self.script_path = Path(script_path)
        self.options = options
        self.backend = backend
        self.checker = checker
        self.out = options.out if options.out is not None else default_out(load_script(self.script_path))
        self.lock_file = lock_path(self.script_path)
        self._mutate = threading.Lock()
        self._build = threading.Lock()
        self.manifest: Manifest = self._read_manifest()

    def _read_manifest(self) -> Manifest:
        path = self.out / "narration.json"
        if not path.is_file():
            raise NarrateError(f"{path}: no manifest; build the script first")
        return read_manifest(path)

    def routes(self) -> Routes:
        """The review routes."""
        r = Routes()
        r.add("GET", "/", lambda _b: (200, "text/html; charset=utf-8", _page()))
        r.add("GET", "/api/state", lambda _b: self._state())
        r.add_prefix("GET", "/clips/", lambda p: serve_clip(self.out, [ln.id for ln in self.manifest.lines], p))
        r.add("POST", "/api/approve", lambda b: self._edit(lambda lock: self._approve(lock, b)))
        r.add("POST", "/api/reject", lambda b: self._edit(lambda lock: self._reject(lock, b)))
        r.add("POST", "/api/note", lambda b: self._edit(lambda lock: self._note(lock, b)))
        r.add("POST", "/api/approve-unflagged", lambda _b: self._edit(self._approve_unflagged))
        r.add("POST", "/api/rebuild", lambda _b: self._rebuild())
        return r

    def _state(self) -> Response:
        with self._mutate:
            return self._state_locked(Lock.load(self.lock_file))

    def _state_locked(self, lock: Lock) -> Response:
        return json_response(
            {
                "manifest": self.manifest.to_json(),
                "lock": {
                    "pins": {
                        i: {"take": p.take, "key": p.key, "approved_at": p.approved_at}
                        for i, p in sorted(lock.pins.items())
                    },
                    "notes": dict(sorted(lock.notes.items())),
                    "rejected": {k: sorted(v) for k, v in sorted(lock.rejected.items())},
                },
                "building": self._build.locked(),
            }
        )

    def _line(self, body) -> ManifestLine:
        id_ = body.get("id") if isinstance(body, dict) else None
        for ln in self.manifest.lines:
            if ln.id == id_:
                return ln
        raise NarrateError(f"unknown line id {id_!r}")

    def _edit(self, action: Callable[[Lock], None]) -> Response:
        """Run a mutation against the manifest and lock as they are on disk now.

        Refused (409) while a rebuild runs: the clips and takes are changing under the page.
        A rebuild only starts while holding `_mutate`, so none starts mid-mutation either.
        """
        with self._mutate:
            if self._build.locked():
                return json_response({"error": BUILDING}, 409)
            self.manifest = self._read_manifest()
            lock = Lock.load(self.lock_file)
            try:
                action(lock)
            except _Conflict as e:
                return json_response({"error": str(e)}, 409)
            lock.save()
            return self._state_locked(lock)

    def _checked(self, line: ManifestLine) -> dict:
        if self.manifest.draft:
            raise NarrateError("the manifest is a draft; build the script first")
        if line.check is None:
            raise NarrateError(f"line {line.id!r} has no checked take")
        if not is_sha256(line.check.get("sha256")):
            raise NarrateError(f"line {line.id!r}: the manifest has no take hash; rebuild the script")
        return line.check

    def _heard(self, body) -> tuple[ManifestLine, dict]:
        """The line a request names and its check, if its take is still the one the page showed.

        The body's `sha256` is the page's `check.sha256`; the take now in the manifest and
        in the take cache must still hash to it, else the request is a 409.
        """
        line = self._line(body)
        c = self._checked(line)
        sha = body.get("sha256")
        if not is_sha256(sha):
            raise NarrateError("'sha256' must be the sha256 of the take shown (64 lowercase hex characters)")
        return line, self._current(line, c, sha)

    def _current(self, line: ManifestLine, c: dict, sha: str) -> dict:
        cached = TakeStore(cache_root(self.options.cache)).path(c["key"], c["take"])
        if c["sha256"] != sha or (cached.is_file() and sha256_file(cached) != sha):
            raise _Conflict(TAKE_CHANGED)
        return c

    def _pin(self, lock: Lock, line: ManifestLine, c: dict) -> None:
        """Pin the line's take, copying its audio into the approved store if it is not there yet."""
        sha = c["sha256"]
        cached = TakeStore(cache_root(self.options.cache)).path(c["key"], c["take"])
        store = ApprovedStore(approved_root(self.options.approved))
        if cached.is_file():
            store.put(sha, cached)
        elif not store.has(sha):
            raise NarrateError(
                f"line {line.id!r}: take {c['take']} is in neither the take cache nor the approved store"
            )
        lock.pin(line.id, c["key"], c["take"], line.duration, c["heard"], c["cer"], c["pace"], sha)

    def _approve(self, lock: Lock, body) -> None:
        line, c = self._heard(body)
        self._pin(lock, line, c)

    def _reject(self, lock: Lock, body) -> None:
        _, c = self._heard(body)
        lock.reject(c["key"], c["take"])

    def _note(self, lock: Lock, body) -> None:
        line = self._line(body)
        text = body.get("note")
        if not isinstance(text, str):
            raise NarrateError("'note' must be a string")
        lock.note(line.id, text)

    def _approve_unflagged(self, lock: Lock) -> None:
        """Pin every `new` line with no flags whose take is not rejected: the take now on disk."""
        for line in self.manifest.lines:
            c = line.check
            if line.status != "new" or line.flags or c is None or not is_sha256(c.get("sha256")):
                continue
            if c["take"] in lock.rejected.get(c["key"], set()):
                continue
            self._pin(lock, line, self._current(line, c, c["sha256"]))

    def _rebuild(self) -> Response:
        with self._mutate:
            if not self._build.acquire(blocking=False):
                return json_response({"error": "a rebuild is already running"}, 409)
        try:
            build(self.script_path, backend=self.backend, checker=self.checker, options=self.options)
            self.manifest = self._read_manifest()
        finally:
            self._build.release()
        return self._state()


def serve(
    app: ReviewApp,
    port: int,
    opener: Callable[[str], object] | None,
    ready: Callable[[LocalServer], None] | None = None,
) -> None:
    """Serve the review page until interrupted (or until the server is shut down from elsewhere)."""
    server = LocalServer(app.routes(), port)
    thread = start(server)
    try:
        print(f"Reviewing {app.script_path} at {server.url} (Ctrl+C to stop)", flush=True)
        if ready is not None:
            ready(server)
        if opener is not None:
            opener(server.url)
        while thread.is_alive():
            thread.join(0.2)
    except KeyboardInterrupt:
        pass
    finally:
        server.shutdown()
        server.server_close()

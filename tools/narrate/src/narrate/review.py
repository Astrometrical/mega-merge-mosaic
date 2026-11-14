"""The review API: state, approvals, rejections, notes and rebuilds (spec 5.4)."""

from __future__ import annotations

import hashlib
import threading
from pathlib import Path

from narrate import NarrateError
from narrate.approved import ApprovedStore, approved_root
from narrate.backends import Backend
from narrate.build import BuildOptions, build, default_out
from narrate.cache import TakeStore, cache_root
from narrate.checkers import Checker
from narrate.lock import Lock, lock_path
from narrate.manifest import Manifest, ManifestLine, read_manifest
from narrate.script import load_script
from narrate.server import Response, Routes, json_response, serve_clip

PLACEHOLDER_PAGE = "<!doctype html><title>narrate review</title><p>review page not built yet</p>"


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class ReviewApp:
    """State on disk: the manifest is re-read after each build, the lock before each mutation."""

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
        r.add("GET", "/", lambda _b: (200, "text/html; charset=utf-8", PLACEHOLDER_PAGE.encode("utf-8")))
        r.add("GET", "/api/state", lambda _b: self._state())
        r.add_prefix("GET", "/clips/", lambda p: serve_clip(self.out, [ln.id for ln in self.manifest.lines], p))
        r.add("POST", "/api/approve", lambda b: self._edit(lambda lock: self._approve(lock, self._line(b))))
        r.add("POST", "/api/reject", lambda b: self._edit(lambda lock: self._reject(lock, self._line(b))))
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

    def _edit(self, action) -> Response:
        with self._mutate:
            lock = Lock.load(self.lock_file)
            action(lock)
            lock.save()
            return self._state_locked(lock)

    def _checked(self, line: ManifestLine) -> dict:
        if self.manifest.draft:
            raise NarrateError("the manifest is a draft; build the script first")
        if line.check is None:
            raise NarrateError(f"line {line.id!r} has no checked take")
        return line.check

    def _approve(self, lock: Lock, line: ManifestLine) -> None:
        c = self._checked(line)
        key, take = c["key"], c["take"]
        store = ApprovedStore(approved_root(self.options.approved))
        cached = TakeStore(cache_root(self.options.cache)).path(key, take)
        if cached.is_file():
            sha = _sha256(cached)
            if not (store.has(key, take) and _sha256(store.path(key, take)) == sha):
                store.put(key, take, cached)
        elif store.has(key, take):
            sha = _sha256(store.path(key, take))
        else:
            raise NarrateError(f"line {line.id!r}: take {take} is in neither the take cache nor the approved store")
        lock.pin(line.id, key, take, line.duration, c["heard"], c["cer"], c["pace"], sha)

    def _reject(self, lock: Lock, line: ManifestLine) -> None:
        c = self._checked(line)
        lock.reject(c["key"], c["take"])

    def _note(self, lock: Lock, body) -> None:
        line = self._line(body)
        text = body.get("note")
        if not isinstance(text, str):
            raise NarrateError("'note' must be a string")
        lock.note(line.id, text)

    def _approve_unflagged(self, lock: Lock) -> None:
        for line in self.manifest.lines:
            c = line.check
            if line.status != "new" or c is None or c["take"] in lock.rejected.get(c["key"], set()):
                continue
            self._approve(lock, line)

    def _rebuild(self) -> Response:
        if not self._build.acquire(blocking=False):
            return json_response({"error": "a rebuild is already running"}, 409)
        try:
            build(self.script_path, backend=self.backend, checker=self.checker, options=self.options)
            self.manifest = self._read_manifest()
        finally:
            self._build.release()
        return self._state()

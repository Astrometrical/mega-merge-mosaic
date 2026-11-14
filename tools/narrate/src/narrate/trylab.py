"""The pronunciation lab: hear a word in several spellings, pick by ear, write the list entry."""

from __future__ import annotations

import datetime
import hashlib
import json
import random
import re
import string
import threading
from collections.abc import Callable
from importlib import resources
from pathlib import Path

from narrate import NarrateError
from narrate._fs import read_text_retry, replace_with_retry
from narrate.build import default_out
from narrate.manifest import Manifest, read_manifest
from narrate.profile import Takes
from narrate.script import load_script
from narrate.server import LocalServer, Response, Routes, json_response, serve_clip, start

PLACEHOLDER = "{}"
# The words before the term differ ("is", "of", "to"): spec 5.6 wants carriers that do not repeat them.
DEFAULT_CARRIERS = (
    "Next up is {}.",
    "Everyone here has heard of {}.",
    "That brings us back to {}, again.",
)
#: Spec 5.6: each spelling in each carrier, with two takes each.
TRY_TAKES = Takes(count=2, max=2)
LIST_ABOUT = "Pronunciation respellings written by narrate try."
_LINE_ID = re.compile(r"c(\d+)-s(\d+)")
_DEL = re.compile(r"\x7f")


def _page() -> bytes:
    return (resources.files("narrate") / "pages" / "try.html").read_bytes()


def candidates_for(word: str, spellings: list[str]) -> list[str]:
    """`word` as written first, then each spelling, deduplicated in order."""
    if not word.strip():
        raise NarrateError("the word must not be empty")
    out = [word]
    for s in spellings:
        if not s.strip():
            raise NarrateError("a --spell value must not be empty")
        if s not in out:
            out.append(s)
    return out


def check_carriers(carriers: list[str]) -> list[str]:
    """The carriers given, else the defaults; each must contain `{}`."""
    chosen = list(carriers) if carriers else list(DEFAULT_CARRIERS)
    for c in chosen:
        if PLACEHOLDER not in c:
            raise NarrateError(f"carrier sentence {c!r} must contain {PLACEHOLDER} where the word goes")
    return chosen


def workdir_for(
    cache: Path, word: str, candidates: list[str], carriers: list[str], voice: str, style: str | None
) -> Path:
    """`<cache>/try/<sha>`: the same request reuses its build output."""
    key = json.dumps([word, candidates, carriers, voice, style], ensure_ascii=False)
    return cache / "try" / hashlib.sha256(key.encode("utf-8")).hexdigest()[:16]


def _toml(value: str) -> str:
    """A TOML basic string (JSON escapes are valid TOML ones; DEL is escaped by hand)."""
    return _DEL.sub("\\\\u007f", json.dumps(value, ensure_ascii=False))


def prepare(word: str, spellings: list[str], carriers: list[str], voice: str, style: str | None, workdir: Path) -> Path:
    """Write `try.narration.toml` into `workdir`: one line `c<i>-s<j>` per carrier and candidate.

    `spellings` are the candidates (the as-written word included); each line's text
    is the carrier with the word as written and its `say` the carrier with the candidate.
    """
    carriers = check_carriers(carriers)
    parts = [f"voice = {_toml(voice)}\n"]
    for i, carrier in enumerate(carriers):
        for j, spelling in enumerate(spellings):
            parts.append("\n[[line]]\n")
            parts.append(f'id = "c{i}-s{j}"\n')
            parts.append(f"text = {_toml(carrier.replace(PLACEHOLDER, word))}\n")
            parts.append(f"say = {_toml(carrier.replace(PLACEHOLDER, spelling))}\n")
            if style is not None:
                parts.append(f"style = {_toml(style)}\n")
            parts.append('kind = "line"\n')
    workdir.mkdir(parents=True, exist_ok=True)
    path = workdir / "try.narration.toml"
    path.write_text("".join(parts), encoding="utf-8")
    return path


def blind_order(word: str, n: int) -> list[int]:
    """Candidate indexes in label order (A, B, ...); a shuffle seeded from the word."""
    order = list(range(n))
    random.Random(int(hashlib.sha256(word.encode("utf-8")).hexdigest()[:8], 16)).shuffle(order)
    return order


def match_for(word: str) -> str:
    """The list entry's `match` regex for a word."""
    return "\\b" + re.escape(word) + "\\b"


def _read_list(path: Path) -> dict:
    if not path.exists():
        return {"about": LIST_ABOUT, "entries": [], "fine_as_written": []}
    try:
        data = json.loads(read_text_retry(path))
    except (OSError, ValueError) as e:
        raise NarrateError(f"{path}: cannot read pronunciation list ({e})") from e
    if not isinstance(data, dict) or not isinstance(data.get("entries"), list):
        raise NarrateError(f"{path}: expected an object with an 'entries' list")
    if not isinstance(data.setdefault("fine_as_written", []), list):
        raise NarrateError(f"{path}: 'fine_as_written' must be a list")
    return data


def _write_list(path: Path, data: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    replace_with_retry(tmp, path)


class TryApp:
    """Serves the lab for a built try script; a pick writes the pronunciation list."""

    def __init__(
        self, script: Path, word: str, candidates: list[str], list_path: Path, out: Path | None = None
    ) -> None:
        self.script_path = Path(script)
        self.word = word
        self.candidates = list(candidates)
        self.list_path = Path(list_path)
        loaded = load_script(self.script_path)
        self.out = Path(out) if out is not None else default_out(loaded)
        path = self.out / "narration.json"
        if not path.is_file():
            raise NarrateError(f"{path}: no manifest; build the try script first")
        self.manifest: Manifest = read_manifest(path)
        self.carrier_count = len({m.group(1) for ln in loaded.lines if (m := _LINE_ID.fullmatch(ln.id))})
        self.order = blind_order(word, len(self.candidates))
        self.picked: str | None = None
        self.spelling: str | None = None
        self._mutate = threading.Lock()

    def routes(self) -> Routes:
        """The lab routes."""
        r = Routes()
        r.add("GET", "/", lambda _b: (200, "text/html; charset=utf-8", _page()))
        r.add("GET", "/api/state", lambda _b: json_response(self._state()))
        r.add_prefix("GET", "/clips/", lambda p: serve_clip(self.out, [ln.id for ln in self.manifest.lines], p))
        r.add("POST", "/api/pick", self._pick)
        return r

    def _state(self) -> dict:
        flags = {ln.id: list(ln.flags) for ln in self.manifest.lines}
        cands = []
        for k, j in enumerate(self.order):
            clips = [
                {"id": f"c{i}-s{j}", "carrier_index": i, "flags": flags[f"c{i}-s{j}"]}
                for i in range(self.carrier_count)
                if f"c{i}-s{j}" in flags
            ]
            cand = {"label": string.ascii_uppercase[k], "clips": clips}
            if self.picked is not None:
                cand["spelling"] = self.candidates[j]
            cands.append(cand)
        return {
            "word": self.word,
            "candidates": cands,
            "list": str(self.list_path),
            "picked": self.picked,
            "spelling": self.spelling,
        }

    def _pick(self, body) -> Response:
        choice = body.get("choice") if isinstance(body, dict) else None
        labels = string.ascii_uppercase[: len(self.candidates)]
        if choice != "none" and not (isinstance(choice, str) and len(choice) == 1 and choice in labels):
            raise NarrateError(f"unknown choice {choice!r}")
        with self._mutate:
            if choice == "none":
                self.picked, self.spelling = "none", None
                data = _read_list(self.list_path) if self.list_path.exists() else None
            else:
                spelling = self.candidates[self.order[labels.index(choice)]]
                data = self._write_pick(spelling)
                self.picked, self.spelling = choice, spelling
            return json_response({**self._state(), "contents": data})

    def _write_pick(self, spelling: str) -> dict:
        data = _read_list(self.list_path)
        match = match_for(self.word)
        old = data["entries"]
        at = next((n for n, e in enumerate(old) if isinstance(e, dict) and e.get("match") == match), None)
        entries = [e for e in old if not (isinstance(e, dict) and e.get("match") == match)]
        fine = data["fine_as_written"]
        if spelling == self.word:
            if self.word not in fine:
                fine.append(self.word)
        else:
            why = (
                f"picked with narrate try on {datetime.date.today().isoformat()} "
                f"from {len(self.candidates)} spellings in {self.carrier_count} sentences"
            )
            entries.insert(
                len(entries) if at is None else min(at, len(entries)), {"match": match, "say": spelling, "why": why}
            )
            data["fine_as_written"] = [w for w in fine if w != self.word]
        data["entries"] = entries
        _write_list(self.list_path, data)
        return data


def serve(
    app: TryApp,
    port: int,
    opener: Callable[[str], object] | None,
    ready: Callable[[LocalServer], None] | None = None,
) -> None:
    """Serve the lab until interrupted (or until the server is shut down from elsewhere)."""
    server = LocalServer(app.routes(), port)
    thread = start(server)
    try:
        print(f"Trying {app.word!r} at {server.url} (Ctrl+C to stop)", flush=True)
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

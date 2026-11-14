import hashlib
import json
import shutil
import threading
import urllib.error
import urllib.request

import pytest

from narrate import NarrateError, fakes
from narrate.approved import ApprovedStore, approved_root
from narrate.build import BuildOptions, build
from narrate.cache import cache_root
from narrate.fakes import FakeBackend, FakeWorld
from narrate.lock import Lock, lock_path
from narrate.review import ReviewApp
from narrate.server import LocalServer, start

LINE = "Each filter is a group of its own."
OTHER = "Each filter is a group of its very own."


@pytest.fixture
def world(monkeypatch):
    w = FakeWorld()
    monkeypatch.setattr(fakes, "_DEFAULT", w)
    return w


def call(url, body=None, raw=None):
    data = raw if raw is not None else (None if body is None else json.dumps(body).encode())
    headers = {"Content-Type": "application/json"} if data is not None else {}
    req = urllib.request.Request(url, data=data, method="POST" if data is not None else "GET", headers=headers)
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def jcall(url, body=None):
    status, data = call(url, body)
    return status, json.loads(data)


def script_text(*lines):
    text = 'voice = "test"\n'
    for id_, t in lines:
        text += f'\n[[line]]\nid = "{id_}"\ntext = "{t}"\n'
    return text


@pytest.fixture
def make_app(root, write_script, voice_library, world):
    servers = []

    def make(*lines, backend=None):
        path = write_script(root, script_text(*lines))
        build(path)
        app = ReviewApp(path, BuildOptions(), backend=backend)
        server = LocalServer(app.routes())
        start(server)
        servers.append(server)
        return path, app, server

    yield make
    for s in servers:
        s.shutdown()
        s.server_close()


def test_missing_manifest_is_an_error(root, write_script, voice_library):
    path = write_script(root, script_text(("a", "Hello there.")))
    with pytest.raises(NarrateError, match="narration.json"):
        ReviewApp(path, BuildOptions())


def test_state_lists_lines(make_app):
    _, _, server = make_app(("a", LINE), ("b", OTHER))
    status, state = jcall(server.url + "/api/state")
    assert status == 200
    assert [ln["id"] for ln in state["manifest"]["lines"]] == ["a", "b"]
    assert state["lock"] == {"pins": {}, "notes": {}, "rejected": {}}
    assert state["building"] is False
    status, page = call(server.url + "/")
    assert status == 200 and b"<title>" in page
    status, wav = call(server.url + "/clips/a.wav")
    assert status == 200 and wav[:4] == b"RIFF"


def test_approve_pins_and_copies(make_app):
    path, _, server = make_app(("a", LINE), ("b", OTHER))
    _, state = jcall(server.url + "/api/state")
    check = state["manifest"]["lines"][0]["check"]
    status, state = jcall(server.url + "/api/approve", {"id": "a"})
    assert status == 200
    assert state["lock"]["pins"]["a"]["take"] == check["take"]
    assert state["lock"]["pins"]["a"]["key"] == check["key"]
    lock = Lock.load(lock_path(path))
    assert (lock.pins["a"].key, lock.pins["a"].take) == (check["key"], check["take"])
    assert "b" not in lock.pins
    assert ApprovedStore(approved_root(None)).has(check["key"], check["take"])
    _, again = jcall(server.url + "/api/state")
    assert "a" in again["lock"]["pins"]


def test_approve_errors(make_app):
    _, _, server = make_app(("a", LINE))
    assert jcall(server.url + "/api/approve", {"id": "zzz"})[0] == 400
    assert jcall(server.url + "/api/approve", {})[0] == 400
    assert jcall(server.url + "/api/note", {"id": "a", "note": 3})[0] == 400


def test_approve_missing_take_is_400(make_app):
    _, _, server = make_app(("a", LINE))
    shutil.rmtree(cache_root(None))
    status, body = jcall(server.url + "/api/approve", {"id": "a"})
    assert status == 400 and "neither" in body["error"]


def test_reject_then_rebuild_picks_another_take(make_app):
    _, _, server = make_app(("a", LINE))
    _, state = jcall(server.url + "/api/state")
    first = state["manifest"]["lines"][0]["check"]
    status, state = jcall(server.url + "/api/reject", {"id": "a"})
    assert status == 200
    assert state["lock"]["rejected"] == {first["key"]: [first["take"]]}
    status, state = jcall(server.url + "/api/rebuild", {})
    assert status == 200
    second = state["manifest"]["lines"][0]["check"]
    assert second["key"] == first["key"] and second["take"] != first["take"]


def test_note_round_trip(make_app):
    path, _, server = make_app(("a", LINE))
    _, state = jcall(server.url + "/api/note", {"id": "a", "note": "too fast"})
    assert state["lock"]["notes"] == {"a": "too fast"}
    assert Lock.load(lock_path(path)).notes == {"a": "too fast"}
    _, state = jcall(server.url + "/api/note", {"id": "a", "note": ""})
    assert state["lock"]["notes"] == {}


def test_approve_unflagged_skips_flagged(root, write_script, voice_library, world):
    world.script(LINE, heard=["completely different words here"])
    path = write_script(root, script_text(("a", LINE), ("b", OTHER)))
    manifest = build(path)
    assert [ln.status for ln in manifest.lines] == ["flagged", "new"]
    server = LocalServer(ReviewApp(path, BuildOptions()).routes())
    start(server)
    try:
        status, state = jcall(server.url + "/api/approve-unflagged", {})
        assert status == 200
        assert list(state["lock"]["pins"]) == ["b"]
    finally:
        server.shutdown()
        server.server_close()


def test_lock_edited_on_disk_between_requests_is_kept(make_app):
    path, _, server = make_app(("a", LINE), ("b", OTHER))
    jcall(server.url + "/api/approve", {"id": "a"})
    lock = Lock.load(lock_path(path))
    lock.note("b", "from disk")
    lock.save()
    _, state = jcall(server.url + "/api/note", {"id": "a", "note": "x"})
    assert state["lock"]["notes"] == {"a": "x", "b": "from disk"}
    assert "a" in state["lock"]["pins"]


class GatedBackend(FakeBackend):
    def __init__(self, world):
        super().__init__(world)
        self.entered = threading.Event()
        self.release = threading.Event()

    def synthesize(self, texts, reference, seed):
        self.entered.set()
        assert self.release.wait(10)
        return super().synthesize(texts, reference, seed)


def test_rebuild_conflict_409(make_app, world):
    backend = GatedBackend(world)
    _, _, server = make_app(("a", LINE), backend=backend)
    jcall(server.url + "/api/reject", {"id": "a"})  # forces a fresh take, so the rebuild synthesises
    first = {}
    t = threading.Thread(target=lambda: first.update(r=jcall(server.url + "/api/rebuild", {})))
    t.start()
    assert backend.entered.wait(10)
    assert jcall(server.url + "/api/state")[1]["building"] is True
    status, body = jcall(server.url + "/api/rebuild", {})
    assert status == 409 and "error" in body
    backend.release.set()
    t.join(10)
    assert first["r"][0] == 200


def test_clip_paths_cannot_escape(make_app):
    _, _, server = make_app(("open", LINE))
    assert call(server.url + "/clips/open.wav")[0] == 200
    assert call(server.url + "/clips/open.wav?take=2")[0] == 200
    for p in [
        "/clips/..%2Fnarration.json",
        "/clips/nope.wav",
        "/clips/open.wav/../x",
        "/clips/..%5Cnarration.json",
        "/clips/%6Fpen.wav",
    ]:
        assert call(server.url + p)[0] == 404


def test_oversized_body_413(make_app):
    _, _, server = make_app(("a", LINE))
    assert call(server.url + "/api/note", raw=b" " * (64 * 1024 + 1))[0] == 413


def test_bad_json_400(make_app):
    _, _, server = make_app(("a", LINE))
    assert call(server.url + "/api/approve", raw=b"{nope")[0] == 400


def test_approve_unflagged_keeps_rejections(make_app):
    path, _, server = make_app(("a", LINE), ("b", OTHER))
    jcall(server.url + "/api/reject", {"id": "b"})
    _, state = jcall(server.url + "/api/approve-unflagged", {})
    assert list(state["lock"]["pins"]) == ["a"]
    assert len(state["lock"]["rejected"]) == 1
    assert "b" not in Lock.load(lock_path(path)).pins


def test_approve_hash_matches_store_and_rebuild_is_approved(make_app):
    path, _, server = make_app(("a", LINE))
    _, state = jcall(server.url + "/api/state")
    check = state["manifest"]["lines"][0]["check"]
    jcall(server.url + "/api/approve", {"id": "a"})
    stored = ApprovedStore(approved_root(None)).path(check["key"], check["take"])
    assert Lock.load(lock_path(path)).pins["a"].sha256 == hashlib.sha256(stored.read_bytes()).hexdigest()
    status, state = jcall(server.url + "/api/rebuild", {})
    line = state["manifest"]["lines"][0]
    assert status == 200 and line["status"] == "approved" and "pinned-audio-missing" not in line["flags"]


def test_approve_falls_back_to_the_approved_store(make_app):
    path, _, server = make_app(("a", LINE))
    jcall(server.url + "/api/approve", {"id": "a"})
    shutil.rmtree(cache_root(None))
    status, state = jcall(server.url + "/api/approve", {"id": "a"})
    assert status == 200 and "a" in state["lock"]["pins"]
    stored = ApprovedStore(approved_root(None)).path(*(lambda p: (p.key, p.take))(Lock.load(lock_path(path)).pins["a"]))
    assert Lock.load(lock_path(path)).pins["a"].sha256 == hashlib.sha256(stored.read_bytes()).hexdigest()


def test_draft_manifest_and_missing_check_are_400(make_app):
    path, app, server = make_app(("a", LINE))
    app.manifest.lines[0].check = None
    status, body = jcall(server.url + "/api/approve", {"id": "a"})
    assert status == 400 and "no checked take" in body["error"]
    status, _ = jcall(server.url + "/api/reject", {"id": "a"})
    assert status == 400
    app.manifest.draft = True
    status, body = jcall(server.url + "/api/approve", {"id": "a"})
    assert status == 400 and "draft" in body["error"]


def test_state_reads_do_not_break_concurrent_saves(make_app):
    path, _, server = make_app(("a", LINE), ("b", OTHER))
    stop = threading.Event()

    def hammer():
        while not stop.is_set():
            call(server.url + "/api/state")

    readers = [threading.Thread(target=hammer) for _ in range(4)]
    for t in readers:
        t.start()
    try:
        for i in range(10):
            assert jcall(server.url + "/api/approve", {"id": "a" if i % 2 else "b"})[0] == 200
            assert jcall(server.url + "/api/note", {"id": "a", "note": f"n{i}"})[0] == 200
    finally:
        stop.set()
        for t in readers:
            t.join(10)
    lock = Lock.load(lock_path(path))
    assert set(lock.pins) == {"a", "b"} and lock.notes == {"a": "n9"}


def test_page_is_served(make_app):
    _, _, server = make_app(("a", LINE))
    status, data = call(server.url + "/")
    assert status == 200
    page = data.decode("utf-8")
    assert page.lstrip().lower().startswith("<!doctype html>")
    for api in ("/api/state", "/api/approve", "/api/reject", "/api/note", "/api/approve-unflagged", "/api/rebuild"):
        assert api in page
    assert "http://" not in page and "https://" not in page

import hashlib
import json
import shutil
import threading
import urllib.error
import urllib.request

import numpy as np
import pytest

from narrate import NarrateError, fakes
from narrate.approved import ApprovedStore, approved_root
from narrate.audio import normalise_loudness, read_wav, trim
from narrate.build import BuildOptions, build, default_out
from narrate.cache import TakeStore, cache_root
from narrate.fakes import FakeBackend, FakeWorld
from narrate.lock import Lock, lock_path
from narrate.manifest import read_manifest
from narrate.review import ReviewApp
from narrate.script import load_script
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


def shown(server, id_):
    """The sha256 of the take the page shows for a line (`check.sha256` in the state)."""
    _, state = jcall(server.url + "/api/state")
    return next(ln for ln in state["manifest"]["lines"] if ln["id"] == id_)["check"]["sha256"]


def heard(server, action, id_):
    """Approve or reject the take the page shows."""
    return jcall(server.url + f"/api/{action}", {"id": id_, "sha256": shown(server, id_)})


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
    status, state = heard(server, "approve", "a")
    assert status == 200
    assert state["lock"]["pins"]["a"]["take"] == check["take"]
    assert state["lock"]["pins"]["a"]["key"] == check["key"]
    lock = Lock.load(lock_path(path))
    assert (lock.pins["a"].key, lock.pins["a"].take) == (check["key"], check["take"])
    assert "b" not in lock.pins
    cached = TakeStore(cache_root(None)).path(check["key"], check["take"])
    assert lock.pins["a"].sha256 == hashlib.sha256(cached.read_bytes()).hexdigest()
    assert ApprovedStore(approved_root(None)).has(lock.pins["a"].sha256)
    _, again = jcall(server.url + "/api/state")
    assert "a" in again["lock"]["pins"]


def test_approve_errors(make_app):
    _, _, server = make_app(("a", LINE))
    sha = shown(server, "a")
    assert jcall(server.url + "/api/approve", {"id": "zzz", "sha256": sha})[0] == 400
    assert jcall(server.url + "/api/approve", {})[0] == 400
    for action in ("approve", "reject"):
        for bad in (None, 3, "abc", sha.upper()):
            status, body = jcall(server.url + f"/api/{action}", {"id": "a", "sha256": bad})
            assert status == 400 and "sha256" in body["error"]
    assert jcall(server.url + "/api/note", {"id": "a", "note": 3})[0] == 400


def test_approve_missing_take_is_400(make_app):
    _, _, server = make_app(("a", LINE))
    shutil.rmtree(cache_root(None))
    status, body = heard(server, "approve", "a")
    assert status == 400 and "neither" in body["error"]


def test_reject_then_rebuild_picks_another_take(make_app):
    _, _, server = make_app(("a", LINE))
    _, state = jcall(server.url + "/api/state")
    first = state["manifest"]["lines"][0]["check"]
    status, state = heard(server, "reject", "a")
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
    heard(server, "approve", "a")
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
    heard(server, "reject", "a")  # forces a fresh take, so the rebuild synthesises
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
    heard(server, "reject", "b")
    _, state = jcall(server.url + "/api/approve-unflagged", {})
    assert list(state["lock"]["pins"]) == ["a"]
    assert len(state["lock"]["rejected"]) == 1
    assert "b" not in Lock.load(lock_path(path)).pins


def test_approve_hash_matches_store_and_rebuild_is_approved(make_app):
    path, _, server = make_app(("a", LINE))
    heard(server, "approve", "a")
    sha = Lock.load(lock_path(path)).pins["a"].sha256
    assert hashlib.sha256(ApprovedStore(approved_root(None)).path(sha).read_bytes()).hexdigest() == sha
    status, state = jcall(server.url + "/api/rebuild", {})
    line = state["manifest"]["lines"][0]
    assert status == 200 and line["status"] == "approved" and "pinned-audio-missing" not in line["flags"]


def test_approve_falls_back_to_the_approved_store(make_app):
    path, _, server = make_app(("a", LINE))
    heard(server, "approve", "a")
    shutil.rmtree(cache_root(None))
    status, state = heard(server, "approve", "a")
    assert status == 200 and "a" in state["lock"]["pins"]
    sha = Lock.load(lock_path(path)).pins["a"].sha256
    assert hashlib.sha256(ApprovedStore(approved_root(None)).path(sha).read_bytes()).hexdigest() == sha


def test_identical_lines_keep_their_own_approved_audio(make_app):
    """Lines with the same text share a key; after a cache clear take numbers restart (probe P9)."""
    path, _, server = make_app(("x", LINE), ("y", LINE))
    assert heard(server, "approve", "x")[0] == 200
    x_pin = Lock.load(lock_path(path)).pins["x"]
    shutil.rmtree(cache_root(None) / "takes")
    _, state = jcall(server.url + "/api/rebuild", {})
    by = {ln["id"]: ln for ln in state["manifest"]["lines"]}
    assert by["x"]["status"] == "approved"
    assert (by["y"]["check"]["key"], by["y"]["check"]["take"]) == (x_pin.key, x_pin.take)  # same take, new audio
    assert heard(server, "approve", "y")[0] == 200
    y_pin = Lock.load(lock_path(path)).pins["y"]
    assert y_pin.sha256 != x_pin.sha256
    _, state = jcall(server.url + "/api/rebuild", {})
    by = {ln["id"]: ln for ln in state["manifest"]["lines"]}
    assert (by["x"]["status"], by["x"]["flags"]) == ("approved", [])
    assert (by["y"]["status"], by["y"]["flags"]) == ("approved", [])
    store = ApprovedStore(approved_root(None))
    assert hashlib.sha256(store.path(x_pin.sha256).read_bytes()).hexdigest() == x_pin.sha256
    audio, sr = store.load(x_pin.sha256)
    got, _ = read_wav(default_out(load_script(path)) / by["x"]["clip"])
    assert np.allclose(got, normalise_loudness(trim(audio, sr), sr), atol=1e-3)


def test_draft_manifest_and_missing_check_are_400(make_app):
    path, _, server = make_app(("a", LINE))
    sha = shown(server, "a")
    manifest_path = default_out(load_script(path)) / "narration.json"
    m = read_manifest(manifest_path)
    m.lines[0].check = None
    m.write(manifest_path)
    status, body = jcall(server.url + "/api/approve", {"id": "a", "sha256": sha})
    assert status == 400 and "no checked take" in body["error"]
    status, _ = jcall(server.url + "/api/reject", {"id": "a", "sha256": sha})
    assert status == 400
    m.draft = True
    m.write(manifest_path)
    status, body = jcall(server.url + "/api/approve", {"id": "a", "sha256": sha})
    assert status == 400 and "draft" in body["error"]


def test_state_reads_do_not_break_concurrent_saves(make_app):
    path, _, server = make_app(("a", LINE), ("b", OTHER))
    shas = [shown(server, "b"), shown(server, "a")]
    stop = threading.Event()

    def hammer():
        while not stop.is_set():
            call(server.url + "/api/state")

    readers = [threading.Thread(target=hammer) for _ in range(4)]
    for t in readers:
        t.start()
    try:
        for i in range(10):
            assert jcall(server.url + "/api/approve", {"id": "a" if i % 2 else "b", "sha256": shas[i % 2]})[0] == 200
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
    assert "sha256: c.sha256" in page  # approve and reject name the take the page shows


def test_approve_unflagged_skips_pinned_audio_missing(root, write_script, voice_library, world):
    """A `new` line flagged pinned-audio-missing carries a fresh, unheard take (probe P1)."""
    path = write_script(root, script_text(("a", LINE), ("b", OTHER)))
    first = build(path).lines[0]
    lock = Lock.load(lock_path(path))
    c = first.check
    lock.pin("a", c["key"], c["take"], first.duration, c["heard"], c["cer"], c["pace"], c["sha256"])
    lock.save()
    shutil.rmtree(cache_root(None) / "takes")  # a new machine: neither the cache nor the store has the audio
    m = build(path)
    assert (m.lines[0].status, m.lines[0].flags) == ("new", ["pinned-audio-missing"])
    server = LocalServer(ReviewApp(path, BuildOptions()).routes())
    start(server)
    try:
        status, state = jcall(server.url + "/api/approve-unflagged", {})
        assert status == 200
        assert state["lock"]["pins"]["a"]["approved_at"] == lock.pins["a"].approved_at
        assert Lock.load(lock_path(path)).pins["a"] == lock.pins["a"]
        assert "b" in state["lock"]["pins"]
    finally:
        server.shutdown()
        server.server_close()


BUILDING = "building — try again when the build finishes"
TAKE_CHANGED = "the take changed — reload"


def test_mutations_during_a_rebuild_are_409(make_app, world):
    backend = GatedBackend(world)
    path, _, server = make_app(("a", LINE), ("b", OTHER), backend=backend)
    heard(server, "reject", "a")  # forces a fresh take, so the rebuild synthesises
    shas = {i: shown(server, i) for i in ("a", "b")}
    before = lock_path(path).read_bytes()
    t = threading.Thread(target=lambda: jcall(server.url + "/api/rebuild", {}))
    t.start()
    try:
        assert backend.entered.wait(10)
        for url, body in [
            ("/api/approve", {"id": "b", "sha256": shas["b"]}),
            ("/api/reject", {"id": "b", "sha256": shas["b"]}),
            ("/api/note", {"id": "b", "note": "typed during the build"}),
            ("/api/approve-unflagged", {}),
        ]:
            assert jcall(server.url + url, body) == (409, {"error": BUILDING})
        assert lock_path(path).read_bytes() == before
    finally:
        backend.release.set()
        t.join(10)
    assert heard(server, "approve", "b")[0] == 200


def test_stale_take_is_409(make_app):
    path, _, server = make_app(("a", LINE))
    for action in ("approve", "reject"):
        assert jcall(server.url + f"/api/{action}", {"id": "a", "sha256": "0" * 64}) == (409, {"error": TAKE_CHANGED})
    assert Lock.load(lock_path(path)).pins == {} and Lock.load(lock_path(path)).rejected == {}


def test_take_rewritten_in_the_cache_is_409(make_app):
    path, _, server = make_app(("a", LINE))
    sha = shown(server, "a")
    _, state = jcall(server.url + "/api/state")
    c = state["manifest"]["lines"][0]["check"]
    cached = TakeStore(cache_root(None)).path(c["key"], c["take"])
    data = bytearray(cached.read_bytes())
    data[-1] ^= 0xFF
    cached.write_bytes(bytes(data))  # the take was regenerated after a cache clear, say
    for action in ("approve", "reject"):
        assert jcall(server.url + f"/api/{action}", {"id": "a", "sha256": sha}) == (409, {"error": TAKE_CHANGED})
    assert jcall(server.url + "/api/approve-unflagged", {}) == (409, {"error": TAKE_CHANGED})
    assert Lock.load(lock_path(path)).pins == {}


def test_manifest_rebuilt_elsewhere_makes_a_stale_approve_409(make_app):
    path, _, server = make_app(("a", LINE), ("b", OTHER))
    stale = shown(server, "a")
    lock = Lock.load(lock_path(path))
    c = read_manifest(default_out(load_script(path)) / "narration.json").lines[0].check
    lock.reject(c["key"], c["take"])
    lock.save()
    fresh = build(path).lines[0].check["sha256"]  # another `narrate build`, outside the review server
    assert fresh != stale
    assert jcall(server.url + "/api/approve", {"id": "a", "sha256": stale}) == (409, {"error": TAKE_CHANGED})
    assert shown(server, "a") == fresh  # the page reloads the take now on disk
    status, state = jcall(server.url + "/api/approve", {"id": "a", "sha256": fresh})
    assert status == 200 and Lock.load(lock_path(path)).pins["a"].sha256 == fresh


def test_approve_unflagged_pins_the_take_on_disk(make_app):
    path, _, server = make_app(("a", LINE))
    lock = Lock.load(lock_path(path))
    c = read_manifest(default_out(load_script(path)) / "narration.json").lines[0].check
    lock.reject(c["key"], c["take"])
    lock.save()
    fresh = build(path).lines[0].check
    status, state = jcall(server.url + "/api/approve-unflagged", {})
    assert status == 200
    pin = Lock.load(lock_path(path)).pins["a"]
    assert (pin.take, pin.sha256) == (fresh["take"], fresh["sha256"])

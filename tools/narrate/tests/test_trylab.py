import json
import re
import urllib.error
import urllib.request

import pytest

from narrate import NarrateError, fakes
from narrate.build import BuildOptions, build
from narrate.cache import cache_root
from narrate.cli import main
from narrate.fakes import FakeWorld
from narrate.script import load_script
from narrate.server import LocalServer, start
from narrate.trylab import TryApp, candidates_for, check_carriers, match_for, prepare, workdir_for

WORD = "Nebula"
SPELLINGS = [WORD, "neb-yoo-la"]
CARRIERS = ["First, {}.", 'Say "{}" here.', "Back to {}\\again."]


@pytest.fixture
def world(monkeypatch):
    w = FakeWorld()
    monkeypatch.setattr(fakes, "_DEFAULT", w)
    return w


def http(url, body=None):
    data = None if body is None else json.dumps(body).encode()
    headers = {"Content-Type": "application/json"} if data is not None else {}
    req = urllib.request.Request(url, data=data, method="POST" if data is not None else "GET", headers=headers)
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def jhttp(url, body=None):
    status, data = http(url, body)
    return status, json.loads(data)


@pytest.fixture
def lab(root, voice_library, world):
    servers = []

    def make(list_data=None, candidates=SPELLINGS, word=WORD):
        workdir = root / "work"
        script = prepare(word, candidates, CARRIERS, "test", None, workdir)
        out = workdir / "out"
        build(script, options=BuildOptions(out))
        list_path = root / "pron.json"
        if list_data is not None:
            list_path.write_text(json.dumps(list_data), encoding="utf-8")
        app = TryApp(script, word, candidates, list_path, out)
        server = LocalServer(app.routes())
        start(server)
        servers.append(server)
        return app, server, list_path

    yield make
    for s in servers:
        s.shutdown()
        s.server_close()


def label_for(server, spelling_index):
    """The blind label of candidate `spelling_index`, found via the clip ids (c*-s<j>)."""
    _, state = jhttp(server.url + "/api/state")
    for c in state["candidates"]:
        if c["clips"][0]["id"].endswith(f"-s{spelling_index}"):
            return c["label"]
    raise AssertionError


def test_prepare_writes_a_script(root):
    path = prepare(WORD, SPELLINGS, CARRIERS, "test", "explainer", root / "work")
    script = load_script(path)
    assert script.voice == "test"
    assert len(script.lines) == 6
    first = script.lines[1]
    assert first.id == "c0-s1"
    assert first.text == "First, Nebula."
    assert first.say == "First, neb-yoo-la."
    assert first.style == "explainer"
    assert first.kind == "line"
    assert script.lines[2].text == 'Say "Nebula" here.'
    assert script.lines[2].say == 'Say "Nebula" here.'
    assert script.lines[5].say == "Back to neb-yoo-la\\again."


def test_prepare_survives_awkward_text(root):
    path = prepare("a\x7f\U0001f600b", ["a\x7f\U0001f600b"], ["x {} y\n"], "test", None, root / "work")
    assert load_script(path).lines[0].text == "x a\x7f\U0001f600b y\n"


def test_bad_carrier_without_braces_is_a_user_error(root):
    with pytest.raises(NarrateError, match="must contain"):
        check_carriers(["no placeholder here"])
    with pytest.raises(NarrateError, match="must contain"):
        prepare(WORD, SPELLINGS, ["nothing"], "test", None, root / "work")
    assert main(["try", WORD, "--voice", "test", "--in", "nothing"]) == 2


def test_candidates_dedupe_and_default_carriers():
    assert candidates_for("A", ["b", "A", "b", "c"]) == ["A", "b", "c"]
    assert len(check_carriers([])) == 3
    with pytest.raises(NarrateError):
        candidates_for(" ", [])


def test_workdir_depends_on_the_request(root):
    a = workdir_for(root, "w", ["w", "x"], ["{}"], "v", None)
    assert a == workdir_for(root, "w", ["w", "x"], ["{}"], "v", None)
    assert a != workdir_for(root, "w", ["w", "y"], ["{}"], "v", None)
    assert a.parent == root / "try"


def test_try_end_to_end_with_fakes(lab):
    app, server, _ = lab(candidates=["Nebula", "neb-yoo-la", "nebb-you-la"])
    status, state = jhttp(server.url + "/api/state")
    assert status == 200
    assert state["word"] == WORD
    assert state["picked"] is None
    assert [c["label"] for c in state["candidates"]] == ["A", "B", "C"]
    assert all(len(c["clips"]) == 3 for c in state["candidates"])
    assert [cl["carrier_index"] for cl in state["candidates"][0]["clips"]] == [0, 1, 2]
    raw = json.dumps(state)
    assert "neb-yoo-la" not in raw and "nebb-you-la" not in raw
    assert all("spelling" not in c for c in state["candidates"])
    status, wav = http(server.url + "/clips/" + state["candidates"][0]["clips"][0]["id"] + ".wav")
    assert status == 200 and wav[:4] == b"RIFF"
    status, page = http(server.url + "/")
    assert status == 200 and b"None of these" in page


def test_blind_order_is_deterministic_and_a_permutation():
    from narrate.trylab import blind_order

    assert blind_order("Nebula", 5) == blind_order("Nebula", 5)
    assert sorted(blind_order("Nebula", 5)) == [0, 1, 2, 3, 4]
    assert any(blind_order(w, 4) != [0, 1, 2, 3] for w in ["a", "b", "c", "d", "e"])


def test_pick_respelling_writes_entry(lab):
    old = {"match": r"\bother\b", "say": "o-ther", "why": ""}
    app, server, list_path = lab({"about": "x", "entries": [old], "fine_as_written": [WORD]})
    label = label_for(server, 1)
    status, body = jhttp(server.url + "/api/pick", {"choice": label})
    assert status == 200
    assert body["picked"] == label and body["spelling"] == "neb-yoo-la"
    assert {c["label"]: c.get("spelling") for c in body["candidates"]}[label] == "neb-yoo-la"
    data = json.loads(list_path.read_text(encoding="utf-8"))
    assert body["contents"] == data
    assert data["entries"][0] == old
    entry = data["entries"][1]
    assert entry["match"] == match_for(WORD) == r"\bNebula\b"
    assert entry["say"] == "neb-yoo-la"
    assert re.fullmatch(r"picked with narrate try on \d{4}-\d\d-\d\d from 2 spellings in 3 sentences", entry["why"])
    assert data["fine_as_written"] == []
    assert '"\\\\bNebula\\\\b"' in list_path.read_text(encoding="utf-8")
    # picking again replaces rather than duplicates
    jhttp(server.url + "/api/pick", {"choice": label})
    assert len(json.loads(list_path.read_text(encoding="utf-8"))["entries"]) == 2
    # the written list is loadable
    from narrate.lexicon import Lexicon

    assert Lexicon.load([list_path]).apply("a Nebula") == "a neb-yoo-la"


def test_pick_creates_a_missing_list(lab):
    app, server, list_path = lab()
    assert not list_path.exists()
    jhttp(server.url + "/api/pick", {"choice": label_for(server, 1)})
    data = json.loads(list_path.read_text(encoding="utf-8"))
    assert set(data) == {"about", "entries", "fine_as_written"} and len(data["entries"]) == 1


def test_pick_as_written_marks_fine(lab):
    entry = {"match": match_for(WORD), "say": "old", "why": "x"}
    app, server, list_path = lab({"about": "x", "entries": [entry], "fine_as_written": ["Other"]})
    status, body = jhttp(server.url + "/api/pick", {"choice": label_for(server, 0)})
    assert status == 200
    data = json.loads(list_path.read_text(encoding="utf-8"))
    assert data["entries"] == []
    assert data["fine_as_written"] == ["Other", WORD]
    jhttp(server.url + "/api/pick", {"choice": label_for(server, 0)})
    assert json.loads(list_path.read_text(encoding="utf-8"))["fine_as_written"] == ["Other", WORD]


def test_pick_none_changes_nothing(lab):
    app, server, list_path = lab({"about": "x", "entries": [], "fine_as_written": []})
    before = list_path.read_bytes()
    status, body = jhttp(server.url + "/api/pick", {"choice": "none"})
    assert status == 200 and body["picked"] == "none"
    assert list_path.read_bytes() == before


def test_pick_rejects_unknown_choices(lab):
    app, server, list_path = lab()
    for bad in ("Z", "", None, 3):
        status, body = jhttp(server.url + "/api/pick", {"choice": bad})
        assert status == 400 and "error" in body
    assert not list_path.exists()


def test_cli_requires_to_without_a_pronunciation_list(voice_library, world):
    assert main(["try", WORD, "--voice", "test"]) == 2


def test_cli_try_serves_and_stops(voice_library, world, monkeypatch, root, capsys):
    import narrate.cli as cli

    seen = {}

    def fake_serve(app, port, opener):
        seen["app"], seen["port"], seen["opener"] = app, port, opener

    monkeypatch.setattr(cli, "try_serve", fake_serve)
    to = root / "p.json"
    code = main(["try", WORD, "--voice", "test", "--spell", "neb-yoo-la", "--to", str(to), "--no-browser"])
    assert code == 0
    app = seen["app"]
    assert seen["opener"] is None
    assert app.candidates == [WORD, "neb-yoo-la"] and app.list_path == to
    assert app.out.parent.parent == cache_root(None) / "try"
    assert (app.out.parent / "try.narration.toml").is_file()

import dataclasses
import json
import math

import pytest
import soundfile as sf

from narrate import fakes
from narrate.build import BuildOptions, build, default_out
from narrate.fakes import FakeWorld
from narrate.manifest import read_manifest
from narrate.profile import ProfileError, Takes
from narrate.script import ScriptError, load_script

LINE = "Each filter is a group of its own."
PASSAGE = "The first panel is wide. The second is tall. Together they make one sky."
SBARRA = "Data courtesy of Adam Sbarra."
SBARRA_SPOKEN = "Data courtesy of Adam Zabara."


def refuse(name):
    raise AssertionError(f"non-finite constant {name} in manifest")


@pytest.fixture
def world(monkeypatch):
    w = FakeWorld()
    monkeypatch.setattr(fakes, "_DEFAULT", w)
    return w


def script_text(*lines: tuple[str, str], extra: str = "", head: str = 'voice = "test"\n') -> str:
    body = head + extra
    for id_, text in lines:
        body += f'\n[[line]]\nid = "{id_}"\ntext = "{text}"\n'
    return body


def write_terms(root):
    (root / "terms.json").write_text(
        json.dumps({"entries": [{"match": "Sbarra", "say": "Zabara", "why": "name"}]}), encoding="utf-8"
    )


def spoken_texts(world) -> list[str]:
    return [t for call in world.calls for t in call]


@pytest.fixture
def make(root, write_script, voice_library):
    def _make(*lines, **kw):
        return write_script(root, script_text(*lines, **kw))

    return _make


def test_build_writes_clips_and_manifest(root, make, world):
    path = make(("one", LINE), ("two", PASSAGE))
    m = build(path)
    out = default_out(load_script(path))
    assert out == root / "out" / "demo"
    assert [ln.status for ln in m.lines] == ["new", "new"]
    assert [ln.kind for ln in m.lines] == ["line", "passage"]
    for ln, ncap in zip(m.lines, (1, 3), strict=True):
        data, sr = sf.read(out / ln.clip)
        assert sr == m.sample_rate
        assert ln.duration == pytest.approx(len(data) / sr)
        assert ln.clip == f"clips/{ln.id}.wav"
        assert 1 <= ln.check["take"] <= 3
        caps = ln.captions
        assert len(caps) == ncap
        assert caps[0].start == 0.0
        assert caps[-1].end == pytest.approx(ln.duration)
        for a, b in zip(caps, caps[1:], strict=False):
            assert a.end == pytest.approx(b.start)
    back = read_manifest(out / "narration.json")
    assert back.script == "demo.narration.toml"
    assert back.voice == "test"
    assert [ln.to_json() for ln in back.lines] == [ln.to_json() for ln in m.lines]


def test_rebuild_is_all_cache(make, world):
    path = make(("one", LINE), ("two", PASSAGE))
    build(path)
    world.events.clear()
    build(path)
    assert "synthesize" not in world.events
    assert "transcribe" not in world.events
    assert "close-backend" not in world.events


def test_editing_one_line_regenerates_only_it(root, make, write_script, world):
    build(make(("one", LINE), ("two", PASSAGE)))
    world.calls.clear()
    edited = "Each filter is a group of its very own."
    build(write_script(root, script_text(("one", edited), ("two", PASSAGE))))
    assert spoken_texts(world) == [edited] * 3


def test_pronunciation_change_regenerates_affected_lines(root, make, write_script, world):
    build(make(("a", SBARRA), ("b", LINE)))
    world.calls.clear()
    write_terms(root)
    build(write_script(root, script_text(("a", SBARRA), ("b", LINE), extra='pronunciations = ["terms.json"]\n')))
    assert spoken_texts(world) == [SBARRA_SPOKEN] * 3


def test_failing_line_gets_more_takes_then_flags(make, world):
    world.script(LINE, heard=["garbled"])
    m = build(make(("one", LINE)))
    assert (m.lines[0].status, m.lines[0].flags) == ("flagged", ["words"])
    assert m.flagged() == ["one"]
    assert spoken_texts(world) == [LINE] * 6


def test_second_round_rescues(make, world):
    world.script(LINE, heard=["garbled", "garbled", "garbled", LINE])
    m = build(make(("one", LINE)))
    assert m.lines[0].status == "new"
    assert m.lines[0].check["take"] == 4


def test_passage_too_fast_flags_pace(make, world):
    world.script(PASSAGE, pace=[6.0])
    m = build(make(("two", PASSAGE)))
    assert (m.lines[0].status, m.lines[0].flags) == ("flagged", ["pace"])


def test_fast_line_is_not_flagged(make, world):
    world.script(LINE, pace=[6.0, 6.0, 4.6])
    assert build(make(("one", LINE))).lines[0].check["take"] == 3


def test_fast_line_alone_stays_new(make, world):
    world.script(LINE, pace=[6.0])
    assert build(make(("one", LINE))).lines[0].status == "new"


def test_backend_closed_before_checking(make, world):
    world.script(LINE, heard=["garbled", "garbled", "garbled", LINE])
    build(make(("one", LINE), ("two", PASSAGE)))
    backend_busy = False
    for e in world.events:
        if e == "synthesize":
            backend_busy = True
        elif e == "close-backend":
            backend_busy = False
        elif e == "transcribe":
            assert not backend_busy
    checker_loaded = False
    for e in world.events:
        if e == "transcribe":
            checker_loaded = True
        elif e == "close-checker":
            checker_loaded = False
        elif e == "synthesize":
            assert not checker_loaded
    assert world.events.count("close-backend") == 2
    assert world.events[-1] == "close-checker"


def test_missing_style_fails_before_generation(root, write_script, voice_library, world):
    body = script_text(("one", LINE)) + '\n[[line]]\nid = "two"\ntext = "Story time."\nstyle = "story"\n'
    with pytest.raises(ProfileError, match=r"line 'two': .*story"):
        build(write_script(root, body))
    assert world.events == []


def test_build_ignores_its_output_folder(make, world):
    path = make(("one", LINE))
    out = default_out(load_script(path))
    build(path)
    assert (out / ".gitignore").read_text(encoding="utf-8").strip() == "*"


def test_missing_voice_is_a_script_error(root, write_script, voice_library, world):
    with pytest.raises(ScriptError):
        build(write_script(root, script_text(("one", LINE), head="")))


def test_one_word_line(make, world):
    ln = build(make(("one", "Next."))).lines[0]
    assert ln.status == "new"
    assert math.isfinite(ln.check["pace"])


def test_empty_transcript_flags_line(root, make, world):
    world.script(LINE, heard=[""])
    path = make(("one", LINE))
    m = build(path)
    assert (m.lines[0].status, m.lines[0].flags) == ("flagged", ["words"])
    json.loads((default_out(load_script(path)) / "narration.json").read_text(encoding="utf-8"), parse_constant=refuse)


def test_silent_take_pace_is_null_in_manifest(make, world, monkeypatch):
    import narrate.build as build_module

    real = build_module.evaluate

    def silent(*args):
        return dataclasses.replace(real(*args), pace=math.inf)

    monkeypatch.setattr(build_module, "evaluate", silent)
    path = make(("one", LINE))
    m = build(path)
    out = default_out(load_script(path)) / "narration.json"
    data = json.loads(out.read_text(encoding="utf-8"), parse_constant=refuse)
    assert data["lines"][0]["check"]["pace"] is None
    assert m.lines[0].check["pace"] == math.inf


def test_respelled_line_passes(root, write_script, voice_library, world):
    write_terms(root)
    world.script(SBARRA_SPOKEN, heard=[SBARRA_SPOKEN])
    path = write_script(root, script_text(("a", SBARRA), extra='pronunciations = ["terms.json"]\n'))
    ln = build(path).lines[0]
    assert ln.status == "new"
    assert "Zabara" in ln.spoken


@pytest.mark.parametrize("order", [["a", "b"], ["b", "a"]])
def test_shared_key_lines_get_their_own_verdicts(root, write_script, voice_library, world, order):
    write_terms(root)
    world.script(SBARRA_SPOKEN, heard=[SBARRA])
    texts = {"a": SBARRA, "b": SBARRA_SPOKEN}
    body = script_text(*[(i, texts[i]) for i in order], extra='pronunciations = ["terms.json"]\n')
    by = {ln.id: ln for ln in build(write_script(root, body)).lines}
    assert by["a"].check["key"] == by["b"].check["key"]
    assert (by["a"].status, by["a"].flags) == ("new", [])
    assert (by["b"].status, by["b"].flags) == ("flagged", ["words"])


def test_options_redirect_out(root, make, world):
    out = root / "elsewhere"
    build(make(("one", LINE)), options=BuildOptions(out=out))
    assert (out / "narration.json").is_file()


def test_takes_option_overrides_the_profile(make, world):
    world.script(LINE, heard=["garbled"])
    m = build(make(("one", LINE)), options=BuildOptions(takes=Takes(count=2, max=2)))
    assert m.lines[0].status == "flagged"
    assert spoken_texts(world) == [LINE] * 2


def refuse_models(monkeypatch):
    import narrate.build as build_module

    def no(*_a, **_kw):
        raise AssertionError("a model was constructed for a build with nothing to generate or hear")

    monkeypatch.setattr(build_module, "get_backend", no)
    monkeypatch.setattr(build_module, "get_checker", no)


def test_cached_build_constructs_no_models(make, world, monkeypatch):
    path = make(("one", LINE), ("two", PASSAGE))
    first = build(path)
    refuse_models(monkeypatch)
    again = build(path)
    assert [ln.to_json() for ln in again.lines] == [ln.to_json() for ln in first.lines]


def test_approved_build_constructs_no_models(make, world, monkeypatch):
    import hashlib
    import shutil

    from narrate.approved import ApprovedStore, approved_root
    from narrate.cache import TakeStore, cache_root
    from narrate.lock import Lock, lock_path

    path = make(("one", LINE))
    ln = build(path).lines[0]
    c = ln.check
    cached = TakeStore(cache_root(None)).path(c["key"], c["take"])
    sha = hashlib.sha256(cached.read_bytes()).hexdigest()
    ApprovedStore(approved_root(None)).put(sha, cached)
    lock = Lock.load(lock_path(path))
    lock.pin("one", c["key"], c["take"], ln.duration, c["heard"], c["cer"], c["pace"], sha)
    lock.save()
    shutil.rmtree(cache_root(None))
    refuse_models(monkeypatch)
    assert build(path).lines[0].status == "approved"

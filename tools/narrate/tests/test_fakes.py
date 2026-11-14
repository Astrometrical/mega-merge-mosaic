import subprocess
import sys

import pytest

from narrate import fakes, measure
from narrate.audio import read_wav, write_wav
from narrate.backends import get_backend
from narrate.checkers import get_checker
from narrate.fakes import FakeBackend, FakeChecker, FakeWorld
from narrate.profile import load_profile

PASSAGE = "Each filter is a group of its own. We stack the frames together. Then we stretch the result."


@pytest.fixture
def world():
    return FakeWorld()


@pytest.fixture
def ref(voice_library):
    return load_profile("test").reference("explainer", "passage")


@pytest.mark.parametrize("target", [4.0, 6.0])
def test_fake_pace_round_trips(world, ref, target):
    world.script(PASSAGE, pace=[target])
    (audio,) = FakeBackend(world).synthesize([PASSAGE], ref, seed=1)
    assert measure.pace(audio, fakes.SAMPLE_RATE, PASSAGE) == pytest.approx(target, rel=0.05)
    assert len(measure.pauses(audio, fakes.SAMPLE_RATE)) >= 2


def test_fake_checker_hears_what_was_scripted(world, ref, root):
    world.script("hello there", heard=["garbled", "ok text"])
    backend, checker = FakeBackend(world), FakeChecker(world)
    heard = []
    for i in range(2):
        (audio,) = backend.synthesize(["hello there"], ref, seed=1)
        path = root / f"take{i}.wav"
        write_wav(path, audio, backend.sample_rate, subtype="FLOAT")
        back, sr = read_wav(path)
        heard.append(checker.transcribe(back, sr))
    assert heard == ["garbled", "ok text"]


def test_unscripted_text_is_heard_as_spoken(world, ref):
    (audio,) = FakeBackend(world).synthesize(["plain words"], ref, seed=3)
    assert FakeChecker(world).transcribe(audio, fakes.SAMPLE_RATE) == "plain words"


def test_events_order(world, ref):
    backend, checker = FakeBackend(world), FakeChecker(world)
    (audio,) = backend.synthesize(["one two"], ref, seed=1)
    backend.close()
    checker.transcribe(audio, backend.sample_rate)
    checker.close()
    assert world.events == ["synthesize", "close-backend", "transcribe", "close-checker"]
    assert world.calls == [["one two"]]


def test_registry(monkeypatch, voice_library):
    monkeypatch.setattr(fakes, "_DEFAULT", FakeWorld())
    profile = load_profile("test")
    backend = get_backend("fake", profile)
    checker = get_checker("fake")
    assert backend.world is fakes.default_world()
    assert checker.world is fakes.default_world()
    with pytest.raises(ValueError, match=r"qwen3-tts.*fake"):
        get_backend("nope", profile)
    with pytest.raises(ValueError, match=r"whisper.*fake"):
        get_checker("nope")


def test_core_import_does_not_import_torch():
    mods = ["narrate.backends", "narrate.checkers", "narrate.cli", "narrate.draft", "narrate.fakes", "narrate.build"]
    code = f"import {', '.join(mods)}, sys; assert 'torch' not in sys.modules"
    subprocess.run([sys.executable, "-c", code], check=True)

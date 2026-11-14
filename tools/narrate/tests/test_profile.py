import dataclasses
from pathlib import Path

import pytest

from narrate.profile import Pace, ProfileError, Takes, load_profile, voice_library


def test_loads_profile(voice_library):
    p = load_profile("test")
    assert p.name == "test"
    assert p.root == voice_library / "test"
    assert (p.backend, p.checker, p.model) == ("fake", "fake", "fake/model")
    assert (p.language, p.precision, p.default_style) == ("English", "fp16", "explainer")
    assert p.pronunciations == ()
    assert p.pace == Pace(5.3, 4.8)
    assert p.takes == Takes(3, 6)
    short = p.styles["explainer"]["short"]
    assert short.seconds == pytest.approx(10.0)
    assert short.text == "short text"
    assert short.audio == voice_library / "test" / "refs" / "explainer-10.wav"
    assert len(short.sha256) == 64 and int(short.sha256, 16) >= 0
    assert load_profile("test").styles["explainer"]["short"].sha256 == short.sha256
    assert p.styles["explainer"]["long"].seconds == pytest.approx(49.0)


def test_reference_choice(voice_library):
    p = load_profile("test")
    assert p.reference("explainer", "line") is p.styles["explainer"]["short"]
    assert p.reference("explainer", "passage") is p.styles["explainer"]["long"]


def test_missing_style_names_it(voice_library):
    with pytest.raises(ProfileError, match="story"):
        load_profile("test").reference("story", "line")


def test_missing_length_names_it(voice_library):
    p = load_profile("test")
    short_only = {"explainer": {"short": p.styles["explainer"]["short"]}}
    p = dataclasses.replace(p, styles=short_only)
    with pytest.raises(ProfileError, match="long"):
        p.reference("explainer", "passage")


def test_library_resolution(tmp_path, monkeypatch):
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path / "home"))
    monkeypatch.delenv("NARRATE_VOICES")
    assert voice_library() == tmp_path / "home" / ".narrate" / "voices"
    monkeypatch.setenv("NARRATE_VOICES", str(tmp_path / "env"))
    assert voice_library() == tmp_path / "env"
    assert voice_library(tmp_path / "given") == tmp_path / "given"


def test_missing_profile_says_where_it_looked(voice_library):
    with pytest.raises(ProfileError) as e:
        load_profile("nobody")
    assert str(voice_library / "nobody" / "voice.toml") in str(e.value)


def test_missing_audio(voice_library):
    (voice_library / "test" / "refs" / "explainer-10.wav").unlink()
    with pytest.raises(ProfileError, match="explainer-10.wav"):
        load_profile("test")


def _edit(lib, old, new):
    f = lib / "test" / "voice.toml"
    f.write_text(f.read_text(encoding="utf-8").replace(old, new), encoding="utf-8")


@pytest.mark.parametrize(
    "old,new",
    [
        ('backend = "fake"', 'backend = "fake"\nbogus = 1'),
        ("[style.explainer]", "[pace]\nmax = 5.3\ntarget = 4.8\nfast = 1\n\n[style.explainer]"),
        ("[style.explainer]", "[takes]\ncount = 3\nmax = 6\nmore = 1\n\n[style.explainer]"),
        ('text = "short text"', 'text = "short text", extra = 1'),
    ],
)
def test_unknown_key(voice_library, old, new):
    _edit(voice_library, old, new)
    with pytest.raises(ProfileError, match="voice.toml"):
        load_profile("test")


def test_unknown_key_in_style(voice_library):
    _edit(voice_library, "[style.explainer]", "[style.explainer]\nmedium = {}")
    with pytest.raises(ProfileError, match="medium"):
        load_profile("test")


def test_default_style_must_exist(voice_library):
    _edit(voice_library, 'default_style = "explainer"', 'default_style = "story"')
    with pytest.raises(ProfileError, match="story"):
        load_profile("test")

import pytest

from narrate.build import default_out
from narrate.captions import share_captions
from narrate.draft import draft
from narrate.manifest import read_manifest
from narrate.script import load_script
from narrate.text import syllables

PASSAGE = "The first panel is wide. The second is tall. Together they make one sky."


def test_draft_needs_no_voice(root, write_script):
    path = write_script(root, f'voice = "nobody"\n\n[[line]]\nid = "p"\ntext = "{PASSAGE}"\n')
    m = draft(path)  # no voice library exists
    assert m.draft is True and m.sample_rate is None and m.voice == "nobody"
    (ln,) = m.lines
    assert ln.status == "draft" and ln.clip is None and ln.check is None
    assert ln.duration == pytest.approx(syllables(PASSAGE) / 4.8)
    assert ln.captions == share_captions(PASSAGE, ln.duration)
    assert len(ln.captions) == 3
    assert read_manifest(default_out(load_script(path)) / "narration.json").draft is True


def test_draft_uses_say_and_pace(root, write_script):
    path = write_script(root, '[[line]]\nid = "a"\ntext = "Hello there."\nsay = "Hello there friend."\n')
    (ln,) = draft(path, pace_target=2.0).lines
    assert ln.spoken == "Hello there friend."
    assert ln.duration == pytest.approx(syllables("Hello there friend.") / 2.0)
    assert len(ln.captions) == 1

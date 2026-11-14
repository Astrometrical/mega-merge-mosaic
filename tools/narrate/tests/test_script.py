import re

import pytest

from narrate import NarrateError
from narrate.script import Line, ScriptError, load_script


def test_non_utf8_script(root):
    p = root / "bad.narration.toml"
    p.write_bytes(b'voice = "test" # \xff\xfe\n')
    with pytest.raises(ScriptError, match=r"bad\.narration\.toml"):
        load_script(p)


def test_loads_lines_and_resolves_lists(root, write_script):
    p = write_script(
        root,
        """
voice = "test"
style = "explainer"
pronunciations = ["terms.json"]
[[line]]
id = "open"
text = "Hello there."
[[line]]
id = "story-1"
text = "A. B. C."
style = "story"
kind = "passage"
at = 1.5
say = "Ay. Bee. See."
""",
    )
    s = load_script(p)
    assert s.voice == "test" and s.style == "explainer" and s.stem == "demo"
    assert s.pronunciations == ((root / "terms.json").resolve(),)
    assert [ln.id for ln in s.lines] == ["open", "story-1"]
    assert s.lines[1] == Line("story-1", "A. B. C.", "story", "passage", 1.5, "Ay. Bee. See.")


@pytest.mark.parametrize(
    "body, needle",
    [
        ('[[line]]\nid = "Open"\ntext = "x"', "'Open'"),
        ('[[line]]\nid = "a"\ntext = "x"\n[[line]]\nid = "a"\ntext = "y"', "duplicate"),
        ('[[line]]\nid = "a"\ntext = "…"', "'a'"),
        ('[[line]]\nid = "a"\ntext = "x"\nkind = "chapter"', "kind"),
        ('[[line]]\nid = "a"\ntext = "x"\nat = -1', "at"),
        ('[[line]]\nid = "a"\ntext = "x"\ntxet = "typo"', "txet"),
        ('voice = "v"', "no lines"),
    ],
)
def test_rejects_bad_scripts(root, write_script, body, needle):
    with pytest.raises(ScriptError, match=re.escape(needle)):
        load_script(write_script(root, body))


def test_bad_toml_names_file(root, write_script):
    p = write_script(root, "this is = = not toml")
    with pytest.raises(ScriptError) as e:
        load_script(p)
    assert str(p) in str(e.value)
    assert isinstance(e.value, NarrateError)

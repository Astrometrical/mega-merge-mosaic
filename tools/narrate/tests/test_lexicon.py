import json

import pytest

from narrate import NarrateError
from narrate.lexicon import Lexicon, LexiconError


def write_list(path, entries, **extra):
    path.write_text(json.dumps({"entries": entries, **extra}), encoding="utf-8")
    return path


ENTRIES = [
    {"match": r"\bAstrometrical\b", "say": "Astro-metrical", "why": "stress"},
    {"match": r"\bData\b", "say": "Dahta", "why": "American as written"},
    {"match": r"\bdata\b", "say": "dahta", "why": "as above"},
    {"match": r"\b(\d+) GB\b", "say": r"\1 gigabytes", "why": "gigibytes"},
    {"match": r"\bSHO\b", "say": "S H O", "why": "Show"},
]


def test_applies_in_order_with_word_bounds(root):
    lx = Lexicon.load([write_list(root / "a.json", ENTRIES)])
    assert (
        lx.apply("From Astrometrical: OSC data, about 2 GB apiece. SHO!")
        == "From Astro-metrical: OSC dahta, about 2 gigabytes apiece. S H O!"
    )
    assert lx.apply("Datapoints and SHOW") == "Datapoints and SHOW"


def test_lists_chain_in_given_order(root):
    a = write_list(root / "a.json", [{"match": "Foo", "say": "Bar", "why": ""}])
    b = write_list(root / "b.json", [{"match": "Bar", "say": "Baz", "why": ""}])
    assert Lexicon.load([a, b]).apply("Foo") == "Baz"
    assert Lexicon.load([b, a]).apply("Foo") == "Bar"


def test_variants_mix_written_and_respelled(root):
    lx = Lexicon.load(
        [
            write_list(
                root / "a.json",
                [
                    {"match": r"\bData\b", "say": "Dahta", "why": ""},
                    {"match": r"\bSbarra\b", "say": "Zabara", "why": ""},
                    {"match": r"\bXISF\b", "say": "X I S F", "why": ""},
                ],
            )
        ]
    )
    v = lx.variants("Data courtesy of Adam Sbarra.")
    assert v[0] == "Data courtesy of Adam Sbarra."
    assert sorted(v) == sorted(
        [
            "Data courtesy of Adam Sbarra.",
            "Dahta courtesy of Adam Sbarra.",
            "Data courtesy of Adam Zabara.",
            "Dahta courtesy of Adam Zabara.",
        ]
    )
    assert lx.variants("Nothing to change.") == ["Nothing to change."]


def test_variants_over_limit_falls_back(root):
    entries = [{"match": f"w{i}", "say": f"x{i}", "why": ""} for i in range(7)]
    lx = Lexicon.load([write_list(root / "a.json", entries)])
    text = " ".join(f"w{i}" for i in range(7))
    assert lx.variants(text, limit=64) == [text, lx.apply(text)]
    assert len(lx.variants(text, limit=128)) == 128


def test_optional_keys_accepted(root):
    p = write_list(root / "a.json", [], about="x", fine_as_written=["a"])
    assert Lexicon.load([p]).apply("hi") == "hi"


def test_bad_regex_names_entry(root):
    p = write_list(
        root / "bad.json",
        [{"match": "ok", "say": "a", "why": ""}, {"match": "(", "say": "b", "why": ""}],
    )
    with pytest.raises(LexiconError, match=r"bad\.json.*entry 2"):
        Lexicon.load([p])


@pytest.mark.parametrize("say", [r"\2", r"\q"])
def test_bad_say_template_names_entry(root, say):
    p = write_list(
        root / "t.json",
        [{"match": "ok", "say": "a", "why": ""}, {"match": r"\bFoo\b", "say": say, "why": ""}],
    )
    with pytest.raises(LexiconError, match=r"t\.json.*entry 2.*say"):
        Lexicon.load([p])


def test_missing_file(root):
    with pytest.raises(LexiconError, match=r"nope\.json"):
        Lexicon.load([root / "nope.json"])


def test_missing_say(root):
    p = write_list(root / "m.json", [{"match": "a", "why": ""}])
    with pytest.raises(LexiconError, match=r"m\.json.*entry 1.*say"):
        Lexicon.load([p])


def test_is_narrate_error():
    assert issubclass(LexiconError, NarrateError)

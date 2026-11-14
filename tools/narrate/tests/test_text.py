from narrate.text import cer, infer_kind, norm, punct_distance, sentences, syllables

TWELVE = "Twelve panels, each 9,255 by 18,310 pixels, about 2 GB apiece."


def test_whisper_formatting_normalises_away():
    assert cer(TWELVE, "12 panels each 9,255 x 18,310 pixels about 2GB a piece") == 0.0
    assert (
        cer(
            "Three narrowband filters: H-alpha, O III and S II.",
            "Three narrowband filters, Hα, O3 and S2.",
        )
        == 0.0
    )
    assert (
        cer(
            "Hi, I'm Daniel Paull, and this is Mega Merge Mosaic.",
            "Hi, I'm Daniel Paull and this is MegaMerge Mosaic.",
        )
        == 0.0
    )
    assert norm("two hundred and fifty-six") == "256"
    assert norm("nine thousand two hundred and fifty five") == "9255"
    assert norm("R, G and B") == "rgandb"


def test_real_mistakes_fail():
    for text, heard in [
        (
            "Hi, I'm Daniel Paull, and this is Mega Merge Mosaic.",
            "Hi, I'm Daniel Pohl and this is MegaMerge Mosaic.",
        ),
        ("Data courtesy of Adam Sbarra.", "Data courtesy of Adam Sparrow."),
        ("Save it as an XISF file.", "Save it as an .exis file."),
        (
            "SHO, HOO and RGB are built in, and you can add your own.",
            "Show, Who and RGB are built in and you can add your own.",
        ),
    ]:
        assert cer(text, heard) >= 0.04, heard


def test_punctuation_breaks_ties():
    text = "...and natural colour from R, G and B."
    apart, merged = "and natural colour from R, G and B.", "and Natural Color from RG&B."
    assert cer(text, apart) == cer(text, merged) == 0.0
    assert punct_distance(text, apart) < punct_distance(text, merged)


def test_norm_typography():
    assert norm("what’s new — here…") == norm("what's new - here...")


def test_sentences_and_kinds():
    showcase = (
        "The North America Nebula is enormous. From a dark site, it spans about four full Moons across the sky, far "
        "too big for most telescopes to capture in a single frame. So imagers do what map makers have always done: "
        "they photograph it in pieces, and then they stitch the pieces together."
    )
    assert len(sentences(showcase)) == 3 and infer_kind(showcase) == "passage"
    assert sentences("Composites combine filters. Here is SHO, the Hubble palette...") == [
        "Composites combine filters.",
        "Here is SHO, the Hubble palette...",
    ]
    assert infer_kind("Each filter is a group of its own. Pick one to see its panels.") == "line"
    assert infer_kind("word " * 52) == "passage"  # 260 characters, one sentence


def test_syllables():
    assert syllables("The North America Nebula is enormous.") == 13
    assert syllables("X I S F") == 4 and syllables("XISF") == 1

"""Text helpers: sentences, syllables, kind inference and ASR-comparison normalisation."""

import re

PASSAGE_SENTENCES = 3
PASSAGE_CHARS = 250

_SENTENCE_BREAK = re.compile(r"""(?:(?<=[.?!…])|(?<=[.?!…]["'”’]))\s+(?=[A-Z0-9"'“‘])""")
_VOWELS = re.compile(r"[aeiouy]+")
_WORD = re.compile(r"[a-z]+")

_UNITS = {
    w: i
    for i, w in enumerate(
        "zero one two three four five six seven eight nine ten eleven twelve "
        "thirteen fourteen fifteen sixteen seventeen eighteen nineteen".split()
    )
}
_TENS = {w: 20 + 10 * i for i, w in enumerate("twenty thirty forty fifty sixty seventy eighty ninety".split())}
_SCALES = {"hundred": 100, "thousand": 1000}
_NUMBER_WORDS = set(_UNITS) | set(_TENS) | set(_SCALES)
_ROMAN = {"ii": "2", "iii": "3", "iv": "4"}


def sentences(text: str) -> list[str]:
    """Split after . ? ! or … (optionally plus a closing quote) before a capital, digit or opening quote."""
    return [p.strip() for p in _SENTENCE_BREAK.split(text) if p.strip()]


def syllables(text: str) -> int:
    """Vowel-group syllable estimate: at least one per `[a-z]+` word."""
    return sum(max(1, len(_VOWELS.findall(w))) for w in _WORD.findall(text.lower()))


def infer_kind(text: str) -> str:
    """`"passage"` for long text, else `"line"`."""
    if len(sentences(text)) >= PASSAGE_SENTENCES or len(text) >= PASSAGE_CHARS:
        return "passage"
    return "line"


def _number_runs(words: list[str]) -> list[str]:
    out: list[str] = []
    i = 0
    while i < len(words):
        if words[i] not in _NUMBER_WORDS:
            out.append(words[i])
            i += 1
            continue
        total = current = 0
        while i < len(words):
            w = words[i]
            if w in _UNITS:
                current += _UNITS[w]
            elif w in _TENS:
                current += _TENS[w]
            elif w == "hundred":
                current = max(current, 1) * 100
            elif w == "thousand":
                total += max(current, 1) * 1000
                current = 0
            elif w == "and" and words[i - 1] in _SCALES and i + 1 < len(words) and words[i + 1] in _NUMBER_WORDS:
                pass
            else:
                break
            i += 1
        out.append(str(total + current))
    return out


def norm(text: str) -> str:
    """Normalise text so ASR formatting differences compare equal."""
    t = text.lower()
    t = t.replace("’", "'").replace("‘", "'")
    t = t.replace("α", "alpha")
    t = t.replace("&", " and ")
    t = re.sub(r"(\d)\s*[x×]\s*(\d)", r"\1 by \2", t)
    t = re.sub(r"\bsulfur", "sulphur", t)
    t = re.sub(r"\bcolor", "colour", t)
    t = re.sub(r"\bgigabytes?\b", "gb", t)
    t = t.replace("-", " ")
    t = re.sub(r"[^a-z0-9 ]", " ", t)
    words = [_ROMAN.get(w, w) for w in t.split()]
    return "".join(_number_runs(words))


def lev(a: str, b: str) -> int:
    """Levenshtein distance."""
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def cer(ref: str, hyp: str) -> float:
    """Character error rate between normalised reference and hypothesis."""
    r = norm(ref)
    return lev(r, norm(hyp)) / max(1, len(r))


def light(text: str) -> str:
    """Light normalisation that keeps commas and word spacing."""
    t = text.lower().replace("&", " and ")
    t = re.sub(r"[^a-z0-9, ]", "", t)
    return re.sub(r" +", " ", t).strip()


def punct_distance(ref: str, hyp: str) -> int:
    """Levenshtein distance between the lightly normalised texts."""
    return lev(light(ref), light(hyp))

"""Build a script: takes, checks, picks, clips, captions and the manifest (spec 5.2)."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import numpy as np

from narrate.audio import normalise_loudness, trim, write_wav
from narrate.backends import Backend, get_backend
from narrate.batching import Job, batch_seed, plan_batches
from narrate.cache import TakeStore, cache_root, take_key
from narrate.captions import Caption, sentence_captions
from narrate.checkers import Checker, get_checker
from narrate.lexicon import Lexicon
from narrate.manifest import Manifest, ManifestLine
from narrate.profile import ProfileError, Reference, VoiceProfile, load_profile
from narrate.script import Line, Script, ScriptError, load_script
from narrate.select import Choice, TakeResult, choose, evaluate
from narrate.text import infer_kind


@dataclass(frozen=True)
class BuildOptions:
    """Where a build reads and writes; `None` means the default."""

    out: Path | None = None
    voices: Path | None = None
    cache: Path | None = None


@dataclass(frozen=True)
class _Plan:
    """Everything about a line that is known before generation."""

    line: Line
    spoken: str
    kind: str
    reference: Reference
    key: str
    answers: list[str]


def default_out(script: Script) -> Path:
    """`<script dir>/out/<stem>`."""
    return script.path.parent / "out" / script.stem


def ensure_ignored(out: Path) -> None:
    """Create `out` with a `.gitignore` of `*` (kept if present) so voice audio is never committed."""
    out.mkdir(parents=True, exist_ok=True)
    ignore = out / ".gitignore"
    if not ignore.exists():
        ignore.write_text("*\n", encoding="utf-8")


def _plan_lines(script: Script, profile: VoiceProfile, lexicon: Lexicon) -> list[_Plan]:
    plans = []
    for line in script.lines:
        spoken = line.say or lexicon.apply(line.text)
        kind = line.kind or infer_kind(line.text)
        style = line.style or script.style or profile.default_style
        try:
            reference = profile.reference(style, kind)
        except ProfileError as e:
            raise ProfileError(f"line {line.id!r}: {e}") from e
        answers = lexicon.variants(line.text)
        if line.say:
            answers = [*answers, line.say]
        plans.append(_Plan(line, spoken, kind, reference, take_key(profile, reference, spoken), answers))
    return plans


def _generate(backend: Backend, store: TakeStore, jobs: list[Job]) -> None:
    """Synthesise and store the jobs, then release the backend."""
    if not jobs:
        return
    try:
        for batch in plan_batches(jobs):
            audios = backend.synthesize([j.spoken for j in batch], batch[0].reference, batch_seed(batch[0]))
            for job, audio in zip(batch, audios, strict=True):
                store.save(job.key, job.take, audio, backend.sample_rate)
    finally:
        backend.close()


def _missing_jobs(plans: list[_Plan], store: TakeStore, takes: range) -> list[Job]:
    jobs: dict[tuple[str, int], Job] = {}
    for p in plans:
        have = set(store.takes(p.key))
        for n in takes:
            if n not in have and (p.key, n) not in jobs:
                jobs[(p.key, n)] = Job(p.key, n, p.spoken, p.reference)
    return list(jobs.values())


def _check(plan: _Plan, store: TakeStore, checker: Checker, takes: range, results: dict[int, TakeResult]) -> None:
    """Score takes not yet in `results`, transcribing only those never heard."""
    for n in takes:
        if n in results:
            continue
        audio, sr = store.load(plan.key, n)
        heard = store.heard(plan.key, n, checker.name)
        if heard is None:
            heard = checker.transcribe(audio, sr)
            store.set_heard(plan.key, n, checker.name, heard)
        results[n] = evaluate(n, plan.answers, plan.spoken, heard, audio, sr)


def _captions(plan: _Plan, clip: np.ndarray, sr: int, duration: float) -> list[Caption]:
    if plan.kind == "line":
        return [Caption(plan.line.text, 0.0, duration)]
    return sentence_captions(plan.line.text, clip, sr)


def _select(plans: list[_Plan], profile: VoiceProfile, store, backend: Backend, checker: Checker) -> dict[str, Choice]:
    """Rounds 1 and 2: generate, check and choose a take per line key."""
    results: dict[str, dict[int, TakeResult]] = {p.line.id: {} for p in plans}
    choices: dict[str, Choice] = {}  # by line id
    first = range(1, profile.takes.count + 1)
    _generate(backend, store, _missing_jobs(plans, store, first))
    for p in plans:
        _check(p, store, checker, first, results[p.line.id])
        choices[p.line.id] = choose(p.kind, list(results[p.line.id].values()), profile.pace)
    flagged = [p for p in plans if choices[p.line.id].status == "flagged"]
    if flagged and profile.takes.max > profile.takes.count:
        more = range(profile.takes.count + 1, profile.takes.max + 1)
        jobs = _missing_jobs(flagged, store, more)
        if jobs:
            checker.close()  # Whisper and Qwen are never in VRAM together; Whisper reloads lazily
        _generate(backend, store, jobs)
        everything = range(1, profile.takes.max + 1)
        for p in flagged:
            _check(p, store, checker, everything, results[p.line.id])
            choices[p.line.id] = choose(p.kind, list(results[p.line.id].values()), profile.pace)
    return choices


def build(
    script_path: Path,
    *,
    backend: Backend | None = None,
    checker: Checker | None = None,
    options: BuildOptions = BuildOptions(),
) -> Manifest:
    """Build every line of a script and write `clips/` and `narration.json`."""
    script = load_script(script_path)
    if script.voice is None:
        raise ScriptError(f'{script.path}: no voice set (add voice = "<profile>")')
    profile = load_profile(script.voice, options.voices)
    lexicon = Lexicon.load([*script.pronunciations, *profile.pronunciations])
    plans = _plan_lines(script, profile, lexicon)
    store = TakeStore(cache_root(options.cache))
    out = options.out if options.out is not None else default_out(script)
    backend = backend if backend is not None else get_backend(profile.backend, profile)
    checker = checker if checker is not None else get_checker(profile.checker)

    try:
        choices = _select(plans, profile, store, backend, checker)
    finally:
        checker.close()

    ensure_ignored(out)
    lines: list[ManifestLine] = []
    sample_rate: int | None = None
    for p in plans:
        choice = choices[p.line.id]
        audio, sr = store.load(p.key, choice.take.n)
        clip = normalise_loudness(trim(audio, sr), sr)
        rel = f"clips/{p.line.id}.wav"
        write_wav(out / rel, clip, sr)
        duration = len(clip) / sr
        sample_rate = sr
        lines.append(
            ManifestLine(
                id=p.line.id,
                text=p.line.text,
                spoken=p.spoken,
                kind=p.kind,
                clip=rel,
                duration=duration,
                at=p.line.at,
                status=choice.status,
                flags=list(choice.flags),
                captions=_captions(p, clip, sr, duration),
                check={
                    "heard": choice.take.heard,
                    "cer": choice.take.cer,
                    "pace": choice.take.pace,
                    "take": choice.take.n,
                    "key": p.key,
                },
            )
        )
    manifest = Manifest(script.path.name, script.voice, False, sample_rate, lines=lines)
    manifest.write(out / "narration.json")
    return manifest

"""Build a script: takes, checks, picks, clips, captions and the manifest (spec 5.2)."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Generic, Protocol, TypeVar

import numpy as np

from narrate._fs import ensure_ignored, sha256_file
from narrate.approved import ApprovedStore, approved_root
from narrate.audio import normalise_loudness, trim, write_wav
from narrate.backends import Backend, get_backend
from narrate.batching import Job, batch_seed, plan_batches
from narrate.cache import TakeStore, cache_root, take_key
from narrate.captions import Caption, sentence_captions
from narrate.checkers import Checker, get_checker
from narrate.lexicon import Lexicon
from narrate.lock import Lock, Pin, lock_path
from narrate.manifest import Manifest, ManifestLine
from narrate.profile import ProfileError, Reference, Takes, VoiceProfile, load_profile
from narrate.script import Line, Script, ScriptError, load_script
from narrate.select import Choice, TakeResult, choose, evaluate
from narrate.text import infer_kind


@dataclass(frozen=True)
class BuildOptions:
    """Where a build reads and writes, and how many takes it tries; `None` means the default.

    `takes` overrides the profile's `[takes]` policy (`narrate try` uses two takes per line).
    """

    out: Path | None = None
    voices: Path | None = None
    cache: Path | None = None
    approved: Path | None = None
    takes: Takes | None = None


class _Closable(Protocol):
    def close(self) -> None: ...


_M = TypeVar("_M", bound=_Closable)


class _Lazy(Generic[_M]):
    """A model made on first `get()`, so a build with nothing to generate or hear loads none.

    `close()` releases only a model that was made, and keeps it: the models reload lazily.
    """

    def __init__(self, make: Callable[[], _M]) -> None:
        self._make = make
        self._made: _M | None = None

    def get(self) -> _M:
        """The model, made now if it was not yet."""
        if self._made is None:
            self._made = self._make()
        return self._made

    def close(self) -> None:
        """Release the model if it was made."""
        if self._made is not None:
            self._made.close()


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


def _generate(backend: _Lazy[Backend], store: TakeStore, jobs: list[Job]) -> None:
    """Synthesise and store the jobs, then release the backend (made only if there are jobs)."""
    if not jobs:
        return
    try:
        model = backend.get()
        for batch in plan_batches(jobs):
            audios = model.synthesize([j.spoken for j in batch], batch[0].reference, batch_seed(batch[0]))
            for job, audio in zip(batch, audios, strict=True):
                store.save(job.key, job.take, audio, model.sample_rate)
    finally:
        backend.close()


def _wanted(have: list[int], rejected: set[int], target: int) -> list[int]:
    """Take numbers to consider: live existing takes first, then fresh numbers above every used one."""
    live = [n for n in have if n not in rejected]
    base = max([*have, *rejected], default=0)
    fresh = range(base + 1, base + 1 + max(0, target - len(live)))
    return [*live, *fresh][:target]


def _missing_jobs(plans: list[_Plan], store: TakeStore, wanted: dict[str, list[int]]) -> list[Job]:
    jobs: dict[tuple[str, int], Job] = {}
    for p in plans:
        have = set(store.takes(p.key))
        for n in wanted[p.line.id]:
            if n not in have and (p.key, n) not in jobs:
                jobs[(p.key, n)] = Job(p.key, n, p.spoken, p.reference)
    return list(jobs.values())


def _check(plan: _Plan, store: TakeStore, checker: _Checking, takes: list[int], results: dict[int, TakeResult]) -> None:
    """Score takes not yet in `results`, transcribing (and so making the checker) only those never heard."""
    for n in takes:
        if n in results:
            continue
        audio, sr = store.load(plan.key, n)
        heard = store.heard(plan.key, n, checker.name)
        if heard is None:
            heard = checker.model.get().transcribe(audio, sr)
            store.set_heard(plan.key, n, checker.name, heard)
        results[n] = evaluate(n, plan.answers, plan.spoken, heard, audio, sr)


def _captions(plan: _Plan, clip: np.ndarray, sr: int, duration: float) -> list[Caption]:
    if plan.kind == "line":
        return [Caption(plan.line.text, 0.0, duration)]
    return sentence_captions(plan.line.text, clip, sr)


@dataclass(frozen=True)
class _Checking:
    """The checker (made on first transcription) and the name its transcripts are cached under."""

    model: _Lazy[Checker]
    name: str


def _select(
    plans: list[_Plan],
    profile: VoiceProfile,
    takes: Takes,
    store: TakeStore,
    backend: _Lazy[Backend],
    checker: _Checking,
    rejected: dict[str, set[int]],
) -> dict[str, Choice]:
    """Rounds 1 and 2: generate, check and choose a take per line id."""

    def wanted(ps: list[_Plan], target: int) -> dict[str, list[int]]:
        return {p.line.id: _wanted(store.takes(p.key), rejected.get(p.key, set()), target) for p in ps}

    results: dict[str, dict[int, TakeResult]] = {p.line.id: {} for p in plans}
    choices: dict[str, Choice] = {}
    first = wanted(plans, takes.count)
    _generate(backend, store, _missing_jobs(plans, store, first))
    for p in plans:
        _check(p, store, checker, first[p.line.id], results[p.line.id])
        choices[p.line.id] = choose(p.kind, list(results[p.line.id].values()), profile.pace)
    flagged = [p for p in plans if choices[p.line.id].status == "flagged"]
    if flagged and takes.max > takes.count:
        more = wanted(flagged, takes.max)
        jobs = _missing_jobs(flagged, store, more)
        if jobs:
            checker.model.close()  # Whisper and Qwen are never in VRAM together; Whisper reloads lazily
        _generate(backend, store, jobs)
        everything = wanted(flagged, takes.max)
        for p in flagged:
            _check(p, store, checker, everything[p.line.id], results[p.line.id])
            choices[p.line.id] = choose(p.kind, list(results[p.line.id].values()), profile.pace)
    return choices


def _usable_pins(
    plans: list[_Plan], lock: Lock, store: TakeStore, approved: ApprovedStore
) -> tuple[dict[str, Pin], set[str]]:
    """Pins that apply and whose audio matches their hash, plus the ids of pins whose audio is missing.

    The audio comes from the approved store, else (repairing the store) from the take cache;
    audio that does not hash to the pin's `sha256` counts as missing and is never copied.
    """
    usable: dict[str, Pin] = {}
    missing: set[str] = set()
    for p in plans:
        pin = lock.pins.get(p.line.id)
        if pin is None or pin.key != p.key or pin.take in lock.rejected.get(pin.key, set()):
            continue
        if approved.has(pin.sha256) and sha256_file(approved.path(pin.sha256)) == pin.sha256:
            usable[p.line.id] = pin
            continue
        cached = store.path(pin.key, pin.take)
        if cached.is_file() and sha256_file(cached) == pin.sha256:
            approved.put(pin.sha256, cached)
            usable[p.line.id] = pin
        else:
            missing.add(p.line.id)
    return usable, missing


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
    approved = ApprovedStore(approved_root(options.approved))
    lock = Lock.load(lock_path(script.path))
    pins, pinned_missing = _usable_pins(plans, lock, store, approved)
    out = options.out if options.out is not None else default_out(script)
    takes = options.takes if options.takes is not None else profile.takes
    # A registry checker's `name` is its registry name, so cached transcripts are found without making it.
    checking = _Checking(
        _Lazy(lambda: checker if checker is not None else get_checker(profile.checker)),
        checker.name if checker is not None else profile.checker,
    )
    synth = _Lazy(lambda: backend if backend is not None else get_backend(profile.backend, profile))

    todo = [p for p in plans if p.line.id not in pins]
    try:
        choices = _select(todo, profile, takes, store, synth, checking, lock.rejected)
    finally:
        checking.model.close()

    ensure_ignored(out)
    lines: list[ManifestLine] = []
    sample_rate: int | None = None
    for p in plans:
        pin = pins.get(p.line.id)
        if pin is not None:
            audio, sr = approved.load(pin.sha256)
            status, flags = "approved", []
            check = {
                "heard": pin.heard,
                "cer": pin.cer,
                "pace": pin.pace,
                "take": pin.take,
                "key": p.key,
                "sha256": pin.sha256,
            }
        else:
            choice = choices[p.line.id]
            audio, sr = store.load(p.key, choice.take.n)
            status = choice.status
            flags = [*choice.flags, *(["pinned-audio-missing"] if p.line.id in pinned_missing else [])]
            check = {
                "heard": choice.take.heard,
                "cer": choice.take.cer,
                "pace": choice.take.pace,
                "take": choice.take.n,
                "key": p.key,
                "sha256": sha256_file(store.path(p.key, choice.take.n)),
            }
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
                status=status,
                flags=flags,
                captions=_captions(p, clip, sr, duration),
                check=check,
            )
        )
    manifest = Manifest(script.path.name, script.voice, False, sample_rate, lines=lines)
    manifest.write(out / "narration.json")
    return manifest

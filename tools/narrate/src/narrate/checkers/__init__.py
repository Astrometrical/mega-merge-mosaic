"""Speech checkers: the `Checker` boundary and its registry."""

from __future__ import annotations

from typing import Protocol

import numpy as np

from narrate import NarrateError

CHECKER_NAMES = ("whisper", "fake")


class Checker(Protocol):
    """Transcribes audio so takes can be compared with the script."""

    name: str

    def transcribe(self, audio: np.ndarray, sr: int) -> str:
        """What the audio says."""
        ...

    def close(self) -> None:
        """Free the model; the next `transcribe` reloads it lazily."""
        ...


def get_checker(name: str) -> Checker:
    """The checker called `name`, imported lazily so core never loads torch."""
    if name == "whisper":
        try:
            from narrate.checkers.whisper import WhisperChecker
        except ImportError as e:
            raise NarrateError("checker 'whisper' needs the qwen extra: uv sync --extra qwen") from e
        return WhisperChecker()
    if name == "fake":
        from narrate import fakes

        return fakes.FakeChecker(fakes.default_world())
    raise NarrateError(f"unknown checker {name!r}; known checkers: {', '.join(CHECKER_NAMES)}")

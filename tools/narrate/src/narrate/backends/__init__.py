"""Speech-synthesis backends: the `Backend` boundary and its registry."""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol

import numpy as np

if TYPE_CHECKING:
    from narrate.profile import Reference, VoiceProfile

BACKEND_NAMES = ("qwen3-tts", "fake")


class Backend(Protocol):
    """Turns texts into speech in a reference voice."""

    sample_rate: int

    def synthesize(self, texts: list[str], reference: Reference, seed: int) -> list[np.ndarray]:
        """One mono float32 clip per text, in input order."""
        ...

    def close(self) -> None:
        """Free the model; the next `synthesize` reloads it lazily."""
        ...


def get_backend(name: str, profile: VoiceProfile) -> Backend:
    """The backend called `name`, imported lazily so core never loads torch."""
    if name == "qwen3-tts":
        from narrate.backends.qwen import QwenBackend

        return QwenBackend(profile)
    if name == "fake":
        from narrate import fakes

        return fakes.FakeBackend(fakes.default_world())
    raise ValueError(f"unknown backend {name!r}; known backends: {', '.join(BACKEND_NAMES)}")

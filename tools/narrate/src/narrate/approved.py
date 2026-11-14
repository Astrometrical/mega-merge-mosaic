"""Private store of approved takes' audio, kept outside git."""

from __future__ import annotations

import os
import shutil
from pathlib import Path

import numpy as np

from narrate._fs import replace_with_retry
from narrate.audio import read_wav


def approved_root(override: Path | None = None) -> Path:
    """`override`, else `$NARRATE_APPROVED`, else `~/.narrate/approved`."""
    if override is not None:
        return override
    env = os.environ.get("NARRATE_APPROVED")
    if env:
        return Path(env)
    return Path.home() / ".narrate" / "approved"


class ApprovedStore:
    """Approved takes under `root/<key[:2]>/<key>/take-<n>.wav`."""

    def __init__(self, root: Path) -> None:
        self.root = root

    def path(self, key: str, take: int) -> Path:
        """Where the approved copy of `take` of `key` lives."""
        return self.root / key[:2] / key / f"take-{take}.wav"

    def has(self, key: str, take: int) -> bool:
        """Whether that take has been stored."""
        return self.path(key, take).is_file()

    def put(self, key: str, take: int, src: Path) -> Path:
        """Copy `src` into the store atomically."""
        final = self.path(key, take)
        tmp = final.with_name(final.name + ".tmp")
        final.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, tmp)
        replace_with_retry(tmp, final)
        return final

    def load(self, key: str, take: int) -> tuple[np.ndarray, int]:
        """Read the approved take as mono float32."""
        return read_wav(self.path(key, take))

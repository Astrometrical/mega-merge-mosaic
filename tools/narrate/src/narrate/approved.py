"""Private store of approved takes' audio, kept outside git and addressed by content."""

from __future__ import annotations

import hashlib
import os
import tempfile
from pathlib import Path

import numpy as np

from narrate import NarrateError
from narrate._fs import ensure_ignored, replace_with_retry, sha256_file
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
    """Approved audio under `root/<sha[:2]>/<sha>.wav`, `sha` being the sha256 of the WAV bytes.

    An address names exactly one audio, so storing a take never replaces another
    line's approved audio, whatever their keys or take numbers.
    """

    def __init__(self, root: Path) -> None:
        self.root = root

    def path(self, sha: str) -> Path:
        """Where the audio whose sha256 is `sha` lives."""
        return self.root / sha[:2] / f"{sha}.wav"

    def has(self, sha: str) -> bool:
        """Whether that audio has been stored."""
        return self.path(sha).is_file()

    def put(self, sha: str, src: Path) -> Path:
        """Store `src`, whose bytes must hash to `sha`; a no-op when the store already holds that audio.

        A file already at the address is never rewritten, unless it does not hash to its
        name (it is corrupt and is nobody's audio).
        """
        data = Path(src).read_bytes()
        if hashlib.sha256(data).hexdigest() != sha:
            raise NarrateError(f"{src}: audio does not hash to {sha}; not stored")
        final = self.path(sha)
        if final.is_file() and sha256_file(final) == sha:
            return final
        ensure_ignored(self.root)
        final.parent.mkdir(parents=True, exist_ok=True)
        fd, name = tempfile.mkstemp(dir=final.parent, prefix=final.name + ".", suffix=".tmp")
        tmp = Path(name)
        try:
            with os.fdopen(fd, "wb") as f:
                f.write(data)
            replace_with_retry(tmp, final)
        finally:
            tmp.unlink(missing_ok=True)
        return final

    def load(self, sha: str) -> tuple[np.ndarray, int]:
        """Read the stored audio as mono float32."""
        return read_wav(self.path(sha))

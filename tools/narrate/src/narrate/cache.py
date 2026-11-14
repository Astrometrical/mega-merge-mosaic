"""Content-addressed cache of synthesised takes."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path

import numpy as np
import soundfile as sf

from narrate._fs import read_text_retry, replace_with_retry
from narrate.audio import read_wav
from narrate.profile import Reference, VoiceProfile

KEY_LENGTH = 24


def cache_root(override: Path | None = None) -> Path:
    """`override`, else `$NARRATE_CACHE`, else `~/.narrate/cache`."""
    if override is not None:
        return override
    env = os.environ.get("NARRATE_CACHE")
    if env:
        return Path(env)
    return Path.home() / ".narrate" / "cache"


def take_key(profile: VoiceProfile, reference: Reference, spoken: str) -> str:
    """Identify a line's takes by everything that shapes the audio."""
    payload = json.dumps(
        {
            "backend": profile.backend,
            "language": profile.language,
            "model": profile.model,
            "precision": profile.precision,
            "ref_sha256": reference.sha256,
            "ref_text": reference.text,
            "spoken": spoken,
        },
        sort_keys=True,
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:KEY_LENGTH]


class TakeStore:
    """Takes on disk under `root/takes/<key[:2]>/<key>/take-<n>.wav`."""

    def __init__(self, root: Path) -> None:
        self.root = root

    def path(self, key: str, n: int) -> Path:
        """Where take `n` of `key` lives."""
        return self.root / "takes" / key[:2] / key / f"take-{n}.wav"

    def takes(self, key: str) -> list[int]:
        """Existing take numbers, ascending."""
        folder = self.path(key, 0).parent
        if not folder.is_dir():
            return []
        found = []
        for p in folder.glob("take-*.wav"):
            stem = p.stem.removeprefix("take-")
            if stem.isdigit():
                found.append(int(stem))
        return sorted(found)

    def save(self, key: str, n: int, audio: np.ndarray, sr: int) -> Path:
        """Write take `n` atomically (exact float round trip)."""
        final = self.path(key, n)
        tmp = final.with_name(final.name + ".tmp")
        tmp.parent.mkdir(parents=True, exist_ok=True)
        self._sidecar(key, n).unlink(missing_ok=True)  # a transcript belongs to its audio
        sf.write(tmp, audio, sr, subtype="FLOAT", format="WAV")
        replace_with_retry(tmp, final)
        return final

    def load(self, key: str, n: int) -> tuple[np.ndarray, int]:
        """Read take `n` as mono float32."""
        return read_wav(self.path(key, n))

    def _sidecar(self, key: str, n: int) -> Path:
        return self.path(key, n).with_suffix(".json")

    def _fingerprint(self, key: str, n: int) -> str:
        return hashlib.sha256(self.path(key, n).read_bytes()).hexdigest()

    def _read_heard(self, key: str, n: int) -> dict[str, str]:
        """Transcripts recorded for the take's current audio; none if it changed."""
        p = self._sidecar(key, n)
        if not p.is_file() or not self.path(key, n).is_file():
            return {}
        data = json.loads(read_text_retry(p))
        if data.get("sha256") != self._fingerprint(key, n):
            return {}
        return data.get("heard", {})

    def heard(self, key: str, n: int, checker: str) -> str | None:
        """What `checker` heard in take `n`, if it has listened."""
        return self._read_heard(key, n).get(checker)

    def set_heard(self, key: str, n: int, checker: str, text: str) -> None:
        """Record what `checker` heard in take `n`."""
        heard = self._read_heard(key, n)
        heard[checker] = text
        final = self._sidecar(key, n)
        final.parent.mkdir(parents=True, exist_ok=True)
        tmp = final.with_name(final.name + ".tmp")
        tmp.write_text(json.dumps({"sha256": self._fingerprint(key, n), "heard": heard}), encoding="utf-8")
        replace_with_retry(tmp, final)

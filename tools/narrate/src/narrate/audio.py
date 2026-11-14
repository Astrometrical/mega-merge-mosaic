"""WAV files, trimming and loudness normalisation."""

from pathlib import Path

import numpy as np
import pyloudnorm
import soundfile as sf

from narrate.measure import speech_span

LOUDNESS_LUFS = -20.0
PEAK_DBFS = -1.0
TRIM_PAD = 0.05
_MIN_MEASURE_SECS = 0.4


def read_wav(path: Path) -> tuple[np.ndarray, int]:
    """Read a WAV as mono float32 (channels averaged)."""
    data, sr = sf.read(path, dtype="float32", always_2d=True)
    return data.mean(axis=1).astype(np.float32), sr


def write_wav(path: Path, audio: np.ndarray, sr: int, subtype: str = "PCM_16") -> None:
    """Write a WAV, creating parent folders."""
    path.parent.mkdir(parents=True, exist_ok=True)
    sf.write(path, audio, sr, subtype=subtype)


def trim(audio: np.ndarray, sr: int, pad: float = TRIM_PAD) -> np.ndarray:
    """Cut to the speech span plus `pad` seconds each side, clamped to the clip."""
    start, end = speech_span(audio, sr)
    if end <= start:
        return audio
    a = max(0, round((start - pad) * sr))
    b = min(len(audio), round((end + pad) * sr))
    return audio[a:b]


def normalise_loudness(
    audio: np.ndarray, sr: int, lufs: float = LOUDNESS_LUFS, peak_dbfs: float = PEAK_DBFS
) -> np.ndarray:
    """Scale to integrated loudness `lufs`; the gain is limited to keep the peak <= `peak_dbfs`."""
    peak = float(np.max(np.abs(audio))) if len(audio) else 0.0
    if peak == 0.0:
        return audio
    measured = audio
    min_len = int(np.ceil(_MIN_MEASURE_SECS * sr))
    if len(measured) < min_len:
        measured = np.pad(measured, (0, min_len - len(measured)))
    loudness = pyloudnorm.Meter(sr).integrated_loudness(measured.astype(np.float64))
    if not np.isfinite(loudness):
        return audio
    gain = 10 ** ((lufs - loudness) / 20)
    gain = min(gain, 10 ** (peak_dbfs / 20) / peak)
    return (audio * gain).astype(np.float32)

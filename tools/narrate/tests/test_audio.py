import numpy as np
import pyloudnorm
import pytest

from narrate.audio import normalise_loudness, read_wav, trim, write_wav
from narrate.measure import speech_span

SR = 24000


def tone(secs, amp=0.3):
    t = np.arange(int(secs * SR)) / SR
    return (amp * np.sin(2 * np.pi * 220 * t)).astype(np.float32)


def test_trim_keeps_pad():
    y = np.concatenate([np.zeros(SR), tone(2.0), np.zeros(SR)])
    start, end = speech_span(y, SR)
    out = trim(y, SR)
    assert len(out) / SR == pytest.approx(end - start + 0.1, abs=0.02)
    assert len(out) < len(y)


def test_float_round_trip_is_exact(root):
    y = np.random.default_rng(0).uniform(-1, 1, 1000).astype(np.float32)
    p = root / "sub dir" / "a.wav"
    write_wav(p, y, SR, subtype="FLOAT")
    back, sr = read_wav(p)
    assert sr == SR
    assert back.tobytes() == y.tobytes()


def test_read_wav_averages_channels(root):
    import soundfile as sf

    p = root / "st.wav"
    sf.write(p, np.stack([np.full(100, 0.2), np.full(100, 0.4)], axis=1), SR, subtype="FLOAT")
    y, _ = read_wav(p)
    assert y.ndim == 1 and y.dtype == np.float32
    assert y == pytest.approx(0.3, abs=1e-6)


def test_normalise_hits_target():
    y = tone(3.0, 0.05)
    out = normalise_loudness(y, SR)
    assert pyloudnorm.Meter(SR).integrated_loudness(out.astype(np.float64)) == pytest.approx(-20.0, abs=0.5)


def test_normalise_limits_peak():
    rng = np.random.default_rng(1)
    y = (0.05 * rng.standard_normal(3 * SR)).astype(np.float32)
    y[SR] = 0.7
    out = normalise_loudness(y, SR)
    assert np.max(np.abs(out)) <= 10 ** (-1 / 20) + 1e-4
    assert np.max(np.abs(out)) > 0.85  # the limit, not the target, was binding


def test_normalise_short_clip():
    y = tone(0.3, 0.02)
    out = normalise_loudness(y, SR)
    assert len(out) == len(y)
    assert np.max(np.abs(out)) > np.max(np.abs(y))


def test_normalise_silence_unchanged():
    y = np.zeros(SR, dtype=np.float32)
    assert normalise_loudness(y, SR).tobytes() == y.tobytes()

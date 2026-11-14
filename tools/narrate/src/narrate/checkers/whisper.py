"""Whisper transcription checker. Needs the `qwen` extra (torch, transformers) and a CUDA GPU."""

from __future__ import annotations

import gc

import numpy as np
import torch
from transformers import WhisperForConditionalGeneration, WhisperProcessor

MODEL = "openai/whisper-large-v3-turbo"
DEVICE = "cuda:0"
WHISPER_SR = 16000
LIMIT_SECS = 28.0
SPLIT_FROM_SECS = 15.0
RMS_FRAME_SECS = 0.01


def _resample(audio: np.ndarray, sr: int) -> np.ndarray:
    """Linear-interpolation resample to 16 kHz (adequate for speech recognition)."""
    y = np.asarray(audio, dtype=np.float32)
    if sr == WHISPER_SR or len(y) == 0:
        return y
    n = round(len(y) * WHISPER_SR / sr)
    x = np.arange(n) * (sr / WHISPER_SR)
    return np.interp(x, np.arange(len(y)), y).astype(np.float32)


def _quietest_cut(y: np.ndarray) -> int:
    """Sample index of the quietest 10 ms RMS frame between 15 s and the limit."""
    hop = round(RMS_FRAME_SECS * WHISPER_SR)
    a, b = int(SPLIT_FROM_SECS * WHISPER_SR) // hop, int(LIMIT_SECS * WHISPER_SR) // hop
    frames = y[: b * hop].reshape(b, hop).astype(np.float64)
    rms = np.sqrt((frames**2).mean(axis=1))
    return (a + int(np.argmin(rms[a:b]))) * hop


class WhisperChecker:
    """Whisper large-v3-turbo; the model loads on first use."""

    name = "whisper"

    def __init__(self) -> None:
        self._proc = None
        self._model = None

    def _load(self) -> None:
        if self._model is None:
            self._proc = WhisperProcessor.from_pretrained(MODEL)
            self._model = WhisperForConditionalGeneration.from_pretrained(MODEL, dtype=torch.float16).to(DEVICE).eval()

    def _transcribe_chunk(self, y: np.ndarray) -> str:
        features = self._proc(y, sampling_rate=WHISPER_SR, return_tensors="pt").input_features
        features = features.to(DEVICE, torch.float16)
        with torch.no_grad():
            ids = self._model.generate(features, language="english", task="transcribe")
        return self._proc.batch_decode(ids, skip_special_tokens=True)[0].strip()

    def _long(self, y: np.ndarray) -> str:
        if len(y) <= LIMIT_SECS * WHISPER_SR:
            return self._transcribe_chunk(y)
        cut = _quietest_cut(y)
        return self._transcribe_chunk(y[:cut]) + " " + self._long(y[cut:])

    def transcribe(self, audio: np.ndarray, sr: int) -> str:
        """What the audio says, splitting audio over 28 s at quiet points."""
        self._load()
        return self._long(_resample(audio, sr))

    def close(self) -> None:
        """Free the model."""
        self._proc = None
        self._model = None
        gc.collect()
        torch.cuda.empty_cache()

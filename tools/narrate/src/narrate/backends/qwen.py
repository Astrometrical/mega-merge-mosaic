"""Qwen3-TTS voice cloning. Needs the `qwen` extra, a CUDA GPU and model weights."""

from __future__ import annotations

import gc

import numpy as np
import torch
from qwen_tts import Qwen3TTSModel

from narrate.profile import Reference, VoiceProfile


class QwenBackend:
    """Clones a reference voice with Qwen3-TTS; the model loads on first use."""

    def __init__(self, profile: VoiceProfile) -> None:
        self.profile = profile
        self.sample_rate = 24000
        self._model = None
        self._prompts: dict[str, object] = {}

    def _load(self):
        if self._model is None:
            dtype = torch.float16 if self.profile.precision == "fp16" else torch.float32
            self._model = Qwen3TTSModel.from_pretrained(
                self.profile.model, device_map="cuda:0", dtype=dtype, attn_implementation="sdpa"
            )
        return self._model

    def synthesize(self, texts: list[str], reference: Reference, seed: int) -> list[np.ndarray]:
        """One mono float32 clip per text; non-finite samples become 0."""
        model = self._load()
        if reference.sha256 not in self._prompts:
            self._prompts[reference.sha256] = model.create_voice_clone_prompt(
                ref_audio=str(reference.audio), ref_text=reference.text
            )
        torch.manual_seed(seed)
        wavs, sr = model.generate_voice_clone(
            text=texts, language=self.profile.language, voice_clone_prompt=self._prompts[reference.sha256]
        )
        self.sample_rate = int(sr)
        return [np.nan_to_num(np.asarray(w, dtype=np.float32), nan=0.0, posinf=0.0, neginf=0.0) for w in wavs]

    def close(self) -> None:
        """Free the model and prompts."""
        self._model = None
        self._prompts = {}
        gc.collect()
        torch.cuda.empty_cache()

import os

import pytest

from narrate.profile import load_profile
from narrate.text import cer

pytestmark = [
    pytest.mark.gpu,
    pytest.mark.skipif(os.environ.get("NARRATE_GPU_TESTS") != "1", reason="set NARRATE_GPU_TESTS=1"),
]

TEXT = "Each filter is a group of its own."


def test_qwen_and_whisper_round_trip():
    import torch

    from narrate.backends.qwen import QwenBackend
    from narrate.checkers.whisper import WhisperChecker

    profile = load_profile(os.environ.get("NARRATE_GPU_VOICE", "dan"))
    backend = QwenBackend(profile)
    (audio,) = backend.synthesize([TEXT], profile.reference(profile.default_style, "line"), seed=1)
    loaded = torch.cuda.memory_allocated()
    backend.close()
    assert torch.cuda.memory_allocated() < loaded

    checker = WhisperChecker()
    heard = checker.transcribe(audio, backend.sample_rate)
    checker.close()
    assert cer(TEXT, heard) < 0.1

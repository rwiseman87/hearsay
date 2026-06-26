"""Silero VAD via onnxruntime (torch-free).

Runs the MIT-licensed Silero ONNX model directly on onnxruntime — no torch, no
silero-vad package (which hard-depends on torch). Interface verified against the
model: inputs ``input`` [1, 512] + ``state`` [2, 1, 128] + ``sr`` (int64), outputs
``output`` [1, 1] (speech prob) + new state. 512-sample frames at 16 kHz.
"""

from __future__ import annotations

import hashlib
import urllib.request
from collections.abc import Sequence
from pathlib import Path

from hearsay.log import get_logger

_log = get_logger("hearsay.vad.silero")

# Pinned source + integrity hash (supply-chain): the download is verified against
# this digest, so a moved/changed upstream file fails loudly instead of silently.
SILERO_URL = "https://raw.githubusercontent.com/snakers4/silero-vad/master/src/silero_vad/data/silero_vad.onnx"
SILERO_SHA256 = "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"
FRAME_SAMPLES = 512
# Silero prepends this many samples of prior audio as left-context to each frame.
CONTEXT_SAMPLES = 64


def download_silero_model(dest: Path) -> Path:
    """Download + verify the Silero ONNX model to ``dest`` (idempotent)."""
    if dest.exists() and hashlib.sha256(dest.read_bytes()).hexdigest() == SILERO_SHA256:
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    _log.info("downloading Silero VAD model to %s", dest)
    with urllib.request.urlopen(SILERO_URL, timeout=60) as response:
        data = response.read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != SILERO_SHA256:
        raise ValueError(f"Silero VAD checksum mismatch: got {digest}, expected {SILERO_SHA256}")
    dest.write_bytes(data)
    return dest


class SileroVAD:
    def __init__(self, model_path: Path, *, sample_rate: int = 16_000) -> None:
        import numpy as np  # noqa: PLC0415 (optional dep; only needed when VAD runs)
        import onnxruntime as ort  # noqa: PLC0415

        if not model_path.exists():
            raise FileNotFoundError(
                f"Silero VAD model not found at {model_path}; run `hearsay fetch-models`"
            )
        self._np = np
        self._session = ort.InferenceSession(str(model_path), providers=["CPUExecutionProvider"])
        self._sr = np.array(sample_rate, dtype=np.int64)
        self.reset()

    @property
    def frame_samples(self) -> int:
        return FRAME_SAMPLES

    def reset(self) -> None:
        self._state = self._np.zeros((2, 1, 128), dtype=self._np.float32)
        self._context = self._np.zeros((1, 0), dtype=self._np.float32)

    def speech_prob(self, frame: Sequence[float]) -> float:
        audio = self._np.asarray(frame, dtype=self._np.float32).reshape(1, -1)
        # Prepend the previous frame's trailing context; without this the model's
        # internal framing misaligns and every frame scores ~0 (verified on real speech).
        audio = self._np.concatenate([self._context, audio], axis=1)
        output, self._state = self._session.run(
            None, {"input": audio, "state": self._state, "sr": self._sr}
        )
        self._context = audio[:, -CONTEXT_SAMPLES:]
        return float(output.reshape(-1)[0])

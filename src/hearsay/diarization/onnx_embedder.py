"""Torch-free speaker-embedding backend: an ONNX model on onnxruntime.

Runs a wespeaker/3D-Speaker-style embedder (fbank ``[1, T, 80]`` in, ``[1, dim]`` out)
on the CPU execution provider -- the same onnxruntime that runs Silero VAD. No torch,
no gated weights. The model is loaded once per meeting and reused for every utterance.
"""

from __future__ import annotations

from collections.abc import Sequence
from pathlib import Path
from typing import TYPE_CHECKING

from hearsay.diarization.features import compute_fbank

if TYPE_CHECKING:
    import numpy as np


class OnnxSpeakerEmbedder:
    def __init__(self, model_path: Path, *, model_id: str | None = None) -> None:
        import numpy as np  # noqa: PLC0415 (optional dep; only needed when diarizing)
        import onnxruntime as ort  # noqa: PLC0415

        if not model_path.exists():
            raise FileNotFoundError(
                f"speaker-embedding model not found at {model_path}; run `hearsay fetch-models`"
            )
        self._np = np
        self._session = ort.InferenceSession(str(model_path), providers=["CPUExecutionProvider"])
        self._input = self._session.get_inputs()[0].name
        out_shape = self._session.get_outputs()[0].shape
        self._dim = int(out_shape[-1]) if isinstance(out_shape[-1], int) else 0
        self._model_id = model_id or model_path.stem

    @property
    def dim(self) -> int:
        return self._dim

    @property
    def model_id(self) -> str:
        return self._model_id

    def embed(self, samples: Sequence[float]) -> np.ndarray:
        feats = compute_fbank(samples)
        if feats.shape[0] == 0:
            return self._np.zeros(self._dim, dtype=self._np.float32)
        out = self._session.run(None, {self._input: feats[None]})[0][0]
        norm = float(self._np.linalg.norm(out))
        embedding: np.ndarray = out / norm if norm > 0.0 else out
        return embedding

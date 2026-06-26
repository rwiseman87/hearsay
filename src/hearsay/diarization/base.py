"""The speaker-embedding seam.

A :class:`SpeakerEmbedder` maps one utterance's 16 kHz mono samples to a fixed-length,
L2-normalized voiceprint. The fusion engine compares these by cosine similarity to
cluster the Them stream into speakers and to recognize people across meetings. The real
ONNX backend lives behind this protocol so the pure clustering logic can be unit-tested
with a deterministic stub.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import TYPE_CHECKING, Protocol, runtime_checkable

if TYPE_CHECKING:
    import numpy as np


@runtime_checkable
class SpeakerEmbedder(Protocol):
    @property
    def dim(self) -> int:
        """Embedding dimensionality (e.g. 512 for wespeaker CAM++)."""
        ...

    @property
    def model_id(self) -> str:
        """Stable id of the model; tags stored centroids so a model change is detected."""
        ...

    def embed(self, samples: Sequence[float]) -> np.ndarray:
        """Return the L2-normalized embedding of ``samples`` (16 kHz mono, ``dim`` long)."""
        ...

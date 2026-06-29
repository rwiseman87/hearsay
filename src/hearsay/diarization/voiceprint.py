"""Voiceprint (speaker-embedding centroid) serialization + cross-meeting matching.

A centroid is a fixed-length, L2-normalized float vector. It is stored on a cluster as
raw float32 bytes (``clusters.centroid``); once that cluster is named and locked it
becomes a recognizable voiceprint for the bound person, so a later meeting can match a
new speaker to it by cosine similarity. Pure stdlib (array + math) -- fully unit-testable.
"""

from __future__ import annotations

import math
from array import array
from collections.abc import Sequence


def centroid_to_bytes(centroid: Sequence[float]) -> bytes:
    """Serialize a centroid as little-endian float32 bytes for ``clusters.centroid``."""
    return array("f", (float(value) for value in centroid)).tobytes()


def centroid_from_bytes(data: bytes) -> list[float]:
    """Inverse of :func:`centroid_to_bytes`."""
    vec = array("f")
    vec.frombytes(data)
    return list(vec)


def _cosine(a: Sequence[float], b: Sequence[float]) -> float:
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(x * x for x in b))
    if na == 0.0 or nb == 0.0:
        return 0.0
    return sum(x * y for x, y in zip(a, b, strict=True)) / (na * nb)


def match_identity(
    centroid: Sequence[float],
    known: Sequence[tuple[str, Sequence[float]]],
    *,
    threshold: float,
) -> str | None:
    """Name of the known voiceprint most similar to ``centroid`` (cosine >= ``threshold``).

    ``None`` if nothing clears the threshold. Voiceprints of a different length (a model
    change) are skipped rather than compared.
    """
    best_name: str | None = None
    best_score = -1.0
    for name, vector in known:
        if len(vector) != len(centroid):
            continue
        score = _cosine(centroid, vector)
        if score >= threshold and score > best_score:
            best_score = score
            best_name = name
    return best_name

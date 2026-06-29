"""Online speaker clustering: group utterance voiceprints into stable speakers.

Pure stdlib logic (no numpy, no I/O) so it is dependency-free and exhaustively
unit-testable. Each Them utterance's embedding is matched to the nearest existing
speaker by cosine similarity to that speaker's running centroid; a match at or above
``threshold`` joins it (updating the centroid), otherwise a new speaker is created.
"Speaker N" ordinals are assigned in first-appearance order. A manual label locks a
speaker's identity; cross-meeting voiceprints can pre-seed a speaker so a returning
person is recognized (provisionally) on their first utterance.

Embeddings are plain ``Sequence[float]`` (the ONNX embedder's numpy vectors satisfy
this), kept as a duration-weighted running sum so the centroid is the weighted mean
direction -- longer utterances carry more signal and a short noisy clip cannot tilt a
speaker's voiceprint as much as a long clean one.
"""

from __future__ import annotations

import math
from collections.abc import Sequence
from dataclasses import dataclass, field


def _dot(a: Sequence[float], b: Sequence[float]) -> float:
    return sum((x * y for x, y in zip(a, b, strict=True)), 0.0)


def _norm(a: Sequence[float]) -> float:
    return math.sqrt(sum((x * x for x in a), 0.0))


def _cosine(a: Sequence[float], b: Sequence[float]) -> float:
    na, nb = _norm(a), _norm(b)
    return _dot(a, b) / (na * nb) if na > 0.0 and nb > 0.0 else 0.0


def _unit(a: Sequence[float]) -> list[float]:
    n = _norm(a)
    return [x / n for x in a] if n > 0.0 else list(a)


@dataclass
class Speaker:
    """One clustered speaker on the Them stream."""

    ordinal: int  # first-appearance number rendered as "Speaker N"
    count: int  # utterances merged so far
    sum_vec: list[float]  # running sum of member embeddings (centroid = its unit vector)
    identity_key: str | None = None  # bound/suggested cross-meeting identity, if any
    locked: bool = False  # a manual label locks the identity against later overrides

    @property
    def centroid(self) -> list[float]:
        return _unit(self.sum_vec)


@dataclass(frozen=True, slots=True)
class Seed:
    """A known person's stored voiceprint, used to recognize them across meetings."""

    identity_key: str
    centroid: list[float]


@dataclass(frozen=True, slots=True)
class Assignment:
    """Result of assigning one utterance: which speaker, and how confident."""

    ordinal: int
    identity_key: str | None
    is_new: bool
    similarity: float


@dataclass
class OnlineSpeakerClusterer:
    """Incremental cosine clusterer over the Them stream (see module docstring)."""

    threshold: float = 0.5
    _speakers: list[Speaker] = field(default_factory=list, init=False)
    _seeds: list[Seed] = field(default_factory=list, init=False)
    _next_ordinal: int = field(default=1, init=False)

    @property
    def speakers(self) -> list[Speaker]:
        return list(self._speakers)

    def add_seed(self, identity_key: str, centroid: Sequence[float]) -> None:
        """Register a known voiceprint so a returning speaker is recognized on first speech."""
        self._seeds.append(Seed(identity_key=identity_key, centroid=_unit(centroid)))

    def assign(self, embedding: Sequence[float], *, weight: float = 1.0) -> Assignment:
        # ``weight`` (the utterance duration in seconds) scales this embedding's pull on
        # the centroid, so a long clean turn outweighs a short noisy one.
        # 1. Nearest established speaker wins if it clears the threshold.
        index, similarity = self._nearest(embedding, [s.centroid for s in self._speakers])
        if index >= 0 and similarity >= self.threshold:
            speaker = self._speakers[index]
            for i, value in enumerate(embedding):
                speaker.sum_vec[i] += weight * value
            speaker.count += 1
            return Assignment(speaker.ordinal, speaker.identity_key, False, similarity)

        # 2. Otherwise, a known voiceprint from a prior meeting provisionally names the new speaker.
        seed_index, seed_similarity = self._nearest(embedding, [s.centroid for s in self._seeds])
        identity = (
            self._seeds[seed_index].identity_key
            if seed_index >= 0 and seed_similarity >= self.threshold
            else None
        )
        speaker = Speaker(
            ordinal=self._next_ordinal,
            count=1,
            sum_vec=[weight * value for value in embedding],
            identity_key=identity,
        )
        self._next_ordinal += 1
        self._speakers.append(speaker)
        return Assignment(speaker.ordinal, identity, True, similarity)

    def bind(self, ordinal: int, identity_key: str) -> bool:
        """Manually label a speaker; locks the binding so later hints cannot override it."""
        for speaker in self._speakers:
            if speaker.ordinal == ordinal:
                speaker.identity_key = identity_key
                speaker.locked = True
                return True
        return False

    def _nearest(
        self, embedding: Sequence[float], centroids: list[list[float]]
    ) -> tuple[int, float]:
        best_index, best = -1, -1.0
        for i, centroid in enumerate(centroids):
            similarity = _cosine(embedding, centroid)
            if similarity > best:
                best_index, best = i, similarity
        return best_index, best

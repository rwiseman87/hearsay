"""Voiceprint serialization + cross-meeting matching (pure stdlib)."""

from __future__ import annotations

from hearsay.diarization import centroid_from_bytes, centroid_to_bytes, match_identity


def test_centroid_roundtrips() -> None:
    vec = [1.0, -0.5, 0.25, 0.0]  # all exactly representable in float32
    assert centroid_from_bytes(centroid_to_bytes(vec)) == vec


def test_match_identity_picks_best_above_threshold() -> None:
    known = [("Alice", [1.0, 0.0]), ("Bob", [0.0, 1.0])]
    assert match_identity([0.9, 0.1], known, threshold=0.6) == "Alice"
    assert match_identity([0.1, 0.9], known, threshold=0.6) == "Bob"


def test_match_identity_none_below_threshold_or_empty() -> None:
    known = [("Alice", [1.0, 0.0])]
    assert match_identity([0.0, 1.0], known, threshold=0.6) is None  # orthogonal -> cosine 0
    assert match_identity([1.0, 0.0], [], threshold=0.6) is None  # nobody known yet


def test_match_identity_skips_dim_mismatch() -> None:
    known = [("Alice", [1.0, 0.0, 0.0])]  # 3-d voiceprint from a different model
    assert match_identity([1.0, 0.0], known, threshold=0.6) is None

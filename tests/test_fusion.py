"""Online speaker clustering -- pure logic, exhaustively tested with synthetic vectors.

No heavy deps (the clusterer is stdlib-only), so these run everywhere. Vectors are 3-D
so cosine similarities are easy to reason about:
    A vs NEAR_A ~= 0.99   A vs MID_A ~= 0.70   A vs FAR_A ~= 0.30   A vs B/C = 0.0
"""

from __future__ import annotations

from hearsay.fusion import OnlineSpeakerClusterer

A = [1.0, 0.0, 0.0]
B = [0.0, 1.0, 0.0]
C = [0.0, 0.0, 1.0]
NEAR_A = [0.9, 0.1, 0.0]
MID_A = [0.7, 0.714, 0.0]
FAR_A = [0.3, 0.954, 0.0]


def test_distinct_voices_make_distinct_speakers() -> None:
    clusterer = OnlineSpeakerClusterer(threshold=0.5)
    for vector in (A, B, C):
        clusterer.assign(vector)
    assert [s.ordinal for s in clusterer.speakers] == [1, 2, 3]
    assert all(s.count == 1 for s in clusterer.speakers)


def test_same_voice_joins_one_speaker() -> None:
    clusterer = OnlineSpeakerClusterer(threshold=0.5)
    first = clusterer.assign(A)
    second = clusterer.assign(NEAR_A)
    assert first.is_new and not second.is_new
    assert second.ordinal == 1
    assert len(clusterer.speakers) == 1
    assert clusterer.speakers[0].count == 2


def test_below_threshold_starts_new_speaker() -> None:
    clusterer = OnlineSpeakerClusterer(threshold=0.5)
    clusterer.assign(A)
    result = clusterer.assign(FAR_A)  # cos ~0.30 < 0.5
    assert result.is_new and result.ordinal == 2
    assert len(clusterer.speakers) == 2


def test_ordinals_follow_first_appearance() -> None:
    clusterer = OnlineSpeakerClusterer(threshold=0.5)
    clusterer.assign(B)  # ordinal 1
    clusterer.assign(A)  # ordinal 2
    result = clusterer.assign(NEAR_A)  # joins ordinal 2
    assert result.ordinal == 2 and not result.is_new
    assert [s.ordinal for s in clusterer.speakers] == [1, 2]


def test_centroid_tracks_member_mean() -> None:
    clusterer = OnlineSpeakerClusterer(threshold=0.5)
    clusterer.assign(A)
    clusterer.assign(NEAR_A)
    centroid = clusterer.speakers[0].centroid
    assert centroid[0] > 0.9 and 0.0 < centroid[1] < 0.2
    assert abs(sum(x * x for x in centroid) - 1.0) < 1e-9  # unit-normalized


def test_threshold_is_configurable() -> None:
    strict = OnlineSpeakerClusterer(threshold=0.8)
    strict.assign(A)
    assert strict.assign(MID_A).is_new  # cos ~0.70 < 0.8 -> new speaker
    assert len(strict.speakers) == 2

    loose = OnlineSpeakerClusterer(threshold=0.5)
    loose.assign(A)
    assert not loose.assign(MID_A).is_new  # cos ~0.70 >= 0.5 -> joins
    assert len(loose.speakers) == 1


def test_seed_provisionally_names_returning_speaker() -> None:
    clusterer = OnlineSpeakerClusterer(threshold=0.5)
    clusterer.add_seed("alice", A)
    first = clusterer.assign(NEAR_A)
    assert first.is_new and first.identity_key == "alice"  # recognized from memory
    second = clusterer.assign(NEAR_A)
    assert not second.is_new
    assert second.ordinal == first.ordinal and second.identity_key == "alice"


def test_unmatched_seed_leaves_speaker_unnamed() -> None:
    clusterer = OnlineSpeakerClusterer(threshold=0.5)
    clusterer.add_seed("alice", A)
    assert clusterer.assign(B).identity_key is None  # different voice


def test_active_speaker_wins_over_seed() -> None:
    clusterer = OnlineSpeakerClusterer(threshold=0.5)
    clusterer.add_seed("alice", A)
    clusterer.assign(NEAR_A)  # speaker 1, provisionally alice
    clusterer.assign(A)  # same voice -> joins speaker 1, not a seed-spawned duplicate
    assert len(clusterer.speakers) == 1
    assert clusterer.speakers[0].count == 2


def test_bind_locks_identity() -> None:
    clusterer = OnlineSpeakerClusterer(threshold=0.5)
    clusterer.assign(A)  # speaker ordinal 1
    assert clusterer.bind(1, "bob") is True
    assert clusterer.speakers[0].locked and clusterer.speakers[0].identity_key == "bob"
    result = clusterer.assign(NEAR_A)  # same voice joins
    assert not result.is_new and result.identity_key == "bob"


def test_bind_unknown_ordinal_returns_false() -> None:
    clusterer = OnlineSpeakerClusterer(threshold=0.5)
    assert clusterer.bind(99, "bob") is False

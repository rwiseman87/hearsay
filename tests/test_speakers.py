from __future__ import annotations

import uuid

import pytest
from sqlalchemy import delete, select
from sqlalchemy.exc import IntegrityError
from sqlalchemy.ext.asyncio import AsyncSession

from hearsay.enums import Stream
from hearsay.models import Cluster, Identity, Meeting, Segment
from hearsay.services import SpeakerService


async def _meeting(session: AsyncSession) -> Meeting:
    meeting = Meeting(title="Sync", folder="f", storage_root="/out")
    session.add(meeting)
    await session.commit()
    return meeting


async def _them_segment(session: AsyncSession, meeting_id: uuid.UUID) -> Segment:
    seg = Segment(
        meeting_id=meeting_id,
        stream=Stream.THEM,
        speaker_label="Speaker 1",
        text="hi",
        start_s=0.0,
        end_s=1.0,
    )
    session.add(seg)
    await session.commit()
    return seg


async def test_create_and_list_clusters_ordered_by_ordinal(session: AsyncSession) -> None:
    meeting = await _meeting(session)
    svc = SpeakerService(session)
    await svc.create_cluster(meeting.id, ordinal=2)
    await svc.create_cluster(meeting.id, ordinal=1)

    clusters = await svc.list_clusters(meeting.id)
    assert [c.ordinal for c in clusters] == [1, 2]
    assert all(c.identity_id is None and not c.locked for c in clusters)


async def test_ordinal_unique_per_meeting(session: AsyncSession) -> None:
    meeting = await _meeting(session)
    svc = SpeakerService(session)
    await svc.create_cluster(meeting.id, ordinal=1)
    with pytest.raises(IntegrityError):
        await svc.create_cluster(meeting.id, ordinal=1)


async def test_assign_segment_to_cluster(session: AsyncSession) -> None:
    meeting = await _meeting(session)
    seg = await _them_segment(session, meeting.id)
    svc = SpeakerService(session)
    cluster = await svc.create_cluster(meeting.id, ordinal=1)

    await svc.assign_segment_cluster(seg.id, cluster.id)
    await session.refresh(seg)
    assert seg.cluster_id == cluster.id


async def test_bind_cluster_creates_then_reuses_identity_by_name(session: AsyncSession) -> None:
    meeting = await _meeting(session)
    svc = SpeakerService(session)
    c1 = await svc.create_cluster(meeting.id, ordinal=1)

    bound = await svc.bind_cluster(c1.id, display_name="  Alice  ")
    assert bound is not None and bound.locked
    identity = await session.get(Identity, bound.identity_id)
    assert identity is not None and identity.display_name == "Alice"  # trimmed

    # A second cluster bound to the same name reuses the one identity (cross-meeting memory).
    c2 = await svc.create_cluster(meeting.id, ordinal=2)
    bound2 = await svc.bind_cluster(c2.id, display_name="Alice")
    assert bound2 is not None and bound2.identity_id == identity.id

    identities, total = await svc.list_identities(page=1, page_size=10)
    assert total == 1 and [i.display_name for i in identities] == ["Alice"]


async def test_bind_unknown_cluster_returns_none(session: AsyncSession) -> None:
    svc = SpeakerService(session)
    assert await svc.bind_cluster(uuid.uuid4(), display_name="X") is None


async def test_delete_meeting_cascades_clusters(session: AsyncSession) -> None:
    meeting = await _meeting(session)
    svc = SpeakerService(session)
    await svc.create_cluster(meeting.id, ordinal=1)

    # Core DELETE exercises the DB-level ON DELETE CASCADE (PRAGMA foreign_keys=ON).
    await session.execute(delete(Meeting).where(Meeting.id == meeting.id))
    await session.commit()
    assert (await session.scalars(select(Cluster))).all() == []


async def test_bind_cluster_relabels_its_segments(session: AsyncSession) -> None:
    meeting = await _meeting(session)
    svc = SpeakerService(session)
    cluster = await svc.create_cluster(meeting.id, ordinal=1)
    other = await svc.create_cluster(meeting.id, ordinal=2)
    s1 = Segment(
        meeting_id=meeting.id,
        stream=Stream.THEM,
        speaker_label="Speaker 1",
        text="a",
        start_s=0.0,
        end_s=1.0,
        cluster_id=cluster.id,
    )
    s2 = Segment(
        meeting_id=meeting.id,
        stream=Stream.THEM,
        speaker_label="Speaker 1",
        text="b",
        start_s=1.0,
        end_s=2.0,
        cluster_id=cluster.id,
    )
    s3 = Segment(
        meeting_id=meeting.id,
        stream=Stream.THEM,
        speaker_label="Speaker 2",
        text="c",
        start_s=2.0,
        end_s=3.0,
        cluster_id=other.id,
    )
    session.add_all([s1, s2, s3])
    await session.commit()

    bound = await svc.bind_cluster(cluster.id, display_name="Alice")
    assert bound is not None and bound.identity is not None
    assert bound.identity.display_name == "Alice"  # eager-loaded for the response

    for segment in (s1, s2, s3):
        await session.refresh(segment)
    assert s1.speaker_label == "Alice" and s2.speaker_label == "Alice"
    assert s3.speaker_label == "Speaker 2"  # a different cluster's segments are untouched


async def test_delete_cluster_nulls_segment_fk(session: AsyncSession) -> None:
    meeting = await _meeting(session)
    seg = await _them_segment(session, meeting.id)
    svc = SpeakerService(session)
    cluster = await svc.create_cluster(meeting.id, ordinal=1)
    await svc.assign_segment_cluster(seg.id, cluster.id)

    # ON DELETE SET NULL keeps the segment but clears its cluster reference.
    await session.execute(delete(Cluster).where(Cluster.id == cluster.id))
    await session.commit()
    await session.refresh(seg)
    assert seg.cluster_id is None

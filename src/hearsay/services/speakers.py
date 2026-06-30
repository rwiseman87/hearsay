"""Diarization clusters + cross-meeting identities.

Routers and the fusion engine stay thin and delegate here. Each method is a single
logical write; ``bind`` is a two-step write (get-or-create identity, then point the
cluster at it) wrapped in one commit.
"""

from __future__ import annotations

from dataclasses import dataclass
from uuid import UUID

from sqlalchemy import delete, func, select, update
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import selectinload

from hearsay.enums import Stream
from hearsay.models import Cluster, Identity, Segment


@dataclass(frozen=True, slots=True)
class TurnSegment:
    """One diarizer turn to persist as a Them segment: speaker ordinal + re-transcribed text."""

    ordinal: int
    text: str
    start_s: float
    end_s: float


class SpeakerService:
    def __init__(self, session: AsyncSession) -> None:
        self._session = session

    async def create_cluster(self, meeting_id: UUID, *, ordinal: int) -> Cluster:
        cluster = Cluster(meeting_id=meeting_id, ordinal=ordinal)
        self._session.add(cluster)
        await self._session.commit()
        await self._session.refresh(cluster)
        return cluster

    async def get_cluster(self, cluster_id: UUID) -> Cluster | None:
        return await self._session.get(Cluster, cluster_id)

    async def list_clusters(self, meeting_id: UUID) -> list[Cluster]:
        rows = await self._session.scalars(
            select(Cluster)
            .where(Cluster.meeting_id == meeting_id)
            .options(selectinload(Cluster.identity))
            .order_by(Cluster.ordinal)
        )
        return list(rows)

    async def assign_segment_cluster(self, segment_id: UUID, cluster_id: UUID) -> None:
        """Point a segment at its resolved cluster (fusion writes this as turns settle)."""
        await self._session.execute(
            update(Segment).where(Segment.id == segment_id).values(cluster_id=cluster_id)
        )
        await self._session.commit()

    async def _get_or_create_identity(self, display_name: str) -> Identity:
        name = display_name.strip()
        identity = await self._session.scalar(select(Identity).where(Identity.display_name == name))
        if identity is None:
            identity = Identity(display_name=name)
            self._session.add(identity)
            await self._session.flush()  # assign identity.id without ending the transaction
        return identity

    async def bind_cluster(self, cluster_id: UUID, *, display_name: str) -> Cluster | None:
        """Rename a cluster to a person: get-or-create the identity, lock the binding."""
        cluster = await self.get_cluster(cluster_id)
        if cluster is None:
            return None
        name = display_name.strip()
        identity = await self._get_or_create_identity(name)
        cluster.identity_id = identity.id
        cluster.locked = True
        # Retroactively relabel this speaker's already-saved segments (one bulk write).
        await self._session.execute(
            update(Segment).where(Segment.cluster_id == cluster_id).values(speaker_label=name)
        )
        await self._session.commit()
        await self._session.refresh(cluster, ["identity"])
        return cluster

    async def known_voiceprints(self, *, exclude_meeting_id: UUID) -> list[tuple[str, bytes]]:
        """(name, centroid) for every person named + locked in another meeting with a stored
        voiceprint -- the candidates a refine matches a returning speaker against."""
        rows = await self._session.execute(
            select(Identity.display_name, Cluster.centroid)
            .join(Cluster, Cluster.identity_id == Identity.id)
            .where(
                Cluster.locked.is_(True),
                Cluster.centroid.is_not(None),
                Cluster.meeting_id != exclude_meeting_id,
            )
        )
        return [(name, centroid) for name, centroid in rows if centroid is not None]

    async def apply_turn_diarization(
        self,
        meeting_id: UUID,
        *,
        turn_segments: list[TurnSegment],
        speaker_count: int,
        ordinal_names: dict[int, str] | None = None,
        ordinal_centroids: dict[int, bytes] | None = None,
        recognized: dict[int, str] | None = None,
    ) -> None:
        """Rebuild a meeting's Them transcript from diarizer turns (one transaction): drop the
        old VAD-segmented Them segments + clusters, create a fresh "Speaker 1..N" set, and
        insert one segment per turn (Me segments untouched). Used by the post-meeting refine --
        the turns follow speaker changes that the VAD's silence-based utterances merged.

        ``ordinal_names`` carries manual renames forward (locked, so a re-diarize never
        overrides a manual binding -- a guardrail). ``recognized`` auto-names a speaker whose
        voiceprint matched a person from a prior meeting, bound but *not* locked (a manual
        rename can still override). ``ordinal_centroids`` stores each speaker's voiceprint so
        a later meeting can recognize them. Precedence: manual name > recognized > "Speaker N"."""
        names = ordinal_names or {}
        autos = {o: n for o, n in (recognized or {}).items() if o not in names}
        centroids = ordinal_centroids or {}
        await self._session.execute(
            delete(Segment).where(Segment.meeting_id == meeting_id, Segment.stream == Stream.THEM)
        )
        await self._session.execute(delete(Cluster).where(Cluster.meeting_id == meeting_id))
        clusters = {
            n: Cluster(meeting_id=meeting_id, ordinal=n, centroid=centroids.get(n))
            for n in range(1, speaker_count + 1)
        }
        self._session.add_all(clusters.values())
        await self._session.flush()  # assign cluster ids without ending the transaction
        for named_ordinal, name in names.items():
            identity = await self._get_or_create_identity(name)
            clusters[named_ordinal].identity_id = identity.id
            clusters[named_ordinal].locked = True
        for auto_ordinal, name in autos.items():
            identity = await self._get_or_create_identity(name)
            clusters[auto_ordinal].identity_id = identity.id  # provisional: leave unlocked
        await self._session.flush()
        for turn in turn_segments:
            label = names.get(turn.ordinal) or autos.get(turn.ordinal) or f"Speaker {turn.ordinal}"
            self._session.add(
                Segment(
                    meeting_id=meeting_id,
                    stream=Stream.THEM,
                    speaker_label=label,
                    text=turn.text,
                    start_s=turn.start_s,
                    end_s=turn.end_s,
                    cluster_id=clusters[turn.ordinal].id,
                )
            )
        await self._session.commit()

    async def list_identities(self, *, page: int, page_size: int) -> tuple[list[Identity], int]:
        """Known people, most-recently-updated first (rename suggestions for next meeting)."""
        total = await self._session.scalar(select(func.count()).select_from(Identity)) or 0
        rows = await self._session.scalars(
            select(Identity)
            .order_by(Identity.updated_at.desc())
            .offset((page - 1) * page_size)
            .limit(page_size)
        )
        return list(rows), total

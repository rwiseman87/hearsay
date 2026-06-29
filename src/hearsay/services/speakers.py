"""Diarization clusters + cross-meeting identities.

Routers and the fusion engine stay thin and delegate here. Each method is a single
logical write; ``bind`` is a two-step write (get-or-create identity, then point the
cluster at it) wrapped in one commit.
"""

from __future__ import annotations

from uuid import UUID

from sqlalchemy import delete, func, select, update
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import selectinload

from hearsay.models import Cluster, Identity, Segment


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

    async def apply_diarization(
        self,
        meeting_id: UUID,
        *,
        segment_ordinals: dict[UUID, int | None],
        speaker_count: int,
        ordinal_names: dict[int, str] | None = None,
    ) -> None:
        """Replace a meeting's clusters with a fresh "Speaker 1..N" set and relabel its Them
        segments (one transaction). Used by the post-meeting pyannote refine. ``ordinal_names``
        carries manual renames forward: a named ordinal keeps that locked identity + label
        instead of "Speaker N", so a re-diarize never overrides a manual binding (a guardrail)."""
        names = ordinal_names or {}
        await self._session.execute(delete(Cluster).where(Cluster.meeting_id == meeting_id))
        clusters = {
            n: Cluster(meeting_id=meeting_id, ordinal=n) for n in range(1, speaker_count + 1)
        }
        self._session.add_all(clusters.values())
        await self._session.flush()  # assign cluster ids without ending the transaction
        for named_ordinal, name in names.items():
            identity = await self._get_or_create_identity(name)
            clusters[named_ordinal].identity_id = identity.id
            clusters[named_ordinal].locked = True
        await self._session.flush()
        for segment_id, ordinal in segment_ordinals.items():
            values: dict[str, str | UUID | None]
            if ordinal is None:
                values = {"speaker_label": "Them", "cluster_id": None}
            else:
                values = {
                    "speaker_label": names.get(ordinal) or f"Speaker {ordinal}",
                    "cluster_id": clusters[ordinal].id,
                }
            await self._session.execute(
                update(Segment).where(Segment.id == segment_id).values(**values)
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

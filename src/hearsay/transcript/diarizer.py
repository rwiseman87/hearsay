"""Per-meeting diarization: embed each Them utterance and cluster it into a speaker.

Wraps the speaker embedder + online clusterer + cluster-row persistence behind one
async :meth:`resolve` call the pipeline makes for finalized Them utterances. Me is
never diarized (the pipeline keeps the channel label). One instance per meeting holds
the clusterer state and the ordinal -> DB cluster-id mapping.
"""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING
from uuid import UUID

from hearsay.db import Database
from hearsay.fusion import OnlineSpeakerClusterer
from hearsay.services import SpeakerService
from hearsay.vad import Utterance

if TYPE_CHECKING:
    from hearsay.diarization import SpeakerEmbedder


class MeetingDiarizer:
    def __init__(
        self,
        *,
        meeting_id: UUID,
        database: Database,
        embedder: SpeakerEmbedder,
        threshold: float = 0.5,
        min_embed_ms: int = 500,
    ) -> None:
        self._meeting_id = meeting_id
        self._db = database
        self._embedder = embedder
        self._min_embed_ms = min_embed_ms
        self._clusterer = OnlineSpeakerClusterer(threshold=threshold)
        self._cluster_ids: dict[int, UUID] = {}

    async def resolve(self, utterance: Utterance) -> tuple[str, UUID | None]:
        """Embed + cluster a finalized Them utterance -> (display label, cluster id)."""
        duration_ms = (utterance.end_s - utterance.start_s) * 1000.0
        if duration_ms < self._min_embed_ms:
            return "Them", None  # too short to attribute a speaker reliably
        embedding = await asyncio.to_thread(self._embedder.embed, utterance.samples)
        # The clusterer is numpy-free; hand it plain floats (works for ndarray or list).
        assignment = self._clusterer.assign([float(value) for value in embedding])
        cluster_id = await self._cluster_id_for(assignment.ordinal)
        label = assignment.identity_key or f"Speaker {assignment.ordinal}"
        return label, cluster_id

    async def _cluster_id_for(self, ordinal: int) -> UUID:
        existing = self._cluster_ids.get(ordinal)
        if existing is not None:
            return existing
        async with self._db.session() as session:
            cluster = await SpeakerService(session).create_cluster(
                self._meeting_id, ordinal=ordinal
            )
        self._cluster_ids[ordinal] = cluster.id
        return cluster.id

"""Live "Them" processing via the Swift ``hearsay-live`` sidecar.

The sidecar (FluidAudio streaming diarization + Parakeet on the ANE) does the diarization,
ASR, and turn assembly; this just streams the Them PCM in and persists + broadcasts the
labeled segments it emits -- no VAD, no online clustering, no fusion. The post-meeting refine
still re-diarizes the whole track for final accuracy + cross-meeting recognition.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any
from uuid import UUID

from hearsay.db import Database
from hearsay.enums import Stream
from hearsay.export import TranscriptLine, TranscriptSink
from hearsay.schemas import TranscriptEvent
from hearsay.services import MeetingService, SpeakerService
from hearsay.transcript.broadcast import Broadcaster
from hearsay.transcript.live_base import LiveSidecarProcessor


class LiveThemProcessor(LiveSidecarProcessor):
    """Owns the ``hearsay-live`` sidecar for a meeting: feed Them audio, persist its turns."""

    def __init__(
        self,
        *,
        binary_path: Path,
        meeting_id: UUID,
        database: Database,
        broadcaster: Broadcaster,
        sink: TranscriptSink,
    ) -> None:
        super().__init__(
            binary_path=binary_path,
            meeting_id=meeting_id,
            database=database,
            broadcaster=broadcaster,
            sink=sink,
        )
        self._cluster_ids: dict[int, UUID] = {}

    async def _handle(self, seg: dict[str, Any]) -> None:
        offset = self._offset_s or 0.0
        ordinal = int(seg["speaker"]) + 1  # sidecar speakers are 0-based
        label = f"Speaker {ordinal}"
        text = str(seg["text"])
        start_s = float(seg["start_s"]) + offset
        end_s = float(seg["end_s"]) + offset
        cluster_id = await self._cluster_for(ordinal)
        async with self._db.session() as session:
            await MeetingService(session).add_segment(
                self._meeting_id,
                stream=Stream.THEM,
                speaker_label=label,
                text=text,
                start_s=start_s,
                end_s=end_s,
                cluster_id=cluster_id,
            )
        await self._sink.append(
            TranscriptLine(speaker_label=label, text=text, start_s=start_s, end_s=end_s)
        )
        self._broadcaster.publish(
            TranscriptEvent(
                kind="final",
                stream=Stream.THEM,
                speaker_label=label,
                text=text,
                start_s=start_s,
                end_s=end_s,
            ).model_dump_json()
        )

    async def _cluster_for(self, ordinal: int) -> UUID:
        existing = self._cluster_ids.get(ordinal)
        if existing is not None:
            return existing
        async with self._db.session() as session:
            cluster = await SpeakerService(session).create_cluster(
                self._meeting_id, ordinal=ordinal
            )
        self._cluster_ids[ordinal] = cluster.id
        return cluster.id

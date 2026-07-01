"""Live "Me" processing via the Swift ``hearsay-me`` sidecar.

The sidecar (FluidAudio streaming VAD + streaming Parakeet on the ANE) does the VAD + ASR,
emitting growing ``partial`` transcripts as you speak and a ``final`` when the utterance closes.
This forwards both to the UI over the WebSocket; only finals are persisted + appended to
``transcript.md``. Me is always the local speaker, so there is no diarization.
"""

from __future__ import annotations

from typing import Any

from hearsay.enums import Stream
from hearsay.export import TranscriptLine
from hearsay.schemas import TranscriptEvent
from hearsay.services import MeetingService
from hearsay.transcript.live_base import LiveSidecarProcessor


class LiveMeProcessor(LiveSidecarProcessor):
    """Owns the ``hearsay-me`` sidecar for a meeting: feed Me audio, stream its transcripts."""

    async def _handle(self, seg: dict[str, Any]) -> None:
        kind = "partial" if str(seg.get("kind", "final")) == "partial" else "final"
        offset = self._offset_s or 0.0
        text = str(seg["text"])
        start_s = float(seg["start_s"]) + offset
        end_s = float(seg["end_s"]) + offset
        # Partials stream to the UI only (the frontend supersedes them with the final); finals
        # are also persisted and appended to transcript.md.
        self._broadcaster.publish(
            TranscriptEvent(
                kind=kind,
                stream=Stream.ME,
                speaker_label="Me",
                text=text,
                start_s=start_s,
                end_s=end_s,
            ).model_dump_json()
        )
        if kind == "partial":
            return
        async with self._db.session() as session:
            await MeetingService(session).add_segment(
                self._meeting_id,
                stream=Stream.ME,
                speaker_label="Me",
                text=text,
                start_s=start_s,
                end_s=end_s,
                cluster_id=None,
            )
        await self._sink.append(
            TranscriptLine(speaker_label="Me", text=text, start_s=start_s, end_s=end_s)
        )

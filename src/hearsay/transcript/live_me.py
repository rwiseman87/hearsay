"""Live "Me" processing via the Swift ``hearsay-me`` sidecar.

The sidecar (FluidAudio streaming VAD + Parakeet on the ANE) does the VAD + ASR; this just
streams the local-mic PCM in and persists + broadcasts the utterances it emits -- no Silero,
no Python ASR in the live path. Me is always the local speaker, so there is no diarization.
"""

from __future__ import annotations

from typing import Any

from hearsay.enums import Stream
from hearsay.export import TranscriptLine
from hearsay.schemas import TranscriptEvent
from hearsay.services import MeetingService
from hearsay.transcript.live_base import LiveSidecarProcessor


class LiveMeProcessor(LiveSidecarProcessor):
    """Owns the ``hearsay-me`` sidecar for a meeting: feed Me audio, persist its utterances."""

    async def _handle(self, seg: dict[str, Any]) -> None:
        offset = self._offset_s or 0.0
        text = str(seg["text"])
        start_s = float(seg["start_s"]) + offset
        end_s = float(seg["end_s"]) + offset
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
        self._broadcaster.publish(
            TranscriptEvent(
                kind="final",
                stream=Stream.ME,
                speaker_label="Me",
                text=text,
                start_s=start_s,
                end_s=end_s,
            ).model_dump_json()
        )

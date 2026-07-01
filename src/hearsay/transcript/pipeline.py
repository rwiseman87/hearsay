"""Per-stream transcription pipeline.

Each stream's audio is streamed to its live Swift sidecar (Them -> ``hearsay-live``,
Me -> ``hearsay-me``), which runs VAD/diarization + ASR on the ANE and emits labeled
segments the sidecar processor persists + broadcasts. This pipeline just routes each
stream's PCM to its processor -- anchoring times to the first chunk's ``host_ts`` so both
streams share one clock -- records the Them track for the post-meeting refine, and rewrites
``transcript.md`` in timestamp order at stop.
"""

from __future__ import annotations

import asyncio
from datetime import datetime
from typing import TYPE_CHECKING
from uuid import UUID

from hearsay.db import Database
from hearsay.enums import MeetingStatus, Stream
from hearsay.export import MeetingMeta, TranscriptLine, TranscriptSink
from hearsay.helper.media_channel import AudioChunk, MediaChannel
from hearsay.log import get_logger
from hearsay.services import MeetingService
from hearsay.transcript.recorder import MeetingAudioRecorder, ThemAudioRecorder

if TYPE_CHECKING:
    from hearsay.transcript.live import LiveThemProcessor
    from hearsay.transcript.live_base import LiveSidecarProcessor
    from hearsay.transcript.live_me import LiveMeProcessor

_log = get_logger("hearsay.pipeline")


class TranscriptionPipeline:
    def __init__(
        self,
        *,
        meeting_id: UUID,
        database: Database,
        sink: TranscriptSink,
        them_recorder: ThemAudioRecorder | None = None,
        audio_recorder: MeetingAudioRecorder | None = None,
        them_processor: LiveThemProcessor | None = None,
        me_processor: LiveMeProcessor | None = None,
    ) -> None:
        self._meeting_id = meeting_id
        self._db = database
        self._sink = sink
        self._them_recorder = them_recorder
        # Mixed Me+Them WAV for playback (fed both streams by meeting time); Them-only recorder
        # above is for the refine.
        self._audio_recorder = audio_recorder
        # Each stream is handled by its live Swift sidecar (VAD/diarization + ASR on the ANE);
        # the pipeline only routes PCM to it. A stream with no processor (e.g. a missing sidecar
        # binary) is drained without transcription -- the Them track is still recorded for refine.
        self._them_processor = them_processor
        self._me_processor = me_processor
        self._tasks: list[asyncio.Task[None]] = []
        self._epoch_ns: int | None = None

    def _processor_for(self, stream: Stream) -> LiveSidecarProcessor | None:
        return self._me_processor if stream is Stream.ME else self._them_processor

    async def open(self, media: MediaChannel, meta: MeetingMeta) -> None:
        await self._sink.open(meta)
        for processor in (self._them_processor, self._me_processor):
            if processor is not None:
                await processor.start()
        self._tasks = [
            asyncio.create_task(self._consume(media, stream)) for stream in (Stream.ME, Stream.THEM)
        ]

    async def _consume(self, media: MediaChannel, stream: Stream) -> None:
        processor = self._processor_for(stream)
        queue = media.queues[stream]
        logged_first = False
        while True:
            chunk: AudioChunk | None = await queue.get()
            if chunk is None:  # end-of-stream (the sidecar drains its own tail on close)
                return
            if self._epoch_ns is None:
                self._epoch_ns = chunk.host_ts
            t0_s = (chunk.host_ts - self._epoch_ns) / 1e9
            if not logged_first:
                logged_first = True
                # Both streams must anchor to ~the same epoch; a large gap here means
                # one stream started late (e.g. mic warmup) rather than drifted.
                _log.info(
                    "stream %s first chunk: host_ts=%d t0_s=%.3f epoch=%d",
                    stream.value,
                    chunk.host_ts,
                    t0_s,
                    self._epoch_ns,
                )
            if stream is Stream.THEM and self._them_recorder is not None:
                self._them_recorder.write(chunk.samples, t0_s=t0_s)
            if self._audio_recorder is not None:  # both streams mix into the playback track
                self._audio_recorder.write(chunk.samples, t0_s=t0_s, stream=stream)
            if processor is not None:
                await processor.feed(chunk.samples, t0_s)

    async def close(self, *, ended_at: datetime) -> None:
        for task in self._tasks:
            task.cancel()
        for task in self._tasks:
            try:
                await task
            except asyncio.CancelledError:
                pass
            except Exception:
                # A consume task that already died (e.g. a sidecar's pipe broke) must not block the
                # finalize below -- log and carry on so the transcript is still rewritten in order.
                _log.exception("consume task failed during close")
        self._tasks = []
        # Drain the live sidecars: closing each finalizes its streaming tail, so the last
        # segments are persisted before the transcript is rewritten in order below.
        for processor in (self._them_processor, self._me_processor):
            if processor is not None:
                await processor.close()
        if self._them_recorder is not None:
            self._them_recorder.close()
        if self._audio_recorder is not None:
            self._audio_recorder.close()  # write the mixed playback WAV
        # The live transcript was appended in the sidecars' emit order across two streams;
        # rewrite it once in timestamp order for the final, readable file.
        lines = await self._ordered_lines()
        await self._sink.finalize(lines, ended_at=ended_at, status=MeetingStatus.FINALIZED.value)

    async def _ordered_lines(self) -> list[TranscriptLine]:
        async with self._db.session() as session:
            segments, _ = await MeetingService(session).list_segments(
                self._meeting_id, page=1, page_size=100_000
            )
        return [
            TranscriptLine(
                speaker_label=segment.speaker_label,
                text=segment.text,
                start_s=segment.start_s,
                end_s=segment.end_s,
            )
            for segment in segments
        ]

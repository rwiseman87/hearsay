"""Per-stream transcription pipeline.

For each stream a consumer reads :class:`AudioChunk`s, segments them with a VAD
(``Me``/``Them`` by channel in Phase 1), transcribes each utterance off the event
loop, and fans the result out three ways: final segments are persisted (DB) and
appended to ``transcript.md``; partials are broadcast to the live WebSocket only.
Times are anchored to the first chunk's ``host_ts`` so both streams share one clock.
"""

from __future__ import annotations

import asyncio
import contextlib
import re
from collections.abc import Callable, Sequence
from datetime import datetime
from uuid import UUID

from hearsay.asr import ASRBackend
from hearsay.config.settings import VADSettings
from hearsay.db import Database
from hearsay.enums import MeetingStatus, Stream
from hearsay.export import MeetingMeta, TranscriptLine, TranscriptSink
from hearsay.helper.media_channel import AudioChunk, MediaChannel
from hearsay.log import get_logger
from hearsay.schemas import TranscriptEvent
from hearsay.services import MeetingService
from hearsay.transcript.broadcast import Broadcaster
from hearsay.vad import VAD, Segmenter, Utterance

_log = get_logger("hearsay.pipeline")

_SPEAKER = {Stream.ME: "Me", Stream.THEM: "Them"}
# Whisper emits bracketed non-speech markers (e.g. [BLANK_AUDIO], [Music], (buzzing))
# when a VAD-passed clip has no real speech; drop clips that are entirely one marker.
_NON_SPEECH = re.compile(r"^[\[(][^\])]*[\])]$")


def _clean_text(text: str) -> str:
    stripped = text.strip()
    return "" if _NON_SPEECH.match(stripped) else stripped


class TranscriptionPipeline:
    def __init__(
        self,
        *,
        meeting_id: UUID,
        database: Database,
        sink: TranscriptSink,
        broadcaster: Broadcaster,
        asr: ASRBackend,
        vad_factory: Callable[[], VAD],
        vad: VADSettings,
        language: str | None = None,
    ) -> None:
        self._meeting_id = meeting_id
        self._db = database
        self._sink = sink
        self._broadcaster = broadcaster
        self._asr = asr
        self._language = language
        self._asr_lock = asyncio.Lock()
        self._tasks: list[asyncio.Task[None]] = []
        self._epoch_ns: int | None = None
        self._segmenters = {
            stream: Segmenter(
                vad_factory(),
                threshold=vad.threshold,
                min_speech_ms=vad.min_speech_ms,
                min_silence_ms=vad.min_silence_ms,
                partial_ms=vad.partial_ms,
            )
            for stream in (Stream.ME, Stream.THEM)
        }

    async def open(self, media: MediaChannel, meta: MeetingMeta) -> None:
        await self._sink.open(meta)
        self._tasks = [
            asyncio.create_task(self._consume(media, stream)) for stream in (Stream.ME, Stream.THEM)
        ]

    async def _consume(self, media: MediaChannel, stream: Stream) -> None:
        segmenter = self._segmenters[stream]
        queue = media.queues[stream]
        logged_first = False
        while True:
            chunk: AudioChunk | None = await queue.get()
            if chunk is None:  # end-of-stream
                final = segmenter.flush()
                if final is not None:
                    await self._emit(stream, final)
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
            for utterance in segmenter.push(chunk.samples, t0_s=t0_s):
                await self._emit(stream, utterance)

    async def _emit(self, stream: Stream, utterance: Utterance) -> None:
        text = await self._transcribe(utterance.samples)
        if not text:
            return
        speaker = _SPEAKER[stream]
        event = TranscriptEvent(
            kind="final" if utterance.is_final else "partial",
            stream=stream,
            speaker_label=speaker,
            text=text,
            start_s=utterance.start_s,
            end_s=utterance.end_s,
        )
        self._broadcaster.publish(event.model_dump_json())
        if utterance.is_final:
            await self._persist(stream, speaker, text, utterance.start_s, utterance.end_s)

    async def _transcribe(self, samples: Sequence[float]) -> str:
        async with self._asr_lock:
            segments = await asyncio.to_thread(
                self._asr.transcribe, samples, language=self._language
            )
        return _clean_text(" ".join(segment.text for segment in segments))

    async def _persist(
        self, stream: Stream, speaker: str, text: str, start_s: float, end_s: float
    ) -> None:
        async with self._db.session() as session:
            await MeetingService(session).add_segment(
                self._meeting_id,
                stream=stream,
                speaker_label=speaker,
                text=text,
                start_s=start_s,
                end_s=end_s,
            )
        await self._sink.append(
            TranscriptLine(speaker_label=speaker, text=text, start_s=start_s, end_s=end_s)
        )

    async def close(self, *, ended_at: datetime) -> None:
        for task in self._tasks:
            task.cancel()
        for task in self._tasks:
            with contextlib.suppress(asyncio.CancelledError):
                await task
        self._tasks = []
        # Force-flush any in-progress utterance (covers a stop that arrives before eos).
        for stream, segmenter in self._segmenters.items():
            final = segmenter.flush()
            if final is not None:
                await self._emit(stream, final)
        # The live transcript was appended in ASR-completion order across two streams;
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

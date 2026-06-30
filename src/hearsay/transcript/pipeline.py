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
from typing import TYPE_CHECKING
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
from hearsay.transcript.recorder import ThemAudioRecorder
from hearsay.vad import VAD, Segmenter, Utterance

if TYPE_CHECKING:
    from hearsay.transcript.diarizer import MeetingDiarizer
    from hearsay.transcript.live import LiveThemProcessor

_log = get_logger("hearsay.pipeline")

_SPEAKER = {Stream.ME: "Me", Stream.THEM: "Them"}
# Whisper emits bracketed non-speech markers (e.g. [BLANK_AUDIO], [Music], (buzzing))
# when a VAD-passed clip has no real speech; drop clips that are entirely one marker.
_NON_SPEECH = re.compile(r"^[\[(][^\])]*[\])]$")


def _clean_text(text: str) -> str:
    stripped = text.strip()
    return "" if _NON_SPEECH.match(stripped) else stripped


class TranscriptionPipeline:
    def __init__(  # noqa: PLR0913 (dependency-injection seam; each arg is a distinct dep)
        self,
        *,
        meeting_id: UUID,
        database: Database,
        sink: TranscriptSink,
        broadcaster: Broadcaster,
        asr: ASRBackend,
        vad_factory: Callable[[], VAD],
        vad: VADSettings,
        diarizer: MeetingDiarizer | None = None,
        them_recorder: ThemAudioRecorder | None = None,
        them_processor: LiveThemProcessor | None = None,
        language: str | None = None,
        condition_on_previous_text: bool = False,
        context_reset_gap_s: float = 8.0,
    ) -> None:
        self._meeting_id = meeting_id
        self._db = database
        self._sink = sink
        self._broadcaster = broadcaster
        self._asr = asr
        self._diarizer = diarizer
        self._them_recorder = them_recorder
        # When set, Them is handled by the live sidecar (diar+ASR+turns); the VAD/online
        # clusterer path below runs only for Me (and Them when streaming is off).
        self._them_processor = them_processor
        self._language = language
        self._condition = condition_on_previous_text
        self._context_reset_gap_s = context_reset_gap_s
        # Per-stream rolling decoding context: the last final's text + when it ended.
        self._context: dict[Stream, str] = {Stream.ME: "", Stream.THEM: ""}
        self._context_end_s: dict[Stream, float] = {Stream.ME: 0.0, Stream.THEM: 0.0}
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
        if self._them_processor is not None:
            await self._them_processor.start()
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
            if stream is Stream.THEM and self._them_recorder is not None:
                self._them_recorder.write(chunk.samples, t0_s=t0_s)
            if stream is Stream.THEM and self._them_processor is not None:
                # The sidecar does diarization + ASR + persistence for Them; no VAD here.
                await self._them_processor.feed(chunk.samples, t0_s)
                continue
            for utterance in segmenter.push(chunk.samples, t0_s=t0_s):
                await self._emit(stream, utterance)

    async def _emit(self, stream: Stream, utterance: Utterance) -> None:
        # Only finals carry decoding context; partials stay context-free (fast + drift-safe).
        prompt = self._context_prompt(stream, utterance) if utterance.is_final else None
        text = await self._transcribe(utterance.samples, prompt=prompt)
        if not text:
            return
        # Only finals are clustered + persisted; partials are ephemeral, so they keep the
        # cheap channel label (Me/Them) rather than spending an embedding on a fragment.
        if utterance.is_final:
            speaker, cluster_id = await self._resolve_speaker(stream, utterance)
        else:
            speaker, cluster_id = _SPEAKER[stream], None
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
            await self._persist(
                stream, speaker, text, utterance.start_s, utterance.end_s, cluster_id
            )
            if self._condition:
                self._context[stream] = text
                self._context_end_s[stream] = utterance.end_s

    def _context_prompt(self, stream: Stream, utterance: Utterance) -> str | None:
        # Carry the previous final's text as a decoding hint, but drop it after a long
        # silence so a stale/wrong prompt cannot snowball into the next utterance.
        if not self._condition:
            return None
        if utterance.start_s - self._context_end_s[stream] > self._context_reset_gap_s:
            return None
        return self._context[stream] or None

    async def _resolve_speaker(
        self, stream: Stream, utterance: Utterance
    ) -> tuple[str, UUID | None]:
        # Me is the mic channel and is never diarized; Them clusters into "Speaker N"
        # when a diarizer is present, else degrades to the generic "Them" label.
        if stream is Stream.THEM and self._diarizer is not None:
            return await self._diarizer.resolve(utterance)
        return _SPEAKER[stream], None

    def bind_speaker(self, ordinal: int, display_name: str) -> None:
        """Relay a manual rename to the live diarizer (no-op if diarization is off)."""
        if self._diarizer is not None:
            self._diarizer.bind(ordinal, display_name)

    async def _transcribe(self, samples: Sequence[float], *, prompt: str | None = None) -> str:
        async with self._asr_lock:
            segments = await asyncio.to_thread(
                self._asr.transcribe, samples, language=self._language, prompt=prompt
            )
        return _clean_text(" ".join(segment.text for segment in segments))

    async def _persist(
        self,
        stream: Stream,
        speaker: str,
        text: str,
        start_s: float,
        end_s: float,
        cluster_id: UUID | None,
    ) -> None:
        async with self._db.session() as session:
            await MeetingService(session).add_segment(
                self._meeting_id,
                stream=stream,
                speaker_label=speaker,
                text=text,
                start_s=start_s,
                end_s=end_s,
                cluster_id=cluster_id,
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
        # Drain the live Them sidecar: closing it finalizes its streaming tail, so the last
        # turns are persisted before the transcript is rewritten in order below.
        if self._them_processor is not None:
            await self._them_processor.close()
        # Release the ASR backend (e.g. terminate the Parakeet sidecar) now that no more
        # utterances will be transcribed; off the loop since it may wait on a subprocess.
        await asyncio.to_thread(self._asr.close)
        if self._them_recorder is not None:
            self._them_recorder.close()
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

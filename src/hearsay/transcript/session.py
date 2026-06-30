"""Meeting orchestration.

``SessionManager`` owns the single active :class:`MeetingSession` (Phase 1 records
one meeting at a time). A session creates the meeting row + folder, drives capture,
and runs the transcription pipeline (media -> VAD -> ASR -> DB + transcript.md + WS).
The ASR/VAD/sink dependencies are injected so the lifecycle is testable without the
helper or the ML stack; the real factories build whisper.cpp + Silero on demand.
"""

from __future__ import annotations

import asyncio
import shutil
from collections.abc import Callable
from datetime import UTC, datetime
from pathlib import Path
from uuid import UUID

from hearsay.asr import ASRBackend, build_asr
from hearsay.config.settings import Settings
from hearsay.db import Database
from hearsay.diarization import SpeakerEmbedder, build_embedder
from hearsay.export import LocalMarkdownSink, MeetingMeta, TranscriptSink
from hearsay.log import get_logger
from hearsay.models import Cluster, Meeting
from hearsay.schemas import TranscriptEvent
from hearsay.services import MeetingService, SpeakerService, meeting_folder_name
from hearsay.transcript.broadcast import Broadcaster
from hearsay.transcript.capture import Capture, HelperCapture
from hearsay.transcript.diarizer import MeetingDiarizer
from hearsay.transcript.pipeline import TranscriptionPipeline
from hearsay.transcript.recorder import ThemAudioRecorder
from hearsay.transcript.refine import (
    RefineError,
    build_recognition_embedder,
    rediarize_meeting,
)
from hearsay.vad import VAD
from hearsay.vad.silero import SileroVAD

_log = get_logger("hearsay.session")

CaptureFactory = Callable[[], Capture]
ASRFactory = Callable[[], ASRBackend]
VADFactory = Callable[[], VAD]
SinkFactory = Callable[[], TranscriptSink]
EmbedderFactory = Callable[[], SpeakerEmbedder | None]
PipelineFactory = Callable[[], TranscriptionPipeline]


class SessionBusyError(RuntimeError):
    """Raised when starting a meeting while another is already recording."""


class MeetingSession:
    def __init__(
        self,
        *,
        meta: MeetingMeta,
        capture: Capture,
        broadcaster: Broadcaster,
        pipeline_factory: PipelineFactory | None = None,
    ) -> None:
        self.meta = meta
        self.meeting_id = meta.id
        self.folder = meta.folder
        self.broadcaster = broadcaster
        self._capture = capture
        self._pipeline_factory = pipeline_factory
        self._pipeline: TranscriptionPipeline | None = None

    async def start(self) -> None:
        self.folder.mkdir(parents=True, exist_ok=True)
        await self._capture.start()
        media = self._capture.media
        if media is not None and self._pipeline_factory is not None:
            self._pipeline = self._pipeline_factory()
            await self._pipeline.open(media, self.meta)

    async def stop(self) -> None:
        if self._pipeline is not None:
            await self._pipeline.close(ended_at=datetime.now(UTC))
            self._pipeline = None
        await self._capture.stop()

    def publish_event(self, event: TranscriptEvent) -> None:
        """Push a transcript event to the live WebSocket stream (used in tests)."""
        self.broadcaster.publish(event.model_dump_json())

    def bind_speaker(self, ordinal: int, display_name: str) -> None:
        """Relay a manual rename to the live pipeline's diarizer."""
        if self._pipeline is not None:
            self._pipeline.bind_speaker(ordinal, display_name)


def _default_capture_factory(settings: Settings) -> CaptureFactory:
    def make() -> Capture:
        return HelperCapture(helper_path=settings.helper_path)

    return make


def _default_asr_factory(settings: Settings) -> ASRFactory:
    def make() -> ASRBackend:
        return build_asr(settings)

    return make


def _default_vad_factory(settings: Settings) -> VADFactory:
    model_path = settings.vad.model_path
    assert model_path is not None  # filled by Settings' validator

    def make() -> VAD:
        return SileroVAD(model_path)

    return make


def _default_embedder_factory(settings: Settings) -> EmbedderFactory:
    def make() -> SpeakerEmbedder | None:
        if not settings.diarization.enabled:
            return None
        try:
            return build_embedder(settings)
        except FileNotFoundError as exc:
            # No model yet -> degrade to channel labels (Them) until `fetch-models` runs.
            _log.warning("speaker embedder unavailable; diarization off (%s)", exc)
            return None

    return make


def _default_title(when: datetime) -> str:
    return f"Meeting {when:%Y-%m-%d %H:%M}"


class SessionManager:
    def __init__(
        self,
        *,
        database: Database,
        settings: Settings,
        capture_factory: CaptureFactory | None = None,
        asr_factory: ASRFactory | None = None,
        vad_factory: VADFactory | None = None,
        sink_factory: SinkFactory | None = None,
        embedder_factory: EmbedderFactory | None = None,
    ) -> None:
        self._db = database
        self._settings = settings
        self._capture_factory = capture_factory or _default_capture_factory(settings)
        self._asr_factory = asr_factory or _default_asr_factory(settings)
        self._vad_factory = vad_factory or _default_vad_factory(settings)
        self._sink_factory = sink_factory or LocalMarkdownSink
        self._embedder_factory = embedder_factory or _default_embedder_factory(settings)
        self._active: MeetingSession | None = None
        self._lock = asyncio.Lock()

    @property
    def active(self) -> MeetingSession | None:
        return self._active

    def _make_pipeline_factory(
        self, meeting_id: UUID, broadcaster: Broadcaster, folder: Path
    ) -> PipelineFactory:
        def make() -> TranscriptionPipeline:
            embedder = self._embedder_factory()
            diarizer = (
                MeetingDiarizer(
                    meeting_id=meeting_id,
                    database=self._db,
                    embedder=embedder,
                    threshold=self._settings.diarization.cluster_threshold,
                    min_embed_ms=self._settings.diarization.min_embed_ms,
                )
                if embedder is not None
                else None
            )
            them_recorder = (
                ThemAudioRecorder(folder / "them.wav")
                if self._settings.diarization.refine
                else None
            )
            return TranscriptionPipeline(
                meeting_id=meeting_id,
                database=self._db,
                sink=self._sink_factory(),
                broadcaster=broadcaster,
                asr=self._asr_factory(),
                vad_factory=self._vad_factory,
                vad=self._settings.vad,
                diarizer=diarizer,
                them_recorder=them_recorder,
                language=self._settings.asr.language,
                condition_on_previous_text=self._settings.asr.condition_on_previous_text,
                context_reset_gap_s=self._settings.asr.context_reset_gap_s,
            )

        return make

    async def start_meeting(self, *, title: str | None) -> Meeting:
        async with self._lock:
            if self._active is not None:
                raise SessionBusyError("a meeting is already recording")
            when = datetime.now(UTC)
            resolved_title = title or _default_title(when)
            folder_name = meeting_folder_name(resolved_title, when)
            async with self._db.session() as session:
                meeting = await MeetingService(session).create(
                    title=resolved_title, folder=folder_name, started_at=when
                )
            broadcaster = Broadcaster()
            meta = MeetingMeta(
                id=meeting.id,
                title=resolved_title,
                started_at=when,
                folder=self._settings.output_dir / folder_name,
            )
            session_obj = MeetingSession(
                meta=meta,
                capture=self._capture_factory(),
                broadcaster=broadcaster,
                pipeline_factory=self._make_pipeline_factory(meeting.id, broadcaster, meta.folder),
            )
            await session_obj.start()
            self._active = session_obj
            return meeting

    async def stop_meeting(self, meeting_id: UUID) -> Meeting | None:
        async with self._lock:
            active = self._active
            if active is not None and active.meeting_id == meeting_id:
                await active.stop()
                self._active = None
        async with self._db.session() as session:
            meeting = await MeetingService(session).finalize(meeting_id)
        if meeting is not None:
            await self._maybe_auto_refine(meeting)
        return meeting

    async def _maybe_auto_refine(self, meeting: Meeting) -> None:
        """Re-diarize the just-finalized meeting (FluidAudio on the ANE; ~seconds), so it ends
        with accurate speaker labels without the manual button. Best-effort: a missing
        recording or a diarizer failure is logged, never raised -- the stop already succeeded."""
        diarization = self._settings.diarization
        if not (diarization.refine and diarization.auto_refine):
            return
        them_wav = self._settings.output_dir / meeting.folder / "them.wav"
        if not them_wav.exists():
            return
        try:
            embedder = build_recognition_embedder(self._settings)
            result = await rediarize_meeting(
                meeting.id, database=self._db, settings=self._settings, embedder=embedder
            )
            _log.info(
                "auto-refined meeting %s at finalize: %d speakers, %d segments relabeled",
                meeting.id,
                result.speaker_count,
                result.segments_relabeled,
            )
        except RefineError as exc:
            _log.info("auto-refine skipped for meeting %s: %s", meeting.id, exc)
        except Exception:
            _log.exception("auto-refine at finalize failed for meeting %s", meeting.id)

    async def delete_meeting(self, meeting_id: UUID) -> bool:
        async with self._lock:
            active = self._active
            if active is not None and active.meeting_id == meeting_id:
                await active.stop()
                self._active = None
        async with self._db.session() as session:
            service = MeetingService(session)
            meeting = await service.get(meeting_id)
            if meeting is None:
                return False
            folder = self._settings.output_dir / meeting.folder
            await service.delete(meeting_id)
        shutil.rmtree(folder, ignore_errors=True)
        return True

    async def relabel_speaker(
        self, meeting_id: UUID, cluster_id: UUID, display_name: str
    ) -> Cluster | None:
        """Rename a speaker: bind + relabel its segments (DB), then propagate to the
        live clusterer so the active meeting's future utterances carry the name too."""
        async with self._db.session() as session:
            service = SpeakerService(session)
            cluster = await service.get_cluster(cluster_id)
            if cluster is None or cluster.meeting_id != meeting_id:
                return None
            ordinal = cluster.ordinal
            bound = await service.bind_cluster(cluster_id, display_name=display_name)
        async with self._lock:
            active = self._active
            if active is not None and active.meeting_id == meeting_id:
                active.bind_speaker(ordinal, display_name.strip())
        return bound

    async def shutdown(self) -> None:
        async with self._lock:
            if self._active is not None:
                await self._active.stop()
                self._active = None

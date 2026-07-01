"""Meeting orchestration.

``SessionManager`` owns the single active :class:`MeetingSession` (Phase 1 records
one meeting at a time). A session creates the meeting row + folder, drives capture,
and runs the transcription pipeline, which routes each stream's PCM to its Swift sidecar
(``hearsay-live`` for Them, ``hearsay-me`` for Me). The capture/sink dependencies are
injected so the lifecycle is testable without the helper.
"""

from __future__ import annotations

import asyncio
import shutil
from collections.abc import Callable
from datetime import UTC, datetime
from pathlib import Path
from uuid import UUID

from hearsay.config.settings import Settings
from hearsay.db import Database
from hearsay.export import LocalMarkdownSink, MeetingMeta, TranscriptSink
from hearsay.log import get_logger
from hearsay.models import Cluster, Meeting
from hearsay.schemas import TranscriptEvent
from hearsay.services import MeetingService, SpeakerService, meeting_folder_name
from hearsay.transcript.broadcast import Broadcaster
from hearsay.transcript.capture import Capture, HelperCapture
from hearsay.transcript.live import LiveThemProcessor
from hearsay.transcript.live_me import LiveMeProcessor
from hearsay.transcript.pipeline import TranscriptionPipeline
from hearsay.transcript.recorder import MeetingAudioRecorder, ThemAudioRecorder
from hearsay.transcript.refine import RefineError, rediarize_meeting

_log = get_logger("hearsay.session")

CaptureFactory = Callable[[], Capture]
SinkFactory = Callable[[], TranscriptSink]
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


def _default_capture_factory(settings: Settings) -> CaptureFactory:
    def make() -> Capture:
        return HelperCapture(helper_path=settings.helper_path)

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
        sink_factory: SinkFactory | None = None,
    ) -> None:
        self._db = database
        self._settings = settings
        self._capture_factory = capture_factory or _default_capture_factory(settings)
        self._sink_factory = sink_factory or LocalMarkdownSink
        self._active: MeetingSession | None = None
        self._lock = asyncio.Lock()

    @property
    def active(self) -> MeetingSession | None:
        return self._active

    def _make_pipeline_factory(
        self, meeting_id: UUID, broadcaster: Broadcaster, folder: Path
    ) -> PipelineFactory:
        def make() -> TranscriptionPipeline:
            sink = self._sink_factory()
            # Both live streams run in Swift sidecars (VAD/diarization + ASR + persistence on the
            # ANE): Them -> hearsay-live, Me -> hearsay-me.
            them_processor = LiveThemProcessor(
                binary_path=self._settings.helper_path.with_name("hearsay-live"),
                meeting_id=meeting_id,
                database=self._db,
                broadcaster=broadcaster,
                sink=sink,
            )
            me_processor = LiveMeProcessor(
                binary_path=self._settings.helper_path.with_name("hearsay-me"),
                meeting_id=meeting_id,
                database=self._db,
                broadcaster=broadcaster,
                sink=sink,
            )
            them_recorder = (
                ThemAudioRecorder(folder / "them.wav")
                if self._settings.diarization.refine
                else None
            )
            audio_recorder = (
                MeetingAudioRecorder(folder / "audio.wav") if self._settings.audio.record else None
            )
            return TranscriptionPipeline(
                meeting_id=meeting_id,
                database=self._db,
                sink=sink,
                them_recorder=them_recorder,
                audio_recorder=audio_recorder,
                them_processor=them_processor,
                me_processor=me_processor,
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
            result = await rediarize_meeting(meeting.id, database=self._db, settings=self._settings)
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
        """Rename a speaker: bind the identity + retroactively relabel its existing segments.
        The live Them sidecar labels by ordinal; a manual name is reapplied to the meeting's
        final speakers by the post-meeting refine (which carries locked names forward)."""
        async with self._db.session() as session:
            service = SpeakerService(session)
            cluster = await service.get_cluster(cluster_id)
            if cluster is None or cluster.meeting_id != meeting_id:
                return None
            return await service.bind_cluster(cluster_id, display_name=display_name)

    async def shutdown(self) -> None:
        async with self._lock:
            if self._active is not None:
                await self._active.stop()
                self._active = None

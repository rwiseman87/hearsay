"""Post-meeting re-diarization: relabel a meeting's speakers from the recorded Them track.

The online clusterer labels Them per-utterance live, which fails on overlapping/continuous
audio (the VAD hands it multi-speaker utterances). This runs an offline diarizer (pyannote)
over the whole ``them.wav``, maps its speaker turns onto the saved segments by time overlap,
replaces the clusters + segment labels, and regenerates ``transcript.md``.
"""

from __future__ import annotations

import asyncio
import wave
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from uuid import UUID

from hearsay.config.settings import Settings
from hearsay.db import Database
from hearsay.diarization import (
    OfflineDiarizer,
    assign_segment_speaker,
    build_offline_diarizer,
    order_speakers,
)
from hearsay.enums import MeetingStatus, Stream
from hearsay.export import LocalMarkdownSink, MeetingMeta, TranscriptLine
from hearsay.log import get_logger
from hearsay.models import Meeting
from hearsay.services import MeetingService, SpeakerService
from hearsay.transcript.recorder import read_offset_s

_log = get_logger("hearsay.refine")


class RefineError(RuntimeError):
    """Re-diarization could not run (missing meeting or no recorded Them track)."""


@dataclass(frozen=True, slots=True)
class RefineResult:
    meeting_id: UUID
    speaker_count: int
    segments_relabeled: int


def _read_wav(path: Path) -> tuple[Any, int]:
    import numpy as np  # noqa: PLC0415 (optional dep; only present when refining)

    with wave.open(str(path)) as wav:
        sample_rate = wav.getframerate()
        frames = wav.readframes(wav.getnframes())
    samples = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0
    return samples, sample_rate


async def rediarize_meeting(
    meeting_id: UUID,
    *,
    database: Database,
    settings: Settings,
    diarizer: OfflineDiarizer | None = None,
) -> RefineResult:
    """Re-diarize ``them.wav`` offline and bake the result into the DB + transcript."""
    async with database.session() as session:
        meeting = await MeetingService(session).get(meeting_id)
    if meeting is None:
        raise RefineError(f"meeting {meeting_id} not found")
    folder = settings.output_dir / meeting.folder
    wav_path = folder / "them.wav"
    if not wav_path.exists():
        raise RefineError(
            f"no them.wav for meeting {meeting_id}; enable diarization.refine before capturing"
        )

    samples, sample_rate = _read_wav(wav_path)
    offset_s = read_offset_s(wav_path)
    diarizer = diarizer or build_offline_diarizer(settings)
    turns = await asyncio.to_thread(diarizer.diarize, samples, sample_rate=sample_rate)
    label_to_ordinal = order_speakers(turns)

    async with database.session() as session:
        segments, _ = await MeetingService(session).list_segments(
            meeting_id, page=1, page_size=100_000
        )
        clusters = await SpeakerService(session).list_clusters(meeting_id)
    # Manual renames (locked clusters) to carry forward so re-diarization never drops a name.
    prior_name_by_cluster = {
        cluster.id: cluster.identity.display_name
        for cluster in clusters
        if cluster.locked and cluster.identity is not None
    }
    them = [segment for segment in segments if segment.stream is Stream.THEM]

    segment_ordinals: dict[UUID, int | None] = {}
    name_votes: Counter[tuple[int, str]] = Counter()
    relabeled = 0
    for segment in them:
        label = assign_segment_speaker(segment.start_s, segment.end_s, turns, offset_s=offset_s)
        ordinal = label_to_ordinal.get(label) if label is not None else None
        segment_ordinals[segment.id] = ordinal
        if ordinal is not None:
            relabeled += 1
            if segment.cluster_id is not None:
                prior_name = prior_name_by_cluster.get(segment.cluster_id)
                if prior_name is not None:
                    name_votes[(ordinal, prior_name)] += 1
    # Bind each manual name to the new ordinal it most dominates (one name <-> one ordinal).
    ordinal_names: dict[int, str] = {}
    used_names: set[str] = set()
    for (ordinal, name), _votes in name_votes.most_common():
        if ordinal not in ordinal_names and name not in used_names:
            ordinal_names[ordinal] = name
            used_names.add(name)

    async with database.session() as session:
        await SpeakerService(session).apply_diarization(
            meeting_id,
            segment_ordinals=segment_ordinals,
            speaker_count=len(label_to_ordinal),
            ordinal_names=ordinal_names,
        )
    await _rewrite_transcript(meeting_id, meeting=meeting, folder=folder, database=database)

    _log.info(
        "rediarized meeting %s: %d speakers, %d/%d Them segments labeled, %d names kept",
        meeting_id,
        len(label_to_ordinal),
        relabeled,
        len(them),
        len(ordinal_names),
    )
    return RefineResult(
        meeting_id=meeting_id, speaker_count=len(label_to_ordinal), segments_relabeled=relabeled
    )


async def _rewrite_transcript(
    meeting_id: UUID, *, meeting: Meeting, folder: Path, database: Database
) -> None:
    async with database.session() as session:
        segments, _ = await MeetingService(session).list_segments(
            meeting_id, page=1, page_size=100_000
        )
    sink = LocalMarkdownSink()
    meta = MeetingMeta(
        id=meeting.id, title=meeting.title, started_at=meeting.started_at, folder=folder
    )
    await sink.open(meta)
    lines = [
        TranscriptLine(
            speaker_label=segment.speaker_label,
            text=segment.text,
            start_s=segment.start_s,
            end_s=segment.end_s,
        )
        for segment in segments
    ]
    await sink.finalize(lines, ended_at=meeting.updated_at, status=MeetingStatus.FINALIZED.value)

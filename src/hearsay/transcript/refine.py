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
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from uuid import UUID

from hearsay.asr import ASRBackend, build_asr
from hearsay.config.settings import Settings
from hearsay.db import Database
from hearsay.diarization import (
    OfflineDiarizer,
    SpeakerEmbedder,
    SpeakerTurn,
    assign_segment_speaker,
    build_embedder,
    build_offline_diarizer,
    centroid_from_bytes,
    centroid_to_bytes,
    match_identity,
    order_speakers,
)
from hearsay.enums import MeetingStatus, Stream
from hearsay.export import LocalMarkdownSink, MeetingMeta, TranscriptLine
from hearsay.log import get_logger
from hearsay.models import Cluster, Meeting, Segment
from hearsay.services import MeetingService, SpeakerService, TurnSegment
from hearsay.transcript.recorder import read_offset_s

_log = get_logger("hearsay.refine")


class RefineError(RuntimeError):
    """Re-diarization could not run (missing meeting or no recorded Them track)."""


@dataclass(frozen=True, slots=True)
class RefineResult:
    meeting_id: UUID
    speaker_count: int
    segments_relabeled: int


def build_recognition_embedder(settings: Settings) -> SpeakerEmbedder | None:
    """The ONNX embedder for cross-meeting voiceprints, or ``None`` if the model is absent."""
    try:
        return build_embedder(settings)
    except FileNotFoundError as exc:
        _log.warning("speaker embedder unavailable; cross-meeting recognition off (%s)", exc)
        return None


def _read_wav(path: Path) -> tuple[Any, int]:
    import numpy as np  # noqa: PLC0415 (optional dep; only present when refining)

    with wave.open(str(path)) as wav:
        sample_rate = wav.getframerate()
        frames = wav.readframes(wav.getnframes())
    samples = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0
    return samples, sample_rate


def _gather_speaker_audio(
    samples: Any, sample_rate: int, turns: Sequence[SpeakerTurn], label_to_ordinal: dict[str, int]
) -> dict[int, Any]:
    """Concatenate each speaker's turn audio (capped) for one voiceprint embedding per speaker."""
    import numpy as np  # noqa: PLC0415

    cap = int(12.0 * sample_rate)  # enough signal for a stable voiceprint, bounded cost
    chunks: dict[int, list[Any]] = {}
    for turn in turns:
        ordinal = label_to_ordinal.get(turn.speaker)
        if ordinal is None:
            continue
        start = max(0, int(turn.start_s * sample_rate))
        end = min(len(samples), int(turn.end_s * sample_rate))
        if end > start:
            chunks.setdefault(ordinal, []).append(samples[start:end])
    return {ordinal: np.concatenate(parts)[:cap] for ordinal, parts in chunks.items() if parts}


async def _recognize_speakers(  # noqa: PLR0913 (a cohesive step; each arg is a distinct input)
    *,
    database: Database,
    settings: Settings,
    embedder: SpeakerEmbedder | None,
    meeting_id: UUID,
    samples: Any,
    sample_rate: int,
    turns: Sequence[SpeakerTurn],
    label_to_ordinal: dict[str, int],
    manual: dict[int, str],
) -> tuple[dict[int, bytes], dict[int, str]]:
    ordinal_centroids: dict[int, bytes] = {}
    recognized: dict[int, str] = {}
    if embedder is None or not turns:
        return ordinal_centroids, recognized
    async with database.session() as session:
        known_bytes = await SpeakerService(session).known_voiceprints(exclude_meeting_id=meeting_id)
    known = [(name, centroid_from_bytes(blob)) for name, blob in known_bytes]
    threshold = settings.diarization.recognition_threshold
    audio_by_ordinal = _gather_speaker_audio(samples, sample_rate, turns, label_to_ordinal)
    for ordinal, audio in audio_by_ordinal.items():
        embedding = await asyncio.to_thread(embedder.embed, audio)
        centroid = [float(value) for value in embedding]
        ordinal_centroids[ordinal] = centroid_to_bytes(centroid)
        if ordinal not in manual:  # a manual carry-forward name wins over auto-recognition
            name = match_identity(centroid, known, threshold=threshold)
            if name is not None:
                recognized[ordinal] = name
    return ordinal_centroids, recognized


def _carry_forward_names(
    them: Sequence[Segment],
    turns: Sequence[SpeakerTurn],
    clusters: Sequence[Cluster],
    label_to_ordinal: dict[str, int],
    offset_s: float,
) -> dict[int, str]:
    """Vote each prior locked name onto the turn ordinal its old segments most overlap, so a
    re-diarize never drops a manual binding (one name <-> one ordinal)."""
    prior_name_by_cluster = {
        cluster.id: cluster.identity.display_name
        for cluster in clusters
        if cluster.locked and cluster.identity is not None
    }
    name_votes: Counter[tuple[int, str]] = Counter()
    for segment in them:
        if segment.cluster_id is None:
            continue
        prior_name = prior_name_by_cluster.get(segment.cluster_id)
        if prior_name is None:
            continue
        label = assign_segment_speaker(segment.start_s, segment.end_s, turns, offset_s=offset_s)
        ordinal = label_to_ordinal.get(label) if label is not None else None
        if ordinal is not None:
            name_votes[(ordinal, prior_name)] += 1
    ordinal_names: dict[int, str] = {}
    used_names: set[str] = set()
    for (ordinal, name), _votes in name_votes.most_common():
        if ordinal not in ordinal_names and name not in used_names:
            ordinal_names[ordinal] = name
            used_names.add(name)
    return ordinal_names


async def _transcribe_turns(
    asr: ASRBackend,
    samples: Any,
    sample_rate: int,
    turns: Sequence[SpeakerTurn],
    label_to_ordinal: dict[str, int],
    offset_s: float,
) -> list[TurnSegment]:
    """Re-transcribe each diarizer turn's audio span so the Them transcript follows speaker
    changes (the live VAD merges back-and-forth exchanges into one multi-speaker utterance).
    Returns turn segments in meeting time (turn times are WAV-relative; ``offset_s`` shifts)."""
    result: list[TurnSegment] = []
    for turn in sorted(turns, key=lambda t: t.start_s):
        ordinal = label_to_ordinal.get(turn.speaker)
        if ordinal is None:
            continue
        start = max(0, int(turn.start_s * sample_rate))
        end = min(len(samples), int(turn.end_s * sample_rate))
        if end <= start:
            continue
        segments = await asyncio.to_thread(asr.transcribe, samples[start:end])
        text = " ".join(segment.text for segment in segments).strip()
        if not text:
            continue
        result.append(
            TurnSegment(
                ordinal=ordinal,
                text=text,
                start_s=turn.start_s + offset_s,
                end_s=turn.end_s + offset_s,
            )
        )
    return result


async def rediarize_meeting(
    meeting_id: UUID,
    *,
    database: Database,
    settings: Settings,
    diarizer: OfflineDiarizer | None = None,
    embedder: SpeakerEmbedder | None = None,
    asr: ASRBackend | None = None,
) -> RefineResult:
    """Re-diarize ``them.wav`` offline and rebuild the Them transcript per speaker turn.

    Each diarizer turn's audio is re-transcribed (FluidAudio Parakeet by default) so the
    transcript splits the multi-speaker utterances the live VAD merged. With an ``embedder``,
    each speaker's voiceprint is stored and matched against people named in prior meetings, so
    a returning person is auto-named (provisionally)."""
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

    async with database.session() as session:
        segments, _ = await MeetingService(session).list_segments(
            meeting_id, page=1, page_size=100_000
        )
        clusters = await SpeakerService(session).list_clusters(meeting_id)
    them = [segment for segment in segments if segment.stream is Stream.THEM]
    if not them:
        # Nothing transcribed for Them (e.g. an ASR failure left the meeting empty) -> don't
        # diarize the recording or create a phantom "Speaker 1" with no segments to label.
        _log.info("rediarize: meeting %s has no Them segments; nothing to refine", meeting_id)
        return RefineResult(meeting_id=meeting_id, speaker_count=0, segments_relabeled=0)

    samples, sample_rate = _read_wav(wav_path)
    offset_s = read_offset_s(wav_path)
    diarizer = diarizer or build_offline_diarizer(settings)
    turns = await asyncio.to_thread(diarizer.diarize, samples, sample_rate=sample_rate)
    label_to_ordinal = order_speakers(turns)

    # Rebuild the Them transcript from the turns: re-transcribe each turn's audio (default
    # Parakeet). Owns the ASR backend it builds, so it tears the sidecar down afterwards.
    owns_asr = asr is None
    asr = asr or build_asr(settings)
    try:
        turn_segments = await _transcribe_turns(
            asr, samples, sample_rate, turns, label_to_ordinal, offset_s
        )
    finally:
        if owns_asr:
            await asyncio.to_thread(asr.close)
    if not turn_segments:
        # The diarizer/ASR produced nothing -> keep the existing transcript rather than wipe it.
        _log.info("rediarize: meeting %s produced no turns; leaving transcript as-is", meeting_id)
        return RefineResult(
            meeting_id=meeting_id, speaker_count=len(label_to_ordinal), segments_relabeled=0
        )

    # Carry manual renames forward onto the new turn ordinals (never drop a manual binding).
    ordinal_names = _carry_forward_names(them, turns, clusters, label_to_ordinal, offset_s)

    # Voiceprints: embed each speaker, store its centroid, and auto-name a returning person
    # by matching against people named in prior meetings (manual carry-forward wins).
    ordinal_centroids, recognized = await _recognize_speakers(
        database=database,
        settings=settings,
        embedder=embedder,
        meeting_id=meeting_id,
        samples=samples,
        sample_rate=sample_rate,
        turns=turns,
        label_to_ordinal=label_to_ordinal,
        manual=ordinal_names,
    )

    async with database.session() as session:
        await SpeakerService(session).apply_turn_diarization(
            meeting_id,
            turn_segments=turn_segments,
            speaker_count=len(label_to_ordinal),
            ordinal_names=ordinal_names,
            ordinal_centroids=ordinal_centroids,
            recognized=recognized,
        )
    await _rewrite_transcript(meeting_id, meeting=meeting, folder=folder, database=database)

    _log.info(
        "rediarized meeting %s: %d speakers, %d turn segments, %d names kept, %d recognized",
        meeting_id,
        len(label_to_ordinal),
        len(turn_segments),
        len(ordinal_names),
        len(recognized),
    )
    return RefineResult(
        meeting_id=meeting_id,
        speaker_count=len(label_to_ordinal),
        segments_relabeled=len(turn_segments),
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

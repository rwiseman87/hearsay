"""Post-meeting re-diarization: pure turn->segment mapping + end-to-end relabel.

The mapping logic is pure (no torch); the end-to-end test drives the real orchestration
(DB + audio.wav + transcript rewrite) with a stub offline diarizer, so it runs in CI.
"""

from __future__ import annotations

import wave
from array import array
from datetime import UTC, datetime
from pathlib import Path

from sqlalchemy import create_engine as create_sync_engine

from hearsay.asr import ASRSegment
from hearsay.config.settings import Settings
from hearsay.db import Database
from hearsay.diarization import (
    DiarizationResult,
    SpeakerTurn,
    assign_segment_speaker,
    centroid_to_bytes,
    order_speakers,
)
from hearsay.enums import Stream
from hearsay.models import Base
from hearsay.services import MeetingService, SpeakerService
from hearsay.transcript import rediarize_meeting

# --- pure mapping ---------------------------------------------------------------------


def test_order_speakers_by_first_appearance() -> None:
    turns = [SpeakerTurn("B", 5.0, 6.0), SpeakerTurn("A", 1.0, 2.0), SpeakerTurn("B", 0.5, 1.0)]
    assert order_speakers(turns) == {"B": 1, "A": 2}  # B first appears at 0.5s


def test_assign_segment_speaker_picks_max_overlap() -> None:
    turns = [SpeakerTurn("A", 0.0, 2.5), SpeakerTurn("B", 2.5, 6.0)]
    assert assign_segment_speaker(0.0, 2.0, turns, offset_s=0.0) == "A"
    assert assign_segment_speaker(2.0, 4.0, turns, offset_s=0.0) == "B"  # 0.5s A vs 1.5s B
    assert assign_segment_speaker(4.0, 6.0, turns, offset_s=0.0) == "B"


def test_assign_segment_speaker_applies_offset() -> None:
    turns = [SpeakerTurn("A", 0.0, 2.0)]  # WAV-relative; meeting time is +offset
    assert assign_segment_speaker(10.5, 11.5, turns, offset_s=10.0) == "A"
    assert assign_segment_speaker(0.5, 1.5, turns, offset_s=10.0) is None  # shifted out of range


def test_assign_segment_speaker_no_overlap_is_none() -> None:
    assert assign_segment_speaker(5.0, 6.0, [SpeakerTurn("A", 0.0, 1.0)], offset_s=0.0) is None
    assert assign_segment_speaker(0.0, 1.0, [], offset_s=0.0) is None


# --- end-to-end relabel ---------------------------------------------------------------


class StubDiarizer:
    def __init__(
        self, turns: list[SpeakerTurn], embeddings: dict[str, list[float]] | None = None
    ) -> None:
        self._turns = turns
        self._embeddings = embeddings or {}

    def diarize(self, samples: object, *, sample_rate: int) -> DiarizationResult:
        return DiarizationResult(turns=self._turns, embeddings=self._embeddings)


class StubASR:
    """Returns a distinct marker per call (t0, t1, ...) so each turn's text is identifiable."""

    name = "stub"
    model = "stub"

    def __init__(self) -> None:
        self._n = 0

    def transcribe(self, samples: object) -> list[ASRSegment]:
        text = f"t{self._n}"
        self._n += 1
        return [ASRSegment(text=text, start_s=0.0, end_s=1.0)]

    def close(self) -> None:
        return None


def _make_db(tmp_path: Path) -> Database:
    db_file = tmp_path / "refine.db"
    engine = create_sync_engine(f"sqlite:///{db_file}")
    Base.metadata.create_all(engine)
    engine.dispose()
    return Database(f"sqlite+aiosqlite:///{db_file}")


def _write_silent_wav(path: Path, seconds: float = 6.0) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    pcm = array("h", [0] * (int(16_000 * seconds) * 2))  # stereo (Me=L, Them=R), silent
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(2)
        wav.setsampwidth(2)
        wav.setframerate(16_000)
        wav.writeframes(pcm.tobytes())


async def test_rediarize_relabels_segments_and_transcript(tmp_path: Path) -> None:
    database = _make_db(tmp_path)
    settings = Settings()
    settings.output_dir = tmp_path

    async with database.session() as session:
        meetings = MeetingService(session)
        meeting = await meetings.create(title="T", folder="mtg", started_at=datetime.now(UTC))
        # One Me segment (must stay untouched) + Them segments from the live (wrong) labels.
        await meetings.add_segment(
            meeting.id, stream=Stream.ME, speaker_label="Me", text="mine", start_s=0.0, end_s=2.0
        )
        for start, end, text in [(0.0, 2.0, "alpha"), (2.0, 4.0, "beta"), (4.0, 6.0, "gamma")]:
            await meetings.add_segment(
                meeting.id,
                stream=Stream.THEM,
                speaker_label="Them",
                text=text,
                start_s=start,
                end_s=end,
            )
        # A stale online cluster that the refine must replace.
        await SpeakerService(session).create_cluster(meeting.id, ordinal=1)

    _write_silent_wav(tmp_path / "mtg" / "audio.wav")
    # Two turns -> the coarse Them segments are rebuilt as one re-transcribed segment per turn.
    stub = StubDiarizer([SpeakerTurn("SPEAKER_x", 0.0, 2.5), SpeakerTurn("SPEAKER_y", 2.5, 6.0)])

    result = await rediarize_meeting(
        meeting.id, database=database, settings=settings, diarizer=stub, asr=StubASR()
    )

    assert result.speaker_count == 2
    assert result.segments_relabeled == 2  # one segment per turn (re-transcribed), not the 3 coarse
    async with database.session() as session:
        segments, _ = await MeetingService(session).list_segments(meeting.id, page=1, page_size=100)
        clusters = await SpeakerService(session).list_clusters(meeting.id)
    await database.dispose()

    me = [s for s in segments if s.stream is Stream.ME]
    them = sorted((s for s in segments if s.stream is Stream.THEM), key=lambda s: s.start_s)
    assert me[0].speaker_label == "Me" and me[0].text == "mine"  # Me untouched
    # Each turn is re-transcribed (StubASR -> t0, t1) and labeled by its speaker ordinal.
    assert [(s.speaker_label, s.text) for s in them] == [("Speaker 1", "t0"), ("Speaker 2", "t1")]
    assert all(s.cluster_id is not None for s in them)
    assert sorted(c.ordinal for c in clusters) == [1, 2]  # stale cluster replaced, not appended

    transcript = (tmp_path / "mtg" / "transcript.md").read_text(encoding="utf-8")
    assert "Speaker 1" in transcript and "Speaker 2" in transcript and "Them" not in transcript


async def test_rediarize_preserves_manual_rename(tmp_path: Path) -> None:
    database = _make_db(tmp_path)
    settings = Settings()
    settings.output_dir = tmp_path

    async with database.session() as session:
        meetings = MeetingService(session)
        speakers = SpeakerService(session)
        meeting = await meetings.create(title="T", folder="mtg", started_at=datetime.now(UTC))
        seg_ids = []
        for start, end in [(0.0, 2.0), (2.0, 4.0), (4.0, 6.0)]:
            seg = await meetings.add_segment(
                meeting.id,
                stream=Stream.THEM,
                speaker_label="Them",
                text="x",
                start_s=start,
                end_s=end,
            )
            seg_ids.append(seg.id)
        # The user manually renamed the first speaker (segs 0+1) to "Alice" (locks the binding).
        alice = await speakers.create_cluster(meeting.id, ordinal=1)
        await speakers.assign_segment_cluster(seg_ids[0], alice.id)
        await speakers.assign_segment_cluster(seg_ids[1], alice.id)
        await speakers.bind_cluster(alice.id, display_name="Alice")

    _write_silent_wav(tmp_path / "mtg" / "audio.wav")
    # pyannote re-clusters: x covers Alice's segs [0,4], y is the other speaker [4,6].
    stub = StubDiarizer([SpeakerTurn("SPEAKER_x", 0.0, 4.0), SpeakerTurn("SPEAKER_y", 4.0, 6.0)])

    await rediarize_meeting(
        meeting.id, database=database, settings=settings, diarizer=stub, asr=StubASR()
    )

    async with database.session() as session:
        segments, _ = await MeetingService(session).list_segments(meeting.id, page=1, page_size=100)
        clusters = await SpeakerService(session).list_clusters(meeting.id)
    await database.dispose()

    them = sorted(segments, key=lambda s: s.start_s)
    # Alice's old segments overlapped turn x -> the manual name carries onto that turn's segment.
    assert [s.speaker_label for s in them] == ["Alice", "Speaker 2"]  # rename survived the rebuild
    alice_cluster = next(c for c in clusters if c.ordinal == 1)
    assert alice_cluster.locked and alice_cluster.identity is not None
    assert alice_cluster.identity.display_name == "Alice"


async def test_rediarize_recognizes_returning_speaker(tmp_path: Path) -> None:
    database = _make_db(tmp_path)
    settings = Settings()
    settings.output_dir = tmp_path

    # Meeting 1: "Alice" was named, with a stored voiceprint of [1, 0].
    async with database.session() as session:
        speakers = SpeakerService(session)
        m1 = await MeetingService(session).create(
            title="M1", folder="m1", started_at=datetime.now(UTC)
        )
        cluster = await speakers.create_cluster(m1.id, ordinal=1)
        await speakers.bind_cluster(cluster.id, display_name="Alice")
        cluster.centroid = centroid_to_bytes([1.0, 0.0])
        await session.commit()

    # Meeting 2: a fresh recording whose first speaker's voiceprint matches Alice's.
    async with database.session() as session:
        meetings = MeetingService(session)
        m2 = await meetings.create(title="M2", folder="m2", started_at=datetime.now(UTC))
        for start, end in [(0.0, 4.0), (4.0, 6.0)]:
            await meetings.add_segment(
                m2.id, stream=Stream.THEM, speaker_label="Them", text="x", start_s=start, end_s=end
            )
    _write_silent_wav(tmp_path / "m2" / "audio.wav")
    # The diarizer returns a voiceprint per speaker; SPEAKER_x's matches Alice's [1, 0].
    stub = StubDiarizer(
        [SpeakerTurn("SPEAKER_x", 0.0, 4.0), SpeakerTurn("SPEAKER_y", 4.0, 6.0)],
        embeddings={"SPEAKER_x": [1.0, 0.0], "SPEAKER_y": [0.0, 1.0]},
    )

    await rediarize_meeting(
        m2.id,
        database=database,
        settings=settings,
        diarizer=stub,
        asr=StubASR(),
    )

    async with database.session() as session:
        segments, _ = await MeetingService(session).list_segments(m2.id, page=1, page_size=100)
        clusters = await SpeakerService(session).list_clusters(m2.id)
    await database.dispose()

    them = sorted(segments, key=lambda s: s.start_s)
    assert them[0].speaker_label == "Alice"  # recognized across meetings by voiceprint
    assert them[1].speaker_label == "Speaker 2"  # the other speaker is unknown
    alice = next(c for c in clusters if c.ordinal == 1)
    assert alice.identity is not None and alice.identity.display_name == "Alice"
    assert not alice.locked  # auto-recognition is provisional; a manual rename still wins


class _BoomDiarizer:
    def diarize(self, samples: object, *, sample_rate: int) -> DiarizationResult:
        raise AssertionError("diarizer must not run when there are no Them segments")


async def test_rediarize_skips_when_no_them_segments(tmp_path: Path) -> None:
    # An ASR failure (e.g. whisper Metal dies mid-meeting) leaves the meeting with no Them
    # segments; the refine must no-op rather than diarize an empty meeting + mint a phantom speaker.
    database = _make_db(tmp_path)
    settings = Settings()
    settings.output_dir = tmp_path

    async with database.session() as session:
        meeting = await MeetingService(session).create(
            title="T", folder="mtg", started_at=datetime.now(UTC)
        )
        await MeetingService(session).add_segment(
            meeting.id, stream=Stream.ME, speaker_label="Me", text="mine", start_s=0.0, end_s=2.0
        )
    _write_silent_wav(tmp_path / "mtg" / "audio.wav")

    result = await rediarize_meeting(
        meeting.id, database=database, settings=settings, diarizer=_BoomDiarizer()
    )

    assert result.speaker_count == 0
    assert result.segments_relabeled == 0
    async with database.session() as session:
        clusters = await SpeakerService(session).list_clusters(meeting.id)
    await database.dispose()
    assert clusters == []  # no phantom "Speaker 1" minted on an empty meeting

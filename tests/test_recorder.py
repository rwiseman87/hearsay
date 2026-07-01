"""MeetingAudioRecorder: one timeline-accurate stereo (Me=L, Them=R) WAV."""

from __future__ import annotations

import wave
from array import array
from pathlib import Path

from hearsay.enums import Stream
from hearsay.transcript.recorder import (
    SAMPLE_RATE,
    MeetingAudioRecorder,
    read_them_channel,
)

_FULL = round(0.9 * 32767)  # a sample normalized to the 0.9 playback peak


def _read_stereo(path: Path) -> tuple[list[int], list[int]]:
    with wave.open(str(path)) as wav:
        assert wav.getnchannels() == 2
        assert wav.getsampwidth() == 2
        assert wav.getframerate() == SAMPLE_RATE
        frames = wav.readframes(wav.getnframes())
    interleaved = list(array("h", frames))
    return interleaved[0::2], interleaved[1::2]  # (left = Me, right = Them)


def test_channels_are_separate_and_timeline_accurate(tmp_path: Path) -> None:
    rec = MeetingAudioRecorder(tmp_path / "audio.wav")
    quarter = SAMPLE_RATE // 4  # 0.25 s
    rec.write([0.5] * quarter, t0_s=0.0, stream=Stream.THEM)  # Them in the first span ...
    rec.write([0.25] * quarter, t0_s=0.0, stream=Stream.ME)  # ... Me, same span, other channel
    # A later span at 0.5 s -> a 0.25 s jump (> _RESYNC_GAP) re-anchors, leaving a silent gap.
    rec.write([0.75] * quarter, t0_s=0.5, stream=Stream.THEM)
    rec.close()

    left, right = _read_stereo(tmp_path / "audio.wav")
    assert len(left) == len(right) == SAMPLE_RATE // 2 + quarter  # 0.5 s + trailing 0.25 s
    # Them's 0.75 span is the overall peak -> normalized to 0.9.
    assert abs(right[SAMPLE_RATE // 2] - _FULL) <= 1
    # Me and Them are separate channels (never summed together): both present in the first span,
    # and the 0.25 : 0.5 level ratio is preserved.
    assert left[0] != 0 and right[0] != 0
    assert abs(right[0] - 2 * left[0]) <= 1
    assert left[quarter] == 0 and right[quarter] == 0  # the gap is silence in both
    assert left[SAMPLE_RATE // 2] == 0  # Me is silent during Them's later span


def test_places_jittery_chunks_contiguously(tmp_path: Path) -> None:
    rec = MeetingAudioRecorder(tmp_path / "audio.wav")
    # Chunks whose t0_s jitters a few ms short of contiguous must NOT leave holes: 160-sample
    # (10 ms) chunks whose t0_s advances only ~9 ms -> would gap if placed by t0_s.
    for i in range(50):
        rec.write([0.5] * 160, t0_s=i * 0.009, stream=Stream.THEM)
    rec.close()

    _, right = _read_stereo(tmp_path / "audio.wav")
    assert len(right) == 50 * 160  # contiguous: exactly the samples written, no silent gaps
    assert all(s != 0 for s in right)  # no dropouts punched through the content


def test_pads_leading_silence(tmp_path: Path) -> None:
    rec = MeetingAudioRecorder(tmp_path / "audio.wav")
    rec.write([0.5] * 100, t0_s=1.0, stream=Stream.ME)  # first audio arrives at meeting time 1 s
    rec.close()

    left, right = _read_stereo(tmp_path / "audio.wav")
    assert len(left) == SAMPLE_RATE + 100  # 1 s of leading silence, then the chunk
    assert left[SAMPLE_RATE - 1] == 0
    assert abs(left[SAMPLE_RATE] - _FULL) <= 1  # the lone 0.5 chunk normalized to 0.9
    assert all(s == 0 for s in right)  # Them never wrote


def test_normalizes_a_quiet_capture_preserving_balance(tmp_path: Path) -> None:
    rec = MeetingAudioRecorder(tmp_path / "audio.wav")
    quarter = SAMPLE_RATE // 4
    rec.write([0.1] * quarter, t0_s=0.0, stream=Stream.THEM)  # a quiet capture (peak 0.1) ...
    rec.write([0.05] * quarter, t0_s=0.5, stream=Stream.THEM)  # ... with a half-level span
    rec.close()

    _, right = _read_stereo(tmp_path / "audio.wav")
    assert abs(max(abs(s) for s in right) - _FULL) <= 1  # scaled up so the peak reaches 0.9
    assert abs(right[SAMPLE_RATE // 2] - round(0.45 * 32767)) <= 1  # the 2:1 ratio is preserved


def test_no_writes_no_file(tmp_path: Path) -> None:
    rec = MeetingAudioRecorder(tmp_path / "audio.wav")
    rec.close()
    assert not (tmp_path / "audio.wav").exists()


def test_read_them_channel_returns_the_right_channel(tmp_path: Path) -> None:
    rec = MeetingAudioRecorder(tmp_path / "audio.wav")
    quarter = SAMPLE_RATE // 4
    rec.write([0.5] * quarter, t0_s=0.0, stream=Stream.THEM)  # Them (0.5) is the peak ...
    rec.write([0.25] * quarter, t0_s=0.0, stream=Stream.ME)  # ... Me (0.25) is on the other channel
    rec.close()

    them, rate = read_them_channel(tmp_path / "audio.wav")
    assert rate == SAMPLE_RATE
    # The refine reads Them only: its peak is 0.5 -> factor 0.9/0.5 = 1.8 -> 0.9 (not Me's 0.45).
    assert abs(float(them.max()) - 0.9) < 0.01


def test_read_them_channel_falls_back_to_mono(tmp_path: Path) -> None:
    path = tmp_path / "old.wav"  # an older single-channel recording still refines
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(SAMPLE_RATE)
        wav.writeframes(array("h", [10000, -10000, 5000]).tobytes())

    them, rate = read_them_channel(path)
    assert rate == SAMPLE_RATE
    assert len(them) == 3

"""Audio recorders: ThemAudioRecorder (refine) + MeetingAudioRecorder (playback mix)."""

from __future__ import annotations

import wave
from array import array
from pathlib import Path

from hearsay.enums import Stream
from hearsay.transcript.recorder import (
    SAMPLE_RATE,
    MeetingAudioRecorder,
    ThemAudioRecorder,
    read_offset_s,
)


def test_records_offset_and_frames(tmp_path: Path) -> None:
    recorder = ThemAudioRecorder(tmp_path / "nested" / "them.wav")
    assert recorder.start_offset_s is None  # nothing written yet -> no file, no offset

    recorder.write((0.0, 0.5, -0.5, 1.0), t0_s=1.25)
    recorder.write((0.1, -0.2), t0_s=2.0)  # later t0 does not move the start offset
    recorder.close()

    assert recorder.start_offset_s == 1.25
    with wave.open(str(recorder.path)) as wav:
        assert wav.getnchannels() == 1
        assert wav.getsampwidth() == 2
        assert wav.getframerate() == 16_000
        assert wav.getnframes() == 6


def test_no_file_when_nothing_written(tmp_path: Path) -> None:
    recorder = ThemAudioRecorder(tmp_path / "them.wav")
    recorder.close()  # close before any write must not create or crash
    assert not recorder.path.exists()


def test_offset_sidecar_roundtrips(tmp_path: Path) -> None:
    recorder = ThemAudioRecorder(tmp_path / "them.wav")
    recorder.write((0.1, 0.2), t0_s=3.5)
    recorder.close()
    assert read_offset_s(tmp_path / "them.wav") == 3.5  # read by an out-of-process rediarize


def test_read_offset_absent_is_zero(tmp_path: Path) -> None:
    assert read_offset_s(tmp_path / "missing.wav") == 0.0


def _read_wav_samples(path: Path) -> list[int]:
    with wave.open(str(path)) as wav:
        assert wav.getnchannels() == 1
        assert wav.getsampwidth() == 2
        assert wav.getframerate() == SAMPLE_RATE
        frames = wav.readframes(wav.getnframes())
    return list(array("h", frames))


def test_meeting_audio_mixes_overlap_and_is_timeline_accurate(tmp_path: Path) -> None:
    rec = MeetingAudioRecorder(tmp_path / "audio.wav")
    quarter = SAMPLE_RATE // 4  # 0.25 s
    # Me and Them cover the same first 0.25 s -> their samples sum to 0.75 (the mix peak).
    rec.write([0.5] * quarter, t0_s=0.0, stream=Stream.THEM)
    rec.write([0.25] * quarter, t0_s=0.0, stream=Stream.ME)
    # A later 0.75 span at 0.5 s -> a 0.25 s jump (> _RESYNC_GAP) re-anchors, leaving a silent gap.
    rec.write([0.75] * quarter, t0_s=0.5, stream=Stream.THEM)
    rec.close()

    samples = _read_wav_samples(tmp_path / "audio.wav")
    # The mix is peak-normalized to 0.9, so both 0.75 spans land at full playback level.
    full = round(0.9 * 32767)
    assert len(samples) == SAMPLE_RATE // 2 + quarter  # 0.5 s offset + trailing 0.25 s chunk
    assert samples[0] == full  # 0.5 + 0.25 mixed == the peak
    assert samples[quarter] == 0  # the gap is silence
    assert samples[SAMPLE_RATE // 2] == full


def test_meeting_audio_places_jittery_chunks_contiguously(tmp_path: Path) -> None:
    rec = MeetingAudioRecorder(tmp_path / "audio.wav")
    # Chunks whose t0_s jitters a few ms short of contiguous must NOT leave holes: 160-sample
    # (10 ms) chunks, but each t0_s advances only ~9 ms -> would gap if placed by t0_s.
    for i in range(50):
        rec.write([0.5] * 160, t0_s=i * 0.009, stream=Stream.THEM)
    rec.close()

    samples = _read_wav_samples(tmp_path / "audio.wav")
    assert len(samples) == 50 * 160  # contiguous: exactly the samples written, no silent gaps
    assert all(s != 0 for s in samples)  # no dropouts punched through the content


def test_meeting_audio_pads_leading_silence(tmp_path: Path) -> None:
    rec = MeetingAudioRecorder(tmp_path / "audio.wav")
    rec.write([0.5] * 100, t0_s=1.0, stream=Stream.ME)  # first audio arrives at meeting time 1 s
    rec.close()

    samples = _read_wav_samples(tmp_path / "audio.wav")
    assert len(samples) == SAMPLE_RATE + 100  # 1 s of leading silence, then the chunk
    assert samples[SAMPLE_RATE - 1] == 0
    assert samples[SAMPLE_RATE] == round(0.9 * 32767)  # the lone 0.5 chunk normalized to 0.9


def test_meeting_audio_normalizes_a_quiet_mix(tmp_path: Path) -> None:
    rec = MeetingAudioRecorder(tmp_path / "audio.wav")
    quarter = SAMPLE_RATE // 4
    rec.write([0.1] * quarter, t0_s=0.0, stream=Stream.THEM)  # a quiet capture (peak 0.1) ...
    rec.write([0.05] * quarter, t0_s=0.5, stream=Stream.THEM)  # ... with a half-level span
    rec.close()

    samples = _read_wav_samples(tmp_path / "audio.wav")
    # Scaled up so the peak reaches 0.9; the 2:1 level ratio is preserved.
    assert max(abs(s) for s in samples) == round(0.9 * 32767)
    assert samples[SAMPLE_RATE // 2] == round(0.45 * 32767)


def test_meeting_audio_no_writes_no_file(tmp_path: Path) -> None:
    rec = MeetingAudioRecorder(tmp_path / "audio.wav")
    rec.close()
    assert not (tmp_path / "audio.wav").exists()

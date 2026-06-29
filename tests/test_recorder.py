"""ThemAudioRecorder -- streaming WAV writer for post-meeting re-diarization."""

from __future__ import annotations

import wave
from pathlib import Path

from hearsay.transcript.recorder import ThemAudioRecorder, read_offset_s


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

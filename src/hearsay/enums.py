"""Shared enumerations.

Per CLAUDE.md, ``StrEnum`` is used for DB and JSON serialization. The binary IPC
frame header uses integer codes instead; the mapping lives in
``hearsay.helper.protocol`` so the wire format stays decoupled from these names.
"""

from __future__ import annotations

from enum import StrEnum


class Environment(StrEnum):
    DEVELOPMENT = "development"
    STAGING = "staging"
    PRODUCTION = "production"


class Stream(StrEnum):
    """Which capture channel a frame/segment came from."""

    ME = "me"  # local microphone
    THEM = "them"  # system audio output (remote participants)


class MeetingStatus(StrEnum):
    """Lifecycle state of a meeting row."""

    RECORDING = "recording"
    FINALIZED = "finalized"


class ASRBackendKind(StrEnum):
    """Which ASR implementation transcribes audio (selected in settings)."""

    # FluidAudio Parakeet TDT on the ANE via the hearsay-asr sidecar (default; no Metal,
    # so it dodges whisper.cpp's unrecoverable Metal command-buffer failures).
    PARAKEET = "parakeet"
    WHISPERCPP = "whispercpp"  # pywhispercpp (Metal+CoreML; torch-free)
    MLX = "mlx"  # mlx-whisper (opt-in; needs the `accel` extra, pulls torch)


class DiarizationBackendKind(StrEnum):
    """Which speaker-embedding implementation diarizes the Them stream."""

    ONNX = "onnx"  # torch-free ONNX embeddings on onnxruntime (default)
    PYANNOTE = "pyannote"  # opt-in; pulls torch + a gated HF model (see docs)


class OfflineDiarizerKind(StrEnum):
    """Which offline (post-meeting) diarizer relabels the recorded Them track."""

    # FluidAudio pyannote community-1 CoreML, run on the ANE via the hearsay-diarize
    # helper (default; torch-free, ungated -- no HF token).
    FLUIDAUDIO = "fluidaudio"
    # In-process pyannote (opt-in; pulls torch + needs a gated HF model).
    PYANNOTE = "pyannote"


class SampleFormat(StrEnum):
    INT16 = "int16"
    FLOAT32 = "float32"


class FrameType(StrEnum):
    AUDIO = "audio"
    HELLO = "hello"
    HEARTBEAT = "heartbeat"
    EOS = "eos"


class ActiveSpeakerMode(StrEnum):
    OFF = "off"
    OCR = "ocr"
    AX = "ax"


class NameHintSource(StrEnum):
    OCR = "ocr"
    AX = "ax"
    ROSTER = "roster"
    MANUAL = "manual"

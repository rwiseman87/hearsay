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

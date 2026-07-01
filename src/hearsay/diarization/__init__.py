"""Speaker diarization: offline whole-track diarization + cross-meeting voiceprints.

All inference runs in the Swift ``hearsay-diarize`` helper (FluidAudio's pyannote
community-1 CoreML diarizer on the Apple Neural Engine -- no torch, no gated model).
:class:`FluidAudioDiarizer` shells out to it and returns a :class:`DiarizationResult`:
the speaker turns plus each speaker's mean voiceprint, which the post-meeting refine
stores on a cluster and matches against people named in prior meetings.
"""

from __future__ import annotations

from hearsay.diarization.offline import (
    DiarizationResult,
    FluidAudioDiarizer,
    OfflineDiarizer,
    SpeakerTurn,
    assign_segment_speaker,
    build_offline_diarizer,
    diarize_helper_path,
    order_speakers,
)
from hearsay.diarization.voiceprint import (
    centroid_from_bytes,
    centroid_to_bytes,
    match_identity,
)

__all__ = [
    "DiarizationResult",
    "FluidAudioDiarizer",
    "OfflineDiarizer",
    "SpeakerTurn",
    "assign_segment_speaker",
    "build_offline_diarizer",
    "centroid_from_bytes",
    "centroid_to_bytes",
    "diarize_helper_path",
    "match_identity",
    "order_speakers",
]

"""Speaker diarization: torch-free speaker embeddings + (Phase 3) online clustering.

The :class:`SpeakerEmbedder` seam turns one Them utterance into a fixed-dimension
voiceprint. The default backend runs a license-clean ONNX model (wespeaker CAM++) on
the onnxruntime we already use for VAD, with kaldi-native-fbank for the exact reference
features -- no torch, no gated model. The fusion engine clusters these embeddings into
stable "Speaker N" identities (next increment).
"""

from __future__ import annotations

from hearsay.diarization.base import SpeakerEmbedder
from hearsay.diarization.manager import (
    KNOWN_EMBEDDING_MODELS,
    EmbeddingModel,
    build_embedder,
    download_embedding_model,
    embedding_model_path,
    resolve_embedding_model,
)
from hearsay.diarization.offline import (
    FluidAudioDiarizer,
    OfflineDiarizer,
    PyannoteDiarizer,
    SpeakerTurn,
    assign_segment_speaker,
    build_offline_diarizer,
    diarize_helper_path,
    order_speakers,
)
from hearsay.diarization.onnx_embedder import OnnxSpeakerEmbedder
from hearsay.diarization.voiceprint import (
    centroid_from_bytes,
    centroid_to_bytes,
    match_identity,
)

__all__ = [
    "KNOWN_EMBEDDING_MODELS",
    "EmbeddingModel",
    "FluidAudioDiarizer",
    "OfflineDiarizer",
    "OnnxSpeakerEmbedder",
    "PyannoteDiarizer",
    "SpeakerEmbedder",
    "SpeakerTurn",
    "assign_segment_speaker",
    "build_embedder",
    "build_offline_diarizer",
    "diarize_helper_path",
    "centroid_from_bytes",
    "centroid_to_bytes",
    "download_embedding_model",
    "embedding_model_path",
    "match_identity",
    "order_speakers",
    "resolve_embedding_model",
]

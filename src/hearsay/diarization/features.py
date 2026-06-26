"""Input features for the speaker-embedding models: Kaldi fbank + mean normalization.

The wespeaker / 3D-Speaker ONNX embedders expect 80-dim Kaldi filterbank features
(povey window, no dither at inference) on int16-scaled samples, with per-utterance
mean normalization. ``kaldi-native-fbank`` is the same C++ extractor those models were
built with, so the features are bit-identical to training -- no hand-rolled approximation
to drift out of distribution.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import numpy as np

SAMPLE_RATE = 16_000
NUM_MEL_BINS = 80
# Models are trained on Kaldi features over int16-range samples; ours arrive as float
# in [-1, 1], so scale up. (wespeaker CAM++ metadata: normalize_samples=0.)
_INT16_SCALE = 32768.0


def compute_fbank(samples: Sequence[float], *, sample_rate: int = SAMPLE_RATE) -> np.ndarray:
    """80-dim Kaldi fbank for ``samples`` (16 kHz mono float), mean-normalized over time.

    Returns a ``(num_frames, 80)`` float32 array; ``(0, 80)`` if too short to frame.
    """
    import kaldi_native_fbank as knf  # noqa: PLC0415 (optional dep; only when diarizing)
    import numpy as np  # noqa: PLC0415

    opts = knf.FbankOptions()
    opts.frame_opts.samp_freq = float(sample_rate)
    opts.frame_opts.dither = 0.0
    opts.frame_opts.snip_edges = True
    opts.frame_opts.window_type = "povey"
    opts.mel_opts.num_bins = NUM_MEL_BINS

    extractor = knf.OnlineFbank(opts)
    audio = np.asarray(samples, dtype=np.float32) * _INT16_SCALE
    extractor.accept_waveform(float(sample_rate), audio)
    extractor.input_finished()
    n = extractor.num_frames_ready
    if n == 0:
        return np.zeros((0, NUM_MEL_BINS), dtype=np.float32)
    feats = np.array([extractor.get_frame(i) for i in range(n)], dtype=np.float32)
    normalized: np.ndarray = feats - feats.mean(axis=0, keepdims=True)  # cepstral mean norm
    return normalized

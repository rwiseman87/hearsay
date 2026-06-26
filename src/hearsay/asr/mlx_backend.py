"""MLX ASR backend (mlx-whisper) — opt-in via the ``accel`` extra.

Apple-Silicon MLX runtime; often faster on M-series, but pulls torch + mlx + friends,
so it stays optional. ``model`` is an MLX-format Hugging Face repo
(e.g. ``mlx-community/whisper-large-v3-turbo``) or a local path. Segment start/end
are in seconds.
"""

from __future__ import annotations

from collections.abc import Sequence

from hearsay.asr.base import ASRSegment


class MlxBackend:
    def __init__(self, model: str) -> None:
        self._model = model

    @property
    def name(self) -> str:
        return "mlx"

    @property
    def model(self) -> str:
        return self._model

    def transcribe(
        self, samples: Sequence[float], *, language: str | None = None
    ) -> list[ASRSegment]:
        import mlx_whisper  # noqa: PLC0415 (optional dep)
        import numpy as np  # noqa: PLC0415 (optional dep)

        audio = np.asarray(samples, dtype=np.float32)
        result = mlx_whisper.transcribe(
            audio, path_or_hf_repo=self._model, language=language, word_timestamps=False
        )
        return [
            ASRSegment(
                text=seg["text"].strip(), start_s=float(seg["start"]), end_s=float(seg["end"])
            )
            for seg in result["segments"]
        ]

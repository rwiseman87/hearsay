"""whisper.cpp ASR backend (pywhispercpp) — the default, torch-free path.

pywhispercpp bundles libwhisper (Metal + CoreML) and pulls no heavy Python deps.
``model`` is a known name (e.g. ``large-v3-turbo``, auto-downloaded to ``models_dir``)
or a path to a local GGML file. ``Segment.t0``/``t1`` are centiseconds.
"""

from __future__ import annotations

import logging
from collections.abc import Sequence
from pathlib import Path

from hearsay.asr.base import ASRSegment


class WhisperCppBackend:
    def __init__(self, model: str, *, models_dir: Path | None = None) -> None:
        from pywhispercpp.model import Model  # noqa: PLC0415 (optional dep)

        # Quiet pywhispercpp's per-utterance INFO logs and whisper.cpp's C++ stderr
        # spam (otherwise both flood the server logs on every transcription).
        logging.getLogger("pywhispercpp").setLevel(logging.WARNING)
        self._model_name = model
        self._impl = Model(
            model,
            models_dir=str(models_dir) if models_dir else None,
            redirect_whispercpp_logs_to=None,
            print_progress=False,
        )

    @property
    def name(self) -> str:
        return "whispercpp"

    @property
    def model(self) -> str:
        return self._model_name

    def transcribe(
        self, samples: Sequence[float], *, language: str | None = None
    ) -> list[ASRSegment]:
        import numpy as np  # noqa: PLC0415 (optional dep)

        audio = np.asarray(samples, dtype=np.float32)
        if language:
            segments = self._impl.transcribe(audio, language=language)
        else:
            segments = self._impl.transcribe(audio)
        return [
            ASRSegment(
                text=segment.text.strip(), start_s=segment.t0 / 100.0, end_s=segment.t1 / 100.0
            )
            for segment in segments
        ]

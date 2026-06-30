"""whisper.cpp ASR backend (pywhispercpp) — the default, torch-free path.

pywhispercpp bundles libwhisper (Metal + CoreML) and pulls no heavy Python deps.
``model`` is a known name (e.g. ``large-v3-turbo``, auto-downloaded to ``models_dir``)
or a path to a local GGML file. ``Segment.t0``/``t1`` are centiseconds.
"""

from __future__ import annotations

import logging
from collections.abc import Sequence
from pathlib import Path
from typing import Any

from hearsay.asr.base import ASRSegment


class WhisperCppBackend:
    def __init__(self, model: str, *, models_dir: Path | None = None, beam_size: int = 5) -> None:
        from pywhispercpp.model import Model  # noqa: PLC0415 (optional dep)

        # Quiet pywhispercpp's per-utterance INFO logs and whisper.cpp's C++ stderr
        # spam (otherwise both flood the server logs on every transcription).
        logging.getLogger("pywhispercpp").setLevel(logging.WARNING)
        self._model_name = model
        # beam_size > 1 selects beam search (sampling strategy 1); 1 keeps greedy (0).
        params: dict[str, Any] = {}
        if beam_size > 1:
            # whisper.cpp's beam_search struct needs both keys; patience -1.0 = its default.
            params["beam_search"] = {"beam_size": beam_size, "patience": -1.0}
        self._impl = Model(
            model,
            models_dir=str(models_dir) if models_dir else None,
            redirect_whispercpp_logs_to=None,
            print_progress=False,
            params_sampling_strategy=1 if beam_size > 1 else 0,
            **params,
        )

    @property
    def name(self) -> str:
        return "whispercpp"

    @property
    def model(self) -> str:
        return self._model_name

    def transcribe(
        self, samples: Sequence[float], *, language: str | None = None, prompt: str | None = None
    ) -> list[ASRSegment]:
        import numpy as np  # noqa: PLC0415 (optional dep)

        audio = np.asarray(samples, dtype=np.float32)
        params: dict[str, Any] = {}
        if language:
            params["language"] = language
        if prompt:
            params["initial_prompt"] = prompt
        segments = self._impl.transcribe(audio, **params)
        return [
            ASRSegment(
                text=segment.text.strip(), start_s=segment.t0 / 100.0, end_s=segment.t1 / 100.0
            )
            for segment in segments
        ]

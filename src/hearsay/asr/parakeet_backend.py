"""Parakeet ASR backend: a persistent Swift sidecar running FluidAudio on the ANE.

Live transcription used to run whisper.cpp in-process on Metal, whose backend can enter
an unrecoverable command-buffer error state and silently stop producing transcripts. This
backend instead owns a long-lived ``hearsay-asr`` subprocess that loads Parakeet TDT once
and transcribes each VAD utterance the pipeline hands it, over a tiny stdio protocol:

    request  (stdin):  <uint32 LE sample count><that many float32 LE samples>
    response (stdout): {"text": "..."}\\n

The model stays warm for the meeting, so per-utterance latency is tens of milliseconds.
``transcribe`` is called serially by the pipeline (off the loop, under its ASR lock); a
local lock additionally guards the pipe against the known stop-teardown race where a
detached in-flight call outlives its cancelled task.
"""

from __future__ import annotations

import contextlib
import json
import struct
import subprocess
import threading
from collections.abc import Sequence
from pathlib import Path

from hearsay.asr.base import SAMPLE_RATE, ASRSegment
from hearsay.log import get_logger

_log = get_logger("hearsay.asr")


class ParakeetBackend:
    """ASR via the persistent ``hearsay-asr`` sidecar (FluidAudio Parakeet on the ANE)."""

    def __init__(self, *, binary_path: Path, version: str = "v3") -> None:
        self._binary_path = binary_path
        self._version = version
        self._proc: subprocess.Popen[bytes] | None = None
        self._lock = threading.Lock()

    @property
    def name(self) -> str:
        return "parakeet"

    @property
    def model(self) -> str:
        return f"parakeet-tdt-{self._version}"

    def _ensure_process(self) -> subprocess.Popen[bytes]:
        if self._proc is not None and self._proc.poll() is None:
            return self._proc
        if not self._binary_path.exists():
            raise RuntimeError(
                f"hearsay-asr not found at {self._binary_path}; build it with "
                "`swift build --package-path helper`"
            )
        _log.info("starting Parakeet ASR sidecar (%s)", self._binary_path)
        # Fixed argv (no shell). stderr is discarded (FluidAudio is chatty); a startup failure
        # surfaces as EOF on stdout in transcribe().
        self._proc = subprocess.Popen(
            [str(self._binary_path)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        return self._proc

    def transcribe(
        self, samples: Sequence[float], *, language: str | None = None, prompt: str | None = None
    ) -> list[ASRSegment]:
        # Parakeet has no whisper-style decoding prompt; language is auto (v3 is multilingual).
        import numpy as np  # noqa: PLC0415 (optional dep; only present with the asr extra)

        pcm = np.ascontiguousarray(np.asarray(samples, dtype=np.float32))
        with self._lock:
            proc = self._ensure_process()
            stdin, stdout = proc.stdin, proc.stdout
            if stdin is None or stdout is None:  # pragma: no cover - Popen always sets PIPEs
                raise RuntimeError("hearsay-asr sidecar has no stdio pipes")
            try:
                stdin.write(struct.pack("<I", len(pcm)) + pcm.tobytes())
                stdin.flush()
                line = stdout.readline()
            except (BrokenPipeError, ValueError) as exc:
                raise RuntimeError("hearsay-asr sidecar write failed (process exited?)") from exc
            if not line:
                raise RuntimeError("hearsay-asr sidecar produced no output (model load failed?)")
            text = str(json.loads(line).get("text", ""))
        if not text:
            return []
        return [ASRSegment(text=text, start_s=0.0, end_s=len(pcm) / SAMPLE_RATE)]

    def close(self) -> None:
        proc, self._proc = self._proc, None
        if proc is None:
            return
        with contextlib.suppress(Exception):
            if proc.stdin is not None:
                proc.stdin.close()  # EOF -> the sidecar exits its read loop
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()

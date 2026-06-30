"""Offline ASR eval: re-run a recorded WAV through VAD + whisper with chosen decode
settings, to A/B accuracy levers (model, beam search, context carryover) on real audio.

A dev harness for tuning (not shipped). It mirrors the live pipeline -- Silero VAD
segmentation + per-utterance whisper.cpp transcription with the same context-carryover
rule -- so the output reflects what the app produces. Window with --start/--end to focus
on one region (e.g. a known error) instead of transcribing the whole file each run.

    uv run python scripts/transcribe_eval.py outputs/recordings/<mtg>/them.wav \
        --model large-v3-turbo --beam-size 1 --no-context --start 0 --end 75
"""

from __future__ import annotations

import wave
from pathlib import Path

import click
import numpy as np

from hearsay.asr.whispercpp_backend import WhisperCppBackend
from hearsay.config.settings import Settings
from hearsay.vad import Segmenter
from hearsay.vad.silero import SileroVAD


def _load_window(path: Path, *, start_s: float, end_s: float | None) -> tuple[list[float], int]:
    with wave.open(str(path), "rb") as wav:
        sample_rate = wav.getframerate()
        raw = wav.readframes(wav.getnframes())
    pcm = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
    lo = int(start_s * sample_rate)
    hi = int(end_s * sample_rate) if end_s is not None else len(pcm)
    return [float(x) for x in pcm[lo:hi]], sample_rate


def _ts(seconds: float) -> str:
    return f"{int(seconds) // 60}:{int(seconds) % 60:02d}"


@click.command()
@click.argument("wav", type=click.Path(exists=True, dir_okay=False, path_type=Path))
@click.option("--model", default="large-v3-turbo", help="whisper.cpp model name or path")
@click.option("--beam-size", default=1, type=int, help="1 = greedy; >1 = beam search")
@click.option("--context/--no-context", default=False, help="carry previous final as prompt")
@click.option("--start", "start_s", default=0.0, type=float, help="window start (s)")
@click.option("--end", "end_s", default=None, type=float, help="window end (s); default EOF")
@click.option("--language", default="en")
@click.option("--reset-gap", "reset_gap_s", default=8.0, type=float, help="context reset gap (s)")
def main(
    wav: Path,
    model: str,
    beam_size: int,
    context: bool,
    start_s: float,
    end_s: float | None,
    language: str,
    reset_gap_s: float,
) -> None:
    settings = Settings()
    vad_path = settings.vad.model_path
    assert vad_path is not None  # filled by Settings' validator
    samples, _ = _load_window(wav, start_s=start_s, end_s=end_s)
    segmenter = Segmenter(
        SileroVAD(vad_path),
        threshold=settings.vad.threshold,
        min_speech_ms=settings.vad.min_speech_ms,
        min_silence_ms=settings.vad.min_silence_ms,
        partial_ms=0,  # finals only -- partials are live-UI noise here
    )
    asr = WhisperCppBackend(model, models_dir=settings.models_dir, beam_size=beam_size)

    click.echo(f"# model={model} beam={beam_size} context={context} window=[{start_s},{end_s}]")
    utterances = list(segmenter.push(samples, t0_s=start_s))
    tail = segmenter.flush()
    if tail is not None:
        utterances.append(tail)

    prev_text = ""
    prev_end_s = 0.0
    for utterance in utterances:
        if not utterance.is_final:
            continue
        prompt: str | None = None
        if context and prev_text and (utterance.start_s - prev_end_s) <= reset_gap_s:
            prompt = prev_text
        segments = asr.transcribe(utterance.samples, language=language, prompt=prompt)
        text = " ".join(s.text.strip() for s in segments).strip()
        if not text:
            continue
        click.echo(f"[{_ts(utterance.start_s)}-{_ts(utterance.end_s)}] {text}")
        prev_text, prev_end_s = text, utterance.end_s


if __name__ == "__main__":
    main()

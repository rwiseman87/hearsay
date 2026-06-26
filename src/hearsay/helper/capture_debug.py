"""`capture-debug`: run the helper for a few seconds and dump me.wav / them.wav.

Phase 0 truth test harness. With ``--synthetic`` it exercises the whole IPC pipe
off-device (no mic / TCC); without it, it captures real mic + system audio.
"""

from __future__ import annotations

import asyncio
import contextlib
import math
import tempfile
import wave
from array import array
from collections.abc import Sequence
from pathlib import Path

import click

from hearsay.enums import Stream
from hearsay.helper.control_channel import ControlChannel
from hearsay.helper.media_channel import AudioChunk, MediaChannel
from hearsay.helper.supervisor import HelperSupervisor, SupervisorError
from hearsay.log import get_logger

_log = get_logger("hearsay.capture_debug")
SAMPLE_RATE = 16_000
# The first real capture builds the mic engine + Core Audio tap, which blocks on
# the macOS TCC prompts until the user clicks Allow. Allow plenty of time for that.
START_CAPTURE_TIMEOUT = 120.0


def rms(samples: Sequence[float]) -> float:
    if not samples:
        return 0.0
    return math.sqrt(sum(s * s for s in samples) / len(samples))


def write_wav(path: Path, samples: Sequence[float], rate: int = SAMPLE_RATE) -> None:
    """Write mono 16-bit PCM at ``rate`` (floats clamped to int16)."""
    pcm = array("h", (max(-32768, min(32767, round(s * 32767))) for s in samples))
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(rate)
        wav.writeframes(pcm.tobytes())


async def _print_events(control: ControlChannel) -> None:
    """Surface notable helper events (tap health, status, errors) as they arrive."""
    async for event in control.events():
        if event.event in ("tap_health", "mic_health", "status", "error"):
            click.echo(f"event {event.event}: {event.data}")


async def _drain(
    media: MediaChannel,
    stream: Stream,
    buffers: dict[Stream, list[float]],
    spans: dict[Stream, tuple[int, int] | None],
) -> None:
    queue = media.queues[stream]
    while True:
        chunk: AudioChunk | None = await queue.get()
        if chunk is None:
            return
        buffers[stream].extend(chunk.samples)
        prev = spans[stream]
        first = prev[0] if prev is not None else chunk.host_ts
        spans[stream] = (first, chunk.host_ts)


def _report(
    out_dir: Path,
    buffers: dict[Stream, list[float]],
    dropped: dict[Stream, int],
    spans: dict[Stream, tuple[int, int] | None],
) -> None:
    me_path = out_dir / "me.wav"
    them_path = out_dir / "them.wav"
    write_wav(me_path, buffers[Stream.ME])
    write_wav(them_path, buffers[Stream.THEM])

    # audio_s = samples / 16 kHz; wall_s = span of host_ts. They should match: a
    # gap is per-stream clock drift (sampling rate diverging from real time).
    click.echo("")
    click.echo(
        f"{'stream':<6}{'samples':>10}{'audio_s':>9}{'wall_s':>9}{'rms':>8}{'dropped':>9}  file"
    )
    for stream, path in ((Stream.ME, me_path), (Stream.THEM, them_path)):
        samples = buffers[stream]
        span = spans[stream]
        wall_s = (span[1] - span[0]) / 1e9 if span is not None else 0.0
        click.echo(
            f"{stream.value:<6}{len(samples):>10}{len(samples) / SAMPLE_RATE:>9.2f}{wall_s:>9.2f}"
            f"{rms(samples):>8.3f}{dropped[stream]:>9}  {path}"
        )
    me_span, them_span = spans[Stream.ME], spans[Stream.THEM]
    if me_span is not None and them_span is not None:
        skew_ms = abs(me_span[0] - them_span[0]) / 1e6
        click.echo(f"start skew: {skew_ms:.1f} ms (fusion alignment tolerance is 750 ms)")


async def run(*, helper_path: Path, seconds: float, out_dir: Path, synthetic: bool) -> int:
    out_dir.mkdir(parents=True, exist_ok=True)
    run_dir = Path(tempfile.mkdtemp(prefix="hearsay-capture-"))
    sup = HelperSupervisor(helper_path=helper_path, run_dir=run_dir, synthetic=synthetic)

    buffers: dict[Stream, list[float]] = {Stream.ME: [], Stream.THEM: []}
    spans: dict[Stream, tuple[int, int] | None] = {Stream.ME: None, Stream.THEM: None}
    dropped: dict[Stream, int] = {Stream.ME: 0, Stream.THEM: 0}
    consumers: list[asyncio.Task[None]] = []
    events_task: asyncio.Task[None] | None = None

    try:
        await sup.start()
        control = sup.control
        media = sup.media
        assert control is not None and media is not None
        click.echo(f"helper connected: {sup.hello.data if sup.hello else {}}")

        perms = await control.call("check_permissions")
        click.echo(f"permissions: {perms.result}")

        consumers = [
            asyncio.create_task(_drain(media, s, buffers, spans)) for s in (Stream.ME, Stream.THEM)
        ]
        events_task = asyncio.create_task(_print_events(control))

        if not synthetic:
            click.echo(
                "accept the macOS Microphone / System Audio Recording prompts if they appear ..."
            )
        started = await control.call(
            "start_capture",
            {"tap_mode": "global_except_self", "sample_rate": SAMPLE_RATE},
            timeout=START_CAPTURE_TIMEOUT,
        )
        if not started.ok:
            click.echo(f"start_capture failed: {started.error}", err=True)
            return 1

        click.echo(f"capturing for {seconds:.1f}s ...")
        await asyncio.sleep(seconds)
        await control.call("stop_capture")

        try:
            await asyncio.wait_for(asyncio.gather(*consumers), timeout=3.0)
        except TimeoutError:
            _log.warning("timed out waiting for eos; using samples captured so far")
        dropped = {s: media.stats[s].dropped for s in (Stream.ME, Stream.THEM)}
    except (SupervisorError, TimeoutError, ConnectionError) as exc:
        click.echo(f"error: capture did not complete ({type(exc).__name__}: {exc})", err=True)
        click.echo(
            "  if a macOS permission prompt appeared, accept it and re-run "
            "(grants are remembered).",
            err=True,
        )
        return 1
    finally:
        for task in consumers:
            if not task.done():
                task.cancel()
        if events_task is not None and not events_task.done():
            events_task.cancel()
        await sup.stop()
        for sock in (run_dir / "control.sock", run_dir / "media.sock"):
            sock.unlink(missing_ok=True)
        with contextlib.suppress(OSError):
            run_dir.rmdir()

    _report(out_dir, buffers, dropped, spans)
    return 0

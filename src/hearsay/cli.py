"""Command-line entry point."""

from __future__ import annotations

import asyncio
import json
import secrets
import socket
from pathlib import Path

import click
import uvicorn

from hearsay import __version__
from hearsay.api import create_app
from hearsay.config.settings import Settings
from hearsay.db import Database
from hearsay.diarization import download_embedding_model, resolve_embedding_model
from hearsay.helper import capture_debug as cd
from hearsay.models import Base
from hearsay.transcript import Broadcaster, HelperCapture, SessionManager
from hearsay.transcript.capture import Capture
from hearsay.vad.silero import download_silero_model


@click.group()
def main() -> None:
    """hearsay - local-first macOS meeting-note transcriber."""


@main.command()
def version() -> None:
    """Print the hearsay version."""
    click.echo(__version__)


@main.command("fetch-models")
def fetch_models() -> None:
    """Download the Silero VAD + speaker-embedding models (whisper.cpp auto-downloads)."""
    settings = Settings()
    model_path = settings.vad.model_path
    assert model_path is not None  # filled by Settings' validator
    path = download_silero_model(model_path)
    click.echo(f"Silero VAD model ready at {path}")

    embedding_model = resolve_embedding_model(settings.diarization.model)
    if embedding_model is not None:
        assert settings.models_dir is not None
        emb_path = download_embedding_model(embedding_model, settings.models_dir)
        click.echo(
            f"speaker-embedding model '{embedding_model.name}' "
            f"({embedding_model.license}) ready at {emb_path}"
        )
    else:
        click.echo(f"speaker-embedding model '{settings.diarization.model}': custom; skipped.")

    click.echo(
        f"whisper.cpp model '{settings.asr.model}' auto-downloads to "
        f"{settings.models_dir} on first transcription."
    )


def _free_port(host: str) -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind((host, 0))
        return int(sock.getsockname()[1])


@main.command()
@click.option("--host", default=None, help="Bind host (default: settings.server_host).")
@click.option("--port", default=None, type=int, help="Bind port (default: auto-pick a free port).")
def serve(host: str | None, port: int | None) -> None:
    """Run the loopback core API + WebSocket server."""
    settings = Settings()
    bind_host = host or settings.server_host
    bind_port = port if port is not None else settings.server_port
    if bind_port == 0:
        bind_port = _free_port(bind_host)

    token = secrets.token_urlsafe(32)
    app = create_app(settings, session_token=token)
    click.echo(f"hearsay core on http://{bind_host}:{bind_port}")
    click.echo(f"open: http://{bind_host}:{bind_port}/?token={token}")
    uvicorn.run(app, host=bind_host, port=bind_port, log_config=None)


async def _print_transcript_events(broadcaster: Broadcaster) -> None:
    with broadcaster.subscribe() as queue:
        while True:
            event = json.loads(await queue.get())
            click.echo(f"[{event['kind']:7}] {event['speaker_label']}: {event['text']}")


async def _run_live(settings: Settings, seconds: float, *, synthetic: bool) -> int:
    database_url = settings.database_url
    assert database_url is not None
    database = Database(database_url)
    async with database.engine.begin() as connection:
        await connection.run_sync(Base.metadata.create_all)

    capture_factory = None
    if synthetic:

        def capture_factory() -> Capture:
            return HelperCapture(helper_path=settings.helper_path, synthetic=True)

    manager = SessionManager(database=database, settings=settings, capture_factory=capture_factory)
    try:
        meeting = await manager.start_meeting(title="live-validation")
    except Exception as exc:  # surface a clean message instead of a traceback
        click.echo(f"failed to start capture/transcription: {type(exc).__name__}: {exc}", err=True)
        await database.dispose()
        return 1
    session = manager.active
    assert session is not None
    click.echo(f"recording -> {session.folder}")
    click.echo(
        "speak + play remote audio; finals persist, partials are live-only. Ctrl-C to stop.\n"
    )
    printer = asyncio.create_task(_print_transcript_events(session.broadcaster))
    try:
        await asyncio.sleep(seconds)
    finally:
        # Runs on normal completion and on Ctrl-C (KeyboardInterrupt unwinds through
        # asyncio.run, executing this cleanup before the outer handler in live()).
        printer.cancel()
        await manager.stop_meeting(meeting.id)
    transcript = session.folder / "transcript.md"
    click.echo(f"\n--- {transcript} ---")
    if transcript.exists():
        click.echo(transcript.read_text(encoding="utf-8"))
    await database.dispose()
    return 0


@main.command()
@click.option("--seconds", default=60.0, show_default=True, help="Recording duration.")
@click.option(
    "--model", default=None, help="Override the ASR model for this run (e.g. 'base' for speed)."
)
@click.option(
    "--synthetic",
    is_flag=True,
    help="Use the helper's tone source (glue smoke test; no real audio).",
)
def live(seconds: float, model: str | None, synthetic: bool) -> None:
    """Run the real capture -> transcribe pipeline and print live transcripts (validation)."""
    settings = Settings()
    if model:
        settings.asr.model = model
    try:
        code = asyncio.run(_run_live(settings, seconds, synthetic=synthetic))
    except KeyboardInterrupt:
        code = 0
    if code != 0:
        raise SystemExit(code)


@main.command("capture-debug")
@click.option("--seconds", default=5.0, show_default=True, help="Capture duration in seconds.")
@click.option(
    "--out",
    "out_dir",
    type=click.Path(file_okay=False, path_type=Path),
    default=None,
    help="Directory for me.wav / them.wav (default: <outputs>/capture-debug).",
)
@click.option(
    "--synthetic",
    is_flag=True,
    help="Use the helper's synthetic tone source (no mic / TCC prompts).",
)
@click.option(
    "--helper",
    "helper_path",
    type=click.Path(dir_okay=False, path_type=Path),
    default=None,
    help="Path to hearsay-helper (defaults to the dev build).",
)
def capture_debug(
    seconds: float, out_dir: Path | None, synthetic: bool, helper_path: Path | None
) -> None:
    """Capture from the helper for a few seconds and write me.wav / them.wav."""
    settings = Settings()
    helper = helper_path or settings.helper_path
    out = out_dir or settings.capture_debug_dir
    code = asyncio.run(
        cd.run(helper_path=helper, seconds=seconds, out_dir=out, synthetic=synthetic)
    )
    if code != 0:
        raise SystemExit(code)

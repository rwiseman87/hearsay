"""Command-line entry point."""

from __future__ import annotations

import asyncio
from pathlib import Path

import click

from hearsay import __version__
from hearsay.config.settings import Settings
from hearsay.helper import capture_debug as cd


@click.group()
def main() -> None:
    """hearsay - local-first macOS meeting-note transcriber."""


@main.command()
def version() -> None:
    """Print the hearsay version."""
    click.echo(__version__)


@main.command("capture-debug")
@click.option("--seconds", default=5.0, show_default=True, help="Capture duration in seconds.")
@click.option(
    "--out",
    "out_dir",
    type=click.Path(file_okay=False, path_type=Path),
    default=Path("capture-debug"),
    show_default=True,
    help="Directory for me.wav / them.wav.",
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
def capture_debug(seconds: float, out_dir: Path, synthetic: bool, helper_path: Path | None) -> None:
    """Capture from the helper for a few seconds and write me.wav / them.wav."""
    helper = helper_path or Settings().helper_path
    code = asyncio.run(
        cd.run(helper_path=helper, seconds=seconds, out_dir=out_dir, synthetic=synthetic)
    )
    if code != 0:
        raise SystemExit(code)

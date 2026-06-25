"""Command-line entry point."""

from __future__ import annotations

import click

from hearsay import __version__


@click.group()
def main() -> None:
    """hearsay - local-first macOS meeting-note transcriber."""


@main.command()
def version() -> None:
    """Print the hearsay version."""
    click.echo(__version__)

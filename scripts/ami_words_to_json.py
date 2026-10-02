# /// script
# requires-python = ">=3.14"
# dependencies = ["click==8.5.0"]
# ///
"""Convert AMI Meeting Corpus word annotations into a compact utterance transcript.

Reads the per-speaker ``words/<meeting>.<speaker>.words.xml`` files from the AMI public manual
annotation zip and writes ``[{speaker, start_s, end_s, text}, ...]`` sorted by start time. Truncated
word fragments, vocal sounds, and punctuation tokens are dropped: no ASR model emits them, so keeping
them would only add noise to the word error rate. A speaker's words split into a new utterance after a
silence of ``--gap`` seconds (default 1.0).

Usage:
    uv run scripts/ami_words_to_json.py outputs/ami/annotations/ami_manual.zip ES2004a \
        shared/eval/ES2004a.utterances.json

Add ``--speaker A`` to emit one speaker only (for example the single headset of a meeting).
"""

from __future__ import annotations

import json
import zipfile
from dataclasses import dataclass
from pathlib import Path
from xml.etree import ElementTree

import click

SPEAKERS = ("A", "B", "C", "D")
DEFAULT_GAP_S = 1.0


@dataclass
class Utterance:
    """One speaker's run of words with no long silence inside it."""

    speaker: str
    start_s: float
    end_s: float
    words: list[str]


def read_words(archive: zipfile.ZipFile, meeting: str, speaker: str) -> list[tuple[float, float, str]]:
    """Read one speaker's spoken words as ``(start_s, end_s, word)`` tuples in time order.

    Args:
        archive: The opened AMI manual annotation zip.
        meeting: Meeting id, for example ``ES2004a``.
        speaker: Speaker letter, ``A`` to ``D``.

    Returns:
        The words, excluding punctuation tokens, truncated fragments, and non-word events.
    """
    with archive.open(f"words/{meeting}.{speaker}.words.xml") as handle:
        root = ElementTree.parse(handle).getroot()
    words: list[tuple[float, float, str]] = []
    for element in root:
        if element.tag != "w" or element.get("punc") == "true" or element.get("trunc") == "true":
            continue
        text = (element.text or "").strip()
        start = element.get("starttime")
        end = element.get("endtime")
        if text and start is not None and end is not None:
            words.append((float(start), float(end), text))
    return sorted(words)


def to_utterances(speaker: str, words: list[tuple[float, float, str]], gap_s: float) -> list[Utterance]:
    """Group a speaker's words into utterances, splitting on silences of at least ``gap_s``."""
    utterances: list[Utterance] = []
    for start, end, text in words:
        current = utterances[-1] if utterances else None
        if current is not None and start - current.end_s < gap_s:
            current.words.append(text)
            current.end_s = max(current.end_s, end)
        else:
            utterances.append(Utterance(speaker, start, end, [text]))
    return utterances


@click.command()
@click.argument("archive_path", type=click.Path(exists=True, dir_okay=False, path_type=Path))
@click.argument("meeting")
@click.argument("out_path", type=click.Path(dir_okay=False, path_type=Path))
@click.option("--gap", "gap_s", type=float, default=DEFAULT_GAP_S, show_default=True,
              help="Silence in seconds that splits a speaker's words into a new utterance.")
@click.option("--speaker", type=click.Choice(SPEAKERS), default=None,
              help="Emit this speaker only; all speakers when omitted.")
def main(archive_path: Path, meeting: str, out_path: Path, gap_s: float, speaker: str | None) -> None:
    """Write MEETING's utterances from the AMI annotation zip ARCHIVE_PATH to OUT_PATH as JSON."""
    speakers: tuple[str, ...] = (speaker,) if speaker is not None else SPEAKERS
    utterances: list[Utterance] = []
    with zipfile.ZipFile(archive_path) as archive:
        for name in speakers:
            utterances.extend(to_utterances(name, read_words(archive, meeting, name), gap_s))
    utterances.sort(key=lambda u: (u.start_s, u.speaker))
    payload = [
        {
            "speaker": u.speaker,
            "start_s": round(u.start_s, 3),
            "end_s": round(u.end_s, 3),
            "text": " ".join(u.words),
        }
        for u in utterances
    ]
    out_path.write_text(json.dumps(payload, separators=(",", ":")) + "\n", encoding="utf-8")
    total_words = sum(len(u.words) for u in utterances)
    click.echo(f"{meeting}: {len(payload)} utterances, {total_words} words -> {out_path}")


if __name__ == "__main__":
    main()

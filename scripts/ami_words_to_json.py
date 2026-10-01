# /// script
# requires-python = ">=3.14"
# dependencies = []
# ///
"""Convert AMI Meeting Corpus word annotations into a compact utterance transcript.

Reads the per-speaker ``words/<meeting>.<speaker>.words.xml`` files from the AMI public manual
annotation zip and writes ``[{speaker, start_s, end_s, text}, ...]`` sorted by start time. Truncated
word fragments, vocal sounds, and punctuation tokens are dropped: no ASR model emits them, so keeping
them would only add noise to the word error rate. A speaker's words split into a new utterance after a
silence of ``--gap`` seconds.

Usage:
    uv run scripts/ami_words_to_json.py outputs/ami/annotations/ami_manual.zip ES2004a \
        shared/eval/ES2004a.utterances.json
"""

from __future__ import annotations

import json
import sys
import zipfile
from dataclasses import dataclass
from pathlib import Path
from xml.etree import ElementTree

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


def main(argv: list[str]) -> int:
    """Entry point: ``ami_words_to_json.py <zip> <meeting> <out.json> [gap_s]``."""
    if len(argv) not in (4, 5):
        print(__doc__, file=sys.stderr)
        return 2
    archive_path, meeting, out_path = Path(argv[1]), argv[2], Path(argv[3])
    gap_s = float(argv[4]) if len(argv) == 5 else DEFAULT_GAP_S
    utterances: list[Utterance] = []
    with zipfile.ZipFile(archive_path) as archive:
        for speaker in SPEAKERS:
            utterances.extend(to_utterances(speaker, read_words(archive, meeting, speaker), gap_s))
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
    print(f"{meeting}: {len(payload)} utterances, {total_words} words -> {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))

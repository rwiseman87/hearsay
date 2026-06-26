from __future__ import annotations

import json
import struct
from pathlib import Path

import pytest

from hearsay.enums import FrameType, SampleFormat, Stream
from hearsay.helper import protocol
from hearsay.helper.protocol import MediaFrame, canonical_frames, decode, encode

FIXTURES = Path(__file__).resolve().parents[1] / "shared" / "fixtures" / "frames.jsonl"


@pytest.mark.parametrize("frame", [f for _, f in canonical_frames()])
def test_round_trip(frame: MediaFrame) -> None:
    assert decode(encode(frame)) == frame


def test_header_size_is_28() -> None:
    assert protocol.HEADER_SIZE == 28


def test_decode_rejects_bad_magic() -> None:
    raw = bytearray(encode(canonical_frames()[0][1]))
    raw[0] = 0x00
    with pytest.raises(protocol.ProtocolError):
        decode(bytes(raw))


def test_decode_rejects_truncated_payload() -> None:
    full = encode(canonical_frames()[1][1])  # audio frame with payload
    with pytest.raises(protocol.ProtocolError):
        decode(full[:-2])


def test_expected_payload_len_audio() -> None:
    encoded = encode(canonical_frames()[1][1])  # audio_them_int16: 4 int16 samples
    assert protocol.expected_payload_len(encoded[: protocol.HEADER_SIZE]) == 8


def test_expected_payload_len_non_audio() -> None:
    encoded = encode(canonical_frames()[0][1])  # hello frame
    assert protocol.expected_payload_len(encoded) == 0


def test_audio_samples_float32() -> None:
    frame = MediaFrame(
        FrameType.AUDIO, Stream.ME, SampleFormat.FLOAT32, 0, 0, struct.pack("<3f", 0.5, -0.5, 1.0)
    )
    assert protocol.audio_samples(frame) == pytest.approx((0.5, -0.5, 1.0))


def test_audio_samples_int16_normalized() -> None:
    frame = MediaFrame(
        FrameType.AUDIO, Stream.THEM, SampleFormat.INT16, 0, 0, struct.pack("<2h", 16384, -32768)
    )
    assert protocol.audio_samples(frame) == pytest.approx((0.5, -1.0), abs=1e-4)


def test_audio_samples_empty_for_non_audio() -> None:
    assert protocol.audio_samples(canonical_frames()[0][1]) == ()


def test_committed_fixtures_match() -> None:
    assert FIXTURES.exists(), "run: uv run python scripts/gen_fixtures.py"
    for line in FIXTURES.read_text().splitlines():
        rec = json.loads(line)
        encoded = bytes.fromhex(rec["encoded_hex"])
        frame = decode(encoded)
        assert frame.type.value == rec["header"]["type"]
        assert frame.stream.value == rec["header"]["stream"]
        assert frame.format.value == rec["header"]["format"]
        assert frame.seq == rec["header"]["seq"]
        assert frame.host_ts == rec["header"]["host_ts"]
        assert frame.n_samples == rec["header"]["n_samples"]
        assert frame.payload.hex() == rec["payload_hex"]
        # re-encode must reproduce the exact bytes (the cross-language contract)
        assert encode(frame).hex() == rec["encoded_hex"]

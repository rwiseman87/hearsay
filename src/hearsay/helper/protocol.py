"""Binary media-frame codec for the helper<->core IPC.

Mirror of ``shared/protocol/ipc.md``. The Swift ``HearsayIPC.FrameCodec`` must
match this byte-for-byte; ``shared/fixtures/frames.jsonl`` pins the contract for
both implementations.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass

from hearsay.enums import FrameType, SampleFormat, Stream

MAGIC = 0xA7
VERSION = 1
HEADER_FMT = "<BBBBBBHIQII"
HEADER_SIZE = struct.calcsize(HEADER_FMT)  # 28
SAMPLE_RATE = 16_000

_TYPE_CODE: dict[FrameType, int] = {
    FrameType.AUDIO: 0,
    FrameType.HELLO: 1,
    FrameType.HEARTBEAT: 2,
    FrameType.EOS: 3,
}
_TYPE_FROM_CODE: dict[int, FrameType] = {v: k for k, v in _TYPE_CODE.items()}
_STREAM_CODE: dict[Stream, int] = {Stream.ME: 0, Stream.THEM: 1}
_STREAM_FROM_CODE: dict[int, Stream] = {v: k for k, v in _STREAM_CODE.items()}
_FORMAT_CODE: dict[SampleFormat, int] = {SampleFormat.INT16: 0, SampleFormat.FLOAT32: 1}
_FORMAT_FROM_CODE: dict[int, SampleFormat] = {v: k for k, v in _FORMAT_CODE.items()}
_BYTES_PER_SAMPLE: dict[SampleFormat, int] = {SampleFormat.INT16: 2, SampleFormat.FLOAT32: 4}


class ProtocolError(ValueError):
    """Raised when bytes do not conform to the IPC frame contract."""


def bytes_per_sample(fmt: SampleFormat) -> int:
    return _BYTES_PER_SAMPLE[fmt]


@dataclass(frozen=True, slots=True)
class MediaFrame:
    """One decoded media frame. ``n_samples`` is derived from the payload."""

    type: FrameType
    stream: Stream
    format: SampleFormat
    seq: int
    host_ts: int
    payload: bytes = b""
    flags: int = 0

    @property
    def n_samples(self) -> int:
        if self.type is not FrameType.AUDIO:
            return 0
        return len(self.payload) // _BYTES_PER_SAMPLE[self.format]


def encode(frame: MediaFrame) -> bytes:
    """Serialize a frame to header+payload bytes."""
    if frame.type is FrameType.AUDIO:
        if len(frame.payload) % _BYTES_PER_SAMPLE[frame.format] != 0:
            raise ProtocolError("audio payload length not a whole number of samples")
    elif frame.payload:
        raise ProtocolError(f"{frame.type} frame must not carry a payload")
    header = struct.pack(
        HEADER_FMT,
        MAGIC,
        VERSION,
        _TYPE_CODE[frame.type],
        _STREAM_CODE[frame.stream],
        _FORMAT_CODE[frame.format],
        frame.flags,
        0,
        frame.seq,
        frame.host_ts,
        frame.n_samples,
        0,
    )
    return header + frame.payload


def decode(buf: bytes) -> MediaFrame:
    """Decode exactly one frame from the front of ``buf``."""
    if len(buf) < HEADER_SIZE:
        raise ProtocolError("buffer shorter than header")
    magic, version, type_code, stream_code, fmt_code, flags, _r0, seq, host_ts, n_samples, _r1 = (
        struct.unpack(HEADER_FMT, buf[:HEADER_SIZE])
    )
    if magic != MAGIC:
        raise ProtocolError(f"bad magic 0x{magic:02x}")
    if version != VERSION:
        raise ProtocolError(f"unsupported version {version}")
    try:
        ftype = _TYPE_FROM_CODE[type_code]
        stream = _STREAM_FROM_CODE[stream_code]
        fmt = _FORMAT_FROM_CODE[fmt_code]
    except KeyError as exc:
        raise ProtocolError(f"unknown enum code: {exc}") from exc
    payload_len = n_samples * _BYTES_PER_SAMPLE[fmt] if ftype is FrameType.AUDIO else 0
    payload = buf[HEADER_SIZE : HEADER_SIZE + payload_len]
    if len(payload) != payload_len:
        raise ProtocolError("truncated payload")
    return MediaFrame(
        type=ftype,
        stream=stream,
        format=fmt,
        seq=seq,
        host_ts=host_ts,
        payload=payload,
        flags=flags,
    )


def expected_payload_len(header: bytes) -> int:
    """Payload byte count that follows a 28-byte header (0 for non-audio frames).

    Lets a stream reader size the second read without decoding the whole frame.
    """
    if len(header) < HEADER_SIZE:
        raise ProtocolError("buffer shorter than header")
    type_code = header[2]
    fmt_code = header[4]
    n_samples = int.from_bytes(header[20:24], "little")
    try:
        ftype = _TYPE_FROM_CODE[type_code]
        fmt = _FORMAT_FROM_CODE[fmt_code]
    except KeyError as exc:
        raise ProtocolError(f"unknown enum code in header: {exc}") from exc
    return n_samples * _BYTES_PER_SAMPLE[fmt] if ftype is FrameType.AUDIO else 0


def audio_samples(frame: MediaFrame) -> tuple[float, ...]:
    """Decode an AUDIO frame's payload to float samples in roughly [-1, 1].

    ``int16`` payloads are normalized by 32768; ``float32`` payloads pass through.
    Non-audio frames yield an empty tuple.
    """
    if frame.type is not FrameType.AUDIO:
        return ()
    if frame.format is SampleFormat.FLOAT32:
        return struct.unpack(f"<{frame.n_samples}f", frame.payload)
    return tuple(v / 32768.0 for v in struct.unpack(f"<{frame.n_samples}h", frame.payload))


def canonical_frames() -> list[tuple[str, MediaFrame]]:
    """Canonical frames used to generate the cross-language golden fixtures."""
    them_i16 = struct.pack("<4h", 0, 1, -1, 32767)
    me_f32 = struct.pack("<2f", 0.0, -1.0)
    return [
        ("hello_me_int16", MediaFrame(FrameType.HELLO, Stream.ME, SampleFormat.INT16, 0, 0)),
        (
            "audio_them_int16",
            MediaFrame(
                FrameType.AUDIO, Stream.THEM, SampleFormat.INT16, 0, 1_000_000_000, them_i16
            ),
        ),
        (
            "audio_me_float32",
            MediaFrame(
                FrameType.AUDIO, Stream.ME, SampleFormat.FLOAT32, 5, 1_234_567_890_123, me_f32
            ),
        ),
        (
            "heartbeat_them",
            MediaFrame(FrameType.HEARTBEAT, Stream.THEM, SampleFormat.INT16, 10, 2_000_000_000),
        ),
        ("eos_them", MediaFrame(FrameType.EOS, Stream.THEM, SampleFormat.INT16, 11, 2_100_000_000)),
    ]


def fixture_record(desc: str, frame: MediaFrame) -> dict[str, object]:
    """Build one golden-fixture record (header fields + hex encodings)."""
    return {
        "desc": desc,
        "header": {
            "type": frame.type.value,
            "stream": frame.stream.value,
            "format": frame.format.value,
            "seq": frame.seq,
            "host_ts": frame.host_ts,
            "flags": frame.flags,
            "n_samples": frame.n_samples,
        },
        "payload_hex": frame.payload.hex(),
        "encoded_hex": encode(frame).hex(),
    }

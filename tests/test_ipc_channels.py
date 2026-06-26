from __future__ import annotations

import asyncio
import socket
import struct

import pytest

from hearsay.enums import FrameType, SampleFormat, Stream
from hearsay.helper.control import Event, Reply, encode_event, encode_reply, parse_command
from hearsay.helper.control_channel import ControlChannel
from hearsay.helper.media_channel import MediaChannel
from hearsay.helper.protocol import MediaFrame, encode

_Conn = tuple[asyncio.StreamReader, asyncio.StreamWriter]


async def _stream_pair() -> tuple[_Conn, _Conn]:
    """A connected pair of asyncio stream endpoints (core side, helper side)."""
    left, right = socket.socketpair()
    core = await asyncio.open_connection(sock=left)
    helper = await asyncio.open_connection(sock=right)
    return core, helper


async def test_control_call_correlates_reply() -> None:
    (core_r, core_w), (helper_r, helper_w) = await _stream_pair()
    chan = ControlChannel(core_r, core_w)
    chan.start()

    async def fake_helper() -> None:
        line = await helper_r.readline()
        cmd = parse_command(line)
        assert cmd.cmd == "ping"
        helper_w.write(encode_reply(Reply(cmd.id, True, {"pong": True})))
        await helper_w.drain()

    task = asyncio.create_task(fake_helper())
    reply = await chan.call("ping", timeout=2.0)
    assert reply.ok and reply.result == {"pong": True}
    await task
    await chan.aclose()
    helper_w.close()


async def test_control_wait_for_event() -> None:
    (core_r, core_w), (_helper_r, helper_w) = await _stream_pair()
    chan = ControlChannel(core_r, core_w)
    chan.start()

    helper_w.write(encode_event(Event("level", 1, {"stream": "me", "rms": 0.1})))
    helper_w.write(encode_event(Event("hello", 2, {"pid": 42})))
    await helper_w.drain()

    hello = await chan.wait_for_event("hello", timeout=2.0)
    assert hello.data["pid"] == 42
    await chan.aclose()
    helper_w.close()


async def test_control_call_raises_on_eof() -> None:
    (core_r, core_w), (_helper_r, helper_w) = await _stream_pair()
    chan = ControlChannel(core_r, core_w)
    chan.start()
    helper_w.close()  # helper drops the connection
    with pytest.raises((ConnectionError, TimeoutError)):
        await chan.call("ping", timeout=2.0)
    await chan.aclose()


async def test_media_channel_routes_and_counts_drops() -> None:
    (core_r, _core_w), (_helper_r, helper_w) = await _stream_pair()
    chan = MediaChannel(core_r)
    chan.start()

    frames = [
        MediaFrame(FrameType.HELLO, Stream.ME, SampleFormat.FLOAT32, 0, 0),
        MediaFrame(
            FrameType.AUDIO, Stream.ME, SampleFormat.FLOAT32, 1, 10, struct.pack("<2f", 0.1, 0.2)
        ),
        MediaFrame(
            FrameType.AUDIO, Stream.THEM, SampleFormat.FLOAT32, 0, 11, struct.pack("<1f", 0.3)
        ),
        # seq jumps 0 -> 2 on THEM: one dropped frame.
        MediaFrame(
            FrameType.AUDIO, Stream.THEM, SampleFormat.FLOAT32, 2, 12, struct.pack("<1f", 0.4)
        ),
        MediaFrame(FrameType.EOS, Stream.ME, SampleFormat.FLOAT32, 2, 13),
        MediaFrame(FrameType.EOS, Stream.THEM, SampleFormat.FLOAT32, 3, 14),
    ]
    for frame in frames:
        helper_w.write(encode(frame))
    await helper_w.drain()
    helper_w.close()

    me_chunk = await asyncio.wait_for(chan.queues[Stream.ME].get(), 2)
    assert me_chunk is not None and me_chunk.samples == pytest.approx((0.1, 0.2))
    assert await asyncio.wait_for(chan.queues[Stream.ME].get(), 2) is None  # eos sentinel

    them1 = await asyncio.wait_for(chan.queues[Stream.THEM].get(), 2)
    them2 = await asyncio.wait_for(chan.queues[Stream.THEM].get(), 2)
    assert them1 is not None and them2 is not None
    assert them1.samples == pytest.approx((0.3,)) and them2.samples == pytest.approx((0.4,))
    assert await asyncio.wait_for(chan.queues[Stream.THEM].get(), 2) is None

    assert chan.stats[Stream.ME].dropped == 0
    assert chan.stats[Stream.THEM].dropped == 1
    assert chan.stats[Stream.ME].frames == 1
    await chan.aclose()

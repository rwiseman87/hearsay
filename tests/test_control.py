from __future__ import annotations

import pytest

from hearsay.helper.control import (
    Command,
    ControlError,
    Event,
    Reply,
    ReplyError,
    encode_command,
    encode_event,
    encode_reply,
    parse_command,
    parse_message,
)


def test_command_round_trip() -> None:
    cmd = Command(7, "start_capture", {"tap_mode": "global_except_self", "sample_rate": 16000})
    assert parse_command(encode_command(cmd)) == cmd


def test_command_default_args_empty() -> None:
    assert parse_command(b'{"id":1,"cmd":"ping"}').args == {}


def test_reply_golden_wire() -> None:
    # Byte-for-byte agreement with the Swift sortedKeys encoder.
    assert (
        encode_reply(Reply(1, True, {"pong": True}))
        == b'{"id":1,"ok":true,"result":{"pong":true}}\n'
    )


def test_reply_round_trip() -> None:
    reply = Reply(1, True, {"pong": True})
    assert parse_message(encode_reply(reply)) == reply


def test_reply_error_round_trip() -> None:
    reply = Reply(2, False, None, ReplyError("no_permission", "microphone denied"))
    assert parse_message(encode_reply(reply)) == reply


def test_event_round_trip() -> None:
    event = Event("tap_health", 123_456_789, {"state": "recovered", "action": "rebuilt_tap"})
    assert parse_message(encode_event(event)) == event


def test_parse_message_classifies() -> None:
    assert isinstance(parse_message(b'{"event":"hello","ts":1,"data":{}}'), Event)
    assert isinstance(parse_message(b'{"id":3,"ok":true,"result":{}}'), Reply)


def test_parse_message_rejects_garbage() -> None:
    with pytest.raises(ControlError):
        parse_message(b'{"nonsense":true}')

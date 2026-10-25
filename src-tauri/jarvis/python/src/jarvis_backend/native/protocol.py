from __future__ import annotations

import json
import struct
import zlib
from dataclasses import dataclass
from enum import IntEnum
from typing import Any

MAGIC = 0x56524A41
PROTOCOL_VERSION = 1
MAX_PAYLOAD_BYTES = 16 * 1024 * 1024
HEADER = struct.Struct("<IHHIQIII")


class ProtocolError(ValueError):
    pass


class MessageType(IntEnum):
    HELLO = 1
    START = 2
    STOP = 3
    SUBMIT = 4
    CANCEL = 5
    RESULT = 6
    STATUS = 7
    ERROR = 8
    SHUTDOWN = 9
    CONFIGURE_GAME = 10
    START_DUPLEX = 11
    STOP_DUPLEX = 12


class StatusCode(IntEnum):
    OK = 0
    MALFORMED = 1
    UNSUPPORTED_VERSION = 2
    UNAVAILABLE = 3
    CANCELLED = 4
    INTERNAL_ERROR = 5


@dataclass(frozen=True, slots=True)
class Frame:
    message_type: MessageType
    request_id: int = 0
    payload: bytes = b""
    flags: int = 0
    version: int = PROTOCOL_VERSION

    def json(self) -> dict[str, Any]:
        try:
            value = json.loads(self.payload.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ProtocolError("frame payload is not valid JSON") from exc
        if not isinstance(value, dict):
            raise ProtocolError("JSON payload must be an object")
        return value


def json_payload(value: dict[str, Any]) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def encode_frame(frame: Frame, max_frame_bytes: int = MAX_PAYLOAD_BYTES) -> bytes:
    if not 0 <= frame.request_id <= 0xFFFFFFFFFFFFFFFF:
        raise ProtocolError("request ID is outside uint64 range")
    if len(frame.payload) > min(max_frame_bytes, MAX_PAYLOAD_BYTES):
        raise ProtocolError("frame payload exceeds configured maximum")
    checksum = zlib.crc32(frame.payload) & 0xFFFFFFFF
    return (
        HEADER.pack(
            MAGIC,
            frame.version,
            int(frame.message_type),
            frame.flags,
            frame.request_id,
            len(frame.payload),
            checksum,
            0,
        )
        + frame.payload
    )


def decode_frame(
    data: bytes,
    expected_version: int = PROTOCOL_VERSION,
    max_frame_bytes: int = MAX_PAYLOAD_BYTES,
) -> Frame:
    if len(data) < HEADER.size:
        raise ProtocolError("incomplete frame header")
    values = HEADER.unpack_from(data)
    magic, version, message_type, flags, request_id, length, checksum, reserved = values
    if magic != MAGIC:
        raise ProtocolError("invalid frame magic")
    if version != expected_version:
        raise ProtocolError(f"unsupported protocol version {version}")
    if reserved != 0:
        raise ProtocolError("reserved header field must be zero")
    if length > min(max_frame_bytes, MAX_PAYLOAD_BYTES):
        raise ProtocolError("frame payload exceeds configured maximum")
    if len(data) != HEADER.size + length:
        raise ProtocolError("frame length mismatch")
    payload = data[HEADER.size :]
    if zlib.crc32(payload) & 0xFFFFFFFF != checksum:
        raise ProtocolError("payload checksum mismatch")
    try:
        kind = MessageType(message_type)
    except ValueError as exc:
        raise ProtocolError(f"unknown message type {message_type}") from exc
    return Frame(kind, request_id, payload, flags, version)

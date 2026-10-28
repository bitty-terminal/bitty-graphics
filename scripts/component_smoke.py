#!/usr/bin/env python3
"""Spawn a packaged component binary and exercise protocol v1 end to end.

The smoke is deliberately small and standard-library only so the same check
runs on every native runner (Linux, macOS, Windows) after the tarball is
unpacked:

1. ``<binary> --version`` reports the descriptor version.
2. ``Hello`` is answered by ``HelloAck`` with protocol 1, the descriptor
   component/version, and a non-empty capability set.
3. One raw RGBA ``DecodeRequest`` (1x1) is answered by a ``DecodeResponse``
   with the exact normalized shape and pixels.
4. ``Shutdown`` is answered by ``ShutdownAck`` and the process exits 0.

The script never reads a path, environment variable, URL, or command from the
wire; it only drives the public v1 framing. On any failure it stops the child
it started and exits 1 with a bounded diagnostic.

Usage:
    scripts/component_smoke.py --binary PATH [--root DIR] [--timeout SECONDS]
    scripts/component-release.sh smoke --binary PATH
"""

from __future__ import annotations

import argparse
import queue
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path

import tomllib

DESCRIPTOR_FILE = "bitty-component.toml"

PROTOCOL_VERSION = 1
TAG_HELLO = 0x01
TAG_HELLO_ACK = 0x02
TAG_DECODE_REQUEST = 0x10
TAG_DECODE_RESPONSE = 0x11
TAG_ERROR = 0x2F
TAG_SHUTDOWN = 0x3F
TAG_SHUTDOWN_ACK = 0x3E

HELLO_CAPABILITY_GRANTED = 0x02
PIXEL_FORMAT_RGBA8 = 0x01
FORMAT_RGBA32 = 32

# Mirrors MAX_RESPONSE_FRAME_BYTES in src/component/protocol.rs (64 MiB
# bitmap plus the fixed response fields). A malformed child cannot make the
# smoke allocate more than one bounded frame.
MAX_RESPONSE_FRAME_BYTES = 64 * 1024 * 1024 + 1024
FRAME_QUEUE_DEPTH = 8

REQUEST_ID = 1
DECODE_TIMEOUT_MS = 5_000
RGBA_PIXEL = bytes([0x11, 0x22, 0x33, 0x44])


class SmokeFailure(Exception):
    """One operator-facing protocol smoke failure."""


def read_descriptor(root: Path) -> tuple[str, str, str]:
    path = root / DESCRIPTOR_FILE
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise SmokeFailure(f"cannot read descriptor {path}: {error}") from error
    except tomllib.TOMLDecodeError as error:
        raise SmokeFailure(f"descriptor {path} is not valid TOML: {error}") from error
    section = data.get("component")
    if not isinstance(section, dict):
        raise SmokeFailure(f"descriptor {path} is missing [component]")
    values = []
    for key in ("name", "version", "executable"):
        value = section.get(key)
        if not isinstance(value, str) or not value:
            raise SmokeFailure(f"descriptor {path} is missing a valid {key}")
        values.append(value)
    return values[0], values[1], values[2]


def encode_string(value: str) -> bytes:
    encoded = value.encode("utf-8")
    return struct.pack(">I", len(encoded)) + encoded


def encode_hello(component: str) -> bytes:
    payload = (
        bytes([TAG_HELLO])
        + struct.pack(">HH", PROTOCOL_VERSION, PROTOCOL_VERSION)
        + encode_string(component)
        + encode_string("component-smoke")
        + bytes([HELLO_CAPABILITY_GRANTED])
    )
    return struct.pack(">I", len(payload)) + payload


def encode_decode_request() -> bytes:
    payload = (
        bytes([TAG_DECODE_REQUEST])
        + struct.pack(">Q", REQUEST_ID)
        + bytes([FORMAT_RGBA32])
        + bytes([1])
        + struct.pack(">I", 1)
        + bytes([1])
        + struct.pack(">I", 1)
        + struct.pack(">II", len(RGBA_PIXEL), DECODE_TIMEOUT_MS)
        + struct.pack(">I", len(RGBA_PIXEL))
        + RGBA_PIXEL
    )
    return struct.pack(">I", len(payload)) + payload


def encode_shutdown() -> bytes:
    payload = bytes([TAG_SHUTDOWN])
    return struct.pack(">I", len(payload)) + payload


def take(payload: bytes, offset: int, length: int) -> tuple[bytes, int]:
    end = offset + length
    if end > len(payload):
        raise SmokeFailure("truncated component frame")
    return payload[offset:end], end


def take_u16(payload: bytes, offset: int) -> tuple[int, int]:
    raw, offset = take(payload, offset, 2)
    return struct.unpack(">H", raw)[0], offset


def take_u32(payload: bytes, offset: int) -> tuple[int, int]:
    raw, offset = take(payload, offset, 4)
    return struct.unpack(">I", raw)[0], offset


def take_u64(payload: bytes, offset: int) -> tuple[int, int]:
    raw, offset = take(payload, offset, 8)
    return struct.unpack(">Q", raw)[0], offset


def take_string(payload: bytes, offset: int) -> tuple[str, int]:
    length, offset = take_u32(payload, offset)
    raw, offset = take(payload, offset, length)
    try:
        return raw.decode("utf-8"), offset
    except UnicodeDecodeError as error:
        raise SmokeFailure("component frame carries a non-UTF-8 string") from error


def parse_hello_ack(payload: bytes, expected_name: str, expected_version: str) -> None:
    if not payload or payload[0] != TAG_HELLO_ACK:
        raise SmokeFailure(f"expected HelloAck, got {describe(payload)}")
    protocol, offset = take_u16(payload, 1)
    component, offset = take_string(payload, offset)
    version, offset = take_string(payload, offset)
    capabilities, offset = take(payload, offset, 1)
    # Fixed capability block: u32 x4, u64, u32 x4.
    for _ in range(4):
        _, offset = take_u32(payload, offset)
    _, offset = take_u64(payload, offset)
    for _ in range(4):
        _, offset = take_u32(payload, offset)
    if offset != len(payload):
        raise SmokeFailure("HelloAck has trailing bytes")
    if protocol != PROTOCOL_VERSION:
        raise SmokeFailure(f"HelloAck protocol {protocol} != {PROTOCOL_VERSION}")
    if component != expected_name:
        raise SmokeFailure(
            f"HelloAck component {component!r} != descriptor {expected_name!r}"
        )
    if version != expected_version:
        raise SmokeFailure(
            f"HelloAck version {version!r} != descriptor {expected_version!r}"
        )
    if capabilities[0] == 0:
        raise SmokeFailure("HelloAck advertises no decode capabilities")


def parse_decode_response(payload: bytes) -> None:
    if not payload or payload[0] != TAG_DECODE_RESPONSE:
        raise SmokeFailure(f"expected DecodeResponse, got {describe(payload)}")
    request, offset = take_u64(payload, 1)
    format_id, offset = take(payload, offset, 1)
    width, offset = take_u32(payload, offset)
    height, offset = take_u32(payload, offset)
    stride, offset = take_u32(payload, offset)
    length, offset = take_u32(payload, offset)
    rgba, offset = take(payload, offset, length)
    if offset != len(payload):
        raise SmokeFailure("DecodeResponse has trailing bytes")
    if request != REQUEST_ID:
        raise SmokeFailure(f"DecodeResponse request {request} != {REQUEST_ID}")
    if format_id[0] != PIXEL_FORMAT_RGBA8:
        raise SmokeFailure(
            f"DecodeResponse format {format_id[0]} != {PIXEL_FORMAT_RGBA8}"
        )
    if (width, height, stride) != (1, 1, 4):
        raise SmokeFailure(
            f"DecodeResponse shape {(width, height, stride)} != (1, 1, 4)"
        )
    if rgba != RGBA_PIXEL:
        raise SmokeFailure(f"DecodeResponse pixels {rgba.hex()} != {RGBA_PIXEL.hex()}")


def read_exact(stream, length: int) -> bytes:
    """Read up to ``length`` bytes; a short result means EOF or truncation."""
    data = bytearray()
    while len(data) < length:
        chunk = stream.read(length - len(data))
        if not chunk:
            break
        data.extend(chunk)
    return bytes(data)


def describe(payload: bytes) -> str:
    if not payload:
        return "an empty frame"
    if payload[0] == TAG_ERROR:
        try:
            request, offset = take_u64(payload, 1)
            kind, offset = take(payload, offset, 1)
            message, offset = take_string(payload, offset)
        except SmokeFailure:
            return "a malformed Error frame"
        return f"Error(request={request}, kind={kind[0]}, message={message!r})"
    return f"tag 0x{payload[0]:02x}"


class Component:
    """One spawned component process plus its decoded frame stream."""

    def __init__(self, binary: Path, timeout: float) -> None:
        self.binary = binary
        self.timeout = timeout
        self.child = subprocess.Popen(
            [str(binary)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        self.frames: queue.Queue[tuple[str, object]] = queue.Queue(
            maxsize=FRAME_QUEUE_DEPTH
        )
        self.reader = threading.Thread(
            target=self._read_frames, name="component-smoke-reader", daemon=True
        )
        self.reader.start()

    def _read_frames(self) -> None:
        assert self.child.stdout is not None
        stream = self.child.stdout
        try:
            while True:
                header = read_exact(stream, 4)
                if not header:
                    self.frames.put(("eof", None))
                    return
                if len(header) != 4:
                    self.frames.put(("error", "truncated frame header"))
                    return
                length = struct.unpack(">I", header)[0]
                if length == 0 or length > MAX_RESPONSE_FRAME_BYTES:
                    self.frames.put(
                        (
                            "error",
                            (
                                f"component frame length {length} outside "
                                f"1..{MAX_RESPONSE_FRAME_BYTES}"
                            ),
                        )
                    )
                    return
                payload = read_exact(stream, length)
                if len(payload) != length:
                    self.frames.put(("error", "truncated frame payload"))
                    return
                self.frames.put(("frame", payload))
        except OSError as error:
            self.frames.put(("error", f"stream failure: {error}"))

    def send(self, frame: bytes) -> None:
        assert self.child.stdin is not None
        try:
            self.child.stdin.write(frame)
            self.child.stdin.flush()
        except OSError as error:
            raise SmokeFailure(f"cannot write to component: {error}") from error

    def recv(self) -> bytes:
        try:
            kind, payload = self.frames.get(timeout=self.timeout)
        except queue.Empty as error:
            raise SmokeFailure(
                f"component produced no frame within {self.timeout:g}s"
            ) from error
        if kind == "eof":
            raise SmokeFailure("component closed stdout before replying")
        if kind == "error":
            raise SmokeFailure(str(payload))
        assert isinstance(payload, bytes)
        return payload

    def stop(self) -> None:
        if self.child.poll() is not None:
            return
        self.child.terminate()
        try:
            self.child.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.child.kill()
            self.child.wait(timeout=5)


def check_version(binary: Path, expected: str, timeout: float) -> None:
    try:
        result = subprocess.run(
            [str(binary), "--version"],
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise SmokeFailure(f"cannot run {binary} --version: {error}") from error
    if result.returncode != 0:
        raise SmokeFailure(
            f"{binary} --version exited {result.returncode}: {result.stderr.strip()}"
        )
    if result.stdout.strip() != expected:
        raise SmokeFailure(
            f"{binary} --version reported {result.stdout.strip()!r}, expected {expected!r}"
        )


def run(binary: Path, root: Path, timeout: float) -> None:
    name, version, executable = read_descriptor(root)
    if not binary.is_file():
        raise SmokeFailure(f"component binary is missing: {binary}")
    check_version(binary, f"{executable} {version}", timeout)

    component = Component(binary, timeout)
    try:
        component.send(encode_hello(name))
        parse_hello_ack(component.recv(), name, version)
        component.send(encode_decode_request())
        parse_decode_response(component.recv())
        component.send(encode_shutdown())
        ack = component.recv()
        if ack != bytes([TAG_SHUTDOWN_ACK]):
            raise SmokeFailure(f"expected ShutdownAck, got {describe(ack)}")
        try:
            code = component.child.wait(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            raise SmokeFailure("component did not exit after ShutdownAck") from error
        if code != 0:
            raise SmokeFailure(f"component exited {code} after ShutdownAck")
    finally:
        component.stop()


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="component-release.sh smoke",
        description=(
            "Spawn a packaged component binary and verify Hello/DecodeRequest/"
            "Shutdown against protocol v1."
        ),
    )
    parser.add_argument("--binary", required=True, help="component executable to run")
    parser.add_argument(
        "--root",
        default=str(Path(__file__).resolve().parent.parent),
        help="component repository root holding bitty-component.toml",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=20.0,
        help="seconds allowed for one frame or process transition (default: 20)",
    )
    args = parser.parse_args(argv)

    binary = Path(args.binary).resolve()
    root = Path(args.root).resolve()
    started = time.monotonic()
    try:
        if args.timeout <= 0:
            raise SmokeFailure("--timeout must be positive")
        run(binary, root, args.timeout)
    except SmokeFailure as error:
        print(f"component-smoke: ERROR: {error}", file=sys.stderr)
        return 1
    elapsed = time.monotonic() - started
    print(
        f"component-smoke: PASS (binary={binary}, root={root}, elapsed={elapsed:.2f}s)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

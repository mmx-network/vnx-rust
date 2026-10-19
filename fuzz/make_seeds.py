#!/usr/bin/env python3
"""Rebuild the deterministic fuzz seeds (only public, synthetic fixture data)."""
from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parent


def words(*values):
    return struct.pack("<" + "H" * len(values), *values)


def seed(target, name, data):
    directory = ROOT / "seeds" / target
    directory.mkdir(parents=True, exist_ok=True)
    (directory / name).write_bytes(data)


def stream(name, data, flags=0, profile=19):
    seed("decode", name, bytes([profile, flags]) + data)


def payload(name, code, data=b"", selector=0, flags=0, profile=19):
    header = bytes([profile, flags, selector]) + words(len(code))
    seed("bytecode", name, header + words(*code) + data)


for path in sorted((ROOT.parent / "crates/vnx/tests/fixtures").glob("*.hex")):
    data = bytes.fromhex(path.read_text())
    stream("cpp-" + path.stem, data)
    stream("schema-" + path.stem, data, flags=1)

stream("null", words(0))
stream("empty-object", words(24) + struct.pack("<I", 0))
stream("many-values", words(0) * 32)
stream("flat-any-prefixes", words(16) * 200 + words(0))
stream("schema-depth", words(14) * 200, flags=1)
stream("value-schema-depth", words(14) * 200)
stream("dynamic-depth", words(17) + words(1, 17) * 200 + words(1, 0))
stream("dynamic-payload-depth", words(1, 17) * 200 + words(1, 0), flags=2)
stream("padding", words(18) + struct.pack("<I", 256) + bytes(256) + words(0))
stream("truncated-string", words(17, 1, 32) + struct.pack("<I", 0xFFFFFFFF))
stream("invalid-utf8", words(17, 1, 32) + struct.pack("<I", 2) + b"\xff\xfe")
stream("lossy-utf8", words(17, 1, 32) + struct.pack("<I", 2) + b"\xff\xfe", flags=8)
stream("big-endian", struct.pack(">HHHQ", 17, 1, 4, 0x0123456789ABCDEF))
for count in [64, 65535]:
    code = [11, count, 11, count, 11, count, 0]
    stream(f"compact-expansion-{count}", words(17, len(code), *code))
    payload(f"compact-expansion-{count}", code)

object_body = struct.pack("<II", 1, 1) + b"x"
stream("object-depth", words(24) + (object_body + words(1, 24)) * 200 + bytes(4))
stream("object-payload-depth", (object_body + words(1, 24)) * 200 + bytes(4), flags=3)
for profile in [0, 4, 8, 12, 16, 64, 128]:
    stream(f"depth-profile-{profile}", words(17, 1, 0), profile=profile)
    payload(f"allocation-profile-{profile}", [11, 64, 11, 64, 0], profile=profile)

for code in [[0], [1], [4], [32], [33, 0], [12, 0], [24], [14],
             [13, 3, 0, 0], [23, 2, 4, 5, 0, 0], [22, 2, 0],
             [21, 2, 65535, 65535, 0], [0x0B00, 0x0200, 2]]:
    payload("code-" + "-".join(str(word) for word in code), code, bytes(16))
for depth in [31, 32, 99, 100, 200]:
    payload(f"optional-depth-{depth}", [33] * depth + [0], b"\x01" * depth)
for depth in [0, 30, 60, 99, 119]:
    payload(f"schema-dag-{depth}", [19, 0], selector=depth)
for selector in [64, 94, 128, 188, 255]:
    payload(f"schema-graph-{selector}", [19, 0], selector=selector)
    payload(f"typed-schema-graph-{selector}", [0], selector=selector, flags=16)

print("Wrote", sum(1 for path in (ROOT / "seeds").glob("*/*") if path.is_file()), "seeds")

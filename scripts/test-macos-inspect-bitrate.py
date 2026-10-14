#!/usr/bin/env python3
"""Bounded synthetic Mach-O regressions for the bitrate inspector."""

import importlib.util
from pathlib import Path
import struct
import tempfile

ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "macos_inspect_bitrate", ROOT / "macos-inspect-bitrate.py")
inspector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(inspector)

HEADER_SIZE = 32
SEGMENT_SIZE = 72 + 2 * 80
COMMANDS_SIZE = SEGMENT_SIZE
DATA_OFFSET = HEADER_SIZE + COMMANDS_SIZE
CODE_OFFSET = DATA_OFFSET + inspector.TABLE_SIZE
VM_BASE = 0x100000000


def table_bytes(*, strings_only=False, mismatch=False, truncated=False):
    output = bytearray(inspector.TABLE_SIZE - (1 if truncated else 0))
    for index, (label, rate) in enumerate(zip(inspector.RECORD_LABELS,
            inspector.RECORD_RATES)):
        offset = index * inspector.RECORD_STRIDE
        output[offset:offset + len(label)] = label
        if not strings_only:
            value = rate + (1 if mismatch and index == 5 else 0)
            struct.pack_into("<i", output, offset + inspector.VALUE_OFFSET, value)
    return bytes(output)


def adrp_add_pair(target, pc):
    page_delta = ((target & ~0xFFF) - (pc & ~0xFFF)) >> 12
    assert -(1 << 20) <= page_delta < (1 << 20)
    encoded = page_delta & ((1 << 21) - 1)
    immlo = encoded & 0x3
    immhi = encoded >> 2
    adrp = 0x90000000 | (immlo << 29) | (immhi << 5)
    add = 0x91000000 | ((target & 0xFFF) << 10)
    return struct.pack("<II", adrp, add)


def macho(cpu=inspector.CPU_TYPE_ARM64, *, table=None, code=b""):
    if table is None:
        table = table_bytes()
    command_size = HEADER_SIZE + COMMANDS_SIZE
    data_offset = command_size
    code_offset = data_offset + len(table)
    code_vm = VM_BASE + 0x10000
    code_vm = (code_vm + 0xFFF) & ~0xFFF
    if code:
        # The fixture's code section begins at a page-aligned VM address.
        code_vm = VM_BASE + 0x10000
    data_vm = VM_BASE
    image_size = code_offset + len(code)
    command = bytearray(COMMANDS_SIZE)
    struct.pack_into("<II16sQQQQiiII", command, 0, 0x19, COMMANDS_SIZE,
        b"__DATA", VM_BASE, 0x20000, 0, image_size, 7, 7, 2, 0)
    struct.pack_into("<16s16sQQIIIIIIII", command, 72,
        b"__const", b"__DATA", data_vm, len(table), data_offset, 2,
        0, 0, 0, 0, 0, 0)
    struct.pack_into("<16s16sQQIIIIIIII", command, 152,
        b"__text", b"__DATA", code_vm, len(code), code_offset, 2,
        0, 0, 0x80000400, 0, 0, 0)
    header = struct.pack("<IiiIIIII", inspector.MH_MAGIC_64, cpu, 0,
        6, 1, COMMANDS_SIZE, 0, 0)
    return header + command + table + code


def universal(x86_image, arm_image):
    x86_offset = 0x1000
    arm_offset = (x86_offset + len(x86_image) + 0xFFF) & ~0xFFF
    header = struct.pack(">II", inspector.FAT_MAGIC, 2)
    entries = struct.pack(">IIIII", inspector.CPU_TYPE_X86_64, 3,
        x86_offset, len(x86_image), 12)
    entries += struct.pack(">IIIII", inspector.CPU_TYPE_ARM64, 0,
        arm_offset, len(arm_image), 12)
    return header + entries + bytes(x86_offset - len(header) - len(entries)) + x86_image + bytes(
        arm_offset - x86_offset - len(x86_image)) + arm_image, arm_offset


def inspect_bytes(payload, find_xrefs=False):
    with tempfile.TemporaryDirectory(prefix="bitrate-inspector-test-") as temp:
        path = Path(temp) / "fixture.dylib"
        path.write_bytes(payload)
        return inspector.inspect(path, find_xrefs=find_xrefs)


def test_valid_candidate_and_static_xref():
    table = table_bytes()
    data_vm = VM_BASE
    code_vm = VM_BASE + 0x10000
    image = macho(table=table, code=adrp_add_pair(data_vm, code_vm))
    result = inspect_bytes(image, find_xrefs=True)
    assert result["architecture"] == "arm64"
    assert result["container"] == "thin"
    assert result["verified"] is False
    assert len(result["candidates"]) == 1
    candidate = result["candidates"][0]
    assert candidate["offset"] == DATA_OFFSET
    assert candidate["vmaddr"] == data_vm
    assert candidate["segment"] == "__DATA"
    assert candidate["section"] == "__const"
    assert [record["value_i32"] for record in candidate["records"]] == list(
        inspector.RECORD_RATES)
    assert len(candidate["static_candidate_xrefs"]) == 1
    assert candidate["static_candidate_xrefs"][0]["kind"] == "STATIC candidate xref"


def test_string_only_sequence_is_rejected():
    assert inspect_bytes(macho(table=table_bytes(strings_only=True)))["candidates"] == []


def test_mismatched_numeric_value_is_rejected():
    assert inspect_bytes(macho(table=table_bytes(mismatch=True)))["candidates"] == []


def test_truncated_table_extent_is_rejected():
    assert inspect_bytes(macho(table=table_bytes(truncated=True)))["candidates"] == []


def test_invalid_container_is_rejected():
    try:
        inspect_bytes(b"not a Mach-O" + bytes(64))
    except inspector.InspectorError:
        return
    raise AssertionError("invalid container was accepted")


def test_universal_selects_arm64_even_when_x86_has_matching_table():
    x86 = macho(inspector.CPU_TYPE_X86_64, table=table_bytes())
    arm = macho(table=table_bytes())
    image, arm_offset = universal(x86, arm)
    result = inspect_bytes(image)
    assert result["container"] == "universal"
    assert result["architecture"] == "arm64"
    assert result["slice_offset"] == arm_offset
    assert len(result["candidates"]) == 1
    assert result["candidates"][0]["offset"] == arm_offset + DATA_OFFSET


if __name__ == "__main__":
    tests = [value for name, value in globals().items()
        if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"PASS {test.__name__}")
    print(f"{len(tests)} tests passed")

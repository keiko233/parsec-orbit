#!/usr/bin/env python3
"""Read-only, bounded static inspection of Parsec bitrate option records.

This reports byte-pattern candidates and optional adjacent arm64 ADRP+ADD
references. It does not establish runtime use, patch files, or inspect processes.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import struct
import sys

CPU_TYPE_ARM64 = 0x0100000C
CPU_TYPE_X86_64 = 0x01000007
MH_MAGIC_64 = 0xFEEDFACF
FAT_MAGIC = 0xCAFEBABE
FAT_MAGIC_64 = 0xCAFEBABF
LC_SEGMENT_64 = 0x19
S_ZEROFILL = 0x1
S_GB_ZEROFILL = 0xC
S_THREAD_LOCAL_ZEROFILL = 0x12
VM_PROT_EXECUTE = 0x4
RECORD_LABELS = tuple(f"{rate} Mbps".encode() + b"\0"
    for rate in (3, 5, 7, 10, 15, 20, 25, 30, 35, 40, 45, 50))
RECORD_RATES = (3, 5, 7, 10, 15, 20, 25, 30, 35, 40, 45, 50)
RECORD_STRIDE = 0x144
VALUE_OFFSET = 0x40
TABLE_SIZE = len(RECORD_LABELS) * RECORD_STRIDE

# Explicit caps keep malformed container metadata from driving unbounded work.
MAX_FILE_SIZE = 2 * 1024 * 1024 * 1024
MAX_FAT_SLICES = 32
MAX_LOAD_COMMANDS = 4096
MAX_SECTIONS = 100_000
MAX_SECTION_SIZE = 256 * 1024 * 1024
MAX_TOTAL_SCAN_SIZE = 512 * 1024 * 1024
MAX_CANDIDATES = 64


class InspectorError(Exception):
    """An unsupported or malformed input image."""


def _unpack(fmt, data, offset=0):
    try:
        return struct.unpack_from(fmt, data, offset)
    except struct.error as error:
        raise InspectorError("truncated Mach-O container") from error


def _read_exact(stream, offset, size, limit):
    if offset < 0 or size < 0 or offset > limit or size > limit - offset:
        raise InspectorError("Mach-O range exceeds its bounded container")
    stream.seek(offset)
    data = stream.read(size)
    if len(data) != size:
        raise InspectorError("truncated Mach-O data")
    return data


def _cstring(raw):
    return raw.split(b"\0", 1)[0].decode("ascii", errors="replace")


def _select_slice(stream, file_size):
    head = _read_exact(stream, 0, min(file_size, 8), file_size)
    if len(head) < 4:
        raise InspectorError("file is too short to contain a Mach-O image")
    magic_be = int.from_bytes(head[:4], "big")
    magic_le = int.from_bytes(head[:4], "little")
    if magic_le == MH_MAGIC_64:
        if len(head) < 8:
            raise InspectorError("truncated thin Mach-O header")
        cpu = int.from_bytes(head[4:8], "little")
        if cpu != CPU_TYPE_ARM64:
            raise InspectorError("thin Mach-O image is not arm64")
        return {"container": "thin", "architecture": "arm64",
            "slice_offset": 0, "slice_size": file_size}

    fat = {
        FAT_MAGIC: (">", False),
        FAT_MAGIC_64: (">", True),
        int.from_bytes(struct.pack(">I", FAT_MAGIC), "little"): ("<", False),
        int.from_bytes(struct.pack(">I", FAT_MAGIC_64), "little"): ("<", True),
    }.get(magic_be)
    if fat is None:
        raise InspectorError("input is not a supported 64-bit Mach-O container")
    endian, is_64 = fat
    if len(head) < 8:
        raise InspectorError("truncated universal Mach-O header")
    count = _unpack(endian + "I", head, 4)[0]
    entry_size = 32 if is_64 else 20
    if count < 1 or count > MAX_FAT_SLICES:
        raise InspectorError("universal Mach-O slice count exceeds bounds")
    table_size = count * entry_size
    if 8 + table_size > file_size:
        raise InspectorError("truncated universal Mach-O architecture table")
    table = _read_exact(stream, 8, table_size, file_size)
    slices = []
    selected = []
    for index in range(count):
        pos = index * entry_size
        if is_64:
            cpu, subtype, offset, size, align, _reserved = _unpack(
                endian + "IIQQII", table, pos)
        else:
            cpu, subtype, offset, size, align = _unpack(
                endian + "IIIII", table, pos)
        if align > 30 or offset % (1 << align):
            raise InspectorError("universal Mach-O slice has invalid alignment")
        if offset < 8 + table_size or size < 32 or offset > file_size or size > file_size - offset:
            raise InspectorError("universal Mach-O slice exceeds file bounds")
        if any(offset < other_offset + other_size and other_offset < offset + size
                for other_offset, other_size in slices):
            raise InspectorError("universal Mach-O slices overlap")
        slices.append((offset, size))
        if cpu == CPU_TYPE_ARM64:
            selected.append((offset, size, subtype))
    if len(selected) != 1:
        raise InspectorError("universal image must contain exactly one arm64 slice")
    offset, size, subtype = selected[0]
    header = _read_exact(stream, offset, 32, file_size)
    magic, cpu, image_subtype = _unpack("<III", header)
    if (magic != MH_MAGIC_64 or cpu != CPU_TYPE_ARM64
            or (image_subtype & 0x00FFFFFF) != (subtype & 0x00FFFFFF)):
        raise InspectorError("arm64 universal slice header does not match its table")
    return {"container": "universal", "architecture": "arm64",
        "slice_offset": offset, "slice_size": size}


def _parse_sections(stream, selected):
    base = selected["slice_offset"]
    size = selected["slice_size"]
    header = _read_exact(stream, base, 32, base + size)
    magic, cpu, _subtype, _filetype, ncmds, sizeofcmds, _flags, _reserved = _unpack(
        "<IiiIIIII", header)
    if magic != MH_MAGIC_64 or cpu != CPU_TYPE_ARM64:
        raise InspectorError("selected slice is not a 64-bit arm64 Mach-O image")
    if ncmds > MAX_LOAD_COMMANDS or sizeofcmds > size - 32:
        raise InspectorError("Mach-O load-command area exceeds bounds")
    commands = _read_exact(stream, base + 32, sizeofcmds, base + size)
    cursor = 0
    sections = []
    for _ in range(ncmds):
        if cursor + 8 > len(commands):
            raise InspectorError("truncated Mach-O load command")
        cmd, cmdsize = _unpack("<II", commands, cursor)
        if cmdsize < 8 or cmdsize % 8 or cmdsize > len(commands) - cursor:
            raise InspectorError("invalid Mach-O load-command size")
        if cmd == LC_SEGMENT_64:
            if cmdsize < 72:
                raise InspectorError("truncated LC_SEGMENT_64 command")
            segname_raw, vmaddr, vmsize, fileoff, filesize, _maxprot, initprot, nsects, _segflags = _unpack(
                "<16sQQQQiiII", commands, cursor + 8)
            segment = _cstring(segname_raw)
            if nsects > MAX_SECTIONS or 72 + nsects * 80 > cmdsize:
                raise InspectorError("Mach-O section table exceeds bounds")
            if fileoff > size or filesize > size - fileoff:
                raise InspectorError("Mach-O segment file range exceeds slice")
            for section_index in range(nsects):
                secpos = cursor + 72 + section_index * 80
                sectname_raw, secseg_raw, addr, secsize, offset, _align, _reloff, _nreloc, flags, *_ = _unpack(
                    "<16s16sQQIIIIIIII", commands, secpos)
                secname = _cstring(sectname_raw)
                secseg = _cstring(secseg_raw)
                section_type = flags & 0xFF
                zero_fill = section_type in (S_ZEROFILL, S_GB_ZEROFILL,
                    S_THREAD_LOCAL_ZEROFILL)
                if zero_fill:
                    continue
                if (secsize > MAX_SECTION_SIZE or offset > size
                        or secsize > size - offset or offset < fileoff
                        or offset + secsize > fileoff + filesize
                        or addr < vmaddr or secsize > vmaddr + vmsize - addr):
                    raise InspectorError("file-backed Mach-O section exceeds segment bounds")
                if secsize:
                    sections.append({"segment": segment, "section": secname,
                        "section_segment": secseg, "offset": offset,
                        "size": secsize, "address": addr,
                        "executable": bool(initprot & VM_PROT_EXECUTE)
                            and bool(flags & 0x80000400)})
                if len(sections) > MAX_SECTIONS:
                    raise InspectorError("Mach-O file-backed section count exceeds bounds")
        cursor += cmdsize
    if cursor != sizeofcmds:
        raise InspectorError("Mach-O load-command sizes do not match header")
    return sections


def _scan_section(stream, section, selected, scan_budget):
    if section["size"] > scan_budget:
        raise InspectorError("total file-backed section scan exceeds bounds")
    raw = _read_exact(stream, selected["slice_offset"] + section["offset"],
        section["size"], selected["slice_offset"] + selected["slice_size"])
    return raw


def _adrp_target(instruction, pc):
    if instruction & 0x9F000000 != 0x90000000:
        return None
    immediate = ((instruction >> 29) & 0x3) | (((instruction >> 5) & 0x7FFFF) << 2)
    if immediate & (1 << 20):
        immediate -= 1 << 21
    return (pc & ~0xFFF) + (immediate << 12), instruction & 0x1F


def _candidate_xrefs(stream, sections, selected, candidate_address, remaining):
    xrefs = []
    for section in sections:
        if not section["executable"]:
            continue
        if section["size"] > remaining:
            raise InspectorError("total file-backed section scan exceeds bounds")
        raw = _scan_section(stream, section, selected, remaining)
        remaining -= section["size"]
        for pos in range(0, len(raw) - 7, 4):
            adrp = int.from_bytes(raw[pos:pos + 4], "little")
            add = int.from_bytes(raw[pos + 4:pos + 8], "little")
            decoded = _adrp_target(adrp, section["address"] + pos)
            if decoded is None:
                continue
            page, adrp_rd = decoded
            # ADD Xd (immediate); ADD Wd would truncate an ASLR pointer.
            if add & 0xFF800000 != 0x91000000 or adrp_rd == 31:
                continue
            add_rn = (add >> 5) & 0x1F
            if add_rn != adrp_rd:
                continue
            immediate = (add >> 10) & 0xFFF
            if (add >> 22) & 1:
                immediate <<= 12
            if page + immediate == candidate_address:
                xrefs.append({"kind": "STATIC candidate xref",
                    "instruction_offset": selected["slice_offset"] + section["offset"] + pos,
                    "instruction_vmaddr": section["address"] + pos,
                    "segment": section["segment"], "section": section["section"],
                    "instructions": [f"0x{adrp:08x}", f"0x{add:08x}"]})
                if len(xrefs) > 1024:
                    raise InspectorError("candidate xref count exceeds bounds")
    return xrefs


def inspect(path, find_xrefs=False):
    path = Path(path)
    try:
        with path.open("rb") as stream:
            initial = os.fstat(stream.fileno())
            file_size = initial.st_size
            if file_size < 32 or file_size > MAX_FILE_SIZE:
                raise InspectorError("file size is outside supported bounds")
            digest = hashlib.sha256()
            stream.seek(0)
            hashed = 0
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                hashed += len(block)
                if hashed > file_size:
                    raise InspectorError("input grew during hashing")
                digest.update(block)
            selected = _select_slice(stream, file_size)
            sections = _parse_sections(stream, selected)
            candidates = []
            remaining = MAX_TOTAL_SCAN_SIZE
            for section in sections:
                if section["size"] > remaining:
                    raise InspectorError("total file-backed section scan exceeds bounds")
                raw = _scan_section(stream, section, selected, remaining)
                remaining -= section["size"]
                search_from = 0
                while True:
                    label_pos = raw.find(RECORD_LABELS[0], search_from)
                    if label_pos < 0:
                        break
                    search_from = label_pos + 1
                    if label_pos + TABLE_SIZE > len(raw):
                        continue
                    records = []
                    valid = True
                    for index, (label, rate) in enumerate(zip(RECORD_LABELS, RECORD_RATES)):
                        pos = label_pos + index * RECORD_STRIDE
                        if raw[pos:pos + len(label)] != label:
                            valid = False
                            break
                        value = int.from_bytes(raw[pos + VALUE_OFFSET:pos + VALUE_OFFSET + 4],
                            "little", signed=True)
                        if value != rate:
                            valid = False
                            break
                        records.append({"label": label[:-1].decode("ascii"),
                            "offset": selected["slice_offset"] + section["offset"] + pos,
                            "vmaddr": section["address"] + pos,
                            "value_i32": value})
                    if not valid:
                        continue
                    start_offset = selected["slice_offset"] + section["offset"] + label_pos
                    start_address = section["address"] + label_pos
                    candidates.append({"offset": start_offset, "vmaddr": start_address,
                        "segment": section["segment"], "section": section["section"],
                        "record_stride": RECORD_STRIDE, "record_count": len(records),
                        "records": records, "static_candidate_xrefs": []})
                    if len(candidates) > MAX_CANDIDATES:
                        raise InspectorError("candidate table count exceeds bounds")
            if find_xrefs:
                for candidate in candidates:
                    candidate["static_candidate_xrefs"] = _candidate_xrefs(
                        stream, sections, selected, candidate["vmaddr"], remaining)
                    # _candidate_xrefs scans executable sections; account for the
                    # bounded second pass conservatively per candidate.
                    remaining -= sum(s["size"] for s in sections if s["executable"])
                    if remaining < 0:
                        raise InspectorError("total file-backed section scan exceeds bounds")
            final = os.fstat(stream.fileno())
            if (hashed != file_size or (initial.st_size, initial.st_mtime_ns, initial.st_ctime_ns)
                    != (final.st_size, final.st_mtime_ns, final.st_ctime_ns)):
                raise InspectorError("input changed during inspection")
            return {"schema_version": 1, "result_kind": "static candidates",
                "verified": False, "read_only": True, "sha256": digest.hexdigest(),
                "path": str(path), "container": selected["container"],
                "architecture": selected["architecture"],
                "slice_offset": selected["slice_offset"],
                "slice_size": selected["slice_size"], "candidates": candidates}
    except OSError as error:
        raise InspectorError(f"cannot read input file: {error}") from error


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("macho", help="selected thin or universal Mach-O file")
    parser.add_argument("--xrefs", action="store_true",
        help="also scan bounded executable sections for adjacent ADRP+ADD pairs")
    args = parser.parse_args(argv)
    try:
        result = inspect(args.macho, find_xrefs=args.xrefs)
    except InspectorError as error:
        print(json.dumps({"error": str(error), "verified": False}), file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

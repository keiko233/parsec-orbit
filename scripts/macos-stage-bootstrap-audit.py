#!/usr/bin/env python3
"""Prepare a disposable M1a dependency audit copy. Never install/launch.

This is a development fixture, not the M1b installer. It copies vendor inputs
locally, retains quarantine, changes the fixture id, adds the dependency only
to a verified arm64 stub, and re-signs owned code and the outer copy.

Two modes exist:

- the default fail-closed audit, whose plan asserts the loader MUST refuse the
  selected host before vendor main (`expected_exit` 78);
- `--acceptance`, which stages an owned fixture plugin and asserts the
  opposite contract: an ordinary-user launch of this plan is expected to pass
  the guard and reach vendor main (`client_execution_allowed` true).
  `--preseed-skel` additionally copies the verified skel into the isolated
  state first; the PoC measured that an empty-state first launch exits 2
  before client execution, while a preseeded first launch reaches it.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import struct
import subprocess
import tempfile

STUB_SHA256 = "785f985d7f0cf6cffda8966b776a61b6c6cb0c79e582a8e3f35cda36305be76d"
DEPENDENCY = b"@executable_path/../Frameworks/libparsec_orbit_loader.dylib\0"
PLUGIN_ID = "com.example.mac-e2e"
PLUGIN_FIXTURE_CFG = "register=com.example.mac-e2e\nquery=parsec-orbit.client\n"


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def patch(path):
    image = bytearray(path.read_bytes())
    magic, cpu, _, kind, ncmds, size = struct.unpack_from("<6I", image)
    if magic != 0xFEEDFACF or cpu != 0x100000C or kind != 2:
        raise ValueError("fixture needs an arm64 Mach-O executable")
    end = 32 + size
    cursor = 32
    first_section = len(image)
    sections_report = []
    signature = None
    for _ in range(ncmds):
        command, count = struct.unpack_from("<2I", image, cursor)
        if count < 8 or count % 8 or cursor + count > end:
            raise ValueError("invalid load command extent")
        if command in (0xC, 0x80000018) and DEPENDENCY[:-1] in image[cursor:cursor + count]:
            raise ValueError("refusing a previously patched fixture")
        if command == 0x19:
            sections = struct.unpack_from("<I", image, cursor + 64)[0]
            if count != 72 + sections * 80:
                raise ValueError("invalid segment command")
            for index in range(sections):
                section = cursor + 72 + index * 80
                section_size = struct.unpack_from("<Q", image, section + 40)[0]
                offset = struct.unpack_from("<I", image, section + 48)[0]
                if offset and section_size:
                    if offset + section_size > len(image):
                        raise ValueError("invalid file-backed section extent")
                    first_section = min(first_section, offset)
                    sections_report.append({"offset": offset, "size": section_size,
                        "sha256": hashlib.sha256(image[offset:offset + section_size]).hexdigest()})
        if command == 0x1D:
            if signature is not None or cursor + count != end:
                raise ValueError("signature command must be unique and last")
            signature = cursor
        cursor += count
    count = (24 + len(DEPENDENCY) + 7) & ~7
    if cursor != end or signature is None or end + count > first_section:
        raise ValueError("unsupported fixture header layout")
    if any(image[end:first_section]):
        raise ValueError("nonzero header padding")
    addition = struct.pack("<6I", 0xC, count, 24, 0, 0, 0) + DEPENDENCY
    addition += bytes(count - len(addition))
    tail = bytes(image[signature:end])
    image[signature:signature + count] = addition
    image[signature + count:end + count] = tail
    struct.pack_into("<2I", image, 16, ncmds + 1, size + count)
    path.write_bytes(image)
    return {"commands_before": ncmds, "dependency_bytes": count,
            "first_section_offset": first_section, "file_backed_sections": sections_report}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", required=True, type=Path)
    parser.add_argument("--loader", required=True, type=Path)
    parser.add_argument("--scratch-parent", type=Path, default=Path(tempfile.gettempdir()))
    parser.add_argument("--acceptance", action="store_true",
        help="stage the owned fixture plugin and the guarded acceptance contract")
    parser.add_argument("--preseed-skel", action="store_true",
        help="with --acceptance, seed the isolated state from the verified skel")
    parser.add_argument("--plugin", type=Path,
        help="fixture plugin dylib to stage; required with --acceptance")
    args = parser.parse_args()
    if args.acceptance and args.plugin is None:
        raise ValueError("--acceptance requires --plugin")
    if not args.acceptance and args.plugin is not None:
        raise ValueError("--plugin is only meaningful with --acceptance")
    if args.preseed_skel and not args.acceptance:
        raise ValueError("--preseed-skel is only meaningful with --acceptance")
    bundle = args.bundle.resolve(strict=True)
    loader_input = args.loader.resolve(strict=True)
    stub = bundle / "Contents/MacOS/parsecd"
    if digest(stub) != STUB_SHA256:
        raise ValueError("unknown vendor stub; no fixture was created")
    run("codesign", "--verify", "--strict", str(bundle))
    manifest = bundle / "Contents/Resources/skel/appdata.json"
    skel = json.loads(manifest.read_text())
    name = skel["so_name"]
    if Path(name).name != name or skel["entry_symbol"] != "wx_main":
        raise ValueError("invalid vendor seed manifest")
    payload = manifest.parent / name
    if digest(payload) != skel["hash"]:
        raise ValueError("vendor seed digest mismatch")
    # Fresh state only. No account/config files are consulted or copied.
    scratch = Path(tempfile.mkdtemp(prefix="orbit-m1a-audit-", dir=args.scratch_parent)).resolve()
    os.chmod(scratch, 0o700)
    copy = scratch / "OrbitBootstrapAudit.app"
    run("ditto", str(bundle), str(copy))
    contents = copy / "Contents"
    info = contents / "Info.plist"
    plist = plistlib.loads(info.read_bytes())
    plist["CFBundleIdentifier"] = "dev.parsec-orbit.bootstrap-audit." + scratch.name
    plist["CFBundleDisplayName"] = ("Orbit Bootstrap Audit (acceptance)"
        if args.acceptance else "Orbit Bootstrap Audit (fail closed)")
    info.write_bytes(plistlib.dumps(plist))
    executable = contents / "MacOS/parsecd"
    run("lipo", str(stub), "-thin", "arm64", "-output", str(executable))
    patch_report = patch(executable)
    frameworks = contents / "Frameworks"
    frameworks.mkdir()
    loader = frameworks / "libparsec_orbit_loader.dylib"
    run("ditto", str(loader_input), str(loader))
    orbit = scratch / "orbit"
    orbit.mkdir(mode=0o700)
    home = orbit / "home"
    home.mkdir(mode=0o700)
    plugin_report = None
    preseed_report = {"seeded": False}
    if args.acceptance:
        plugin_input = args.plugin.resolve(strict=True)
        plugins = orbit / ("plugins/" + PLUGIN_ID)
        plugins.mkdir(mode=0o700, parents=True)
        staged_plugin = plugins / plugin_input.name
        run("ditto", str(plugin_input), str(staged_plugin))
        run("codesign", "--force", "--sign", "-", str(staged_plugin))
        entry = staged_plugin.name
        (plugins / "plugin.toml").write_text(
            f'id = "{PLUGIN_ID}"\nname = "Fixture"\nversion = "0.1.0"\n'
            f'kind = "dylib"\nentry = "{entry}"\n')
        (plugins / "fixture.cfg").write_text(PLUGIN_FIXTURE_CFG)
        plugin_report = {"id": PLUGIN_ID, "source": str(plugin_input),
            "staged": str(staged_plugin), "staged_sha256": digest(staged_plugin)}
        if args.preseed_skel:
            state = home / ".parsec"
            state.mkdir(mode=0o700)
            staged_skel = contents / "Resources/skel"
            run("ditto", str(staged_skel / name), str(state / name))
            run("ditto", str(staged_skel / "appdata.json"), str(state / "appdata.json"))
            for seeded in (state / name, state / "appdata.json"):
                os.chmod(seeded, 0o600)
            if digest(state / name) != skel["hash"]:
                raise ValueError("preseeded client digest mismatch")
            preseed_report = {"seeded": True, "client_sha256": digest(state / name),
                "appdata_sha256": digest(state / "appdata.json")}
    record = contents / "Resources/orbit-install.json"
    record.write_text(json.dumps({"schema_version": 1, "executable": str(executable),
        "loader": str(loader), "orbit_root": str(orbit), "uid": os.getuid()}, indent=2))
    os.chmod(record, 0o600)
    entitlements = scratch / "entitlements.plist"
    entitlements.write_bytes(plistlib.dumps({"com.apple.security.cs.disable-library-validation": True}))
    run("codesign", "--force", "--sign", "-", str(loader))
    run("codesign", "--force", "--sign", "-", "--options", "runtime",
        "--entitlements", str(entitlements), str(copy))
    run("codesign", "--verify", "--strict", str(copy))
    signed_stub = executable.read_bytes()
    for section in patch_report["file_backed_sections"]:
        offset, size = section["offset"], section["size"]
        if hashlib.sha256(signed_stub[offset:offset + size]).hexdigest() != section["sha256"]:
            raise ValueError("signing changed a vendor file-backed section")
    patch_report["instruction_bytes_preserved"] = True
    if digest(contents / "Resources/skel" / name) != skel["hash"] or digest(stub) != STUB_SHA256:
        raise ValueError("vendor byte-preservation check failed")
    applications = Path("/Applications/Parsec.app/Contents/MacOS/parsecd")
    if args.acceptance:
        mode = "acceptance"
        scope = "guarded ordinary-user acceptance run"
        expected_exit = None
        client_execution_allowed = True
    else:
        mode = "fail-closed"
        scope = "fail-closed pre-main audit only"
        expected_exit = 78
        client_execution_allowed = False
    plan = {"schema_version": 1, "mode": mode, "scope": scope,
        "scratch": str(scratch), "bundle": str(copy), "executable": str(executable),
        "orbit_root": str(orbit), "expected_exit": expected_exit,
        "client_execution_allowed": client_execution_allowed,
        "source_bundle": str(bundle),
        "source_stub_sha256": STUB_SHA256, "seed_sha256": skel["hash"],
        "seed_manifest_sha256": digest(contents / "Resources/skel/appdata.json"),
        "staged_executable_sha256": digest(executable), "staged_loader_sha256": digest(loader),
        "applications_stub_sha256": digest(applications) if applications.is_file() else None,
        "plugin": plugin_report, "preseed": preseed_report,
        "patch": patch_report, "quarantine_removed": False,
        "tracing_permission": "fs_usage requires root; this script never requests it"}
    plan_path = scratch / "audit-plan.json"
    plan_path.write_text(json.dumps(plan, indent=2) + "\n")
    print(plan_path)


if __name__ == "__main__":
    main()

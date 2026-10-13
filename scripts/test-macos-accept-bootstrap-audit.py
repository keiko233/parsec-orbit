#!/usr/bin/env python3
"""Mocked regressions for macos-accept-bootstrap-audit.py.

Everything runs against synthetic scratch trees under a temporary directory;
no staged vendor copy is launched, no sudo is attempted and no real account
path is opened. Run: python3 scripts/test-macos-accept-bootstrap-audit.py
"""

import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct
import tempfile

ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "macos_accept_runner", ROOT / "macos-accept-bootstrap-audit.py")
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)

PLUGIN_ID = "com.example.mac-e2e"


def arm64_image(payload=b""):
    return struct.pack("<II", 0xFEEDFACF, 0x100000C) + payload


class Fixture:
    """A synthetic acceptance scratch tree and its plan."""

    def __init__(self):
        self.root = Path(tempfile.mkdtemp(prefix="accept-test-")).resolve()
        os.chmod(self.root, 0o700)
        self.scratch = self.root / "orbit-m1a-audit-test"
        self.scratch.mkdir(mode=0o700)
        bundle = self.scratch / "OrbitBootstrapAudit.app"
        contents = bundle / "Contents"
        (contents / "MacOS").mkdir(parents=True)
        (contents / "Frameworks").mkdir()
        skel = contents / "Resources/skel"
        skel.mkdir(parents=True)
        self.executable = contents / "MacOS/parsecd"
        self.executable.write_bytes(arm64_image(b"stub"))
        self.loader = contents / "Frameworks/libparsec_orbit_loader.dylib"
        self.loader.write_bytes(b"loader")
        self.client_name = "parsecd-150-101a.dylib"
        self.client = skel / self.client_name
        self.client.write_bytes(b"client")
        self.manifest = skel / "appdata.json"
        self.manifest.write_text(json.dumps({"so_name": self.client_name,
            "entry_symbol": "wx_main", "hash": runner.sha256_file(self.client)}))
        self.orbit = self.scratch / "orbit"
        (self.orbit / "home").mkdir(parents=True)
        os.chmod(self.orbit, 0o700)
        plugin_dir = self.orbit / f"plugins/{PLUGIN_ID}"
        plugin_dir.mkdir(parents=True)
        self.plugin = plugin_dir / "libfixture.dylib"
        self.plugin.write_bytes(b"plugin")
        self.plan_path = self.scratch / "audit-plan.json"
        self.plan = {"schema_version": 1, "mode": "acceptance",
            "scope": "guarded ordinary-user acceptance run",
            "scratch": str(self.scratch), "bundle": str(bundle),
            "executable": str(self.executable), "orbit_root": str(self.orbit),
            "expected_exit": None, "client_execution_allowed": True,
            "source_stub_sha256": runner.sha256_file(self.executable),
            "seed_sha256": runner.sha256_file(self.client),
            "seed_manifest_sha256": runner.sha256_file(self.manifest),
            "staged_executable_sha256": runner.sha256_file(self.executable),
            "staged_loader_sha256": runner.sha256_file(self.loader),
            "applications_stub_sha256": None,
            "plugin": {"id": PLUGIN_ID, "source": str(self.plugin),
                "staged": str(self.plugin),
                "staged_sha256": runner.sha256_file(self.plugin)},
            "preseed": {"seeded": False}}
        self.write()

    def write(self):
        self.plan_path.write_text(json.dumps(self.plan))

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)


def expect_rejected(fixture, fragment, euid=None):
    fixture.write()
    try:
        runner.validate_plan(fixture.plan_path, euid=euid)
    except runner.PlanError as error:
        assert fragment in str(error), f"{fragment!r} not in {error!r}"
        return
    raise AssertionError(f"plan was accepted, expected {fragment!r}")


def evidence(loader_lines, vendor_lines, appdata=True, clients=None):
    return {"vendor_log": "/nonexistent", "loader_log": "/nonexistent",
        "vendor_lines": vendor_lines, "loader_lines": loader_lines,
        "state_appdata": appdata,
        "state_clients": ["parsecd-150-101a.dylib"] if clients is None else clients}


def healthy_lines():
    vendor = ["[F 2026-10-13 08:03:58] ===== Parsec: Started =====",
        "[D 2026-10-13 08:03:58] log: Parsec release (150-101a, Service: -1, Loader: 14)",
        "[D 2026-10-13 08:04:00] log: Parsec release (150-104a, Service: -1, Loader: 14)"]
    loader = ["[10-13 08:03:58.100] [INFO] [parsecd pid=42] client 150-101a detected; parsec-orbit.client/home/.parsec/parsecd-150-101a.dylib",
        "[10-13 08:03:58.120] [INFO] [parsecd pid=42] loaded com.example.fixture 0.1.0 (Fixture) ABI 1",
        "[10-13 08:03:58.130] [INFO] [parsecd pid=42] fixture[com.example.mac-e2e]: register com.example.mac-e2e -> 0",
        "[10-13 08:03:58.140] [INFO] [parsecd pid=42] fixture[com.example.mac-e2e]: query parsec-orbit.client -> 1.0 (64 bytes)",
        "[10-13 08:03:58.150] [INFO] [parsecd pid=42] ready: 1 loaded, 0 skipped, 0 failed",
        "[10-13 08:04:00.100] [INFO] [parsecd pid=42] client 150-104a detected; parsec-orbit.client/home/.parsec/parsecd-150-104a.dylib",
        "[10-13 08:04:00.150] [INFO] [parsecd pid=42] ready: 1 loaded, 0 skipped, 0 failed"]
    return loader, vendor


def test_valid_plan_accepted():
    fixture = Fixture()
    try:
        plan, ctx = runner.validate_plan(fixture.plan_path)
        assert plan["mode"] == "acceptance"
        assert ctx["seed_name"] == fixture.client_name
    finally:
        fixture.cleanup()


def test_mode_mismatch_rejected():
    fixture = Fixture()
    try:
        fixture.plan["mode"] = "fail-closed"
        expect_rejected(fixture, "guarded acceptance plan")
    finally:
        fixture.cleanup()


def test_client_execution_flag_required():
    fixture = Fixture()
    try:
        fixture.plan["client_execution_allowed"] = False
        expect_rejected(fixture, "does not allow client execution")
    finally:
        fixture.cleanup()


def test_escaping_payload_rejected():
    fixture = Fixture()
    try:
        outside = fixture.root.parent / "escapee-parsecd"
        outside.write_bytes(arm64_image())
        fixture.plan["executable"] = str(outside)
        expect_rejected(fixture, "escapes the scratch directory")
    finally:
        fixture.cleanup()


def test_hash_mismatch_rejected():
    fixture = Fixture()
    try:
        fixture.plugin.write_bytes(b"tampered")
        expect_rejected(fixture, "staged plugin does not match")
    finally:
        fixture.cleanup()


def test_root_euid_rejected():
    fixture = Fixture()
    try:
        expect_rejected(fixture, "ordinary user", euid=0)
    finally:
        fixture.cleanup()


def test_evidence_complete_passes():
    loader, vendor = healthy_lines()
    checks, detail = runner.evaluate_evidence(evidence(loader, vendor))
    assert not runner.failed_checks(checks), runner.failed_checks(checks)
    assert detail["second_detection"]
    assert detail["builds"] == ["101a", "104a"]


def test_missing_loader_token_fails():
    loader, vendor = healthy_lines()
    vendor = [line.replace("Loader: 14", "Loader: 13") for line in vendor]
    checks, _ = runner.evaluate_evidence(evidence(loader, vendor))
    assert "vendor_loader_token_14" in runner.failed_checks(checks)


def test_failed_plugin_count_fails():
    loader, vendor = healthy_lines()
    loader = [line.replace("0 failed", "1 failed") for line in loader]
    checks, _ = runner.evaluate_evidence(evidence(loader, vendor))
    assert "ready_zero_failed" in runner.failed_checks(checks)


def test_missing_query_fails():
    loader, vendor = healthy_lines()
    loader = [line for line in loader if "query parsec-orbit.client" not in line]
    checks, _ = runner.evaluate_evidence(evidence(loader, vendor))
    assert "fixture_client_query" in runner.failed_checks(checks)


def test_missing_state_image_fails():
    loader, vendor = healthy_lines()
    checks, _ = runner.evaluate_evidence(evidence(loader, vendor, clients=[]))
    assert "state_client_image" in runner.failed_checks(checks)


def test_real_state_change_detection():
    before = {"/Users/Shared/.parsec": {"exists": False}}
    after = {"/Users/Shared/.parsec": {"exists": True, "ino": 7}}
    assert runner.state_changed(before, after) == ["/Users/Shared/.parsec"]
    assert runner.state_changed(before, dict(before)) == []


def test_mach_o_check():
    fixture = Fixture()
    try:
        assert runner.mach_o_arm64(fixture.executable)
        x86 = fixture.root / "x86"
        x86.write_bytes(struct.pack("<II", 0xFEEDFACF, 0x01000007))
        assert not runner.mach_o_arm64(x86)
        script = fixture.root / "script"
        script.write_text("#!/bin/sh\n")
        assert not runner.mach_o_arm64(script)
    finally:
        fixture.cleanup()


def main():
    tests = [value for name, value in sorted(globals().items())
        if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
    print(f"{len(tests)} pass")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

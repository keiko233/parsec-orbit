#!/usr/bin/env python3
"""Focused network-manager tests; all state and HTTP traffic stay temporary/local."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile
import threading
import unittest
import urllib.error
import urllib.parse
import urllib.request
import warnings
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "orbit_manager_macos", ROOT / "scripts/orbit-manager-macos.py"
)
manager_module = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(manager_module)


FILES = sorted(manager_module.EXPECTED_FILES)


def make_bundle(
    extra: tuple[str, bytes] | None = None,
    *,
    duplicate: str | None = None,
    symlink: str | None = None,
) -> bytes:
    import io

    output = io.BytesIO()
    with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
        for name in FILES:
            contents = b"#!/bin/sh\nexit 0\n" if name.endswith(".sh") or name.endswith("installer-macos") else name.encode()
            info = zipfile.ZipInfo(name)
            kind = stat.S_IFLNK | 0o777 if name == symlink else stat.S_IFREG | 0o644
            info.external_attr = kind << 16
            archive.writestr(info, contents)
        if extra is not None:
            info = zipfile.ZipInfo(extra[0])
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            archive.writestr(info, extra[1])
        if duplicate is not None:
            info = zipfile.ZipInfo(duplicate)
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                archive.writestr(info, b"duplicate")
    return output.getvalue()


class FakeRunner:
    def __init__(self):
        self.events: list[tuple[str, list[str]]] = []
        self.managed = False
        self.recording_state: str | None = None
        self.inspect_class: str | None = None
        self.wrapper_fails = False
        self.wrapper_raises = False

    def __call__(self, argv: list[str], capture: bool) -> subprocess.CompletedProcess:
        if len(argv) > 1 and argv[1] == "verify-any":
            self.events.append(("verify", argv))
            return subprocess.run(argv, text=True, capture_output=True, check=False)
        if len(argv) > 1 and argv[1] == "inspect":
            self.events.append(("inspect", argv))
            classification = self.inspect_class or ("applied" if self.managed else "pristine")
            recording = {"state": self.recording_state or ("installed" if self.managed else "uninstalled")}
            return subprocess.CompletedProcess(
                argv,
                0,
                stdout=json.dumps({"classification": {"class": classification}, "recording": recording}),
                stderr="",
            )
        if argv[0].endswith("/scripts/install-macos.sh"):
            self.events.append(("wrapper", argv))
            if self.wrapper_raises:
                raise KeyboardInterrupt()
            if self.wrapper_fails:
                return subprocess.CompletedProcess(argv, 9, stdout="", stderr="simulated wrapper failure")
            if "--uninstall" in argv:
                self.managed = False
            else:
                self.managed = True
            return subprocess.CompletedProcess(argv, 0, stdout="", stderr="")
        raise AssertionError(f"unexpected process invocation: {argv}")


class LocalReleaseServer:
    def __init__(self):
        self.assets: dict[str, bytes] = {}
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                payload = owner.assets.get(self.path)
                if payload is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *_args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base = f"http://127.0.0.1:{self.server.server_port}"
        self.origin = manager_module.origin_of(self.base)

    def fetch(self, url: str, limit: int):
        if manager_module.origin_of(url) != self.origin:
            raise AssertionError(f"test transport was asked to leave loopback: {url}")
        try:
            with urllib.request.urlopen(url, timeout=3) as response:
                data = response.read(limit + 1)
                if len(data) > limit:
                    raise manager_module.ManagerError("test response exceeded the limit")
                return data, response.geturl()
        except urllib.error.HTTPError as error:
            raise manager_module.FetchUnavailable(f"fixture returned HTTP {error.code}") from error

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)


class OrbitManagerMacOSTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.market = ROOT / "target/release/parsec-orbit-market"
        if not cls.market.is_file() or not os.access(cls.market, os.X_OK):
            raise unittest.SkipTest("release-built parsec-orbit-market is unavailable")
        cls.signer_dir = tempfile.TemporaryDirectory(prefix="orbit-manager-signing-")
        cls.signer_root = Path(cls.signer_dir.name).resolve()
        cls.key_base = cls.signer_root / "ephemeral-release-key"
        generated = subprocess.run(
            [str(cls.market), "keygen", str(cls.key_base)],
            text=True,
            capture_output=True,
            check=True,
        )
        public_line = next(line for line in generated.stdout.splitlines() if line.startswith("ed25519 "))
        cls.public_key = re.match(r"ed25519 ([A-Za-z0-9+/=]+)", public_line).group(1)

    @classmethod
    def tearDownClass(cls):
        cls.signer_dir.cleanup()

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="orbit-manager-test-")
        self.temp_root = Path(self.temporary.name).resolve()
        self.root = self.temp_root / "OrbitRoot"
        self.app = self.temp_root / "Parsec.app"
        self.key_file = self.temp_root / "trusted-keys.txt"
        self.key_bytes = f"ed25519 {self.public_key} temporary integration-test key\n".encode()
        self.key_file.write_bytes(self.key_bytes)
        os.chmod(self.key_file, 0o600)
        self.bundle = make_bundle()
        self.server = LocalReleaseServer()
        self.runner = FakeRunner()
        self._real_atomic_write = manager_module.atomic_write
        self.output: list[str] = []
        self.manager = manager_module.Manager(
            self.root,
            fetcher=self.server.fetch,
            test_http_origins=frozenset({self.server.origin}),
            runner=self.runner,
            output=self.output.append,
        )
        self.manifest_url = self.server.base + "/release-macos.json"
        self.publish("1.2.3", self.bundle)

    def tearDown(self):
        self.server.close()
        self.temporary.cleanup()

    def publish(self, version: str, bundle: bytes, *, digest_override: str | None = None):
        verifier = self.market.read_bytes()
        manifest = {
            "format": 1,
            "platform": "macos-arm64",
            "version": version,
            "verifier": {
                "url": "parsec-orbit-market",
                "sha256": hashlib.sha256(verifier).hexdigest(),
                "size": len(verifier),
            },
            "bundle": {
                "url": "parsec-orbit-macos-arm64.zip",
                "sha256": digest_override or hashlib.sha256(bundle).hexdigest(),
                "size": len(bundle),
            },
        }
        manifest_bytes = (json.dumps(manifest, sort_keys=True) + "\n").encode()
        manifest_path = self.temp_root / "release-macos.json"
        manifest_path.write_bytes(manifest_bytes)
        subprocess.run(
            [str(self.market), "sign", str(manifest_path), "--key", str(self.key_base) + ".key"],
            text=True,
            capture_output=True,
            check=True,
        )
        self.server.assets["/release-macos.json"] = manifest_bytes
        self.server.assets["/release-macos.json.sig"] = manifest_path.with_name("release-macos.json.sig").read_bytes()
        self.server.assets["/parsec-orbit-market"] = verifier
        self.server.assets["/parsec-orbit-macos-arm64.zip"] = bundle

    def run_manager(self, command: str, *, allow_unsigned: bool = False):
        self.manager.run(
            command,
            app=self.app,
            manifest_url=self.manifest_url,
            key_override=self.key_file,
            allow_unsigned=allow_unsigned,
        )

    def test_signed_install_upgrade_and_offline_uninstall(self):
        self.run_manager("install")
        self.assertEqual([event[0] for event in self.runner.events], ["verify", "inspect", "wrapper"])
        self.assertEqual(self.key_file.read_bytes(), self.key_bytes)
        first_state = json.loads((self.root / ".network-install-manager/state.json").read_text())
        self.assertEqual(first_state["installed"]["version"], "1.2.3")
        self.assertTrue(first_state["bootstrap"]["signed"])

        self.runner.events.clear()
        self.publish("1.2.4", self.bundle)
        self.run_manager("upgrade")
        self.assertEqual([event[0] for event in self.runner.events], ["verify", "inspect", "wrapper"])
        self.assertIn("/tools/", self.runner.events[0][1][0])

        fetch_count = len(self.server.assets)
        self.runner.events.clear()
        self.manager.run(
            "uninstall",
            app=None,
            manifest_url=manager_module.DEFAULT_MANIFEST,
            key_override=None,
            allow_unsigned=False,
        )
        self.assertEqual([event[0] for event in self.runner.events], ["wrapper"])
        self.assertIn("--uninstall", self.runner.events[0][1])
        self.assertEqual(len(self.server.assets), fetch_count)
        state = json.loads((self.root / ".network-install-manager/state.json").read_text())
        self.assertIsNone(state["installed"])

    def test_invalid_signature_stops_before_inspection_or_wrapper(self):
        self.server.assets["/release-macos.json.sig"] = b"not a valid signature\n"
        with self.assertRaisesRegex(manager_module.ManagerError, "signature verification failed"):
            self.run_manager("install")
        self.assertEqual([event[0] for event in self.runner.events], ["verify"])
        self.assertFalse((self.root / ".network-install-manager/state.json").exists())

    def test_bundle_digest_mismatch_stops_before_inspection(self):
        wrong = "0" * 64
        self.publish("1.2.3", self.bundle, digest_override=wrong)
        with self.assertRaisesRegex(manager_module.ManagerError, "bundle size or sha256"):
            self.run_manager("install")
        self.assertEqual([event[0] for event in self.runner.events], ["verify"])

    def test_traversal_archive_is_rejected_before_inspection(self):
        bundle = make_bundle(("../escaped.txt", b"outside"))
        self.publish("1.2.3", bundle)
        with self.assertRaisesRegex(manager_module.ManagerError, "unsafe ZIP member"):
            self.run_manager("install")
        self.assertEqual([event[0] for event in self.runner.events], ["verify"])
        self.assertFalse((self.temp_root / "escaped.txt").exists())

    def test_duplicate_and_symlink_archive_entries_are_rejected(self):
        duplicate_bundle = make_bundle(duplicate=FILES[0])
        self.publish("1.2.3", duplicate_bundle)
        with self.assertRaisesRegex(manager_module.ManagerError, "duplicate ZIP member"):
            self.run_manager("install")
        self.assertEqual(self.runner.events[-1][0], "verify")

        self.runner.events.clear()
        symlink_bundle = make_bundle(symlink="scripts/install-macos.sh")
        self.publish("1.2.3", symlink_bundle)
        with self.assertRaisesRegex(manager_module.ManagerError, "non-regular ZIP member"):
            self.run_manager("install")
        self.assertEqual(self.runner.events[-1][0], "verify")

    def test_unsigned_bootstrap_is_not_promoted_and_signed_upgrade_rebootstraps(self):
        self.server.assets.pop("/release-macos.json.sig")
        self.run_manager("install", allow_unsigned=True)
        manager_dir = self.root / ".network-install-manager"
        state = json.loads((manager_dir / "state.json").read_text())
        self.assertFalse(state["installed"]["signed"])
        self.assertIsNone(state["bootstrap"])
        self.assertEqual(list((manager_dir / "tools").iterdir()), [])

        self.runner.events.clear()
        self.publish("1.2.4", self.bundle)
        self.run_manager("upgrade")
        self.assertEqual([event[0] for event in self.runner.events], ["verify", "inspect", "wrapper"])
        self.assertIn("/transactions/", self.runner.events[0][1][0])
        self.assertTrue(json.loads((manager_dir / "state.json").read_text())["bootstrap"]["signed"])

    def test_upgrade_adopts_an_already_applied_local_install(self):
        self.runner.managed = True
        self.run_manager("upgrade")
        self.assertEqual([event[0] for event in self.runner.events], ["verify", "inspect", "wrapper"])
        state = json.loads((self.root / ".network-install-manager/state.json").read_text())
        self.assertEqual(state["installed"]["app"], str(self.app))

    def test_restored_record_is_not_treated_as_applied(self):
        self.runner.recording_state = "uninstalled"
        self.run_manager("install")
        self.assertEqual(self.runner.events[-1][0], "wrapper")

    def test_stale_inspection_fails_closed(self):
        self.runner.managed = True
        self.runner.inspect_class = "stale"
        with self.assertRaisesRegex(manager_module.ManagerError, "stale or interrupted"):
            self.run_manager("upgrade")
        self.assertEqual([event[0] for event in self.runner.events], ["verify", "inspect"])

    def test_cached_tree_tampering_blocks_offline_uninstall(self):
        self.run_manager("install")
        state = json.loads((self.root / ".network-install-manager/state.json").read_text())
        wrapper = (
            self.root / ".network-install-manager" / "releases" / state["installed"]["cache_id"]
            / "tree/scripts/install-macos.sh"
        )
        wrapper.write_bytes(wrapper.read_bytes() + b"# modified\n")
        self.runner.events.clear()
        with self.assertRaisesRegex(manager_module.ManagerError, "digest mismatch"):
            self.manager.run(
                "uninstall",
                app=None,
                manifest_url=manager_module.DEFAULT_MANIFEST,
                key_override=None,
                allow_unsigned=False,
            )
        self.assertEqual(self.runner.events, [])

    def test_wrapper_failure_preserves_previous_state_and_cache(self):
        self.run_manager("install")
        manager_dir = self.root / ".network-install-manager"
        old_state = (manager_dir / "state.json").read_bytes()
        old_releases = sorted(path.name for path in (manager_dir / "releases").iterdir())
        self.publish("1.2.4", self.bundle)
        self.runner.wrapper_fails = True
        with self.assertRaisesRegex(manager_module.ManagerError, "wrapper failed"):
            self.run_manager("upgrade")
        self.assertEqual((manager_dir / "state.json").read_bytes(), old_state)
        self.assertEqual(sorted(path.name for path in (manager_dir / "releases").iterdir()), old_releases)
        self.assertFalse((manager_dir / "pending.json").exists())

    def test_existing_recovery_record_survives_repeated_retry_refusal(self):
        with mock.patch.object(manager_module, "atomic_write", side_effect=self.fail_state_write):
            with self.assertRaisesRegex(manager_module.ManagerError, "wrapper succeeded"):
                self.run_manager("install")
        pending_path = self.root / ".network-install-manager/pending.json"
        first_pending = pending_path.read_bytes()
        transaction_dirs = sorted(
            path.name for path in (self.root / ".network-install-manager/transactions").iterdir()
        )
        with self.assertRaisesRegex(manager_module.ManagerError, "recovery record"):
            self.run_manager("upgrade")
        self.assertEqual(pending_path.read_bytes(), first_pending)
        self.assertEqual(
            sorted(path.name for path in (self.root / ".network-install-manager/transactions").iterdir()),
            transaction_dirs,
        )

    def fail_state_write(self, path, data, mode=0o600):
        real_atomic_write = self._real_atomic_write
        if Path(path).name == "state.json":
            raise OSError("simulated manager state write failure")
        return real_atomic_write(path, data, mode)

    def test_github_release_redirect_fixture_pins_before_cdn(self):
        latest = "https://github.com/keiko233/parsec-orbit/releases/latest/download/release-macos.json"
        versioned = "https://github.com/keiko233/parsec-orbit/releases/download/v1.2.3/release-macos.json"
        cdn = "https://release-assets.githubusercontent.com/asset-id/release-macos.json"
        self.assertTrue(manager_module.redirect_is_allowed(latest, versioned))
        self.assertTrue(manager_module.redirect_is_allowed(latest, cdn))
        self.assertTrue(manager_module.final_redirect_is_allowed(latest, cdn))
        pinned = manager_module.pinned_manifest_url(latest, cdn, (latest, versioned, cdn))
        self.assertEqual(pinned, versioned)
        artifact = urllib.parse.urljoin(pinned, "parsec-orbit-macos-arm64.zip")
        self.assertIn("/releases/download/v1.2.3/", artifact)
        self.assertFalse(manager_module.redirect_is_allowed(latest, "https://attacker.invalid/payload"))
        self.assertFalse(manager_module.redirect_is_allowed(latest, "http://github.com/file"))

    def test_post_wrapper_state_failure_keeps_cache_and_recovery_record(self):
        with mock.patch.object(manager_module, "atomic_write", side_effect=self.fail_state_write):
            with self.assertRaisesRegex(manager_module.ManagerError, "wrapper succeeded"):
                self.run_manager("install")
        manager_dir = self.root / ".network-install-manager"
        pending = json.loads((manager_dir / "pending.json").read_text())
        self.assertTrue(pending["wrapper_succeeded"])
        self.assertTrue((manager_dir / "releases" / pending["cache_id"]).is_dir())
        self.assertTrue((manager_dir / "tools" / pending["candidate_verifier_sha256"]).is_dir())
        with self.assertRaisesRegex(manager_module.ManagerError, "recovery record"):
            self.run_manager("upgrade")

    def test_unknown_wrapper_completion_preserves_pending_and_staging(self):
        self.runner.wrapper_raises = True
        with self.assertRaises(KeyboardInterrupt):
            self.run_manager("install")
        manager_dir = self.root / ".network-install-manager"
        pending_path = manager_dir / "pending.json"
        first_pending = pending_path.read_bytes()
        self.assertEqual(json.loads(first_pending)["wrapper_status"], "unknown")
        transaction_dirs = sorted(path.name for path in (manager_dir / "transactions").iterdir())
        self.runner.wrapper_raises = False
        with self.assertRaisesRegex(manager_module.ManagerError, "recovery record"):
            self.run_manager("upgrade")
        self.assertEqual(pending_path.read_bytes(), first_pending)
        self.assertEqual(
            sorted(path.name for path in (manager_dir / "transactions").iterdir()),
            transaction_dirs,
        )

    def test_secure_file_read_does_not_block_on_a_fifo_race(self):
        path = self.temp_root / "key-file"
        path.write_text("regular before race\n")
        original_open = os.open
        replaced = False

        def replace_with_fifo(candidate, flags, *args, **kwargs):
            nonlocal replaced
            if Path(candidate) == path and not replaced:
                path.unlink()
                os.mkfifo(path)
                replaced = True
            return original_open(candidate, flags, *args, **kwargs)

        with mock.patch.object(manager_module.os, "open", side_effect=replace_with_fifo):
            with self.assertRaisesRegex(manager_module.ManagerError, "changed while it was opened"):
                manager_module.read_secure_file(path, os.geteuid(), "test key", 1024)
        self.assertTrue(replaced)


if __name__ == "__main__":
    unittest.main(verbosity=2)

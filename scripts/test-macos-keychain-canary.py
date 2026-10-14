"""Transaction tests with mocked Security commands; never accesses Keychain."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("canary", Path(__file__).with_name("macos-keychain-canary.py"))
canary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(canary)


class TransactionTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="orbit-keychain-controller-test-")
        self.root = Path(self.scratch.name).resolve()
        self.controller, self.client = self.root / "controller", self.root / "client"
        self.controller.write_bytes(b"mock controller; not executable")
        self.client.write_bytes(b"mock client; not executable")
        self.plan = self.root / "keychain-plan.json"
        self.plan.write_text(json.dumps({"scope": "two owned dummy Keychain items only", "payload_is_fake": True,
            "scratch": str(self.root), "controller": str(self.controller), "client": str(self.client),
            "controller_sha256": canary.digest(self.controller), "client_sha256": canary.digest(self.client),
            "services": ["fake-controller-item", "fake-client-item"]}))
        self.calls = []
        self.deleted = set()

    def tearDown(self):
        self.scratch.cleanup()

    def command(self, argv):
        self.calls.append(argv)
        if len(argv) == 1:
            return SimpleNamespace(returncode=0, stdout="probe 0 -25293 0\nprobe 1 0 1\n", stderr="")
        operation, i = argv[1:]
        if operation == "delete":
            self.deleted.add(i)
        if operation == "probe" and i in self.deleted:
            return SimpleNamespace(returncode=1, stdout=f"probe {i} -25300 0\n", stderr="")
        stdout = f"probe {i} 0 1\n" if operation == "probe" else f"{operation} {i} 0\n"
        return SimpleNamespace(returncode=0, stdout=stdout, stderr="")

    def run_mock(self, command):
        with patch.object(canary, "command", side_effect=command), contextlib.redirect_stdout(io.StringIO()):
            code = canary.run_plan(self.plan)
        return code, json.loads((self.root / "keychain-result.json").read_text())

    def test_success_removes_only_the_two_own_items_and_preserves_raw_statuses(self):
        code, report = self.run_mock(self.command)
        self.assertEqual(code, 0)
        self.assertEqual(report["status"], "measured")
        self.assertFalse(report["complete_isolation_proven"])
        self.assertEqual(report["client_observations"][0]["status"], -25293)
        self.assertTrue(report["client_observations"][1]["payload_matched"])
        self.assertTrue(all(row["item_absent"] for row in report["cleanup_verification"]))
        self.assertEqual([args[2] for args in self.calls if len(args) == 3 and args[1] == "delete"], ["0", "1"])

    def test_timed_out_add_is_cleaned_even_without_success_response(self):
        def command(argv):
            if len(argv) == 3 and argv[1] == "add":
                self.calls.append(argv)
                raise subprocess.TimeoutExpired(argv, 15)
            return self.command(argv)
        code, report = self.run_mock(command)
        self.assertEqual(code, 2)
        self.assertEqual(report["status"], "incomplete")
        self.assertEqual(report["cleanup"][0]["index"], 0)
        self.assertEqual(len(report["cleanup"]), 1)
        self.assertFalse(any(len(args) == 1 for args in self.calls))

    def test_cleanup_failure_is_not_hidden_by_completed_client_probes(self):
        def command(argv):
            if len(argv) == 3 and argv[1] == "delete":
                self.calls.append(argv)
                return SimpleNamespace(returncode=1, stdout=f"delete {argv[2]} -25291\n", stderr="")
            return self.command(argv)
        code, report = self.run_mock(command)
        self.assertEqual(code, 2)
        self.assertEqual(report["status"], "cleanup-failed")
        self.assertEqual(len(report["cleanup"]), 2)

    def test_changed_payload_never_reaches_any_security_command(self):
        self.client.write_bytes(b"changed")
        with patch.object(canary, "command", side_effect=self.command):
            with self.assertRaises(ValueError):
                canary.run_plan(self.plan)
        self.assertEqual(self.calls, [])

    def test_successful_delete_response_without_absence_is_cleanup_failure(self):
        def command(argv):
            if len(argv) == 3 and argv[1] == "delete":
                self.calls.append(argv)
                return SimpleNamespace(returncode=0, stdout=f"delete {argv[2]} 0\n", stderr="")
            return self.command(argv)
        code, report = self.run_mock(command)
        self.assertEqual(code, 2)
        self.assertEqual(report["status"], "cleanup-failed")
        self.assertFalse(any(row["item_absent"] for row in report["cleanup_verification"]))


if __name__ == "__main__":
    unittest.main()

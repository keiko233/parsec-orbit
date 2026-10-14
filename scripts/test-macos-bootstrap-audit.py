"""Audit evidence regressions and the ordinary-user pre-exec protocol.

No vendor image, account state or privileged tracer is executed by these tests.
"""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("audit", Path(__file__).with_name("macos-run-bootstrap-audit.py"))
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)
spawn_spec = importlib.util.spec_from_file_location("suspended", Path(__file__).with_name("macos-run-suspended-bootstrap-audit.py"))
suspended = importlib.util.module_from_spec(spawn_spec)
spawn_spec.loader.exec_module(suspended)


class EvidenceTests(unittest.TestCase):
    def test_exit_only_is_not_a_file_audit(self):
        trace = "18:34:13.878278  exit  0.000005 parsecd.4466691\n"
        result = audit.trace_assessment(trace, "trace-probe-one.txt", 0)
        self.assertEqual(result["status"], "incomplete")
        self.assertFalse(result["pre_exec_probe_observed"])
        self.assertFalse(result["installation_record_observed"])

    def test_both_sides_of_exec_are_required(self):
        probe = "18:34:13.800001  open F=3 /scratch/trace-probe-one.txt 0.000005 gate.123\n"
        record = "18:34:13.870001  open F=4 /scratch/orbit-install.json 0.000005 parsecd.123\n"
        self.assertEqual(audit.trace_assessment(probe, "trace-probe-one.txt", 0)["status"], "incomplete")
        self.assertEqual(audit.trace_assessment(record, "trace-probe-one.txt", 0)["status"], "incomplete")
        result = audit.trace_assessment(probe + record, "trace-probe-one.txt", 0)
        self.assertEqual(result["status"], "startup-paths-observed")
        self.assertFalse(result["complete_isolation_proven"])
        self.assertEqual(audit.trace_assessment(probe + record, "trace-probe-two.txt", 0)["status"], "incomplete")

    def test_diagnostics_and_trace_loss_cannot_pass(self):
        diagnostic = "ERROR: open failed /scratch/trace-probe-one.txt\n"
        self.assertFalse(audit.path_event(diagnostic, "trace-probe-one.txt"))
        trace = "18:34:13.800001  open F=3 trace-probe-one.txt 0.000005 gate.123\n"
        trace += "18:34:13.870001  lstat64 orbit-install.json 0.000005 parsecd.123\n"
        self.assertEqual(audit.trace_assessment(trace, "trace-probe-one.txt", 1)["status"], "incomplete")
        trace += "fs_usage: buffer overrun, events generated too quickly\n"
        self.assertTrue(audit.trace_assessment(trace, "trace-probe-one.txt", 0)["event_loss_reported"])
        self.assertEqual(audit.trace_assessment(trace, "trace-probe-one.txt", 0)["status"], "incomplete")


@unittest.skipUnless(sys.platform == "darwin", "requires libproc")
class GateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = tempfile.TemporaryDirectory(prefix="orbit-audit-protocol-")
        cls.root = Path(cls.scratch.name).resolve()
        source = cls.root / "gate.c"
        source.write_text(audit.HELPER)
        cls.helper = cls.root / "gate"
        subprocess.run(["clang", "-Wall", "-Werror", str(source), "-o", str(cls.helper)], check=True)
        subprocess.run(["codesign", "--force", "--sign", "-", str(cls.helper)], check=True, capture_output=True)
        cls.probe = cls.root / "probe"
        cls.probe.write_text("P")

    @classmethod
    def tearDownClass(cls):
        cls.scratch.cleanup()

    def run_gate(self, commands, probe=None):
        result = subprocess.run([str(self.helper), "/usr/bin/false", str(probe or self.probe)],
            input=commands, text=True, capture_output=True, timeout=5)
        self.assertEqual(len(result.stdout.splitlines()), 2)
        self.assertEqual(Path(result.stdout.splitlines()[1]), self.helper)
        return result.returncode

    def test_eof_and_probe_only_never_exec(self):
        self.assertEqual(self.run_gate(""), 6)
        self.assertEqual(self.run_gate("PP"), 6)

    def test_probe_then_explicit_go_execs(self):
        self.assertEqual(self.run_gate("PPG"), 1)  # /usr/bin/false

    def test_failed_probe_and_invalid_protocol_withhold_exec(self):
        self.assertEqual(self.run_gate("XG"), 6)
        self.assertEqual(self.run_gate("PG", self.root / "missing"), 8)
        empty = self.root / "empty"
        empty.touch()
        self.assertEqual(self.run_gate("PG", empty), 9)
        alias = self.root / "alias"
        alias.symlink_to(self.probe)
        self.assertEqual(self.run_gate("PG", alias), 8)


@unittest.skipUnless(sys.platform == "darwin", "requires macOS start-suspended")
class SuspendedTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = tempfile.TemporaryDirectory(prefix="orbit-suspended-protocol-")
        cls.root = Path(cls.scratch.name).resolve()
        cls.bridge = suspended.build_bridge(cls.root)
        cls.marker = cls.root / "constructor-entered"
        source = cls.root / "target.c"
        import json
        source.write_text('#include <fcntl.h>\n#include <unistd.h>\n'
            '__attribute__((constructor)) static void init(void) { '
            f'int fd = open({json.dumps(str(cls.marker))}, O_WRONLY | O_CREAT, 0600); '
            'if (fd >= 0) { write(fd, "C", 1); close(fd); } }\nint main(void) { return 78; }\n')
        cls.executable = cls.root / "target"
        subprocess.run(["clang", "-Wall", "-Werror", str(source), "-o", str(cls.executable)], check=True)
        subprocess.run(["codesign", "--force", "--sign", "-", "--options", "runtime", str(cls.executable)],
            check=True, capture_output=True)

    @classmethod
    def tearDownClass(cls):
        cls.scratch.cleanup()

    def test_stopped_after_exec_before_constructor_then_resume(self):
        import signal
        with (self.root / "stdout").open("w") as out, (self.root / "stderr").open("w") as err:
            target = suspended.SuspendedTarget(self.bridge, self.executable, out.fileno(), err.fileno())
            try:
                self.assertEqual(self.bridge.target_stopped(target.pid), 1)
                self.assertFalse(self.marker.exists())
                self.assertEqual(Path(target.identity()[1]), self.executable)
                target.checked_signal(signal.SIGCONT)
                self.assertEqual(target.wait(5), 78)
                self.assertEqual(self.marker.read_text(), "C")
            finally:
                target.stop()
        self.marker.unlink()

    def test_cancel_before_any_constructor_runs(self):
        import signal
        with (self.root / "stdout").open("w") as out, (self.root / "stderr").open("w") as err:
            target = suspended.SuspendedTarget(self.bridge, self.executable, out.fileno(), err.fileno())
            self.assertFalse(self.marker.exists())
            self.assertEqual(target.stop(), -signal.SIGKILL)
            self.assertFalse(self.marker.exists())


if __name__ == "__main__":
    unittest.main()

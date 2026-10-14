#!/usr/bin/env python3
"""Exercise safe log opening and real append, rotation, and truncation behavior."""

import contextlib
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().with_name("console-macos.py")
SPEC = importlib.util.spec_from_file_location("console_macos", SCRIPT)
console = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(console)


class ConsoleMacOSTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="orbit console test ")
        self.root = Path(self.temp.name).resolve()

    def tearDown(self):
        self.temp.cleanup()

    def test_once_reads_bounded_tagged_snapshots_from_documented_paths(self):
        (self.root / "parsec-orbit.log").write_text("orbit line\n", encoding="utf-8")
        state = self.root / "home" / ".parsec"
        state.mkdir(parents=True)
        (state / "log.txt").write_text("parsec line\n", encoding="utf-8")
        sources = console.log_sources(self.root)
        self.assertEqual(sources[0].snapshot(), ["[orbit] orbit line"])
        self.assertEqual(sources[1].snapshot(), ["[parsec] parsec line"])

    def test_once_keeps_only_the_most_recent_line_limit(self):
        path = self.root / "parsec-orbit.log"
        path.write_text("".join(f"line {index}\n" for index in range(1005)),
            encoding="utf-8")
        lines = console.LogTail(path, "orbit").snapshot()
        self.assertEqual(len(lines), console.MAX_LINES_PER_POLL)
        self.assertEqual(lines[0], "[orbit] line 5")
        self.assertEqual(lines[-1], "[orbit] line 1004")

    def test_follow_starts_at_end_and_reads_appends(self):
        path = self.root / "parsec-orbit.log"
        path.write_text("old\n", encoding="utf-8")
        tail = console.LogTail(path, "orbit")
        self.assertEqual(tail.poll(), [])
        with path.open("ab") as log:
            log.write("new ✓\n".encode("utf-8"))
        self.assertEqual(tail.poll(), ["[orbit] new ✓"])

    def test_missing_log_appearing_later_is_read_from_the_beginning(self):
        path = self.root / "home" / ".parsec" / "log.txt"
        tail = console.LogTail(path, "orbit")
        self.assertEqual(tail.poll(), [])
        path.parent.mkdir(parents=True)
        path.write_text("appeared\n", encoding="utf-8")
        self.assertEqual(tail.poll(), ["[orbit] appeared"])

    def test_tiny_line_flood_keeps_pending_bytes_bounded(self):
        path = self.root / "parsec-orbit.log"
        path.write_text("existing\n", encoding="utf-8")
        tail = console.LogTail(path, "orbit")
        tail.poll()
        with path.open("ab") as log:
            for _ in range(20):
                log.write(b"x\n" * 10000)
                tail.poll()
                self.assertLessEqual(len(tail.pending), console.MAX_PENDING_BYTES)

    def test_follow_restarts_on_rotation_and_truncation(self):
        path = self.root / "parsec-orbit.log"
        path.write_text("initial\n", encoding="utf-8")
        tail = console.LogTail(path, "orbit")
        tail.poll()

        rotated = self.root / "parsec-orbit.log.1"
        path.rename(rotated)
        path.write_text("rotated start\n", encoding="utf-8")
        self.assertEqual(tail.poll(), ["[orbit] rotated start"])

        path.write_text("x\n", encoding="utf-8")
        self.assertEqual(tail.poll(), ["[orbit] x"])

    def test_refuses_symlink_hardlink_fifo_and_foreign_owner(self):
        target = self.root / "target.log"
        target.write_text("safe\n", encoding="utf-8")
        symlink = self.root / "symlink.log"
        symlink.symlink_to(target)
        with self.assertRaises(console.UnsafeLogError):
            console.open_log(symlink)

        linked = self.root / "hardlink.log"
        os.link(target, linked)
        with self.assertRaises(console.UnsafeLogError):
            console.open_log(linked)

        fifo = self.root / "pipe.log"
        os.mkfifo(fifo)
        with self.assertRaises(console.UnsafeLogError):
            console.open_log(fifo)

        foreign = self.root / "foreign-owner.log"
        foreign.write_text("separate inode\n", encoding="utf-8")
        self.assertEqual(foreign.stat().st_nlink, 1)
        with mock.patch.object(console.os, "getuid", return_value=os.getuid() + 1):
            with self.assertRaises(console.UnsafeLogError):
                console.open_log(foreign)

    def test_refuses_symlink_parent_components(self):
        real_dir = self.root / "real"
        real_dir.mkdir()
        (real_dir / "parsec-orbit.log").write_text("hidden\n", encoding="utf-8")
        link = self.root / "linked"
        link.symlink_to(real_dir, target_is_directory=True)
        with self.assertRaises(console.UnsafeLogError):
            console.open_log(link / "parsec-orbit.log")

    def test_refuses_parent_traversal_components(self):
        with self.assertRaises(console.UnsafeLogError):
            console.open_log(self.root / ".." / "outside.log")

    def test_once_returns_failure_for_an_unsafe_log(self):
        link = self.root / "parsec-orbit.log"
        target = self.root / "real.log"
        target.write_text("unsafe target\n", encoding="utf-8")
        link.symlink_to(target)
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            result = console.main(["--orbit-root", str(self.root), "--once"])
        self.assertEqual(result, 1)
        self.assertIn("refusing non-regular log", stderr.getvalue())

    def test_continuous_mode_reports_repeated_refusal_once(self):
        target = self.root / "real.log"
        target.write_text("unsafe\n", encoding="utf-8")
        (self.root / "parsec-orbit.log").symlink_to(target)
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), mock.patch.object(
                console.time, "sleep", side_effect=[None, None, KeyboardInterrupt]):
            result = console.main(["--orbit-root", str(self.root)])
        self.assertEqual(result, 130)
        self.assertEqual(stderr.getvalue().count("refusing non-regular log"), 1)

    def test_rejects_relative_orbit_root(self):
        with self.assertRaises(SystemExit) as error:
            console.parse_args(["--orbit-root", "relative/path"])
        self.assertEqual(error.exception.code, 2)


if __name__ == "__main__":
    unittest.main()

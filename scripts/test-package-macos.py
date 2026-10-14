#!/usr/bin/env python3
"""Check release archive layout, executable modes and digest binding using owned files."""

import importlib.util
import hashlib
from pathlib import Path
import stat
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location("package_macos", Path(__file__).with_name("package-macos.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class PackageTests(unittest.TestCase):
    def test_explicit_layout_preserves_executable_modes_and_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "binary"
            manifest = root / "manifest"
            binary.write_bytes(b"owned executable fixture")
            manifest.write_text('kind = "dylib"\n')
            destination = root / "distribution.zip"
            module.write_bundle(destination, {"target/release/tool": binary,
                                              "crates/plugin/plugin.macos.toml": manifest})
            with zipfile.ZipFile(destination) as archive:
                self.assertEqual(set(archive.namelist()), {"target/release/tool", "crates/plugin/plugin.macos.toml"})
                self.assertEqual(archive.read("target/release/tool"), binary.read_bytes())
                self.assertEqual(archive.getinfo("target/release/tool").external_attr >> 16, stat.S_IFREG | 0o755)
                self.assertEqual(archive.getinfo("crates/plugin/plugin.macos.toml").external_attr >> 16, stat.S_IFREG | 0o644)
            evidence = module.artifact(destination)
            self.assertEqual(evidence["size"], len(destination.read_bytes()))
            self.assertEqual(evidence["sha256"], hashlib.sha256(destination.read_bytes()).hexdigest())

    def test_unlisted_files_are_never_archived(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "included"
            source.write_bytes(b"included")
            (root / "unrelated-secret").write_bytes(b"not included")
            module.write_bundle(root / "bundle.zip", {"scripts/install-macos.sh": source})
            with zipfile.ZipFile(root / "bundle.zip") as archive:
                self.assertEqual(archive.namelist(), ["scripts/install-macos.sh"])

    def test_symlink_source_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "source").write_bytes(b"fixture")
            (root / "link").symlink_to(root / "source")
            with self.assertRaisesRegex(ValueError, "regular release file"):
                module.write_bundle(root / "bundle.zip", {"target/release/tool": root / "link"})


if __name__ == "__main__":
    unittest.main()

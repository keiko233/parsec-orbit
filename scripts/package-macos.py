#!/usr/bin/env python3
"""Build the arm64 distribution consumed by orbit-manager-macos.

Run on macOS before publishing a release. Only repository-owned artifacts are
signed; no installed Parsec bundle or user data is accessed. The optional
Ed25519 key signs the distribution manifest through the existing market tool.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import tempfile
import zipfile


ROOT = Path(__file__).resolve().parent.parent
PLUGINS = ("config", "hooks", "js-runtime")


def run(*args):
    subprocess.run([str(arg) for arg in args], cwd=ROOT, check=True)


def artifact(path):
    return {"url": path.name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "size": path.stat().st_size}


def write_bundle(destination, sources):
    """Use explicit members rather than recursively archiving build/state trees."""
    with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for member, source in sorted(sources.items()):
            if source.is_symlink() or not source.is_file():
                raise ValueError(f"expected a regular release file: {source}")
            info = zipfile.ZipInfo(member)
            info.create_system = 3
            info.external_attr = (stat.S_IFREG | (0o755 if member.startswith("target/release/")
                                                  or member.endswith(".sh") else 0o644)) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, source.read_bytes())


def package(out, signing_key=None, no_build=False):
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise ValueError("packaging requires macOS arm64")
    if os.getuid() == 0:
        raise ValueError("run packaging as an ordinary user")
    version = re.search(r'^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M)
    if not version:
        raise ValueError("workspace version is missing")
    if not no_build:
        args = ["cargo", "build", "--release", "--locked"]
        for name in ("installer-macos", "loader-macos", "market") + tuple("plugin-" + p for p in PLUGINS):
            args.extend(("-p", "parsec-orbit-" + name))
        run(*args)
    release = ROOT / "target/release"
    names = ["parsec-orbit-installer-macos", "libparsec_orbit_loader.dylib", "parsec-orbit-market"]
    names.extend("libparsec_orbit_plugin_" + p.replace("-", "_") + ".dylib" for p in PLUGINS)
    out.mkdir(parents=True, exist_ok=True)
    if out.is_symlink():
        raise ValueError("output directory must not be a symlink")
    # Stage and sign copies so the user's Cargo artifacts keep their identities.
    with tempfile.TemporaryDirectory(prefix=".package-macos-", dir=out) as directory:
        stage = Path(directory)
        sources = {}
        for name in names:
            source = release / name
            if source.is_symlink() or not source.is_file():
                raise ValueError(f"missing regular release artifact: {source}")
            copied = stage / name
            shutil.copyfile(source, copied)
            copied.chmod(0o755)
            run("/usr/bin/lipo", copied, "-verify_arch", "arm64")
            run("/usr/bin/codesign", "--force", "--sign", "-", copied)
            run("/usr/bin/codesign", "--verify", "--strict", copied)
            if name != "parsec-orbit-market":
                sources["target/release/" + name] = copied
        for plugin in PLUGINS:
            member = f"crates/parsec-orbit-plugin-{plugin}/plugin.macos.toml"
            sources[member] = ROOT / member
        for name in ("install-macos.sh", "console-macos.sh", "console-macos.py"):
            sources["scripts/" + name] = ROOT / "scripts" / name
        bundle = stage / "parsec-orbit-macos-arm64.zip"
        write_bundle(bundle, sources)
        manifest = stage / "release-macos.json"
        manifest.write_text(json.dumps({"format": 1, "platform": "macos-arm64",
                                       "version": version.group(1),
                                       "verifier": artifact(stage / "parsec-orbit-market"),
                                       "bundle": artifact(bundle)}, indent=2) + "\n")
        outputs = [bundle, manifest, stage / "parsec-orbit-market"]
        if signing_key:
            run(stage / "parsec-orbit-market", "sign", manifest, "--key", signing_key.resolve())
            outputs.append(Path(str(manifest) + ".sig"))
        else:
            print("No signing key: network installation will require explicit --allow-unsigned.")
        for name in ("orbit-manager-macos.sh", "orbit-manager-macos.py",
                     "console-macos.sh", "console-macos.py"):
            source = ROOT / "scripts" / name
            copied = stage / name
            shutil.copyfile(source, copied)
            copied.chmod(0o755)
            outputs.append(copied)
        checksums = stage / "SHA256SUMS-macos.txt"
        checksums.write_text("".join(f"{artifact(path)['sha256']}  {path.name}\n"
                                     for path in sorted(outputs)))
        outputs.append(checksums)
        # Avoid retaining a stale signature after an unsigned rebuild.
        if not signing_key:
            (out / "release-macos.json.sig").unlink(missing_ok=True)
        for source in outputs:
            os.replace(source, out / source.name)
    print(f"Packaged macOS {version.group(1)} in {out}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=ROOT / "dist/macos")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--signing-key", type=Path)
    args = parser.parse_args()
    try:
        package(args.out.absolute(), args.signing_key, args.no_build)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"error: {error}\n")


if __name__ == "__main__":
    main()

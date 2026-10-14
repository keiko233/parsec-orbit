#!/usr/bin/env python3
"""Nested App Sandbox + custom SBPL canary; scratch-only measurement fixture.

Measures whether an App Sandboxed process can additionally apply a custom SBPL
deny profile through sandbox_init(flags=0) at constructor time (the M1a
"hardening layer" question), and whether that extra layer, if accepted, closes
the metadata-stat gap the plain App Sandbox canary exposed. Also verifies that
an owned payload dylib in a temporary-exception path can still be dlopened
under the App Sandbox. No vendor code, real account path or Keychain API is
used; every probed path is created under a fresh private scratch directory.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import plistlib
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

PAYLOAD_C = r'''
int nested_canary_payload_value(void) { return 42; }
'''

SOURCE = r'''
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <sandbox.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int nested_rc = -999;
static char nested_error[1024];

static void emit(const char *name, int ok, int error) {
    printf("RESULT %s %d %d\n", name, ok, error);
    fflush(stdout);
}

static void sbpl_quote(const char *s, char *out, size_t n) {
    size_t j = 0;
    for (size_t i = 0; s[i] && j + 3 < n; ++i) {
        if (s[i] == '\\' || s[i] == '"') out[j++] = '\\';
        out[j++] = s[i];
    }
    out[j] = 0;
}

__attribute__((constructor)) static void early(void) {
    const char *root = getenv("NESTED_PROTECTED");
    const char *mode = getenv("NESTED_MODE");
    if (!root || !mode || strcmp(mode, "nested") != 0) return;
    char quoted[4096], profile[8192], *error = NULL;
    sbpl_quote(root, quoted, sizeof(quoted));
    snprintf(profile, sizeof(profile),
        "(version 1)\n"
        "(allow default)\n"
        "(deny file-read* (subpath \"%s\"))\n"
        "(deny file-write* (subpath \"%s\"))\n", quoted, quoted);
    nested_rc = sandbox_init(profile, 0, &error);
    if (error) {
        snprintf(nested_error, sizeof(nested_error), "%s", error);
        sandbox_free_error(error);
    }
}

int main(void) {
    const char *blocked = getenv("NESTED_PROTECTED_FILE");
    const char *allowed = getenv("NESTED_ALLOWED_FILE");
    const char *payload = getenv("NESTED_PAYLOAD");
    if (!blocked || !allowed || !payload) return 90;
    printf("NESTED_INIT %d\n", nested_rc);
    printf("NESTED_ERROR %s\n", nested_error[0] ? nested_error : "(none)");
    errno = 0; int fd = open(blocked, O_RDONLY); int error = errno;
    emit("protected_open", fd >= 0, error); if (fd >= 0) close(fd);
    struct stat st;
    errno = 0; int rc = stat(blocked, &st); error = errno;
    emit("protected_stat", rc == 0, error);
    errno = 0; rc = lstat(blocked, &st); error = errno;
    emit("protected_lstat", rc == 0, error);
    errno = 0; fd = open(allowed, O_RDWR | O_APPEND); error = errno;
    emit("allowed_open", fd >= 0, error);
    if (fd >= 0) {
        errno = 0; ssize_t n = write(fd, "n", 1); error = errno;
        emit("allowed_write", n == 1, error);
        close(fd);
    }
    errno = 0; void *handle = dlopen(payload, RTLD_NOW | RTLD_LOCAL); error = errno;
    emit("allowed_payload_dlopen", handle != NULL, error);
    if (handle) {
        int (*value)(void) = (int (*)(void))dlsym(handle, "nested_canary_payload_value");
        emit("allowed_payload_call", value != NULL && value() == 42, 0);
    } else {
        emit("allowed_payload_call", 0, error);
    }
    return 0;
}
'''


def command(args: list[str]) -> None:
    subprocess.run(args, check=True, capture_output=True, text=True, timeout=60)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="run the owned fixture after staging")
    args = parser.parse_args()
    if sys.platform != "darwin" or platform.machine() != "arm64" or os.geteuid() == 0:
        raise ValueError("requires an ordinary-user macOS arm64 host")
    root = Path(tempfile.mkdtemp(prefix="orbit-nested-", dir="/private/tmp")).resolve(strict=True)
    os.chmod(root, 0o700)
    allowed, protected = root / "allowed", root / "protected"
    allowed.mkdir(mode=0o700)
    protected.mkdir(mode=0o700)
    (protected / "data").write_text("scratch protected canary\n")
    (protected / "data").chmod(0o600)
    (allowed / "data").write_text("scratch allowed canary\n")
    (allowed / "data").chmod(0o600)
    (allowed / "protected-alias").symlink_to(protected / "data")

    payload = allowed / "libnested_payload.dylib"
    (root / "payload.c").write_text(PAYLOAD_C)
    source = root / "canary.c"
    source.write_text(SOURCE)
    bundle_id = "dev.parsec-orbit.nested-canary." + uuid.uuid4().hex
    contents = root / "NestedSandboxCanary.app/Contents"
    (contents / "MacOS").mkdir(parents=True, mode=0o700)
    executable = contents / "MacOS/nested-canary"
    info = {
        "CFBundleIdentifier": bundle_id,
        "CFBundleExecutable": "nested-canary",
        "CFBundlePackageType": "APPL",
        "CFBundleName": "OrbitNestedSandboxCanary",
        "CFBundleVersion": "1",
    }
    (contents / "Info.plist").write_bytes(plistlib.dumps(info))
    entitlements = {
        "com.apple.security.app-sandbox": True,
        "com.apple.security.cs.disable-library-validation": True,
        "com.apple.security.temporary-exception.files.absolute-path.read-write": [str(allowed) + "/"],
    }
    entitlements_file = root / "entitlements.plist"
    entitlements_file.write_bytes(plistlib.dumps(entitlements))

    command(["clang", "-arch", "arm64", "-dynamiclib", "-Wno-deprecated-declarations",
             str(root / "payload.c"), "-o", str(payload)])
    command(["codesign", "--force", "--sign", "-", "--options", "runtime", str(payload)])
    command(["clang", "-arch", "arm64", "-Wno-deprecated-declarations", str(source), "-o", str(executable)])
    command(["codesign", "--force", "--sign", "-", "--options", "runtime", "--entitlements",
             str(entitlements_file), str(contents.parent)])
    command(["codesign", "--verify", "--strict", str(contents.parent)])
    command(["codesign", "--verify", "--strict", str(payload)])

    report: dict = {
        "fixture": "macos-nested-sandbox-canary",
        "scope": "owned scratch fixture only; no vendor, account state or Keychain",
        "host_macos": platform.mac_ver()[0],
        "host_architecture": platform.machine(),
        "scratch": str(root),
        "bundle_id": bundle_id,
        "entitlements": entitlements,
        "mechanism": {
            "first_layer": "App Sandbox entitlement (kernel-enforced from exec)",
            "second_layer": "sandbox_init(custom SBPL, flags=0) in a constructor, deprecated/unsupported API",
        },
        "strict_signature_verified": True,
        "inherited_fd_note": "no inherited protected descriptor is used by this fixture",
    }
    if not args.run:
        report["status"] = "staged"
    else:
        env = os.environ.copy()
        env.update({
            "NESTED_PROTECTED": str(protected),
            "NESTED_PROTECTED_FILE": str(protected / "data"),
            "NESTED_ALLOWED_FILE": str(allowed / "data"),
            "NESTED_PAYLOAD": str(payload),
        })
        runs = {}
        for mode in ("control", "nested"):
            run_env = dict(env)
            run_env["NESTED_MODE"] = mode
            proc = subprocess.run([str(executable)], env=run_env, capture_output=True, text=True, timeout=15)
            observations = []
            nested_init = None
            nested_error = None
            for line in proc.stdout.splitlines():
                fields = line.split(" ", 2)
                if fields[0] == "RESULT" and len(fields) >= 3:
                    parts = line.split()
                    observations.append({"name": parts[1], "success": parts[2] == "1", "errno": int(parts[3])})
                elif fields[0] == "NESTED_INIT":
                    nested_init = int(fields[1])
                elif fields[0] == "NESTED_ERROR":
                    nested_error = fields[1] if len(fields) > 1 else "(none)"
            runs[mode] = {
                "exit": proc.returncode,
                "nested_init_rc": nested_init,
                "nested_init_error": nested_error,
                "results": observations,
            }
        report["runs"] = runs
        control = {item["name"]: item for item in runs["control"]["results"]}
        nested = {item["name"]: item for item in runs["nested"]["results"]}
        expected = {
            "protected_open", "protected_stat", "protected_lstat", "allowed_open",
            "allowed_write", "allowed_payload_dlopen", "allowed_payload_call",
        }
        complete = (
            set(control) == expected and set(nested) == expected
            and all(run["exit"] == 0 for run in runs.values())
        )
        report["assessment"] = {
            "nested_init_accepted": runs["nested"]["nested_init_rc"] == 0,
            "nested_init_error": runs["nested"]["nested_init_error"],
            "metadata_denied_in_control": not control.get("protected_stat", {}).get("success", True),
            "metadata_denied_after_nested": (
                "protected_stat" in nested and not nested["protected_stat"]["success"]
                and nested["protected_stat"]["errno"] in (1, 13)
            ),
            "allowed_exception_preserved": (
                control.get("allowed_open", {}).get("success") is True
                and nested.get("allowed_open", {}).get("success") is True
                and nested.get("allowed_write", {}).get("success") is True
            ),
            "payload_dlopen_preserved": (
                nested.get("allowed_payload_dlopen", {}).get("success") is True
                and nested.get("allowed_payload_call", {}).get("success") is True
            ),
        }
        report["status"] = "measured" if complete else "incomplete"
    artifact = root / "nested-sandbox-result.json"
    artifact.write_text(json.dumps(report, indent=2) + "\n")
    artifact.chmod(0o600)
    print(artifact)
    return 0 if report["status"] in ("staged", "measured") else 2


if __name__ == "__main__":
    raise SystemExit(main())

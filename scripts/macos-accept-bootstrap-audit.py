#!/usr/bin/env python3
"""Run a prepared guarded acceptance copy as the ordinary user. Never sudo.

The plan is written by `macos-stage-bootstrap-audit.py --acceptance`. This
runner validates it, launches the staged copy (directly or through Launch
Services), waits for the readiness/plugin evidence, stops only the launched
process after re-verifying its start identity, and compares the pristine
inputs and the real account state before/after.

This is a development fixture, not the M1b installer: it never installs,
never requests root and never touches /Applications. Only os.lstat metadata
of the real account paths is read; their contents are never opened.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import re
import signal
import subprocess
import time

APPLICATIONS_STUB = Path("/Applications/Parsec.app/Contents/MacOS/parsecd")
POLL_SECONDS = 2.0
TERM_GRACE_SECONDS = 10.0
OPEN_DISCOVERY_SECONDS = 30.0

HELPER = r'''
#include <CoreGraphics/CoreGraphics.h>
#include <libproc.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/proc_info.h>
#include <unistd.h>

static int identity(int pid) {
    struct proc_bsdinfo info;
    char path[PROC_PIDPATHINFO_MAXSIZE];
    if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof info) != sizeof info) return 2;
    if (proc_pidpath(pid, path, sizeof path) <= 0) return 3;
    printf("%llu %llu\n%s\n", (unsigned long long)info.pbi_start_tvsec,
        (unsigned long long)info.pbi_start_tvusec, path);
    return 0;
}

static int windows(int pid) {
    CFArrayRef list = CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly, kCGNullWindowID);
    if (list == NULL) return 4;
    for (CFIndex index = 0; index < CFArrayGetCount(list); index++) {
        CFDictionaryRef window = CFArrayGetValueAtIndex(list, index);
        CFNumberRef owner = CFDictionaryGetValue(window, kCGWindowOwnerPID);
        int owner_pid = 0;
        if (owner == NULL || !CFNumberGetValue(owner, kCFNumberIntType, &owner_pid)) continue;
        if (owner_pid != pid) continue;
        CFNumberRef number = CFDictionaryGetValue(window, kCGWindowNumber);
        CFStringRef owner_name = CFDictionaryGetValue(window, kCGWindowOwnerName);
        CFStringRef name = CFDictionaryGetValue(window, kCGWindowName);
        int window_number = 0;
        char owner_text[256] = "";
        char name_text[256] = "";
        if (number != NULL) CFNumberGetValue(number, kCFNumberIntType, &window_number);
        if (owner_name != NULL) CFStringGetCString(owner_name, owner_text, sizeof owner_text, kCFStringEncodingUTF8);
        if (name != NULL) CFStringGetCString(name, name_text, sizeof name_text, kCFStringEncodingUTF8);
        printf("%d\t%s\t%s\n", window_number, owner_text, name_text);
    }
    CFRelease(list);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 3) return 1;
    if (strcmp(argv[1], "identity") == 0) return identity(atoi(argv[2]));
    if (strcmp(argv[1], "windows") == 0) return windows(atoi(argv[2]));
    return 1;
}
'''


class PlanError(Exception):
    pass


def sha256_file(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def lstat_record(path):
    try:
        status = os.lstat(path)
    except FileNotFoundError:
        return {"exists": False}
    except OSError as error:
        return {"exists": None, "error": str(error)}
    return {"exists": True, "mode": oct(status.st_mode), "size": status.st_size,
        "mtime_ns": status.st_mtime_ns, "ino": status.st_ino}


def real_state_records():
    home = Path(os.path.expanduser("~"))
    paths = [home / ".parsec", home / ".parsec-persistent",
        Path("/Users/Shared/.parsec"), Path("/Users/Shared/.parsec-persistent")]
    return {str(path): lstat_record(path) for path in paths}


def state_changed(before, after):
    return sorted(key for key in before if before[key] != after.get(key))


def mach_o_arm64(path):
    with Path(path).open("rb") as stream:
        header = stream.read(8)
    if len(header) < 8:
        return False
    magic, cpu = int.from_bytes(header[:4], "little"), int.from_bytes(header[4:8], "little")
    return magic == 0xFEEDFACF and cpu == 0x100000C


def inside(path, root):
    try:
        return Path(path).resolve().is_relative_to(root)
    except OSError:
        return False


def validate_plan(plan_path, euid=None):
    euid = os.geteuid() if euid is None else euid
    if euid == 0:
        raise PlanError("run this controller as the ordinary user, never as root")
    plan_path = Path(plan_path).resolve(strict=True)
    plan = json.loads(plan_path.read_text())
    if plan.get("schema_version") != 1 or plan.get("mode") != "acceptance":
        raise PlanError("only a schema-1 guarded acceptance plan is accepted")
    if plan.get("client_execution_allowed") is not True:
        raise PlanError("the plan does not allow client execution")
    scratch = Path(plan["scratch"]).resolve(strict=True)
    status = os.lstat(scratch)
    if not os.path.isdir(scratch) or status.st_uid != euid or status.st_mode & 0o077:
        raise PlanError("scratch must be a current-user-owned 0700 directory")
    if not plan_path.is_relative_to(scratch):
        raise PlanError("the plan must live inside its own scratch directory")
    executable = Path(plan["executable"]).resolve(strict=True)
    bundle = Path(plan["bundle"]).resolve(strict=True)
    loader = (bundle / "Contents/Frameworks/libparsec_orbit_loader.dylib").resolve(strict=True)
    orbit_root = Path(plan["orbit_root"]).resolve(strict=True)
    for path in (executable, bundle, loader, orbit_root):
        if not inside(path, scratch):
            raise PlanError(f"{path} escapes the scratch directory")
    orbit_status = os.lstat(orbit_root)
    if not os.path.isdir(orbit_root) or orbit_status.st_uid != euid or orbit_status.st_mode & 0o077:
        raise PlanError("orbit_root must be a current-user-owned 0700 directory")
    if not mach_o_arm64(executable):
        raise PlanError("the staged executable is not an arm64 Mach-O image")
    plugin = plan.get("plugin") or {}
    staged_plugin = Path(plugin.get("staged", "")).resolve(strict=True)
    if plugin.get("id") != "com.example.mac-e2e" or not inside(staged_plugin, scratch):
        raise PlanError("the plan does not stage the owned com.example.mac-e2e plugin")
    skel = bundle / "Contents/Resources/skel"
    manifest = json.loads((skel / "appdata.json").read_text())
    name = manifest["so_name"]
    if Path(name).name != name or manifest.get("entry_symbol") != "wx_main":
        raise PlanError("the staged skel manifest is invalid")
    digests = {
        "staged executable": (executable, plan.get("staged_executable_sha256")),
        "staged loader": (loader, plan.get("staged_loader_sha256")),
        "staged plugin": (staged_plugin, plugin.get("staged_sha256")),
        "staged skel client": (skel / name, plan.get("seed_sha256")),
        "staged skel manifest": (skel / "appdata.json", plan.get("seed_manifest_sha256")),
    }
    for what, (path, expected) in digests.items():
        if not isinstance(expected, str) or sha256_file(path) != expected:
            raise PlanError(f"{what} does not match the prepared digest")
    applications = plan.get("applications_stub_sha256")
    if isinstance(applications, str):
        if not APPLICATIONS_STUB.is_file() or sha256_file(APPLICATIONS_STUB) != applications:
            raise PlanError("/Applications/Parsec.app stub changed since preparation")
    source_bundle = plan.get("source_bundle")
    if isinstance(source_bundle, str):
        source = Path(source_bundle)
        if sha256_file(source / "Contents/MacOS/parsecd") != plan.get("source_stub_sha256"):
            raise PlanError("the pristine source stub changed since preparation")
    preseed = plan.get("preseed") or {}
    if preseed.get("seeded"):
        state = orbit_root / "home/.parsec"
        state_status = os.lstat(state)
        if state_status.st_mode & 0o077:
            raise PlanError("the preseeded isolated state is not private")
        if sha256_file(state / name) != plan.get("seed_sha256"):
            raise PlanError("the preseeded client does not match the verified skel")
        if sha256_file(state / "appdata.json") != plan.get("seed_manifest_sha256"):
            raise PlanError("the preseeded manifest does not match the verified skel")
    return plan, {"scratch": scratch, "executable": executable, "bundle": bundle,
        "loader": loader, "orbit_root": orbit_root, "plugin": staged_plugin,
        "seed_name": name, "manifest": manifest}


def compile_helper(run_dir):
    source = run_dir / "accept-helper.c"
    source.write_text(HELPER)
    binary = run_dir / "accept-helper"
    result = subprocess.run(["clang", "-Wall", "-Werror", str(source), "-o", str(binary),
        "-framework", "CoreGraphics", "-framework", "CoreFoundation"],
        capture_output=True, text=True)
    if result.returncode:
        return None, result.stderr.strip()
    return binary, None


def read_identity(helper, pid):
    result = subprocess.run([str(helper), "identity", str(pid)], capture_output=True, text=True)
    lines = result.stdout.splitlines()
    if result.returncode or len(lines) != 2:
        return None
    seconds, _, microseconds = lines[0].partition(" ")
    if not seconds.isdigit() or not microseconds.isdigit():
        return None
    return {"sec": int(seconds), "usec": int(microseconds), "path": lines[1]}


def read_windows(helper, pid):
    result = subprocess.run([str(helper), "windows", str(pid)], capture_output=True, text=True)
    if result.returncode:
        return None, (result.stderr.strip() or f"helper exit {result.returncode}")
    windows = []
    for line in result.stdout.splitlines():
        number, _, rest = line.partition("\t")
        owner, _, name = rest.partition("\t")
        windows.append({"number": number, "owner": owner, "name": name})
    return windows, None


def process_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def stop_owned(helper, pid, expected, process=None):
    """Signals only the owned PID, and only while its start identity holds."""
    def gone():
        # A reaped child is the strongest form of "gone"; this also avoids
        # reading proc identity from a zombie, whose start fields are zero.
        if process is not None and process.poll() is not None:
            return True
        return not process_alive(pid)

    if gone():
        return {"stopped": "exited-before-signal"}
    current = read_identity(helper, pid) if helper else None
    if helper and current is None:
        return {"stopped": "refused", "reason": "identity unreadable before SIGTERM"}
    if helper and (current["sec"], current["usec"]) != (expected["sec"], expected["usec"]):
        return {"stopped": "refused", "reason": "start identity changed before SIGTERM"}
    os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + TERM_GRACE_SECONDS
    while time.monotonic() < deadline:
        if gone():
            return {"stopped": "SIGTERM"}
        time.sleep(0.25)
    if gone():
        return {"stopped": "SIGTERM"}
    current = read_identity(helper, pid) if helper else None
    if helper and (current is None
            or (current["sec"], current["usec"]) != (expected["sec"], expected["usec"])):
        return {"stopped": "refused", "reason": "start identity changed before SIGKILL"}
    os.kill(pid, signal.SIGKILL)
    deadline = time.monotonic() + TERM_GRACE_SECONDS
    while time.monotonic() < deadline:
        if gone():
            return {"stopped": "SIGKILL"}
        time.sleep(0.25)
    return {"stopped": "still-alive"}


def launch_direct(ctx, run_dir, attempt):
    stdout = (run_dir / f"attempt-{attempt}.stdout.log").open("w")
    stderr = (run_dir / f"attempt-{attempt}.stderr.log").open("w")
    return subprocess.Popen([str(ctx["executable"])], cwd=str(ctx["scratch"]),
        stdout=stdout, stderr=stderr)


def discover_open_pid(ctx, deadline):
    while time.monotonic() < deadline:
        result = subprocess.run(["ps", "-axo", "pid=,command="], capture_output=True, text=True)
        matches = []
        for line in result.stdout.splitlines():
            pid_text, _, command = line.strip().partition(" ")
            if not pid_text.isdigit():
                continue
            token = command.strip().split(" ", 1)[0]
            if token and os.path.realpath(token) == str(ctx["executable"]):
                matches.append(int(pid_text))
        if matches:
            return max(matches), len(matches)
        time.sleep(0.5)
    return None, 0


def launch_open(ctx):
    subprocess.run(["open", "-n", str(ctx["bundle"])], check=True, capture_output=True)
    pid, matches = discover_open_pid(ctx, time.monotonic() + OPEN_DISCOVERY_SECONDS)
    if pid is None:
        raise PlanError("launch services did not start the staged executable in time")
    return pid, matches


def read_lines(path):
    try:
        return Path(path).read_text(errors="replace").splitlines()
    except OSError:
        return []


def collect_evidence(ctx):
    state = ctx["orbit_root"] / "home/.parsec"
    clients = []
    if state.is_dir():
        try:
            clients = sorted(entry.name for entry in state.iterdir()
                if entry.name.startswith("parsecd-") and entry.name.endswith(".dylib"))
        except OSError:
            clients = []
    return {"vendor_log": str(state / "log.txt"),
        "loader_log": str(ctx["orbit_root"] / "parsec-orbit.log"),
        "vendor_lines": read_lines(state / "log.txt"),
        "loader_lines": read_lines(ctx["orbit_root"] / "parsec-orbit.log"),
        "state_appdata": (state / "appdata.json").is_file(), "state_clients": clients}


def evaluate_evidence(evidence):
    loader = evidence["loader_lines"]
    vendor = evidence["vendor_lines"]
    release = [line for line in vendor if "Parsec release (" in line]
    detections = [line for line in loader if "detected" in line and "client " in line]
    fixtures = [line for line in loader if "fixture[" in line]
    readies = []
    for line in loader:
        match = re.search(r"ready:\s*(\d+) loaded,\s*(\d+) skipped,\s*(\d+) failed", line)
        if match:
            readies.append((line, [int(value) for value in match.groups()]))
    builds = sorted({match for line in release for match in re.findall(r"150-([0-9a-z]+)", line)})
    checks = {
        "vendor_release_announced": bool(release),
        "vendor_loader_token_14": any("Loader: 14" in line for line in release),
        "vendor_loader_token_uniform": bool(release) and all("Loader: 14" in line for line in release),
        "client_detected": any(re.search(r"client \d+-[0-9a-z]+ detected", line) for line in detections),
        "fixture_register": any("register com.example.mac-e2e -> 0" in line for line in fixtures),
        "fixture_client_query": any("query parsec-orbit.client -> 1.0" in line for line in fixtures),
        "ready_zero_failed": any(values[2] == 0 for _, values in readies),
        "state_appdata": evidence["state_appdata"],
        "state_client_image": bool(evidence["state_clients"]),
    }
    return checks, {"release_lines": release, "detections": detections,
        "fixture_lines": fixtures, "ready_lines": [line for line, _ in readies],
        "builds": builds, "second_detection": len(detections) > 1}


def failed_checks(checks):
    return [name for name, value in checks.items() if not value]


def payload_hashes(ctx, plan):
    hashes = {"staged_executable": sha256_file(ctx["executable"]),
        "staged_loader": sha256_file(ctx["loader"]),
        "staged_plugin": sha256_file(ctx["plugin"]),
        "skel_client": sha256_file(ctx["bundle"] / "Contents/Resources/skel" / ctx["seed_name"]),
        "skel_manifest": sha256_file(ctx["bundle"] / "Contents/Resources/skel/appdata.json")}
    if APPLICATIONS_STUB.is_file():
        hashes["applications_stub"] = sha256_file(APPLICATIONS_STUB)
    source_bundle = plan.get("source_bundle")
    if isinstance(source_bundle, str):
        stub = Path(source_bundle) / "Contents/MacOS/parsecd"
        if stub.is_file():
            hashes["source_stub"] = sha256_file(stub)
    return hashes


def expected_hashes(plan):
    expected = {"staged_executable": plan.get("staged_executable_sha256"),
        "staged_loader": plan.get("staged_loader_sha256"),
        "staged_plugin": (plan.get("plugin") or {}).get("staged_sha256"),
        "skel_client": plan.get("seed_sha256"),
        "skel_manifest": plan.get("seed_manifest_sha256"),
        "applications_stub": plan.get("applications_stub_sha256"),
        "source_stub": plan.get("source_stub_sha256")}
    return {name: value for name, value in expected.items() if isinstance(value, str)}


def run_acceptance(plan, ctx, args):
    run_root = ctx["scratch"] / f"acceptance-{random.getrandbits(32):08x}"
    run_root.mkdir(mode=0o700)
    helper, helper_error = compile_helper(run_root)
    before_state = real_state_records()
    before_hashes = payload_hashes(ctx, plan)
    deadline = time.monotonic() + args.timeout
    attempts = []
    evidence = collect_evidence(ctx)
    detail = {}
    checks = {}
    windows = []
    windows_error = None
    complete = False
    launch_index = 0
    while True:
        launch_index += 1
        attempt = {"attempt": launch_index, "launch": args.launch}
        process = None
        pid = None
        identity = None
        try:
            if args.launch == "direct":
                process = launch_direct(ctx, run_root, launch_index)
                pid = process.pid
                attempt["discovery"] = "direct"
            else:
                pid, matches = launch_open(ctx)
                attempt["discovery"] = "open"
                attempt["path_matches"] = matches
            attempt["pid"] = pid
            identity = read_identity(helper, pid) if helper else None
            attempt["identity"] = identity
            first_complete = None
            while process is None or process.poll() is None:
                if time.monotonic() >= deadline:
                    break
                evidence = collect_evidence(ctx)
                checks, detail = evaluate_evidence(evidence)
                if failed_checks(checks):
                    first_complete = None
                elif first_complete is None:
                    first_complete = time.monotonic()
                elif time.monotonic() - first_complete >= args.settle:
                    complete = True
                    break
                time.sleep(POLL_SECONDS)
            if helper is not None:
                windows_result, windows_error = read_windows(helper, pid)
                if windows_result is not None:
                    windows = windows_result
            if process is not None and process.poll() is None:
                attempt["stop"] = stop_owned(helper, pid, identity or {}, process)
                process.wait(timeout=TERM_GRACE_SECONDS)
                attempt["exit"] = {"kind": "terminated", "code": process.returncode}
            elif process is not None:
                attempt["exit"] = {"kind": "exited", "code": process.returncode}
            else:
                attempt["stop"] = stop_owned(helper, pid, identity or {})
                attempt["exit"] = {"kind": "handled"}
        except Exception as error:
            attempt["error"] = str(error)
            if process is not None and process.poll() is None:
                attempt["stop"] = stop_owned(helper, process.pid, identity or {}, process)
        attempts.append(attempt)
        evidence = collect_evidence(ctx)
        checks, detail = evaluate_evidence(evidence)
        if not failed_checks(checks) or launch_index >= args.max_launches:
            break
        if time.monotonic() >= deadline:
            break
        time.sleep(2.0)
    after_state = real_state_records()
    after_hashes = payload_hashes(ctx, plan)
    changed_state = state_changed(before_state, after_state)
    expected = expected_hashes(plan)
    changed_hashes = sorted(name for name in before_hashes
        if after_hashes.get(name) != before_hashes[name])
    mismatched_hashes = sorted(name for name in expected
        if name in before_hashes and before_hashes[name] != expected[name])
    measured_missing = sorted(name for name in expected if name not in before_hashes)
    if helper is None:
        unmeasured = [f"process identity helper unavailable: {helper_error}",
            "window observation unavailable"]
    elif windows_error:
        unmeasured = [f"window observation unavailable: {windows_error}"]
    else:
        unmeasured = []
    failures = [f"check {name}" for name in failed_checks(checks)]
    if changed_state:
        failures.append("real account state changed: " + ", ".join(changed_state))
    if changed_hashes:
        failures.append("staged payload changed: " + ", ".join(changed_hashes))
    if mismatched_hashes:
        failures.append("prepared digest mismatch: " + ", ".join(mismatched_hashes))
    unmeasured.extend(f"digest not measured: {name}" for name in measured_missing)
    result = {"schema_version": 1, "mode": "acceptance", "launch": args.launch,
        "scratch": str(ctx["scratch"]), "run": str(run_root),
        "helper": str(helper) if helper else None, "attempts": attempts,
        "windows": windows, "complete": complete,
        "evidence": {"vendor_log": evidence["vendor_log"],
            "loader_log": evidence["loader_log"],
            "release_lines": detail.get("release_lines", []),
            "detections": detail.get("detections", []),
            "fixture_lines": detail.get("fixture_lines", []),
            "ready_lines": detail.get("ready_lines", []),
            "builds": detail.get("builds", []),
            "second_detection": detail.get("second_detection", False),
            "state_clients": evidence["state_clients"],
            "state_appdata": evidence["state_appdata"]},
        "checks": checks,
        "real_state": {"before": before_state, "after": after_state,
            "changed": changed_state},
        "payload_hashes": {"before": before_hashes, "after": after_hashes,
            "changed": changed_hashes},
        "checks_passed": not failures, "failures": failures, "unmeasured": unmeasured}
    report = run_root / "acceptance-result.json"
    report.write_text(json.dumps(result, indent=2) + "\n")
    print(report)
    return 0 if not failures else 2


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plan", type=Path)
    parser.add_argument("--check", action="store_true", help="validate the plan only")
    parser.add_argument("--run", action="store_true", help="run the guarded acceptance launch")
    parser.add_argument("--launch", choices=("direct", "open"), default="direct")
    parser.add_argument("--timeout", type=float, default=240.0)
    parser.add_argument("--settle", type=float, default=5.0)
    parser.add_argument("--max-launches", type=int, default=3)
    args = parser.parse_args()
    if args.check and args.run:
        parser.error("choose either --check or --run")
    plan, ctx = validate_plan(args.plan)
    print(f"plan ok: mode={plan['mode']} launch={args.launch} "
        f"bundle={ctx['bundle']} orbit_root={ctx['orbit_root']}")
    if not args.run:
        return 0
    return run_acceptance(plan, ctx, args)


if __name__ == "__main__":
    raise SystemExit(main())

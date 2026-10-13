#!/usr/bin/env python3
"""Development-only post-exec PID audit of the prepared exit-78 copy.

posix_spawn starts the actual target suspended before user code. fs_usage
selects that process object rather than the helper's pre-exec object. This
avoids assuming a PID filter survives exec. Only fs_usage may run as root.
"""
import argparse
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

spec = importlib.util.spec_from_file_location("audit", Path(__file__).with_name("macos-run-bootstrap-audit.py"))
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)

BRIDGE = r'''
#include <spawn.h>
#include <fcntl.h>
#include <libproc.h>
#include <sys/proc_info.h>
#include <sys/proc.h>
#include <stdint.h>
#include <unistd.h>
extern char **environ;
int spawn_target(const char *path, int out, int err, int *pid) {
    posix_spawnattr_t attr; posix_spawn_file_actions_t actions;
    int rc = posix_spawnattr_init(&attr);
    if (rc) return rc;
    rc = posix_spawn_file_actions_init(&actions);
    if (rc) { posix_spawnattr_destroy(&attr); return rc; }
    rc = posix_spawnattr_setflags(&attr, POSIX_SPAWN_START_SUSPENDED);
    if (!rc) rc = posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0);
    if (!rc) rc = posix_spawn_file_actions_adddup2(&actions, out, 1);
    if (!rc) rc = posix_spawn_file_actions_adddup2(&actions, err, 2);
    if (!rc) rc = posix_spawn_file_actions_addclose(&actions, out);
    if (!rc) rc = posix_spawn_file_actions_addclose(&actions, err);
    char *argv[] = {(char *)path, NULL};
    if (!rc) rc = posix_spawn(pid, path, &actions, &attr, argv, environ);
    posix_spawn_file_actions_destroy(&actions);
    posix_spawnattr_destroy(&attr);
    return rc;
}
int target_identity(int pid, uint64_t *sec, uint64_t *usec, char *path, int size) {
    struct proc_bsdinfo info;
    if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof info) != sizeof info) return -1;
    if (proc_pidpath(pid, path, size) <= 0) return -2;
    *sec = info.pbi_start_tvsec; *usec = info.pbi_start_tvusec;
    return 0;
}
int target_stopped(int pid) {
    struct proc_bsdinfo info;
    if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof info) != sizeof info) return -1;
    return info.pbi_status == SSTOP;
}
'''


class SuspendedTarget:
    def __init__(self, bridge, executable, out, err):
        self.bridge = bridge
        self.executable = executable.resolve(strict=True)
        self.status = None
        pid = ctypes.c_int()
        rc = bridge.spawn_target(os.fsencode(self.executable), out, err, ctypes.byref(pid))
        if rc:
            raise OSError(rc, os.strerror(rc))
        self.pid = pid.value
        # An unreaped direct child cannot have its PID reused. Even if identity
        # binding fails, the controller owns this exact child for cleanup.
        try:
            self.start, path = self.identity()
            if Path(path).resolve(strict=True) != self.executable:
                raise ValueError("suspended target has an unexpected executable")
            if bridge.target_stopped(self.pid) != 1:
                raise ValueError("target was not stopped before user code")
        except BaseException:
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
            raise

    def identity(self):
        sec, usec = ctypes.c_uint64(), ctypes.c_uint64()
        path = ctypes.create_string_buffer(4096)
        if self.bridge.target_identity(self.pid, ctypes.byref(sec), ctypes.byref(usec), path, len(path)):
            raise ValueError("cannot read owned target identity")
        return f"{sec.value} {usec.value}", os.fsdecode(path.value)

    def poll(self):
        if self.status is None:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                self.status = os.waitstatus_to_exitcode(status)
        return self.status

    def checked_signal(self, sig):
        if self.poll() is not None:
            return
        start, path = self.identity()
        if start != self.start or Path(path).resolve(strict=True) != self.executable:
            raise ValueError("owned target identity changed; refusing signal")
        os.kill(self.pid, sig)

    def wait(self, timeout):
        deadline = time.monotonic() + timeout
        while self.poll() is None:
            if time.monotonic() >= deadline:
                raise TimeoutError("owned target did not exit")
            time.sleep(0.01)
        return self.status

    def stop(self):
        self.checked_signal(signal.SIGKILL)
        return self.wait(5)


def build_bridge(root):
    source, library = root / "spawn-bridge.c", root / "spawn-bridge.dylib"
    source.write_text(BRIDGE)
    subprocess.run(["clang", "-Wall", "-Werror", "-dynamiclib", str(source), "-o", str(library)], check=True)
    subprocess.run(["codesign", "--force", "--sign", "-", str(library)], check=True, capture_output=True)
    bridge = ctypes.CDLL(str(library))
    bridge.spawn_target.argtypes = [ctypes.c_char_p, ctypes.c_int, ctypes.c_int, ctypes.POINTER(ctypes.c_int)]
    bridge.spawn_target.restype = ctypes.c_int
    bridge.target_identity.argtypes = [ctypes.c_int, ctypes.POINTER(ctypes.c_uint64),
        ctypes.POINTER(ctypes.c_uint64), ctypes.c_char_p, ctypes.c_int]
    bridge.target_identity.restype = ctypes.c_int
    bridge.target_stopped.argtypes = [ctypes.c_int]
    bridge.target_stopped.restype = ctypes.c_int
    return bridge


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plan", type=Path)
    parser.add_argument("--trace", action="store_true")
    args = parser.parse_args()
    plan, scratch, executable = audit.validated_plan(args.plan)
    root = Path(tempfile.mkdtemp(prefix="suspended-run-", dir=scratch))
    os.chmod(root, 0o700)
    bridge = build_bridge(root)
    report = {"scope": plan["scope"], "launch_mode": "post-exec-start-suspended",
        "root_trace_requested": args.trace, "exec_completed": False,
        "user_code_released": False, "client_execution_allowed": False}
    target = tracer = None
    try:
        with (root / "stdout.log").open("w") as out, (root / "stderr.log").open("w") as err, \
                (root / "fs-usage.log").open("w") as trace:
            target = SuspendedTarget(bridge, executable, out.fileno(), err.fileno())
            report.update(pid=target.pid, start_identity=target.start, executable=str(executable), exec_completed=True)
            if args.trace:
                tracer = subprocess.Popen(["sudo", "-n", "/usr/bin/fs_usage", "-w", "-f", "filesys",
                    "-t", "5", str(target.pid)], stdout=trace, stderr=trace)
                # There is no same-PID pre-exec probe in this mode: the actual
                # executable is already installed and stopped before user code.
                # Warm-up is not a kernel-ready guarantee. Require the actual
                # record event afterward and never call this complete isolation.
                time.sleep(1)
                if tracer.poll() is not None:
                    raise ValueError("PID tracer failed; user code remains suspended")
            target.checked_signal(signal.SIGCONT)
            report["user_code_released"] = True
            report["exit"] = target.wait(10)
            if tracer:
                tracer.wait(timeout=10)
            if report["exit"] != 78 or "isolation backend is not validated" not in (root / "stderr.log").read_text():
                raise ValueError("pre-main gate differed from the prepared contract")
            report["status"] = "gate-observed"
            if tracer:
                evidence = (root / "fs-usage.log").read_text()
                result = audit.trace_assessment(evidence, "unused-pre-exec-probe", tracer.returncode)
                result["pre_exec_probe_observed"] = None
                result["status"] = "startup-paths-observed" if result["installation_record_observed"] \
                    and tracer.returncode == 0 and not result["event_loss_reported"] else "incomplete"
                report["file_trace"] = result
    except Exception as error:
        report.update(status="failed", error=str(error))
    finally:
        if target:
            target.stop()
        if tracer and tracer.poll() is None:
            tracer.wait(timeout=10)
        (root / "audit-result.json").write_text(json.dumps(report, indent=2) + "\n")
        print(root / "audit-result.json")
    return 0 if report.get("status") == "gate-observed" and \
        report.get("file_trace", {}).get("status") != "incomplete" else 2


if __name__ == "__main__":
    raise SystemExit(main())

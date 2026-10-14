#!/usr/bin/env python3
"""Run only a prepared fail-closed copy; optional PID-only root file trace.

The owned child pauses before exec so fs_usage can attach to its PID first.
No process-name lookup/signals, installation mutation, account migration or
privileged vendor execution. --trace explicitly requires sudo authorization.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import time

HELPER = r'''
#include <libproc.h>
#include <fcntl.h>
#include <sys/proc_info.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
static int identity(int pid) {
    struct proc_bsdinfo info; char path[PROC_PIDPATHINFO_MAXSIZE];
    if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof info) != sizeof info) return 2;
    if (proc_pidpath(pid, path, sizeof path) <= 0) return 3;
    printf("%llu %llu\n%s\n", info.pbi_start_tvsec, info.pbi_start_tvusec, path);
    fflush(stdout); return 0;
}
int main(int argc, char **argv) {
    if (argc == 3 && argv[1][0] == 'i') return identity(atoi(argv[2]));
    if (argc != 3) return 4;
    if (identity(getpid())) return 5;
    char gate = 0;
    while (read(0, &gate, 1) == 1) {
        if (gate == 'G') break;
        if (gate != 'P') return 6;
        int fd = open(argv[2], O_RDONLY | O_NOFOLLOW);
        char byte;
        if (fd < 0) return 8;
        int result = read(fd, &byte, 1);
        close(fd);
        if (result != 1) return 9;
    }
    if (gate != 'G') return 6;
    char *args[] = {argv[1], 0};
    execv(argv[1], args); return 7;
}
'''


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def path_event(trace, basename):
    # A filename mentioned in a diagnostic is not a captured syscall.
    return any(re.match(r"^\d\d:\d\d:\d\d\.\d+\s+(?:open\S*|stat\S*|lstat\S*|access\S*|getattrlist\S*)\s", line)
        and basename in line for line in trace.splitlines())


def trace_assessment(trace, probe_name, returncode):
    probe = path_event(trace, probe_name)
    record = path_event(trace, "orbit-install.json")
    loss = any(word in trace.lower() for word in ("buffer overrun", "dropped events", "ktrace_start:"))
    usable = returncode == 0 and probe and record and not loss
    return {"status": "startup-paths-observed" if usable else "incomplete",
        "tracer_exit": returncode, "pre_exec_probe_observed": probe,
        "installation_record_observed": record, "event_loss_reported": loss,
        "complete_isolation_proven": False}


def validated_plan(plan_path):
    if os.geteuid() == 0:
        raise ValueError("run this controller as the ordinary user; only fs_usage may run as root")
    plan = json.loads(plan_path.read_text())
    if plan.get("client_execution_allowed") is not False or plan.get("expected_exit") != 78:
        raise ValueError("only fail-closed pre-main audit plans are accepted")
    scratch = Path(plan["scratch"]).resolve(strict=True)
    if scratch != plan_path.resolve(strict=True).parent or scratch.stat().st_uid != os.getuid():
        raise ValueError("plan must be inside the run-owned scratch directory")
    executable = Path(plan["executable"]).resolve(strict=True)
    loader = Path(plan["bundle"]) / "Contents/Frameworks/libparsec_orbit_loader.dylib"
    if not executable.is_relative_to(scratch) or not loader.resolve(strict=True).is_relative_to(scratch):
        raise ValueError("payload escapes scratch")
    if sha256(executable) != plan["staged_executable_sha256"] or sha256(loader) != plan["staged_loader_sha256"]:
        raise ValueError("staged payload changed since preparation")
    return plan, scratch, executable


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plan", type=Path)
    parser.add_argument("--trace", action="store_true")
    args = parser.parse_args()
    plan, scratch, executable = validated_plan(args.plan)
    run_root = Path(tempfile.mkdtemp(prefix="run-", dir=scratch))
    os.chmod(run_root, 0o700)
    source = run_root / "audit-exec-gate.c"
    source.write_text(HELPER)
    probe = run_root / ("trace-probe-" + run_root.name + ".txt")
    probe.write_text("P")
    os.chmod(probe, 0o600)
    helper = run_root / "audit-exec-gate"
    subprocess.run(["clang", "-Wall", "-Werror", str(source), "-o", str(helper)], check=True)
    subprocess.run(["codesign", "--force", "--sign", "-", str(helper)], check=True, capture_output=True)
    with (run_root / "stderr.log").open("w") as errors:
        child = subprocess.Popen([str(helper), str(executable), str(probe)], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=errors, text=True)
        tracer = None
        trace_file = None
        exec_released = False
        start = child.stdout.readline().strip()
        initial_path = child.stdout.readline().strip()
        if not start or Path(initial_path).resolve() != helper:
            child.stdin.close()
            child.wait(timeout=5)
            raise ValueError("failed to bind owned child identity before exec")

        def owned_signal():
            if child.poll() is not None:
                return
            current = subprocess.run([str(helper), "identity", str(child.pid)], capture_output=True, text=True)
            lines = current.stdout.splitlines()
            if current.returncode or len(lines) != 2 or lines[0] != start:
                raise ValueError("child start identity changed; refusing signal")
            if Path(lines[1]).resolve() not in (helper, executable):
                raise ValueError("child executable identity changed; refusing signal")
            os.kill(child.pid, signal.SIGKILL)

        try:
            if args.trace:
                trace_file = (run_root / "fs-usage.log").open("w")
                tracer = subprocess.Popen(["sudo", "-n", "/usr/bin/fs_usage", "-w", "-f", "filesys",
                    "-t", "5", str(child.pid)], stdout=trace_file, stderr=trace_file)
                # Observe a known syscall from this same PID before exec.
                # No arbitrary warm-up can establish kernel capture readiness.
                deadline = time.monotonic() + 3
                while not path_event((run_root / "fs-usage.log").read_text(), probe.name):
                    if tracer.poll() is not None or time.monotonic() >= deadline:
                        raise ValueError("PID trace did not capture the pre-exec probe; vendor exec remains withheld")
                    child.stdin.write("P")
                    child.stdin.flush()
                    time.sleep(0.1)
            child.stdin.write("G")
            child.stdin.flush()
            exec_released = True
            child.stdin.close()
            child.stdin = None
            output, _ = child.communicate(timeout=10)
            code = child.returncode
            if tracer:
                tracer.wait(timeout=10)
            report = {"scope": plan["scope"], "pid": child.pid, "start_identity": start,
                "executable": str(executable), "exit": code, "stdout": output,
                "root_trace_requested": args.trace, "exec_released": exec_released,
                "client_execution_allowed": False}
            if code != 78 or "isolation backend is not validated" not in (run_root / "stderr.log").read_text():
                raise ValueError("pre-main gate result differed from the prepared contract")
            report["status"] = "gate-observed"
            if tracer:
                report["file_trace"] = trace_assessment((run_root / "fs-usage.log").read_text(),
                    probe.name, tracer.returncode)
            (run_root / "audit-result.json").write_text(json.dumps(report, indent=2) + "\n")
            print(run_root / "audit-result.json")
            if tracer and report["file_trace"]["status"] == "incomplete":
                return 2
        except Exception as error:
            (run_root / "audit-result.json").write_text(json.dumps({"status": "failed",
                "scope": plan["scope"], "pid": child.pid, "start_identity": start,
                "root_trace_requested": args.trace, "exec_released": exec_released,
                "error": str(error), "client_execution_allowed": False}, indent=2) + "\n")
            print(run_root / "audit-result.json")
            raise
        finally:
            owned_signal()
            if child.poll() is None:
                child.wait(timeout=5)
            # A bounded root trace exits by itself; do not signal unrelated
            # descendants or guess which PID sudo execed.
            if tracer and tracer.poll() is None:
                tracer.wait(timeout=10)
            if trace_file:
                trace_file.close()


if __name__ == "__main__":
    raise SystemExit(main())

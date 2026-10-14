#!/usr/bin/env python3
"""Measure text-const write protections in an owned hardened arm64 host.

Each protection attempt runs in its own disposable child. The fixture contains
no vendor code or client data, and never executes from its const table.
"""

import argparse
import json
import os
from pathlib import Path
import plistlib
import platform
import re
import signal
import subprocess
import sys
import tempfile


ENTITLEMENTS = {"com.apple.security.cs.disable-library-validation": True}
ATTEMPTS = ("mprotect", "vm_protect", "vm_protect_copy", "owned_code_copy")
MAX_TIMEOUT = 30

C_SOURCE = r'''#include <errno.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <mach/vm_map.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

static const unsigned char text_const_table[16384]
    __attribute__((section("__TEXT,__const"), used, aligned(16384))) = { 0x42 };
static volatile uint32_t data_control = 0x13579bdf;
int cold_owned_function(void);

static int protection_for_page(vm_address_t address, vm_prot_t *protection) {
    mach_vm_address_t region = address;
    mach_vm_size_t size = 0;
    vm_region_basic_info_data_64_t info;
    mach_msg_type_number_t count = VM_REGION_BASIC_INFO_COUNT_64;
    mach_port_t object = MACH_PORT_NULL;
    kern_return_t result = mach_vm_region(mach_task_self(), &region, &size,
        VM_REGION_BASIC_INFO_64, (vm_region_info_t)&info, &count, &object);
    if (object != MACH_PORT_NULL) mach_port_deallocate(mach_task_self(), object);
    if (result != KERN_SUCCESS || address < region || address >= region + size) {
        return (int)result;
    }
    *protection = info.protection;
    return KERN_SUCCESS;
}

static void emit_control(const char *name, int kern, int result, int saved_errno,
        unsigned before, unsigned after, int restore_kern) {
    printf("{\"event\":\"control\",\"name\":\"%s\",\"region_kern_return\":%d,"
        "\"result\":%d,\"errno\":%d,\"before\":%u,\"after\":%u,"
        "\"restore_kern_return\":%d}\n", name, kern, result,
        saved_errno, before, after, restore_kern);
    fflush(stdout);
}

static void run_attempt(const char *mode, volatile unsigned char *target,
        size_t page_size, int is_text) {
    vm_address_t page = (vm_address_t)target & ~((vm_address_t)page_size - 1);
    vm_prot_t old_protection = 0;
    int region_result = protection_for_page(page, &old_protection);
    unsigned before = *target;
    printf("{\"event\":\"before\",\"mode\":\"%s\",\"is_text_const\":%s,"
        "\"address\":\"0x%llx\",\"page_address\":\"0x%llx\","
        "\"region_kern_return\":%d,\"initial_protection\":%d,"
        "\"before_value\":%u}\n", mode, is_text ? "true" : "false",
        (unsigned long long)(uintptr_t)target, (unsigned long long)page,
        region_result, (int)old_protection, before);
    fflush(stdout);

    int result = -1;
    int saved_errno = 0;
    kern_return_t call_kern = KERN_SUCCESS;
    errno = 0;
    if (strcmp(mode, "mprotect") == 0) {
        result = mprotect((void *)page, page_size, PROT_READ | PROT_WRITE);
        saved_errno = errno;
    } else {
        vm_prot_t requested = VM_PROT_READ | VM_PROT_WRITE;
        if (strcmp(mode, "vm_protect_copy") == 0) requested |= VM_PROT_COPY;
        call_kern = vm_protect(mach_task_self(), page, page_size, FALSE, requested);
        result = (int)call_kern;
        saved_errno = errno;
    }
    printf("{\"event\":\"protection_call\",\"mode\":\"%s\","
        "\"result\":%d,\"errno\":%d,\"kern_return\":%d}\n",
        mode, result, saved_errno, (int)call_kern);
    fflush(stdout);

    if (result == 0) {
        *target = (unsigned char)(before ^ 1U);
        unsigned after = *target;
        kern_return_t restore_kern = KERN_SUCCESS;
        int restore_result = 0;
        int restore_errno = 0;
        errno = 0;
        if (strcmp(mode, "mprotect") == 0) {
            int native_protection = 0;
            if (old_protection & VM_PROT_READ) native_protection |= PROT_READ;
            if (old_protection & VM_PROT_WRITE) native_protection |= PROT_WRITE;
            if (old_protection & VM_PROT_EXECUTE) native_protection |= PROT_EXEC;
            restore_result = mprotect((void *)page, page_size, native_protection);
            restore_errno = errno;
        } else {
            restore_kern = vm_protect(mach_task_self(), page, page_size,
                FALSE, old_protection);
            restore_result = (int)restore_kern;
            restore_errno = errno;
        }
        vm_prot_t final_protection = 0;
        int final_region_kern = protection_for_page(page, &final_protection);
        printf("{\"event\":\"after\",\"mode\":\"%s\","
            "\"after_value\":%u,\"restore_result\":%d,\"restore_errno\":%d,"
            "\"restore_kern_return\":%d,\"protection_after_restore_kern_return\":%d,"
            "\"protection_after_restore\":%d,\"restoration_matches_initial\":%s,"
            "\"altered_bytes_executed\":false}\n", mode, after, restore_result,
            restore_errno, (int)restore_kern, final_region_kern,
            (int)final_protection, final_region_kern == KERN_SUCCESS
                && final_protection == old_protection ? "true" : "false");
        fflush(stdout);
    } else {
        vm_prot_t final_protection = 0;
        int final_region_kern = protection_for_page(page, &final_protection);
        printf("{\"event\":\"after\",\"mode\":\"%s\","
            "\"after_value\":%u,\"restore_result\":null,"
            "\"protection_after_call_kern_return\":%d,"
            "\"protection_after_call\":%d,\"restoration_matches_initial\":%s,"
            "\"altered_bytes_executed\":false}\n", mode, *target,
            final_region_kern, (int)final_protection,
            final_region_kern == KERN_SUCCESS && final_protection == old_protection
                ? "true" : "false");
        fflush(stdout);
    }
}

__attribute__((noinline)) static int run_owned_code_copy(void) {
    size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    uintptr_t function_address = (uintptr_t)cold_owned_function;
    uintptr_t runner_address = (uintptr_t)run_owned_code_copy;
    vm_address_t page = function_address & ~((vm_address_t)page_size - 1);
    vm_address_t runner_page = runner_address & ~((vm_address_t)page_size - 1);
    volatile uint32_t *instructions = (volatile uint32_t *)function_address;
    uint32_t before_instruction = instructions[0];
    uint32_t return_instruction = instructions[1];
    vm_prot_t old_protection = 0;
    int region_result = protection_for_page(page, &old_protection);
    int shape_ok = function_address == page
        && runner_page != page
        && before_instruction == 0x528000e0U
        && return_instruction == 0xd65f03c0U
        && old_protection == (VM_PROT_READ | VM_PROT_EXECUTE);
    printf("{\"event\":\"code_stage\",\"stage\":\"preflight\","
        "\"function_address\":\"0x%llx\",\"page_address\":\"0x%llx\","
        "\"page_size\":%llu,\"runner_address\":\"0x%llx\","
        "\"runner_page_address\":\"0x%llx\",\"runner_page_is_target_page\":%s,"
        "\"region_kern_return\":%d,"
        "\"initial_protection\":%d,\"instruction_before\":[\"0x%08x\","
        "\"0x%08x\"],\"shape_valid\":%s,\"function_called_before_write\":false}\n",
        (unsigned long long)function_address, (unsigned long long)page,
        (unsigned long long)page_size, (unsigned long long)runner_address,
        (unsigned long long)runner_page, runner_page == page ? "true" : "false",
        region_result, (int)old_protection,
        before_instruction, return_instruction, shape_ok ? "true" : "false");
    fflush(stdout);
    if (!shape_ok) return 30;

    printf("{\"event\":\"code_stage\",\"stage\":\"before_vm_protect_copy\"}\n");
    fflush(stdout);
    errno = 0;
    kern_return_t protect_result = vm_protect(mach_task_self(), page,
        page_size, FALSE, VM_PROT_READ | VM_PROT_WRITE | VM_PROT_COPY);
    int protect_errno = errno;
    printf("{\"event\":\"code_protection_call\",\"operation\":\"vm_protect_rw_copy\","
        "\"kern_return\":%d,\"errno\":%d}\n", (int)protect_result,
        protect_errno);
    fflush(stdout);
    if (protect_result != KERN_SUCCESS) return 0;

    printf("{\"event\":\"code_stage\",\"stage\":\"before_instruction_write\","
        "\"replacement\":\"0x52800120\"}\n");
    fflush(stdout);
    instructions[0] = 0x52800120U; /* mov w0, #9; keep ret unchanged */
    uint32_t changed_instruction = instructions[0];
    printf("{\"event\":\"code_stage\",\"stage\":\"instruction_written\","
        "\"instruction_after\":\"0x%08x\",\"return_instruction\":\"0x%08x\","
        "\"altered_function_called_yet\":false}\n", changed_instruction,
        instructions[1]);
    fflush(stdout);

    printf("{\"event\":\"code_stage\",\"stage\":\"before_restore_rx\"}\n");
    fflush(stdout);
    errno = 0;
    kern_return_t restore_result = vm_protect(mach_task_self(), page,
        page_size, FALSE, old_protection);
    int restore_errno = errno;
    vm_prot_t restored_protection = 0;
    int restored_region_result = protection_for_page(page, &restored_protection);
    int restored = restore_result == KERN_SUCCESS
        && restored_region_result == KERN_SUCCESS
        && restored_protection == old_protection;
    printf("{\"event\":\"code_restore\",\"kern_return\":%d,"
        "\"errno\":%d,\"region_kern_return\":%d,"
        "\"protection_after_restore\":%d,\"restoration_matches_initial\":%s}\n",
        (int)restore_result, restore_errno, restored_region_result,
        (int)restored_protection, restored ? "true" : "false");
    fflush(stdout);
    if (!restored) return 31;

    printf("{\"event\":\"code_stage\",\"stage\":\"before_owned_function_call\","
        "\"only_altered_function_called\":true}\n");
    fflush(stdout);
    int value = cold_owned_function();
    printf("{\"event\":\"code_stage\",\"stage\":\"after_owned_function_call\","
        "\"return_value\":%d,\"altered_function_called_once\":true}\n", value);
    fflush(stdout);
    return 0;
}

static int run_child(const char *mode) {
    size_t page_size = (size_t)sysconf(_SC_PAGESIZE);
    if (page_size == 0 || (page_size & (page_size - 1)) != 0) return 20;
    if (strcmp(mode, "owned_code_copy") == 0) return run_owned_code_copy();
    void *heap_page = NULL;
    if (posix_memalign(&heap_page, page_size, page_size) != 0) return 21;
    memset(heap_page, 0x6d, page_size);

    /* The owned mutable control confirms this API and protection tuple work. */
    vm_prot_t control_protection = 0;
    vm_address_t data_page = (vm_address_t)&data_control & ~((vm_address_t)page_size - 1);
    int control_region = protection_for_page(data_page, &control_protection);
    int control_result = -1;
    int control_errno = 0;
    kern_return_t control_kern = KERN_SUCCESS;
    errno = 0;
    if (strcmp(mode, "mprotect") == 0) {
        control_result = mprotect((void *)data_page, page_size, PROT_READ | PROT_WRITE);
        control_errno = errno;
    } else {
        vm_prot_t requested = VM_PROT_READ | VM_PROT_WRITE;
        if (strcmp(mode, "vm_protect_copy") == 0) requested |= VM_PROT_COPY;
        control_kern = vm_protect(mach_task_self(), data_page, page_size,
            FALSE, requested);
        control_result = (int)control_kern;
        control_errno = errno;
    }
    unsigned data_before = data_control;
    unsigned char heap_before = *(volatile unsigned char *)heap_page;
    if (control_result == 0) {
        data_control ^= 1U;
        *(volatile unsigned char *)heap_page ^= 1U;
    }
    kern_return_t control_restore_kern = KERN_SUCCESS;
    int control_restore = 0;
    if (strcmp(mode, "mprotect") == 0) {
        int prot = 0;
        if (control_protection & VM_PROT_READ) prot |= PROT_READ;
        if (control_protection & VM_PROT_WRITE) prot |= PROT_WRITE;
        if (control_protection & VM_PROT_EXECUTE) prot |= PROT_EXEC;
        control_restore = mprotect((void *)data_page, page_size, prot);
    } else {
        control_restore_kern = vm_protect(mach_task_self(), data_page,
            page_size, FALSE, control_protection);
        control_restore = (int)control_restore_kern;
    }
    emit_control("owned_data_and_heap", control_region, control_result,
        control_errno, data_before, data_control, (int)control_restore_kern);
    printf("{\"event\":\"heap_control\",\"before\":%u,\"after\":%u,"
        "\"protection_call_succeeded\":%s,\"restore_result\":%d}\n",
        (unsigned)heap_before,
        (unsigned)*(volatile unsigned char *)heap_page,
        control_result == 0 ? "true" : "false", control_restore);
    fflush(stdout);

    run_attempt(mode, (volatile unsigned char *)text_const_table,
        page_size, 1);
    free(heap_page);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    return run_child(argv[1]);
}
'''

ASM_SOURCE = r'''.section __TEXT,__probe,regular,pure_instructions
.p2align 14
.globl _cold_owned_function
_cold_owned_function:
    mov w0, #7
    ret
'''


class CanaryError(Exception):
    pass


def _run(command, timeout=20):
    try:
        return subprocess.run(command, capture_output=True, text=True,
            timeout=timeout, check=False)
    except subprocess.TimeoutExpired as error:
        raise CanaryError(f"command timed out: {command[0]}") from error


def _events(stdout):
    result = []
    for line in stdout.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            result.append(value)
    return result


def _signed_runtime_evidence(binary):
    verify = _run(["/usr/bin/codesign", "--verify", "--strict", "--verbose=2", str(binary)])
    if verify.returncode != 0:
        raise CanaryError(f"codesign verification failed: {verify.stderr.strip()}")
    details = _run(["/usr/bin/codesign", "-d", "--verbose=4", str(binary)])
    text = details.stdout + details.stderr
    flags = re.search(r"^CodeDirectory .*flags=.*$", text, re.MULTILINE)
    if flags is None or "runtime" not in flags.group(0):
        raise CanaryError("signed host does not report the hardened runtime flag")
    entitlements = _run(["/usr/bin/codesign", "-d", "--entitlements", ":-", str(binary)])
    encoded = entitlements.stdout + entitlements.stderr
    start = encoded.find("<?xml")
    end = encoded.find("</plist>", start)
    if start < 0 or end < 0:
        raise CanaryError("could not read signed host entitlements")
    signed_entitlements = plistlib.loads(encoded[start:end + len("</plist>")].encode())
    if signed_entitlements != ENTITLEMENTS:
        raise CanaryError(f"unexpected signed entitlements: {signed_entitlements!r}")
    return {"verified": True, "runtime_flag": flags.group(0),
        "entitlements": signed_entitlements}


def _verify_const_page_layout(output, page_size):
    section_re = re.compile(
        r"Section\s+sectname (\S+)\s+segname (\S+)\s+addr (0x[0-9a-fA-F]+)\s+"
        r"size (0x[0-9a-fA-F]+)", re.MULTILINE)
    sections = [(name, segment, int(address, 16), int(size, 16))
        for name, segment, address, size in section_re.findall(output)]
    const_sections = [item for item in sections
        if item[0] == "__const" and item[1] == "__TEXT"]
    if len(const_sections) != 1:
        raise CanaryError("fixture must have exactly one __TEXT,__const section")
    _name, _segment, const_address, const_size = const_sections[0]
    if const_address % page_size or const_size < page_size:
        raise CanaryError("const table does not occupy a dedicated page-aligned section")
    const_pages = set(range(const_address // page_size,
        (const_address + const_size + page_size - 1) // page_size))
    executable_names = {"__text", "__stubs", "__stub_helper", "__auth_stubs"}
    for name, segment, address, size in sections:
        if segment != "__TEXT" or name not in executable_names or size == 0:
            continue
        code_pages = set(range(address // page_size,
            (address + size + page_size - 1) // page_size))
        if const_pages & code_pages:
            raise CanaryError("__TEXT,__const shares a VM page with executable sections")
    return {"vmaddr": f"0x{const_address:x}", "size": const_size,
        "page_size": page_size, "isolated_from_executable_sections": True}


def _verify_owned_function_page(otool_output, nm_output, const_layout, page_size):
    symbols = []
    for line in nm_output.splitlines():
        match = re.match(r"^([0-9a-fA-F]+)\s+([A-Za-z])\s+(\S+)$", line)
        if match:
            symbols.append((int(match.group(1), 16), match.group(2), match.group(3)))
    targets = [address for address, _kind, name in symbols
        if name in ("_cold_owned_function", "cold_owned_function")]
    runners = [address for address, _kind, name in symbols
        if name in ("_run_owned_code_copy", "run_owned_code_copy")]
    if len(targets) != 1:
        raise CanaryError("owned cold function symbol is missing or ambiguous")
    if len(runners) != 1:
        raise CanaryError("owned function-copy controller symbol is missing or ambiguous")
    address = targets[0]
    runner_address = runners[0]
    if address % page_size == 0:
        pass
    else:
        raise CanaryError("owned cold function is not aligned to a dedicated VM page")
    page = address // page_size
    runner_page = runner_address // page_size
    if runner_page == page:
        raise CanaryError("active copy controller shares the owned function page")
    const_page = int(const_layout["vmaddr"], 16) // page_size
    if page == const_page:
        raise CanaryError("owned code page overlaps the const table page")
    co_resident = [name for symbol_address, kind, name in symbols
        if symbol_address // page_size == page and kind in ("T", "t")
        and name not in ("_cold_owned_function", "cold_owned_function")]
    if co_resident:
        raise CanaryError(f"other text symbols share the owned function page: {co_resident}")
    text_section_re = re.compile(
        r"Section\s+sectname __probe\s+segname __TEXT\s+addr (0x[0-9a-fA-F]+)\s+"
        r"size (0x[0-9a-fA-F]+)", re.MULTILINE)
    text_sections = [(int(start, 16), int(size, 16))
        for start, size in text_section_re.findall(otool_output)]
    if not any(start <= address < start + size for start, size in text_sections):
        raise CanaryError("owned cold function is outside __TEXT,__probe")
    return {"vmaddr": f"0x{address:x}", "page_vmaddr": f"0x{page * page_size:x}",
        "page_size": page_size, "isolated_text_symbol_page": True,
        "same_page_text_symbols": ["_cold_owned_function"],
        "controller_vmaddr": f"0x{runner_address:x}",
        "controller_page_vmaddr": f"0x{runner_page * page_size:x}",
        "controller_page_is_distinct": True}


def _run_attempt(binary, mode, timeout):
    result = {"mode": mode, "evidence_tags": ["measured", "owned_synthetic_host_only"]}
    try:
        child = subprocess.run([str(binary), mode], capture_output=True,
            text=True, timeout=timeout, check=False)
        result["timed_out"] = False
        result["returncode"] = child.returncode
        result["exit_code"] = child.returncode if child.returncode >= 0 else None
        result["signal"] = (signal.Signals(-child.returncode).name
            if child.returncode < 0 else None)
        result["events"] = _events(child.stdout)
        result["stderr"] = child.stderr[-4000:]
    except subprocess.TimeoutExpired as error:
        stdout = error.stdout or ""
        stderr = error.stderr or ""
        if isinstance(stdout, bytes):
            stdout = stdout.decode(errors="replace")
        if isinstance(stderr, bytes):
            stderr = stderr.decode(errors="replace")
        result.update({"timed_out": True, "returncode": None,
            "exit_code": None, "signal": None, "events": _events(stdout),
            "stderr": stderr[-4000:]})
    result["cs_kill_observation"] = (
        "SIGKILL observed; code-signing attribution is not independently proven"
        if result["signal"] == "SIGKILL" else "no SIGKILL observed")
    return result


def run_canary(timeout=8):
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise CanaryError("canary requires a native Darwin arm64 host")
    for tool in ("/usr/bin/clang", "/usr/bin/codesign", "/usr/bin/file"):
        if not Path(tool).is_file():
            raise CanaryError(f"required tool is unavailable: {tool}")

    scratch = Path(tempfile.mkdtemp(prefix="orbit-text-const-canary-", dir="/private/tmp"))
    os.chmod(scratch, 0o700)
    source = scratch / "owned-host.c"
    assembly = scratch / "owned-probe.s"
    entitlements_file = scratch / "entitlements.plist"
    binary = scratch / "owned-host"
    source.write_text(C_SOURCE)
    assembly.write_text(ASM_SOURCE)
    entitlements_file.write_bytes(plistlib.dumps(ENTITLEMENTS))
    compile_result = _run(["/usr/bin/clang", "-arch", "arm64", "-O0",
        "-Wall", "-Wextra", "-Werror", str(source), str(assembly),
        "-o", str(binary)])
    if compile_result.returncode != 0:
        raise CanaryError(f"owned host compilation failed: {compile_result.stderr.strip()}")
    sign_result = _run(["/usr/bin/codesign", "--force", "--sign", "-",
        "--options", "runtime", "--entitlements", str(entitlements_file), str(binary)])
    if sign_result.returncode != 0:
        raise CanaryError(f"ad-hoc signing failed: {sign_result.stderr.strip()}")
    signature = _signed_runtime_evidence(binary)
    file_result = _run(["/usr/bin/file", str(binary)])
    if file_result.returncode != 0 or "arm64" not in file_result.stdout:
        raise CanaryError("compiled host is not confirmed arm64")
    section_result = _run(["/usr/bin/otool", "-l", str(binary)])
    if section_result.returncode != 0:
        raise CanaryError("fixture does not contain __TEXT,__const")
    const_section = _verify_const_page_layout(section_result.stdout,
        os.sysconf("SC_PAGESIZE"))
    nm_result = _run(["/usr/bin/nm", "-n", str(binary)])
    if nm_result.returncode != 0:
        raise CanaryError("could not inspect owned host symbols")
    function_page = _verify_owned_function_page(section_result.stdout,
        nm_result.stdout, const_section, os.sysconf("SC_PAGESIZE"))

    attempts = [_run_attempt(binary, mode, timeout) for mode in ATTEMPTS]
    artifact = {"schema_version": 1,
        "evidence_tags": ["measured", "owned_synthetic_host_only"],
        "scope": "owned arm64 synthetic host; no vendor bytes or real client memory",
        "verified": False, "read_only_of_product": True,
        "host": {"path": str(binary), "file_description": file_result.stdout.strip(),
            "signature": signature, "const_section": "__TEXT,__const",
            "const_section_layout": const_section,
            "owned_function": "cold_owned_function",
            "owned_function_page_layout": function_page},
        "timeout_seconds_per_child": timeout,
        "attempts": attempts}
    artifact_path = scratch / "artifact.json"
    artifact_path.write_text(json.dumps(artifact, indent=2, sort_keys=True) + "\n")
    os.chmod(artifact_path, 0o600)
    artifact["artifact_path"] = str(artifact_path)
    return artifact


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--timeout", type=float, default=8.0,
        help=f"maximum seconds per attempt child (1-{MAX_TIMEOUT}, default 8)")
    args = parser.parse_args(argv)
    if not 1 <= args.timeout <= MAX_TIMEOUT:
        parser.error(f"--timeout must be between 1 and {MAX_TIMEOUT} seconds")
    try:
        artifact = run_canary(args.timeout)
    except CanaryError as error:
        print(json.dumps({"error": str(error), "verified": False}), file=sys.stderr)
        return 2
    print(json.dumps(artifact, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

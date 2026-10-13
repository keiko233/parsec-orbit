#!/usr/bin/env python3
"""Prepare an owned App Sandbox/Keychain canary without accessing Keychain.

`prepare` only builds/signs scratch artifacts. `run PLAN` requires explicit
authorization: it adds, queries and removes two uniquely named dummy items in
the login Keychain. No real Parsec item is named, searched, read or modified.
All authentication UI is disabled; a locked/unsupported store is a limitation.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import platform
import pwd
import subprocess
import tempfile
import uuid

SOURCE = r'''
#include <Security/Security.h>
#include <CoreFoundation/CoreFoundation.h>
#include <stdio.h>
#include <string.h>
#ifdef CANARY_SBPL
#include <errno.h>
#include <fcntl.h>
#include <sandbox.h>
#include <unistd.h>
/* The measured custom-profile mechanism, without the App Sandbox
   entitlement: deny the real Parsec state roots, then verify one owned
   sentinel is denied before any Security call. Fail closed on any doubt. */
__attribute__((constructor)) static void install_custom_profile(void) {
    const char *roots[] = {ROOT_ONE, ROOT_TWO, ROOT_THREE, ROOT_FOUR};
    char profile[32768];
    int used = snprintf(profile, sizeof(profile), "(version 1)\n(allow default)\n");
    if (used < 0) _exit(95);
    for (int i = 0; i < 4; ++i) {
        int step = snprintf(profile + used, sizeof(profile) - used,
            "(deny file-read* (subpath \"%s\"))\n(deny file-write* (subpath \"%s\"))\n", roots[i], roots[i]);
        if (step < 0 || step >= (int)(sizeof(profile) - used)) _exit(95);
        used += step;
    }
    int step = snprintf(profile + used, sizeof(profile) - used,
        "(deny network-outbound (remote unix-socket (subpath \"%s\")))\n"
        "(deny file-read* (literal \"%s\"))\n", ROOT_ONE, SENTINEL_PATH);
    if (step < 0 || step >= (int)(sizeof(profile) - used)) _exit(95);
    int sentinel = open(SENTINEL_PATH, O_CREAT | O_WRONLY | O_TRUNC, 0600);
    if (sentinel < 0) _exit(95);
    close(sentinel);
    FILE *saved = fopen(PROFILE_PATH, "w");
    if (saved) { fputs(profile, saved); fclose(saved); }
    char *error = NULL;
    if (sandbox_init(profile, 0, &error) != 0) {
        fprintf(stderr, "CANARY_SBPL_INSTALL error=%s\n", error ? error : "(none)");
        _exit(95);
    }
    errno = 0;
    int probe_fd = open(SENTINEL_PATH, O_RDONLY);
    int gate = probe_fd < 0 && (errno == EPERM || errno == EACCES);
    if (probe_fd >= 0) close(probe_fd);
    unlink(SENTINEL_PATH);
    fprintf(stderr, "CANARY_SBPL_GATE verified=%d errno=%d\n", gate, gate ? errno : 0);
    if (!gate) _exit(96);
}
#endif
static const char *services[] = {SERVICE_ONE, SERVICE_TWO};
static const char *payload = PAYLOAD;
static CFMutableDictionaryRef query(int i) {
    CFMutableDictionaryRef q = CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFStringRef s = CFStringCreateWithCString(NULL, services[i], kCFStringEncodingUTF8);
    CFDictionarySetValue(q, kSecClass, kSecClassGenericPassword);
    CFDictionarySetValue(q, kSecAttrService, s);
    CFDictionarySetValue(q, kSecAttrAccount, CFSTR("orbit-owned-canary"));
    CFDictionarySetValue(q, kSecUseDataProtectionKeychain, kCFBooleanFalse);
    CFRelease(s); return q;
}
static int probe(int i) {
    CFMutableDictionaryRef q = query(i);
    CFDictionarySetValue(q, kSecReturnData, kCFBooleanTrue);
    CFDictionarySetValue(q, kSecUseAuthenticationUI, kSecUseAuthenticationUIFail);
    CFDictionarySetValue(q, kSecMatchLimit, kSecMatchLimitOne);
    CFTypeRef data = NULL; OSStatus status = SecItemCopyMatching(q, &data);
    int matched = status == errSecSuccess && data && CFGetTypeID(data) == CFDataGetTypeID()
        && CFDataGetLength(data) == (CFIndex)strlen(payload)
        && memcmp(CFDataGetBytePtr(data), payload, strlen(payload)) == 0;
    printf("probe %d %d %d\n", i, (int)status, matched);
    if (data) CFRelease(data); CFRelease(q); return status;
}
#ifdef CANARY_CONTROLLER
static int add(int i) {
    CFMutableDictionaryRef q = query(i);
    SecAccessRef access = NULL;
    CFArrayRef trusted = NULL;
    SecTrustedApplicationRef apps[2] = {NULL, NULL};
    OSStatus status = errSecSuccess;
    if (i == 1) {
        status = SecTrustedApplicationCreateFromPath(CONTROLLER_PATH, &apps[0]);
        if (!status) status = SecTrustedApplicationCreateFromPath(CLIENT_PATH, &apps[1]);
        if (!status) trusted = CFArrayCreate(NULL, (const void **)apps, 2, &kCFTypeArrayCallBacks);
    }
    // nil trusts only this controller; item 1 explicitly trusts the owned client too.
    if (!status) status = SecAccessCreate(CFSTR("Orbit owned canary"), trusted, &access);
    if (!status) {
        CFDataRef value = CFDataCreate(NULL, (const UInt8 *)payload, strlen(payload));
        CFDictionarySetValue(q, kSecAttrAccess, access);
        CFDictionarySetValue(q, kSecValueData, value);
        status = SecItemAdd(q, NULL); CFRelease(value);
    }
    printf("add %d %d\n", i, (int)status);
    if (access) CFRelease(access); if (trusted) CFRelease(trusted);
    for (int j = 0; j < 2; ++j) if (apps[j]) CFRelease(apps[j]);
    CFRelease(q); return status;
}
static int remove_item(int i) {
    CFMutableDictionaryRef q = query(i);
    CFDictionarySetValue(q, kSecUseAuthenticationUI, kSecUseAuthenticationUIFail);
    OSStatus status = SecItemDelete(q);
    printf("delete %d %d\n", i, (int)status); CFRelease(q);
    return status == errSecItemNotFound ? 0 : status;
}
#endif
int main(int argc, char **argv) {
    OSStatus no_ui = SecKeychainSetUserInteractionAllowed(false);
    if (no_ui) { printf("interaction-disable %d\n", (int)no_ui); return 92; }
#ifdef CANARY_CONTROLLER
    if (argc != 3 || (strcmp(argv[2], "0") && strcmp(argv[2], "1"))) return 90;
    int i = argv[2][0] - '0'; int status;
    if (!strcmp(argv[1], "add")) status = add(i);
    else if (!strcmp(argv[1], "delete")) status = remove_item(i);
    else if (!strcmp(argv[1], "probe")) status = probe(i);
    else return 91;
    return status == 0 ? 0 : 1;
#else
    (void)argc; (void)argv;
    probe(0); probe(1); return 0;
#endif
}
'''


def command(args):
    return subprocess.run(args, capture_output=True, text=True, timeout=15)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def prepare(client_mode):
    root = Path(tempfile.mkdtemp(prefix="orbit-kc-", dir="/private/tmp")).resolve(strict=True)
    os.chmod(root, 0o700)
    token = uuid.uuid4().hex
    services = [f"dev.parsec-orbit.keychain-canary.{token}.controller-only",
        f"dev.parsec-orbit.keychain-canary.{token}.explicit-client"]
    contents = root / "KeychainCanary.app/Contents"
    (contents / "MacOS").mkdir(parents=True, mode=0o700)
    client, controller = contents / "MacOS/canary", root / "controller"
    info = {"CFBundleIdentifier": "dev.parsec-orbit.keychain-canary." + token,
        "CFBundleExecutable": "canary", "CFBundlePackageType": "APPL", "CFBundleVersion": "1"}
    (contents / "Info.plist").write_bytes(plistlib.dumps(info))
    definitions = {"SERVICE_ONE": services[0], "SERVICE_TWO": services[1],
        "PAYLOAD": "OWNED_FAKE_CANARY_" + token,
        "CONTROLLER_PATH": str(controller), "CLIENT_PATH": str(client)}
    real_home = Path(pwd.getpwuid(os.getuid()).pw_dir)
    sentinel = root / "sbpl-sentinel"
    profile_out = root / "sbpl-profile.sbpl"
    if client_mode == "sbpl":
        roots = [real_home / ".parsec", real_home / ".parsec-persistent",
            Path("/Users/Shared/.parsec"), Path("/Users/Shared/.parsec-persistent")]
        definitions.update({"ROOT_ONE": str(roots[0]), "ROOT_TWO": str(roots[1]),
            "ROOT_THREE": str(roots[2]), "ROOT_FOUR": str(roots[3]),
            "SENTINEL_PATH": str(sentinel), "PROFILE_PATH": str(profile_out)})
    source = root / "canary.c"
    source.write_text("".join(f"#define {key} {json.dumps(value)}\n" for key, value in definitions.items()) + SOURCE)
    client_flags = ["-DCANARY_SBPL"] if client_mode == "sbpl" else []
    for executable, flags in ((client, client_flags), (controller, ["-DCANARY_CONTROLLER"])):
        result = command(["clang", "-arch", "arm64", "-Wall", "-Werror", "-Wno-deprecated-declarations",
            *flags, "-framework", "Security", "-framework", "CoreFoundation", str(source), "-o", str(executable)])
        if result.returncode:
            raise ValueError(result.stderr)
    entitlements = root / "entitlements.plist"
    entitlement_map = {"com.apple.security.cs.disable-library-validation": True}
    if client_mode == "app-sandbox":
        entitlement_map["com.apple.security.app-sandbox"] = True
    entitlements.write_bytes(plistlib.dumps(entitlement_map))
    for target, extra in ((controller, []), (contents.parent, ["--entitlements", str(entitlements)])):
        result = command(["codesign", "--force", "--sign", "-", "--options", "runtime", *extra, str(target)])
        if result.returncode:
            raise ValueError(result.stderr)
        result = command(["codesign", "--verify", "--strict", str(target)])
        if result.returncode:
            raise ValueError(result.stderr)
    plan = {"scope": "two owned dummy Keychain items only", "scratch": str(root),
        "keychain_access_performed": False, "controller": str(controller), "client": str(client),
        "client_mode": client_mode,
        "controller_sha256": digest(controller), "client_sha256": digest(client),
        "services": services, "account": "orbit-owned-canary", "bundle_id": info["CFBundleIdentifier"],
        "payload_is_fake": True, "authentication_ui": "disabled",
        "needs_explicit_authorization": "adds, queries and deletes only these UUID-bound dummy items in login Keychain"}
    path = root / "keychain-plan.json"
    path.write_text(json.dumps(plan, indent=2) + "\n"); path.chmod(0o600)
    print(path)


def run_plan(path):
    plan = json.loads(path.read_text())
    root = Path(plan["scratch"]).resolve(strict=True)
    if root != path.resolve(strict=True).parent or root.stat().st_uid != os.geteuid() or root.stat().st_mode & 0o077:
        raise ValueError("plan is not in a private owned scratch directory")
    if plan.get("scope") != "two owned dummy Keychain items only" or plan.get("payload_is_fake") is not True:
        raise ValueError("only the prepared dummy item plan is accepted")
    controller, client = Path(plan["controller"]).resolve(strict=True), Path(plan["client"]).resolve(strict=True)
    for executable, expected in ((controller, plan["controller_sha256"]), (client, plan["client_sha256"])):
        if not executable.is_relative_to(root) or digest(executable) != expected:
            raise ValueError("prepared payload changed or escaped scratch")
    report = {"scope": plan["scope"], "services": plan["services"], "status": "incomplete",
        "client_mode": plan.get("client_mode", "app-sandbox"), "complete_isolation_proven": False,
        "operations": [], "cleanup": [], "cleanup_verification": []}
    attempted = []
    try:
        for i in range(2):
            # A timed-out add may have committed before its process ended.
            # Cleanup must cover intent, not just successfully returned calls.
            attempted.append(i)
            result = command([str(controller), "add", str(i)])
            report["operations"].append({"operation": "add", "index": i, "exit": result.returncode,
                "stdout": result.stdout, "stderr": result.stderr})
            if result.returncode:
                raise ValueError("dummy item creation failed; client probe withheld")
            result = command([str(controller), "probe", str(i)])
            report["operations"].append({"operation": "controller-probe", "index": i,
                "exit": result.returncode, "stdout": result.stdout, "stderr": result.stderr})
            if result.returncode or not result.stdout.strip().endswith("0 1"):
                raise ValueError("controller could not verify its own dummy payload")
        result = command([str(client)])
        report["operations"].append({"operation": f"{report['client_mode']}-client-probe",
            "exit": result.returncode,
            "stdout": result.stdout, "stderr": result.stderr})
        profile_path = root / "sbpl-profile.sbpl"
        if report["client_mode"] == "sbpl" and profile_path.exists():
            report["client_profile"] = profile_path.read_text()
        lines = result.stdout.splitlines()
        observations = []
        for i, line in enumerate(lines):
            fields = line.split()
            if len(fields) == 4 and fields[:2] == ["probe", str(i)] and fields[3] in ("0", "1"):
                try:
                    observations.append({"index": i, "status": int(fields[2]), "payload_matched": fields[3] == "1"})
                except ValueError:
                    pass
        report["client_observations"] = observations
        report["status"] = "measured" if result.returncode == 0 and len(lines) == len(observations) == 2 else "incomplete"
    except Exception as error:
        report["error"] = str(error)
    finally:
        for i in attempted:
            try:
                result = command([str(controller), "delete", str(i)])
                report["cleanup"].append({"index": i, "exit": result.returncode,
                    "stdout": result.stdout, "stderr": result.stderr})
                if result.returncode:
                    report["status"] = "cleanup-failed"
            except Exception as error:
                report["cleanup"].append({"index": i, "error": str(error)})
                report["status"] = "cleanup-failed"
            try:
                result = command([str(controller), "probe", str(i)])
                absent = result.returncode == 1 and result.stdout.strip() == f"probe {i} -25300 0"
                report["cleanup_verification"].append({"index": i, "exit": result.returncode,
                    "stdout": result.stdout, "stderr": result.stderr, "item_absent": absent})
                if not absent:
                    report["status"] = "cleanup-failed"
            except Exception as error:
                report["cleanup_verification"].append({"index": i, "error": str(error), "item_absent": False})
                report["status"] = "cleanup-failed"
        output = root / "keychain-result.json"
        output.write_text(json.dumps(report, indent=2) + "\n"); output.chmod(0o600)
        print(output)
    return 0 if report["status"] == "measured" else 2


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="mode", required=True)
    prepare_parser = subcommands.add_parser("prepare")
    prepare_parser.add_argument(
        "--client",
        choices=("app-sandbox", "sbpl"),
        default="app-sandbox",
        help="app-sandbox: entitlement client (spent plan); sbpl: re-signed client "
        "with the measured custom constructor profile",
    )
    run = subcommands.add_parser("run"); run.add_argument("plan", type=Path)
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64" or os.geteuid() == 0:
        raise ValueError("requires ordinary-user macOS arm64")
    return prepare(args.client) if args.mode == "prepare" else run_plan(args.plan)


if __name__ == "__main__":
    raise SystemExit(main())

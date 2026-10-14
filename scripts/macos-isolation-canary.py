#!/usr/bin/env python3
"""Scratch-only macOS sandbox measurement fixture; never a production backend.

Builds an arm64 C executable with a strong LC_LOAD_DYLIB dependency on a
constructor dylib. The constructor calls the deprecated/unsupported sandbox_init
API with an experimental custom SBPL profile and flags=0. Every probed path is
created under a new private TemporaryDirectory. No account or vendor paths are
read, written, or probed. Results are emitted as JSON on stdout.
"""

from __future__ import annotations

import json
import argparse
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path
from typing import Any


CANARY_XATTR_NAME = "com.parsec-orbit.canary"


MAIN_C = r'''#define _DARWIN_C_SOURCE 1
#include <arpa/inet.h>
#include <CoreFoundation/CoreFoundation.h>
#include <dirent.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <mach/mach.h>
#include <netinet/in.h>
#include <servers/bootstrap.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/un.h>
#include <sys/xattr.h>
#include <unistd.h>

extern int canary_preopened_fd(void);
extern int canary_preopened_dirfd(void);

static void result(const char *name, int succeeded, int err) {
  printf("RESULT %s %s %d\n", name, succeeded ? "success" : "failure", succeeded ? 0 : err);
}
static void check(const char *name, int rc) { result(name, rc == 0, errno); }
static void check_fd(const char *name, int fd) { result(name, fd >= 0, errno); }

int main(void) {
  const char *p = getenv("CANARY_PROTECTED");
  const char *a = getenv("CANARY_ALLOWED");
  const char *sock = getenv("CANARY_SOCKET");
  if (!p || !a || !sock) return 90;
  char path[4096], linkpath[4096], allowed[4096], allowed_link[4096];
  struct stat st;

  snprintf(path, sizeof(path), "%s/read.txt", p);
  errno = 0; int fd = open(path, O_RDONLY); check_fd("open", fd); if (fd >= 0) close(fd);
  errno = 0; fd = openat(AT_FDCWD, path, O_RDONLY); check_fd("openat_absolute", fd); if (fd >= 0) close(fd);
  errno = 0; fd = openat(canary_preopened_dirfd(), "read.txt", O_RDONLY); check_fd("openat_preopened_dirfd", fd); if (fd >= 0) close(fd);
  errno = 0; FILE *fp = fopen(path, "r"); result("fopen", fp != NULL, errno); if (fp) fclose(fp);
  errno = 0; check("stat", stat(path, &st));
  errno = 0; check("lstat", lstat(path, &st));
  errno = 0; check("access", access(path, R_OK));

  errno = 0; DIR *dir = opendir(p);
  result("opendir", dir != NULL, errno);
  if (dir) {
    errno = 0; struct dirent *ent = readdir(dir);
    result("readdir", ent != NULL, errno);
    closedir(dir);
  } else {
    printf("RESULT readdir skipped %d\n", errno);
  }

  snprintf(path, sizeof(path), "%s/unlink.txt", p);
  errno = 0; check("unlink", unlink(path));
  snprintf(path, sizeof(path), "%s/rename-source.txt", p);
  snprintf(linkpath, sizeof(linkpath), "%s/rename-target.txt", p);
  errno = 0; check("rename", rename(path, linkpath));
  snprintf(path, sizeof(path), "%s/write.txt", p);
  errno = 0; fd = open(path, O_WRONLY | O_APPEND); check_fd("write_open", fd);
  if (fd >= 0) {
    errno = 0; ssize_t n = write(fd, "x", 1); result("write", n == 1, errno);
    errno = 0; int rc = flock(fd, LOCK_EX | LOCK_NB); result("flock", rc == 0, errno);
    if (rc == 0) flock(fd, LOCK_UN);
    close(fd);
  } else {
    printf("RESULT write skipped %d\nRESULT flock skipped %d\n", errno, errno);
  }

  snprintf(linkpath, sizeof(linkpath), "%s/new-read-link.txt", p);
  snprintf(path, sizeof(path), "%s/read.txt", p);
  errno = 0; check("symlink_create", symlink(path, linkpath));
  snprintf(linkpath, sizeof(linkpath), "%s/protected-read-link", a);
  errno = 0; fd = open(linkpath, O_RDONLY); check_fd("open_through_symlink", fd); if (fd >= 0) close(fd);
  snprintf(path, sizeof(path), "%s/write.txt", p);
  snprintf(linkpath, sizeof(linkpath), "%s/new-write-link.txt", p);
  errno = 0; check("write_symlink_create", symlink(path, linkpath));
  snprintf(linkpath, sizeof(linkpath), "%s/protected-write-link", a);
  errno = 0; fd = open(linkpath, O_RDWR); check_fd("write_symlink_open", fd);
  if (fd >= 0) {
    errno = 0; ssize_t linked_write = pwrite(fd, "s", 1, 0); result("write_through_symlink", linked_write == 1, errno);
    errno = 0; int linked_lock = flock(fd, LOCK_EX | LOCK_NB); result("flock_through_symlink", linked_lock == 0, errno);
    if (linked_lock == 0) flock(fd, LOCK_UN);
    close(fd);
  } else printf("RESULT write_through_symlink skipped %d\nRESULT flock_through_symlink skipped %d\n", errno, errno);

  fd = canary_preopened_fd();
  errno = 0; check("preopened_fd_fstat", fstat(fd, &st));
  errno = 0; ssize_t n = pread(fd, (char[1]){0}, 1, 0); result("preopened_fd_read", n >= 0, errno);
  errno = 0; n = pwrite(fd, "z", 1, 0); result("preopened_fd_write", n == 1, errno);
  errno = 0; int rc = flock(fd, LOCK_EX | LOCK_NB); result("preopened_fd_flock", rc == 0, errno);
  if (rc == 0) flock(fd, LOCK_UN);

  snprintf(allowed, sizeof(allowed), "%s/ordinary.txt", a);
  errno = 0; fd = open(allowed, O_WRONLY | O_APPEND); check_fd("allowed_open", fd);
  if (fd >= 0) { errno = 0; n = write(fd, "a", 1); result("allowed_write", n == 1, errno); close(fd); }
  snprintf(allowed_link, sizeof(allowed_link), "%s/ordinary-link.txt", a);
  errno = 0; check("allowed_symlink", symlink(allowed, allowed_link));
  errno = 0; fd = open(allowed_link, O_RDONLY); check_fd("allowed_open_through_symlink", fd); if (fd >= 0) close(fd);
  errno = 0; check("allowed_metadata", stat(allowed, &st));
  errno = 0; DIR *adir = opendir(a); result("allowed_opendir", adir != NULL, errno);
  if (adir) { errno = 0; struct dirent *ent = readdir(adir); result("allowed_readdir", ent != NULL, errno); closedir(adir); }
  else printf("RESULT allowed_readdir skipped %d\n", errno);

  errno = 0; int s = socket(AF_UNIX, SOCK_STREAM, 0); check_fd("unix_socket_create", s);
  if (s >= 0) {
    struct sockaddr_un un = {0}; un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof(un.sun_path), "%s", sock);
    errno = 0; rc = connect(s, (struct sockaddr *)&un, sizeof(un)); result("unix_socket_connect", rc == 0, errno);
    close(s);
  } else printf("RESULT unix_socket_connect skipped %d\n", errno);
  snprintf(path, sizeof(path), "%s/unix-socket", p);
  errno = 0; check("unix_socket_path_unlink", unlink(path));
  errno = 0; s = socket(AF_UNIX, SOCK_STREAM, 0); check_fd("unix_socket_bind_socket", s);
  if (s >= 0) {
    struct sockaddr_un un = {0}; un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof(un.sun_path), "%s/bind-socket", p);
    errno = 0; rc = bind(s, (struct sockaddr *)&un, sizeof(un)); result("unix_socket_bind", rc == 0, errno);
    close(s);
  } else printf("RESULT unix_socket_bind skipped %d\n", errno);

  {
    const char *dfd_s = getenv("CANARY_ALLOWED_DIRFD");
    int adfd = (dfd_s && *dfd_s) ? atoi(dfd_s) : -1;
    if (adfd >= 0) {
      errno = 0; fd = openat(adfd, "../protected/read.txt", O_RDONLY); check_fd("openat_relative_escape", fd);
      if (fd >= 0) close(fd);
    } else printf("RESULT openat_relative_escape skipped %d\n", errno);
  }
  {
    char cwd[4096];
    int have_cwd = getcwd(cwd, sizeof(cwd)) != NULL;
    snprintf(path, sizeof(path), "%s/read.txt", p);
    errno = 0; rc = chdir(a);
    if (rc == 0) {
      errno = 0; fd = open("../protected/read.txt", O_RDONLY); check_fd("chdir_relative_escape", fd);
      if (fd >= 0) close(fd);
      if (have_cwd) chdir(cwd); else chdir("/private/tmp");
    } else {
      printf("RESULT chdir_relative_escape skipped %d\n", errno);
    }
  }
  snprintf(linkpath, sizeof(linkpath), "%s/protected-read-link", a);
  errno = 0; fd = open(linkpath, O_RDONLY | O_NOFOLLOW); result("open_nofollow_alias", fd >= 0, errno);
  if (fd >= 0) close(fd);
  {
    const char *firmlink = getenv("CANARY_FIRMLINK_FILE");
    if (firmlink && *firmlink) {
      errno = 0; fd = open(firmlink, O_RDONLY); check_fd("firmlink_open", fd);
      if (fd >= 0) close(fd);
    } else printf("RESULT firmlink_open skipped %d\n", errno);
    const char *tmpalias = getenv("CANARY_TMP_ALIAS_FILE");
    if (tmpalias && *tmpalias) {
      errno = 0; fd = open(tmpalias, O_RDONLY); check_fd("tmp_alias_open", fd);
      if (fd >= 0) close(fd);
    } else printf("RESULT tmp_alias_open skipped %d\n", errno);
  }
  snprintf(path, sizeof(path), "%s/new-dir", p);
  errno = 0; check("mkdir", mkdir(path, 0700));
  snprintf(path, sizeof(path), "%s/rmdir-target", p);
  errno = 0; check("rmdir", rmdir(path));
  snprintf(path, sizeof(path), "%s/read.txt", p);
  errno = 0; check("chmod", chmod(path, 0600));
  snprintf(path, sizeof(path), "%s/write.txt", p);
  errno = 0; check("truncate", truncate(path, 0));
  errno = 0; check("utimes", utimes(path, NULL));
  snprintf(path, sizeof(path), "%s/read.txt", p);
  snprintf(linkpath, sizeof(linkpath), "%s/hardlink-target", a);
  errno = 0; check("hardlink_protected_to_allowed", link(path, linkpath));
  snprintf(path, sizeof(path), "%s/ordinary.txt", a);
  snprintf(linkpath, sizeof(linkpath), "%s/hardlink-source", p);
  errno = 0; check("hardlink_allowed_to_protected", link(path, linkpath));
  snprintf(path, sizeof(path), "%s/read.txt", p);
  errno = 0; ssize_t xr = listxattr(path, NULL, 0, 0); result("listxattr", xr >= 0, errno);
  const char *xattr_name = getenv("CANARY_XATTR_NAME");
  if (!xattr_name || !*xattr_name) xattr_name = "com.apple.quarantine";
  errno = 0; xr = getxattr(path, xattr_name, NULL, 0, 0, 0); result("getxattr", xr >= 0, errno);
  errno = 0; struct statfs vfs; check("statfs", statfs(p, &vfs));
  char resolved[4096];
  errno = 0; char *rp = realpath(path, resolved); result("realpath_protected", rp != NULL, errno);
  errno = 0; fd = open(p, O_RDONLY | O_DIRECTORY); check_fd("dir_open", fd);
  if (fd >= 0) {
    errno = 0; check("fchdir", fchdir(fd));
    close(fd);
  } else {
    printf("RESULT fchdir skipped %d\n", errno);
  }
  snprintf(path, sizeof(path), "%s/write.txt", p);
  errno = 0; check("access_w", access(path, W_OK));
  errno = 0; FILE *afp = fopen(path, "a"); result("fopen_append", afp != NULL, errno);
  if (afp) fclose(afp);
  errno = 0; fd = open(path, O_WRONLY | O_TRUNC); check_fd("open_trunc", fd);
  if (fd >= 0) close(fd);
  snprintf(path, sizeof(path), "%s/ordinary.txt", a);
  errno = 0; fd = open(path, O_RDONLY); check_fd("allowed_read_post_policy", fd);
  if (fd >= 0) {
    char buffer[1];
    errno = 0; ssize_t n = read(fd, buffer, 1);
    result("allowed_read_bytes", n == 1, errno);
    close(fd);
  } else {
    printf("RESULT allowed_read_bytes skipped %d\n", errno);
  }
  errno = 0; void *fw = dlopen("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation", RTLD_NOW);
  result("dlopen_system_framework", fw != NULL, errno);
  if (fw) dlclose(fw);
  {
    const char *port_s = getenv("CANARY_TCP_PORT");
    int port = (port_s && *port_s) ? atoi(port_s) : 0;
    errno = 0; int ts = socket(AF_INET, SOCK_STREAM, 0); check_fd("tcp_socket", ts);
    if (ts >= 0 && port > 0) {
      struct sockaddr_in in = {0};
      in.sin_family = AF_INET; in.sin_port = htons((uint16_t)port);
      inet_pton(AF_INET, "127.0.0.1", &in.sin_addr);
      errno = 0; rc = connect(ts, (struct sockaddr *)&in, sizeof(in));
      result("tcp_loopback_connect", rc == 0, errno);
      close(ts);
    } else printf("RESULT tcp_loopback_connect skipped %d\n", errno);
  }
  {
    /* Owned Mach services: the unsandboxed helper publishes both names; the
       profile decides whether the guarded client can reach them. */
    const char *name_a = getenv("CANARY_MACH_A");
    const char *name_b = getenv("CANARY_MACH_B");
    if (!name_a || !*name_a || !name_b || !*name_b) {
      printf("RESULT mach_lookup_a skipped %d\n", EINVAL);
      printf("RESULT mach_send_a skipped %d\n", EINVAL);
      printf("RESULT mach_lookup_b skipped %d\n", EINVAL);
      printf("RESULT mach_lookup_system skipped %d\n", EINVAL);
    } else {
      CFStringRef service = CFStringCreateWithCString(NULL, name_a, kCFStringEncodingUTF8);
      CFMessagePortRef remote = service ? CFMessagePortCreateRemote(NULL, service) : NULL;
      result("mach_lookup_a", remote != NULL, errno);
      if (remote) {
        CFDataRef request = CFDataCreate(NULL, (const UInt8 *)"ping", 4);
        CFDataRef reply = NULL;
        SInt32 sent = CFMessagePortSendRequest(remote, 1, request, 1.0, 1.0, NULL, &reply);
        int code = sent == 0 ? 0 : (int)(sent < 0 ? -sent : sent);
        result("mach_send_a", sent == 0, code);
        if (request) CFRelease(request);
        if (reply) CFRelease(reply);
        CFRelease(remote);
      } else printf("RESULT mach_send_a skipped %d\n", errno);
      if (service) CFRelease(service);
      /* Same helper, second owned name: allowlist rules must distinguish it. */
      mach_port_t port = MACH_PORT_NULL;
      kern_return_t looked_up = bootstrap_look_up(bootstrap_port, name_b, &port);
      result("mach_lookup_b", looked_up == KERN_SUCCESS, (int)looked_up);
      if (looked_up == KERN_SUCCESS) mach_port_deallocate(mach_task_self(), port);
      /* A standard system service: a blanket deny must be visible here too. */
      port = MACH_PORT_NULL;
      kern_return_t system = bootstrap_look_up(bootstrap_port, "com.apple.cfprefsd.agent", &port);
      result("mach_lookup_system", system == KERN_SUCCESS, (int)system);
      if (system == KERN_SUCCESS) mach_port_deallocate(mach_task_self(), port);
    }
  }
  return 0;
}
'''


EARLY_C = r'''#define _DARWIN_C_SOURCE 1
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static void order_log(const char *event, long long value) {
  const char *log = getenv("CANARY_ORDER_LOG");
  if (!log) return;
  FILE *f = fopen(log, "a");
  if (!f) return;
  struct timespec ts = {0};
  clock_gettime(CLOCK_MONOTONIC, &ts);
  fprintf(f, "%s %lld.%09ld %lld\n", event, (long long)ts.tv_sec, ts.tv_nsec, value);
  fclose(f);
}

__attribute__((constructor)) static void early_constructor(void) {
  const char *root = getenv("CANARY_PROTECTED");
  if (!root) _exit(90);
  char file[4096];
  snprintf(file, sizeof(file), "%s/read.txt", root);
  errno = 0;
  int fd = open(file, O_RDONLY);
  int error = fd >= 0 ? 0 : errno;
  if (fd >= 0) close(fd);
  order_log("early_constructor_pre_guard_open", fd >= 0 ? 0 : -error);
}
'''


MACH_HELPER_C = r'''#include <CoreFoundation/CoreFoundation.h>
#include <stdio.h>

static CFDataRef reply(CFMessagePortRef local, SInt32 id, CFDataRef data, void *info) {
  (void)local; (void)id; (void)data; (void)info;
  return CFDataCreate(NULL, (const UInt8 *)"pong", 4);
}

int main(int argc, char **argv) {
  if (argc != 3) return 2;
  CFStringRef name_a = CFStringCreateWithCString(NULL, argv[1], kCFStringEncodingUTF8);
  CFStringRef name_b = CFStringCreateWithCString(NULL, argv[2], kCFStringEncodingUTF8);
  CFMessagePortRef port_a = name_a ? CFMessagePortCreateLocal(NULL, name_a, reply, NULL, NULL) : NULL;
  CFMessagePortRef port_b = name_b ? CFMessagePortCreateLocal(NULL, name_b, reply, NULL, NULL) : NULL;
  if (!port_a || !port_b) { fprintf(stderr, "publish failed\n"); return 3; }
  CFRunLoopSourceRef source_a = CFMessagePortCreateRunLoopSource(NULL, port_a, 0);
  CFRunLoopSourceRef source_b = CFMessagePortCreateRunLoopSource(NULL, port_b, 0);
  if (!source_a || !source_b) { fprintf(stderr, "source failed\n"); return 4; }
  CFRunLoopAddSource(CFRunLoopGetCurrent(), source_a, kCFRunLoopDefaultMode);
  CFRunLoopAddSource(CFRunLoopGetCurrent(), source_b, kCFRunLoopDefaultMode);
  printf("READY\n");
  fflush(stdout);
  CFRunLoopRun();
  return 0;
}
'''


GUARD_C = r'''#define _DARWIN_C_SOURCE 1
#include <errno.h>
#include <fcntl.h>
#include <sandbox.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static int inherited_fd = -1;
static int inherited_dirfd = -1;
int canary_preopened_fd(void) { return inherited_fd; }
int canary_preopened_dirfd(void) { return inherited_dirfd; }

static void sbpl_quote(const char *s, char *out, size_t n) {
  size_t j = 0;
  for (size_t i = 0; s[i] && j + 3 < n; ++i) {
    if (s[i] == '\\' || s[i] == '"') out[j++] = '\\';
    out[j++] = s[i];
  }
  out[j] = 0;
}

static void order_log(const char *event, long long value) {
  const char *log = getenv("CANARY_ORDER_LOG");
  if (!log) return;
  FILE *f = fopen(log, "a");
  if (!f) return;
  struct timespec ts = {0};
  clock_gettime(CLOCK_MONOTONIC, &ts);
  fprintf(f, "%s %lld.%09ld %lld\n", event, (long long)ts.tv_sec, ts.tv_nsec, value);
  fclose(f);
}

__attribute__((constructor)) static void install_experimental_guard(void) {
  const char *root = getenv("CANARY_PROTECTED");
  if (!root) _exit(91);
  const char *sentinel = getenv("CANARY_SENTINEL");
  const char *profile_out = getenv("CANARY_PROFILE_OUT");
  if (!sentinel || !profile_out) _exit(91);
  char file[4096], quoted[8192], squoted[8192], profile[24000];
  snprintf(file, sizeof(file), "%s/descriptor.txt", root);
  inherited_fd = open(file, O_RDWR);
  inherited_dirfd = open(root, O_RDONLY | O_DIRECTORY);
  if (inherited_fd < 0 || inherited_dirfd < 0) _exit(92);
  order_log("guard_constructor_begin", 0);
  /* The sentinel is created before the policy and probed after it so the
   * guard can verify the deny policy is actually installed (fail closed). */
  int sfd = open(sentinel, O_CREAT | O_WRONLY | O_TRUNC, 0600);
  if (sfd < 0) _exit(92);
  if (write(sfd, "sentinel\n", 9) != 9) _exit(92);
  close(sfd);
  if (getenv("CANARY_EXPLICIT_BASELINE")) {
    order_log("guard_constructor_baseline", 0);
    dprintf(STDERR_FILENO, "CANARY_BASELINE_UNGUARDED\n");
    return; /* Explicit scratch-only control run, never an error fallback. */
  }
  sbpl_quote(root, quoted, sizeof(quoted));
  sbpl_quote(sentinel, squoted, sizeof(squoted));
  const char *extra = getenv("CANARY_IPC");
  if (!extra) extra = "";
  snprintf(profile, sizeof(profile),
    "(version 1)\n"
    "(allow default)\n"
    "(deny file-read* (subpath \"%s\"))\n"
    "(deny file-write* (subpath \"%s\"))\n"
    "(deny network-outbound (remote unix-socket (subpath \"%s\")))\n"
    "(deny file-read* (literal \"%s\"))\n"
    "%s", quoted, quoted, quoted, squoted, extra);
  FILE *pf = fopen(profile_out, "w");
  if (pf) { fputs(profile, pf); fclose(pf); }
  char *error = NULL;
  /* Experimental custom SBPL with flags=0; sandbox_init is deprecated and unsupported. */
  int rc = sandbox_init(profile, 0, &error);
  if (rc != 0) {
    dprintf(STDERR_FILENO, "CANARY_GUARD_ERROR rc=%d errno=%d detail=%s\n", rc, errno, error ? error : "(none)");
    if (error) sandbox_free_error(error);
    _exit(93); /* Fail closed for this fixture; never continue unguarded. */
  }
  errno = 0;
  int gate_fd = open(sentinel, O_RDONLY);
  int gate_errno = gate_fd >= 0 ? 0 : errno;
  if (gate_fd >= 0) close(gate_fd);
  int gate_verified = (gate_fd < 0 && (gate_errno == EPERM || gate_errno == EACCES));
  dprintf(STDERR_FILENO, "CANARY_GATE sandbox_init_rc=%d sentinel_open_errno=%d gate_verified=%d\n",
    rc, gate_errno, gate_verified);
  order_log("guard_constructor_policy_installed", gate_verified);
  if (!gate_verified) _exit(94); /* A silently ineffective profile must not continue. */
  dprintf(STDERR_FILENO, "CANARY_GUARD_INSTALLED api=sandbox_init flags=0\n");
}
'''


def run(cmd: list[str], *, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(cmd, text=True, capture_output=True, env=env, check=False, timeout=30)


def set_canary_xattr(path: Path, name: str, value: bytes) -> bool:
    """Attach one owned xattr so the value-read probe can distinguish permit from absence."""
    import ctypes
    import ctypes.util

    libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
    fn = libc.setxattr
    fn.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p,
                   ctypes.c_size_t, ctypes.c_uint32, ctypes.c_int]
    return fn(str(path).encode(), name.encode(), value, len(value), 0, 0) == 0


def record(name: str, status: str, **fields: Any) -> dict[str, Any]:
    return {"name": name, "status": status, **fields}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", action="store_true", help="explicit separate scratch-only unguarded control")
    parser.add_argument(
        "--ipc",
        choices=("none", "allowlist"),
        default="none",
        help="none: path/deny profile only; allowlist: additionally deny mach-lookup "
        "except the owned canary service name",
    )
    args = parser.parse_args()
    if os.geteuid() == 0:
        raise ValueError("this scratch-only fixture must run as an ordinary user")
    if sys.platform != "darwin":
        print(json.dumps({"fixture": "macos-isolation-canary", "status": "unsupported", "reason": "requires macOS"}, indent=2))
        return 2
    if platform.machine() != "arm64":
        print(json.dumps({"fixture": "macos-isolation-canary", "status": "unsupported", "reason": f"requires arm64 host; got {platform.machine()}"}, indent=2))
        return 2
    tools = {name: shutil.which(name) for name in ("clang", "codesign", "otool")}
    missing = [name for name, path in tools.items() if not path]
    if missing:
        print(json.dumps({"fixture": "macos-isolation-canary", "status": "unsupported", "reason": "missing tools", "missing": missing}, indent=2))
        return 2

    report: dict[str, Any] = {
        "fixture": "macos-isolation-canary",
        "classification": "development measurement only; not a production backend",
        "host": {"macos": platform.mac_ver()[0], "architecture": platform.machine()},
        "mechanism": {
            "api": "sandbox_init",
            "support": "deprecated/unsupported; experimental custom SBPL profile",
            "flags": 0,
            "profile": (
                "allow default; deny file-read*/file-write* and unix-socket network-outbound beneath "
                "one canonical private scratch root, plus a literal file-read deny on the guard's own "
                "sentinel file"
            ),
            "gate": (
                "the constructor probes the sentinel after installing the policy and exits 94 unless the "
                "probe fails with EPERM/EACCES"
            ),
            "fallback": "none; constructor exits before main if guard installation fails",
        },
        "entitlements": ["com.apple.security.cs.disable-library-validation"],
        "results": [],
        "explicit_baseline": args.baseline,
    }
    with tempfile.TemporaryDirectory(prefix="orbit-canary-", dir="/private/tmp") as td:
        root = Path(td).resolve(strict=True)
        protected, allowed = root / "protected", root / "allowed"
        protected.mkdir(mode=0o700)
        allowed.mkdir(mode=0o700)
        for name, content in {
            "read.txt": "scratch-read-only\n",
            "descriptor.txt": "descriptor-before-guard\n",
            "unlink.txt": "unlink-canary\n",
            "rename-source.txt": "rename-canary\n",
            "write.txt": "write-canary\n",
        }.items():
            (protected / name).write_text(content)
            (protected / name).chmod(0o600)
        (allowed / "ordinary.txt").write_text("allowed-scratch-file\n")
        (allowed / "ordinary.txt").chmod(0o600)
        (allowed / "protected-read-link").symlink_to(protected / "read.txt")
        (allowed / "protected-write-link").symlink_to(protected / "write.txt")
        (protected / "rmdir-target").mkdir(mode=0o700)
        set_canary_xattr(protected / "read.txt", CANARY_XATTR_NAME, b"1")
        # A listening Unix-domain endpoint is also private and disposable.
        sockpath = protected / "unix-socket"
        listener = socket_listener(sockpath)
        tcp_listener = socket.socket()
        tcp_listener.bind(("127.0.0.1", 0))
        tcp_listener.listen(1)
        tcp_port = tcp_listener.getsockname()[1]
        allowed_dirfd = os.open(allowed, os.O_RDONLY)
        sentinel = root / "sentinel.txt"
        order_log = root / "constructor-order.log"
        profile_out = root / "profile.sbpl"
        firmlink_file = Path("/System/Volumes/Data") / str(protected).lstrip("/") / "read.txt"
        if not firmlink_file.exists():
            firmlink_file = None
        try:
            tmp_alias_file = Path("/tmp") / protected.relative_to("/private/tmp") / "read.txt"
        except ValueError:
            tmp_alias_file = None
        if tmp_alias_file is not None and not tmp_alias_file.exists():
            tmp_alias_file = None
        (root / "main.c").write_text(MAIN_C)
        (root / "guard.c").write_text(GUARD_C)
        (root / "early.c").write_text(EARLY_C)
        entitlements = root / "entitlements.plist"
        entitlements.write_text('''<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.security.cs.disable-library-validation</key><true/></dict></plist>
''')
        clang = tools["clang"] or "clang"
        codesign = tools["codesign"] or "codesign"
        dylib, early_dylib, executable = root / "libcanary_guard.dylib", root / "libcanary_early.dylib", root / "canary"
        compile_early = run([clang, "-arch", "arm64", "-dynamiclib", "-Wno-deprecated-declarations", "-Wl,-install_name,@rpath/libcanary_early.dylib", str(root / "early.c"), "-o", str(early_dylib)])
        compile_guard = run([clang, "-arch", "arm64", "-dynamiclib", "-Wno-deprecated-declarations", "-Wl,-install_name,@rpath/libcanary_guard.dylib", str(root / "guard.c"), "-L", str(root), "-lcanary_early", "-Wl,-rpath,@loader_path", "-o", str(dylib)]) if compile_early.returncode == 0 else None
        compile_main = run([clang, "-arch", "arm64", str(root / "main.c"), "-L", str(root), "-lcanary_guard", "-framework", "CoreFoundation", "-Wl,-rpath,@executable_path", "-o", str(executable)]) if compile_guard is not None and compile_guard.returncode == 0 else None
        if compile_early.returncode or compile_guard is None or compile_guard.returncode or compile_main is None or compile_main.returncode:
            report["status"] = "build_failed"
            report["build"] = {"early": compile_early.stderr, "guard": compile_guard.stderr if compile_guard else "not run", "main": compile_main.stderr if compile_main else "not run"}
            listener.close()
            print(json.dumps(report, indent=2))
            return 1
        load_commands = run([tools["otool"] or "otool", "-l", str(executable)]).stdout
        report["strong_dependency"] = any(
            "cmd LC_LOAD_DYLIB" in block and "@rpath/libcanary_guard.dylib" in block
            for block in load_commands.split("Load command ")
        )
        if not report["strong_dependency"]:
            report["status"] = "fixture_invalid"
            report["reason"] = "expected strong dylib load command was absent"
            listener.close()
            print(json.dumps(report, indent=2))
            return 1

        sign_early = run([codesign, "--force", "--sign", "-", "--options", "runtime", str(early_dylib)])
        sign_guard = run([codesign, "--force", "--sign", "-", "--options", "runtime", str(dylib)]) if sign_early.returncode == 0 else None
        sign_main = run([codesign, "--force", "--sign", "-", "--options", "runtime", "--entitlements", str(entitlements), str(executable)]) if sign_guard is not None and sign_guard.returncode == 0 else None
        verify_early = run([codesign, "--verify", "--strict", "--verbose=2", str(early_dylib)]) if sign_early.returncode == 0 else None
        verify_guard = run([codesign, "--verify", "--strict", "--verbose=2", str(dylib)]) if sign_guard is not None and sign_guard.returncode == 0 else None
        verify = run([codesign, "--verify", "--strict", "--verbose=2", str(executable)]) if sign_main is not None and sign_main.returncode == 0 else None
        report["signing"] = {
            "early": sign_early.returncode == 0,
            "guard": bool(sign_guard and sign_guard.returncode == 0),
            "main": bool(sign_main and sign_main.returncode == 0),
            "early_strict_verify": bool(verify_early and verify_early.returncode == 0),
            "guard_strict_verify": bool(verify_guard and verify_guard.returncode == 0),
            "main_strict_verify": bool(verify and verify.returncode == 0),
            "verify_output": verify.stderr if verify else "not run",
        }
        if sign_early.returncode or not sign_guard or sign_guard.returncode or not sign_main or sign_main.returncode or not verify_early or verify_early.returncode or not verify_guard or verify_guard.returncode or not verify or verify.returncode:
            report["status"] = "signing_failed"
            listener.close()
            print(json.dumps(report, indent=2))
            return 1

        token = uuid.uuid4().hex
        mach_a = f"dev.parsec-orbit.isolation-canary.{token}.a"
        mach_b = f"dev.parsec-orbit.isolation-canary.{token}.b"
        helper_source = root / "mach_helper.c"
        helper_source.write_text(MACH_HELPER_C)
        helper_binary = root / "mach_helper"
        compile_helper = run([clang, "-arch", "arm64", "-Wall", "-Werror", "-framework", "CoreFoundation", str(helper_source), "-o", str(helper_binary)])
        sign_helper = run([codesign, "--force", "--sign", "-", str(helper_binary)]) if compile_helper.returncode == 0 else None
        if compile_helper.returncode or sign_helper is None or sign_helper.returncode:
            report["status"] = "build_failed"
            report["build"] = {"helper": compile_helper.stderr}
            listener.close()
            print(json.dumps(report, indent=2))
            return 1
        helper = subprocess.Popen([str(helper_binary), mach_a, mach_b], text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        ready = helper.stdout.readline().strip() if helper.stdout else ""
        if ready != "READY":
            helper.terminate()
            helper.wait(timeout=5)
            report["status"] = "helper_failed"
            report["helper"] = {"stdout": ready, "stderr": helper.stderr.read() if helper.stderr else ""}
            listener.close()
            print(json.dumps(report, indent=2))
            return 1

        env = os.environ.copy()
        env.pop("CANARY_EXPLICIT_BASELINE", None)
        if args.baseline:
            env["CANARY_EXPLICIT_BASELINE"] = "1"
        env.update({
            "CANARY_PROTECTED": str(protected),
            "CANARY_ALLOWED": str(allowed),
            "CANARY_SOCKET": str(sockpath),
            "CANARY_SENTINEL": str(sentinel),
            "CANARY_ORDER_LOG": str(order_log),
            "CANARY_PROFILE_OUT": str(profile_out),
            "CANARY_ALLOWED_DIRFD": str(allowed_dirfd),
            "CANARY_TCP_PORT": str(tcp_port),
            "CANARY_XATTR_NAME": CANARY_XATTR_NAME,
            "CANARY_FIRMLINK_FILE": str(firmlink_file) if firmlink_file else "",
            "CANARY_TMP_ALIAS_FILE": str(tmp_alias_file) if tmp_alias_file else "",
            "CANARY_MACH_A": mach_a,
            "CANARY_MACH_B": mach_b,
        })
        if args.ipc == "allowlist":
            env["CANARY_IPC"] = f'(deny mach-lookup)\n(allow mach-lookup (global-name "{mach_a}"))\n'
        else:
            env.pop("CANARY_IPC", None)
        try:
            proc = subprocess.run([str(executable)], env=env, text=True, capture_output=True, timeout=30, pass_fds=(allowed_dirfd,))
        finally:
            helper.terminate()
            try:
                helper.wait(timeout=5)
            except subprocess.TimeoutExpired:
                helper.kill()
                helper.wait(timeout=5)
        report["ipc"] = {
            "variant": args.ipc,
            "mach_a": mach_a,
            "mach_b": mach_b,
            "clauses": env.get("CANARY_IPC", ""),
            "note": "mach probes report kern_return_t/SInt32 codes, not errno; compare against the control run",
        }
        report["mach_helper"] = {
            "returncode": helper.returncode,
            "stderr": helper.stderr.read() if helper.stderr else "",
        }
        listener.close()
        tcp_listener.close()
        os.close(allowed_dirfd)
        parsed = []
        for line in proc.stdout.splitlines():
            match = re.fullmatch(r"RESULT (\S+) (success|failure|skipped) (-?\d+)", line)
            if match:
                name, status, code = match.groups()
                parsed.append(record(name, status, errno=int(code) if status != "success" else None))
        report["process"] = {"exit_code": proc.returncode, "stdout": proc.stdout, "stderr": proc.stderr}
        report["results"] = parsed
        gate: dict[str, Any] = {}
        for line in proc.stderr.splitlines():
            match = re.fullmatch(r"CANARY_GATE sandbox_init_rc=(-?\d+) sentinel_open_errno=(-?\d+) gate_verified=(\d+)", line)
            if match:
                gate = {
                    "sandbox_init_rc": int(match.group(1)),
                    "sentinel_open_errno": int(match.group(2)),
                    "gate_verified": match.group(3) == "1",
                }
        report["sentinel_gate"] = gate
        if profile_out.exists():
            report["profile"] = profile_out.read_text()
        order_events = []
        if order_log.exists():
            for line in order_log.read_text().splitlines():
                parts = line.split()
                if len(parts) == 3:
                    order_events.append({"event": parts[0], "monotonic": parts[1], "value": int(parts[2])})
        report["constructor_order"] = {
            "events": order_events,
            "early_before_guard": [event["event"] for event in order_events[:2]]
            == ["early_constructor_pre_guard_open", "guard_constructor_begin"],
            "pre_guard_open_succeeded": any(
                event["event"] == "early_constructor_pre_guard_open" and event["value"] == 0
                for event in order_events
            ),
        }
        deny_errnos = {1, 13}  # EPERM and EACCES
        report["assessment"] = {
            "kernel_denials_observed": sum(item["status"] == "failure" and item.get("errno") in deny_errnos for item in parsed),
            "successful_operations": sum(item["status"] == "success" for item in parsed),
            "other_failures_or_skips": sum(item["status"] == "skipped" or
                item["status"] == "failure" and item.get("errno") not in deny_errnos for item in parsed),
            "note": "A failed call counts as a denial only when errno is EPERM or EACCES.",
        }
        report["protected_paths_canonical"] = True
        report["profile_order"] = "allow-default-before-specific-denies"
        expected_names = {
            "open", "openat_absolute", "openat_preopened_dirfd", "fopen", "stat", "lstat",
            "access", "opendir", "readdir", "unlink", "rename", "write_open", "write", "flock",
            "symlink_create", "open_through_symlink", "write_symlink_create", "write_symlink_open",
            "write_through_symlink", "flock_through_symlink", "preopened_fd_fstat",
            "preopened_fd_read", "preopened_fd_write", "preopened_fd_flock", "allowed_open",
            "allowed_write", "allowed_symlink", "allowed_open_through_symlink", "allowed_metadata",
            "allowed_opendir", "allowed_readdir", "unix_socket_create", "unix_socket_connect",
            "unix_socket_path_unlink", "unix_socket_bind_socket", "unix_socket_bind",
            "openat_relative_escape", "chdir_relative_escape", "open_nofollow_alias", "mkdir",
            "rmdir", "chmod", "truncate", "utimes", "hardlink_protected_to_allowed",
            "hardlink_allowed_to_protected", "listxattr", "getxattr", "statfs",
            "realpath_protected", "dir_open", "fchdir", "access_w", "fopen_append", "open_trunc",
            "allowed_read_post_policy", "allowed_read_bytes", "dlopen_system_framework",
            "tcp_socket", "tcp_loopback_connect", "firmlink_open", "tmp_alias_open",
            "mach_lookup_a", "mach_send_a", "mach_lookup_b", "mach_lookup_system",
        }
        observed_names = {item["name"] for item in parsed}
        report["expected_probe_count"] = len(expected_names)
        report["all_probe_results_present"] = len(parsed) == len(expected_names) and observed_names == expected_names
        report["missing_probes"] = sorted(expected_names - observed_names)
        report["unexpected_probes"] = sorted(observed_names - expected_names)
        report["keychain_and_mach_ipc"] = (
            "Mach and unix-socket rules measured with owned endpoints in this fixture; "
            "Keychain remains unmeasured and no real service is contacted"
        )
        if proc.returncode != 0 or not report["all_probe_results_present"]:
            report["status"] = "unmeasured_incomplete_run"
            report["reason"] = "process did not exit normally with every probe result; no fallback attempted"
        elif args.baseline and "CANARY_BASELINE_UNGUARDED" in proc.stderr:
            report["status"] = "baseline-measured"
        elif "CANARY_GUARD_INSTALLED" in proc.stderr:
            report["status"] = "measured"
        elif "CANARY_GUARD_ERROR" in proc.stderr or proc.returncode == 93:
            report["status"] = "guard_install_failed"
            report["reason"] = "sandbox_init returned failure; the C main was not allowed to run"
        elif proc.returncode == 94:
            report["status"] = "sentinel_gate_failed"
            report["reason"] = "the installed policy did not deny the owned sentinel; client execution stayed closed"
        elif proc.returncode != 0 or not parsed:
            report["status"] = "unmeasured_early_termination"
            report["reason"] = "process ended before a guard-installed marker and complete result output; no fallback was attempted"
        else:
            report["status"] = "unmeasured"
            report["reason"] = "guard installation could not be confirmed"
        print(json.dumps(report, indent=2))
        return 0 if report["status"] in ("measured", "baseline-measured") else 1


def socket_listener(path: Path):
    import socket

    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(path))
    listener.listen(1)
    listener.settimeout(0.2)
    return listener


if __name__ == "__main__":
    raise SystemExit(main())

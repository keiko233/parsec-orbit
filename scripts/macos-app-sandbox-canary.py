#!/usr/bin/env python3
"""Owned App Sandbox canary, not an installer or a usable Parsec client.

Every explicit probe uses private scratch canaries. No Keychain APIs or vendor
code run. macOS may create the new fixture's own uniquely named app container;
this script never reads, removes or reuses any existing app container.
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import platform
import socket
import subprocess
import sys
import tempfile
import time
import uuid

SOURCE = r'''
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/file.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <dirent.h>
#include <sys/mount.h>
#include <sys/xattr.h>
#include <limits.h>
#include <spawn.h>
#include <sys/wait.h>
#include <dlfcn.h>
#include <CoreFoundation/CoreFoundation.h>
extern char **environ;
static void result(const char *name, int ok, int error) {
    printf("%s %d %d\n", name, ok, ok ? 0 : error); fflush(stdout);
}
static void result_unmeasured(const char *name) {
    printf("%s U 0\n", name); fflush(stdout);
}
static void open_probe(const char *name, const char *path) {
    if (!path || !*path) { result_unmeasured(name); return; }
    errno = 0; int fd = open(path, O_RDONLY); int error = errno;
    result(name, fd >= 0, error); if (fd >= 0) close(fd);
}
static int run_cat(const char *path, int *exit_out, char *errbuf, size_t errbuf_len, size_t *err_len_out) {
    if (!path || !*path) return -1;
    int out_pipe[2] = {-1,-1}, err_pipe[2] = {-1,-1};
    if (pipe(out_pipe) != 0) return -1;
    if (pipe(err_pipe) != 0) { close(out_pipe[0]); close(out_pipe[1]); return -1; }
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_adddup2(&fa, out_pipe[1], STDOUT_FILENO);
    posix_spawn_file_actions_adddup2(&fa, err_pipe[1], STDERR_FILENO);
    posix_spawn_file_actions_addclose(&fa, out_pipe[0]);
    posix_spawn_file_actions_addclose(&fa, err_pipe[0]);
    char *const argv[] = {(char*)"/bin/cat", (char*)path, NULL};
    pid_t pid = 0;
    int sprc = posix_spawnp(&pid, "/bin/cat", &fa, NULL, argv, environ);
    posix_spawn_file_actions_destroy(&fa);
    close(out_pipe[1]); close(err_pipe[1]);
    if (sprc != 0) { close(out_pipe[0]); close(err_pipe[0]); errno = sprc; return -2; }
    char tmp[512];
    ssize_t n;
    size_t err_len = 0;
    errbuf[0] = '\0';
    while ((n = read(out_pipe[0], tmp, sizeof(tmp))) > 0) {}
    close(out_pipe[0]);
    while ((n = read(err_pipe[0], tmp, sizeof(tmp)-1)) > 0) {
        if (err_len < errbuf_len-1) {
            size_t copy = (size_t)n;
            if (err_len + copy > errbuf_len-1) copy = errbuf_len-1-err_len;
            memcpy(errbuf+err_len, tmp, copy);
            err_len += copy;
            errbuf[err_len] = '\0';
        }
    }
    close(err_pipe[0]);
    if (err_len_out) *err_len_out = err_len;
    int status = 0;
    while (waitpid(pid, &status, 0) < 0) { if (errno != EINTR) break; }
    if (WIFEXITED(status)) *exit_out = WEXITSTATUS(status);
    else if (WIFSIGNALED(status)) *exit_out = 128 + WTERMSIG(status);
    else *exit_out = 999;
    return 0;
}
__attribute__((constructor)) static void early(void) {
    open_probe("constructor_protected_open", getenv("AS_PROTECTED_FILE"));
}
int main(void) {
    const char *mode = getenv("AS_MODE");
    const char *blocked = getenv("AS_PROTECTED_FILE"), *allowed = getenv("AS_ALLOWED_FILE");
    const char *blocked_dir = getenv("AS_PROTECTED_DIR"), *allowed_dir = getenv("AS_ALLOWED_DIR");
    if (mode && strcmp(mode, "self-exec") == 0) {
        open_probe("selfexec_child_protected_open", blocked);
        open_probe("selfexec_child_allowed_open", allowed);
        return 0;
    }
    open_probe("protected_open", blocked); open_probe("allowed_open", allowed);
    open_probe("allowed_alias_to_protected", getenv("AS_ALIAS"));
    errno = 0; struct stat st; int rc = stat(blocked, &st); int error = errno;
    result("protected_stat", rc == 0, error);
    errno = 0; int fd = open(allowed, O_WRONLY | O_APPEND); error = errno;
    result("allowed_write_open", fd >= 0, error);
    if (fd >= 0) { errno = 0; int n = write(fd, "W", 1); error = errno;
        result("allowed_write", n == 1, error); close(fd); }
    const char *inh_s = getenv("AS_INHERITED_FD");
    fd = (inh_s && *inh_s) ? atoi(inh_s) : -1;
    if (fd >= 0) {
    errno = 0; char byte; int n = pread(fd, &byte, 1, 0); error = errno;
    result("inherited_protected_read", n == 1, error);
    errno = 0; n = pwrite(fd, "X", 1, 0); error = errno;
    result("inherited_protected_write", n == 1, error);
    errno = 0; rc = flock(fd, LOCK_EX | LOCK_NB); error = errno;
    result("inherited_protected_lock", rc == 0, error); if (!rc) flock(fd, LOCK_UN);
    } else { result_unmeasured("inherited_protected_read"); result_unmeasured("inherited_protected_write"); result_unmeasured("inherited_protected_lock"); }
    errno = 0; int s = socket(AF_UNIX, SOCK_STREAM, 0); error = errno;
    result("unix_socket_create", s >= 0, error);
    if (s >= 0) {
        struct sockaddr_un addr = {0}; addr.sun_family = AF_UNIX;
        const char *sockpath = getenv("AS_SOCKET");
        snprintf(addr.sun_path, sizeof(addr.sun_path), "%s", sockpath ? sockpath : "");
        errno = 0; rc = connect(s, (struct sockaddr *)&addr, sizeof(addr)); error = errno;
        result("unix_protected_connect", rc == 0, error); close(s);
    } else { result_unmeasured("unix_protected_connect"); }
    if (blocked && *blocked) {
        errno = 0; struct stat lst; rc = lstat(blocked, &lst); error = errno;
        result("metadata_lstat_protected", rc == 0, error);
        errno = 0; rc = access(blocked, R_OK); error = errno;
        result("metadata_access_r_protected", rc == 0, error);
        errno = 0; rc = access(blocked, W_OK); error = errno;
        result("metadata_access_w_protected", rc == 0, error);
        errno = 0; rc = access(blocked, F_OK); error = errno;
        result("metadata_access_f_protected", rc == 0, error);
    } else { result_unmeasured("metadata_lstat_protected"); result_unmeasured("metadata_access_r_protected"); result_unmeasured("metadata_access_w_protected"); result_unmeasured("metadata_access_f_protected"); }
    const char *alias = getenv("AS_ALIAS");
    if (alias && *alias) {
        char linkbuf[PATH_MAX]; errno = 0; ssize_t lr = readlink(alias, linkbuf, sizeof(linkbuf)-1); error = errno;
        if (lr >= 0) { linkbuf[lr] = '\0'; result("metadata_readlink_alias", 1, 0); printf("raw metadata_readlink_alias target=%s\n", linkbuf); fflush(stdout); }
        else result("metadata_readlink_alias", 0, error);
        errno = 0; int nfd = open(alias, O_RDONLY | O_NOFOLLOW); error = errno;
        result("metadata_open_nofollow_alias", nfd >= 0, error); if (nfd >= 0) close(nfd);
    } else { result_unmeasured("metadata_readlink_alias"); result_unmeasured("metadata_open_nofollow_alias"); }
    if (blocked_dir && *blocked_dir) {
        errno = 0; DIR *d = opendir(blocked_dir); error = errno;
        result("metadata_opendir_protected", d != NULL, error);
        if (d) { errno = 0; struct dirent *e = readdir(d); error = errno;
            if (e) result("metadata_readdir_protected", 1, 0);
            else result("metadata_readdir_protected", 0, error);
            closedir(d);
        } else result_unmeasured("metadata_readdir_protected");
    } else { result_unmeasured("metadata_opendir_protected"); result_unmeasured("metadata_readdir_protected"); }
    if (blocked && *blocked) {
        errno = 0; struct statfs vfs; rc = statfs(blocked, &vfs); error = errno;
        result("metadata_statfs_protected", rc == 0, error);
        errno = 0; ssize_t xr = listxattr(blocked, NULL, 0, 0); error = errno;
        result("metadata_listxattr_protected", xr >= 0, error);
        const char *xattr_name = getenv("AS_XATTR_NAME");
        if (!xattr_name || !*xattr_name) xattr_name = "com.apple.quarantine";
        errno = 0; ssize_t gr = getxattr(blocked, xattr_name, NULL, 0, 0, 0); error = errno;
        result("metadata_getxattr_protected", gr >= 0, error);
        errno = 0; char resolved[PATH_MAX]; const char *rp = realpath(blocked, resolved); error = errno;
        result("metadata_realpath_protected", rp != NULL, error);
        if (rp) { printf("raw metadata_realpath_protected resolved=%s\n", resolved); fflush(stdout); }
    } else { result_unmeasured("metadata_statfs_protected"); result_unmeasured("metadata_listxattr_protected"); result_unmeasured("metadata_getxattr_protected"); result_unmeasured("metadata_realpath_protected"); }
    const char *adirfd_s = getenv("AS_ALLOWED_DIRFD");
    if (adirfd_s && *adirfd_s) {
        int adfd = atoi(adirfd_s);
        errno = 0; int efd = openat(adfd, "../protected/data", O_RDONLY); error = errno;
        result("escape_openat_dotdot", efd >= 0, error); if (efd >= 0) close(efd);
    } else result_unmeasured("escape_openat_dotdot");
    if (allowed_dir && *allowed_dir) {
        char cwd[PATH_MAX] = {0};
        int has_old = (getcwd(cwd, sizeof(cwd)) != NULL);
        errno = 0; int chr = chdir(allowed_dir); int cherr = errno;
        if (chr == 0) {
            errno = 0; int rfd = open("../protected/data", O_RDONLY); error = errno;
            result("escape_chdir_relative_open", rfd >= 0, error); if (rfd >= 0) close(rfd);
            if (has_old) chdir(cwd);
            else chdir("/private/tmp");
            (void)cherr;
        } else result("escape_chdir_relative_open", 0, cherr);
    } else result_unmeasured("escape_chdir_relative_open");
    if (allowed_dir && blocked && *allowed_dir && *blocked) {
        char linkpath[PATH_MAX]; snprintf(linkpath, sizeof(linkpath), "%s/new-link", allowed_dir);
        char srcpath[PATH_MAX]; snprintf(srcpath, sizeof(srcpath), "%s/data", allowed_dir);
        char dstpath[PATH_MAX]; snprintf(dstpath, sizeof(dstpath), "%s/new-link", blocked_dir ? blocked_dir : allowed_dir);
        errno = 0; rc = link(srcpath, dstpath); error = errno;
        result("escape_hardlink_allowed_to_protected", rc == 0, error);
        errno = 0; rc = link(blocked, linkpath); error = errno;
        result("escape_hardlink_protected_to_allowed", rc == 0, error);
        if (rc == 0) unlink(linkpath);
    } else { result_unmeasured("escape_hardlink_allowed_to_protected"); result_unmeasured("escape_hardlink_protected_to_allowed"); }
    const char *alias_avail = getenv("AS_ALIAS_AVAILABLE");
    const char *alias_prot = getenv("AS_ALIAS_PROTECTED");
    const char *alias_allow = getenv("AS_ALIAS_ALLOWED");
    if (alias_avail && strcmp(alias_avail, "1") == 0 && alias_prot && *alias_prot) open_probe("alias_protected_open", alias_prot);
    else result_unmeasured("alias_protected_open");
    if (alias_avail && strcmp(alias_avail, "1") == 0 && alias_allow && *alias_allow) open_probe("alias_allowed_open", alias_allow);
    else result_unmeasured("alias_allowed_open");
    {
        int ex = 0; char ebuf[1024]; size_t elen = 0;
        int pr = run_cat(blocked, &ex, ebuf, sizeof(ebuf), &elen);
        if (pr == 0) { result("child_cat_protected", ex == 0, ex); printf("raw child_cat_protected exit=%d stderr_bytes=%lu\n", ex, (unsigned long)elen); fflush(stdout); fprintf(stderr, "child_cat_protected exit=%d stderr=%s\n", ex, ebuf); }
        else if (pr == -2) { result("child_cat_protected", 0, errno); fprintf(stderr, "child_cat_protected spawn failed errno=%d\n", errno); }
        else result_unmeasured("child_cat_protected");
        pr = run_cat(allowed, &ex, ebuf, sizeof(ebuf), &elen);
        if (pr == 0) { result("child_cat_allowed", ex == 0, ex); printf("raw child_cat_allowed exit=%d stderr_bytes=%lu\n", ex, (unsigned long)elen); fflush(stdout); fprintf(stderr, "child_cat_allowed exit=%d stderr=%s\n", ex, ebuf); }
        else if (pr == -2) { result("child_cat_allowed", 0, errno); fprintf(stderr, "child_cat_allowed spawn failed errno=%d\n", errno); }
        else result_unmeasured("child_cat_allowed");
    }
    {
        const char *self = getenv("AS_SELF");
        if (self && *self) {
            int outp[2] = {-1,-1}, errp[2] = {-1,-1};
            int ok_pipes = (pipe(outp) == 0 && pipe(errp) == 0);
            if (!ok_pipes) { result_unmeasured("selfexec_spawn"); result_unmeasured("selfexec_protected_open"); if (outp[0]>=0) close(outp[0]); if (outp[1]>=0) close(outp[1]); if (errp[0]>=0) close(errp[0]); if (errp[1]>=0) close(errp[1]); }
            else {
                posix_spawn_file_actions_t fa; posix_spawn_file_actions_init(&fa);
                posix_spawn_file_actions_adddup2(&fa, outp[1], STDOUT_FILENO);
                posix_spawn_file_actions_adddup2(&fa, errp[1], STDERR_FILENO);
                posix_spawn_file_actions_addclose(&fa, outp[0]);
                posix_spawn_file_actions_addclose(&fa, errp[0]);
                size_t nenv = 0; while (environ[nenv]) nenv++;
                char **nenv_p = calloc(nenv+2, sizeof(char*));
                for (size_t i=0;i<nenv;i++) nenv_p[i]=environ[i];
                nenv_p[nenv]=(char*)"AS_MODE=self-exec"; nenv_p[nenv+1]=NULL;
                char *const argv[] = {(char*)self, NULL};
                pid_t pid=0; errno=0; int sprc = posix_spawn(&pid, self, &fa, NULL, argv, nenv_p); int sperr = sprc ? sprc : 0;
                posix_spawn_file_actions_destroy(&fa);
                close(outp[1]); close(errp[1]);
                free(nenv_p);
                if (sprc != 0) { result("selfexec_spawn", 0, sperr); result_unmeasured("selfexec_protected_open"); close(outp[0]); close(errp[0]); }
                else {
                    char tmp[512]; ssize_t nn;
                    char child_out[8192]; size_t child_len=0;
                    child_out[0]='\0';
                    while ((nn = read(outp[0], tmp, sizeof(tmp))) > 0) { if (child_len+(size_t)nn < sizeof(child_out)-1) { memcpy(child_out+child_len, tmp, (size_t)nn); child_len+=(size_t)nn; child_out[child_len]='\0'; } }
                    close(outp[0]);
                    while ((nn = read(errp[0], tmp, sizeof(tmp))) > 0) {}
                    close(errp[0]);
                    int status=0; while (waitpid(pid,&status,0)<0) { if (errno!=EINTR) break; }
                    int chexit = WIFEXITED(status)?WEXITSTATUS(status):999;
                    result("selfexec_spawn", (chexit==0), chexit);
                    printf("raw selfexec_child_output begin\n"); fflush(stdout);
                    char *save = NULL;
                    char *copy = child_out;
                    char linebuf[1024];
                    size_t pos = 0;
                    for (size_t i=0;i<=child_len;i++) {
                        char c = child_out[i];
                        if (c=='\n' || c=='\0') { linebuf[pos]='\0'; printf("raw selfexec-child %s\n", linebuf); pos=0; if (c=='\0') break; }
                        else if (pos < sizeof(linebuf)-1) linebuf[pos++]=c;
                    }
                    (void)save; (void)copy;
                    printf("raw selfexec_child_output end\n"); fflush(stdout);
                    int found=0, cok=0, cerr=0;
                    char *tok = strtok(child_out, "\n");
                    while (tok) { char nm[128]; int ok=0,er=0; if (sscanf(tok,"%127s %d %d",nm,&ok,&er)==3 && strcmp(nm,"selfexec_child_protected_open")==0) { found=1; cok=ok; cerr=er; break; } tok=strtok(NULL,"\n"); }
                    if (found) result("selfexec_protected_open", cok, cerr);
                    else result_unmeasured("selfexec_protected_open");
                }
            }
        } else { result_unmeasured("selfexec_spawn"); result_unmeasured("selfexec_protected_open"); }
    }
    {
        const char *res = getenv("AS_RESOURCES");
        if (res && *res) {
            char ctl[PATH_MAX]; snprintf(ctl,sizeof(ctl),"%s/control.txt",res);
            open_probe("bundle_read_control", ctl);
            char nw[PATH_MAX]; snprintf(nw,sizeof(nw),"%s/write-test",res);
            errno=0; int wfd=open(nw,O_CREAT|O_WRONLY,0600); error=errno;
            result("bundle_write_new", wfd>=0, error); if (wfd>=0){close(wfd); unlink(nw);}
        } else { result_unmeasured("bundle_read_control"); result_unmeasured("bundle_write_new"); }
    }
    {
        void *sym = dlsym(RTLD_DEFAULT, "sandbox_check");
        if (!sym) { result("sandbox_check_available", 0, 0); result_unmeasured("sandbox_check_matches"); printf("raw sandbox_check symbol missing\n"); fflush(stdout); }
        else {
            result("sandbox_check_available", 1, 0);
            const char *ops[] = {"file-read-data","file-read-metadata","file-write-data"};
            const char *files[2] = {blocked, allowed};
            const char *flabel[2] = {"protected","allowed"};
            int (*fn)(pid_t, const char*, int, ...) = (int(*)(pid_t,const char*,int,...))sym;
            printf("raw sandbox_check table begin (rc=return; success semantics undocumented, do not substitute)\n");
            for (int t=0;t<=8;t++) for (int oi=0;oi<3;oi++) for (int fi=0;fi<2;fi++) {
                const char *p = files[fi] ? files[fi] : "";
                errno=0; int rcc = fn(getpid(), ops[oi], t, p); int e=errno;
                printf("raw sandbox_check op=%s file=%s type=%d rc=%d errno=%d\n", ops[oi], flabel[fi], t, rcc, e);
            }
            printf("raw sandbox_check table end\n"); fflush(stdout);
            int any_match_frd = 0;
            for (int t=0;t<=8;t++) {
                errno=0; int rcp = fn(getpid(), "file-read-data", t, blocked?blocked:"");
                errno=0; int rca = fn(getpid(), "file-read-data", t, allowed?allowed:"");
                if (rcp != 0 && rca == 0) { any_match_frd = 1; printf("raw sandbox_check file-read-data type=%d matches enforcement (protected denied, allowed allowed)\n", t); fflush(stdout); }
            }
            if (!any_match_frd) printf("raw sandbox_check file-read-data no type matched enforcement in this best-effort pass\n");
            fflush(stdout);
            result_unmeasured("sandbox_check_matches");
            printf("raw sandbox_check note: success semantics undocumented; raw table above is evidence only\n"); fflush(stdout);
        }
    }
    {
        const char *mname = getenv("AS_MACH_NAME");
        if (!mname || !*mname) { result_unmeasured("mach_lookup"); result_unmeasured("mach_send"); }
        else {
            CFStringRef ns = CFStringCreateWithCString(NULL, mname, kCFStringEncodingUTF8);
            if (!ns) { result_unmeasured("mach_lookup"); result_unmeasured("mach_send"); }
            else {
                CFMessagePortRef remote = CFMessagePortCreateRemote(NULL, ns);
                result("mach_lookup", remote != NULL, remote ? 0 : 1);
                if (!remote) { result_unmeasured("mach_send"); printf("raw mach lookup failed name=%s\n", mname); fflush(stdout); }
                else {
                    CFDataRef req = CFDataCreate(NULL, (const UInt8*)"ping", 4);
                    CFDataRef resp = NULL;
                    SInt32 sr = CFMessagePortSendRequest(remote, 1, req, 1.0, 1.0, kCFRunLoopDefaultMode, &resp);
                    int sr_int = (int)sr;
                    int ok = (sr == 0);
                    int errfield = sr_int >= 0 ? sr_int : -sr_int + 100000;
                    result("mach_send", ok, errfield);
                    printf("raw mach send sr=%d errfield=%d\n", sr_int, errfield); fflush(stdout);
                    if (req) CFRelease(req);
                    if (resp) CFRelease(resp);
                    CFRelease(remote);
                }
                CFRelease(ns);
            }
        }
    }
    return 0;
}
'''

MACH_HELPER_SOURCE = r'''
#include <CoreFoundation/CoreFoundation.h>
#include <stdio.h>
#include <stdlib.h>
static CFDataRef callback(CFMessagePortRef local, SInt32 msgid, CFDataRef data, void *info) {
    (void)local; (void)info;
    long len = data ? (long)CFDataGetLength(data) : -1;
    printf("helper received msgid=%d len=%ld\n", (int)msgid, len); fflush(stdout);
    const char pong[4] = {'p','o','n','g'};
    return CFDataCreate(NULL, (const UInt8*)pong, 4);
}
int main(void) {
    const char *name = getenv("MACH_HELPER_NAME");
    if (!name || !*name) { fprintf(stderr, "missing MACH_HELPER_NAME\n"); return 2; }
    CFStringRef ns = CFStringCreateWithCString(NULL, name, kCFStringEncodingUTF8);
    if (!ns) { fprintf(stderr, "CFString failed\n"); return 2; }
    CFMessagePortRef local = CFMessagePortCreateLocal(NULL, ns, callback, NULL, NULL);
    CFRelease(ns);
    if (!local) { fprintf(stderr, "CFMessagePortCreateLocal failed\n"); return 1; }
    CFRunLoopSourceRef src = CFMessagePortCreateRunLoopSource(NULL, local, 0);
    if (!src) { fprintf(stderr, "runloop source failed\n"); CFRelease(local); return 1; }
    CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopDefaultMode);
    printf("ready %s\n", name); fflush(stdout);
    CFRunLoopRunInMode(kCFRunLoopDefaultMode, 20.0, false);
    printf("helper done\n"); fflush(stdout);
    CFRelease(src); CFRelease(local);
    return 0;
}
'''

EXPECTED_OBSERVATIONS = 40

EXPECTED_NAMES = [
    "constructor_protected_open", "protected_open", "allowed_open",
    "allowed_alias_to_protected", "protected_stat", "allowed_write_open",
    "allowed_write", "inherited_protected_read", "inherited_protected_write",
    "inherited_protected_lock", "unix_socket_create", "unix_protected_connect",
    "metadata_lstat_protected", "metadata_access_r_protected",
    "metadata_access_w_protected", "metadata_access_f_protected",
    "metadata_readlink_alias", "metadata_open_nofollow_alias",
    "metadata_opendir_protected", "metadata_readdir_protected",
    "metadata_statfs_protected", "metadata_listxattr_protected",
    "metadata_getxattr_protected", "metadata_realpath_protected",
    "escape_openat_dotdot", "escape_chdir_relative_open",
    "escape_hardlink_allowed_to_protected", "escape_hardlink_protected_to_allowed",
    "alias_protected_open", "alias_allowed_open",
    "child_cat_protected", "child_cat_allowed",
    "selfexec_spawn", "selfexec_protected_open",
    "bundle_read_control", "bundle_write_new",
    "sandbox_check_available", "sandbox_check_matches",
    "mach_lookup", "mach_send",
]


def command(args):
    subprocess.run(args, check=True, capture_output=True, text=True, timeout=30)


def set_canary_xattr(path, name, value):
    """Attach one owned xattr so the client probe can distinguish permit from absence."""
    import ctypes
    import ctypes.util

    libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
    fn = libc.setxattr
    fn.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p,
                   ctypes.c_size_t, ctypes.c_uint32, ctypes.c_int]
    return fn(str(path).encode(), name.encode(), value, len(value), 0, 0) == 0


def parse_observations(stdout):
    observations = []
    for line in stdout.splitlines():
        fields = line.split()
        if len(fields) == 3 and fields[1] in ("0", "1") and fields[2].lstrip("-").isdigit():
            observations.append({"name": fields[0], "success": fields[1] == "1", "errno": int(fields[2])})
        elif len(fields) == 3 and fields[1] == "U" and fields[2].lstrip("-").isdigit():
            observations.append({"name": fields[0], "success": None, "errno": int(fields[2]), "unmeasured": True})
    return observations


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="run the explicitly owned canary after staging")
    args = parser.parse_args()
    if sys.platform != "darwin" or platform.machine() != "arm64" or os.geteuid() == 0:
        raise ValueError("requires an ordinary-user macOS arm64 host")
    root = Path(tempfile.mkdtemp(prefix="orbit-as-", dir="/private/tmp")).resolve(strict=True)
    os.chmod(root, 0o700)
    allowed, protected = root / "allowed", root / "protected"
    allowed.mkdir(mode=0o700); protected.mkdir(mode=0o700)
    for path in (allowed / "data", protected / "data"):
        path.write_text("scratch canary\n"); path.chmod(0o600)
    (allowed / "protected-alias").symlink_to(protected / "data")
    xattr_name = "com.parsec-orbit.canary"
    xattr_prepared = set_canary_xattr(protected / "data", xattr_name, b"1")
    bundle_id = "dev.parsec-orbit.app-sandbox-canary." + uuid.uuid4().hex
    contents = root / "AppSandboxCanary.app/Contents"
    (contents / "MacOS").mkdir(parents=True, mode=0o700)
    resources = contents / "Resources"
    resources.mkdir(mode=0o700, exist_ok=True)
    (resources / "control.txt").write_text("owned control\n")
    os.chmod(resources / "control.txt", 0o644)
    executable = contents / "MacOS/canary"
    info = {"CFBundleIdentifier": bundle_id, "CFBundleExecutable": "canary",
        "CFBundlePackageType": "APPL", "CFBundleName": "OrbitAppSandboxCanary",
        "CFBundleVersion": "1"}
    (contents / "Info.plist").write_bytes(plistlib.dumps(info))
    entitlements = {"com.apple.security.app-sandbox": True,
        "com.apple.security.cs.disable-library-validation": True,
        "com.apple.security.network.client": True,
        "com.apple.security.temporary-exception.files.absolute-path.read-write": [str(allowed) + "/"]}
    entitlements_file = root / "entitlements.plist"
    entitlements_file.write_bytes(plistlib.dumps(entitlements))
    source = root / "canary.c"; source.write_text(SOURCE)
    command(["clang", "-arch", "arm64", "-Wall", "-Werror", str(source), "-o", str(executable), "-framework", "CoreFoundation"])
    command(["codesign", "--force", "--sign", "-", "--options", "runtime", "--entitlements",
        str(entitlements_file), str(contents.parent)])
    command(["codesign", "--verify", "--strict", str(contents.parent)])
    report = {"scope": "owned App Sandbox canary only; no vendor or account state",
        "host_macos": platform.mac_ver()[0], "scratch": str(root), "bundle_id": bundle_id,
        "entitlements": entitlements, "strict_signature_verified": True,
        "xattr_prepared": xattr_prepared, "xattr_name": xattr_name if xattr_prepared else "",
        "keychain_and_mach_ipc": "keychain unmeasured, no Keychain APIs; mach best-effort CFMessagePort probe, see results",
        "possible_os_owned_container": "~/Library/Containers/" + bundle_id,
        "inherited_fd_note": "inherited-fd probes (inherited_protected_*) use a harness-provided fd passed via pass_fds that cannot occur in a real Finder launch; they describe descriptor reuse, not launch-time isolation",
        "expected_observations": EXPECTED_OBSERVATIONS,
        "status": "staged", "complete_isolation_proven": False}
    if args.run:
        alias_root = "/System/Volumes/Data" + str(root)
        alias_available = False
        try:
            if os.path.isdir(alias_root):
                rp = os.path.realpath(alias_root)
                if rp == alias_root or os.stat(alias_root).st_ino == os.stat(str(root)).st_ino:
                    alias_available = True
        except OSError:
            alias_available = False
        report["alias_root"] = alias_root
        report["alias_available"] = alias_available
        mach_name = bundle_id + ".probe"
        helper_path = root / "mach_helper"
        helper_proc = None
        mach_helper_error = None
        mach_helper_stdout = ""
        mach_helper_stderr = ""
        try:
            (root / "mach_helper.c").write_text(MACH_HELPER_SOURCE)
            command(["clang", "-arch", "arm64", "-Wall", "-Werror", str(root / "mach_helper.c"), "-o", str(helper_path), "-framework", "CoreFoundation"])
            command(["codesign", "--force", "--sign", "-", str(helper_path)])
        except Exception as exc:
            mach_helper_error = "helper build/sign failed: %r" % (exc,)
            helper_path = None
        if helper_path is not None:
            try:
                helper_env = os.environ.copy()
                helper_env["MACH_HELPER_NAME"] = mach_name
                helper_proc = subprocess.Popen([str(helper_path)], env=helper_env,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                time.sleep(0.6)
                if helper_proc.poll() is not None:
                    try:
                        out, err = helper_proc.communicate(timeout=2)
                    except Exception:
                        out, err = "", ""
                    mach_helper_stdout = out
                    mach_helper_stderr = err
                    mach_helper_error = "helper exited early rc=%s stdout=%r stderr=%r" % (helper_proc.returncode, out[:500], err[:500])
                    helper_proc = None
            except Exception as exc:
                mach_helper_error = "helper launch failed: %r" % (exc,)
                helper_proc = None
        env = os.environ.copy()
        env.update(AS_PROTECTED_FILE=str(protected / "data"), AS_ALLOWED_FILE=str(allowed / "data"),
            AS_ALIAS=str(allowed / "protected-alias"), AS_SOCKET=str(protected / "socket"),
            AS_PROTECTED_DIR=str(protected), AS_ALLOWED_DIR=str(allowed),
            AS_RESOURCES=str(resources), AS_SELF=str(executable))
        env["AS_XATTR_NAME"] = xattr_name if xattr_prepared else ""
        env.pop("AS_MODE", None)
        if alias_available:
            env.update(AS_ALIAS_AVAILABLE="1", AS_ALIAS_PROTECTED=alias_root + "/protected/data",
                AS_ALIAS_ALLOWED=alias_root + "/allowed/data")
        else:
            env.update(AS_ALIAS_AVAILABLE="0", AS_ALIAS_PROTECTED="", AS_ALIAS_ALLOWED="")
        if helper_proc is not None:
            env["AS_MACH_NAME"] = mach_name
        else:
            env["AS_MACH_NAME"] = ""
            if mach_helper_error is None:
                mach_helper_error = "helper unavailable; mach probes unmeasured"
        report["mach_name"] = mach_name
        if mach_helper_error is not None:
            report["mach_helper_error"] = mach_helper_error
        allowed_dir_fd = None
        try:
            allowed_dir_fd = os.open(str(allowed), os.O_RDONLY)
        except OSError as exc:
            report["allowed_dirfd_error"] = "%r" % (exc,)
        if allowed_dir_fd is not None:
            env["AS_ALLOWED_DIRFD"] = str(allowed_dir_fd)
        else:
            env["AS_ALLOWED_DIRFD"] = ""
        try:
            with (protected / "data").open("r+b") as inherited, socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
                listener.bind(str(protected / "socket")); listener.listen(1)
                env["AS_INHERITED_FD"] = str(inherited.fileno())
                pass_fds = (inherited.fileno(),)
                if allowed_dir_fd is not None:
                    pass_fds = (inherited.fileno(), allowed_dir_fd)
                try:
                    result = subprocess.run([str(executable)], env=env, pass_fds=pass_fds,
                        capture_output=True, text=True, timeout=20)
                    observations = parse_observations(result.stdout)
                    report.update(exit=result.returncode, stdout=result.stdout, stderr=result.stderr, results=observations)
                    names = [o["name"] for o in observations]
                    complete = (result.returncode == 0 and len(observations) == EXPECTED_OBSERVATIONS
                        and set(names) == set(EXPECTED_NAMES))
                    report["status"] = "measured" if complete else "incomplete"
                    if not complete:
                        report["completeness"] = {"expected": EXPECTED_OBSERVATIONS, "observed": len(observations),
                            "missing": sorted(set(EXPECTED_NAMES) - set(names)),
                            "extra": sorted(set(names) - set(EXPECTED_NAMES))}
                except subprocess.TimeoutExpired:
                    report.update(status="timed-out", error="owned canary did not finish; subprocess stopped only its direct child")
        finally:
            if allowed_dir_fd is not None:
                try:
                    os.close(allowed_dir_fd)
                except OSError:
                    pass
            if helper_proc is not None:
                try:
                    helper_proc.terminate()
                    try:
                        out, err = helper_proc.communicate(timeout=3)
                    except subprocess.TimeoutExpired:
                        helper_proc.kill()
                        out, err = helper_proc.communicate(timeout=3)
                    mach_helper_stdout += out
                    mach_helper_stderr += err
                except Exception as exc:
                    report["mach_helper_cleanup_error"] = "%r" % (exc,)
                report["mach_helper_stdout"] = mach_helper_stdout[-4000:]
                report["mach_helper_stderr"] = mach_helper_stderr[-4000:]
                report["mach_helper_returncode"] = helper_proc.returncode
            else:
                if mach_helper_stdout or mach_helper_stderr:
                    report["mach_helper_stdout"] = mach_helper_stdout[-4000:]
                    report["mach_helper_stderr"] = mach_helper_stderr[-4000:]
    artifact = root / "app-sandbox-result.json"
    artifact.write_text(json.dumps(report, indent=2) + "\n"); artifact.chmod(0o600)
    print(artifact)
    return 0 if report["status"] in ("staged", "measured") else 2


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Follow Orbit and isolated Parsec logs without opening or controlling Parsec.

Run this when macOS loader output is needed in a terminal. The viewer only
opens the two documented log files for bounded, nonblocking reads.
"""

import argparse
from collections import deque
import errno
import os
from pathlib import Path
import stat
import sys
import time

MAX_READ_BYTES = 64 * 1024
MAX_LINE_BYTES = 64 * 1024
MAX_PENDING_BYTES = 64 * 1024
MAX_LINES_PER_POLL = 1000
MAX_ONCE_BYTES = 256 * 1024
POLL_INTERVAL = 0.2


class UnsafeLogError(Exception):
    """The selected path is not a safe, current-user regular log file."""


def validate_stat(info, path):
    if not stat.S_ISREG(info.st_mode):
        raise UnsafeLogError(f"refusing non-regular log: {path}")
    if info.st_uid != os.getuid():
        raise UnsafeLogError(f"refusing log owned by another user: {path}")
    if info.st_nlink != 1:
        raise UnsafeLogError(f"refusing multiply-linked log: {path}")


def open_log(path):
    """Open a validated log without following symlinks or blocking."""
    path = Path(path)
    if ".." in path.parts:
        raise UnsafeLogError(f"refusing parent traversal in log path: {path}")
    path = Path(os.path.normpath(os.fspath(path)))
    if not path.is_absolute() or len(path.parts) < 2:
        raise UnsafeLogError(f"log path must be absolute: {path}")

    directory_flags = (os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0))
    directory_fd = os.open(path.anchor, directory_flags)
    try:
        for component in path.parts[1:-1]:
            try:
                next_fd = os.open(component, directory_flags, dir_fd=directory_fd)
            except FileNotFoundError:
                return None
            info = os.fstat(next_fd)
            if not stat.S_ISDIR(info.st_mode):
                os.close(next_fd)
                raise UnsafeLogError(f"refusing non-directory path component: {path}")
            os.close(directory_fd)
            directory_fd = next_fd

        name = path.name
        try:
            before = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
        except FileNotFoundError:
            return None
        validate_stat(before, path)

        flags = os.O_RDONLY | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0)
        try:
            descriptor = os.open(name, flags, dir_fd=directory_fd)
        except FileNotFoundError:
            return None
        except OSError as error:
            if error.errno == errno.ELOOP:
                raise UnsafeLogError(f"refusing symlink log: {path}") from error
            raise

        try:
            opened = os.fstat(descriptor)
            validate_stat(opened, path)
            if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
                raise UnsafeLogError(f"log changed while opening: {path}")
            current = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
            if (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino):
                raise UnsafeLogError(f"log changed while opening: {path}")
            return descriptor, opened
        except BaseException:
            os.close(descriptor)
            raise
    except OSError as error:
        if error.errno in (errno.ELOOP, errno.ENOTDIR):
            raise UnsafeLogError(f"refusing symlink or invalid parent component: {path}") from error
        raise
    finally:
        os.close(directory_fd)


def split_lines(buffer, limit):
    """Return displayable lines and the bounded bytes not yet completed."""
    lines = []
    while len(lines) < limit:
        newline = buffer.find(b"\n")
        if newline >= 0 and newline <= MAX_LINE_BYTES:
            lines.append(bytes(buffer[:newline]).decode("utf-8", errors="replace"))
            del buffer[:newline + 1]
            continue
        if len(buffer) >= MAX_LINE_BYTES:
            lines.append(bytes(buffer[:MAX_LINE_BYTES]).decode("utf-8", errors="replace"))
            del buffer[:MAX_LINE_BYTES]
            continue
        break
    return lines


class LogTail:
    def __init__(self, path, tag):
        self.path = path
        self.tag = tag
        self.identity = None
        self.offset = 0
        self.pending = bytearray()
        self.initialized = False
        self.reported_error = None

    def poll(self):
        opened = open_log(self.path)
        if opened is None:
            self.identity = None
            self.offset = 0
            self.pending.clear()
            self.initialized = True
            self.reported_error = None
            return []

        descriptor, info = opened
        try:
            identity = (info.st_dev, info.st_ino)
            if not self.initialized:
                self.initialized = True
                self.identity = identity
                self.offset = info.st_size
                return []
            if self.identity != identity or info.st_size < self.offset:
                self.identity = identity
                self.offset = 0
                self.pending.clear()
            lines = split_lines(self.pending, MAX_LINES_PER_POLL)
            if len(lines) == MAX_LINES_PER_POLL:
                return [f"[{self.tag}] {line}" for line in lines]
            capacity = MAX_PENDING_BYTES - len(self.pending)
            read_size = min(MAX_READ_BYTES, capacity)
            if read_size <= 0:
                return [f"[{self.tag}] {line}" for line in lines]
            data = os.pread(descriptor, read_size, self.offset)
            self.offset += len(data)
        finally:
            os.close(descriptor)

        self.pending.extend(data)
        lines.extend(split_lines(self.pending, MAX_LINES_PER_POLL - len(lines)))
        return [f"[{self.tag}] {line}" for line in lines]

    def snapshot(self):
        """Read only a capped tail for --once, then return immediately."""
        opened = open_log(self.path)
        if opened is None:
            return []
        descriptor, info = opened
        try:
            start = max(0, info.st_size - MAX_ONCE_BYTES)
            data = os.pread(descriptor, MAX_ONCE_BYTES, start)
        finally:
            os.close(descriptor)
        if start:
            first_newline = data.find(b"\n")
            if first_newline < 0:
                return []
            data = data[first_newline + 1:]
        pending = bytearray(data)
        lines = deque(maxlen=MAX_LINES_PER_POLL)
        while pending:
            batch = split_lines(pending, MAX_LINES_PER_POLL)
            if not batch:
                lines.append(bytes(pending).decode("utf-8", errors="replace"))
                pending.clear()
                break
            lines.extend(batch)
        return [f"[{self.tag}] {line}" for line in lines]


def log_sources(orbit_root):
    return [
        LogTail(orbit_root / "parsec-orbit.log", "orbit"),
        LogTail(orbit_root / "home" / ".parsec" / "log.txt", "parsec"),
    ]


def parse_args(argv=None):
    parser = argparse.ArgumentParser(
        description="View macOS Parsec Orbit logs without controlling Parsec.")
    parser.add_argument(
        "--orbit-root", type=Path,
        help="absolute Orbit root (default: ~/Library/Application Support/ParsecOrbit)")
    parser.add_argument(
        "--once", action="store_true",
        help="print a bounded recent snapshot and exit")
    args = parser.parse_args(argv)
    if args.orbit_root is not None and not args.orbit_root.is_absolute():
        parser.error("--orbit-root must be an absolute path")
    return args


def main(argv=None):
    args = parse_args(argv)
    root = args.orbit_root or Path.home() / "Library" / "Application Support" / "ParsecOrbit"
    sources = log_sources(root)

    if args.once:
        failed = False
        for source in sources:
            try:
                lines = source.snapshot()
            except (OSError, UnsafeLogError) as error:
                print(f"console-macos: {error}", file=sys.stderr)
                failed = True
                continue
            for line in lines:
                print(line, flush=True)
        return 1 if failed else 0

    print(f"Following macOS Orbit logs under {root} (Ctrl+C to stop).", file=sys.stderr)
    try:
        while True:
            for source in sources:
                try:
                    lines = source.poll()
                except (OSError, UnsafeLogError) as error:
                    message = str(error)
                    if source.reported_error != message:
                        print(f"console-macos: {message}", file=sys.stderr)
                    source.reported_error = message
                    continue
                source.reported_error = None
                for line in lines:
                    print(line, flush=True)
            time.sleep(POLL_INTERVAL)
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    raise SystemExit(main())

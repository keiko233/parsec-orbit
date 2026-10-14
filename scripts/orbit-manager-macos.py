#!/usr/bin/env python3
"""Install Parsec Orbit macOS arm64 releases from a verified release manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path, PurePosixPath
from typing import Callable


DEFAULT_MANIFEST = (
    "https://github.com/keiko233/parsec-orbit/releases/latest/download/"
    "release-macos.json"
)
OFFICIAL_KEY = "2LCW/gbjU6USD1CX5myen6o6Y0E/NN+KFjsRiSV7Sbs="
MAX_MANIFEST = 64 * 1024
MAX_SIGNATURE = 4096
MAX_VERIFIER = 32 * 1024 * 1024
MAX_BUNDLE = 256 * 1024 * 1024
MAX_FILE_UNPACKED = 512 * 1024 * 1024
MAX_TOTAL_UNPACKED = 1024 * 1024 * 1024
DEFAULT_APP = "/Applications/Parsec.app"
DEFAULT_ROOT_PARTS = ("Library", "Application Support", "ParsecOrbit")
VERSION_RE = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$"
)
SHA_RE = re.compile(r"^[a-f0-9]{64}$")
GITHUB_RELEASE_PATH_RE = re.compile(
    r"^/[^/]+/[^/]+/releases/download/[^/]+/release-macos\.json$"
)
GITHUB_ASSET_HOSTS = {
    "github.com",
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
}

EXPECTED_FILES = {
    "scripts/install-macos.sh",
    "scripts/console-macos.sh",
    "scripts/console-macos.py",
    "target/release/parsec-orbit-installer-macos",
    "target/release/libparsec_orbit_loader.dylib",
    "target/release/libparsec_orbit_plugin_config.dylib",
    "target/release/libparsec_orbit_plugin_hooks.dylib",
    "target/release/libparsec_orbit_plugin_js_runtime.dylib",
    "crates/parsec-orbit-plugin-config/plugin.macos.toml",
    "crates/parsec-orbit-plugin-hooks/plugin.macos.toml",
    "crates/parsec-orbit-plugin-js-runtime/plugin.macos.toml",
}
EXECUTABLE_FILES = {
    "scripts/install-macos.sh",
    "scripts/console-macos.sh",
    "target/release/parsec-orbit-installer-macos",
}


class ManagerError(Exception):
    """An expected validation, download, or installer failure."""


class FetchUnavailable(ManagerError):
    """A remote object could not be retrieved from its permitted location."""


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def strict_json(data: bytes, label: str) -> object:
    def pairs(pairs_list: list[tuple[str, object]]) -> dict[str, object]:
        result: dict[str, object] = {}
        for key, value in pairs_list:
            if key in result:
                raise ManagerError(f"{label} has a duplicate JSON key: {key}")
            result[key] = value
        return result

    try:
        return json.loads(data.decode("utf-8"), object_pairs_hook=pairs)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ManagerError(f"{label} is not valid UTF-8 JSON: {error}") from error


def _url_parts(url: str):
    try:
        parts = urllib.parse.urlsplit(url)
        port = parts.port
    except ValueError as error:
        raise ManagerError(f"invalid URL {url!r}: {error}") from error
    if parts.username is not None or parts.password is not None:
        raise ManagerError("URLs containing credentials are refused")
    if parts.fragment:
        raise ManagerError("URLs containing fragments are refused")
    return parts


def origin_of(url: str) -> tuple[str, str, int]:
    parts = _url_parts(url)
    scheme = parts.scheme.lower()
    host = (parts.hostname or "").lower()
    port = parts.port or (443 if scheme == "https" else 80)
    return scheme, host, port


def validate_location(url: str, test_origins: frozenset[tuple[str, str, int]] = frozenset()) -> None:
    parts = _url_parts(url)
    origin = origin_of(url)
    if origin[0] == "https" and origin[1]:
        return
    if origin in test_origins and origin[0] == "http" and origin[1] in {
        "127.0.0.1",
        "::1",
        "localhost",
    }:
        return
    raise ManagerError("release locations must use HTTPS")


def redirect_is_allowed(initial_url: str, target_url: str) -> bool:
    """Check a redirect before urllib is allowed to contact its target."""
    source = _url_parts(initial_url)
    target = _url_parts(target_url)
    source_origin = origin_of(initial_url)
    target_origin = origin_of(target_url)
    if source_origin[0] != "https" or target_origin[0] != "https":
        return False
    if target_origin == source_origin:
        return True
    return (
        source_origin[1] == "github.com"
        and target_origin[1] in GITHUB_ASSET_HOSTS
        and target_origin[2] == 443
        and source.port in (None, 443)
    )


def final_redirect_is_allowed(initial_url: str, final_url: str) -> bool:
    if initial_url == final_url:
        return origin_of(final_url)[0] == "https" or origin_of(final_url)[0] == "http"
    initial = _url_parts(initial_url)
    final = _url_parts(final_url)
    first_origin = origin_of(initial_url)
    final_origin = origin_of(final_url)
    if first_origin[0] == "https" and final_origin[0] == "https":
        return final_origin == first_origin or (
            first_origin[1] == "github.com"
            and final_origin[1] in GITHUB_ASSET_HOSTS
            and final_origin[2] == 443
        )
    # The test fetch transport can only serve a directly configured loopback
    # URL. Production urllib never accepts HTTP as its initial URL.
    return first_origin == final_origin and first_origin[0] == "http" and (
        first_origin[1] in {"127.0.0.1", "::1", "localhost"}
    )


class GuardedRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Reject unsafe redirect targets before urllib sends another request."""

    def __init__(self, initial_url: str):
        super().__init__()
        self.initial_url = initial_url
        self.history = [initial_url]

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        target = urllib.parse.urljoin(req.full_url, newurl)
        if not redirect_is_allowed(self.initial_url, target):
            raise ManagerError(f"refusing unsafe redirect to {target}")
        self.history.append(target)
        return super().redirect_request(req, fp, code, msg, headers, target)


def fetch_https(url: str, limit: int) -> tuple[bytes, str, tuple[str, ...]]:
    validate_location(url)
    redirects = GuardedRedirectHandler(url)
    opener = urllib.request.build_opener(redirects)
    request = urllib.request.Request(
        url,
        headers={"User-Agent": "parsec-orbit-macos-manager/1", "Accept": "application/octet-stream"},
    )
    try:
        with opener.open(request, timeout=30) as response:
            final_url = response.geturl()
            if not final_redirect_is_allowed(url, final_url):
                raise ManagerError(f"refusing unchecked final URL {final_url}")
            chunks: list[bytes] = []
            total = 0
            while True:
                chunk = response.read(min(64 * 1024, limit + 1 - total))
                if not chunk:
                    break
                total += len(chunk)
                if total > limit:
                    raise ManagerError(f"download exceeds the {limit}-byte limit")
                chunks.append(chunk)
            return b"".join(chunks), final_url, tuple(redirects.history)
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        raise FetchUnavailable(f"download failed for {url}: {error}") from error


def validate_manifest_url(url: str, test_origins: frozenset[tuple[str, str, int]]) -> None:
    validate_location(url, test_origins)
    parts = _url_parts(url)
    if parts.query:
        raise ManagerError("manifest URL query strings are refused")
    decoded_path = urllib.parse.unquote(parts.path)
    if "\\" in decoded_path or any(part in {".", ".."} for part in decoded_path.split("/")):
        raise ManagerError("manifest URL contains an unsafe path")


def pinned_manifest_url(initial_url: str, final_url: str, history: tuple[str, ...]) -> str:
    if not final_redirect_is_allowed(initial_url, final_url):
        raise ManagerError(f"manifest ended at an unchecked URL: {final_url}")
    initial_origin = origin_of(initial_url)
    candidates = [initial_url, *history]
    if initial_origin[1] == "github.com":
        versioned = []
        for candidate in candidates:
            parts = _url_parts(candidate)
            if (
                (parts.hostname or "").lower() == "github.com"
                and GITHUB_RELEASE_PATH_RE.fullmatch(urllib.parse.unquote(parts.path))
            ):
                versioned.append(candidate)
        if not versioned:
            raise ManagerError(
                "GitHub latest manifest did not resolve to a versioned release download URL"
            )
        return versioned[-1]
    same_origin = [candidate for candidate in candidates if origin_of(candidate) == initial_origin]
    return same_origin[-1] if same_origin else final_url


def validate_manifest(data: bytes) -> dict[str, object]:
    value = strict_json(data, "release manifest")
    if not isinstance(value, dict):
        raise ManagerError("release manifest must be a JSON object")
    if value.get("format") != 1 or isinstance(value.get("format"), bool):
        raise ManagerError("release manifest format must be 1")
    if value.get("platform") != "macos-arm64":
        raise ManagerError("release manifest platform must be macos-arm64")
    version = value.get("version")
    if not isinstance(version, str) or not VERSION_RE.fullmatch(version):
        raise ManagerError("release manifest version is not valid semantic version text")
    for name, expected_url, maximum in (
        ("verifier", "parsec-orbit-market", MAX_VERIFIER),
        ("bundle", "parsec-orbit-macos-arm64.zip", MAX_BUNDLE),
    ):
        item = value.get(name)
        if not isinstance(item, dict):
            raise ManagerError(f"release manifest {name} descriptor must be an object")
        if item.get("url") != expected_url:
            raise ManagerError(f"release manifest {name} URL must be {expected_url}")
        digest = item.get("sha256")
        if not isinstance(digest, str) or not SHA_RE.fullmatch(digest):
            raise ManagerError(f"release manifest {name} sha256 must be 64 lowercase hex digits")
        size = item.get("size")
        if isinstance(size, bool) or not isinstance(size, int) or not 0 < size <= maximum:
            raise ManagerError(f"release manifest {name} size is outside allowed bounds")
    return value


def validate_absolute_path(text: str, label: str) -> Path:
    if not text.startswith("/") or "\n" in text or "\r" in text:
        raise ManagerError(f"{label} must be an absolute path without newlines")
    path = Path(text)
    if any(part in {".", ".."} for part in path.parts):
        raise ManagerError(f"{label} must not contain . or .. path components")
    return path


def check_components(path: Path, *, allow_missing: bool = False) -> None:
    if not path.is_absolute():
        raise ManagerError(f"path must be absolute: {path}")
    current = Path("/")
    for part in path.parts[1:]:
        if part in {"", ".", ".."}:
            raise ManagerError(f"non-canonical path component in {path}")
        current = current / part
        try:
            metadata = current.lstat()
        except FileNotFoundError:
            if allow_missing:
                continue
            raise ManagerError(f"path component does not exist: {current}")
        if stat.S_ISLNK(metadata.st_mode):
            raise ManagerError(f"symlink path component refused: {current}")


def ensure_user_directory(path: Path, uid: int, *, create: bool) -> None:
    check_components(path, allow_missing=create)
    missing: list[Path] = []
    current = path
    while not current.exists():
        missing.append(current)
        current = current.parent
    for directory in reversed(missing):
        directory.mkdir(mode=0o700)
    metadata = path.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != uid:
        raise ManagerError(f"directory must be owned by the current user: {path}")


def assert_owned_regular(path: Path, uid: int, label: str) -> os.stat_result:
    check_components(path)
    try:
        metadata = path.lstat()
    except FileNotFoundError as error:
        raise ManagerError(f"{label} does not exist: {path}") from error
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != uid
        or metadata.st_nlink != 1
    ):
        raise ManagerError(f"{label} must be an owned, single-link regular file: {path}")
    return metadata


def read_secure_file(path: Path, uid: int, label: str, limit: int) -> bytes:
    before = assert_owned_regular(path, uid, label)
    if before.st_size > limit:
        raise ManagerError(f"{label} exceeds the {limit}-byte limit")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or opened.st_uid != uid
            or opened.st_nlink != 1
            or opened.st_dev != before.st_dev
            or opened.st_ino != before.st_ino
        ):
            raise ManagerError(f"{label} changed while it was opened")
        data = bytearray()
        while len(data) <= limit:
            chunk = os.read(descriptor, min(64 * 1024, limit + 1 - len(data)))
            if not chunk:
                break
            data.extend(chunk)
        if len(data) > limit:
            raise ManagerError(f"{label} exceeds the {limit}-byte limit")
        after = os.fstat(descriptor)
        if (
            after.st_dev != opened.st_dev
            or after.st_ino != opened.st_ino
            or after.st_size != opened.st_size
            or after.st_mtime_ns != opened.st_mtime_ns
        ):
            raise ManagerError(f"{label} changed while it was read")
        return bytes(data)
    finally:
        os.close(descriptor)


def safe_zip_name(name: str) -> str:
    if not name or "\\" in name or name.startswith("/") or "\x00" in name:
        raise ManagerError(f"unsafe ZIP member name: {name!r}")
    path = PurePosixPath(name)
    if any(part in {"", ".", ".."} for part in name.split("/")):
        raise ManagerError(f"unsafe ZIP member path: {name!r}")
    if path.is_absolute() or re.match(r"^[A-Za-z]:", name):
        raise ManagerError(f"absolute ZIP member path refused: {name!r}")
    return path.as_posix()


def extract_validated_zip(archive_bytes: bytes, destination: Path) -> dict[str, str]:
    if len(archive_bytes) > MAX_BUNDLE:
        raise ManagerError("release ZIP exceeds the compressed size limit")
    try:
        archive = zipfile.ZipFile(__import__("io").BytesIO(archive_bytes))
    except (zipfile.BadZipFile, OSError) as error:
        raise ManagerError(f"release bundle is not a valid ZIP: {error}") from error
    with archive:
        infos = archive.infolist()
        seen: set[str] = set()
        seen_folded: set[str] = set()
        advertised_total = 0
        for info in infos:
            name = safe_zip_name(info.filename)
            folded = name.casefold()
            if name in seen or folded in seen_folded:
                raise ManagerError(f"duplicate ZIP member name: {name}")
            seen.add(name)
            seen_folded.add(folded)
            if info.is_dir():
                raise ManagerError(f"ZIP directory entries are not part of the release contract: {name}")
            mode = info.external_attr >> 16
            kind = stat.S_IFMT(mode)
            if kind not in (0, stat.S_IFREG):
                raise ManagerError(f"non-regular ZIP member refused: {name}")
            if info.flag_bits & 1:
                raise ManagerError(f"encrypted ZIP member refused: {name}")
            if info.file_size < 0 or info.file_size > MAX_FILE_UNPACKED:
                raise ManagerError(f"ZIP member exceeds the unpacked size limit: {name}")
            advertised_total += info.file_size
            if advertised_total > MAX_TOTAL_UNPACKED:
                raise ManagerError("release ZIP exceeds the total unpacked size limit")
        if seen != EXPECTED_FILES:
            missing = sorted(EXPECTED_FILES - seen)
            extra = sorted(seen - EXPECTED_FILES)
            raise ManagerError(f"ZIP file layout mismatch (missing={missing}, extra={extra})")

        digests: dict[str, str] = {}
        actual_total = 0
        for info in infos:
            name = safe_zip_name(info.filename)
            path = destination.joinpath(*PurePosixPath(name).parts)
            path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            digest = hashlib.sha256()
            written = 0
            flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
            descriptor = os.open(path, flags, 0o700 if name in EXECUTABLE_FILES else 0o600)
            try:
                with archive.open(info, "r") as member:
                    while True:
                        chunk = member.read(64 * 1024)
                        if not chunk:
                            break
                        written += len(chunk)
                        actual_total += len(chunk)
                        if written > MAX_FILE_UNPACKED or actual_total > MAX_TOTAL_UNPACKED:
                            raise ManagerError("release ZIP exceeded unpacked size limits while extracting")
                        digest.update(chunk)
                        view = memoryview(chunk)
                        while view:
                            count = os.write(descriptor, view)
                            view = view[count:]
                if written != info.file_size:
                    raise ManagerError(f"ZIP member size did not match its directory entry: {name}")
                os.fchmod(descriptor, 0o755 if name in EXECUTABLE_FILES else 0o644)
                os.fsync(descriptor)
            except (zipfile.BadZipFile, RuntimeError, OSError) as error:
                raise ManagerError(f"could not safely extract {name}: {error}") from error
            finally:
                os.close(descriptor)
            digests[name] = digest.hexdigest()
    return digests


def validate_release_tree(release_dir: Path, expected_metadata_sha: str, uid: int) -> dict[str, object]:
    check_components(release_dir)
    root_meta = release_dir.lstat()
    if not stat.S_ISDIR(root_meta.st_mode) or root_meta.st_uid != uid:
        raise ManagerError("cached release directory is not a user-owned directory")
    metadata_path = release_dir / "cache.json"
    metadata_bytes = read_secure_file(metadata_path, uid, "cached release metadata", MAX_MANIFEST)
    if sha256_bytes(metadata_bytes) != expected_metadata_sha:
        raise ManagerError("cached release metadata digest does not match manager state")
    metadata = strict_json(metadata_bytes, "cached release metadata")
    if not isinstance(metadata, dict) or metadata.get("format") != 1:
        raise ManagerError("cached release metadata has an unsupported format")
    files = metadata.get("files")
    if not isinstance(files, dict) or set(files) != EXPECTED_FILES:
        raise ManagerError("cached release metadata does not list the exact release layout")
    tree = release_dir / "tree"
    check_components(tree)
    found: set[str] = set()
    expected_dirs = {
        "/".join(PurePosixPath(name).parts[:index])
        for name in EXPECTED_FILES
        for index in range(1, len(PurePosixPath(name).parts))
    }
    for directory, dirs, filenames in os.walk(tree, followlinks=False):
        directory_path = Path(directory)
        for item in dirs:
            path = directory_path / item
            info = path.lstat()
            if not stat.S_ISDIR(info.st_mode) or info.st_uid != uid:
                raise ManagerError(f"unsafe directory in cached release: {path}")
            relative_dir = path.relative_to(tree).as_posix()
            if relative_dir not in expected_dirs:
                raise ManagerError(f"unexpected directory in cached release: {relative_dir}")
        for item in filenames:
            path = directory_path / item
            relative = path.relative_to(tree).as_posix()
            if relative not in EXPECTED_FILES:
                raise ManagerError(f"unexpected file in cached release: {relative}")
            info = assert_owned_regular(path, uid, "cached release file")
            if info.st_size > MAX_FILE_UNPACKED:
                raise ManagerError(f"cached release file exceeds size limit: {relative}")
            file_hash = sha256_bytes(read_secure_file(path, uid, "cached release file", MAX_FILE_UNPACKED))
            if files.get(relative) != file_hash:
                raise ManagerError(f"cached release file digest mismatch: {relative}")
            found.add(relative)
    if found != EXPECTED_FILES:
        raise ManagerError("cached release is missing required files")
    return metadata


def fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def atomic_write(path: Path, data: bytes, mode: int = 0o600) -> None:
    descriptor, temp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temp_path = Path(temp_name)
    try:
        os.fchmod(descriptor, mode)
        with os.fdopen(descriptor, "wb", closefd=True) as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temp_path, path)
        fsync_directory(path.parent)
    except BaseException:
        try:
            os.close(descriptor)
        except OSError:
            pass
        temp_path.unlink(missing_ok=True)
        raise


class Manager:
    def __init__(
        self,
        orbit_root: Path,
        *,
        fetcher: Callable[
            [str, int],
            tuple[bytes, str] | tuple[bytes, str, tuple[str, ...]],
        ]
        | None = None,
        test_http_origins: frozenset[tuple[str, str, int]] = frozenset(),
        runner: Callable[[list[str], bool], subprocess.CompletedProcess] | None = None,
        output: Callable[[str], None] = print,
    ):
        self.uid = os.geteuid()
        self.root = orbit_root
        self.manager_dir = self.root / ".network-install-manager"
        self.fetcher = fetcher
        self.test_http_origins = test_http_origins
        self.runner = runner or self._run_subprocess
        self.output = output

    @staticmethod
    def _run_subprocess(argv: list[str], capture: bool) -> subprocess.CompletedProcess:
        return subprocess.run(
            argv,
            text=True,
            capture_output=capture,
            check=False,
        )

    def _fetch(self, url: str, limit: int) -> tuple[bytes, str, tuple[str, ...]]:
        validate_location(url, self.test_http_origins)
        if self.fetcher is None:
            data, final_url, history = fetch_https(url, limit)
        else:
            try:
                fetched = self.fetcher(url, limit)
            except FetchUnavailable:
                raise
            except (urllib.error.URLError, TimeoutError, OSError) as error:
                raise FetchUnavailable(f"download failed for {url}: {error}") from error
            if len(fetched) == 2:
                data, final_url = fetched
                history = (url, final_url)
            elif len(fetched) == 3:
                data, final_url, history = fetched
                history = tuple(history)
            else:
                raise ManagerError("injected fetch transport returned an invalid result")
        if not isinstance(data, bytes) or len(data) > limit:
            raise ManagerError(f"download from {url} exceeded its size limit")
        validate_location(final_url, self.test_http_origins)
        if not final_redirect_is_allowed(url, final_url):
            raise ManagerError(f"download ended at an unchecked URL: {final_url}")
        return data, final_url, history

    def _prepare_dirs(self) -> None:
        ensure_user_directory(self.root, self.uid, create=True)
        if not self.manager_dir.exists():
            self.manager_dir.mkdir(mode=0o700)
        info = self.manager_dir.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != self.uid:
            raise ManagerError("manager cache directory must be an owned directory")
        os.chmod(self.manager_dir, 0o700)
        for name in ("releases", "tools", "tmp", "transactions"):
            path = self.manager_dir / name
            if not path.exists():
                path.mkdir(mode=0o700)
            info = path.lstat()
            if not stat.S_ISDIR(info.st_mode) or info.st_uid != self.uid:
                raise ManagerError(f"manager {name} directory must be owned by the current user")
            os.chmod(path, 0o700)

    def _state(self) -> dict[str, object]:
        path = self.manager_dir / "state.json"
        try:
            data = read_secure_file(path, self.uid, "manager state", MAX_MANIFEST)
        except ManagerError as error:
            if "does not exist" in str(error):
                return {"format": 1, "installed": None, "bootstrap": None}
            raise
        value = strict_json(data, "manager state")
        if not isinstance(value, dict) or value.get("format") != 1:
            raise ManagerError("manager state has an unsupported format")
        if set(value) != {"format", "installed", "bootstrap"}:
            raise ManagerError("manager state has unexpected fields")
        return value

    def _assert_no_pending_operation(self) -> None:
        pending_path = self.manager_dir / "pending.json"
        if not pending_path.exists() and not pending_path.is_symlink():
            return
        data = read_secure_file(pending_path, self.uid, "pending manager operation", MAX_MANIFEST)
        pending = strict_json(data, "pending manager operation")
        if (
            isinstance(pending, dict)
            and pending.get("wrapper_succeeded") is True
            and pending.get("state") == self._state()
        ):
            pending_path.unlink()
            fsync_directory(self.manager_dir)
            return
        operation = pending.get("operation", "unknown") if isinstance(pending, dict) else "unknown"
        app = pending.get("app", "unknown") if isinstance(pending, dict) else "unknown"
        status = (
            pending.get("wrapper_status", pending.get("wrapper_succeeded", "unknown"))
            if isinstance(pending, dict)
            else "unknown"
        )
        raise ManagerError(
            f"an interrupted {operation} has a recovery record at {pending_path} "
            f"(app={app}, wrapper_status={status}); inspect that record and the app "
            "before starting another manager operation"
        )

    def _lock(self):
        lock = self.manager_dir / "lock"
        try:
            lock.mkdir(mode=0o700)
        except FileExistsError as error:
            raise ManagerError(f"another manager operation may be active: {lock}") from error
        (lock / "pid").write_text(f"{os.getpid()}\n", encoding="ascii")
        return lock

    @staticmethod
    def _unlock(lock: Path) -> None:
        (lock / "pid").unlink(missing_ok=True)
        lock.rmdir()

    def _key_snapshot(self, key_override: Path | None, work: Path) -> tuple[Path, str]:
        if key_override is None:
            contents = f"ed25519 {OFFICIAL_KEY} official Parsec Orbit release key\n".encode("ascii")
            self.output("Using the embedded official Parsec Orbit Ed25519 release key.")
        else:
            source = read_secure_file(key_override, self.uid, "trusted key file", 1024 * 1024)
            if not source.strip():
                raise ManagerError("--keys file must contain at least one trusted key")
            try:
                source.decode("utf-8")
            except UnicodeDecodeError as error:
                raise ManagerError("--keys file must be UTF-8 text") from error
            contents = source
        snapshot = work / "trusted-keys.txt"
        descriptor = os.open(snapshot, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(contents)
            stream.flush()
            os.fsync(stream.fileno())
        return snapshot, sha256_bytes(contents)

    def _trusted_tool(self, bootstrap: object) -> Path | None:
        if not isinstance(bootstrap, dict) or bootstrap.get("signed") is not True:
            return None
        digest = bootstrap.get("sha256")
        if not isinstance(digest, str) or not SHA_RE.fullmatch(digest):
            return None
        path = self.manager_dir / "tools" / digest / "parsec-orbit-market"
        try:
            data = read_secure_file(path, self.uid, "cached market verifier", MAX_VERIFIER)
        except ManagerError:
            return None
        if sha256_bytes(data) != digest:
            return None
        return path

    def _verify_manifest(self, verifier: Path, manifest_path: Path, key_path: Path) -> bool:
        result = self.runner(
            [str(verifier), "verify-any", str(manifest_path), "--keys", str(key_path)],
            True,
        )
        return result.returncode == 0

    def _run_checked(self, argv: list[str], label: str, *, capture: bool) -> subprocess.CompletedProcess:
        result = self.runner(argv, capture)
        if result.returncode != 0:
            detail = (getattr(result, "stderr", "") or getattr(result, "stdout", "") or "").strip()
            suffix = f": {detail}" if detail else ""
            raise ManagerError(f"{label} failed with exit code {result.returncode}{suffix}")
        return result

    def _check_manifest_path(
        self, initial_url: str, final_url: str, history: tuple[str, ...]
    ) -> str:
        return pinned_manifest_url(initial_url, final_url, history)

    @staticmethod
    def _inspect_is_applied(value: object) -> bool:
        if not isinstance(value, dict):
            raise ManagerError("installer inspection did not return a JSON object")
        classification = value.get("classification")
        if not isinstance(classification, dict) or not isinstance(classification.get("class"), str):
            raise ManagerError("installer inspection omitted its app classification")
        app_class = classification["class"].casefold()
        recording = value.get("recording")
        recorded_state = recording.get("state") if isinstance(recording, dict) else None
        if recorded_state in {"preparing", "backed-up"} or app_class == "stale":
            raise ManagerError("Parsec.app has a stale or interrupted Orbit transaction; refusing to proceed")
        if recorded_state == "installed" and app_class != "applied":
            raise ManagerError("installer state says installed but the app is not classified as applied")
        if app_class == "applied":
            if recorded_state != "installed":
                raise ManagerError("app classification is applied but its transaction state is not installed")
            return True
        if app_class not in {"pristine", "missing"}:
            raise ManagerError(f"Parsec.app classification is not safe for network installation: {app_class}")
        if recorded_state not in (None, "uninstalled", "rolled-back"):
            raise ManagerError(f"unexpected Orbit transaction state: {recorded_state}")
        return False

    def _install_or_upgrade(
        self,
        command: str,
        app: Path,
        manifest_url: str,
        key_override: Path | None,
        allow_unsigned: bool,
    ) -> None:
        lock = self._lock()
        work: Path | None = None
        pending_path = self.manager_dir / "pending.json"
        pending_record: dict[str, object] | None = None
        pending_created = False
        wrapper_invoked = False
        wrapper_completed = False
        wrapper_succeeded = False
        state_committed = False
        try:
            self._assert_no_pending_operation()
            state = self._state()
            old_installed = state.get("installed")
            if command == "install" and old_installed is not None:
                raise ManagerError("this Orbit root already has a managed installation; use upgrade")
            if isinstance(old_installed, dict) and old_installed.get("app") != str(app):
                raise ManagerError("the requested app path differs from the managed installation")
            work = Path(tempfile.mkdtemp(prefix="operation-", dir=self.manager_dir / "transactions"))
            validate_manifest_url(manifest_url, self.test_http_origins)
            data, final_url, manifest_history = self._fetch(manifest_url, MAX_MANIFEST)
            final_manifest_url = self._check_manifest_path(manifest_url, final_url, manifest_history)
            manifest = validate_manifest(data)
            manifest_sha = sha256_bytes(data)
            verifier_desc = manifest["verifier"]
            bundle_desc = manifest["bundle"]
            assert isinstance(verifier_desc, dict) and isinstance(bundle_desc, dict)
            key_path, key_sha = self._key_snapshot(key_override, work)
            manifest_path = work / "release-macos.json"
            manifest_path.write_bytes(data)
            # final_manifest_url names the manifest itself; append .sig instead
            # of replacing its filename, keeping both assets pinned to one tag.
            sig_url = final_manifest_url + ".sig"
            signature: bytes | None = None
            try:
                signature, _, _ = self._fetch(sig_url, MAX_SIGNATURE)
            except FetchUnavailable:
                if not allow_unsigned:
                    raise ManagerError("release manifest signature is unavailable")
                self.output("No detached signature was available; continuing only because --allow-unsigned was supplied.")
            if signature is not None and not signature.strip():
                if not allow_unsigned:
                    raise ManagerError("release manifest signature is empty")
                signature = None
                self.output("The detached signature was empty; continuing only because --allow-unsigned was supplied.")
            if signature is not None:
                (work / "release-macos.json.sig").write_bytes(signature)

            cached_tool = self._trusted_tool(state.get("bootstrap"))
            verifier_path: Path
            candidate_tool: Path | None = None
            signed = False
            if cached_tool is not None:
                verifier_path = cached_tool
                signed = signature is not None and self._verify_manifest(verifier_path, manifest_path, key_path)
                if not signed and not allow_unsigned:
                    raise ManagerError("release manifest signature verification failed")
            else:
                self.output(
                    "Trust boundary: bootstrapping parsec-orbit-market over HTTPS from the "
                    "pinned release origin and checking the manifest's unsigned size and SHA-256 "
                    "before executing it. This first verifier is not trusted until it verifies a "
                    "signed release and that manager operation succeeds."
                )
                verifier_url = urllib.parse.urljoin(final_manifest_url, str(verifier_desc["url"]))
                verifier_bytes, _, _ = self._fetch(verifier_url, MAX_VERIFIER)
                if len(verifier_bytes) != verifier_desc["size"] or sha256_bytes(verifier_bytes) != verifier_desc["sha256"]:
                    raise ManagerError("bootstrapped verifier size or sha256 does not match the manifest")
                candidate_tool = work / "parsec-orbit-market"
                descriptor = os.open(candidate_tool, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o700)
                with os.fdopen(descriptor, "wb") as stream:
                    stream.write(verifier_bytes)
                    stream.flush()
                    os.fsync(stream.fileno())
                verifier_path = candidate_tool
                signed = signature is not None and self._verify_manifest(verifier_path, manifest_path, key_path)
                if not signed and not allow_unsigned:
                    raise ManagerError("release manifest signature verification failed")

            if not signed:
                self.output(
                    "WARNING: --allow-unsigned is active. Authenticity relies only on HTTPS and "
                    "the manifest's own SHA-256 values; no verifier will be trusted or cached."
                )

            bundle_url = urllib.parse.urljoin(final_manifest_url, str(bundle_desc["url"]))
            bundle_bytes, _, _ = self._fetch(bundle_url, MAX_BUNDLE)
            if len(bundle_bytes) != bundle_desc["size"] or sha256_bytes(bundle_bytes) != bundle_desc["sha256"]:
                raise ManagerError("release bundle size or sha256 does not match the manifest")

            release_work = work / "release-cache"
            tree = release_work / "tree"
            tree.mkdir(parents=True, mode=0o700)
            file_hashes = extract_validated_zip(bundle_bytes, tree)
            cache_metadata = {
                "format": 1,
                "version": manifest["version"],
                "manifest_sha256": manifest_sha,
                "bundle_sha256": bundle_desc["sha256"],
                "files": file_hashes,
            }
            metadata_bytes = (json.dumps(cache_metadata, sort_keys=True, separators=(",", ":")) + "\n").encode()
            metadata_path = release_work / "cache.json"
            metadata_path.write_bytes(metadata_bytes)
            os.chmod(metadata_path, 0o600)
            metadata_sha = sha256_bytes(metadata_bytes)
            validate_release_tree(release_work, metadata_sha, self.uid)
            cache_id = f"{manifest['version']}-{manifest_sha[:16]}"
            release_final = self.manager_dir / "releases" / cache_id
            if release_final.exists():
                existing = validate_release_tree(release_final, metadata_sha, self.uid)
                if existing != cache_metadata:
                    raise ManagerError("an existing release cache conflicts with this manifest")
                shutil.rmtree(release_work)
                wrapper_root = release_final
            else:
                wrapper_root = release_work

            installer = wrapper_root / "tree" / "target/release/parsec-orbit-installer-macos"
            inspect = self._run_checked(
                [str(installer), "inspect", str(app), "--orbit-root", str(self.root), "--json"],
                "installer inspection",
                capture=True,
            )
            try:
                inspect_json = json.loads(inspect.stdout)
            except (json.JSONDecodeError, TypeError) as error:
                raise ManagerError(f"installer inspection returned invalid JSON: {error}") from error
            managed = self._inspect_is_applied(inspect_json)
            if command == "install" and managed:
                raise ManagerError("Parsec.app is already managed by Orbit; use upgrade")
            if command == "upgrade" and not managed:
                raise ManagerError("Parsec.app has no Orbit installation to upgrade")

            next_bootstrap = state.get("bootstrap")
            new_tool_bytes: bytes | None = None
            if signed:
                expected_tool_hash = str(verifier_desc["sha256"])
                if cached_tool is None or sha256_bytes(read_secure_file(cached_tool, self.uid, "cached verifier", MAX_VERIFIER)) != expected_tool_hash:
                    if candidate_tool is None:
                        verifier_url = urllib.parse.urljoin(final_manifest_url, str(verifier_desc["url"]))
                        verifier_bytes, _, _ = self._fetch(verifier_url, MAX_VERIFIER)
                        if len(verifier_bytes) != verifier_desc["size"] or sha256_bytes(verifier_bytes) != expected_tool_hash:
                            raise ManagerError("verified release verifier size or sha256 does not match the manifest")
                        candidate_tool = work / "parsec-orbit-market"
                        descriptor = os.open(candidate_tool, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o700)
                        with os.fdopen(descriptor, "wb") as stream:
                            stream.write(verifier_bytes)
                            stream.flush()
                            os.fsync(stream.fileno())
                    new_tool_bytes = read_secure_file(candidate_tool, self.uid, "candidate verifier", MAX_VERIFIER)
                    if sha256_bytes(new_tool_bytes) != expected_tool_hash:
                        raise ManagerError("candidate verifier changed after verification")
                next_bootstrap = {
                    "signed": True,
                    "sha256": expected_tool_hash,
                    "manifest_sha256": manifest_sha,
                    "keys_sha256": key_sha,
                }

            installed = {
                "cache_id": cache_id,
                "cache_metadata_sha256": metadata_sha,
                "version": manifest["version"],
                "manifest_sha256": manifest_sha,
                "bundle_sha256": bundle_desc["sha256"],
                "app": str(app),
                "orbit_root": str(self.root),
                "signed": signed,
            }
            new_state = {"format": 1, "installed": installed, "bootstrap": next_bootstrap}
            pending_record = {
                "format": 1,
                "operation": command,
                "wrapper_succeeded": False,
                "wrapper_status": "not-started",
                "app": str(app),
                "orbit_root": str(self.root),
                "cache_id": cache_id,
                "cache_metadata_sha256": metadata_sha,
                "release_staging": (
                    str(release_work.relative_to(self.manager_dir))
                    if wrapper_root == release_work
                    else None
                ),
                "candidate_verifier_sha256": (
                    sha256_bytes(new_tool_bytes) if new_tool_bytes is not None else None
                ),
                "state": new_state,
            }
            pending_created = True
            atomic_write(
                pending_path,
                (json.dumps(pending_record, sort_keys=True) + "\n").encode(),
            )

            wrapper = wrapper_root / "tree/scripts/install-macos.sh"
            argv = [str(wrapper), "--app", str(app), "--orbit-root", str(self.root), "--no-build"]
            pending_record["wrapper_status"] = "running"
            atomic_write(
                pending_path,
                (json.dumps(pending_record, sort_keys=True) + "\n").encode(),
            )
            wrapper_invoked = True
            result = self.runner(argv, False)
            wrapper_completed = True
            if result.returncode != 0:
                detail = (getattr(result, "stderr", "") or getattr(result, "stdout", "") or "").strip()
                suffix = f": {detail}" if detail else ""
                raise ManagerError(
                    f"Orbit {command} wrapper failed with exit code {result.returncode}{suffix}"
                )
            wrapper_succeeded = True
            pending_record["wrapper_succeeded"] = True
            pending_record["wrapper_status"] = "succeeded"
            atomic_write(
                pending_path,
                (json.dumps(pending_record, sort_keys=True) + "\n").encode(),
            )

            if wrapper_root == release_work:
                os.replace(release_work, release_final)
                fsync_directory(release_final.parent)
            if new_tool_bytes is not None:
                tool_hash = str(next_bootstrap["sha256"])
                tool_dir = self.manager_dir / "tools" / tool_hash
                if tool_dir.exists():
                    tool_path = tool_dir / "parsec-orbit-market"
                    current = read_secure_file(tool_path, self.uid, "cached market verifier", MAX_VERIFIER)
                    if sha256_bytes(current) != tool_hash:
                        raise ManagerError("existing cached verifier conflicts with the signed release")
                else:
                    temp_tool_dir = work / "tool-cache"
                    temp_tool_dir.mkdir(mode=0o700)
                    tool_path = temp_tool_dir / "parsec-orbit-market"
                    tool_path.write_bytes(new_tool_bytes)
                    os.chmod(tool_path, 0o700)
                    os.replace(temp_tool_dir, tool_dir)
                    fsync_directory(tool_dir.parent)
            state_bytes = (json.dumps(new_state, sort_keys=True) + "\n").encode()
            atomic_write(self.manager_dir / "state.json", state_bytes)
            state_committed = True
            pending_path.unlink()
            fsync_directory(self.manager_dir)
            self.output(f"Orbit {command} completed for release {manifest['version']}.")
        except BaseException as error:
            if not wrapper_succeeded:
                completion_unknown = wrapper_invoked and not wrapper_completed
                if completion_unknown and pending_record is not None:
                    pending_record["wrapper_status"] = "unknown"
                    try:
                        atomic_write(
                            pending_path,
                            (json.dumps(pending_record, sort_keys=True) + "\n").encode(),
                        )
                    except OSError:
                        pass
                    if isinstance(error, (KeyboardInterrupt, SystemExit)):
                        raise
                    raise ManagerError(
                        f"Orbit {command} wrapper completion is unknown; its staged release and "
                        f"recovery record were preserved at {pending_path}"
                    ) from error
                if pending_created and pending_path.exists():
                    pending_path.unlink()
                raise
            if state_committed:
                raise
            raise ManagerError(
                f"the installer wrapper succeeded, but manager finalization failed: {error}. "
                f"Validated cache and staging files were preserved; review {pending_path} "
                "before retrying"
            ) from error
        finally:
            completion_unknown = wrapper_invoked and not wrapper_completed
            if work is not None and (state_committed or (not wrapper_succeeded and not completion_unknown)):
                shutil.rmtree(work, ignore_errors=True)
            self._unlock(lock)

    def _uninstall(self, app_override: Path | None) -> None:
        lock = self._lock()
        wrapper_succeeded = False
        state_committed = False
        pending_path = self.manager_dir / "pending.json"
        pending_record: dict[str, object] | None = None
        pending_created = False
        wrapper_invoked = False
        wrapper_completed = False
        try:
            self._assert_no_pending_operation()
            state = self._state()
            installed = state.get("installed")
            if not isinstance(installed, dict):
                raise ManagerError("no managed Orbit installation is recorded")
            saved_app = installed.get("app")
            if not isinstance(saved_app, str):
                raise ManagerError("manager state has no saved app path")
            app = app_override or Path(saved_app)
            if str(app) != saved_app:
                raise ManagerError("the requested app path differs from the managed installation")
            if installed.get("orbit_root") != str(self.root):
                raise ManagerError("the requested Orbit root differs from the managed installation")
            cache_id = installed.get("cache_id")
            metadata_sha = installed.get("cache_metadata_sha256")
            if not isinstance(cache_id, str) or not re.fullmatch(r"[0-9A-Za-z.+-]{1,128}", cache_id):
                raise ManagerError("manager state contains an unsafe cached release identifier")
            if not isinstance(metadata_sha, str) or not SHA_RE.fullmatch(metadata_sha):
                raise ManagerError("manager state contains an invalid cache metadata digest")
            release_dir = self.manager_dir / "releases" / cache_id
            validate_release_tree(release_dir, metadata_sha, self.uid)
            wrapper = release_dir / "tree/scripts/install-macos.sh"
            argv = [str(wrapper), "--app", str(app), "--orbit-root", str(self.root), "--no-build", "--uninstall"]
            new_state = {"format": 1, "installed": None, "bootstrap": state.get("bootstrap")}
            pending_record = {
                "format": 1,
                "operation": "uninstall",
                "wrapper_succeeded": False,
                "wrapper_status": "not-started",
                "app": str(app),
                "orbit_root": str(self.root),
                "cache_id": cache_id,
                "cache_metadata_sha256": metadata_sha,
                "state": new_state,
            }
            pending_created = True
            atomic_write(
                pending_path,
                (json.dumps(pending_record, sort_keys=True) + "\n").encode(),
            )
            pending_record["wrapper_status"] = "running"
            atomic_write(
                pending_path,
                (json.dumps(pending_record, sort_keys=True) + "\n").encode(),
            )
            wrapper_invoked = True
            result = self.runner(argv, False)
            wrapper_completed = True
            if result.returncode != 0:
                detail = (getattr(result, "stderr", "") or getattr(result, "stdout", "") or "").strip()
                suffix = f": {detail}" if detail else ""
                raise ManagerError(f"Orbit uninstall wrapper failed with exit code {result.returncode}{suffix}")
            wrapper_succeeded = True
            pending_record["wrapper_succeeded"] = True
            pending_record["wrapper_status"] = "succeeded"
            atomic_write(
                pending_path,
                (json.dumps(pending_record, sort_keys=True) + "\n").encode(),
            )
            atomic_write(
                self.manager_dir / "state.json",
                (json.dumps(new_state, sort_keys=True) + "\n").encode(),
            )
            state_committed = True
            pending_path.unlink()
            fsync_directory(self.manager_dir)
            self.output("Orbit uninstall completed. Orbit data and verified release caches were kept.")
        except BaseException as error:
            if not wrapper_succeeded and wrapper_invoked and not wrapper_completed:
                if pending_record is not None:
                    pending_record["wrapper_status"] = "unknown"
                    try:
                        atomic_write(
                            pending_path,
                            (json.dumps(pending_record, sort_keys=True) + "\n").encode(),
                        )
                    except OSError:
                        pass
                if isinstance(error, (KeyboardInterrupt, SystemExit)):
                    raise
                raise ManagerError(
                    f"Orbit uninstall wrapper completion is unknown; its recovery record was "
                    f"preserved at {pending_path}"
                ) from error
            if wrapper_succeeded and not state_committed:
                raise ManagerError(
                    f"the uninstall wrapper succeeded, but manager state finalization failed: {error}. "
                    f"The pending record is preserved at {pending_path}; inspect it before retrying"
                ) from error
            if not wrapper_succeeded and pending_created and pending_path.exists():
                pending_path.unlink()
            raise
        finally:
            self._unlock(lock)

    def run(
        self,
        command: str,
        *,
        app: Path | None,
        manifest_url: str,
        key_override: Path | None,
        allow_unsigned: bool,
    ) -> None:
        self._prepare_dirs()
        if command == "uninstall":
            if allow_unsigned:
                raise ManagerError("--allow-unsigned does not apply to offline uninstall")
            self._uninstall(app)
            return
        if app is None:
            app = Path(DEFAULT_APP)
        self._install_or_upgrade(command, app, manifest_url, key_override, allow_unsigned)


def require_supported_host() -> None:
    if sys.platform != "darwin" or os.uname().machine != "arm64":
        raise ManagerError("the network manager requires macOS arm64")
    if os.geteuid() == 0:
        raise ManagerError("run the manager as your ordinary user; the installer wrapper handles sudo")
    if sys.version_info < (3, 9):
        raise ManagerError("Python 3.9 or newer is required")


def default_root() -> Path:
    home = Path.home()
    return home.joinpath(*DEFAULT_ROOT_PARTS)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="orbit-manager-macos.sh",
        description="Install, upgrade, or uninstall a verified Parsec Orbit macOS arm64 release.",
    )
    parser.add_argument("command", choices=("install", "upgrade", "uninstall"))
    parser.add_argument("--manifest", default=DEFAULT_MANIFEST, help="HTTPS release-macos.json URL")
    parser.add_argument("--keys", type=Path, help="owned regular file containing trusted Ed25519 public keys")
    parser.add_argument("--allow-unsigned", action="store_true", help="trust HTTPS and manifest digests only")
    parser.add_argument("--app", help=f"Parsec.app path (default: {DEFAULT_APP})")
    parser.add_argument("--orbit-root", help="Orbit data and private manager cache directory")
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    options = parser.parse_args(argv)
    try:
        require_supported_host()
        root = validate_absolute_path(options.orbit_root, "--orbit-root") if options.orbit_root else default_root()
        app = validate_absolute_path(options.app, "--app") if options.app else None
        if options.command == "uninstall" and options.manifest != DEFAULT_MANIFEST:
            raise ManagerError("--manifest does not apply to offline uninstall")
        if options.command == "uninstall" and options.keys is not None:
            raise ManagerError("--keys does not apply to offline uninstall")
        manager = Manager(root)
        manager.run(
            options.command,
            app=app,
            manifest_url=options.manifest,
            key_override=options.keys,
            allow_unsigned=options.allow_unsigned,
        )
        return 0
    except ManagerError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    except OSError as error:
        print(f"error: filesystem operation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

//! Verified atomic seeding of fresh isolated client state.
//!
//! The recorded preseed smoke test places `appdata.json` and the client payload
//! directly under `<home>/.parsec`, and records the loaded image at that path
//! (`docs/design/macos/smoke-test.md`, lines 24-25 and 99-102). This module
//! validates the manifest and payload from the official resource `skel`
//! directory, stages those files under a private sibling directory, and then
//! commits the complete `.parsec` directory with macOS's exclusive atomic
//! rename. An existing state directory is never replaced. The recorded launch
//! evidence belongs to the earlier smoke test; this code change does not
//! establish fresh-client launch behavior or complete the runtime isolation
//! audit. The manifest hash only checks that its payload matches that
//! manifest; it does not authenticate vendor provenance, code signatures or
//! an approved vendor version. The M1b caller/installer must perform those
//! checks.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::{CString, OsStr};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The manifest inside the vendor skel directory.
const MANIFEST_NAME: &str = "appdata.json";

/// The entry symbol the stub resolves in the client payload.
const ENTRY_SYMBOL: &str = "wx_main";

/// The state directory created under the isolated home.
const STATE_DIR: &str = ".parsec";

/// The staging directory prefix. Each candidate also includes pid and a nonce.
const STAGE_PREFIX: &str = ".parsec.stage-";

/// Maximum number of occupied staging names to skip before failing safely.
const STAGE_ATTEMPTS: usize = 1024;

static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// What a successful [`seed`] planted.
#[derive(Debug, PartialEq, Eq)]
pub struct SeedReport {
    /// Every file copied from the skel directory, the manifest included.
    pub files_copied: usize,
    /// The manifest digest the payload matched.
    pub verified_digest: String,
    /// The payload file name from the manifest.
    pub so_name: String,
}

/// The exact manifest fields required by the macOS stub.
///
/// Derive-based decoding rejects malformed JSON, duplicate fields, wrong
/// field types, trailing non-whitespace and unknown fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    entry_symbol: String,
    hash: String,
    so_name: String,
}

fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

/// Resolves an absolute directory path while checking every path component.
/// Relative paths, `.`/`..`, symlink components and non-directory components
/// are rejected before the canonical path is returned.
fn validate_directory_path(path: &Path, label: &str) -> Result<(PathBuf, fs::Metadata), String> {
    if !path.is_absolute() {
        return Err(format!(
            "{label} must be an absolute path: {}",
            path.display()
        ));
    }
    let bytes = path.as_os_str().as_bytes();
    if bytes
        .split(|byte| *byte == b'/')
        .any(|component| component == b"." || component == b"..")
    {
        return Err(format!(
            "unsafe path component in {label} {}",
            path.display()
        ));
    }

    let mut current = PathBuf::from("/");
    for component in bytes
        .split(|byte| *byte == b'/')
        .filter(|part| !part.is_empty())
    {
        current.push(OsStr::from_bytes(component));
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("cannot inspect {label} {}: {error}", current.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "refusing symlinked {label} component {}",
                current.display()
            ));
        }
        if !metadata.file_type().is_dir() {
            return Err(format!(
                "refusing non-directory {label} component {}",
                current.display()
            ));
        }
    }

    let canonical = fs::canonicalize(&current)
        .map_err(|error| format!("cannot resolve {label} {}: {error}", path.display()))?;
    if canonical != current {
        return Err(format!(
            "refusing changed or aliased {label} path {}",
            path.display()
        ));
    }
    let metadata = fs::symlink_metadata(&canonical)
        .map_err(|error| format!("cannot inspect {label} {}: {error}", canonical.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(format!(
            "refusing changed or non-directory {label} {}",
            canonical.display()
        ));
    }
    Ok((canonical, metadata))
}

/// Checks that the source and target roots are real, safe directories.
fn validate_roots(skel_dir: &Path, home_dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let (source, _) = validate_directory_path(skel_dir, "source root")?;
    if source == Path::new("/") {
        return Err(format!(
            "refusing filesystem root as source: {}",
            skel_dir.display()
        ));
    }
    let (home, home_metadata) = validate_directory_path(home_dir, "home root")?;
    if home == Path::new("/") {
        return Err("refusing filesystem root as home".into());
    }
    let effective_uid = unsafe { libc::geteuid() };
    validate_home_metadata(
        home_dir,
        home_metadata.uid(),
        home_metadata.permissions().mode(),
        effective_uid,
    )?;
    if home.starts_with(&source) {
        return Err(format!(
            "unsafe source root {} contains home {}",
            source.display(),
            home.display()
        ));
    }
    Ok((source, home))
}

fn validate_home_metadata(
    home_dir: &Path,
    owner_uid: u32,
    mode: u32,
    effective_uid: u32,
) -> Result<(), String> {
    if owner_uid != effective_uid {
        return Err(format!(
            "home root {} is not owned by the effective user",
            home_dir.display()
        ));
    }
    if mode & 0o7777 != 0o700 {
        return Err(format!(
            "home root {} must have mode 0700",
            home_dir.display()
        ));
    }
    Ok(())
}

/// Reads one regular file without following a final-component symlink.
///
/// Nonblocking open prevents a source replaced with a FIFO after the metadata
/// check from hanging the seed operation. The opened descriptor is checked
/// again before any bytes are read.
fn read_regular_file(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot stat {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("refusing symlinked {}", path.display()));
    }
    if !metadata.file_type().is_file() {
        return Err(format!("refusing special file {}", path.display()));
    }

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| format!("cannot stat opened file {}: {error}", path.display()))?
        .file_type()
        .is_file()
    {
        return Err(format!("refusing special file {}", path.display()));
    }

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    Ok(bytes)
}

/// Reads the manifest bytes from the already-collected source snapshot.
fn parse_manifest(path: &Path, body: &[u8]) -> Result<Manifest, String> {
    let manifest: Manifest = serde_json::from_slice(body)
        .map_err(|error| format!("invalid manifest JSON in {}: {error}", path.display()))?;
    if manifest.entry_symbol != ENTRY_SYMBOL {
        return Err(format!(
            "{} names entry symbol {}, expected {ENTRY_SYMBOL}",
            path.display(),
            manifest.entry_symbol
        ));
    }
    if !valid_sha256_hex(&manifest.hash) {
        return Err(format!(
            "{} hash must contain exactly 64 hexadecimal SHA-256 characters",
            path.display()
        ));
    }
    if manifest.so_name.trim().is_empty() {
        return Err(format!("{} has an empty so_name", path.display()));
    }
    Ok(manifest)
}

/// Resolves the payload inside the skel directory, refusing escapes.
fn payload_relative(so_name: &str) -> Result<PathBuf, String> {
    let relative = Path::new(so_name);
    if so_name.is_empty() || relative.is_absolute() {
        return Err(format!("payload name {so_name:?} is not a relative path"));
    }
    if so_name
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(format!(
            "payload name {so_name:?} escapes the skel directory"
        ));
    }
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(format!(
                "payload name {so_name:?} escapes the skel directory"
            ));
        }
    }
    Ok(relative.to_path_buf())
}

/// Reads every regular source file into a stable, sorted snapshot.
///
/// Symlinks and special files fail closed. The same captured manifest bytes
/// are parsed and staged, so a later source change cannot alter appdata after
/// payload verification.
fn collect_files(dir: &Path, base: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<(), String> {
    let metadata = fs::symlink_metadata(dir)
        .map_err(|error| format!("cannot stat {}: {error}", dir.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("refusing symlinked {}", dir.display()));
    }
    if !metadata.file_type().is_dir() {
        return Err(format!("refusing non-directory {}", dir.display()));
    }

    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| format!("cannot list {}: {error}", dir.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| format!("cannot list {}: {error}", dir.display()))
        })
        .collect::<Result<_, _>>()?;
    entries.sort();

    for path in entries {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot stat {}: {error}", path.display()))?;
        let kind = metadata.file_type();
        if kind.is_symlink() {
            return Err(format!("refusing symlinked {}", path.display()));
        }
        if kind.is_dir() {
            collect_files(&path, base, out)?;
        } else if kind.is_file() {
            let relative = path
                .strip_prefix(base)
                .map_err(|_| format!("{} escapes {}", path.display(), base.display()))?
                .to_path_buf();
            if relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err(format!("unsafe source path {}", relative.display()));
            }
            out.push((relative, read_regular_file(&path)?));
        } else {
            return Err(format!("refusing special file {}", path.display()));
        }
    }
    Ok(())
}

/// Creates a private directory, accepting only a directory made earlier in
/// this staging tree.
fn create_private_dir(path: &Path) -> Result<(), String> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure directory {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)
                .map_err(|error| format!("cannot stat {}: {error}", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                return Err(format!(
                    "refusing non-directory staging path {}",
                    path.display()
                ));
            }
            if metadata.permissions().mode() & 0o777 != 0o700 {
                return Err(format!(
                    "staging directory {} is not private",
                    path.display()
                ));
            }
            Ok(())
        }
        Err(error) => Err(format!(
            "cannot create directory {}: {error}",
            path.display()
        )),
    }
}

fn staging_path(home_dir: &Path, pid: u32, nonce: u64) -> PathBuf {
    home_dir.join(format!("{STAGE_PREFIX}{pid}-{nonce:016x}"))
}

/// Creates a fresh private sibling directory, skipping but never deleting a
/// preexisting candidate.
fn create_staging_with(
    home_dir: &Path,
    mut next_nonce: impl FnMut() -> u64,
) -> Result<PathBuf, String> {
    for _ in 0..STAGE_ATTEMPTS {
        let path = staging_path(home_dir, std::process::id(), next_nonce());
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&path) {
            Ok(()) => {
                if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o700)) {
                    let _ = fs::remove_dir_all(&path);
                    return Err(format!(
                        "cannot secure staging directory {}: {error}",
                        path.display()
                    ));
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "cannot create staging directory {}: {error}",
                    path.display()
                ))
            }
        }
    }
    Err(format!(
        "cannot allocate a fresh staging directory under {}",
        home_dir.display()
    ))
}

fn create_staging(home_dir: &Path) -> Result<PathBuf, String> {
    create_staging_with(home_dir, || STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed))
}

fn create_staged_parents(staging: &Path, relative: &Path) -> Result<(), String> {
    let mut current = staging.to_path_buf();
    let Some(parent) = relative.parent() else {
        return Ok(());
    };
    for component in parent.components() {
        match component {
            Component::Normal(name) => {
                current.push(name);
                create_private_dir(&current)?;
            }
            _ => return Err(format!("unsafe staged path {}", relative.display())),
        }
    }
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot stage {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("cannot write staged file {}: {error}", path.display()))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot secure staged file {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("cannot sync staged file {}: {error}", path.display()))
}

fn sync_staged_directories(staging: &Path, files: &[(PathBuf, Vec<u8>)]) -> Result<(), String> {
    let mut directories = BTreeSet::new();
    for (relative, _) in files {
        let mut child = relative.as_path();
        while let Some(parent) = child.parent() {
            if parent.as_os_str().is_empty() {
                break;
            }
            directories.insert(parent.to_path_buf());
            child = parent;
        }
    }
    let mut directories: Vec<_> = directories.into_iter().collect();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for relative in directories {
        File::open(staging.join(&relative))
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                format!(
                    "cannot sync staged directory {}: {error}",
                    relative.display()
                )
            })?;
    }
    File::open(staging)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "cannot sync staging directory {}: {error}",
                staging.display()
            )
        })
}

/// Atomically renames `source` to an absent destination on macOS.
fn rename_exclusive(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: both strings are NUL-terminated paths, and renamex_np only reads
    // them during the call. RENAME_EXCL prevents replacing a concurrent seed.
    let result =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn require_absent(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(format!("refusing to overwrite existing {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot inspect destination {}: {error}",
            path.display()
        )),
    }
}

/// Plants verified vendor `skel_dir` files directly in `<home>/.parsec`.
///
/// Production always uses SHA-256. Every regular source file is captured
/// before validation, then written with mode `0600` under mode `0700`
/// directories. File and directory contents are synced before the state root
/// is committed with `renamex_np(RENAME_EXCL)`, followed by a parent sync.
/// A matching digest proves only that the payload matches this manifest; the
/// caller (M1b) remains responsible for authenticating vendor provenance,
/// code signatures and the approved vendor version.
/// The installer must keep activation closed until the full isolation audit
/// is complete; this operation alone does not prove a client launch is safe.
pub fn seed(skel_dir: &Path, home_dir: &Path) -> Result<SeedReport, String> {
    seed_with_digest(skel_dir, home_dir, sha256_hex)
}

fn seed_with_digest(
    skel_dir: &Path,
    home_dir: &Path,
    digest: impl Fn(&[u8]) -> String,
) -> Result<SeedReport, String> {
    let (source_root, home_root) = validate_roots(skel_dir, home_dir)?;
    let destination = home_root.join(STATE_DIR);
    require_absent(&destination)?;

    let mut planned = Vec::new();
    collect_files(&source_root, &source_root, &mut planned)?;

    let manifest_path = source_root.join(MANIFEST_NAME);
    let manifest_relative = Path::new(MANIFEST_NAME);
    let manifest_bytes = planned
        .iter()
        .find(|(relative, _)| relative == manifest_relative)
        .map(|(_, bytes)| bytes.as_slice())
        .ok_or_else(|| format!("manifest {} is missing", manifest_path.display()))?;
    let manifest = parse_manifest(&manifest_path, manifest_bytes)?;
    let payload_name = payload_relative(&manifest.so_name)?;
    let payload = planned
        .iter()
        .find(|(relative, _)| *relative == payload_name)
        .map(|(_, bytes)| bytes)
        .ok_or_else(|| {
            format!(
                "payload {} missing from {}",
                manifest.so_name,
                source_root.display()
            )
        })?;
    let computed = digest(payload);
    if !valid_sha256_hex(&computed) {
        return Err("SHA-256 provider returned a non-hex or non-64-character digest".into());
    }
    if !computed.eq_ignore_ascii_case(&manifest.hash) {
        return Err(format!(
            "payload {} does not match the manifest SHA-256",
            payload_name.display()
        ));
    }

    let staging = create_staging(&home_root)?;
    let mut committed = false;
    let result = (|| {
        for (relative, bytes) in &planned {
            create_staged_parents(&staging, relative)?;
            write_private_file(&staging.join(relative), bytes)?;
        }
        sync_staged_directories(&staging, &planned)?;

        match rename_exclusive(&staging, &destination) {
            Ok(()) => committed = true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(format!(
                    "refusing to overwrite existing {}",
                    destination.display()
                ));
            }
            Err(error) => {
                return Err(format!(
                    "cannot atomically commit {}: {error}",
                    destination.display()
                ));
            }
        }

        File::open(&home_root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                format!(
                    "{} was committed but its parent could not be synced: {error}",
                    destination.display()
                )
            })
    })();
    if result.is_err() && !committed {
        let _ = fs::remove_dir_all(&staging);
    }
    result?;

    Ok(SeedReport {
        files_copied: planned.len(),
        verified_digest: manifest.hash,
        so_name: manifest.so_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_core::testutil::TempDir;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{symlink, MetadataExt};
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn fixture_skel() -> (TempDir, String) {
        let skel = TempDir::new("seed-skel");
        let payload = "verified client payload".to_string();
        let manifest = format!(
            "{{\"entry_symbol\":\"wx_main\",\"hash\":\"{}\",\"so_name\":\"parsecd-150-101a.dylib\"}}",
            sha256_hex(payload.as_bytes())
        );
        skel.write(MANIFEST_NAME, &manifest);
        skel.write("parsecd-150-101a.dylib", &payload);
        skel.write("nested/deeper/extra.bin", "extra");
        (skel, payload)
    }

    fn manifest(hash: &str, so_name: &str) -> String {
        format!("{{\"entry_symbol\":\"wx_main\",\"hash\":\"{hash}\",\"so_name\":\"{so_name}\"}}")
    }

    fn state_root(home: &TempDir) -> PathBuf {
        home.path().join(STATE_DIR)
    }

    fn private_home_path(home: &TempDir) -> PathBuf {
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::canonicalize(home.path()).unwrap()
    }

    fn seed_for_test(skel: &Path, home: &TempDir) -> Result<SeedReport, String> {
        let source = fs::canonicalize(skel).unwrap();
        let home = private_home_path(home);
        seed(&source, &home)
    }

    fn assert_no_staging(home: &TempDir) {
        let leftovers: Vec<_> = fs::read_dir(home.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(STAGE_PREFIX))
            .collect();
        assert!(
            leftovers.is_empty(),
            "leftover staging paths: {leftovers:?}"
        );
    }

    #[test]
    fn production_sha256_matches_the_standard_abc_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn plants_verified_vendor_files_directly_at_the_observed_state_path() {
        let (skel, payload) = fixture_skel();
        let home = TempDir::new("seed-home");
        let report = seed_for_test(skel.path(), &home).expect("fresh seed plants state");
        let root = state_root(&home);

        assert_eq!(report.files_copied, 3);
        assert_eq!(report.so_name, "parsecd-150-101a.dylib");
        assert_eq!(report.verified_digest, sha256_hex(payload.as_bytes()));
        assert_eq!(
            fs::read_to_string(root.join(MANIFEST_NAME)).unwrap(),
            manifest(&sha256_hex(payload.as_bytes()), "parsecd-150-101a.dylib")
        );
        assert_eq!(
            fs::read_to_string(root.join("parsecd-150-101a.dylib")).unwrap(),
            payload
        );
        assert_eq!(
            fs::read_to_string(root.join("nested/deeper/extra.bin")).unwrap(),
            "extra"
        );
        assert_no_staging(&home);

        assert_eq!(
            fs::metadata(home.path().join(STATE_DIR)).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(root.join("nested/deeper")).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.join("parsecd-150-101a.dylib"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn malformed_duplicate_unknown_and_trailing_json_fail_closed() {
        let (skel, _) = fixture_skel();
        let home = TempDir::new("seed-home-json");
        let cases = [
            "{\"entry_symbol\":\"wx_main\"".to_string(),
            format!("{} trailing", manifest(&"a".repeat(64), "client.dylib")),
            format!(
                "{{\"entry_symbol\":\"wx_main\",\"hash\":\"{}\",\"hash\":\"{}\",\"so_name\":\"client.dylib\"}}",
                "a".repeat(64),
                "b".repeat(64)
            ),
            format!(
                "{{\"entry_symbol\":\"wx_main\",\"hash\":\"{}\",\"so_name\":\"client.dylib\",\"extra\":true}}",
                "a".repeat(64)
            ),
            "{\"entry_symbol\":\"wx_main\",\"hash\":7,\"so_name\":\"client.dylib\"}".into(),
        ];
        for body in cases {
            fs::write(skel.path().join(MANIFEST_NAME), body).unwrap();
            let error = seed_for_test(skel.path(), &home).unwrap_err();
            assert!(error.contains("manifest JSON"), "{error}");
            assert!(!home.path().join(STATE_DIR).exists());
            assert_no_staging(&home);
        }
    }

    #[test]
    fn rejects_non_sha256_length_and_non_hex_hashes() {
        let (skel, _) = fixture_skel();
        let home = TempDir::new("seed-home-hash-shape");
        let too_short = "a".repeat(63);
        let too_long = "a".repeat(65);
        let non_hex = format!("{}g", "a".repeat(63));
        for hash in ["", too_short.as_str(), too_long.as_str(), non_hex.as_str()] {
            fs::write(
                skel.path().join(MANIFEST_NAME),
                manifest(hash, "parsecd-150-101a.dylib"),
            )
            .unwrap();
            let error = seed_for_test(skel.path(), &home).unwrap_err();
            assert!(error.contains("64 hexadecimal"), "{error}");
            assert!(!home.path().join(STATE_DIR).exists());
        }
        assert_no_staging(&home);
    }

    #[test]
    fn private_digest_helper_rejects_a_malformed_digest_result() {
        let (skel, _) = fixture_skel();
        let home = TempDir::new("seed-home-bad-digest-provider");
        let source = fs::canonicalize(skel.path()).unwrap();
        let private_home = private_home_path(&home);
        let error = seed_with_digest(&source, &private_home, |_| "short".into()).unwrap_err();
        assert!(error.contains("provider"), "{error}");
        assert!(!home.path().join(STATE_DIR).exists());
        assert_no_staging(&home);
    }

    #[test]
    fn digest_mismatch_does_not_create_state_or_staging() {
        let (skel, _) = fixture_skel();
        fs::write(
            skel.path().join("parsecd-150-101a.dylib"),
            "tampered payload",
        )
        .unwrap();
        let home = TempDir::new("seed-home-tampered");
        let error = seed_for_test(skel.path(), &home).unwrap_err();
        assert!(error.contains("does not match"), "{error}");
        assert!(!home.path().join(STATE_DIR).exists());
        assert_no_staging(&home);
    }

    #[test]
    fn rejects_missing_manifest_payload_and_escaping_payload_names() {
        let empty = TempDir::new("seed-skel-empty");
        let home = TempDir::new("seed-home-empty");
        assert!(seed_for_test(empty.path(), &home)
            .unwrap_err()
            .contains("manifest"));

        let missing = TempDir::new("seed-skel-missing-payload");
        missing.write(MANIFEST_NAME, &manifest(&"a".repeat(64), "gone.dylib"));
        assert!(seed_for_test(missing.path(), &home)
            .unwrap_err()
            .contains("missing"));

        let (skel, payload) = fixture_skel();
        let hash = sha256_hex(payload.as_bytes());
        for unsafe_name in ["../outside.dylib", "/tmp/client.dylib", "./client.dylib"] {
            fs::write(
                skel.path().join(MANIFEST_NAME),
                manifest(&hash, unsafe_name),
            )
            .unwrap();
            let error = seed_for_test(skel.path(), &home).unwrap_err();
            assert!(
                error.contains("relative path") || error.contains("escapes"),
                "{error}"
            );
            assert!(!home.path().join(STATE_DIR).exists());
        }
    }

    #[test]
    fn refuses_symlinked_source_root_and_entries() {
        let (skel, _) = fixture_skel();
        let home = TempDir::new("seed-home-symlink");
        let alias_parent = TempDir::new("seed-symlink-parent");
        let alias_parent_path = fs::canonicalize(alias_parent.path()).unwrap();
        let source_path = fs::canonicalize(skel.path()).unwrap();
        let home_path = private_home_path(&home);
        let alias = alias_parent_path.join("source");
        symlink(&source_path, &alias).unwrap();
        assert!(seed(&alias, &home_path)
            .unwrap_err()
            .contains("symlinked source root"));
        let alias_with_trailing_slash = PathBuf::from(format!("{}/", alias.display()));
        assert!(seed(&alias_with_trailing_slash, &home_path)
            .unwrap_err()
            .contains("symlinked source root"));

        let outside = alias_parent_path.join("outside.dylib");
        fs::write(&outside, "target").unwrap();
        fs::remove_file(skel.path().join("parsecd-150-101a.dylib")).unwrap();
        symlink(&outside, skel.path().join("parsecd-150-101a.dylib")).unwrap();
        assert!(seed_for_test(skel.path(), &home)
            .unwrap_err()
            .contains("symlinked"));
        assert!(!home.path().join(STATE_DIR).exists());
        assert_no_staging(&home);
    }

    #[test]
    fn rejects_relative_dot_and_parent_component_roots() {
        let (skel, _) = fixture_skel();
        let home = TempDir::new("seed-home-relative-roots");
        let home_path = private_home_path(&home);
        for source in [
            Path::new("."),
            Path::new("relative/source"),
            Path::new("/./tmp"),
        ] {
            let error = seed(source, &home_path).unwrap_err();
            assert!(
                error.contains("absolute path") || error.contains("unsafe path component"),
                "{error}"
            );
        }
        let source_path = fs::canonicalize(skel.path()).unwrap();
        for relative_home in [Path::new("."), Path::new("relative/home")] {
            assert!(seed(&source_path, relative_home)
                .unwrap_err()
                .contains("absolute path"));
        }
        let dot_home = PathBuf::from(format!("{}/./", home_path.display()));
        assert!(seed(&source_path, &dot_home)
            .unwrap_err()
            .contains("unsafe path component"));
        let parent_home = home_path.join("..").join(
            home_path
                .file_name()
                .expect("temporary home has a final component"),
        );
        assert!(seed(&source_path, &parent_home)
            .unwrap_err()
            .contains("unsafe path component"));

        let file_source = home.path().join("source-file");
        fs::write(&file_source, "not a directory").unwrap();
        let file_source = fs::canonicalize(file_source).unwrap();
        let error = seed(&file_source, &home_path).unwrap_err();
        assert!(
            error.contains("non-directory source root component"),
            "{error}"
        );
        assert!(!home.path().join(STATE_DIR).exists());
    }

    #[test]
    fn rejects_symlinked_ancestor_components_for_source_and_home() {
        let (skel, _) = fixture_skel();
        let source_path = fs::canonicalize(skel.path()).unwrap();
        let home = TempDir::new("seed-home-ancestor-symlink");
        let home_path = private_home_path(&home);
        let aliases = TempDir::new("seed-ancestor-aliases");
        let aliases_path = fs::canonicalize(aliases.path()).unwrap();

        let source_parent = source_path.parent().unwrap();
        let source_parent_alias = aliases_path.join("source-parent");
        symlink(source_parent, &source_parent_alias).unwrap();
        let aliased_source = source_parent_alias.join(source_path.file_name().unwrap());
        assert!(seed(&aliased_source, &home_path)
            .unwrap_err()
            .contains("symlinked source root component"));

        let home_parent = home_path.parent().unwrap();
        let home_parent_alias = aliases_path.join("home-parent");
        symlink(home_parent, &home_parent_alias).unwrap();
        let aliased_home = home_parent_alias.join(home_path.file_name().unwrap());
        assert!(seed(&source_path, &aliased_home)
            .unwrap_err()
            .contains("symlinked home root component"));

        let home_alias = aliases_path.join("home-alias");
        symlink(&home_path, &home_alias).unwrap();
        assert!(seed(&source_path, &home_alias)
            .unwrap_err()
            .contains("symlinked home root component"));
        assert!(!home.path().join(STATE_DIR).exists());
    }

    #[test]
    fn rejects_home_metadata_with_a_different_owner_or_unsafe_mode() {
        let path = Path::new("/private/test-home");
        let owner_error = validate_home_metadata(path, 501, 0o700, 502).unwrap_err();
        assert!(owner_error.contains("not owned by the effective user"));
        let mode_error = validate_home_metadata(path, 502, 0o755, 502).unwrap_err();
        assert!(mode_error.contains("must have mode 0700"));
        assert!(validate_home_metadata(path, 502, 0o700, 502).is_ok());
    }

    #[test]
    fn requires_home_to_be_owned_by_effective_user_and_private() {
        let (skel, _) = fixture_skel();
        let source = fs::canonicalize(skel.path()).unwrap();
        let home = TempDir::new("seed-home-mode");
        let home_path = fs::canonicalize(home.path()).unwrap();
        fs::set_permissions(&home_path, fs::Permissions::from_mode(0o755)).unwrap();
        let error = seed(&source, &home_path).unwrap_err();
        assert!(error.contains("must have mode 0700"), "{error}");
        assert!(!home_path.join(STATE_DIR).exists());

        fs::set_permissions(&home_path, fs::Permissions::from_mode(0o700)).unwrap();
        let report = seed(&source, &home_path).expect("private user-owned home is accepted");
        assert_eq!(report.files_copied, 3);
    }

    #[test]
    fn refuses_special_source_files_without_opening_them() {
        let (skel, _) = fixture_skel();
        let fifo = skel.path().join("special.pipe");
        let c_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: c_path is NUL-terminated and mode is a valid permission mask.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let home = TempDir::new("seed-home-special");
        let error = seed_for_test(skel.path(), &home).unwrap_err();
        assert!(error.contains("special file"), "{error}");
        assert!(!home.path().join(STATE_DIR).exists());
        assert_no_staging(&home);
    }

    #[test]
    fn refuses_filesystem_root_and_source_that_contains_home() {
        let home = TempDir::new("seed-home-unsafe-root");
        let home_path = private_home_path(&home);
        assert!(seed(Path::new("/"), &home_path)
            .unwrap_err()
            .contains("filesystem root"));

        let outer = TempDir::new("seed-root-contains-home");
        let nested_home = outer.path().join("home");
        fs::create_dir(&nested_home).unwrap();
        fs::set_permissions(&nested_home, fs::Permissions::from_mode(0o700)).unwrap();
        let outer_path = fs::canonicalize(outer.path()).unwrap();
        let nested_home_path = fs::canonicalize(&nested_home).unwrap();
        let error = seed(&outer_path, &nested_home_path).unwrap_err();
        assert!(error.contains("contains home"), "{error}");
        assert!(!nested_home.join(STATE_DIR).exists());
    }

    #[test]
    fn never_replaces_existing_state_including_a_dangling_symlink() {
        let (skel, _) = fixture_skel();
        let home = TempDir::new("seed-home-existing");
        let state = home.path().join(STATE_DIR);
        symlink(home.path().join("missing-target"), &state).unwrap();
        let error = seed_for_test(skel.path(), &home).unwrap_err();
        assert!(error.contains("existing"), "{error}");
        assert!(fs::symlink_metadata(&state)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_no_staging(&home);
    }

    #[test]
    fn second_seed_preserves_existing_state_contents() {
        let (skel, _) = fixture_skel();
        let home = TempDir::new("seed-home-existing-state");
        seed_for_test(skel.path(), &home).expect("first seed plants state");
        let state = state_root(&home);
        let payload = state.join("parsecd-150-101a.dylib");
        fs::write(&payload, "owner data").unwrap();

        let error = seed_for_test(skel.path(), &home).unwrap_err();
        assert!(error.contains("existing"), "{error}");
        assert_eq!(fs::read_to_string(payload).unwrap(), "owner data");
        assert_no_staging(&home);
    }

    #[test]
    fn skips_a_preexisting_stage_without_modifying_or_deleting_it() {
        let home = TempDir::new("seed-home-stage-collision");
        let occupied = staging_path(home.path(), std::process::id(), 41);
        fs::create_dir(&occupied).unwrap();
        fs::write(occupied.join("keep.txt"), "preserve").unwrap();
        let mut nonces = [41, 42].into_iter();
        let staging = create_staging_with(home.path(), || nonces.next().unwrap()).unwrap();
        assert_ne!(staging, occupied);
        assert_eq!(
            fs::read_to_string(occupied.join("keep.txt")).unwrap(),
            "preserve"
        );
        assert_eq!(fs::metadata(&staging).unwrap().mode() & 0o777, 0o700);
        fs::remove_dir_all(staging).unwrap();
    }

    #[test]
    fn exclusive_rename_preserves_an_existing_empty_destination() {
        let home = TempDir::new("seed-home-exclusive-rename");
        let source = home.path().join("source");
        let destination = home.path().join(STATE_DIR);
        fs::create_dir(&source).unwrap();
        fs::write(source.join("source.txt"), "source").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("keep.txt"), "destination").unwrap();

        let error = rename_exclusive(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(destination.join("keep.txt")).unwrap(),
            "destination"
        );
        assert_eq!(
            fs::read_to_string(source.join("source.txt")).unwrap(),
            "source"
        );
    }

    #[test]
    fn concurrent_seeds_commit_exactly_one_complete_tree() {
        let (skel, payload) = fixture_skel();
        let home = TempDir::new("seed-home-concurrent");
        let source = fs::canonicalize(skel.path()).unwrap();
        let destination = private_home_path(&home);
        let workers = 8;
        let barrier = Arc::new(Barrier::new(workers));
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let source = source.clone();
                let destination = destination.clone();
                thread::spawn(move || {
                    barrier.wait();
                    seed(&source, &destination)
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("seed thread must not panic"))
            .collect();

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let root = state_root(&home);
        assert_eq!(
            fs::read_to_string(root.join("parsecd-150-101a.dylib")).unwrap(),
            payload
        );
        assert_eq!(
            fs::read_to_string(root.join(MANIFEST_NAME)).unwrap(),
            manifest(&sha256_hex(payload.as_bytes()), "parsecd-150-101a.dylib")
        );
        assert_no_staging(&home);
    }
}

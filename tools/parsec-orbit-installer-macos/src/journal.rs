//! Installation records: the install-directory layout, the atomic manifest
//! and the append-only journal.
//!
//! Every transaction lives in `<orbit_root>/installs/<installation-id>/`:
//! the manifest records the current state and the pristine/applied snapshots,
//! the journal appends one JSON line per event and is never rewritten, and
//! the `pristine` directory is the only copy of the vendor bundle the
//! installer is allowed to restore from. The manifest is replaced atomically
//! (write, fsync, rename) so an interrupted transaction is always classifiable
//! from what is on disk.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::digest::Tree;
use crate::platform;

/// Schema version of `manifest.json`.
pub const SCHEMA_VERSION: u32 = 1;
pub const MANIFEST_FILE: &str = "manifest.json";
pub const JOURNAL_FILE: &str = "journal.jsonl";
/// The renamed original bundle.
pub const PRISTINE_DIR: &str = "pristine";
/// The patched bundle before the swap.
pub const STAGED_DIR: &str = "staged";

/// Seconds since the Unix epoch.
pub fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// `<bundle id>@<first 12 hex digits of the pristine stub hash>`.
pub fn installation_id(bundle_id: &str, stub_sha256: &str) -> String {
    let prefix: String = stub_sha256.chars().take(12).collect();
    format!("{bundle_id}@{prefix}")
}

/// Where one installation's records live.
#[derive(Debug, Clone)]
pub struct InstallDir {
    root: PathBuf,
}

impl InstallDir {
    pub fn new(orbit_root: &Path, installation_id: &str) -> InstallDir {
        InstallDir {
            root: orbit_root.join("installs").join(installation_id),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn pristine(&self) -> PathBuf {
        self.root.join(PRISTINE_DIR)
    }

    pub fn staged(&self) -> PathBuf {
        self.root.join(STAGED_DIR)
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    pub fn journal_path(&self) -> PathBuf {
        self.root.join(JOURNAL_FILE)
    }

    pub fn entitlements_path(&self) -> PathBuf {
        self.root.join(crate::ENTITLEMENTS_FILE)
    }

    /// Creates the install directory as a private (0700) directory when it
    /// does not exist yet; an existing directory must already be private.
    pub fn ensure(&self) -> Result<(), String> {
        if self.root.exists() {
            return platform::require_private_dir(&self.root, "install directory");
        }
        if let Some(parent) = self.root.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        platform::create_private_dir(&self.root)
    }
}

/// The transaction state recorded in the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallState {
    /// The manifest exists, the backup may or may not have been renamed yet.
    Preparing,
    /// The pristine backup exists; the app path is being rebuilt.
    BackedUp,
    /// The patched bundle is installed and verified.
    Installed,
    /// A failure or interruption restored the pristine app.
    RolledBack,
    /// An uninstall restored the pristine app.
    Uninstalled,
}

/// The record consumed by the installer itself (the loader has its own
/// record inside the bundle).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub installation_id: String,
    pub bundle_id: String,
    /// Canonical selected bundle path.
    pub app: PathBuf,
    /// Canonical `Contents/MacOS/parsecd` inside the selected bundle.
    pub executable: PathBuf,
    /// Canonical Orbit state root.
    pub orbit_root: PathBuf,
    pub stub_sha256: String,
    /// SHA-256 of the loader input.
    pub loader_sha256: String,
    /// SHA-256 of the loader as installed (the input plus the ad-hoc
    /// signature the installer adds); `None` until `sign-loader` ran.
    pub applied_loader_sha256: Option<String>,
    pub applied_stub_sha256: Option<String>,
    /// Snapshot of the input bundle before any mutation.
    pub pristine_tree: Tree,
    /// Snapshot of the verified staged bundle, recorded before the swap.
    pub applied_tree: Option<Tree>,
    pub state: InstallState,
    pub updated_unix: u64,
}

/// Reads the manifest; `Ok(None)` means the install directory has no record.
pub fn read_manifest(dir: &InstallDir) -> Result<Option<Manifest>, String> {
    let path = dir.manifest_path();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{}: invalid manifest: {error}", path.display()))?;
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "{}: unsupported manifest schema {}",
            path.display(),
            manifest.schema_version
        ));
    }
    if manifest.installation_id != dir_name(dir) {
        return Err(format!(
            "{}: manifest names installation {} but lives in {}",
            path.display(),
            manifest.installation_id,
            dir_name(dir)
        ));
    }
    Ok(Some(manifest))
}

fn dir_name(dir: &InstallDir) -> String {
    dir.root()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Replaces the manifest atomically: private temporary file, fsync, rename.
pub fn write_manifest(dir: &InstallDir, manifest: &Manifest) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("cannot serialize the manifest: {error}"))?;
    dir.ensure()?;
    let temporary = dir.root().join("manifest.json.tmp");
    {
        let mut file = platform::create_private_file(&temporary)?;
        file.write_all(&json)
            .map_err(|error| format!("{}: {error}", temporary.display()))?;
        file.write_all(b"\n")
            .map_err(|error| format!("{}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("{}: {error}", temporary.display()))?;
    }
    fs::rename(&temporary, dir.manifest_path())
        .map_err(|error| format!("cannot replace {}: {error}", dir.manifest_path().display()))?;
    platform::fsync_dir(dir.root())
}

/// One append-only journal entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub unix: u64,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

/// Appends events to `journal.jsonl`; the file is opened append-only and
/// never rewritten.
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    pub fn new(dir: &InstallDir) -> Journal {
        Journal {
            path: dir.journal_path(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, event: &str, detail: Option<serde_json::Value>) -> Result<(), String> {
        let entry = JournalEntry {
            unix: unix_time(),
            event: event.to_string(),
            detail,
        };
        let mut line = serde_json::to_vec(&entry)
            .map_err(|error| format!("cannot serialize a journal entry: {error}"))?;
        line.push(b'\n');
        let mut file = platform::open_append_private(&self.path)?;
        file.write_all(&line)
            .map_err(|error| format!("{}: {error}", self.path.display()))?;
        file.sync_data()
            .map_err(|error| format!("{}: {error}", self.path.display()))?;
        Ok(())
    }

    /// Every entry currently in the journal, oldest first.
    pub fn entries(&self) -> Result<Vec<JournalEntry>, String> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("{}: {error}", self.path.display())),
        };
        let mut entries = Vec::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            entries.push(serde_json::from_str(line).map_err(|error| {
                format!("{}: line {}: {error}", self.path.display(), index + 1)
            })?);
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_core::testutil::TempDir;

    fn sample(dir: &InstallDir, state: InstallState) -> Manifest {
        Manifest {
            schema_version: SCHEMA_VERSION,
            installation_id: dir_name(dir),
            bundle_id: "tv.parsec.www".into(),
            app: PathBuf::from("/Applications/Parsec.app"),
            executable: PathBuf::from("/Applications/Parsec.app/Contents/MacOS/parsecd"),
            orbit_root: PathBuf::from("/Users/test/Library/Application Support/ParsecOrbit"),
            stub_sha256: "ab".repeat(32),
            loader_sha256: "cd".repeat(32),
            applied_loader_sha256: None,
            applied_stub_sha256: None,
            pristine_tree: Tree::new(),
            applied_tree: None,
            state,
            updated_unix: unix_time(),
        }
    }

    #[test]
    fn installation_ids_use_the_bundle_id_and_hash_prefix() {
        assert_eq!(
            installation_id("tv.parsec.www", &"785f985d7f0c".repeat(6)),
            "tv.parsec.www@785f985d7f0c"
        );
    }

    #[test]
    fn manifests_round_trip_and_reject_foreign_directories() {
        let scratch = TempDir::new("installer-journal");
        let dir = InstallDir::new(scratch.path(), "tv.parsec.www@785f985d7f0c");
        dir.ensure().unwrap();
        assert!(read_manifest(&dir).unwrap().is_none());
        let manifest = sample(&dir, InstallState::Preparing);
        write_manifest(&dir, &manifest).unwrap();
        let read = read_manifest(&dir).unwrap().unwrap();
        assert_eq!(read.state, InstallState::Preparing);
        assert_eq!(read.bundle_id, manifest.bundle_id);

        let other = InstallDir::new(scratch.path(), "other@000000000000");
        other.ensure().unwrap();
        fs::copy(dir.manifest_path(), other.manifest_path()).unwrap();
        let error = read_manifest(&other).unwrap_err();
        assert!(error.contains("lives in"), "{error}");
    }

    #[test]
    fn the_journal_appends_and_never_rewrites() {
        let scratch = TempDir::new("installer-journal-append");
        let dir = InstallDir::new(scratch.path(), "id@0123456789ab");
        dir.ensure().unwrap();
        let journal = Journal::new(&dir);
        journal
            .append("begin", Some(serde_json::json!({"step": "backup"})))
            .unwrap();
        journal.append("end", None).unwrap();
        let entries = journal.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].event, "begin");
        assert_eq!(entries[1].event, "end");
    }
}

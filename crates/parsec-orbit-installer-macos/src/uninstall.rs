//! Uninstall: restore exactly the pristine bundle the apply transaction
//! recorded, keep the Orbit data unless purging is requested.
//!
//! The uninstall never guesses. It needs a recorded `Installed` transaction
//! whose applied snapshot still matches the bundle on disk; a vendor update
//! or any external change refuses before the first rename, so a stale backup
//! can never overwrite a newer installation. The patched bundle is moved
//! aside (not deleted) as `applied-removed` evidence and the pristine bundle
//! is renamed back into place.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::digest;
use crate::journal::{self, InstallDir, InstallState, Journal, Manifest};
use crate::platform;
use crate::state::{self, AppClass};
use crate::whitelist::Whitelist;
use crate::STUB_RELATIVE;

/// Data directories and files removed by `--purge-data`, relative to the
/// Orbit root. The installs directory keeps the manifest and journal as
/// provenance.
const PURGE_PATHS: &[&str] = &["home", "plugins", "data", "market", "parsec-orbit.log"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UninstallStep {
    Inspect,
    FindRecord,
    CheckApplied,
    RemoveApplied,
    Restore,
    VerifyRestored,
    Purge,
    Commit,
}

impl UninstallStep {
    pub fn name(self) -> &'static str {
        match self {
            UninstallStep::Inspect => "inspect",
            UninstallStep::FindRecord => "find-record",
            UninstallStep::CheckApplied => "check-applied",
            UninstallStep::RemoveApplied => "remove-applied",
            UninstallStep::Restore => "restore",
            UninstallStep::VerifyRestored => "verify-restored",
            UninstallStep::Purge => "purge",
            UninstallStep::Commit => "commit",
        }
    }
}

/// Failure injection for tests, mirroring [`crate::apply::Faults`].
pub trait UninstallFaults {
    fn check(&self, step: UninstallStep) -> Result<(), String>;
}

impl UninstallFaults for () {
    fn check(&self, _step: UninstallStep) -> Result<(), String> {
        Ok(())
    }
}

/// Caller-provided settings.
#[derive(Debug, Clone)]
pub struct UninstallOptions {
    pub orbit_root: PathBuf,
    /// Remove the Orbit data directories after a successful restore.
    pub purge_data: bool,
}

impl UninstallOptions {
    pub fn new(orbit_root: impl Into<PathBuf>) -> UninstallOptions {
        UninstallOptions {
            orbit_root: orbit_root.into(),
            purge_data: false,
        }
    }
}

/// The uninstall record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallReport {
    pub installation_id: String,
    pub app: PathBuf,
    pub install_dir: PathBuf,
    pub restored_stub_sha256: String,
    /// Where the patched bundle was moved instead of being deleted.
    pub removed: PathBuf,
    pub purged: bool,
    pub purged_paths: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallFailure {
    pub step: UninstallStep,
    pub message: String,
    /// Whether the pristine bundle was put back after a partial removal.
    pub restored: bool,
    pub warnings: Vec<String>,
}

impl fmt::Display for UninstallFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "uninstall failed at {}: {}",
            self.step.name(),
            self.message
        )
    }
}

impl std::error::Error for UninstallFailure {}

/// Restores the pristine bundle recorded for `app`.
pub fn uninstall(
    app: &Path,
    options: &UninstallOptions,
) -> Result<UninstallReport, UninstallFailure> {
    uninstall_with(app, options, &())
}

/// Restores the pristine bundle, consulting `faults` before every step.
pub fn uninstall_with(
    app: &Path,
    options: &UninstallOptions,
    faults: &dyn UninstallFaults,
) -> Result<UninstallReport, UninstallFailure> {
    let _lock = crate::lock::AppLock::acquire(app).map_err(|message| UninstallFailure {
        step: UninstallStep::Inspect,
        message,
        restored: false,
        warnings: Vec::new(),
    })?;
    let mut transaction = Transaction {
        faults,
        options,
        app: app.to_path_buf(),
        orbit_root: options.orbit_root.clone(),
        install: InstallDir::new(&options.orbit_root, "unresolved"),
        manifest: None,
        removed: PathBuf::new(),
        purged_paths: Vec::new(),
        warnings: Vec::new(),
        step: UninstallStep::Inspect,
    };
    transaction.run()
}

struct Transaction<'a> {
    faults: &'a dyn UninstallFaults,
    options: &'a UninstallOptions,
    app: PathBuf,
    orbit_root: PathBuf,
    install: InstallDir,
    manifest: Option<Manifest>,
    removed: PathBuf,
    purged_paths: Vec<PathBuf>,
    warnings: Vec<String>,
    step: UninstallStep,
}

impl Transaction<'_> {
    fn run(&mut self) -> Result<UninstallReport, UninstallFailure> {
        match self.execute() {
            Ok(report) => Ok(report),
            Err((step, message)) => {
                let mut restored = false;
                if matches!(
                    step,
                    UninstallStep::RemoveApplied
                        | UninstallStep::Restore
                        | UninstallStep::VerifyRestored
                        | UninstallStep::Purge
                        | UninstallStep::Commit
                ) {
                    match self.restore_if_needed() {
                        Ok(warnings) => {
                            self.warnings.extend(warnings);
                            restored = true;
                        }
                        Err(error) => {
                            return Err(UninstallFailure {
                                step,
                                message: format!("{message}; the restore also failed: {error}"),
                                restored: false,
                                warnings: self.warnings.clone(),
                            });
                        }
                    }
                }
                Err(UninstallFailure {
                    step,
                    message,
                    restored,
                    warnings: self.warnings.clone(),
                })
            }
        }
    }

    fn execute(&mut self) -> Result<UninstallReport, (UninstallStep, String)> {
        self.step = UninstallStep::Inspect;
        self.check()?;
        self.inspect().map_err(|error| self.at(error))?;

        self.step = UninstallStep::FindRecord;
        self.check()?;
        self.find_record().map_err(|error| self.at(error))?;

        self.step = UninstallStep::CheckApplied;
        self.check()?;
        self.check_applied().map_err(|error| self.at(error))?;

        self.step = UninstallStep::RemoveApplied;
        self.check()?;
        self.remove_applied().map_err(|error| self.at(error))?;

        self.step = UninstallStep::Restore;
        self.check()?;
        self.restore().map_err(|error| self.at(error))?;

        self.step = UninstallStep::VerifyRestored;
        self.check()?;
        self.verify_restored().map_err(|error| self.at(error))?;

        self.step = UninstallStep::Purge;
        self.check()?;
        self.purge().map_err(|error| self.at(error))?;

        self.step = UninstallStep::Commit;
        self.check()?;
        self.commit().map_err(|error| self.at(error))?;

        let manifest = self.manifest.as_ref().expect("recorded by find-record");
        Ok(UninstallReport {
            installation_id: manifest.installation_id.clone(),
            app: self.app.clone(),
            install_dir: self.install.root().to_path_buf(),
            restored_stub_sha256: manifest.stub_sha256.clone(),
            removed: self.removed.clone(),
            purged: self.options.purge_data,
            purged_paths: self.purged_paths.clone(),
            warnings: self.warnings.clone(),
        })
    }

    fn check(&self) -> Result<(), (UninstallStep, String)> {
        self.faults
            .check(self.step)
            .map_err(|error| (self.step, error))
    }

    fn at(&self, message: String) -> (UninstallStep, String) {
        (self.step, message)
    }

    fn journal(&self) -> Journal {
        Journal::new(&self.install)
    }

    fn inspect(&mut self) -> Result<(), String> {
        if !self.app.is_absolute() {
            return Err(format!(
                "the selected app must be an absolute path: {}",
                self.app.display()
            ));
        }
        let metadata = fs::symlink_metadata(&self.app)
            .map_err(|error| format!("{}: {error}", self.app.display()))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err(format!(
                "{} is not a real bundle directory",
                self.app.display()
            ));
        }
        self.app = fs::canonicalize(&self.app)
            .map_err(|error| format!("{}: {error}", self.app.display()))?;
        if !self.options.orbit_root.is_absolute() {
            return Err(format!(
                "the Orbit state root must be an absolute path: {}",
                self.options.orbit_root.display()
            ));
        }
        self.orbit_root = platform::canonical_absolute(&self.options.orbit_root)?;
        let executable = self.app.join(STUB_RELATIVE);
        let running = platform::running_executables(&executable)?;
        if !running.is_empty() {
            return Err(format!(
                "the selected app is running (pid {running:?}); quit it and retry"
            ));
        }
        Ok(())
    }

    /// Finds the installation record whose `app` names the selected bundle.
    fn find_record(&mut self) -> Result<(), String> {
        match state::find_manifest(&self.orbit_root, &self.app)? {
            Some(manifest) => {
                self.install = InstallDir::new(&self.orbit_root, &manifest.installation_id);
                self.manifest = Some(manifest);
                Ok(())
            }
            None => Err(format!(
                "no recorded installation for {} under {}",
                self.app.display(),
                self.orbit_root.join("installs").display()
            )),
        }
    }

    fn check_applied(&mut self) -> Result<(), String> {
        let manifest = self.manifest.as_ref().expect("recorded by find-record");
        if manifest.state != InstallState::Installed {
            return Err(format!(
                "the recorded state of {} is {:?}, not an installed apply",
                self.install.root().display(),
                manifest.state
            ));
        }
        let classification = state::classify(&self.app, Some(manifest), &Whitelist::supported())?;
        if classification.class != AppClass::Applied {
            return Err(format!(
                "{} no longer matches the recorded apply ({}); refusing to \
                 overwrite it with the pristine backup",
                self.app.display(),
                classification.differences.join("; ")
            ));
        }
        Ok(())
    }

    fn remove_applied(&mut self) -> Result<(), String> {
        self.check_applied()?;
        self.removed = self.move_aside()?;
        self.journal().append(
            "applied-removed",
            Some(serde_json::json!({"path": self.removed})),
        )?;
        Ok(())
    }

    fn restore(&mut self) -> Result<(), String> {
        let pristine = self.install.pristine();
        fs::rename(&pristine, &self.app).map_err(|error| {
            format!(
                "cannot restore {} from {}: {error}",
                self.app.display(),
                pristine.display()
            )
        })?;
        if let Some(parent) = self.app.parent() {
            platform::fsync_dir(parent)?;
        }
        Ok(())
    }

    fn verify_restored(&mut self) -> Result<(), String> {
        let manifest = self.manifest.as_ref().expect("recorded by find-record");
        let actual = digest::tree(&self.app)?;
        let differences = digest::differences(&manifest.pristine_tree, &actual);
        if !differences.is_empty() {
            return Err(format!(
                "the restored bundle differs from the recorded pristine snapshot: {}",
                differences.join("; ")
            ));
        }
        if let Err(error) = platform::codesign_verify(&self.app) {
            self.warnings.push(format!(
                "the restored bundle fails strict signature verification: {error}"
            ));
        }
        let journal = self.journal();
        let _ = journal.append(
            "restored",
            Some(serde_json::json!({
                "stub_sha256": manifest.stub_sha256,
                "warnings": self.warnings.len(),
            })),
        );
        Ok(())
    }

    fn purge(&mut self) -> Result<(), String> {
        if !self.options.purge_data {
            return Ok(());
        }
        platform::require_private_dir(&self.orbit_root, "Orbit root")?;
        for relative in PURGE_PATHS {
            let path = self.orbit_root.join(relative);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("{}: {error}", path.display())),
            };
            if metadata.file_type().is_dir() {
                fs::remove_dir_all(&path)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
            } else {
                fs::remove_file(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            }
            self.purged_paths.push(path);
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<(), String> {
        let mut manifest = self.manifest.take().expect("recorded by find-record");
        manifest.state = InstallState::Uninstalled;
        manifest.updated_unix = journal::unix_time();
        journal::write_manifest(&self.install, &manifest)?;
        self.manifest = Some(manifest);
        self.journal().append(
            "uninstalled",
            Some(serde_json::json!({
                "purged": self.options.purge_data,
                "purged_paths": self.purged_paths,
            })),
        )?;
        Ok(())
    }

    /// Puts the pristine bundle back after a partial removal. Returns
    /// non-fatal warnings; a second call is a no-op.
    fn restore_if_needed(&mut self) -> Result<Vec<String>, String> {
        let mut warnings = Vec::new();
        let pristine = self.install.pristine();
        if !pristine.exists() {
            // Either nothing was removed yet, or the restore already ran.
            return Ok(warnings);
        }
        if fs::symlink_metadata(&self.app).is_ok() {
            let actual = digest::tree(&self.app)?;
            let expected = self
                .manifest
                .as_ref()
                .and_then(|record| record.applied_tree.as_ref())
                .ok_or("occupied app path has no recorded applied snapshot")?;
            if !digest::differences(expected, &actual).is_empty() {
                return Err(
                    "the app path changed externally; preserving it and the pristine backup".into(),
                );
            }
            let moved = self.move_aside()?;
            let journal = self.journal();
            let _ = journal.append("moved-aside", Some(serde_json::json!({"path": moved})));
        }
        fs::rename(&pristine, &self.app).map_err(|error| {
            format!(
                "cannot restore {} from {}: {error}",
                self.app.display(),
                pristine.display()
            )
        })?;
        if let Some(parent) = self.app.parent() {
            platform::fsync_dir(parent)?;
        }
        if let Some(manifest) = self.manifest.as_ref() {
            let actual = digest::tree(&self.app)?;
            let differences = digest::differences(&manifest.pristine_tree, &actual);
            if !differences.is_empty() {
                return Err(format!(
                    "the restored bundle differs from the recorded pristine \
                     snapshot: {}",
                    differences.join("; ")
                ));
            }
        }
        if let Err(error) = platform::codesign_verify(&self.app) {
            warnings.push(format!(
                "the restored bundle fails strict signature verification: {error}"
            ));
        }
        let journal = self.journal();
        let _ = journal.append(
            "rolled-back",
            Some(serde_json::json!({"reason": "uninstall failure"})),
        );
        Ok(warnings)
    }

    /// Renames the patched bundle out of the app path without deleting it.
    fn move_aside(&self) -> Result<PathBuf, String> {
        for attempt in 0..16 {
            let name = if attempt == 0 {
                "applied-removed".to_string()
            } else {
                format!("applied-removed-{attempt}")
            };
            let destination = self.install.root().join(name);
            if destination.exists() {
                continue;
            }
            fs::rename(&self.app, &destination).map_err(|error| {
                format!(
                    "cannot move {} to {}: {error}",
                    self.app.display(),
                    destination.display()
                )
            })?;
            return Ok(destination);
        }
        Err(format!(
            "cannot find a free name for the removed bundle in {}",
            self.install.root().display()
        ))
    }
}

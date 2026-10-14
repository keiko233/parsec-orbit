//! Transactional apply: inspect, back up, stage, patch, sign, verify, swap.
//!
//! The order is fixed and every mutation is recorded before it happens
//! ([`Manifest`] plus the append-only journal): the input is inspected and
//! verified before anything changes, an interrupted transaction is rolled
//! back on the next run, the original bundle is kept as the pristine backup
//! and is the only source for a restore, and a failure after the backup
//! renames that pristine bundle back before the error is reported. A
//! re-applied, already-installed bundle is a verified no-op.
//!
//! Nothing here escalates privileges and nothing touches a running app: the
//! process check binds the selected executable path, and the caller decides
//! where the Orbit state root is (the CLI defaults to
//! `~/Library/Application Support/ParsecOrbit`).

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::digest::{self, Tree};
use crate::journal::{self, InstallDir, InstallState, Journal, Manifest, SCHEMA_VERSION};
use crate::macho::{self, PatchReport};
use crate::platform::{self, effective_uid};
use crate::state::{self, AppClass};
use crate::whitelist::Whitelist;
use crate::{LOADER_DEPENDENCY, LOADER_RELATIVE, RECORD_RELATIVE, SKEL_RELATIVE, STUB_RELATIVE};

/// Entitlements applied to the outer bundle: Hardened Runtime plus the
/// measured disable-library-validation exception only.
const ENTITLEMENTS_PLIST: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>com.apple.security.cs.disable-library-validation</key>\n\
\t<true/>\n\
</dict>\n\
</plist>\n";

/// Paths a successful apply may add or change inside the bundle; every other
/// difference from the pristine snapshot fails the staged verification.
const ALLOWED_STAGED_CHANGES: &[&str] = &[
    STUB_RELATIVE,
    LOADER_RELATIVE,
    "Contents/Frameworks",
    RECORD_RELATIVE,
    "Contents/_CodeSignature/CodeResources",
    "Contents/_CodeSignature",
];

/// The transaction steps, in order. Fault injection names them; the journal
/// records them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    Inspect,
    Recover,
    Backup,
    Stage,
    Patch,
    SignLoader,
    SignBundle,
    VerifyStaged,
    Swap,
    VerifyInstalled,
    Preseed,
    Commit,
}

impl Step {
    pub fn name(self) -> &'static str {
        match self {
            Step::Inspect => "inspect",
            Step::Recover => "recover",
            Step::Backup => "backup",
            Step::Stage => "stage",
            Step::Patch => "patch",
            Step::SignLoader => "sign-loader",
            Step::SignBundle => "sign-bundle",
            Step::VerifyStaged => "verify-staged",
            Step::Swap => "swap",
            Step::VerifyInstalled => "verify-installed",
            Step::Preseed => "preseed",
            Step::Commit => "commit",
        }
    }

    /// Whether a failure at this step must restore the pristine bundle.
    fn mutates(self) -> bool {
        !matches!(self, Step::Inspect | Step::Recover)
    }
}

/// Failure injection for tests. A step error aborts the transaction at that
/// point; the normal rollback rules still apply, except for `Preseed`, where
/// a fault becomes a recorded warning.
pub trait Faults {
    fn check(&self, step: Step) -> Result<(), String>;
}

impl Faults for () {
    fn check(&self, _step: Step) -> Result<(), String> {
        Ok(())
    }
}

/// Caller-provided settings.
#[derive(Debug, Clone)]
pub struct ApplyOptions {
    /// Orbit state root; created privately when missing.
    pub orbit_root: PathBuf,
    pub whitelist: Whitelist,
}

impl ApplyOptions {
    pub fn new(orbit_root: impl Into<PathBuf>) -> ApplyOptions {
        ApplyOptions {
            orbit_root: orbit_root.into(),
            whitelist: Whitelist::supported(),
        }
    }

    pub fn with_whitelist(mut self, whitelist: Whitelist) -> ApplyOptions {
        self.whitelist = whitelist;
        self
    }
}

/// How the request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplyOutcome {
    Applied,
    AlreadyApplied,
}

/// What the best-effort isolated-state preseed did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreseedOutcome {
    Seeded,
    Skipped,
    Warned,
}

/// The transaction record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyReport {
    pub outcome: ApplyOutcome,
    pub installation_id: String,
    pub install_dir: PathBuf,
    pub journal: PathBuf,
    pub app: PathBuf,
    pub bundle_id: String,
    pub stub_sha256: String,
    pub loader_sha256: String,
    pub applied_loader_sha256: Option<String>,
    pub applied_stub_sha256: Option<String>,
    pub preseed: PreseedOutcome,
    pub preseed_detail: String,
    pub patch: Option<PatchReport>,
    pub warnings: Vec<String>,
}

impl ApplyReport {
    pub fn outcome_text(&self) -> &'static str {
        match self.outcome {
            ApplyOutcome::Applied => "applied",
            ApplyOutcome::AlreadyApplied => "already applied",
        }
    }

    pub fn preseed_text(&self) -> &'static str {
        match self.preseed {
            PreseedOutcome::Seeded => "seeded",
            PreseedOutcome::Skipped => "skipped",
            PreseedOutcome::Warned => "warned",
        }
    }
}

/// A refused or interrupted apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyFailure {
    pub step: Step,
    pub message: String,
    /// Whether the pristine bundle was restored before returning.
    pub rolled_back: bool,
    pub warnings: Vec<String>,
}

impl fmt::Display for ApplyFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "apply failed at {}: {}",
            self.step.name(),
            self.message
        )
    }
}

impl std::error::Error for ApplyFailure {}

/// Applies the patch with the shipped whitelist and no fault injection.
pub fn apply(
    app: &Path,
    loader: &Path,
    options: &ApplyOptions,
) -> Result<ApplyReport, ApplyFailure> {
    apply_with(app, loader, options, &())
}

/// Applies the patch, consulting `faults` before every step.
pub fn apply_with(
    app: &Path,
    loader: &Path,
    options: &ApplyOptions,
    faults: &dyn Faults,
) -> Result<ApplyReport, ApplyFailure> {
    let _lock = crate::lock::AppLock::acquire(app).map_err(|message| ApplyFailure {
        step: Step::Inspect,
        message,
        rolled_back: false,
        warnings: Vec::new(),
    })?;
    let mut transaction = Transaction::new(app, loader, options, faults);
    transaction.run()
}

struct Transaction<'a> {
    faults: &'a dyn Faults,
    options: &'a ApplyOptions,
    app: PathBuf,
    executable: PathBuf,
    bundle_id: String,
    stub_sha256: String,
    pristine_tree: Tree,
    staged_pristine_tree: Option<Tree>,
    loader_source: PathBuf,
    loader_sha256: String,
    orbit_root: PathBuf,
    install: InstallDir,
    journal: Journal,
    manifest: Option<Manifest>,
    patch: Option<PatchReport>,
    staged_tree: Option<Tree>,
    preseed: (PreseedOutcome, String),
    warnings: Vec<String>,
    step: Step,
}

impl<'a> Transaction<'a> {
    fn new(app: &Path, loader: &Path, options: &'a ApplyOptions, faults: &'a dyn Faults) -> Self {
        Transaction {
            faults,
            options,
            app: app.to_path_buf(),
            executable: PathBuf::new(),
            bundle_id: String::new(),
            stub_sha256: String::new(),
            pristine_tree: Tree::new(),
            staged_pristine_tree: None,
            loader_source: loader.to_path_buf(),
            loader_sha256: String::new(),
            orbit_root: options.orbit_root.clone(),
            // Replaced in `inspect`, once the bundle id is known.
            install: InstallDir::new(&options.orbit_root, "unresolved"),
            journal: Journal::new(&InstallDir::new(&options.orbit_root, "unresolved")),
            manifest: None,
            patch: None,
            staged_tree: None,
            preseed: (PreseedOutcome::Skipped, String::new()),
            warnings: Vec::new(),
            step: Step::Inspect,
        }
    }

    fn run(&mut self) -> Result<ApplyReport, ApplyFailure> {
        match self.execute() {
            Ok(report) => Ok(report),
            Err((step, message)) => {
                if step.mutates() {
                    match self.roll_back(&format!("failed at {}: {message}", step.name())) {
                        Ok(warnings) => {
                            let mut warnings = warnings;
                            warnings.extend(self.warnings.iter().cloned());
                            Err(ApplyFailure {
                                step,
                                message,
                                rolled_back: true,
                                warnings,
                            })
                        }
                        Err(error) => Err(ApplyFailure {
                            step,
                            message: format!("{message}; the rollback also failed: {error}"),
                            rolled_back: false,
                            warnings: self.warnings.clone(),
                        }),
                    }
                } else {
                    Err(ApplyFailure {
                        step,
                        message,
                        rolled_back: false,
                        warnings: self.warnings.clone(),
                    })
                }
            }
        }
    }

    fn execute(&mut self) -> Result<ApplyReport, (Step, String)> {
        // Recovery runs before inspection: an interrupted transaction may
        // have renamed the app away, so the selected path cannot be hashed
        // until the pristine bundle is back in place.
        self.step = Step::Recover;
        self.check()?;
        self.recover_before_inspect()
            .map_err(|error| self.at(error))?;

        self.step = Step::Inspect;
        self.check()?;
        self.inspect().map_err(|error| self.at(error))?;

        self.step = Step::Recover;
        if let Some(report) = self.recover().map_err(|error| self.at(error))? {
            return Ok(report);
        }

        self.step = Step::Backup;
        self.check()?;
        self.backup().map_err(|error| self.at(error))?;

        self.step = Step::Stage;
        self.check()?;
        self.stage().map_err(|error| self.at(error))?;

        self.step = Step::Patch;
        self.check()?;
        self.patch_stub().map_err(|error| self.at(error))?;

        self.step = Step::SignLoader;
        self.check()?;
        self.sign_loader().map_err(|error| self.at(error))?;

        self.step = Step::SignBundle;
        self.check()?;
        self.sign_bundle().map_err(|error| self.at(error))?;

        self.step = Step::VerifyStaged;
        self.check()?;
        self.verify_staged().map_err(|error| self.at(error))?;

        self.step = Step::Swap;
        self.check()?;
        self.swap().map_err(|error| self.at(error))?;

        self.step = Step::VerifyInstalled;
        self.check()?;
        self.verify_installed().map_err(|error| self.at(error))?;

        // A preseed fault or failure is recorded, never fatal: the client can
        // still seed itself, and the measured cost is the empty-state quirk.
        self.step = Step::Preseed;
        match self.faults.check(Step::Preseed) {
            Ok(()) => self.preseed(),
            Err(message) => self.warn("preseed", message),
        }

        self.step = Step::Commit;
        self.check()?;
        self.commit().map_err(|error| self.at(error))?;

        Ok(ApplyReport {
            outcome: ApplyOutcome::Applied,
            installation_id: self.installation_id(),
            install_dir: self.install.root().to_path_buf(),
            journal: self.journal.path().to_path_buf(),
            app: self.app.clone(),
            bundle_id: self.bundle_id.clone(),
            stub_sha256: self.stub_sha256.clone(),
            loader_sha256: self.loader_sha256.clone(),
            applied_loader_sha256: self
                .manifest
                .as_ref()
                .and_then(|manifest| manifest.applied_loader_sha256.clone()),
            applied_stub_sha256: self
                .manifest
                .as_ref()
                .and_then(|manifest| manifest.applied_stub_sha256.clone()),
            preseed: self.preseed.0,
            preseed_detail: self.preseed.1.clone(),
            patch: self.patch.clone(),
            warnings: self.warnings.clone(),
        })
    }

    fn check(&self) -> Result<(), (Step, String)> {
        self.faults
            .check(self.step)
            .map_err(|error| (self.step, error))
    }

    fn at(&self, message: String) -> (Step, String) {
        (self.step, message)
    }

    fn installation_id(&self) -> String {
        journal::installation_id(&self.bundle_id, &self.stub_sha256)
    }

    fn note(&mut self, event: &str, detail: serde_json::Value) -> Result<(), String> {
        self.journal.append(event, Some(detail))
    }

    /// Records a warning in the journal and the report; never fails.
    fn warn(&mut self, event: &str, message: String) {
        self.preseed = (PreseedOutcome::Warned, message.clone());
        self.warnings.push(format!("{event}: {message}"));
        let _ = self.journal.append(
            "warning",
            Some(serde_json::json!({"event": event, "message": message})),
        );
    }

    // --- steps -----------------------------------------------------------

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
        self.executable = self.app.join(STUB_RELATIVE);
        if !self.executable.is_file() {
            return Err(format!(
                "{} is missing the supported bootstrap stub",
                self.executable.display()
            ));
        }
        self.stub_sha256 = digest::sha256_file(&self.executable)?;
        if !self.options.orbit_root.is_absolute() {
            return Err(format!(
                "the Orbit state root must be an absolute path: {}",
                self.options.orbit_root.display()
            ));
        }
        self.orbit_root = platform::canonical_absolute(&self.options.orbit_root)?;
        // A recorded install takes precedence over the pristine whitelist:
        // the patched stub is supposed to have a hash the vendor list does
        // not contain.
        self.manifest = state::find_manifest(&self.orbit_root, &self.app)?;
        let recorded_stub = self
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.applied_stub_sha256.as_deref());
        if !self.options.whitelist.permits(&self.stub_sha256)
            && recorded_stub != Some(self.stub_sha256.as_str())
        {
            return Err(format!(
                "the stub {} has SHA-256 {} which is not a supported vendor \
                 bootstrap; refusing before any change",
                self.executable.display(),
                self.stub_sha256
            ));
        }
        if let Some(manifest) = self.manifest.as_ref() {
            self.install = InstallDir::new(&self.orbit_root, &manifest.installation_id);
        } else {
            self.install = InstallDir::new(&self.orbit_root, &self.installation_id());
        }
        self.journal = Journal::new(&self.install);
        self.bundle_id = platform::bundle_identifier(&self.app)?;
        let running = platform::running_executables(&self.executable)?;
        if !running.is_empty() {
            return Err(format!(
                "the selected app is running (pid {running:?}); quit it and retry"
            ));
        }

        let metadata = fs::symlink_metadata(&self.loader_source)
            .map_err(|error| format!("{}: {error}", self.loader_source.display()))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(format!(
                "the loader input {} must be a regular file",
                self.loader_source.display()
            ));
        }
        if metadata.len() > 64 * 1024 * 1024 {
            return Err("the loader input exceeds the 64 MiB inspection limit".into());
        }
        self.loader_source = fs::canonicalize(&self.loader_source)
            .map_err(|error| format!("{}: {error}", self.loader_source.display()))?;
        self.loader_sha256 = digest::sha256_file(&self.loader_source)?;
        let loader = fs::read(&self.loader_source)
            .map_err(|error| format!("{}: {error}", self.loader_source.display()))?;
        macho::verify_loader(&loader)
            .map_err(|error| format!("the loader input is not a valid arm64 dylib: {error}"))?;

        self.pristine_tree = digest::tree(&self.app)?;
        Ok(())
    }

    /// Restores an interrupted transaction or accepts an installed bundle as
    /// the already-applied no-op.
    ///
    /// Runs before inspection (the app path may be empty after a crash) and
    /// only for a transaction that was interrupted before its commit.
    fn recover_before_inspect(&mut self) -> Result<(), String> {
        let orbit_root = platform::canonical_absolute(&self.options.orbit_root)?;
        if !orbit_root.is_dir() {
            return Ok(());
        }
        let app = platform::canonical_absolute(&self.app)?;
        let Some(manifest) = state::find_manifest(&orbit_root, &app)? else {
            return Ok(());
        };
        if !matches!(
            manifest.state,
            InstallState::Preparing | InstallState::BackedUp
        ) {
            return Ok(());
        }
        self.app = app;
        self.orbit_root = orbit_root;
        self.install = InstallDir::new(&self.orbit_root, &manifest.installation_id);
        self.journal = Journal::new(&self.install);
        self.manifest = Some(manifest);
        let warnings =
            self.roll_back("an interrupted transaction was rolled back before inspection")?;
        self.warnings.extend(warnings);
        Ok(())
    }

    fn recover(&mut self) -> Result<Option<ApplyReport>, String> {
        // `inspect` loaded the record for this app path, if there is one.
        let manifest = self.manifest.clone();
        let Some(manifest) = manifest else {
            if self.install.root().exists() {
                for name in [journal::PRISTINE_DIR, journal::STAGED_DIR] {
                    let path = self.install.root().join(name);
                    if path.exists() {
                        return Err(format!(
                            "{} exists without a manifest; refusing to guess at \
                             an unrecorded transaction",
                            path.display()
                        ));
                    }
                }
                if let Ok(listing) = fs::read_dir(self.install.root()) {
                    for entry in listing.flatten() {
                        let name = entry.file_name();
                        if name.to_string_lossy().starts_with("applied-removed") {
                            return Err(format!(
                                "{} exists without a manifest; refusing to guess \
                                 at an unrecorded transaction",
                                entry.path().display()
                            ));
                        }
                    }
                }
            }
            return Ok(None);
        };
        if manifest.app != self.app || manifest.orbit_root != self.orbit_root {
            return Err(format!(
                "the recorded installation in {} names {} (Orbit root {}), not {}",
                self.install.root().display(),
                manifest.app.display(),
                manifest.orbit_root.display(),
                self.app.display()
            ));
        }
        self.manifest = Some(manifest);

        match self.manifest.as_ref().expect("just set").state {
            InstallState::Installed => {
                let classification =
                    state::classify(&self.app, self.manifest.as_ref(), &self.options.whitelist)?;
                if classification.class != AppClass::Applied {
                    return Err(format!(
                        "the recorded apply no longer matches {} ({}); refusing to \
                         touch the pristine backup. Restore the vendor app or run \
                         the matching uninstall first",
                        self.app.display(),
                        classification.differences.join("; ")
                    ));
                }
                platform::codesign_verify(&self.app)?;
                Ok(Some(
                    self.report_without_patch(ApplyOutcome::AlreadyApplied),
                ))
            }
            InstallState::Preparing | InstallState::BackedUp => {
                let warnings = self.roll_back("an interrupted transaction was rolled back")?;
                self.warnings.extend(warnings);
                Ok(None)
            }
            InstallState::RolledBack | InstallState::Uninstalled => Ok(None),
        }
    }

    fn report_without_patch(&self, outcome: ApplyOutcome) -> ApplyReport {
        ApplyReport {
            outcome,
            installation_id: self.installation_id(),
            install_dir: self.install.root().to_path_buf(),
            journal: self.journal.path().to_path_buf(),
            app: self.app.clone(),
            bundle_id: self.bundle_id.clone(),
            stub_sha256: self.stub_sha256.clone(),
            loader_sha256: self.loader_sha256.clone(),
            applied_loader_sha256: self
                .manifest
                .as_ref()
                .and_then(|manifest| manifest.applied_loader_sha256.clone()),
            applied_stub_sha256: self
                .manifest
                .as_ref()
                .and_then(|manifest| manifest.applied_stub_sha256.clone()),
            preseed: PreseedOutcome::Skipped,
            preseed_detail: "the applied bundle already has its isolated state".into(),
            patch: None,
            warnings: self.warnings.clone(),
        }
    }

    fn backup(&mut self) -> Result<(), String> {
        let current = digest::tree(&self.app)?;
        if !digest::differences(&self.pristine_tree, &current).is_empty() {
            return Err("the selected bundle changed after inspection; refusing backup".into());
        }
        // The input must still be a valid, sealed vendor bundle before the
        // first mutation. A bundle with a record is checked by `recover`
        // (tree classification plus signature), which reports the cause.
        platform::codesign_verify(&self.app).map_err(|error| {
            format!(
                "the selected bundle {} fails strict code-signature verification: {error}",
                self.app.display()
            )
        })?;
        if self.orbit_root.exists() {
            platform::require_private_dir(&self.orbit_root, "Orbit root")?;
        } else {
            if let Some(parent) = self.orbit_root.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("{}: {error}", parent.display()))?;
            }
            platform::create_private_dir(&self.orbit_root)?;
            platform::fsync_dir(
                self.orbit_root
                    .parent()
                    .unwrap_or(self.orbit_root.as_path()),
            )?;
        }
        self.orbit_root = fs::canonicalize(&self.orbit_root)
            .map_err(|error| format!("{}: {error}", self.orbit_root.display()))?;
        self.install = InstallDir::new(&self.orbit_root, &self.installation_id());
        self.journal = Journal::new(&self.install);
        self.install.ensure()?;

        if !platform::same_volume(&self.app, self.install.root())? {
            return Err(format!(
                "{} and the Orbit state root {} are on different filesystems; \
                 the rename-based backup cannot run",
                self.app.display(),
                self.install.root().display()
            ));
        }
        if self.install.pristine().exists() {
            return Err(format!(
                "{} already exists although no record references it; refusing to \
                 overwrite the only possible pristine copy",
                self.install.pristine().display()
            ));
        }

        let manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            installation_id: self.installation_id(),
            bundle_id: self.bundle_id.clone(),
            app: self.app.clone(),
            executable: self.executable.clone(),
            orbit_root: self.orbit_root.clone(),
            stub_sha256: self.stub_sha256.clone(),
            loader_sha256: self.loader_sha256.clone(),
            applied_loader_sha256: None,
            applied_stub_sha256: None,
            pristine_tree: self.pristine_tree.clone(),
            applied_tree: None,
            state: InstallState::Preparing,
            updated_unix: journal::unix_time(),
        };
        self.manifest = Some(manifest);
        self.write_manifest()?;
        self.note(
            "transaction-begin",
            serde_json::json!({
                "app": self.app,
                "loader": self.loader_source,
                "stub_sha256": self.stub_sha256,
                "loader_sha256": self.loader_sha256,
            }),
        )?;

        fs::rename(&self.app, self.install.pristine()).map_err(|error| {
            format!(
                "cannot move {} to {}: {error}; the installer never escalates, \
                 so run it with the required permission on the selected bundle",
                self.app.display(),
                self.install.pristine().display()
            )
        })?;
        platform::fsync_dir(
            self.app
                .parent()
                .ok_or_else(|| format!("{} has no parent directory", self.app.display()))?,
        )?;
        if let Some(manifest) = self.manifest.as_mut() {
            manifest.state = InstallState::BackedUp;
            manifest.updated_unix = journal::unix_time();
        }
        self.write_manifest()?;
        self.note(
            "backed-up",
            serde_json::json!({"pristine": self.install.pristine()}),
        )
    }

    fn stage(&mut self) -> Result<(), String> {
        platform::ditto(&self.install.pristine(), &self.install.staged())?;
        let staged = digest::tree(&self.install.staged())?;
        let differences = digest::staged_copy_differences(&self.pristine_tree, &staged);
        if !differences.is_empty() {
            return Err(format!(
                "the staged copy differs from the pristine snapshot: {}",
                differences.join("; ")
            ));
        }
        self.staged_pristine_tree = Some(staged);
        self.note(
            "staged",
            serde_json::json!({"staged": self.install.staged()}),
        )
    }

    fn patch_stub(&mut self) -> Result<(), String> {
        let path = self.install.staged().join(STUB_RELATIVE);
        let image = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let (patched, report) = macho::patch(&image, LOADER_DEPENDENCY)?;
        fs::write(&path, &patched).map_err(|error| format!("{}: {error}", path.display()))?;
        self.note(
            "patched",
            serde_json::json!({
                "commands_before": report.commands_before,
                "dependency_bytes": report.dependency_bytes,
                "first_section_offset": report.first_section_offset,
            }),
        )?;
        self.patch = Some(report);
        Ok(())
    }

    fn sign_loader(&mut self) -> Result<(), String> {
        let staged = self.install.staged();
        let frameworks = staged.join("Contents/Frameworks");
        if frameworks.exists() {
            if !frameworks.is_dir() {
                return Err(format!("{} is not a directory", frameworks.display()));
            }
        } else {
            fs::create_dir(&frameworks)
                .map_err(|error| format!("{}: {error}", frameworks.display()))?;
            platform::set_mode(&frameworks, 0o755)?;
        }
        let loader = staged.join(LOADER_RELATIVE);
        if loader.exists() {
            return Err(format!(
                "the selected bundle already contains {}",
                loader.display()
            ));
        }
        fs::copy(&self.loader_source, &loader)
            .map_err(|error| format!("{}: {error}", loader.display()))?;
        if digest::sha256_file(&loader)? != self.loader_sha256 {
            return Err(format!(
                "the copied loader {} does not match the input",
                loader.display()
            ));
        }
        platform::set_mode(&loader, 0o755)?;
        platform::codesign_sign(&loader, None, false)?;
        // The ad-hoc signature changes the file; the applied hash is the one
        // the staged and installed verification compare against.
        let applied_loader_sha256 = digest::sha256_file(&loader)?;
        if let Some(manifest) = self.manifest.as_mut() {
            manifest.applied_loader_sha256 = Some(applied_loader_sha256.clone());
            manifest.updated_unix = journal::unix_time();
        }

        let record = serde_json::json!({
            "schema_version": 1,
            "executable": path_text(&self.executable)?,
            "loader": path_text(&self.app.join(LOADER_RELATIVE))?,
            "orbit_root": path_text(&self.orbit_root)?,
            "uid": effective_uid(),
        });
        let bytes = serde_json::to_vec_pretty(&record)
            .map_err(|error| format!("cannot serialize the install record: {error}"))?;
        platform::write_private_file(&staged.join(RECORD_RELATIVE), &bytes)?;
        self.note(
            "signed-loader",
            serde_json::json!({
                "loader": loader,
                "loader_sha256": self.loader_sha256,
                "applied_loader_sha256": applied_loader_sha256,
            }),
        )
    }

    fn sign_bundle(&mut self) -> Result<(), String> {
        let entitlements = self.install.entitlements_path();
        // A rolled-back transaction leaves the file behind; it is ours.
        if entitlements.exists() {
            fs::remove_file(&entitlements)
                .map_err(|error| format!("{}: {error}", entitlements.display()))?;
        }
        platform::write_private_file(&entitlements, ENTITLEMENTS_PLIST.as_bytes())?;
        platform::codesign_sign(&self.install.staged(), Some(&entitlements), true)?;
        self.note(
            "signed-bundle",
            serde_json::json!({"entitlements": "com.apple.security.cs.disable-library-validation"}),
        )
    }

    fn verify_staged(&mut self) -> Result<(), String> {
        let staged = self.install.staged();
        platform::codesign_verify(&staged)?;

        let stub = staged.join(STUB_RELATIVE);
        let image = fs::read(&stub).map_err(|error| format!("{}: {error}", stub.display()))?;
        macho::verify_dependency(&image, LOADER_DEPENDENCY)?;
        let report = self
            .patch
            .as_ref()
            .ok_or_else(|| "the transaction lost its patch report".to_string())?;
        macho::sections_preserved(&image, &report.file_backed_sections)?;
        let applied_stub_sha256 = digest::sha256_file(&stub)?;

        let loader = staged.join(LOADER_RELATIVE);
        let applied_loader_sha256 = self
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.applied_loader_sha256.clone())
            .ok_or_else(|| "the transaction lost the applied loader hash".to_string())?;
        if digest::sha256_file(&loader)? != applied_loader_sha256 {
            return Err(format!(
                "the staged loader {} does not match the signed copy",
                loader.display()
            ));
        }
        verify_record(&staged, &self.executable, &self.orbit_root)?;

        let tree = digest::tree(&staged)?;
        let pristine_copy = self
            .staged_pristine_tree
            .as_ref()
            .ok_or("the transaction lost its staged pristine snapshot")?;
        let changed = digest::changed_paths_for_signing(
            pristine_copy,
            &tree,
            &["", "Contents", "Contents/Resources", "Contents/MacOS"],
        );
        let unexpected: Vec<String> = changed
            .into_iter()
            .filter(|(path, _)| !ALLOWED_STAGED_CHANGES.contains(&path.as_str()))
            .map(|(path, reason)| format!("{path}: {reason}"))
            .collect();
        if !unexpected.is_empty() {
            return Err(format!(
                "the staged bundle changes files outside the supported patch: {}",
                unexpected.join("; ")
            ));
        }

        self.staged_tree = Some(tree.clone());
        if let Some(manifest) = self.manifest.as_mut() {
            manifest.applied_stub_sha256 = Some(applied_stub_sha256.clone());
            manifest.applied_tree = Some(tree);
            manifest.updated_unix = journal::unix_time();
        }
        self.write_manifest()?;
        self.note(
            "verified-staged",
            serde_json::json!({"applied_stub_sha256": applied_stub_sha256}),
        )
    }

    fn swap(&mut self) -> Result<(), String> {
        let running = platform::running_executables(&self.executable)?;
        if !running.is_empty() {
            return Err(format!(
                "the selected executable started during the transaction \
                 (pid {running:?}); refusing to change it"
            ));
        }
        if self.app.exists() {
            return Err(format!(
                "{} is occupied before the swap",
                self.app.display()
            ));
        }
        fs::rename(self.install.staged(), &self.app).map_err(|error| {
            format!(
                "cannot move the staged bundle into {}: {error}",
                self.app.display()
            )
        })?;
        if let Some(parent) = self.app.parent() {
            platform::fsync_dir(parent)?;
        }
        self.note("swapped", serde_json::json!({"app": self.app}))
    }

    fn verify_installed(&mut self) -> Result<(), String> {
        let expected = self
            .staged_tree
            .as_ref()
            .ok_or_else(|| "the transaction lost its staged snapshot".to_string())?;
        let actual = digest::tree(&self.app)?;
        let differences = digest::differences(expected, &actual);
        if !differences.is_empty() {
            return Err(format!(
                "the installed bundle does not match the verified staged bundle: {}",
                differences.join("; ")
            ));
        }
        platform::codesign_verify(&self.app)?;
        let stub_sha256 = digest::sha256_file(&self.app.join(STUB_RELATIVE))?;
        if self
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.applied_stub_sha256.as_deref())
            != Some(stub_sha256.as_str())
        {
            return Err("the installed stub hash does not match the recorded apply".into());
        }
        let applied_loader_sha256 = self
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.applied_loader_sha256.as_deref())
            .ok_or_else(|| "the transaction lost the applied loader hash".to_string())?;
        if digest::sha256_file(&self.app.join(LOADER_RELATIVE))? != applied_loader_sha256 {
            return Err("the installed loader hash does not match the recorded copy".into());
        }
        verify_record(&self.app, &self.executable, &self.orbit_root)?;
        self.note(
            "verified-installed",
            serde_json::json!({"stub_sha256": stub_sha256}),
        )
    }

    /// Best-effort seed of the fresh isolated state. Every failure is a
    /// recorded warning: the client can seed itself, and an existing state
    /// directory is never replaced.
    fn preseed(&mut self) {
        let home = self.orbit_root.join("home");
        let home_ready = if home.exists() {
            platform::require_private_dir(&home, "isolated home")
        } else if let Some(parent) = home.parent() {
            let _ = platform::fsync_dir(parent);
            platform::create_private_dir(&home)
        } else {
            Err(format!("{} has no parent directory", home.display()))
        };
        if let Err(error) = home_ready {
            self.warn("preseed", error);
            return;
        }

        let state = home.join(".parsec");
        if state.exists() {
            self.preseed = (
                PreseedOutcome::Skipped,
                format!(
                    "{} already exists; an existing isolated state is never replaced",
                    state.display()
                ),
            );
            let _ = self.note("preseed-skipped", serde_json::json!({"state": state}));
            return;
        }

        let skel = self.app.join(SKEL_RELATIVE);
        match seed_isolated_state(&skel, &home) {
            Ok(detail) => {
                self.preseed = (PreseedOutcome::Seeded, detail.clone());
                let _ = self.note("preseeded", serde_json::json!({"files": detail}));
            }
            Err(error) => self.warn("preseed", error),
        }
    }

    fn commit(&mut self) -> Result<(), String> {
        if let Some(manifest) = self.manifest.as_mut() {
            manifest.state = InstallState::Installed;
            manifest.updated_unix = journal::unix_time();
        }
        self.write_manifest()?;
        self.note(
            "committed",
            serde_json::json!({"preseed": self.preseed.0, "warnings": self.warnings.len()}),
        )
    }

    // --- shared helpers ---------------------------------------------------

    fn write_manifest(&self) -> Result<(), String> {
        let manifest = self
            .manifest
            .as_ref()
            .ok_or_else(|| "the transaction has no manifest yet".to_string())?;
        journal::write_manifest(&self.install, manifest)
    }

    /// Renames the pristine backup back over whatever occupies the app path
    /// and records the state. Returns non-fatal warnings.
    fn roll_back(&mut self, reason: &str) -> Result<Vec<String>, String> {
        let mut warnings = Vec::new();
        let Some(manifest) = journal::read_manifest(&self.install)? else {
            // The transaction died before its first manifest write.
            return Ok(warnings);
        };
        let pristine = self.install.pristine();
        let staged = self.install.staged();
        let app_present = fs::symlink_metadata(&self.app).is_ok();

        if pristine.exists() {
            if app_present {
                let actual = digest::tree(&self.app)?;
                let expected = manifest.applied_tree.as_ref().ok_or_else(|| {
                    "the app path is occupied by an unrecorded bundle; preserving it and the pristine backup".to_string()
                })?;
                if !digest::differences(expected, &actual).is_empty() {
                    return Err(
                        "the app path changed externally; preserving it and the pristine backup"
                            .into(),
                    );
                }
                let moved = self.move_aside()?;
                let _ = self.note("moved-aside", serde_json::json!({"path": moved}));
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
            let restored = digest::tree(&self.app)?;
            let differences = digest::differences(&manifest.pristine_tree, &restored);
            if !differences.is_empty() {
                return Err(format!(
                    "the restored bundle differs from the recorded pristine \
                     snapshot: {}",
                    differences.join("; ")
                ));
            }
            if let Err(error) = platform::codesign_verify(&self.app) {
                warnings.push(format!(
                    "the restored bundle fails strict signature verification: {error}"
                ));
            }
        } else if app_present {
            let actual = digest::tree(&self.app)?;
            let differences = digest::differences(&manifest.pristine_tree, &actual);
            if !differences.is_empty() {
                return Err(format!(
                    "there is no pristine backup in {} and {} no longer matches \
                     the recorded pristine snapshot: {}",
                    self.install.root().display(),
                    self.app.display(),
                    differences.join("; ")
                ));
            }
        } else {
            return Err(format!(
                "both {} and its pristine backup in {} are missing",
                self.app.display(),
                pristine.display()
            ));
        }

        if staged.exists() {
            fs::remove_dir_all(&staged)
                .map_err(|error| format!("{}: {error}", staged.display()))?;
        }
        // Transaction scratch; the next attempt writes it freshly.
        let entitlements = self.install.entitlements_path();
        if entitlements.exists() {
            let _ = fs::remove_file(&entitlements);
        }

        let mut manifest = manifest;
        manifest.state = InstallState::RolledBack;
        manifest.updated_unix = journal::unix_time();
        journal::write_manifest(&self.install, &manifest)?;
        self.manifest = Some(manifest);
        self.note("rolled-back", serde_json::json!({"reason": reason}))?;
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

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(|text| text.to_string())
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))
}

/// The install record must name exactly the installed stub, loader and root.
fn verify_record(staged: &Path, executable: &Path, orbit_root: &Path) -> Result<(), String> {
    let path = staged.join(RECORD_RELATIVE);
    let bytes = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let record: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    // The loader path in the record is the bundle-relative sibling of the stub.
    let bundle = executable
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| format!("{} is not inside a bundle", executable.display()))?;
    let loader_path = bundle.join(LOADER_RELATIVE);
    let expected = [
        ("schema_version", serde_json::json!(1)),
        ("executable", serde_json::json!(path_text(executable)?)),
        ("loader", serde_json::json!(path_text(&loader_path)?)),
        ("orbit_root", serde_json::json!(path_text(orbit_root)?)),
        ("uid", serde_json::json!(effective_uid())),
    ];
    for (field, value) in expected {
        if record.get(field) != Some(&value) {
            return Err(format!(
                "{} records {field} as {:?}, expected {value}",
                path.display(),
                record.get(field)
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn seed_isolated_state(skel: &Path, home: &Path) -> Result<String, String> {
    let report = parsec_orbit_macos_state::seed(skel, home)?;
    Ok(format!(
        "{} files; payload {} ({})",
        report.files_copied, report.so_name, report.verified_digest
    ))
}

#[cfg(not(target_os = "macos"))]
fn seed_isolated_state(_skel: &Path, _home: &Path) -> Result<String, String> {
    Err("isolated-state seeding requires macOS".into())
}

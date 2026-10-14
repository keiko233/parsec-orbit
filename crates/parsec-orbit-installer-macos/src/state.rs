//! Classification of a selected bundle against the installer's record.
//!
//! `apply` and `uninstall` never trust the path alone: the stub hash must be
//! supported (or match the recorded apply), and a recorded `Installed` state
//! only accepts a bundle whose full tree still matches the applied snapshot.
//! Everything else is reported and refused instead of being guessed at, so a
//! vendor update or a partially edited bundle cannot consume the only
//! pristine backup.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::digest::{self, Tree};
use crate::journal::{self, InstallDir, InstallState, Manifest};
use crate::whitelist::Whitelist;
use crate::STUB_RELATIVE;

/// Finds the installation record whose `app` names the selected bundle.
///
/// `Ok(None)` means no record; several matching records refuse instead of
/// guessing. The result must be sealed into an [`InstallDir`] through
/// [`InstallDir::new`]`(orbit_root, manifest.installation_id)`.
pub fn find_manifest(orbit_root: &Path, app: &Path) -> Result<Option<Manifest>, String> {
    let installs = orbit_root.join("installs");
    if !installs.is_dir() {
        return Ok(None);
    }
    let mut names: Vec<PathBuf> = fs::read_dir(&installs)
        .map_err(|error| format!("{}: {error}", installs.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| format!("{}: {error}", installs.display()))
        })
        .collect::<Result<_, _>>()?;
    names.sort();

    let mut found: Vec<Manifest> = Vec::new();
    for path in names {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let dir = InstallDir::new(orbit_root, name);
        if !dir.root().is_dir() {
            continue;
        }
        if let Some(manifest) = journal::read_manifest(&dir)? {
            if manifest.app == app {
                found.push(manifest);
            }
        }
    }
    match found.len() {
        0 => Ok(None),
        1 => Ok(Some(found.remove(0))),
        count => Err(format!(
            "{count} recorded installations name {}; refusing to guess",
            app.display()
        )),
    }
}

/// What the selected path currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppClass {
    /// The selected path does not exist.
    Missing,
    /// A bundle without the supported bootstrap stub.
    Invalid,
    /// A supported, unpatched vendor bundle.
    Pristine,
    /// The bundle matches the recorded applied snapshot.
    Applied,
    /// A record exists but the bundle no longer matches it.
    Stale,
    /// The stub is not supported and no record covers it.
    Unsupported,
}

/// The classification and the evidence behind it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classification {
    pub class: AppClass,
    pub stub_sha256: Option<String>,
    pub recorded_state: Option<InstallState>,
    /// Tree differences that explain a `Stale` or `Invalid` result.
    pub differences: Vec<String>,
}

/// Classifies `app`, comparing against the recorded manifest when there is
/// one. An unrecorded bundle is `Pristine` only when its stub hash is on the
/// whitelist.
pub fn classify(
    app: &Path,
    manifest: Option<&Manifest>,
    whitelist: &Whitelist,
) -> Result<Classification, String> {
    let metadata = match fs::symlink_metadata(app) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Classification {
                class: AppClass::Missing,
                stub_sha256: None,
                recorded_state: manifest.map(|manifest| manifest.state),
                differences: vec![format!("{}: missing", app.display())],
            });
        }
        Err(error) => return Err(format!("{}: {error}", app.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Ok(Classification {
            class: AppClass::Invalid,
            stub_sha256: None,
            recorded_state: manifest.map(|manifest| manifest.state),
            differences: vec![format!("{}: not a real bundle directory", app.display())],
        });
    }

    let stub = app.join(STUB_RELATIVE);
    let stub_sha256 = match digest::sha256_file(&stub) {
        Ok(sha256) => sha256,
        Err(error) => {
            return Ok(Classification {
                class: AppClass::Invalid,
                stub_sha256: None,
                recorded_state: manifest.map(|manifest| manifest.state),
                differences: vec![error],
            });
        }
    };
    let recorded_state = manifest.map(|manifest| manifest.state);

    if let Some(manifest) = manifest {
        if manifest.state == InstallState::Installed {
            return classify_installed(app, &stub_sha256, manifest, recorded_state);
        }
        if let Some(pristine) = recorded_tree_matches(app, &manifest.pristine_tree)? {
            if pristine {
                return Ok(Classification {
                    class: AppClass::Pristine,
                    stub_sha256: Some(stub_sha256),
                    recorded_state,
                    differences: Vec::new(),
                });
            }
        }
    }

    if whitelist.permits(&stub_sha256) {
        Ok(Classification {
            class: AppClass::Pristine,
            stub_sha256: Some(stub_sha256),
            recorded_state,
            differences: Vec::new(),
        })
    } else {
        Ok(Classification {
            class: AppClass::Unsupported,
            stub_sha256: Some(stub_sha256),
            recorded_state,
            differences: Vec::new(),
        })
    }
}

fn classify_installed(
    app: &Path,
    stub_sha256: &str,
    manifest: &Manifest,
    recorded_state: Option<InstallState>,
) -> Result<Classification, String> {
    if manifest.applied_stub_sha256.as_deref() != Some(stub_sha256) {
        return Ok(Classification {
            class: AppClass::Stale,
            stub_sha256: Some(stub_sha256.to_string()),
            recorded_state,
            differences: vec![format!(
                "{}: stub hash {} does not match the recorded applied stub {}",
                app.display(),
                stub_sha256,
                manifest.applied_stub_sha256.as_deref().unwrap_or("(none)")
            )],
        });
    }
    let Some(expected) = manifest.applied_tree.as_ref() else {
        return Ok(Classification {
            class: AppClass::Stale,
            stub_sha256: Some(stub_sha256.to_string()),
            recorded_state,
            differences: vec!["the recorded apply has no bundle snapshot".into()],
        });
    };
    let actual = digest::tree(app)?;
    let differences = digest::differences(expected, &actual);
    Ok(Classification {
        class: if differences.is_empty() {
            AppClass::Applied
        } else {
            AppClass::Stale
        },
        stub_sha256: Some(stub_sha256.to_string()),
        recorded_state,
        differences,
    })
}

/// `Ok(Some(true))` when the tree matches, `Ok(Some(false))` when it differs,
/// `Ok(None)` when the bundle cannot be walked.
fn recorded_tree_matches(app: &Path, expected: &Tree) -> Result<Option<bool>, String> {
    match digest::tree(app) {
        Ok(actual) => Ok(Some(digest::differences(expected, &actual).is_empty())),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::SCHEMA_VERSION;
    use parsec_orbit_core::testutil::TempDir;
    use std::path::PathBuf;

    fn fake_app(scratch: &TempDir, stub: &str) -> PathBuf {
        let app = scratch.path().join("Parsec.app");
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join(STUB_RELATIVE), stub).unwrap();
        app
    }

    fn manifest_for(app: &Path, stub_sha256: String, state: InstallState, tree: Tree) -> Manifest {
        Manifest {
            schema_version: SCHEMA_VERSION,
            installation_id: "id@0123456789ab".into(),
            bundle_id: "tv.parsec.www".into(),
            app: app.to_path_buf(),
            executable: app.join(STUB_RELATIVE),
            orbit_root: PathBuf::from("/tmp/orbit"),
            stub_sha256: stub_sha256.clone(),
            loader_sha256: "cd".repeat(32),
            applied_loader_sha256: None,
            applied_stub_sha256: if state == InstallState::Installed {
                Some(stub_sha256)
            } else {
                None
            },
            pristine_tree: tree.clone(),
            applied_tree: if state == InstallState::Installed {
                Some(tree)
            } else {
                None
            },
            state,
            updated_unix: 0,
        }
    }

    #[test]
    fn unrecorded_supported_and_unsupported_stubs() {
        let scratch = TempDir::new("installer-state");
        let app = fake_app(&scratch, "vendor stub");
        let sha = digest::sha256_file(&app.join(STUB_RELATIVE)).unwrap();
        let whitelist = Whitelist::from_hashes([sha.clone()]);
        let classification = classify(&app, None, &whitelist).unwrap();
        assert_eq!(classification.class, AppClass::Pristine);

        let whitelist = Whitelist::supported();
        let classification = classify(&app, None, &whitelist).unwrap();
        assert_eq!(classification.class, AppClass::Unsupported);

        let classification = classify(&scratch.path().join("missing"), None, &whitelist).unwrap();
        assert_eq!(classification.class, AppClass::Missing);
    }

    #[test]
    fn recorded_states_are_verified_against_the_tree() {
        let scratch = TempDir::new("installer-state-recorded");
        let app = fake_app(&scratch, "vendor stub");
        let sha = digest::sha256_file(&app.join(STUB_RELATIVE)).unwrap();
        let whitelist = Whitelist::from_hashes([sha.clone()]);
        let tree = digest::tree(&app).unwrap();
        let manifest = manifest_for(&app, sha, InstallState::Installed, tree);

        let classification = classify(&app, Some(&manifest), &whitelist).unwrap();
        assert_eq!(classification.class, AppClass::Applied);

        fs::write(app.join("Contents/MacOS/parsecd"), "patched stub").unwrap();
        let classification = classify(&app, Some(&manifest), &whitelist).unwrap();
        assert_eq!(classification.class, AppClass::Stale);
        assert!(!classification.differences.is_empty());

        let rolled_back = manifest_for(
            &app,
            "00".repeat(32),
            InstallState::RolledBack,
            digest::tree(&app).unwrap(),
        );
        let classification = classify(&app, Some(&rolled_back), &whitelist).unwrap();
        assert_eq!(classification.class, AppClass::Pristine);
    }
}

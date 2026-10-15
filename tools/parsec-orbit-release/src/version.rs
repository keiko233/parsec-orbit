//! `version`: the workspace version, read from `[workspace.package]` in the
//! workspace `Cargo.toml`. CI asks the tool instead of parsing the file itself
//! (`docs/design/restructure.md`, section 7.4).

use std::io::Write;
use std::path::Path;

use parsec_orbit_registry::Error;
use serde::Deserialize;

use crate::args::Args;

#[derive(Deserialize)]
struct Cargo {
    workspace: Option<Workspace>,
}

#[derive(Deserialize)]
struct Workspace {
    package: Option<Package>,
}

#[derive(Deserialize)]
struct Package {
    version: Option<String>,
}

/// The `[workspace.package] version` of the workspace in `root`.
pub fn workspace_version(root: &Path) -> Result<String, Error> {
    let path = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    let cargo: Cargo = toml::from_str(&text)
        .map_err(|error| Error::Index(format!("{}: {error}", path.display())))?;
    let version = cargo
        .workspace
        .and_then(|workspace| workspace.package)
        .and_then(|package| package.version)
        .ok_or_else(|| {
            Error::Index(format!(
                "{}: [workspace.package] has no version",
                path.display()
            ))
        })?;
    semver::Version::parse(&version).map_err(|error| {
        Error::Index(format!(
            "{}: version {version:?} is not a semantic version: {error}",
            path.display()
        ))
    })?;
    Ok(version)
}

/// `version [--workspace DIR]`: prints the version on one line.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), Error> {
    let parsed = Args::parse(args, &["--workspace"])?;
    parsed.no_words()?;
    let root = parsed.value("--workspace").unwrap_or(".");
    writeln!(out, "{}", workspace_version(Path::new(root))?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use std::path::PathBuf;

    fn run_in(root: &Path) -> Result<String, Error> {
        let mut out = Vec::new();
        run(
            &["--workspace".to_string(), root.display().to_string()],
            &mut out,
        )?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn the_workspace_package_version_is_printed() {
        let dir = TempDir::new("version");
        dir.write(
            "Cargo.toml",
            "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"1.2.3\"\n",
        );
        assert_eq!(run_in(dir.path()).unwrap(), "1.2.3\n");
    }

    #[test]
    fn a_package_version_outside_the_workspace_table_is_not_used() {
        let dir = TempDir::new("version-package");
        dir.write("Cargo.toml", "[package]\nversion = \"9.9.9\"\n");
        assert!(matches!(run_in(dir.path()), Err(Error::Index(_))));
    }

    #[test]
    fn a_missing_or_malformed_version_is_refused() {
        let dir = TempDir::new("version-bad");
        dir.write("Cargo.toml", "[workspace.package]\nversion = \"one\"\n");
        assert!(matches!(run_in(dir.path()), Err(Error::Index(_))));
        dir.write("Cargo.toml", "[workspace]\n");
        assert!(matches!(run_in(dir.path()), Err(Error::Index(_))));
        let missing = dir.path().join("nowhere");
        assert!(matches!(run_in(&missing), Err(Error::Io(_))));
    }

    #[test]
    fn the_repository_version_is_a_semantic_version() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let version = workspace_version(&root).unwrap();
        assert!(semver::Version::parse(&version).is_ok(), "{version}");
    }
}

//! `registry check [--manifest DIR]`: validates `manifest/` offline. It reads
//! the sources and never downloads a package; `registry build` does that and
//! verifies every digest.

use std::fmt::Display;
use std::io::Write;
use std::path::{Path, PathBuf};

use parsec_orbit_registry::index::{is_sha256_hex, valid_id, PlatformId};
use parsec_orbit_registry::Error;

use crate::args::Args;
use crate::manifest::{
    artifact_problems, is_date, location_ok, platform_of, read_optional, Entry, HostReleases,
    PluginRelease, PluginReleases, RESERVED_PREFIX, SUMMARY_LIMIT,
};

/// What a check found: every problem, and how much was read.
#[derive(Debug, Default)]
pub struct Report {
    pub problems: Vec<String>,
    pub plugins: usize,
    pub releases: usize,
    pub host_releases: usize,
}

impl Report {
    fn fail(&mut self, path: &Path, message: impl Display) {
        self.problems.push(format!("{}: {message}", path.display()));
    }

    /// An error that already names its file.
    fn error(&mut self, error: Error) {
        self.problems.push(error.to_string());
    }
}

pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), Error> {
    let parsed = Args::parse(args, &["--manifest"])?;
    parsed.no_words()?;
    let root = PathBuf::from(parsed.value("--manifest").unwrap_or("manifest"));
    let report = check(&root)?;
    for problem in &report.problems {
        writeln!(out, "{problem}")?;
    }
    if !report.problems.is_empty() {
        return Err(Error::Index(format!(
            "{} problem(s) in {}",
            report.problems.len(),
            root.display()
        )));
    }
    writeln!(
        out,
        "ok: {} plugins, {} releases, {} host releases",
        report.plugins, report.releases, report.host_releases
    )?;
    Ok(())
}

/// Checks the manifest in `root`. Only a missing `plugins/` folder is an
/// error of its own; every other problem goes into the report.
pub fn check(root: &Path) -> Result<Report, Error> {
    let mut report = Report::default();
    let plugins = root.join("plugins");
    let folders = std::fs::read_dir(&plugins)
        .map_err(|error| Error::Io(format!("{}: {error}", plugins.display())))?;
    let mut paths = folders
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    for path in paths {
        let hidden = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'));
        if !hidden {
            check_plugin(&path, &mut report);
        }
    }
    check_host(&root.join("host").join("releases.toml"), &mut report);
    Ok(report)
}

fn check_plugin(folder: &Path, report: &mut Report) {
    if !folder.is_dir() {
        return report.fail(folder, "only plugin folders belong in plugins/");
    }
    let name = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !valid_id(&name) {
        return report.fail(folder, "the folder name is not a valid plugin id");
    }
    let entry_path = folder.join("entry.toml");
    let entry: Entry = match read_optional(&entry_path) {
        Ok(Some(entry)) => entry,
        Ok(None) => return report.fail(&entry_path, "missing; every plugin folder needs one"),
        Err(error) => return report.error(error),
    };
    check_entry(&entry, &name, &entry_path, report);
    report.plugins += 1;

    let releases_path = folder.join("releases.toml");
    match read_optional::<PluginReleases>(&releases_path) {
        Ok(Some(file)) => check_plugin_releases(&file.release, &releases_path, report),
        Ok(None) => {}
        Err(error) => report.error(error),
    }
}

fn check_entry(entry: &Entry, folder_name: &str, path: &Path, report: &mut Report) {
    if entry.id != folder_name {
        report.fail(
            path,
            format!(
                "id {:?} does not match the folder name {folder_name:?}",
                entry.id
            ),
        );
    }
    if !valid_id(&entry.id) {
        report.fail(path, format!("id {:?} is not a valid plugin id", entry.id));
    }
    if let Some(summary) = &entry.summary {
        let length = summary.chars().count();
        if length > SUMMARY_LIMIT {
            report.fail(
                path,
                format!("summary is {length} characters; at most {SUMMARY_LIMIT} are allowed"),
            );
        }
    }
    if let Some(homepage) = &entry.homepage {
        if !homepage.starts_with("https://") {
            report.fail(path, "homepage must be an https:// URL");
        }
    }
    match &entry.build {
        None if entry.id.starts_with(RESERVED_PREFIX) => report.fail(
            path,
            "the com.parsec-orbit. prefix is reserved for official modules, which have a [build] table",
        ),
        None => {}
        Some(build) if build.platforms.is_empty() => {
            report.fail(path, "[build] lists no platforms")
        }
        Some(build) => {
            for platform in &build.platforms {
                if PlatformId::try_from(platform.clone()).is_err() {
                    report.fail(path, format!("{platform:?} is not a platform (<os>-<arch>)"));
                }
            }
        }
    }
}

fn check_plugin_releases(releases: &[PluginRelease], path: &Path, report: &mut Report) {
    let mut seen: Vec<(String, String)> = Vec::new();
    for (index, release) in releases.iter().enumerate() {
        let label = format!("release {}", index + 1);
        let version = match semver::Version::parse(&release.version) {
            Ok(version) => version.to_string(),
            Err(error) => {
                report.fail(
                    path,
                    format!(
                        "{label}: version {:?} is not a semantic version: {error}",
                        release.version
                    ),
                );
                continue;
            }
        };
        let platform = match platform_of(release.platform.as_deref()) {
            Ok(platform) => platform.as_str().to_string(),
            Err(_) => {
                report.fail(
                    path,
                    format!(
                        "{label}: platform {:?} is not <os>-<arch>",
                        release.platform
                    ),
                );
                continue;
            }
        };
        if seen.contains(&(version.clone(), platform.clone())) {
            report.fail(
                path,
                format!("{label}: {version} for {platform} is listed twice"),
            );
        }
        seen.push((version, platform));
        if !location_ok(&release.url) {
            report.fail(
                path,
                format!("{label}: url must be https:// or a relative path without `..`"),
            );
        }
        for mirror in &release.mirrors {
            if !location_ok(mirror) {
                report.fail(
                    path,
                    format!("{label}: mirror {mirror:?} must be https:// or relative"),
                );
            }
        }
        if !is_sha256_hex(&release.sha256) {
            report.fail(
                path,
                format!("{label}: sha256 must be 64 lowercase hex digits"),
            );
        }
        if release
            .published
            .as_ref()
            .is_some_and(|date| !is_date(date))
        {
            report.fail(
                path,
                format!("{label}: published must be a date (YYYY-MM-DD)"),
            );
        }
        report.releases += 1;
    }
}

fn check_host(path: &Path, report: &mut Report) {
    let file: HostReleases = match read_optional(path) {
        Ok(Some(file)) => file,
        Ok(None) => return,
        Err(error) => return report.error(error),
    };
    let mut seen: Vec<(String, String)> = Vec::new();
    for (index, release) in file.release.iter().enumerate() {
        let label = format!("host release {}", index + 1);
        if let Err(error) = semver::Version::parse(&release.version) {
            report.fail(
                path,
                format!(
                    "{label}: version {:?} is not a semantic version: {error}",
                    release.version
                ),
            );
        }
        if release.api == 0 {
            report.fail(path, format!("{label}: api must be at least 1"));
        }
        match platform_of(release.platform.as_deref()) {
            Ok(platform) => {
                let key = (release.version.clone(), platform.as_str().to_string());
                if seen.contains(&key) {
                    report.fail(
                        path,
                        format!("{label}: {} for {} is listed twice", key.0, key.1),
                    );
                }
                seen.push(key);
            }
            Err(_) => report.fail(
                path,
                format!(
                    "{label}: platform {:?} is not <os>-<arch>",
                    release.platform
                ),
            ),
        }
        for problem in artifact_problems(&release.loader, &format!("{label} loader")) {
            report.fail(path, problem);
        }
        for problem in artifact_problems(&release.installer, &format!("{label} installer")) {
            report.fail(path, problem);
        }
        report.host_releases += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use std::path::PathBuf;

    const ENTRY: &str =
        "id = \"com.example.p\"\nsummary = \"A plugin\"\nhomepage = \"https://example.com\"\n";
    const RELEASES: &str = "[[release]]\nversion = \"1.0.0\"\nurl = \"https://example.com/p.zip\"\nsha256 = \"0000000000000000000000000000000000000000000000000000000000000000\"\npublished = 2026-11-01\n";
    const HOST: &str = "[[release]]\nversion = \"1.0.0\"\napi = 1\nloader = { url = \"https://example.com/bcrypt.dll\", sha256 = \"1111111111111111111111111111111111111111111111111111111111111111\", size = 10 }\ninstaller = { url = \"https://example.com/orbit.exe\", sha256 = \"2222222222222222222222222222222222222222222222222222222222222222\", size = 20 }\n";

    /// A valid manifest with one plugin, one release and one host release.
    fn valid(dir: &TempDir) {
        dir.write("plugins/com.example.p/entry.toml", ENTRY);
        dir.write("plugins/com.example.p/releases.toml", RELEASES);
        dir.write("host/releases.toml", HOST);
    }

    fn problems_of(dir: &TempDir) -> Vec<String> {
        check(dir.path()).unwrap().problems
    }

    #[test]
    fn a_valid_manifest_has_no_problems() {
        let dir = TempDir::new("check-valid");
        valid(&dir);
        let report = check(dir.path()).unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(
            (report.plugins, report.releases, report.host_releases),
            (1, 1, 1)
        );
    }

    #[test]
    fn each_mistake_is_reported_with_its_file() {
        // (file to rewrite, its new text, text the problem must contain)
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "plugins/com.example.p/entry.toml",
                ENTRY.replace("com.example.p\"", "com.example.q\""),
                "does not match the folder name",
            ),
            (
                "plugins/com.example.p/entry.toml",
                ENTRY.replace("A plugin", &"x".repeat(121)),
                "at most 120",
            ),
            (
                "plugins/com.example.p/entry.toml",
                ENTRY.replace("https://example.com", "http://example.com"),
                "homepage must be",
            ),
            (
                "plugins/com.example.p/entry.toml",
                format!("{ENTRY}\n[build]\nplatforms = [\"Windows\"]\n"),
                "is not a platform",
            ),
            (
                "plugins/com.example.p/entry.toml",
                format!("{ENTRY}\n[build]\nplatforms = []\n"),
                "lists no platforms",
            ),
            (
                "plugins/com.example.p/releases.toml",
                RELEASES.replace("1.0.0", "one"),
                "is not a semantic version",
            ),
            (
                "plugins/com.example.p/releases.toml",
                RELEASES.replace(
                    "version = \"1.0.0\"",
                    "version = \"1.0.0\"\nplatform = \"Windows\"",
                ),
                "is not <os>-<arch>",
            ),
            (
                "plugins/com.example.p/releases.toml",
                RELEASES.replace("https://example.com/p.zip", "http://example.com/p.zip"),
                "url must be",
            ),
            (
                "plugins/com.example.p/releases.toml",
                RELEASES.replace("https://example.com/p.zip", "../p.zip"),
                "url must be",
            ),
            (
                "plugins/com.example.p/releases.toml",
                RELEASES.replace(
                    "0000000000000000000000000000000000000000000000000000000000000000",
                    "ABC",
                ),
                "sha256 must be",
            ),
            (
                "plugins/com.example.p/releases.toml",
                RELEASES.replace("2026-11-01", "2026-11-01T10:00:00Z"),
                "published must be a date",
            ),
            (
                "plugins/com.example.p/releases.toml",
                format!("{RELEASES}\n{RELEASES}"),
                "listed twice",
            ),
            (
                "plugins/com.example.p/releases.toml",
                RELEASES.replace("published = 2026-11-01", "published = \"2026-11-01\""),
                "releases.toml",
            ),
            (
                "host/releases.toml",
                HOST.replace("api = 1", "api = 0"),
                "api must be at least 1",
            ),
            (
                "host/releases.toml",
                HOST.replace("size = 10", "size = 0"),
                "loader size 0",
            ),
            (
                "host/releases.toml",
                format!("{HOST}\n{HOST}"),
                "listed twice",
            ),
        ];
        for (file, text, expected) in cases {
            let dir = TempDir::new("check-case");
            valid(&dir);
            dir.write(file, text);
            let problems = problems_of(&dir);
            assert!(
                problems.iter().any(|problem| problem.contains(expected)),
                "{file}: expected {expected:?} in {problems:?}"
            );
            assert!(
                problems.iter().all(|problem| problem.contains(".toml")),
                "{problems:?}"
            );
        }
    }

    #[test]
    fn the_reserved_prefix_needs_a_build_table() {
        let dir = TempDir::new("check-reserved");
        valid(&dir);
        let entry = ENTRY.replace("com.example.p", "com.parsec-orbit.p");
        dir.write("plugins/com.parsec-orbit.p/entry.toml", &entry);
        let problems = problems_of(&dir);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("reserved for official modules")),
            "{problems:?}"
        );
        dir.write(
            "plugins/com.parsec-orbit.p/entry.toml",
            format!("{entry}\n[build]\nplatforms = [\"windows-x86_64\"]\n"),
        );
        assert!(problems_of(&dir).is_empty());
    }

    #[test]
    fn a_plugin_folder_without_an_entry_is_reported() {
        let dir = TempDir::new("check-no-entry");
        valid(&dir);
        std::fs::create_dir_all(dir.path().join("plugins/com.example.bare")).unwrap();
        let problems = problems_of(&dir);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("com.example.bare") && p.contains("entry.toml")),
            "{problems:?}"
        );
    }

    #[test]
    fn the_folder_name_must_be_a_plugin_id() {
        let dir = TempDir::new("check-bad-folder");
        valid(&dir);
        dir.write("plugins/has space/entry.toml", ENTRY);
        let problems = problems_of(&dir);
        assert!(
            problems.iter().any(|p| p.contains("not a valid plugin id")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_missing_manifest_is_an_error_not_a_report() {
        let dir = TempDir::new("check-missing");
        let missing: PathBuf = dir.path().join("nowhere");
        assert!(matches!(check(&missing), Err(Error::Io(_))));
    }

    #[test]
    fn run_prints_the_problems_and_fails() {
        let dir = TempDir::new("check-run");
        valid(&dir);
        dir.write("host/releases.toml", HOST.replace("size = 20", "size = 0"));
        let mut out = Vec::new();
        let args = vec!["--manifest".to_string(), dir.path().display().to_string()];
        let error = run(&args, &mut out).unwrap_err();
        assert!(matches!(error, Error::Index(_)));
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(
                &dir.path()
                    .join("host")
                    .join("releases.toml")
                    .display()
                    .to_string()
            ) && text.contains("installer size 0"),
            "{text}"
        );
    }

    #[test]
    fn the_committed_manifest_is_valid() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../manifest");
        let report = check(&root).unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(report.plugins, 9);
    }
}

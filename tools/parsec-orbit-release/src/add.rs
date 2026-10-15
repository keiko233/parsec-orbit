//! `registry add --tag vX.Y.Z --assets DIR`: records the release assets of a
//! tag in `manifest/` (`docs/design/restructure.md`, section 4.3).
//!
//! Each plugin package in the assets gets a `[[release]]` block in its
//! `releases.toml`; the loader and installer of a platform get one in
//! `host/releases.toml`. Blocks are appended as text, so hand-written content
//! is never rewritten. Everything is checked before the first byte is written:
//! a release that is already listed, or a file that would no longer parse,
//! stops the whole command.

use std::collections::BTreeMap;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use parsec_orbit_registry::index::PlatformId;
use parsec_orbit_registry::{package, trust, Error, LOADER_API};

use crate::args::{usage, Args};
use crate::manifest::{read_optional, HostReleases, PluginReleases, DEFAULT_PLATFORM};
use crate::time;

/// The repository whose release assets the URLs point at.
const REPOSITORY: &str = "keiko233/parsec-orbit";

/// Platform, loader file and installer file of each host release, as the
/// release job uploads them (`docs/design/restructure.md`, section 5.2).
const HOST_PLATFORMS: [(&str, &str, &str); 2] = [
    (
        PlatformId::WINDOWS_X86_64,
        "bcrypt.dll",
        "orbit-windows-x86_64.exe",
    ),
    (
        PlatformId::MACOS_ARM64,
        "libparsec_orbit_loader.dylib",
        "orbit-macos-arm64",
    ),
];

/// What the release is and where its assets live.
struct Source<'a> {
    tag: &'a str,
    repo: &'a str,
    /// The tag without its `v`: the version of the host releases.
    version: &'a str,
    /// Today, UTC, as `published`.
    date: &'a str,
}

/// The blocks to append, per file, and what was found for the report.
#[derive(Default)]
struct Plan {
    blocks: BTreeMap<PathBuf, Vec<String>>,
    notes: Vec<String>,
}

pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), Error> {
    let parsed = Args::parse(args, &["--tag", "--assets", "--manifest", "--repo"])?;
    parsed.no_words()?;
    let tag = parsed.required("--tag")?;
    let version = tag_version(tag)?;
    let repo = parsed.value("--repo").unwrap_or(REPOSITORY);
    check_repo(repo)?;
    let assets = PathBuf::from(parsed.required("--assets")?);
    let manifest = PathBuf::from(parsed.value("--manifest").unwrap_or("manifest"));
    let date = time::date_of(time::now_unix());
    let source = Source {
        tag,
        repo,
        version: &version,
        date: &date,
    };

    let plan = plan(&manifest, &assets, &source)?;
    let appends = render(&plan)?;
    for (path, text) in &appends {
        append(path, text)?;
    }
    for note in &plan.notes {
        writeln!(out, "{note}")?;
    }
    let added: usize = plan.blocks.values().map(Vec::len).sum();
    writeln!(out, "added {added} release(s) to {}", manifest.display())?;
    Ok(())
}

/// `v0.3.0` gives `0.3.0`; anything else is a usage error.
fn tag_version(tag: &str) -> Result<String, Error> {
    let version = tag
        .strip_prefix('v')
        .ok_or_else(|| usage(format!("--tag {tag:?} must look like vX.Y.Z")))?;
    semver::Version::parse(version)
        .map_err(|_| usage(format!("--tag {tag:?} must look like vX.Y.Z")))?;
    Ok(version.to_string())
}

/// `owner/name`, as it appears in a release URL.
fn check_repo(repo: &str) -> Result<(), Error> {
    let parts: Vec<&str> = repo.split('/').collect();
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    };
    match parts.as_slice() {
        [owner, name] if plain(owner) && plain(name) => Ok(()),
        _ => Err(usage(format!("--repo {repo:?} must be owner/name"))),
    }
}

fn plan(manifest: &Path, assets: &Path, source: &Source) -> Result<Plan, Error> {
    let mut plan = Plan::default();
    let mut planned: Vec<(String, String, String)> = Vec::new();
    for path in zip_files(assets)? {
        plugin_release(manifest, &path, source, &mut plan, &mut planned)?;
    }
    host_releases(manifest, assets, source, &mut plan)?;
    if plan.blocks.is_empty() {
        return Err(Error::Package(format!(
            "no plugin package or loader in {}: nothing to add",
            assets.display()
        )));
    }
    Ok(plan)
}

/// The `.zip` files of `assets`, sorted by name.
fn zip_files(assets: &Path) -> Result<Vec<PathBuf>, Error> {
    let entries = std::fs::read_dir(assets)
        .map_err(|error| Error::Io(format!("{}: {error}", assets.display())))?;
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| {
        path.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    });
    paths.sort();
    Ok(paths)
}

fn plugin_release(
    manifest: &Path,
    path: &Path,
    source: &Source,
    plan: &mut Plan,
    planned: &mut Vec<(String, String, String)>,
) -> Result<(), Error> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bytes = std::fs::read(path)?;
    let package =
        package::read(&bytes).map_err(|error| Error::Package(format!("{name}: {error}")))?;
    let platform = platform_from_name(&name)?;
    let kind_platform = match package.manifest.kind.as_str() {
        "dll" => PlatformId::WINDOWS_X86_64,
        "dylib" => PlatformId::MACOS_ARM64,
        other => {
            return Err(Error::Package(format!(
                "{name}: kind {other:?} is neither dll nor dylib"
            )))
        }
    };
    if kind_platform != platform {
        return Err(Error::Package(format!(
            "{name}: the package is for {kind_platform} but the name says {platform}"
        )));
    }

    let id = package.manifest.id;
    let version = package.manifest.version;
    let folder = manifest.join("plugins").join(&id);
    if !folder.join("entry.toml").is_file() {
        return Err(Error::Index(format!(
            "{}/entry.toml is missing: add the entry before its first release",
            folder.display()
        )));
    }
    let releases_path = folder.join("releases.toml");
    let existing: PluginReleases = read_optional(&releases_path)?.unwrap_or_default();
    let listed = existing.release.iter().any(|release| {
        release.version == version
            && release.platform.as_deref().unwrap_or(DEFAULT_PLATFORM) == platform
    });
    let key = (id.clone(), version.clone(), platform.clone());
    if listed {
        return Err(Error::Conflict(format!(
            "{id} {version} for {platform} is already in {}; nothing was added",
            releases_path.display()
        )));
    }
    if planned.contains(&key) {
        return Err(Error::Conflict(format!(
            "two packages in the assets are {id} {version} for {platform}; nothing was added"
        )));
    }
    planned.push(key);

    let url = release_url(source, &name);
    plan.blocks.entry(releases_path).or_default().push(format!(
        "[[release]]\nversion = \"{version}\"\nplatform = \"{platform}\"\nurl = \"{url}\"\nsha256 = \"{}\"\npublished = {}\n",
        trust::sha256_hex(&bytes),
        source.date
    ));
    plan.notes
        .push(format!("added {id} {version} for {platform}"));
    Ok(())
}

/// The platform in a package name, which ends in `-<os>-<arch>.zip`.
fn platform_from_name(name: &str) -> Result<String, Error> {
    let stem = name.strip_suffix(".zip").unwrap_or(name);
    let mut parts = stem.rsplit('-');
    if let (Some(arch), Some(os)) = (parts.next(), parts.next()) {
        let candidate = format!("{os}-{arch}");
        if PlatformId::try_from(candidate.clone()).is_ok() {
            return Ok(candidate);
        }
    }
    Err(Error::Package(format!(
        "{name}: the name must end in -<os>-<arch>.zip, such as -windows-x86_64.zip"
    )))
}

fn host_releases(
    manifest: &Path,
    assets: &Path,
    source: &Source,
    plan: &mut Plan,
) -> Result<(), Error> {
    let path = manifest.join("host").join("releases.toml");
    let existing: HostReleases = read_optional(&path)?.unwrap_or_default();
    for (platform, loader_name, installer_name) in HOST_PLATFORMS {
        let loader = assets.join(loader_name);
        let installer = assets.join(installer_name);
        if !loader.is_file() {
            plan.notes.push(format!(
                "host {platform}: no {loader_name} in the assets; no release"
            ));
            continue;
        }
        if !installer.is_file() {
            plan.notes.push(format!(
                "host {platform}: {installer_name} is not in the assets, so no release (a host release needs both files)"
            ));
            continue;
        }
        let listed = existing.release.iter().any(|release| {
            release.version == source.version
                && release.platform.as_deref().unwrap_or(DEFAULT_PLATFORM) == platform
        });
        if listed {
            return Err(Error::Conflict(format!(
                "host {} for {platform} is already in {}; nothing was added",
                source.version,
                path.display()
            )));
        }
        let loader_bytes = std::fs::read(&loader)?;
        let installer_bytes = std::fs::read(&installer)?;
        plan.blocks.entry(path.clone()).or_default().push(format!(
            "[[release]]\nversion = \"{}\"\napi = {LOADER_API}\nplatform = \"{platform}\"\nloader = {{ url = \"{}\", sha256 = \"{}\", size = {} }}\ninstaller = {{ url = \"{}\", sha256 = \"{}\", size = {} }}\n",
            source.version,
            release_url(source, loader_name),
            trust::sha256_hex(&loader_bytes),
            loader_bytes.len(),
            release_url(source, installer_name),
            trust::sha256_hex(&installer_bytes),
            installer_bytes.len(),
        ));
        plan.notes
            .push(format!("added host {} for {platform}", source.version));
    }
    Ok(())
}

fn release_url(source: &Source, file: &str) -> String {
    format!(
        "https://github.com/{}/releases/download/{}/{file}",
        source.repo, source.tag
    )
}

/// The text each file gets appended, after checking that the file still
/// parses as TOML with the new blocks. Nothing is written here.
fn render(plan: &Plan) -> Result<Vec<(PathBuf, String)>, Error> {
    let mut appends = Vec::new();
    for (path, blocks) in &plan.blocks {
        let existing = read_text(path)?;
        let mut text = String::new();
        if !existing.is_empty() {
            if !existing.ends_with('\n') {
                text.push('\n');
            }
            text.push('\n');
        }
        text.push_str(&blocks.join("\n"));
        check_toml(path, &format!("{existing}{text}"))?;
        appends.push((path.clone(), text));
    }
    Ok(appends)
}

fn read_text(path: &Path) -> Result<String, Error> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(Error::Io(format!("{}: {error}", path.display()))),
    }
}

fn check_toml(path: &Path, text: &str) -> Result<(), Error> {
    toml::from_str::<toml::Table>(text)
        .map(|_| ())
        .map_err(|error| Error::Index(format!("{}: would not parse: {error}", path.display())))
}

fn append(path: &Path, text: &str) -> Result<(), Error> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    file.write_all(text.as_bytes())
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    const SOURCE: Source = Source {
        tag: "v0.3.0",
        repo: REPOSITORY,
        version: "0.3.0",
        date: "2026-10-15",
    };

    /// A plugin package in `assets`, packed by the registry's own packer.
    fn package_in(dir: &TempDir, id: &str, kind: &str, entry: &str, file_name: &str) {
        dir.write(
            &format!("stage/{id}/plugin.toml"),
            format!("id = \"{id}\"\nname = \"Demo\"\nversion = \"0.3.0\"\nkind = \"{kind}\"\nentry = \"{entry}\"\n"),
        );
        dir.write(&format!("stage/{id}/{entry}"), b"binary");
        let bytes = package::pack(&dir.path().join("stage").join(id)).unwrap();
        dir.write(&format!("assets/{file_name}"), bytes);
    }

    /// An official module with an entry and a releases file holding `header`.
    fn module(dir: &TempDir, id: &str) {
        dir.write(
            &format!("manifest/plugins/{id}/entry.toml"),
            format!("id = \"{id}\"\n\n[build]\nplatforms = [\"windows-x86_64\"]\n"),
        );
        dir.write(
            &format!("manifest/plugins/{id}/releases.toml"),
            "# header\n",
        );
        dir.write("manifest/host/releases.toml", "# host header\n");
    }

    fn assets(dir: &TempDir) -> PathBuf {
        dir.path().join("assets")
    }

    fn manifest(dir: &TempDir) -> PathBuf {
        dir.path().join("manifest")
    }

    /// Plans and writes, the way `run` does, without reading the clock.
    fn add(dir: &TempDir, source: &Source) -> Result<Vec<String>, Error> {
        let plan = plan(&manifest(dir), &assets(dir), source)?;
        for (path, text) in render(&plan)? {
            append(&path, &text)?;
        }
        Ok(plan.notes)
    }

    #[test]
    fn a_package_becomes_a_release_block_that_still_checks() {
        let dir = TempDir::new("add-plugin");
        module(&dir, "com.parsec-orbit.demo");
        package_in(
            &dir,
            "com.parsec-orbit.demo",
            "dll",
            "demo_lib.dll",
            "demo-lib-windows-x86_64.zip",
        );
        add(&dir, &SOURCE).unwrap();

        let text = std::fs::read_to_string(
            manifest(&dir).join("plugins/com.parsec-orbit.demo/releases.toml"),
        )
        .unwrap();
        assert!(
            text.starts_with("# header\n"),
            "the hand-written header is kept"
        );
        let sha = trust::sha256_hex(
            &std::fs::read(assets(&dir).join("demo-lib-windows-x86_64.zip")).unwrap(),
        );
        let expected = format!(
            "[[release]]\nversion = \"0.3.0\"\nplatform = \"windows-x86_64\"\nurl = \"https://github.com/keiko233/parsec-orbit/releases/download/v0.3.0/demo-lib-windows-x86_64.zip\"\nsha256 = \"{sha}\"\npublished = 2026-10-15\n"
        );
        assert!(text.ends_with(&expected), "{text}");
        let report = crate::check::check(&manifest(&dir)).unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(report.releases, 1);
    }

    #[test]
    fn the_platform_comes_from_the_name_and_must_match_the_kind() {
        let dir = TempDir::new("add-platform");
        module(&dir, "com.parsec-orbit.demo");
        package_in(
            &dir,
            "com.parsec-orbit.demo",
            "dylib",
            "libdemo.dylib",
            "demo-macos-arm64.zip",
        );
        package_in(
            &dir,
            "com.parsec-orbit.demo",
            "dylib",
            "libdemo.dylib",
            "demo-windows-x86_64.zip",
        );
        let error = add(&dir, &SOURCE).unwrap_err();
        assert!(
            matches!(&error, Error::Package(text) if text.contains("the name says")),
            "{error}"
        );

        let other = TempDir::new("add-name");
        module(&other, "com.parsec-orbit.demo");
        package_in(&other, "com.parsec-orbit.demo", "dll", "d.dll", "demo.zip");
        let error = add(&other, &SOURCE).unwrap_err();
        assert!(
            matches!(&error, Error::Package(text) if text.contains("must end in")),
            "{error}"
        );
    }

    #[test]
    fn a_release_already_listed_is_refused_and_nothing_is_written() {
        let dir = TempDir::new("add-twice");
        module(&dir, "com.parsec-orbit.demo");
        package_in(
            &dir,
            "com.parsec-orbit.demo",
            "dll",
            "demo_lib.dll",
            "demo-lib-windows-x86_64.zip",
        );
        add(&dir, &SOURCE).unwrap();
        let path = manifest(&dir).join("plugins/com.parsec-orbit.demo/releases.toml");
        let before = std::fs::read_to_string(&path).unwrap();

        let error = add(&dir, &SOURCE).unwrap_err();
        assert!(matches!(error, Error::Conflict(_)), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn two_packages_for_one_version_and_platform_conflict() {
        let dir = TempDir::new("add-same");
        module(&dir, "com.parsec-orbit.demo");
        package_in(
            &dir,
            "com.parsec-orbit.demo",
            "dll",
            "demo_lib.dll",
            "a-windows-x86_64.zip",
        );
        package_in(
            &dir,
            "com.parsec-orbit.demo",
            "dll",
            "demo_lib.dll",
            "b-windows-x86_64.zip",
        );
        let error = add(&dir, &SOURCE).unwrap_err();
        assert!(matches!(error, Error::Conflict(_)), "{error}");
        let text = std::fs::read_to_string(
            manifest(&dir).join("plugins/com.parsec-orbit.demo/releases.toml"),
        )
        .unwrap();
        assert_eq!(text, "# header\n", "a conflict writes nothing");
    }

    #[test]
    fn a_package_without_an_entry_is_refused() {
        let dir = TempDir::new("add-no-entry");
        package_in(
            &dir,
            "com.example.none",
            "dll",
            "n.dll",
            "none-windows-x86_64.zip",
        );
        let error = add(&dir, &SOURCE).unwrap_err();
        assert!(
            matches!(&error, Error::Index(text) if text.contains("com.example.none")),
            "{error}"
        );
    }

    #[test]
    fn a_loader_with_its_installer_is_a_host_release_and_a_lone_loader_is_not() {
        let dir = TempDir::new("add-host");
        module(&dir, "com.parsec-orbit.demo");
        dir.write("assets/bcrypt.dll", b"MZ loader");
        dir.write("assets/orbit-windows-x86_64.exe", b"MZ installer");
        dir.write("assets/libparsec_orbit_loader.dylib", b"dylib loader");
        let notes = add(&dir, &SOURCE).unwrap();
        assert!(
            notes.iter().any(|note| note.contains("orbit-macos-arm64")),
            "{notes:?}"
        );

        let host = std::fs::read_to_string(manifest(&dir).join("host/releases.toml")).unwrap();
        let parsed: HostReleases = toml::from_str(&host).unwrap();
        assert_eq!(parsed.release.len(), 1, "{host}");
        let release = &parsed.release[0];
        assert_eq!(
            (release.version.as_str(), release.api),
            ("0.3.0", LOADER_API)
        );
        assert_eq!(release.platform.as_deref(), Some("windows-x86_64"));
        assert_eq!(release.loader.size, "MZ loader".len() as u64);
        assert_eq!(release.loader.sha256, trust::sha256_hex(b"MZ loader"));
        assert!(release
            .installer
            .url
            .ends_with("/v0.3.0/orbit-windows-x86_64.exe"));
    }

    #[test]
    fn an_empty_asset_folder_is_an_error() {
        let dir = TempDir::new("add-empty");
        module(&dir, "com.parsec-orbit.demo");
        dir.write("assets/README.txt", "nothing");
        let error = add(&dir, &SOURCE).unwrap_err();
        assert!(
            matches!(&error, Error::Package(text) if text.contains("nothing to add")),
            "{error}"
        );
    }

    #[test]
    fn the_repository_option_changes_the_urls() {
        let dir = TempDir::new("add-repo");
        module(&dir, "com.parsec-orbit.demo");
        package_in(
            &dir,
            "com.parsec-orbit.demo",
            "dll",
            "demo_lib.dll",
            "demo-windows-x86_64.zip",
        );
        let fork = Source {
            repo: "someone/fork",
            ..SOURCE
        };
        let plan = plan(&manifest(&dir), &assets(&dir), &fork).unwrap();
        let block = &plan.blocks.values().next().unwrap()[0];
        assert!(
            block.contains(
                "github.com/someone/fork/releases/download/v0.3.0/demo-windows-x86_64.zip"
            ),
            "{block}"
        );
    }

    #[test]
    fn tags_and_repositories_are_checked() {
        assert_eq!(tag_version("v0.3.0").unwrap(), "0.3.0");
        assert!(matches!(tag_version("0.3.0"), Err(Error::Usage(_))));
        assert!(matches!(tag_version("vone"), Err(Error::Usage(_))));
        assert!(check_repo("keiko233/parsec-orbit").is_ok());
        assert!(matches!(check_repo("parsec-orbit"), Err(Error::Usage(_))));
        assert!(matches!(check_repo("a/b/c"), Err(Error::Usage(_))));
    }

    #[test]
    fn the_platform_is_read_from_the_last_two_words() {
        assert_eq!(
            platform_from_name("parsec-orbit-lib-hooks-windows-x86_64.zip").unwrap(),
            "windows-x86_64"
        );
        assert_eq!(
            platform_from_name("demo-macos-arm64.zip").unwrap(),
            "macos-arm64"
        );
        assert!(platform_from_name("demo.zip").is_err());
    }
}

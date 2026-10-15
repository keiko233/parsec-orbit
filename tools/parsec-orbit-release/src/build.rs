//! `registry build [--manifest DIR] [--sign KEYFILE] [--dist DIR]`: turns the
//! sources of `manifest/` into `index.json` and `host.json`
//! (`docs/spec/registry.md`, `docs/design/restructure.md`, section 4.3).
//!
//! Every release is fetched and checked against its digest and size; a
//! package is read with the registry's own reader, and its `plugin.toml` must
//! name the id and version the manifest lists. The generated files are
//! written only when their content changes: the serial rises by one with each
//! change and the files are otherwise left byte for byte as they were.
//! `--dist DIR` reads each release from the file of its last name instead of
//! the network, for development and tests.

use std::collections::BTreeSet;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use parsec_orbit_registry::host::{self, Artifact as HostArtifact, HostManifest, HostRelease};
use parsec_orbit_registry::index::{
    Index, PlatformId, Plugin, Release, FORMAT as INDEX_FORMAT, MAX_PACKAGE_BYTES,
};
use parsec_orbit_registry::package::{self, Package};
use parsec_orbit_registry::source::{Fetch, Real};
use parsec_orbit_registry::{trust, Error};
use semver::Version;
use serde::Deserialize;

use crate::args::Args;
use crate::manifest::{
    platform_of, read_optional, sorted_dirs, Artifact, Entry, HostReleases, PluginRelease,
    PluginReleases, RESERVED_PREFIX,
};
use crate::time;

/// The name the index carries.
const INDEX_NAME: &str = "Parsec Orbit official registry";

pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), Error> {
    let parsed = Args::parse(args, &["--manifest", "--sign", "--dist"])?;
    parsed.no_words()?;
    let manifest = PathBuf::from(parsed.value("--manifest").unwrap_or("manifest"));
    let secret = match parsed.value("--sign") {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|error| Error::Io(format!("{path}: {error}")))?;
            trust::signing_key(&text)?;
            Some(text)
        }
        None => None,
    };
    let fetch: Box<dyn Fetch> = match parsed.value("--dist") {
        Some(dir) => Box::new(Dist(PathBuf::from(dir))),
        None => Box::new(Real),
    };
    let generated = time::rfc3339_of(time::now_unix());
    let sign = secret.as_deref();
    publish_index(&manifest, fetch.as_ref(), &generated, sign, out)?;
    publish_host(&manifest, fetch.as_ref(), &generated, sign, out)
}

/// Serves `--dist DIR`: each release is read from the file named like the
/// last part of its URL.
struct Dist(PathBuf);

impl Fetch for Dist {
    fn fetch(&self, location: &str, limit: u64) -> Result<Vec<u8>, Error> {
        let name = location.rsplit(['/', '\\']).next().unwrap_or_default();
        if name.is_empty() || name == "." || name == ".." {
            return Err(Error::Io(format!("{location}: no file name to read")));
        }
        let path = self.0.join(name);
        let bytes = std::fs::read(&path)
            .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
        if bytes.len() as u64 > limit {
            return Err(Error::Io(format!(
                "{} is larger than {limit} bytes",
                path.display()
            )));
        }
        Ok(bytes)
    }
}

fn publish_index(
    manifest: &Path,
    fetch: &dyn Fetch,
    generated: &str,
    secret: Option<&str>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let path = manifest.join("index.json");
    let previous = read_previous(&path, Index::parse)?;
    let mut built = build_index(manifest, fetch, previous.as_ref())?;
    if let Some(old) = &previous {
        if unstamped_index(old) == unstamped_index(&built) {
            return writeln!(
                out,
                "index.json: unchanged, serial {}",
                old.serial.unwrap_or(0)
            )
            .map_err(Error::from);
        }
    }
    let serial = previous.and_then(|old| old.serial).unwrap_or(0) + 1;
    built.serial = Some(serial);
    built.generated = Some(generated.to_string());
    publish_file(&path, &built.to_json(), secret)?;
    writeln!(out, "index.json: written, serial {serial}")?;
    Ok(())
}

fn publish_host(
    manifest: &Path,
    fetch: &dyn Fetch,
    generated: &str,
    secret: Option<&str>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let Some(mut built) = build_host(manifest, fetch)? else {
        return writeln!(out, "host.json: no host releases; left as it is").map_err(Error::from);
    };
    let path = manifest.join("host.json");
    let previous = read_previous(&path, HostManifest::parse)?;
    if let Some(old) = &previous {
        if unstamped_host(old) == unstamped_host(&built) {
            return writeln!(
                out,
                "host.json: unchanged, serial {}",
                old.serial.unwrap_or(0)
            )
            .map_err(Error::from);
        }
    }
    let serial = previous.and_then(|old| old.serial).unwrap_or(0) + 1;
    built.serial = Some(serial);
    built.generated = Some(generated.to_string());
    let mut text = serde_json::to_string_pretty(&built)
        .map_err(|error| Error::Index(format!("host.json: {error}")))?;
    text.push('\n');
    publish_file(&path, &text, secret)?;
    writeln!(out, "host.json: written, serial {serial}")?;
    Ok(())
}

/// Writes a generated file and its signature. Without a key, a signature left
/// over from an older version of the file is removed: it no longer matches.
fn publish_file(path: &Path, text: &str, secret: Option<&str>) -> Result<(), Error> {
    write_atomic(path, text.as_bytes())?;
    let signature = PathBuf::from(format!("{}.sig", path.display()));
    match secret {
        Some(secret) => {
            let key = trust::signing_key(secret)?;
            let line = format!("{}\n", trust::sign(text.as_bytes(), &key));
            write_atomic(&signature, line.as_bytes())
        }
        None => match std::fs::remove_file(&signature) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Error::Io(format!("{}: {error}", signature.display()))),
        },
    }
}

/// Replaces `path` through a temporary file, so a reader sees either version.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let temporary = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&temporary, bytes)
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))
}

/// The previous generated file, or `None` when it does not exist yet.
fn read_previous<T>(path: &Path, parse: fn(&[u8]) -> Result<T, Error>) -> Result<Option<T>, Error> {
    match std::fs::read(path) {
        Ok(bytes) => parse(&bytes).map(Some),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Io(format!("{}: {error}", path.display()))),
    }
}

/// The index without its serial and timestamp: what the content comparison uses.
fn unstamped_index(index: &Index) -> Index {
    Index {
        serial: None,
        generated: None,
        ..index.clone()
    }
}

fn unstamped_host(host: &HostManifest) -> HostManifest {
    HostManifest {
        serial: None,
        generated: None,
        ..host.clone()
    }
}

/// What the index keeps about one release's package.
struct Known {
    api: u32,
    requires: Vec<String>,
    size: u64,
    meta: Meta,
}

/// The plugin-level fields the index takes from a package's `plugin.toml`.
#[derive(Clone)]
struct Meta {
    name: String,
    description: String,
    author: String,
    license: String,
    role: String,
}

fn build_index(
    manifest: &Path,
    fetch: &dyn Fetch,
    previous: Option<&Index>,
) -> Result<Index, Error> {
    let mut plugins: Vec<Plugin> = Vec::new();
    for folder in sorted_dirs(&manifest.join("plugins"))? {
        let entry_path = folder.join("entry.toml");
        let entry: Entry = read_optional(&entry_path)?
            .ok_or_else(|| Error::Index(format!("{}: missing", entry_path.display())))?;
        let releases: PluginReleases =
            read_optional(&folder.join("releases.toml"))?.unwrap_or_default();
        if releases.release.is_empty() {
            continue;
        }
        let old = previous.and_then(|index| index.plugin(&entry.id));

        // The packages are kept in version order: the newest sets the plugin fields.
        let mut versions: Vec<(Version, Release, Meta)> = Vec::new();
        for release in &releases.release {
            let version = Version::parse(&release.version).map_err(|error| {
                Error::Index(format!(
                    "{}: {} is not a version: {error}",
                    entry.id, release.version
                ))
            })?;
            let platform = platform_of(release.platform.as_deref())?;
            let kept = old.and_then(|plugin| {
                plugin.versions.iter().find(|known| {
                    known.version == release.version
                        && known.platform == platform
                        && known.url == release.url
                        && known.sha256 == release.sha256
                })
            });
            let known = match (kept, old) {
                (Some(kept), Some(plugin)) => Known {
                    api: kept.api,
                    requires: kept.requires.clone(),
                    size: kept.size,
                    meta: meta_of(plugin),
                },
                _ => fetch_package(fetch, release, &entry.id, &platform)?,
            };
            let meta = known.meta.clone();
            versions.push((
                version,
                Release {
                    version: release.version.clone(),
                    api: known.api,
                    platform,
                    url: release.url.clone(),
                    sha256: release.sha256.clone(),
                    size: known.size,
                    requires: known.requires,
                    published: release.published.as_ref().map(|date| date.to_string()),
                    yanked: release.yanked,
                    mirrors: release.mirrors.clone(),
                },
                meta,
            ));
        }
        versions.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.platform.as_str().cmp(b.1.platform.as_str()))
        });
        let newest = versions
            .last()
            .map(|(_, _, meta)| meta.clone())
            .expect("a plugin here has at least one release");
        plugins.push(Plugin {
            id: entry.id.clone(),
            name: newest.name,
            description: newest.description,
            author: newest.author,
            license: newest.license,
            role: Some(newest.role),
            summary: entry.summary,
            category: entry.category,
            homepage: entry.homepage,
            group: entry.group,
            default: entry.default,
            required: false,
            official: entry.id.starts_with(RESERVED_PREFIX),
            versions: versions
                .into_iter()
                .map(|(_, release, _)| release)
                .collect(),
        });
    }
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    mark_required(&mut plugins);
    let index = Index {
        format: INDEX_FORMAT,
        name: INDEX_NAME.to_string(),
        serial: None,
        generated: None,
        plugins,
    };
    // The checks a client makes when it reads the index.
    Index::parse(index.to_json().as_bytes())?;
    Ok(index)
}

fn meta_of(plugin: &Plugin) -> Meta {
    Meta {
        name: plugin.name.clone(),
        description: plugin.description.clone(),
        author: plugin.author.clone(),
        license: plugin.license.clone(),
        role: plugin.role.clone().unwrap_or_else(|| "plugin".to_string()),
    }
}

/// `required`: a lib that a default plugin needs, directly or through another.
fn mark_required(plugins: &mut [Plugin]) {
    let requires_of = |plugin: &Plugin| -> Vec<String> {
        plugin
            .versions
            .iter()
            .flat_map(|release| release.requires.clone())
            .collect()
    };
    let mut needed = BTreeSet::new();
    let mut pending: Vec<String> = plugins
        .iter()
        .filter(|plugin| plugin.default)
        .flat_map(requires_of)
        .collect();
    while let Some(id) = pending.pop() {
        if !needed.insert(id.clone()) {
            continue;
        }
        if let Some(plugin) = plugins.iter().find(|plugin| plugin.id == id) {
            pending.extend(requires_of(plugin));
        }
    }
    for plugin in plugins.iter_mut() {
        plugin.required = needed.contains(&plugin.id) && plugin.role.as_deref() == Some("lib");
    }
}

/// Downloads a plugin package and checks it against its manifest entry.
fn fetch_package(
    fetch: &dyn Fetch,
    release: &PluginRelease,
    id: &str,
    platform: &PlatformId,
) -> Result<Known, Error> {
    let bytes = fetch.fetch(&release.url, MAX_PACKAGE_BYTES)?;
    trust::check_digest(&bytes, &release.sha256)?;
    let package = package::read(&bytes)
        .map_err(|error| Error::Package(format!("{}: {error}", release.url)))?;
    let manifest = &package.manifest;
    if manifest.id != id {
        return Err(Error::Index(format!(
            "{}: the package is {} but is listed for {id}",
            release.url, manifest.id
        )));
    }
    if manifest.version != release.version {
        return Err(Error::Index(format!(
            "{}: the package is version {} but releases.toml says {}",
            release.url, manifest.version, release.version
        )));
    }
    let kind_platform = match manifest.kind.as_str() {
        "dll" => PlatformId::WINDOWS_X86_64,
        "dylib" => PlatformId::MACOS_ARM64,
        other => {
            return Err(Error::Package(format!(
                "{}: kind {other:?} is neither dll nor dylib",
                release.url
            )))
        }
    };
    if kind_platform != platform.as_str() {
        return Err(Error::Package(format!(
            "{}: the package is for {kind_platform} but releases.toml says {}",
            release.url,
            platform.as_str()
        )));
    }
    Ok(Known {
        api: manifest.api,
        requires: manifest.requires.clone(),
        size: bytes.len() as u64,
        meta: Meta {
            name: manifest.name.clone(),
            description: manifest.description.clone(),
            author: manifest.author.clone(),
            license: manifest.license.clone(),
            role: role_of(&package)?,
        },
    })
}

/// The `role` of a package's `plugin.toml` (`plugin` when absent).
fn role_of(package: &Package) -> Result<String, Error> {
    #[derive(Deserialize)]
    struct Role {
        role: Option<String>,
    }
    let file = package
        .files
        .iter()
        .find(|file| file.path == "plugin.toml")
        .ok_or_else(|| Error::Package(format!("{}/plugin.toml is missing", package.folder)))?;
    let text = std::str::from_utf8(&file.data)
        .map_err(|_| Error::Package("plugin.toml is not UTF-8".to_string()))?;
    let role: Role =
        toml::from_str(text).map_err(|error| Error::Package(format!("plugin.toml: {error}")))?;
    match role.role.as_deref().unwrap_or("plugin") {
        role @ ("lib" | "plugin") => Ok(role.to_string()),
        other => Err(Error::Index(format!(
            "plugin.toml has role {other:?}; expected \"lib\" or \"plugin\""
        ))),
    }
}

/// Downloads a loader or installer and checks its size and digest.
fn fetch_artifact(
    fetch: &dyn Fetch,
    artifact: &Artifact,
    what: &str,
) -> Result<HostArtifact, Error> {
    let bytes = fetch.fetch(&artifact.url, MAX_PACKAGE_BYTES)?;
    if bytes.len() as u64 != artifact.size {
        return Err(Error::Hash(format!(
            "{what} {} is {} bytes, releases.toml says {}",
            artifact.url,
            bytes.len(),
            artifact.size
        )));
    }
    trust::check_digest(&bytes, &artifact.sha256)?;
    Ok(HostArtifact {
        url: artifact.url.clone(),
        sha256: artifact.sha256.clone(),
        size: artifact.size,
    })
}

/// The `host.json` of `host/releases.toml`, or `None` when there are none.
fn build_host(manifest: &Path, fetch: &dyn Fetch) -> Result<Option<HostManifest>, Error> {
    let file: HostReleases =
        read_optional(&manifest.join("host").join("releases.toml"))?.unwrap_or_default();
    if file.release.is_empty() {
        return Ok(None);
    }
    let mut releases = Vec::new();
    let mut latest: Option<Version> = None;
    for release in &file.release {
        let version = Version::parse(&release.version).map_err(|error| {
            Error::Index(format!("{} is not a version: {error}", release.version))
        })?;
        if latest.as_ref().is_none_or(|newest| &version > newest) {
            latest = Some(version);
        }
        releases.push(HostRelease {
            version: release.version.clone(),
            api: release.api,
            platform: platform_of(release.platform.as_deref())?,
            loader: fetch_artifact(fetch, &release.loader, "loader")?,
            installer: fetch_artifact(fetch, &release.installer, "installer")?,
        });
    }
    let host = HostManifest {
        format: host::FORMAT,
        serial: None,
        generated: None,
        latest: latest
            .map(|version| version.to_string())
            .unwrap_or_default(),
        releases,
    };
    let text = serde_json::to_vec(&host).map_err(|error| Error::Index(error.to_string()))?;
    // The checks a client makes when it reads host.json.
    HostManifest::parse(&text)?;
    Ok(Some(host))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    /// A plugin package, packed by the registry's own packer, written to `dist`.
    fn package_in(
        dir: &TempDir,
        id: &str,
        version: &str,
        kind: &str,
        role: &str,
        requires: &[&str],
        file: &str,
    ) -> Vec<u8> {
        let requires: Vec<String> = requires.iter().map(|id| format!("\"{id}\"")).collect();
        let entry = if kind == "dylib" {
            "libdemo.dylib"
        } else {
            "demo.dll"
        };
        dir.write(
            &format!("stage/{id}/plugin.toml"),
            format!(
                "id = \"{id}\"\nname = \"Name of {id}\"\nversion = \"{version}\"\nrole = \"{role}\"\nkind = \"{kind}\"\nentry = \"{entry}\"\ndescription = \"d\"\nauthor = \"a\"\nlicense = \"MIT\"\nrequires = [{}]\n",
                requires.join(", ")
            ),
        );
        dir.write(
            &format!("stage/{id}/{entry}"),
            format!("binary {id} {version}"),
        );
        let bytes = package::pack(&dir.path().join("stage").join(id)).unwrap();
        dir.write(&format!("dist/{file}"), &bytes);
        bytes
    }

    /// A `[[release]]` block for a package written by `package_in`.
    fn release_block(version: &str, file: &str, bytes: &[u8]) -> String {
        format!(
            "[[release]]\nversion = \"{version}\"\nurl = \"https://example.com/{file}\"\nsha256 = \"{}\"\n",
            trust::sha256_hex(bytes)
        )
    }

    fn module(dir: &TempDir, id: &str, default: bool, releases: &str) {
        let default = if default { "default = true\n" } else { "" };
        dir.write(&format!("manifest/plugins/{id}/entry.toml"), format!("id = \"{id}\"\n{default}summary = \"s\"\n\n[build]\nplatforms = [\"windows-x86_64\"]\n"));
        dir.write(&format!("manifest/plugins/{id}/releases.toml"), releases);
    }

    /// Runs `registry build` on the fixture and returns what it printed.
    fn build(dir: &TempDir, extra: &[&str]) -> Result<String, Error> {
        let mut args = vec![
            "--manifest".to_string(),
            dir.path().join("manifest").display().to_string(),
            "--dist".to_string(),
            dir.path().join("dist").display().to_string(),
        ];
        args.extend(extra.iter().map(|word| word.to_string()));
        let mut out = Vec::new();
        run(&args, &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    fn read_index(dir: &TempDir) -> Index {
        Index::parse(&std::fs::read(dir.path().join("manifest/index.json")).unwrap()).unwrap()
    }

    /// Three plugins: a default plugin that needs a library, the library, and
    /// a library nobody default needs.
    fn fixture(dir: &TempDir) {
        let bridge = package_in(
            dir,
            "com.parsec-orbit.webview-bridge",
            "0.1.1",
            "dll",
            "lib",
            &[],
            "bridge.zip",
        );
        let webview = package_in(
            dir,
            "com.parsec-orbit.webview",
            "0.1.1",
            "dll",
            "plugin",
            &["com.parsec-orbit.webview-bridge"],
            "webview.zip",
        );
        let hooks = package_in(
            dir,
            "com.parsec-orbit.hooks",
            "0.1.1",
            "dll",
            "lib",
            &[],
            "hooks.zip",
        );
        module(
            dir,
            "com.parsec-orbit.webview-bridge",
            true,
            &release_block("0.1.1", "bridge.zip", &bridge),
        );
        module(
            dir,
            "com.parsec-orbit.webview",
            true,
            &release_block("0.1.1", "webview.zip", &webview),
        );
        module(
            dir,
            "com.parsec-orbit.hooks",
            false,
            &release_block("0.1.1", "hooks.zip", &hooks),
        );
        module(dir, "com.example.none", false, "");
    }

    #[test]
    fn the_index_is_built_from_the_packages_and_the_manifest() {
        let dir = TempDir::new("build-index");
        fixture(&dir);
        build(&dir, &[]).unwrap();
        let index = read_index(&dir);
        let ids: Vec<&str> = index
            .plugins
            .iter()
            .map(|plugin| plugin.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "com.parsec-orbit.hooks",
                "com.parsec-orbit.webview",
                "com.parsec-orbit.webview-bridge"
            ]
        );
        assert_eq!(index.serial, Some(1));
        assert!(index
            .generated
            .as_deref()
            .is_some_and(|text| text.ends_with('Z') && text.len() == 20));

        let bridge = index.plugin("com.parsec-orbit.webview-bridge").unwrap();
        assert_eq!(
            (bridge.name.as_str(), bridge.role.as_deref()),
            ("Name of com.parsec-orbit.webview-bridge", Some("lib"))
        );
        assert!(bridge.default && bridge.official && bridge.required);
        assert_eq!(bridge.summary.as_deref(), Some("s"));
        let zip = std::fs::read(dir.path().join("dist/bridge.zip")).unwrap();
        assert_eq!(bridge.versions[0].size, zip.len() as u64);
        assert_eq!(bridge.versions[0].sha256, trust::sha256_hex(&zip));
        let webview = index.plugin("com.parsec-orbit.webview").unwrap();
        assert!(
            webview.default && !webview.required,
            "a plugin is never required"
        );
        assert_eq!(
            webview.versions[0].requires,
            ["com.parsec-orbit.webview-bridge"]
        );
        let hooks = index.plugin("com.parsec-orbit.hooks").unwrap();
        assert!(
            !hooks.default && !hooks.required,
            "a lib nobody default needs is not required"
        );
        assert_eq!(hooks.versions[0].platform.as_str(), "windows-x86_64");
    }

    #[test]
    fn rebuilding_an_unchanged_manifest_changes_nothing() {
        let dir = TempDir::new("build-noop");
        fixture(&dir);
        build(&dir, &[]).unwrap();
        let index = std::fs::read(dir.path().join("manifest/index.json")).unwrap();
        let output = build(&dir, &[]).unwrap();
        assert!(
            output.contains("index.json: unchanged, serial 1"),
            "{output}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("manifest/index.json")).unwrap(),
            index
        );
    }

    #[test]
    fn a_new_release_bumps_the_serial() {
        let dir = TempDir::new("build-serial");
        fixture(&dir);
        build(&dir, &[]).unwrap();
        let v2 = package_in(
            &dir,
            "com.parsec-orbit.hooks",
            "0.2.0",
            "dll",
            "lib",
            &[],
            "hooks-0.2.0.zip",
        );
        let releases = dir
            .path()
            .join("manifest/plugins/com.parsec-orbit.hooks/releases.toml");
        let mut text = std::fs::read_to_string(&releases).unwrap();
        text.push('\n');
        text.push_str(&release_block("0.2.0", "hooks-0.2.0.zip", &v2));
        std::fs::write(&releases, text).unwrap();
        let output = build(&dir, &[]).unwrap();
        assert!(output.contains("index.json: written, serial 2"), "{output}");
        let index = read_index(&dir);
        assert_eq!(index.serial, Some(2));
        assert_eq!(
            index
                .plugin("com.parsec-orbit.hooks")
                .unwrap()
                .versions
                .len(),
            2
        );
    }

    #[test]
    fn a_digest_that_does_not_match_stops_the_build() {
        let dir = TempDir::new("build-hash");
        fixture(&dir);
        let releases = dir
            .path()
            .join("manifest/plugins/com.parsec-orbit.hooks/releases.toml");
        let text = std::fs::read_to_string(&releases).unwrap();
        let real = trust::sha256_hex(&std::fs::read(dir.path().join("dist/hooks.zip")).unwrap());
        std::fs::write(&releases, text.replace(&real, &"0".repeat(64))).unwrap();
        let error = build(&dir, &[]).unwrap_err();
        assert!(matches!(error, Error::Hash(_)), "{error}");
        assert!(
            !dir.path().join("manifest/index.json").exists(),
            "nothing is written"
        );
    }

    #[test]
    fn a_package_for_another_id_or_version_is_refused() {
        let dir = TempDir::new("build-wrong");
        fixture(&dir);
        let other = package_in(
            &dir,
            "com.example.other",
            "0.1.1",
            "dll",
            "plugin",
            &[],
            "hooks.zip",
        );
        let releases = dir
            .path()
            .join("manifest/plugins/com.parsec-orbit.hooks/releases.toml");
        std::fs::write(&releases, release_block("0.1.1", "hooks.zip", &other)).unwrap();
        let error = build(&dir, &[]).unwrap_err();
        assert!(
            matches!(&error, Error::Index(text) if text.contains("listed for com.parsec-orbit.hooks")),
            "{error}"
        );
    }

    #[test]
    fn a_signed_index_verifies_with_the_registry() {
        let dir = TempDir::new("build-sign");
        fixture(&dir);
        let (secret, public) = trust::generate_keypair().unwrap();
        let key = dir.write("keys/test.key", format!("{secret}\n"));
        build(&dir, &["--sign", &key.display().to_string()]).unwrap();
        let bytes = std::fs::read(dir.path().join("manifest/index.json")).unwrap();
        let signature =
            std::fs::read_to_string(dir.path().join("manifest/index.json.sig")).unwrap();
        let trusted = [trust::TrustedKey {
            key: trust::public_key(&public).unwrap(),
            comment: "test".to_string(),
        }];
        assert_eq!(trust::verify(&bytes, &signature, &trusted).unwrap(), 0);
    }

    #[test]
    fn an_unsigned_rebuild_removes_the_old_signature() {
        let dir = TempDir::new("build-stale-sig");
        fixture(&dir);
        let (secret, _) = trust::generate_keypair().unwrap();
        let key = dir.write("keys/test.key", format!("{secret}\n"));
        build(&dir, &["--sign", &key.display().to_string()]).unwrap();
        let sig = dir.path().join("manifest/index.json.sig");
        assert!(sig.exists());
        // A change makes the old signature wrong, so it must not be left behind.
        dir.write(
            "manifest/plugins/com.parsec-orbit.hooks/entry.toml",
            "id = \"com.parsec-orbit.hooks\"\nsummary = \"changed\"\n\n[build]\nplatforms = [\"windows-x86_64\"]\n",
        );
        build(&dir, &[]).unwrap();
        assert!(!sig.exists());
    }

    #[test]
    fn host_releases_become_a_signed_host_json() {
        let dir = TempDir::new("build-host");
        fixture(&dir);
        dir.write("dist/bcrypt.dll", b"MZ loader");
        dir.write("dist/orbit-windows-x86_64.exe", b"MZ installer");
        dir.write(
            "manifest/host/releases.toml",
            format!(
                "[[release]]\nversion = \"0.3.0\"\napi = 1\nplatform = \"windows-x86_64\"\nloader = {{ url = \"https://example.com/bcrypt.dll\", sha256 = \"{}\", size = 9 }}\ninstaller = {{ url = \"https://example.com/orbit-windows-x86_64.exe\", sha256 = \"{}\", size = 12 }}\n",
                trust::sha256_hex(b"MZ loader"),
                trust::sha256_hex(b"MZ installer")
            ),
        );
        let output = build(&dir, &[]).unwrap();
        assert!(output.contains("host.json: written, serial 1"), "{output}");
        let host =
            HostManifest::parse(&std::fs::read(dir.path().join("manifest/host.json")).unwrap())
                .unwrap();
        assert_eq!((host.latest.as_str(), host.serial), ("0.3.0", Some(1)));
        assert_eq!(host.releases[0].loader.size, 9);
        assert_eq!(
            host.releases[0].installer.sha256,
            trust::sha256_hex(b"MZ installer")
        );

        let again = build(&dir, &[]).unwrap();
        assert!(again.contains("host.json: unchanged, serial 1"), "{again}");
    }

    #[test]
    fn a_loader_of_the_wrong_size_stops_the_host_build() {
        let dir = TempDir::new("build-host-size");
        fixture(&dir);
        dir.write("dist/bcrypt.dll", b"MZ loader");
        dir.write("dist/orbit-windows-x86_64.exe", b"MZ installer");
        dir.write(
            "manifest/host/releases.toml",
            format!(
                "[[release]]\nversion = \"0.3.0\"\napi = 1\nloader = {{ url = \"https://example.com/bcrypt.dll\", sha256 = \"{}\", size = 99 }}\ninstaller = {{ url = \"https://example.com/orbit-windows-x86_64.exe\", sha256 = \"{}\", size = 13 }}\n",
                trust::sha256_hex(b"MZ loader"),
                trust::sha256_hex(b"MZ installer")
            ),
        );
        let error = build(&dir, &[]).unwrap_err();
        assert!(
            matches!(&error, Error::Hash(text) if text.contains("loader")),
            "{error}"
        );
    }

    #[test]
    fn the_committed_manifest_builds_an_empty_index() {
        // No release is listed yet, so the committed manifest yields no plugins
        // and no host.json. Checked in memory: nothing is written.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../manifest");
        let index = build_index(&root, &Real, None).unwrap();
        assert!(index.plugins.is_empty());
        assert!(build_host(&root, &Real).unwrap().is_none());
    }
}

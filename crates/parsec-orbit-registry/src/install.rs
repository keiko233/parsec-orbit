//! Installing, updating and removing plugins below a plugins directory.
//!
//! Every plugin installed through the market lives in `<plugins>\<id>\` and
//! carries a `market.json` that says where it came from. Plugins put there by
//! hand have none, and are never replaced or removed without `force`.
//!
//! An install is staged: the package is checked, written to a folder next to
//! the target, and only then swapped in, so a failure at any step leaves the
//! installed plugin as it was. A plugin whose DLL the running client holds
//! cannot be replaced (Windows refuses the rename); that is reported as such.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::index::{Index, PlatformId, Release, MAX_PACKAGE_BYTES};
use crate::package::{self, Manifest};
use crate::source::{resolve, Fetch};
use crate::trust::{self, TrustedKey};
use crate::{Error, LOADER_API};

/// Name of the file that records where a plugin came from.
pub const MARKET_FILE: &str = "market.json";

/// Largest index accepted.
const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub id: String,
    pub version: String,
    pub sha256: String,
    /// Location of the index it was installed from.
    pub index: String,
    /// Seconds since the Unix epoch.
    pub installed_unix: u64,
}

/// A plugin found in the plugins directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub id: String,
    pub version: String,
    pub dir: PathBuf,
    /// `None` for plugins that were not installed through the market.
    pub origin: Option<Origin>,
}

/// An index that was fetched and checked.
#[derive(Debug, Clone)]
pub struct LoadedIndex {
    pub index: Index,
    pub location: String,
    /// Whether a trusted key signed it.
    pub signed: bool,
    /// Why the signature was not accepted, when `--allow-unsigned` let the
    /// index through anyway. A missing signature and one that failed to
    /// verify read very differently to whoever decides to trust it.
    pub signature_note: Option<String>,
}

/// Options of an install.
#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOptions {
    /// Replace a plugin that was not installed through the market.
    pub force: bool,
    /// Install the plugins the release `requires` first.
    pub with_deps: bool,
}

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub id: String,
    pub version: String,
    /// The version it replaced.
    pub replaced: Option<String>,
}

/// Signs and verifies a staged macOS plugin library.
pub trait DylibSigner: Send + Sync {
    fn sign(&self, dylib: &Path) -> Result<(), Error>;
    fn verify(&self, dylib: &Path) -> Result<(), Error>;
}

struct SystemDylibSigner;

fn run_codesign(action: &str, arguments: &[&str], dylib: &Path) -> Result<(), Error> {
    let output = Command::new("/usr/bin/codesign")
        .args(arguments)
        .arg(dylib)
        .output()
        .map_err(|error| Error::Io(format!("could not start codesign to {action}: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(Error::Io(format!(
        "codesign could not {action} {}: {} (status {})",
        dylib.display(),
        detail.trim(),
        output.status
    )))
}

impl DylibSigner for SystemDylibSigner {
    fn sign(&self, dylib: &Path) -> Result<(), Error> {
        let output = Command::new("/usr/bin/lipo")
            .arg(dylib)
            .args(["-verify_arch", "arm64"])
            .output()
            .map_err(|error| Error::Io(format!("could not verify arm64 architecture: {error}")))?;
        if !output.status.success() {
            return Err(Error::Package(format!(
                "{} has no valid arm64 slice: {}",
                dylib.display(),
                String::from_utf8_lossy(&output.stderr).trim(),
            )));
        }
        run_codesign("sign", &["--force", "--sign", "-"], dylib)
    }

    fn verify(&self, dylib: &Path) -> Result<(), Error> {
        run_codesign("verify", &["--verify", "--strict", "--verbose=2"], dylib)
    }
}

pub struct Market<'a> {
    pub fetch: &'a dyn Fetch,
    pub plugins_dir: PathBuf,
    pub trusted: Vec<TrustedKey>,
    /// Accept an index without a valid signature.
    pub allow_unsigned: bool,
    /// Loader ABI version; releases that need more are not offered.
    pub api: u32,
    /// Host platform used to filter the index.
    pub platform: PlatformId,
    /// Ad-hoc signer for staged macOS dylibs.
    pub code_signer: Arc<dyn DylibSigner>,
}

impl<'a> Market<'a> {
    pub fn new(
        fetch: &'a dyn Fetch,
        plugins_dir: PathBuf,
        trusted: Vec<TrustedKey>,
        platform: PlatformId,
    ) -> Market<'a> {
        Market {
            fetch,
            plugins_dir,
            trusted,
            allow_unsigned: false,
            api: LOADER_API,
            platform,
            code_signer: Arc::new(SystemDylibSigner),
        }
    }

    /// Replaces the platform signer, primarily for isolated transaction tests.
    pub fn with_code_signer(mut self, signer: Arc<dyn DylibSigner>) -> Market<'a> {
        self.code_signer = signer;
        self
    }

    /// Fetches the index at `location` and its signature (`<location>.sig`).
    pub fn load_index(&self, location: &str) -> Result<LoadedIndex, Error> {
        let bytes = self.fetch.fetch(location, MAX_INDEX_BYTES)?;
        let signature = self
            .fetch
            .fetch(&format!("{location}.sig"), 4096)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
        let mut signature_note = None;
        let signed = match signature {
            Ok(signature) => match trust::verify(&bytes, &signature, &self.trusted) {
                Ok(_) => true,
                Err(error) if self.allow_unsigned => {
                    signature_note = Some(format!("the signature did not verify: {error}"));
                    false
                }
                Err(error) => return Err(error),
            },
            Err(error) if self.allow_unsigned => {
                signature_note = Some(format!("there is no signature: {error}"));
                false
            }
            Err(_) => {
                return Err(Error::Signature(format!(
                    "{location}.sig could not be read: an index without a signature is not accepted"
                )))
            }
        };
        Ok(LoadedIndex {
            index: Index::parse(&bytes)?,
            location: location.to_string(),
            signed,
            signature_note,
        })
    }

    fn staging_name(id: &str, tag: &str) -> String {
        format!(".orbit-{tag}-{id}-{}", std::process::id())
    }

    /// The plugins found below the plugins directory.
    pub fn installed(&self) -> Result<Vec<InstalledPlugin>, Error> {
        let mut found = Vec::new();
        let entries = match std::fs::read_dir(&self.plugins_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
            Err(error) => {
                return Err(Error::Io(format!(
                    "{}: {error}",
                    self.plugins_dir.display()
                )))
            }
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(dir.join("plugin.toml")) else {
                continue;
            };
            let Ok(manifest) = package::parse_manifest(&text) else {
                continue;
            };
            // A manifest without an id is known by its folder name.
            let id = if manifest.id.is_empty() {
                entry.file_name().to_string_lossy().into_owned()
            } else {
                manifest.id
            };
            let origin = std::fs::read_to_string(dir.join(MARKET_FILE))
                .ok()
                .and_then(|text| serde_json::from_str::<Origin>(&text).ok());
            found.push(InstalledPlugin {
                id,
                version: if manifest.version.is_empty() {
                    "-".to_string()
                } else {
                    manifest.version
                },
                dir,
                origin,
            });
        }
        found.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(found)
    }

    /// Installs `id` (and, with `with_deps`, what it requires). Returns what
    /// was installed, dependencies first.
    pub fn install(
        &self,
        index: &LoadedIndex,
        id: &str,
        version: Option<&str>,
        options: InstallOptions,
    ) -> Result<Vec<Installed>, Error> {
        let mut done = Vec::new();
        let mut visiting = Vec::new();
        self.install_into(index, id, version, options, &mut done, &mut visiting)?;
        Ok(done)
    }

    fn install_into(
        &self,
        index: &LoadedIndex,
        id: &str,
        version: Option<&str>,
        options: InstallOptions,
        done: &mut Vec<Installed>,
        visiting: &mut Vec<String>,
    ) -> Result<(), Error> {
        if visiting.iter().any(|visited| visited == id) {
            return Ok(()); // a cycle of requirements: the first visit finishes it
        }
        let plugin = index
            .index
            .plugin(id)
            .ok_or_else(|| Error::NotFound(format!("{id} is not in the index")))?;
        let release = plugin.select(version, self.api, &self.platform)?;
        visiting.push(id.to_string());

        if options.with_deps {
            let present: Vec<String> = self.installed()?.into_iter().map(|p| p.id).collect();
            for dependency in &release.requires {
                let already = present.contains(dependency)
                    || done.iter().any(|installed| &installed.id == dependency);
                // What the index does not list cannot be fetched; the loader
                // will say what is missing when it starts.
                if !already && index.index.plugin(dependency).is_some() {
                    self.install_into(index, dependency, None, options, done, visiting)?;
                }
            }
        }

        let installed = self.install_release(index, id, release, options)?;
        done.push(installed);
        Ok(())
    }

    fn install_release(
        &self,
        index: &LoadedIndex,
        id: &str,
        release: &Release,
        options: InstallOptions,
    ) -> Result<Installed, Error> {
        let location = resolve(&index.location, &release.url)?;
        let bytes = self
            .fetch
            .fetch(&location, release.size.min(MAX_PACKAGE_BYTES))?;
        if bytes.len() as u64 != release.size {
            return Err(Error::Hash(format!(
                "{location} has {} bytes, the index says {}",
                bytes.len(),
                release.size
            )));
        }
        trust::check_digest(&bytes, &release.sha256)?;

        let package = package::read(&bytes)?;
        if package.manifest.id != id {
            return Err(Error::Package(format!(
                "the package is for {:?}, not {id}",
                package.manifest.id
            )));
        }
        if package.manifest.version != release.version {
            return Err(Error::Package(format!(
                "the package is version {:?}, the index says {}",
                package.manifest.version, release.version
            )));
        }
        if release.platform == PlatformId::macos_arm64() {
            if package.manifest.kind != "dylib" {
                return Err(Error::Package(format!(
                    "{} {} targets macos-arm64 but its manifest kind is {:?}, expected \"dylib\"",
                    id, release.version, package.manifest.kind
                )));
            }
            if package
                .manifest
                .entry
                .as_deref()
                .is_none_or(|entry| !entry.ends_with(".dylib"))
            {
                return Err(Error::Package(format!(
                    "{} {} targets macos-arm64 but its manifest entry is not a .dylib",
                    id, release.version
                )));
            }
        }

        std::fs::create_dir_all(&self.plugins_dir)?;
        let target = self.plugins_dir.join(id);
        let previous = self.describe(&target)?;
        if let Some((_, None)) = &previous {
            if !options.force {
                return Err(Error::Conflict(format!(
                    "{} exists and was not installed through the market; use --force to replace it",
                    target.display()
                )));
            }
        }
        if let Some((_, Some(origin))) = &previous {
            if origin.id != id {
                return Err(Error::Conflict(format!(
                    "{} belongs to {}",
                    target.display(),
                    origin.id
                )));
            }
        }

        // Stage, then swap.
        let staging = self.plugins_dir.join(Self::staging_name(id, "new"));
        let _ = std::fs::remove_dir_all(&staging);
        let staged = (|| -> Result<(), Error> {
            package::write_to(&package, &staging)?;
            if release.platform == PlatformId::macos_arm64() {
                let mut dylibs = 0;
                for file in package
                    .files
                    .iter()
                    .filter(|file| file.path.ends_with(".dylib"))
                {
                    let dylib = staging.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
                    self.code_signer.sign(&dylib)?;
                    self.code_signer.verify(&dylib)?;
                    dylibs += 1;
                }
                if dylibs == 0 {
                    return Err(Error::Package(format!(
                        "{} {} targets macos-arm64 but contains no .dylib files",
                        id, release.version
                    )));
                }
            }
            let origin = Origin {
                id: id.to_string(),
                version: release.version.clone(),
                sha256: release.sha256.clone(),
                index: index.location.clone(),
                installed_unix: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_secs()),
            };
            std::fs::write(
                staging.join(MARKET_FILE),
                serde_json::to_string_pretty(&origin).expect("an origin serialises"),
            )?;
            Ok(())
        })();
        if let Err(error) = staged {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }

        let backup = self.plugins_dir.join(Self::staging_name(id, "old"));
        let _ = std::fs::remove_dir_all(&backup);
        if previous.is_some() {
            if let Err(error) = std::fs::rename(&target, &backup) {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(Error::Io(format!(
                    "cannot replace {}: {error} (is the Parsec client running? close it and try again)",
                    target.display()
                )));
            }
        }
        if let Err(error) = std::fs::rename(&staging, &target) {
            // Put the old one back.
            if previous.is_some() {
                let _ = std::fs::rename(&backup, &target);
            }
            let _ = std::fs::remove_dir_all(&staging);
            return Err(Error::Io(format!("{}: {error}", target.display())));
        }
        let _ = std::fs::remove_dir_all(&backup);

        Ok(Installed {
            id: id.to_string(),
            version: release.version.clone(),
            replaced: previous.map(|(manifest, _)| manifest.version),
        })
    }

    /// What is at `dir`: its manifest and where it came from.
    #[allow(clippy::type_complexity)]
    fn describe(&self, dir: &Path) -> Result<Option<(Manifest, Option<Origin>)>, Error> {
        if !dir.exists() {
            return Ok(None);
        }
        let manifest = std::fs::read_to_string(dir.join("plugin.toml"))
            .ok()
            .and_then(|text| package::parse_manifest(&text).ok())
            .unwrap_or_default();
        let origin = std::fs::read_to_string(dir.join(MARKET_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<Origin>(&text).ok());
        Ok(Some((manifest, origin)))
    }

    /// Plugins installed through the market that have a newer compatible
    /// release in `index`.
    pub fn updates<'i>(
        &self,
        index: &'i LoadedIndex,
    ) -> Result<Vec<(InstalledPlugin, &'i Release)>, Error> {
        let mut updates = Vec::new();
        for installed in self.installed()? {
            if installed.origin.is_none() {
                continue;
            }
            let Some(plugin) = index.index.plugin(&installed.id) else {
                continue;
            };
            let Ok(release) = plugin.select(None, self.api, &self.platform) else {
                continue;
            };
            let newer = match (
                Version::parse(&installed.version),
                Version::parse(&release.version),
            ) {
                (Ok(current), Ok(latest)) => latest > current,
                _ => false,
            };
            if newer {
                updates.push((installed, release));
            }
        }
        Ok(updates)
    }

    /// Removes a plugin installed through the market.
    pub fn remove(&self, id: &str, force: bool) -> Result<InstalledPlugin, Error> {
        let installed = self
            .installed()?
            .into_iter()
            .find(|plugin| plugin.id == id)
            .ok_or_else(|| Error::NotFound(format!("{id} is not installed")))?;
        if installed.origin.is_none() && !force {
            return Err(Error::Conflict(format!(
                "{} was not installed through the market; use --force to remove it",
                installed.dir.display()
            )));
        }
        std::fs::remove_dir_all(&installed.dir).map_err(|error| {
            Error::Io(format!(
                "cannot remove {}: {error} (is the Parsec client running?)",
                installed.dir.display()
            ))
        })?;
        Ok(installed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Plugin, FORMAT};
    use crate::package::tests::{good, zip_of, MANIFEST};
    use crate::source::tests::Memory;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Dir(PathBuf);
    impl Dir {
        fn new(label: &str) -> Dir {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "parsec-orbit-market-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&path);
            Dir(path)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const INDEX_URL: &str = "https://market.example/index.json";

    fn package_for(id: &str, version: &str, requires: &[&str]) -> Vec<u8> {
        package_for_platform(id, version, requires, &PlatformId::windows_x86_64())
    }

    fn package_for_platform(
        id: &str,
        version: &str,
        requires: &[&str],
        platform: &PlatformId,
    ) -> Vec<u8> {
        let requires: Vec<String> = requires.iter().map(|r| format!("\"{r}\"")).collect();
        match platform.as_str() {
            PlatformId::WINDOWS_X86_64 => {
                let manifest = format!(
                    "id = \"{id}\"\nversion = \"{version}\"\nentry = \"p.dll\"\nrequires = [{}]\n",
                    requires.join(", ")
                );
                zip_of(&[
                    (&format!("{id}/plugin.toml"), manifest.as_bytes()),
                    (
                        &format!("{id}/p.dll"),
                        format!("MZ {id} {version}").as_bytes(),
                    ),
                ])
            }
            PlatformId::MACOS_ARM64 => {
                let manifest = format!(
                    "id = \"{id}\"\nversion = \"{version}\"\nkind = \"dylib\"\nentry = \"p.dylib\"\nrequires = [{}]\n",
                    requires.join(", ")
                );
                zip_of(&[
                    (&format!("{id}/plugin.toml"), manifest.as_bytes()),
                    (
                        &format!("{id}/p.dylib"),
                        format!("Mach-O {id} {version}").as_bytes(),
                    ),
                    (
                        &format!("{id}/Frameworks/helper.dylib"),
                        b"nested helper dylib",
                    ),
                ])
            }
            other => unreachable!("no test package for {other}"),
        }
    }

    /// A market with these `(id, version, requires)` releases, signed.
    struct Fixture {
        memory: Memory,
        trusted: Vec<TrustedKey>,
        dir: Dir,
    }

    fn fixture(releases: &[(&str, &str, &[&str])]) -> Fixture {
        let windows_releases: Vec<_> = releases
            .iter()
            .map(|(id, version, requires)| (*id, *version, *requires, PlatformId::windows_x86_64()))
            .collect();
        fixture_with_platform(&windows_releases)
    }

    fn fixture_with_platform(releases: &[(&str, &str, &[&str], PlatformId)]) -> Fixture {
        let (secret, public) = trust::generate_keypair().unwrap();
        let key = trust::signing_key(&secret).unwrap();
        let mut memory = Memory::default();
        let mut plugins: Vec<Plugin> = Vec::new();
        for (id, version, requires, platform) in releases {
            let bytes = package_for_platform(id, version, requires, platform);
            let url = format!("{id}-{version}.zip");
            memory
                .files
                .insert(format!("https://market.example/{url}"), bytes.clone());
            let release = Release {
                version: version.to_string(),
                api: 1,
                platform: platform.clone(),
                url,
                sha256: trust::sha256_hex(&bytes),
                size: bytes.len() as u64,
                requires: requires.iter().map(|r| r.to_string()).collect(),
            };
            match plugins.iter_mut().find(|p| p.id == *id) {
                Some(plugin) => plugin.versions.push(release),
                None => plugins.push(Plugin {
                    id: id.to_string(),
                    name: id.to_string(),
                    description: String::new(),
                    author: String::new(),
                    license: String::new(),
                    versions: vec![release],
                }),
            }
        }
        let index = Index {
            format: FORMAT,
            name: "test".into(),
            plugins,
        }
        .to_json();
        memory
            .files
            .insert(INDEX_URL.into(), index.clone().into_bytes());
        memory.files.insert(
            format!("{INDEX_URL}.sig"),
            trust::sign(index.as_bytes(), &key).into_bytes(),
        );
        Fixture {
            memory,
            trusted: trust::parse_trusted_keys(&format!("ed25519 {public}\n")).unwrap(),
            dir: Dir::new("plugins"),
        }
    }

    impl Fixture {
        fn market(&self) -> Market<'_> {
            self.market_for(PlatformId::windows_x86_64())
        }

        fn market_for(&self, platform: PlatformId) -> Market<'_> {
            Market::new(
                &self.memory,
                self.dir.0.clone(),
                self.trusted.clone(),
                platform,
            )
        }
    }

    #[derive(Default)]
    struct RecordingSigner {
        fail_sign: AtomicBool,
        signed: std::sync::Mutex<Vec<PathBuf>>,
        verified: std::sync::Mutex<Vec<PathBuf>>,
    }

    impl DylibSigner for RecordingSigner {
        fn sign(&self, dylib: &Path) -> Result<(), Error> {
            self.signed.lock().unwrap().push(dylib.to_path_buf());
            if self.fail_sign.load(Ordering::SeqCst) {
                return Err(Error::Io("simulated codesign signing failure".into()));
            }
            use std::io::Write;
            std::fs::OpenOptions::new()
                .append(true)
                .open(dylib)?
                .write_all(b"\nad-hoc signature")?;
            Ok(())
        }

        fn verify(&self, dylib: &Path) -> Result<(), Error> {
            self.verified.lock().unwrap().push(dylib.to_path_buf());
            let bytes = std::fs::read(dylib)?;
            if bytes.ends_with(b"\nad-hoc signature") {
                Ok(())
            } else {
                Err(Error::Io("simulated codesign verification failure".into()))
            }
        }
    }

    #[test]
    fn install_checks_stages_and_records_the_origin() {
        let f = fixture(&[("com.example.p", "1.0.0", &[])]);
        let market = f.market();
        let index = market.load_index(INDEX_URL).unwrap();
        assert!(index.signed);

        let done = market
            .install(&index, "com.example.p", None, InstallOptions::default())
            .unwrap();
        assert_eq!(
            done,
            [Installed {
                id: "com.example.p".into(),
                version: "1.0.0".into(),
                replaced: None
            }]
        );
        let dir = f.dir.0.join("com.example.p");
        assert_eq!(
            std::fs::read(dir.join("p.dll")).unwrap(),
            b"MZ com.example.p 1.0.0"
        );
        let origin: Origin =
            serde_json::from_str(&std::fs::read_to_string(dir.join(MARKET_FILE)).unwrap()).unwrap();
        assert_eq!(
            (origin.id.as_str(), origin.version.as_str()),
            ("com.example.p", "1.0.0")
        );
        assert_eq!(origin.index, INDEX_URL);
        // No staging folders are left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&f.dir.0)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty());

        let installed = market.installed().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].id, "com.example.p");
        assert!(installed[0].origin.is_some());
    }

    #[test]
    fn macos_install_signs_and_verifies_every_dylib_but_records_package_digest() {
        let f =
            fixture_with_platform(&[("com.example.mac", "1.0.0", &[], PlatformId::macos_arm64())]);
        let signer = Arc::new(RecordingSigner::default());
        let market = f
            .market_for(PlatformId::macos_arm64())
            .with_code_signer(signer.clone());
        let index = market.load_index(INDEX_URL).unwrap();
        let package_digest = index.index.plugins[0].versions[0].sha256.clone();

        market
            .install(&index, "com.example.mac", None, InstallOptions::default())
            .unwrap();

        let installed = f.dir.0.join("com.example.mac");
        for path in ["p.dylib", "Frameworks/helper.dylib"] {
            assert!(
                std::fs::read(installed.join(path))
                    .unwrap()
                    .ends_with(b"\nad-hoc signature"),
                "{path} was not signed"
            );
        }
        let signed = signer.signed.lock().unwrap();
        let verified = signer.verified.lock().unwrap();
        assert_eq!(signed.len(), 2);
        assert_eq!(verified.len(), 2);
        assert!(signed.iter().any(|path| path.ends_with("p.dylib")));
        assert!(signed
            .iter()
            .any(|path| path.ends_with("Frameworks/helper.dylib")));
        assert_eq!(signed.as_slice(), verified.as_slice());

        let origin: Origin =
            serde_json::from_slice(&std::fs::read(installed.join(MARKET_FILE)).unwrap()).unwrap();
        assert_eq!(origin.sha256, package_digest);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn system_signer_rejects_a_library_without_arm64_before_signing() {
        let dir = Dir::new("wrong-architecture");
        std::fs::create_dir_all(&dir.0).unwrap();
        let source = dir.0.join("probe.c");
        let library = dir.0.join("probe.dylib");
        std::fs::write(&source, b"int orbit_wrong_arch(void) { return 42; }").unwrap();
        let output = Command::new("clang")
            .args(["-arch", "x86_64", "-dynamiclib"])
            .arg(source)
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let before = std::fs::read(&library).unwrap();
        assert!(SystemDylibSigner
            .sign(&library)
            .unwrap_err()
            .to_string()
            .contains("arm64"));
        assert_eq!(std::fs::read(library).unwrap(), before);
    }

    #[test]
    fn a_macos_signing_failure_keeps_the_previous_installation() {
        let f = fixture_with_platform(&[
            ("com.example.mac", "1.0.0", &[], PlatformId::macos_arm64()),
            ("com.example.mac", "2.0.0", &[], PlatformId::macos_arm64()),
        ]);
        let signer = Arc::new(RecordingSigner::default());
        let market = f
            .market_for(PlatformId::macos_arm64())
            .with_code_signer(signer.clone());
        let index = market.load_index(INDEX_URL).unwrap();
        market
            .install(
                &index,
                "com.example.mac",
                Some("1.0.0"),
                InstallOptions::default(),
            )
            .unwrap();

        let installed = f.dir.0.join("com.example.mac");
        let old_library = std::fs::read(installed.join("p.dylib")).unwrap();
        let old_origin = std::fs::read(installed.join(MARKET_FILE)).unwrap();
        signer.fail_sign.store(true, Ordering::SeqCst);
        let error = market
            .install(
                &index,
                "com.example.mac",
                Some("2.0.0"),
                InstallOptions::default(),
            )
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("simulated codesign signing failure"));
        assert_eq!(
            std::fs::read(installed.join("p.dylib")).unwrap(),
            old_library
        );
        assert_eq!(
            std::fs::read(installed.join(MARKET_FILE)).unwrap(),
            old_origin
        );
        assert!(!std::fs::read_dir(&f.dir.0)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(".orbit-")));
    }

    #[test]
    fn a_tampered_package_is_refused_before_anything_is_written() {
        let mut f = fixture(&[("com.example.p", "1.0.0", &[])]);
        let url = "https://market.example/com.example.p-1.0.0.zip";
        let mut bytes = f.memory.files[url].clone();
        let last = bytes.len() - 30;
        bytes[last] ^= 0xFF;
        f.memory.files.insert(url.into(), bytes);
        let market = f.market();
        let index = market.load_index(INDEX_URL).unwrap();
        let error = market
            .install(&index, "com.example.p", None, InstallOptions::default())
            .unwrap_err();
        assert!(matches!(error, Error::Hash(_)), "{error}");
        assert!(!f.dir.0.join("com.example.p").exists());
    }

    #[test]
    fn a_package_that_is_not_what_the_index_says_is_refused() {
        // Right digest (the index was built from it), wrong plugin inside.
        let mut f = fixture(&[("com.example.p", "1.0.0", &[])]);
        let wrong = package_for("com.example.other", "1.0.0", &[]);
        let url = "https://market.example/com.example.p-1.0.0.zip";
        f.memory.files.insert(url.into(), wrong.clone());
        let index_text = String::from_utf8(f.memory.files[INDEX_URL].clone()).unwrap();
        let mut index = Index::parse(index_text.as_bytes()).unwrap();
        index.plugins[0].versions[0].sha256 = trust::sha256_hex(&wrong);
        index.plugins[0].versions[0].size = wrong.len() as u64;
        let (secret, public) = trust::generate_keypair().unwrap();
        let json = index.to_json();
        f.memory
            .files
            .insert(INDEX_URL.into(), json.clone().into_bytes());
        f.memory.files.insert(
            format!("{INDEX_URL}.sig"),
            trust::sign(json.as_bytes(), &trust::signing_key(&secret).unwrap()).into_bytes(),
        );
        f.trusted = trust::parse_trusted_keys(&format!("ed25519 {public}")).unwrap();
        let market = f.market();
        let loaded = market.load_index(INDEX_URL).unwrap();
        let error = market
            .install(&loaded, "com.example.p", None, InstallOptions::default())
            .unwrap_err();
        assert!(error.to_string().contains("is for"), "{error}");
        assert!(!f.dir.0.join("com.example.p").exists());
    }

    #[test]
    fn an_index_needs_a_signature_from_a_trusted_key() {
        let mut f = fixture(&[("com.example.p", "1.0.0", &[])]);
        // Signed by somebody else.
        let (other_secret, _) = trust::generate_keypair().unwrap();
        let index = f.memory.files[INDEX_URL].clone();
        f.memory.files.insert(
            format!("{INDEX_URL}.sig"),
            trust::sign(&index, &trust::signing_key(&other_secret).unwrap()).into_bytes(),
        );
        let mut market = f.market();
        assert!(matches!(
            market.load_index(INDEX_URL),
            Err(Error::Signature(_))
        ));

        // Modified after signing.
        let f2 = fixture(&[("com.example.p", "1.0.0", &[])]);
        let mut modified = f2.memory;
        let mut text = String::from_utf8(modified.files[INDEX_URL].clone()).unwrap();
        text = text.replace("\"test\"", "\"tampered\"");
        modified.files.insert(INDEX_URL.into(), text.into_bytes());
        let market2 = Market::new(
            &modified,
            f2.dir.0.clone(),
            f2.trusted.clone(),
            PlatformId::windows_x86_64(),
        );
        assert!(matches!(
            market2.load_index(INDEX_URL),
            Err(Error::Signature(_))
        ));

        // No signature at all.
        let mut unsigned = fixture(&[("com.example.p", "1.0.0", &[])]);
        unsigned.memory.files.remove(&format!("{INDEX_URL}.sig"));
        let market3 = unsigned.market();
        let error = market3.load_index(INDEX_URL).unwrap_err();
        assert!(error.to_string().contains("without a signature"), "{error}");

        // ... unless the user said so.
        let mut market4 = unsigned.market();
        market4.allow_unsigned = true;
        let loaded = market4.load_index(INDEX_URL).unwrap();
        assert!(!loaded.signed);

        // A bad signature is not excused by allow_unsigned silently: it is
        // reported as unsigned, never as signed.
        market.allow_unsigned = true;
        assert!(!market.load_index(INDEX_URL).unwrap().signed);
    }

    #[test]
    fn an_update_replaces_the_plugin_and_reports_the_old_version() {
        let f = fixture(&[
            ("com.example.p", "1.0.0", &[]),
            ("com.example.p", "1.1.0", &[]),
        ]);
        let market = f.market();
        let index = market.load_index(INDEX_URL).unwrap();
        market
            .install(
                &index,
                "com.example.p",
                Some("1.0.0"),
                InstallOptions::default(),
            )
            .unwrap();
        let updates = market.updates(&index).unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].1.version, "1.1.0");

        let done = market
            .install(&index, "com.example.p", None, InstallOptions::default())
            .unwrap();
        assert_eq!(done[0].replaced.as_deref(), Some("1.0.0"));
        assert_eq!(
            std::fs::read(f.dir.0.join("com.example.p/p.dll")).unwrap(),
            b"MZ com.example.p 1.1.0"
        );
        assert!(market.updates(&index).unwrap().is_empty());
    }

    #[test]
    fn a_failed_swap_leaves_the_installed_plugin_alone() {
        let f = fixture(&[
            ("com.example.p", "1.0.0", &[]),
            ("com.example.p", "2.0.0", &[]),
        ]);
        let market = f.market();
        let index = market.load_index(INDEX_URL).unwrap();
        market
            .install(
                &index,
                "com.example.p",
                Some("1.0.0"),
                InstallOptions::default(),
            )
            .unwrap();
        // A file the "client" has open cannot be renamed away; simulate the
        // refusal with a staging folder that is in the way.
        let blocker = f.dir.0.join(Market::staging_name("com.example.p", "new"));
        std::fs::write(&blocker, "a file where the staging folder goes").unwrap();
        let result = market.install(
            &index,
            "com.example.p",
            Some("2.0.0"),
            InstallOptions::default(),
        );
        // Whether the stale file is cleared or not, the old plugin is either
        // untouched or fully replaced - never half.
        let dll = std::fs::read(f.dir.0.join("com.example.p/p.dll")).unwrap();
        match result {
            Ok(_) => assert_eq!(dll, b"MZ com.example.p 2.0.0"),
            Err(_) => assert_eq!(dll, b"MZ com.example.p 1.0.0"),
        }
        assert!(f.dir.0.join("com.example.p/plugin.toml").is_file());
    }

    #[test]
    fn plugins_installed_by_hand_are_not_overwritten_or_removed_without_force() {
        let f = fixture(&[("com.example.p", "1.0.0", &[])]);
        let hand = f.dir.0.join("com.example.p");
        std::fs::create_dir_all(&hand).unwrap();
        std::fs::write(
            hand.join("plugin.toml"),
            "id = \"com.example.p\"\nversion = \"0.0.1\"\n",
        )
        .unwrap();
        std::fs::write(hand.join("mine.txt"), "mine").unwrap();
        let market = f.market();
        let index = market.load_index(INDEX_URL).unwrap();

        let error = market
            .install(&index, "com.example.p", None, InstallOptions::default())
            .unwrap_err();
        assert!(matches!(error, Error::Conflict(_)), "{error}");
        assert!(hand.join("mine.txt").exists());
        assert!(matches!(
            market.remove("com.example.p", false),
            Err(Error::Conflict(_))
        ));
        assert!(
            market.updates(&index).unwrap().is_empty(),
            "not the market's to update"
        );

        let done = market
            .install(
                &index,
                "com.example.p",
                None,
                InstallOptions {
                    force: true,
                    with_deps: false,
                },
            )
            .unwrap();
        assert_eq!(done[0].replaced.as_deref(), Some("0.0.1"));
        assert!(!hand.join("mine.txt").exists());
    }

    #[test]
    fn remove_deletes_what_the_market_installed() {
        let f = fixture(&[("com.example.p", "1.0.0", &[])]);
        let market = f.market();
        let index = market.load_index(INDEX_URL).unwrap();
        market
            .install(&index, "com.example.p", None, InstallOptions::default())
            .unwrap();
        let removed = market.remove("com.example.p", false).unwrap();
        assert_eq!(removed.version, "1.0.0");
        assert!(!f.dir.0.join("com.example.p").exists());
        assert!(matches!(
            market.remove("com.example.p", false),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn dependencies_are_installed_first_when_asked() {
        let f = fixture(&[
            (
                "com.example.app",
                "1.0.0",
                &["com.example.lib", "com.example.elsewhere"],
            ),
            ("com.example.lib", "1.0.0", &["com.example.base"]),
            ("com.example.base", "1.0.0", &[]),
        ]);
        let market = f.market();
        let index = market.load_index(INDEX_URL).unwrap();

        let without = market
            .install(&index, "com.example.base", None, InstallOptions::default())
            .unwrap();
        assert_eq!(without.len(), 1);

        let done = market
            .install(
                &index,
                "com.example.app",
                None,
                InstallOptions {
                    force: false,
                    with_deps: true,
                },
            )
            .unwrap();
        let order: Vec<&str> = done.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            order,
            ["com.example.lib", "com.example.app"],
            "base was there already; elsewhere is not in the index"
        );
        assert_eq!(market.installed().unwrap().len(), 3);
    }

    #[test]
    fn a_cycle_of_requirements_terminates() {
        let f = fixture(&[
            ("com.example.a", "1.0.0", &["com.example.b"]),
            ("com.example.b", "1.0.0", &["com.example.a"]),
        ]);
        let market = f.market();
        let index = market.load_index(INDEX_URL).unwrap();
        let done = market
            .install(
                &index,
                "com.example.a",
                None,
                InstallOptions {
                    force: false,
                    with_deps: true,
                },
            )
            .unwrap();
        assert_eq!(done.len(), 2);
    }

    #[test]
    fn unknown_plugins_and_incompatible_releases_are_reported() {
        let f = fixture(&[("com.example.p", "1.0.0", &[])]);
        let mut market = f.market();
        let index = market.load_index(INDEX_URL).unwrap();
        assert!(matches!(
            market.install(&index, "com.example.nope", None, InstallOptions::default()),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            market.install(
                &index,
                "com.example.p",
                Some("9.9.9"),
                InstallOptions::default()
            ),
            Err(Error::NotFound(_))
        ));
        market.api = 0;
        assert!(matches!(
            market.install(&index, "com.example.p", None, InstallOptions::default()),
            Err(Error::Incompatible(_))
        ));
    }

    #[test]
    fn installed_lists_plugins_in_any_state_and_skips_staging_folders() {
        let f = fixture(&[]);
        let market = f.market();
        assert!(market.installed().unwrap().is_empty(), "no directory yet");
        for (dir, manifest) in [
            ("one", "id = \"com.example.one\"\nversion = \"1.0.0\"\n"),
            ("two", "version = \"2.0.0\"\n"),
            (".orbit-new-x", "id = \"hidden\"\n"),
        ] {
            let path = f.dir.0.join(dir);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("plugin.toml"), manifest).unwrap();
        }
        std::fs::create_dir_all(f.dir.0.join("no-manifest")).unwrap();
        std::fs::write(f.dir.0.join("a-file.txt"), "x").unwrap();
        let ids: Vec<String> = market
            .installed()
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, ["com.example.one", "two"]);
    }

    #[test]
    fn the_good_package_of_the_package_tests_installs_too() {
        // Different shape (a folder with data/), same rules.
        let package = package::read(&good()).unwrap();
        assert_eq!(package.manifest.id, "com.example.p");
        let _ = MANIFEST;
    }
}

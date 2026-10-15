//! `package --platform <id> --out DIR`: stages and zips every official module
//! of a platform from the release build. The module list is `manifest/`'s
//! `[build]` tables; the crate behind a module is the workspace folder whose
//! `plugin.toml` has its id (`docs/design/restructure.md`, sections 4.3 and 5).
//!
//! Build never packages and packaging never builds: run `cargo build --release`
//! first, and this command fails naming any library that is missing.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use parsec_orbit_registry::index::PlatformId;
use parsec_orbit_registry::{package, trust, Error};
use serde::Deserialize;

use crate::args::{usage, Args};
use crate::manifest::{read_optional, sorted_dirs, Entry};
use crate::version;

/// What a platform's packages and loader look like.
struct Target {
    kind: &'static str,
    prefix: &'static str,
    suffix: &'static str,
    loader: &'static str,
}

impl Target {
    fn for_platform(platform: &str) -> Result<Target, Error> {
        match platform {
            PlatformId::WINDOWS_X86_64 => Ok(Target {
                kind: "dll",
                prefix: "",
                suffix: ".dll",
                loader: "bcrypt.dll",
            }),
            PlatformId::MACOS_ARM64 => Ok(Target {
                kind: "dylib",
                prefix: "lib",
                suffix: ".dylib",
                loader: "libparsec_orbit_loader.dylib",
            }),
            other => Err(usage(format!("there are no packaging rules for {other}"))),
        }
    }

    /// The library file a crate with library name `name` builds to.
    fn library(&self, name: &str) -> String {
        format!("{}{name}{}", self.prefix, self.suffix)
    }
}

#[derive(Deserialize)]
struct PluginId {
    id: String,
}

#[derive(Deserialize)]
struct Cargo {
    package: CargoPackage,
    lib: Option<CargoLib>,
}

#[derive(Deserialize)]
struct CargoPackage {
    name: String,
}

#[derive(Deserialize)]
struct CargoLib {
    name: Option<String>,
}

pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), Error> {
    let parsed = Args::parse(
        args,
        &["--platform", "--out", "--workspace", "--target-dir"],
    )?;
    parsed.no_words()?;
    let platform = PlatformId::try_from(parsed.required("--platform")?.to_string())?;
    let out_dir = PathBuf::from(parsed.required("--out")?);
    let workspace = PathBuf::from(parsed.value("--workspace").unwrap_or("."));
    let target_dir = parsed
        .value("--target-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));
    let written = package(&workspace, &target_dir, &out_dir, platform.as_str())?;
    if written.is_empty() {
        writeln!(out, "no official module is built for {}", platform.as_str())?;
    }
    for file in &written {
        writeln!(out, "{}", file.display())?;
    }
    Ok(())
}

/// Stages every official module of `platform` into `out_dir` and writes
/// `SHA256SUMS.txt` for the files it wrote. Returns those files.
pub fn package(
    workspace: &Path,
    target_dir: &Path,
    out_dir: &Path,
    platform: &str,
) -> Result<Vec<PathBuf>, Error> {
    let target = Target::for_platform(platform)?;
    let version = version::workspace_version(workspace)?;
    let crates = discover(workspace)?;
    let release_dir = target_dir.join("release");
    let stage_root = out_dir.join(".stage");
    std::fs::create_dir_all(out_dir)?;

    let mut written = Vec::new();
    for id in modules_for(&workspace.join("manifest"), platform)? {
        let folder = crates.get(&id).ok_or_else(|| {
            Error::Index(format!(
                "no plugin.toml with id {id} under libs/, plugins/ or examples/"
            ))
        })?;
        let cargo: Cargo = read_toml(&folder.join("Cargo.toml"))?;
        let library_name = cargo
            .lib
            .and_then(|lib| lib.name)
            .unwrap_or_else(|| cargo.package.name.replace('-', "_"));
        let file_name = target.library(&library_name);
        let library = release_dir.join(&file_name);
        if !library.is_file() {
            return Err(Error::Io(format!(
                "{} is not built; run cargo build --release first",
                library.display()
            )));
        }

        let stage = stage_root.join(&id);
        let _ = std::fs::remove_dir_all(&stage);
        std::fs::create_dir_all(&stage)?;
        let manifest = std::fs::read_to_string(folder.join("plugin.toml"))?;
        let stamped = stamp_manifest(&manifest, &version, target.kind, &file_name);
        std::fs::write(stage.join("plugin.toml"), stamped)?;
        copy_readmes(folder, &stage)?;
        std::fs::copy(&library, stage.join(&file_name))?;

        let zip = out_dir.join(format!("{}-{platform}.zip", cargo.package.name));
        std::fs::write(&zip, package::pack(&stage)?)?;
        std::fs::remove_dir_all(&stage)?;
        written.push(zip);
    }

    let loader = release_dir.join(target.loader);
    if loader.is_file() {
        let copy = out_dir.join(target.loader);
        std::fs::copy(&loader, &copy)?;
        written.push(copy);
    }
    let _ = std::fs::remove_dir_all(&stage_root);
    write_sums(out_dir, &written)?;
    Ok(written)
}

/// The ids of the modules whose `[build]` lists `platform`, sorted.
fn modules_for(manifest: &Path, platform: &str) -> Result<Vec<String>, Error> {
    let mut ids = Vec::new();
    for folder in sorted_dirs(&manifest.join("plugins"))? {
        let entry: Option<Entry> = read_optional(&folder.join("entry.toml"))?;
        let Some(entry) = entry else { continue };
        let builds_here = entry
            .build
            .is_some_and(|build| build.platforms.iter().any(|p| p == platform));
        if builds_here {
            ids.push(entry.id);
        }
    }
    ids.sort();
    Ok(ids)
}

/// Maps each plugin id to its folder under `libs/`, `plugins/` or `examples/`.
fn discover(workspace: &Path) -> Result<BTreeMap<String, PathBuf>, Error> {
    let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();
    for group in ["libs", "plugins", "examples"] {
        for folder in sorted_dirs(&workspace.join(group))? {
            let manifest = folder.join("plugin.toml");
            if !manifest.is_file() {
                continue;
            }
            let plugin: PluginId = read_toml(&manifest)?;
            if let Some(other) = found.insert(plugin.id.clone(), folder.clone()) {
                return Err(Error::Index(format!(
                    "{} is the id of both {} and {}",
                    plugin.id,
                    other.display(),
                    folder.display()
                )));
            }
        }
    }
    Ok(found)
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Error> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    toml::from_str(&text).map_err(|error| Error::Index(format!("{}: {error}", path.display())))
}

/// The `README*.md` files of a crate, copied next to the plugin.
fn copy_readmes(folder: &Path, stage: &Path) -> Result<(), Error> {
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("README") && name.ends_with(".md") && entry.path().is_file() {
            std::fs::copy(entry.path(), stage.join(name))?;
        }
    }
    Ok(())
}

/// `plugin.toml` with `version`, `kind` and `entry` set for the target. Those
/// lines are replaced, or added above the first table; every other line is
/// kept as it is, comments included.
pub fn stamp_manifest(text: &str, version: &str, kind: &str, entry: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let table_start = lines
        .iter()
        .position(|line| line.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let mut added = Vec::new();
    for (key, value) in [("version", version), ("kind", kind), ("entry", entry)] {
        let line = format!("{key} = \"{value}\"");
        match lines[..table_start]
            .iter()
            .position(|existing| key_of(existing) == Some(key))
        {
            Some(index) => lines[index] = line,
            None => added.push(line),
        }
    }
    let mut at = table_start;
    while at > 0 && lines[at - 1].trim().is_empty() {
        at -= 1;
    }
    for (offset, line) in added.into_iter().enumerate() {
        lines.insert(at + offset, line);
    }
    let mut stamped = lines.join("\n");
    stamped.push('\n');
    stamped
}

/// The key of a top-level `key = value` line, or `None` for anything else.
fn key_of(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    let (key, _) = line.split_once('=')?;
    let key = key.trim();
    let plain = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    plain.then_some(key)
}

/// `SHA256SUMS.txt` in `sha256sum` format, for the files written by this run.
fn write_sums(out_dir: &Path, files: &[PathBuf]) -> Result<(), Error> {
    let mut named: Vec<(String, PathBuf)> = files
        .iter()
        .map(|file| {
            let name = file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            (name, file.clone())
        })
        .collect();
    named.sort();
    let mut text = String::new();
    for (name, file) in named {
        let digest = trust::sha256_hex(&std::fs::read(&file)?);
        text.push_str(&format!("{digest}  {name}\n"));
    }
    std::fs::write(out_dir.join("SHA256SUMS.txt"), text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    const MANIFEST: &str = "# keep me\nid = \"com.parsec-orbit.demo\"\nversion = \"0.0.1\"\nrole = \"lib\"\nkind = \"dll\"\nentry = \"old.dll\"\nrequires = [\n  \"com.example.base\",\n]\n\n[dependencies]\nversion = \"not top level\"\n";

    #[test]
    fn version_kind_and_entry_are_set_and_everything_else_is_kept() {
        let stamped = stamp_manifest(MANIFEST, "0.3.0", "dylib", "libdemo.dylib");
        assert_eq!(
            stamped,
            "# keep me\nid = \"com.parsec-orbit.demo\"\nversion = \"0.3.0\"\nrole = \"lib\"\nkind = \"dylib\"\nentry = \"libdemo.dylib\"\nrequires = [\n  \"com.example.base\",\n]\n\n[dependencies]\nversion = \"not top level\"\n"
        );
    }

    #[test]
    fn missing_keys_are_added_above_the_first_table() {
        let text = "id = \"com.example.p\"\n\n[dependencies]\nserde = \"1\"\n";
        let stamped = stamp_manifest(text, "1.0.0", "dll", "p.dll");
        assert_eq!(
            stamped,
            "id = \"com.example.p\"\nversion = \"1.0.0\"\nkind = \"dll\"\nentry = \"p.dll\"\n\n[dependencies]\nserde = \"1\"\n"
        );
    }

    #[test]
    fn a_comment_that_mentions_a_key_is_not_a_key() {
        assert_eq!(key_of("# version = \"x\""), None);
        assert_eq!(key_of("  kind = \"dll\""), Some("kind"));
        assert_eq!(key_of("\"quoted\" = 1"), None);
    }

    #[test]
    fn each_platform_has_its_kind_and_library_name() {
        let windows = Target::for_platform("windows-x86_64").unwrap();
        assert_eq!(
            (windows.kind, windows.library("demo_lib")),
            ("dll", "demo_lib.dll".to_string())
        );
        let mac = Target::for_platform("macos-arm64").unwrap();
        assert_eq!(
            (mac.kind, mac.library("demo_lib"), mac.loader),
            (
                "dylib",
                "libdemo_lib.dylib".to_string(),
                "libparsec_orbit_loader.dylib"
            )
        );
        assert!(matches!(
            Target::for_platform("linux-x86_64"),
            Err(Error::Usage(_))
        ));
    }

    /// A workspace with two crates, a registry entry for each and the
    /// `release` build outputs of the windows and macOS packages.
    fn workspace(dir: &TempDir) {
        dir.write(
            "Cargo.toml",
            "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.3.0\"\n",
        );
        dir.write("libs/demo/Cargo.toml", "[package]\nname = \"demo-lib\"\nversion.workspace = true\n\n[lib]\nname = \"demo_lib\"\n");
        dir.write("libs/demo/plugin.toml", "# demo\nid = \"com.parsec-orbit.demo\"\nname = \"Demo\"\nversion = \"0.0.1\"\nrole = \"lib\"\nkind = \"dll\"\nentry = \"old.dll\"\n");
        dir.write("libs/demo/README.md", "readme");
        dir.write("libs/demo/README-more.md", "more");
        dir.write("libs/demo/NOTES.txt", "not shipped");
        dir.write(
            "plugins/mac/Cargo.toml",
            "[package]\nname = \"parsec-orbit-plugin-mac\"\n",
        );
        dir.write(
            "plugins/mac/plugin.toml",
            "id = \"com.example.mac\"\nversion = \"0.0.1\"\nkind = \"dll\"\nentry = \"x.dll\"\n",
        );
        dir.write("manifest/plugins/com.parsec-orbit.demo/entry.toml", "id = \"com.parsec-orbit.demo\"\n\n[build]\nplatforms = [\"windows-x86_64\", \"macos-arm64\"]\n");
        dir.write(
            "manifest/plugins/com.example.mac/entry.toml",
            "id = \"com.example.mac\"\n\n[build]\nplatforms = [\"macos-arm64\"]\n",
        );
        dir.write(
            "manifest/plugins/com.example.none/entry.toml",
            "id = \"com.example.none\"\n",
        );
        dir.write("target/release/demo_lib.dll", b"MZ demo");
        dir.write("target/release/bcrypt.dll", b"MZ loader");
    }

    #[test]
    fn a_windows_package_is_zipped_with_its_loader_and_sums() {
        let dir = TempDir::new("package-windows");
        workspace(&dir);
        let out = dir.path().join("dist");
        let written = package(
            dir.path(),
            &dir.path().join("target"),
            &out,
            "windows-x86_64",
        )
        .unwrap();
        let names: Vec<String> = written
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["demo-lib-windows-x86_64.zip", "bcrypt.dll"]);

        let zip = std::fs::read(out.join("demo-lib-windows-x86_64.zip")).unwrap();
        let read = package::read(&zip).unwrap();
        assert_eq!(read.folder, "com.parsec-orbit.demo");
        assert_eq!(read.manifest.version, "0.3.0");
        assert_eq!(read.manifest.kind, "dll");
        assert_eq!(read.manifest.entry.as_deref(), Some("demo_lib.dll"));
        let paths: Vec<&str> = read.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            ["README-more.md", "README.md", "demo_lib.dll", "plugin.toml"]
        );

        let sums = std::fs::read_to_string(out.join("SHA256SUMS.txt")).unwrap();
        let expected = format!(
            "{}  bcrypt.dll\n{}  demo-lib-windows-x86_64.zip\n",
            trust::sha256_hex(b"MZ loader"),
            trust::sha256_hex(&zip)
        );
        assert_eq!(sums, expected);
        assert!(!out.join(".stage").exists());
    }

    #[test]
    fn a_macos_package_names_the_dylib_it_lacks() {
        let dir = TempDir::new("package-macos");
        workspace(&dir);
        let target = dir.path().join("target");
        let out = dir.path().join("dist");
        // Modules are packaged in id order: the macOS-only one comes first.
        let error = package(dir.path(), &target, &out, "macos-arm64").unwrap_err();
        assert!(
            matches!(&error, Error::Io(text) if text.contains("libparsec_orbit_plugin_mac.dylib")),
            "{error}"
        );

        dir.write("target/release/libparsec_orbit_plugin_mac.dylib", b"dylib");
        let error = package(dir.path(), &target, &out, "macos-arm64").unwrap_err();
        assert!(
            matches!(&error, Error::Io(text) if text.contains("libdemo_lib.dylib")),
            "{error}"
        );

        dir.write("target/release/libdemo_lib.dylib", b"dylib");
        let written = package(dir.path(), &target, &out, "macos-arm64").unwrap();
        assert_eq!(
            written.len(),
            2,
            "two packages; no loader is built in this test"
        );
        let zip = out.join("parsec-orbit-plugin-mac-macos-arm64.zip");
        let read = package::read(&std::fs::read(&zip).unwrap()).unwrap();
        assert_eq!(read.manifest.kind, "dylib");
        assert_eq!(
            read.manifest.entry.as_deref(),
            Some("libparsec_orbit_plugin_mac.dylib")
        );
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let dir = TempDir::new("package-duplicate");
        workspace(&dir);
        dir.write("plugins/copy/Cargo.toml", "[package]\nname = \"copy\"\n");
        dir.write(
            "plugins/copy/plugin.toml",
            "id = \"com.parsec-orbit.demo\"\n",
        );
        let error = package(
            dir.path(),
            &dir.path().join("target"),
            &dir.path().join("dist"),
            "windows-x86_64",
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::Index(ref text) if text.contains("com.parsec-orbit.demo")),
            "{error}"
        );
    }

    #[test]
    fn the_repository_has_a_crate_for_every_official_module() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let crates = discover(&root).unwrap();
        let ids = modules_for(&root.join("manifest"), "windows-x86_64").unwrap();
        assert!(!ids.is_empty());
        for id in &ids {
            assert!(crates.contains_key(id), "no crate for {id}");
        }
        assert!(
            modules_for(&root.join("manifest"), "macos-arm64")
                .unwrap()
                .len()
                == 3
        );
    }
}

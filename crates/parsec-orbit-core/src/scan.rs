//! Discovery and load decisions.
//!
//! The loader understands two layouts:
//!
//! ```text
//! <client dir>\plugins\<name>\plugin.toml   + entry (dll/js)
//! %APPDATA%\ParsecOrbit\plugins\<name>\plugin.toml
//! <client dir>\plugins\loose.dll            (no manifest, legacy layout)
//! ```
//!
//! `plugin.toml` selects the plugin kind. `dll` plugins are mapped through the
//! [`Platform`](crate::Platform) and called through the ABI; `js` plugins need
//! the JavaScript runtime that is not written yet, so they are reported and
//! skipped instead of being loaded.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use parsec_orbit_plugin_abi::ABI_VERSION;

use crate::activate::activate;
use crate::manifest::{describe, read_manifest};
use crate::{Env, Loaded, Manifest, KIND_DLL, KIND_JS, MANIFEST};

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Summary {
    pub loaded: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// Outcome of [`scan`].
#[derive(Default)]
pub struct Scan {
    pub summary: Summary,
    /// Every plugin that was mapped, in load order.
    pub loaded: Vec<Loaded>,
}

fn is_dll(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
}

fn first_dll(dir: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_dll(path))
        .collect();
    candidates.sort();
    candidates.into_iter().next()
}

/// Maps one DLL through the platform and activates it.
fn load_dll(
    env: Env,
    path: &Path,
    manifest: Option<&Manifest>,
    registry: &mut Vec<Loaded>,
) -> Result<String, String> {
    let module = env.platform.load(path)?;
    activate(env, path, manifest, module.handle, module.exports, registry)
}

/// Handles one plugin directory.
fn load_plugin_dir(env: Env, dir: &Path, scan: &mut Scan, seen: &mut HashSet<String>) {
    let log = env.logger;

    let manifest_path = dir.join(MANIFEST);
    if !manifest_path.is_file() {
        log.debug(&format!("{}: no {MANIFEST}, skipped", dir.display()));
        scan.summary.skipped += 1;
        return;
    }

    let manifest = match read_manifest(&manifest_path) {
        Ok(manifest) => manifest,
        Err(error) => {
            log.error(&format!("{}: {error}", manifest_path.display()));
            scan.summary.failed += 1;
            return;
        }
    };

    let label = describe(Some(&manifest), dir);

    log.debug(&format!(
        "{label}: name={:?} author={:?} license={:?} description={:?}",
        manifest.name, manifest.author, manifest.license, manifest.description
    ));

    if !manifest.enabled {
        log.info(&format!("{label}: disabled in the manifest, skipped"));
        scan.summary.skipped += 1;
        return;
    }
    if manifest.api > ABI_VERSION {
        log.warn(&format!(
            "{label}: manifest asks for API {} but the loader implements {ABI_VERSION}",
            manifest.api
        ));
        scan.summary.failed += 1;
        return;
    }
    if let Some(id) = &manifest.id {
        if !seen.insert(id.clone()) {
            log.debug(&format!("{label}: already loaded, skipped"));
            scan.summary.skipped += 1;
            return;
        }
    }

    let kind = manifest
        .kind
        .clone()
        .unwrap_or_else(|| KIND_DLL.to_string());
    match kind.as_str() {
        KIND_DLL => {
            let entry = manifest
                .entry
                .as_ref()
                .map(|entry| dir.join(entry))
                .or_else(|| first_dll(dir));
            let Some(entry) = entry else {
                log.warn(&format!("{label}: no DLL entry point found"));
                scan.summary.failed += 1;
                return;
            };
            match load_dll(env, &entry, Some(&manifest), &mut scan.loaded) {
                Ok(_) => scan.summary.loaded += 1,
                Err(error) => {
                    log.error(&format!("{label}: {error}"));
                    scan.summary.failed += 1;
                }
            }
        }
        KIND_JS => {
            // Reserved: the JavaScript runtime is a plugin of its own that is
            // not written yet, so the plugin stays on disk and is reported.
            log.info(&format!(
                "{label}: kind \"{KIND_JS}\" needs the JavaScript runtime, which is not implemented yet"
            ));
            scan.summary.skipped += 1;
        }
        other => {
            log.warn(&format!("{label}: unknown kind \"{other}\""));
            scan.summary.failed += 1;
        }
    }
}

/// Walks every plugin directory, in priority order, and loads what it finds.
pub fn scan(env: Env, plugin_dirs: &[PathBuf]) -> Scan {
    let log = env.logger;
    let mut result = Scan::default();
    let mut seen: HashSet<String> = HashSet::new();

    for dir in plugin_dirs {
        if !dir.is_dir() {
            log.debug(&format!("{}: not present", dir.display()));
            continue;
        }
        log.debug(&format!("scanning {}", dir.display()));

        let mut entries: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect(),
            Err(error) => {
                log.warn(&format!("{}: {error}", dir.display()));
                continue;
            }
        };
        entries.sort();

        for entry in entries {
            if entry.is_dir() {
                load_plugin_dir(env, &entry, &mut result, &mut seen);
                continue;
            }
            if !is_dll(&entry) {
                continue;
            }
            // A DLL dropped straight into a plugin directory is loaded without
            // metadata, which is what the first proof of concept did.
            match load_dll(env, &entry, None, &mut result.loaded) {
                Ok(_) => result.summary.loaded += 1,
                Err(error) => {
                    log.error(&format!("{}: {error}", entry.display()));
                    result.summary.failed += 1;
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use crate::{Exports, Logger, Module, Platform};
    use parsec_orbit_plugin_abi::{ParsecOrbitHost, ParsecOrbitPluginDesc};
    use std::cell::RefCell;

    /// Maps nothing: every library "fails to load", so the decisions around
    /// the platform call are what is under test.
    struct Unloadable;
    impl Platform for Unloadable {
        fn load(&self, path: &Path) -> Result<Module, String> {
            Err(format!("could not load {}", path.display()))
        }
    }

    /// "Loads" any library as one with the given exports, recording the paths.
    struct Fake {
        exports: Exports,
        seen: RefCell<Vec<PathBuf>>,
    }
    impl Platform for Fake {
        fn load(&self, path: &Path) -> Result<Module, String> {
            self.seen.borrow_mut().push(path.to_path_buf());
            Ok(Module {
                handle: 1,
                exports: self.exports,
            })
        }
    }

    #[derive(Default)]
    struct Recording(RefCell<Vec<String>>);
    impl Logger for Recording {
        fn log(&self, _level: u32, message: &str) {
            self.0.borrow_mut().push(message.to_string());
        }
    }
    impl Recording {
        fn mentions(&self, text: &str) -> bool {
            self.0.borrow().iter().any(|line| line.contains(text))
        }
    }

    fn host() -> ParsecOrbitHost {
        ParsecOrbitHost {
            abi_version: ABI_VERSION,
            struct_size: std::mem::size_of::<ParsecOrbitHost>() as u32,
            log: None,
            query_interface: None,
            loader_version: std::ptr::null(),
            reserved: [0; 8],
        }
    }

    fn run(platform: &dyn Platform, dirs: &[&Path]) -> (Scan, Recording) {
        let logger = Recording::default();
        let host = host();
        let dirs: Vec<PathBuf> = dirs.iter().map(|dir| dir.to_path_buf()).collect();
        let result = scan(
            Env {
                platform,
                logger: &logger,
                host: &host,
            },
            &dirs,
        );
        (result, logger)
    }

    fn counts(scan: &Scan) -> (usize, usize, usize) {
        (
            scan.summary.loaded,
            scan.summary.skipped,
            scan.summary.failed,
        )
    }

    #[test]
    fn first_dll_is_the_alphabetically_first_dll_of_any_case() {
        let dir = TempDir::new("first-dll");
        dir.write("zeta.dll", "");
        dir.write("Beta.DLL", "");
        dir.write("alpha.txt", "");
        dir.write("alpha.dll.bak", "");
        // Uppercase sorts before lowercase, which is what `sort` gives; the
        // point is that the choice is deterministic.
        assert_eq!(first_dll(dir.path()), Some(dir.path().join("Beta.DLL")));
    }

    #[test]
    fn first_dll_is_none_without_a_dll_or_a_directory() {
        let dir = TempDir::new("first-dll-none");
        dir.write("readme.txt", "");
        assert_eq!(first_dll(dir.path()), None);
        assert_eq!(first_dll(&dir.path().join("missing")), None);
    }

    #[test]
    fn scan_of_a_missing_directory_does_nothing() {
        let dir = TempDir::new("scan-missing");
        let (result, log) = run(&Unloadable, &[&dir.path().join("absent")]);
        assert_eq!(counts(&result), (0, 0, 0));
        assert!(log.mentions("not present"));
    }

    #[test]
    fn scan_of_an_empty_directory_does_nothing() {
        let dir = TempDir::new("scan-empty");
        let (result, _) = run(&Unloadable, &[dir.path()]);
        assert_eq!(counts(&result), (0, 0, 0));
    }

    #[test]
    fn scan_classifies_every_kind_of_plugin_directory() {
        let dir = TempDir::new("scan-mixed");
        // skipped: js plugin (needs the runtime), then a duplicate of it
        dir.write(
            "a-js/plugin.toml",
            "id = \"js.one\"\nkind = \"js\"\nentry = \"main.js\"\n",
        );
        dir.write("b-dup/plugin.toml", "id = \"js.one\"\nkind = \"js\"\n");
        // skipped: disabled in the manifest
        dir.write("c-disabled/plugin.toml", "id = \"off\"\nenabled = false\n");
        // skipped: a directory without a manifest
        dir.write("d-nomanifest/notes.txt", "hello");
        // failed: manifest that does not parse
        dir.write("e-badtoml/plugin.toml", "id = ");
        // failed: asks for an ABI the loader does not have
        dir.write("f-newapi/plugin.toml", "id = \"new\"\napi = 99\n");
        // failed: unknown kind
        dir.write("g-kind/plugin.toml", "id = \"kind\"\nkind = \"lua\"\n");
        // failed: dll plugin with nothing to load
        dir.write("h-nodll/plugin.toml", "id = \"nodll\"\n");
        // failed: the platform cannot map the entry
        dir.write(
            "i-garbage/plugin.toml",
            "id = \"garbage\"\nentry = \"x.dll\"\n",
        );
        dir.write("i-garbage/x.dll", "not a dll");
        // skipped: same id as a plugin that already claimed it, even though
        // that one failed to load
        dir.write(
            "j-garbage-again/plugin.toml",
            "id = \"garbage\"\nentry = \"x.dll\"\n",
        );
        dir.write("j-garbage-again/x.dll", "not a dll");
        // failed: a loose DLL that is not loadable
        dir.write("loose.dll", "not a dll");
        // ignored: files that are not DLLs
        dir.write("readme.txt", "ignore me");
        dir.write("LOOSE.TXT", "ignore me");

        let (result, _) = run(&Unloadable, &[dir.path()]);
        assert_eq!(result.summary.loaded, 0);
        assert_eq!(
            result.summary.skipped, 5,
            "js, duplicate js, disabled, no manifest, duplicate of a failed id"
        );
        assert_eq!(
            result.summary.failed, 6,
            "bad toml, new api, unknown kind, no dll, garbage dll, loose dll"
        );
    }

    #[test]
    fn scan_deduplicates_ids_across_directories() {
        let first = TempDir::new("scan-first");
        let second = TempDir::new("scan-second");
        first.write("p/plugin.toml", "id = \"same\"\nkind = \"js\"\n");
        second.write("p/plugin.toml", "id = \"same\"\nkind = \"js\"\n");
        second.write("q/plugin.toml", "id = \"other\"\nkind = \"js\"\n");

        let (result, _) = run(&Unloadable, &[first.path(), second.path()]);
        assert_eq!(result.summary.skipped, 3);
        assert_eq!(result.summary.failed, 0);
    }

    #[test]
    fn scan_picks_the_manifest_entry_over_other_dlls() {
        let dir = TempDir::new("scan-entry");
        dir.write(
            "p/plugin.toml",
            "id = \"entry\"\nentry = \"sub/real.dll\"\n",
        );
        dir.write("p/aaa.dll", "decoy");
        let platform = Fake {
            exports: Exports::default(),
            seen: RefCell::default(),
        };
        let (result, _) = run(&platform, &[dir.path()]);
        // The fake maps whatever it is asked for; the point is which path.
        assert_eq!(
            *platform.seen.borrow(),
            vec![dir.path().join("p").join("sub/real.dll")]
        );
        // No exports at all, so activation fails.
        assert_eq!(result.summary.failed, 1);
    }

    struct DescCell(ParsecOrbitPluginDesc);
    unsafe impl Sync for DescCell {}

    unsafe extern "system" fn query(_abi: u32) -> *const ParsecOrbitPluginDesc {
        static DESC: DescCell = DescCell(ParsecOrbitPluginDesc {
            abi_version: ABI_VERSION,
            struct_size: std::mem::size_of::<ParsecOrbitPluginDesc>() as u32,
            id: b"com.example.fake\0".as_ptr(),
            name: b"Fake\0".as_ptr(),
            version: b"1.0.0\0".as_ptr(),
            author: b"me\0".as_ptr(),
            description: b"d\0".as_ptr(),
            license: b"MIT\0".as_ptr(),
            capabilities: 0,
        });
        &DESC.0
    }

    unsafe extern "system" fn init(_host: *const ParsecOrbitHost) -> i32 {
        0
    }

    #[test]
    fn scan_loads_a_plugin_through_the_platform_and_records_it() {
        let dir = TempDir::new("scan-ok");
        dir.write(
            "p/plugin.toml",
            "id = \"com.example.fake\"\nentry = \"f.dll\"\n",
        );
        dir.write("p/f.dll", "");
        let platform = Fake {
            exports: Exports {
                query: Some(query),
                init: Some(init),
                ..Exports::default()
            },
            seen: RefCell::default(),
        };
        let (result, log) = run(&platform, &[dir.path()]);
        assert_eq!(counts(&result), (1, 0, 0));
        assert_eq!(result.loaded.len(), 1);
        assert_eq!(result.loaded[0].id, "com.example.fake");
        assert!(log.mentions("loaded com.example.fake 1.0.0 (Fake) ABI 1"));
    }
}

//! Discovery and load decisions.
//!
//! The loader understands two layouts:
//!
//! ```text
//! <client dir>\plugins\<name>\plugin.toml   + entry (dll/js/...)
//! %APPDATA%\ParsecOrbit\plugins\<name>\plugin.toml
//! <client dir>\plugins\loose.dll            (no manifest, legacy layout)
//! ```
//!
//! A scan has three steps (see `docs/spec/loading.md`):
//!
//! 1. **Discovery** reads manifests and drops what is disabled, too new or a
//!    duplicate.
//! 2. **Ordering** sorts the survivors by `requires` / `optional`
//!    ([`crate::plan`]).
//! 3. **Loading** walks that order. `dll` plugins are mapped through the
//!    [`Platform`](crate::Platform) and called through the ABI; every other
//!    kind goes to the plugin that registered `parsec-orbit.kind.<kind>`.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use parsec_orbit_plugin_abi::ABI_VERSION;

/// A manifest `entry` has to stay inside the plugin directory. Absolute paths
/// and `..` components would otherwise let a manifest map a file from anywhere
/// on the host.
fn manifest_entry_stays_inside(entry: &Path) -> bool {
    !entry.as_os_str().is_empty()
        && !entry.is_absolute()
        && entry
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

use crate::activate::{activate, Plugin};
use crate::host::data_dir_for;
use crate::kind::{self, Lookup};
use crate::manifest::{describe, read_manifest};
use crate::plan::{self, Node, Verdict};
use crate::{Env, Loaded, Manifest, KIND_DLL, MANIFEST};

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

/// A plugin that survived discovery.
struct Candidate {
    label: String,
    dir: PathBuf,
    /// `None` for a loose DLL.
    manifest: Option<Manifest>,
    /// Manifest id; the file name of the directory when the manifest has none;
    /// the path of the library for a loose DLL.
    id: String,
    kind: String,
    /// The library of a loose DLL.
    loose: Option<PathBuf>,
}

impl Candidate {
    fn node(&self) -> Node {
        match &self.manifest {
            Some(manifest) => Node {
                id: self.id.clone(),
                provides: manifest.provides.clone(),
                requires: manifest.requires.clone(),
                optional: manifest.optional.clone(),
            },
            None => Node {
                id: self.id.clone(),
                ..Node::default()
            },
        }
    }
}

/// Reads one plugin directory. `None` means the directory was dealt with
/// (and counted) already.
fn discover_dir(
    env: Env,
    dir: &Path,
    summary: &mut Summary,
    seen: &mut HashSet<String>,
) -> Option<Candidate> {
    let log = env.logger;

    let manifest_path = dir.join(MANIFEST);
    if !manifest_path.is_file() {
        log.debug(&format!("{}: no {MANIFEST}, skipped", dir.display()));
        summary.skipped += 1;
        return None;
    }

    let manifest = match read_manifest(&manifest_path) {
        Ok(manifest) => manifest,
        Err(error) => {
            // The toml parser formats errors over several lines; the loader log
            // is line-oriented and prefixes one line per decision.
            let error = error.lines().collect::<Vec<_>>().join(" ");
            log.error(&format!("{}: {error}", manifest_path.display()));
            summary.failed += 1;
            return None;
        }
    };

    let label = describe(Some(&manifest), dir);

    log.debug(&format!(
        "{label}: name={} author={} license={} description={}",
        manifest.name.as_deref().unwrap_or("-"),
        manifest.author.as_deref().unwrap_or("-"),
        manifest.license.as_deref().unwrap_or("-"),
        manifest.description.as_deref().unwrap_or("-")
    ));
    for warning in &manifest.warnings {
        log.warn(&format!("{label}: {warning}"));
    }

    if !manifest.enabled {
        log.info(&format!("{label}: disabled in the manifest, skipped"));
        summary.skipped += 1;
        return None;
    }
    if manifest.api > ABI_VERSION {
        log.warn(&format!(
            "{label}: manifest asks for API {} but the loader implements {ABI_VERSION}",
            manifest.api
        ));
        summary.failed += 1;
        return None;
    }

    let id = manifest.id.clone().unwrap_or_else(|| {
        dir.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string())
    });
    if !seen.insert(id.clone()) {
        log.debug(&format!("{label}: already loaded, skipped"));
        summary.skipped += 1;
        return None;
    }

    let kind = manifest
        .kind
        .clone()
        .unwrap_or_else(|| KIND_DLL.to_string());
    Some(Candidate {
        label,
        dir: dir.to_path_buf(),
        manifest: Some(manifest),
        id,
        kind,
        loose: None,
    })
}

/// Walks every plugin directory, in priority order.
fn discover(env: Env, plugin_dirs: &[PathBuf], summary: &mut Summary) -> Vec<Candidate> {
    let log = env.logger;
    let mut candidates = Vec::new();
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
                candidates.extend(discover_dir(env, &entry, summary, &mut seen));
                continue;
            }
            if !is_dll(&entry) {
                continue;
            }
            // A DLL dropped straight into a plugin directory is loaded without
            // metadata, which is what the first proof of concept did.
            candidates.push(Candidate {
                label: entry.display().to_string(),
                dir: entry.parent().unwrap_or(dir).to_path_buf(),
                manifest: None,
                id: entry.display().to_string(),
                kind: KIND_DLL.to_string(),
                loose: Some(entry),
            });
        }
    }
    candidates
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Pending,
    Loaded,
    Skipped,
    Failed,
}

/// Outcome of loading one plugin.
enum Outcome {
    Loaded,
    Skipped(String),
    Failed(String),
}

/// Maps one DLL through the platform and activates it.
fn load_dll(env: Env, candidate: &Candidate, registry: &mut Vec<Loaded>) -> Outcome {
    if let Some(manifest) = &candidate.manifest {
        if let Some(entry) = &manifest.entry {
            if !manifest_entry_stays_inside(Path::new(entry)) {
                return Outcome::Failed("manifest entry escapes the plugin directory".to_string());
            }
        }
    }
    let entry = match (&candidate.loose, &candidate.manifest) {
        (Some(path), _) => Some(path.clone()),
        (None, Some(manifest)) => manifest
            .entry
            .as_ref()
            .map(|entry| candidate.dir.join(entry))
            .or_else(|| first_dll(&candidate.dir)),
        (None, None) => None,
    };
    let Some(entry) = entry else {
        return Outcome::Failed("no DLL entry point found".to_string());
    };
    let module = match env.platform.load(&entry) {
        Ok(module) => module,
        Err(error) => return Outcome::Failed(error),
    };
    let plugin = Plugin {
        dir: &candidate.dir,
        entry: &entry,
        manifest: candidate.manifest.as_ref(),
    };
    match activate(env, plugin, module.handle, module.exports, registry) {
        Ok(_) => Outcome::Loaded,
        Err(error) => Outcome::Failed(error),
    }
}

/// Hands a plugin of another kind to the plugin that serves that kind.
fn load_with_handler(env: Env, candidate: &Candidate) -> Outcome {
    let kind = &candidate.kind;
    let handler = match kind::handler_for(kind) {
        Lookup::Found(handler) => handler,
        Lookup::Missing => {
            return Outcome::Skipped(format!(
                "no handler for kind \"{kind}\" (is the runtime plugin installed?)"
            ))
        }
        Lookup::Unusable(reason) => return Outcome::Failed(reason),
    };
    let Some(manifest) = &candidate.manifest else {
        return Outcome::Failed("kind handlers need a manifest".to_string());
    };
    let Some(entry) = &manifest.entry else {
        return Outcome::Failed(format!("kind \"{kind}\" needs an `entry` in the manifest"));
    };
    if !manifest_entry_stays_inside(Path::new(entry)) {
        return Outcome::Failed("manifest entry escapes the plugin directory".to_string());
    }
    let entry = candidate.dir.join(entry);
    let data_dir = env.data_root.map(|root| data_dir_for(root, &candidate.id));
    let request = kind::LoadRequest {
        id: &candidate.id,
        dir: &candidate.dir,
        entry: &entry,
        data_dir: data_dir.as_deref(),
        manifest: &manifest.source,
    };
    match kind::load(handler, &request) {
        Ok(()) => {
            env.logger.info(&format!(
                "loaded {} through the \"{kind}\" handler",
                candidate.id
            ));
            Outcome::Loaded
        }
        Err(error) => Outcome::Failed(error),
    }
}

/// Discovers, orders and loads every plugin below `plugin_dirs`.
pub fn scan(env: Env, plugin_dirs: &[PathBuf]) -> Scan {
    let log = env.logger;
    let mut result = Scan::default();

    let candidates = discover(env, plugin_dirs, &mut result.summary);
    let nodes: Vec<Node> = candidates.iter().map(Candidate::node).collect();
    let plan = plan::plan(&nodes);
    let mut states = vec![State::Pending; candidates.len()];

    for &index in &plan.order {
        let candidate = &candidates[index];
        let label = &candidate.label;

        let outcome = if let Some(verdict) = &plan.verdicts[index] {
            match verdict {
                Verdict::Skip(reason) => Outcome::Skipped(reason.clone()),
                Verdict::Fail(reason) => Outcome::Failed(reason.clone()),
            }
        } else if let Some(missing) = nodes[index].requires.iter().find(|name| {
            !plan
                .providers(name)
                .iter()
                .any(|&provider| states[provider] == State::Loaded)
        }) {
            Outcome::Skipped(format!("dependency {missing} did not load"))
        } else if candidate.kind == KIND_DLL {
            load_dll(env, candidate, &mut result.loaded)
        } else {
            load_with_handler(env, candidate)
        };

        match outcome {
            Outcome::Loaded => {
                states[index] = State::Loaded;
                result.summary.loaded += 1;
            }
            Outcome::Skipped(reason) => {
                log.info(&format!("{label}: skipped, {reason}"));
                states[index] = State::Skipped;
                result.summary.skipped += 1;
            }
            Outcome::Failed(reason) => {
                log.error(&format!("{label}: {reason}"));
                states[index] = State::Failed;
                result.summary.failed += 1;
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::register;
    use crate::testutil::TempDir;
    use crate::{Exports, Logger, Module, Platform};
    use parsec_orbit_plugin_abi::{
        ParsecOrbitHost, ParsecOrbitInterfaceHeader, ParsecOrbitKindHandlerV1,
        ParsecOrbitPluginDesc, ParsecOrbitPluginLoadInfo,
    };
    use std::cell::RefCell;
    use std::ffi::CStr;
    use std::sync::Mutex;

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

    fn run(platform: &dyn Platform, dirs: &[&Path]) -> (Scan, Recording) {
        let logger = Recording::default();
        let dirs: Vec<PathBuf> = dirs.iter().map(|dir| dir.to_path_buf()).collect();
        let result = scan(
            Env {
                platform,
                logger: &logger,
                loader_version: "0.0.0-test",
                data_root: None,
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
        // skipped: js plugin without a handler, then a duplicate of it
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
        // skipped: a kind nobody serves
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
            result.summary.skipped, 6,
            "js, duplicate js, disabled, no manifest, unserved kind, duplicate of a failed id"
        );
        assert_eq!(
            result.summary.failed, 5,
            "bad toml, new api, no dll, garbage dll, loose dll"
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
    fn a_manifest_without_an_id_is_known_by_its_directory_name() {
        let dir = TempDir::new("scan-nameless");
        dir.write("first/plugin.toml", "kind = \"js\"\n");
        dir.write("second/plugin.toml", "kind = \"js\"\n");
        let later = TempDir::new("scan-nameless-2");
        later.write("first/plugin.toml", "kind = \"js\"\n");
        // The same directory name in a later plugin directory is the same id.
        let (result, _) = run(&Unloadable, &[dir.path(), later.path()]);
        assert_eq!(result.summary.skipped, 3);
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

    #[test]
    fn scan_rejects_a_manifest_entry_that_escapes_the_plugin_directory() {
        let dir = TempDir::new("scan-entry-escape");
        dir.write(
            "p/plugin.toml",
            "id = \"escape\"\nentry = \"../evil.dll\"\n",
        );
        let platform = Fake {
            exports: Exports::default(),
            seen: RefCell::default(),
        };
        let (result, log) = run(&platform, &[dir.path()]);

        assert!(
            platform.seen.borrow().is_empty(),
            "the entry was never mapped"
        );
        assert_eq!(result.summary.failed, 1);
        assert!(log.mentions("manifest entry escapes the plugin directory"));
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

    /// A platform on which every library is a working plugin.
    fn working() -> Fake {
        Fake {
            exports: Exports {
                query: Some(query),
                init: Some(init),
                ..Exports::default()
            },
            seen: RefCell::default(),
        }
    }

    /// Directory names of the libraries the platform was asked to map.
    fn mapped(platform: &Fake) -> Vec<String> {
        platform
            .seen
            .borrow()
            .iter()
            .map(|path| {
                path.parent()
                    .and_then(Path::file_name)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    fn dll_plugin(dir: &TempDir, name: &str, manifest: &str) {
        dir.write(&format!("{name}/plugin.toml"), manifest);
        dir.write(&format!("{name}/{name}.dll"), "");
    }

    #[test]
    fn scan_loads_a_plugin_through_the_platform_and_records_it() {
        let dir = TempDir::new("scan-ok");
        dir.write(
            "p/plugin.toml",
            "id = \"com.example.fake\"\nentry = \"f.dll\"\n",
        );
        dir.write("p/f.dll", "");
        let platform = working();
        let (result, log) = run(&platform, &[dir.path()]);
        assert_eq!(counts(&result), (1, 0, 0));
        assert_eq!(result.loaded.len(), 1);
        assert_eq!(result.loaded[0].id, "com.example.fake");
        assert!(log.mentions("loaded com.example.fake 1.0.0 (Fake) ABI 1"));
    }

    // ---- ordering and dependencies ----

    #[test]
    fn requires_decides_the_load_order() {
        let dir = TempDir::new("scan-requires");
        dll_plugin(&dir, "a-app", "id = \"app\"\nrequires = [\"lib\"]\n");
        dll_plugin(&dir, "b-free", "id = \"free\"\n");
        dll_plugin(&dir, "c-lib", "id = \"lib\"\n");
        let platform = working();
        let (result, _) = run(&platform, &[dir.path()]);
        assert_eq!(counts(&result), (3, 0, 0));
        assert_eq!(mapped(&platform), ["b-free", "c-lib", "a-app"]);
        let ids: Vec<&str> = result
            .loaded
            .iter()
            .map(|plugin| plugin.id.as_str())
            .collect();
        assert_eq!(ids, ["free", "lib", "app"]);
    }

    #[test]
    fn optional_orders_but_does_not_require() {
        let dir = TempDir::new("scan-optional");
        dll_plugin(
            &dir,
            "a-app",
            "id = \"app\"\noptional = [\"lib\", \"gone\"]\n",
        );
        dll_plugin(&dir, "b-lib", "id = \"lib\"\n");
        let platform = working();
        let (result, _) = run(&platform, &[dir.path()]);
        assert_eq!(counts(&result), (2, 0, 0));
        assert_eq!(mapped(&platform), ["b-lib", "a-app"]);
    }

    #[test]
    fn a_failed_optional_dependency_does_not_stop_the_dependent() {
        let dir = TempDir::new("scan-optional-failed");
        dll_plugin(&dir, "a-app", "id = \"app\"\noptional = [\"lib\"]\n");
        dir.write("b-lib/plugin.toml", "id = \"lib\"\n");
        let platform = working();
        let (result, _) = run(&platform, &[dir.path()]);
        assert_eq!(
            counts(&result),
            (1, 0, 1),
            "lib has no dll, app loads anyway"
        );
    }

    #[test]
    fn provides_lets_an_alias_stand_in_for_an_id() {
        let dir = TempDir::new("scan-provides");
        dll_plugin(&dir, "a-app", "id = \"app\"\nrequires = [\"js-runtime\"]\n");
        dll_plugin(&dir, "b-boa", "id = \"boa\"\nprovides = [\"js-runtime\"]\n");
        let platform = working();
        let (result, _) = run(&platform, &[dir.path()]);
        assert_eq!(counts(&result), (2, 0, 0));
        assert_eq!(mapped(&platform), ["b-boa", "a-app"]);
    }

    #[test]
    fn a_missing_requirement_skips_the_plugin_and_says_why() {
        let dir = TempDir::new("scan-missing-req");
        dll_plugin(
            &dir,
            "a-app",
            "id = \"app\"\nrequires = [\"com.parsec-orbit.js-runtime\"]\n",
        );
        let platform = working();
        let (result, log) = run(&platform, &[dir.path()]);
        assert_eq!(counts(&result), (0, 1, 0));
        assert!(platform.seen.borrow().is_empty(), "no code of it ran");
        assert!(
            log.mentions("skipped, needs com.parsec-orbit.js-runtime, not installed"),
            "{:?}",
            log.0.borrow()
        );
    }

    #[test]
    fn dependents_of_a_skipped_or_failed_plugin_are_skipped() {
        let dir = TempDir::new("scan-chain");
        dll_plugin(&dir, "a-base", "id = \"base\"\nrequires = [\"absent\"]\n");
        dll_plugin(&dir, "b-mid", "id = \"mid\"\nrequires = [\"base\"]\n");
        dll_plugin(&dir, "c-top", "id = \"top\"\nrequires = [\"mid\"]\n");
        dir.write("d-broken/plugin.toml", "id = \"broken\"\n");
        dll_plugin(
            &dir,
            "e-needs-broken",
            "id = \"needs-broken\"\nrequires = [\"broken\"]\n",
        );
        let platform = working();
        let (result, log) = run(&platform, &[dir.path()]);
        assert_eq!(counts(&result), (0, 4, 1));
        assert!(platform.seen.borrow().is_empty());
        assert!(log.mentions("skipped, dependency base did not load"));
        assert!(log.mentions("skipped, dependency mid did not load"));
        assert!(log.mentions("skipped, dependency broken did not load"));
    }

    #[test]
    fn a_dependency_cycle_fails_its_members_and_prints_the_cycle() {
        let dir = TempDir::new("scan-cycle");
        dll_plugin(&dir, "a", "id = \"a\"\nrequires = [\"b\"]\n");
        dll_plugin(&dir, "b", "id = \"b\"\nrequires = [\"a\"]\n");
        dll_plugin(&dir, "c-waits", "id = \"waits\"\nrequires = [\"a\"]\n");
        dll_plugin(&dir, "d-free", "id = \"free\"\n");
        let platform = working();
        let (result, log) = run(&platform, &[dir.path()]);
        assert_eq!(counts(&result), (1, 1, 2));
        assert_eq!(mapped(&platform), ["d-free"]);
        assert!(
            log.mentions("dependency cycle: a -> b -> a"),
            "{:?}",
            log.0.borrow()
        );
        assert!(log.mentions("skipped, dependency a did not load"));
    }

    #[test]
    fn manifest_warnings_are_logged_with_the_plugin_label() {
        let dir = TempDir::new("scan-warnings");
        dll_plugin(&dir, "a", "id = \"a\"\nrequires = [\"b@^1\"]\n");
        dll_plugin(&dir, "b", "id = \"b\"\n");
        let platform = working();
        let (result, log) = run(&platform, &[dir.path()]);
        assert_eq!(
            counts(&result),
            (2, 0, 0),
            "the constraint falls back to the bare id"
        );
        assert!(log.mentions("version constraints are not supported"));
    }

    #[test]
    fn a_manifest_and_descriptor_that_disagree_are_reported() {
        let dir = TempDir::new("scan-mismatch");
        dir.write(
            "p/plugin.toml",
            "id = \"manifest.id\"\nname = \"Other\"\nversion = \"1.0.0\"\nentry = \"f.dll\"\n",
        );
        dir.write("p/f.dll", "");
        let platform = working();
        let (result, log) = run(&platform, &[dir.path()]);
        assert_eq!(counts(&result), (1, 0, 0));
        assert_eq!(
            result.loaded[0].id, "manifest.id",
            "the manifest id is the plugin's id"
        );
        assert!(log.mentions("disagree on id, name"), "{:?}", log.0.borrow());
    }

    // ---- kind handlers ----

    static HANDLED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    unsafe extern "system" fn handle(info: *const ParsecOrbitPluginLoadInfo) -> i32 {
        let info = unsafe { &*info };
        let id = unsafe { CStr::from_ptr(info.id as *const _) }
            .to_string_lossy()
            .into_owned();
        let entry = unsafe { CStr::from_ptr(info.entry as *const _) }
            .to_string_lossy()
            .into_owned();
        HANDLED.lock().unwrap().push(format!("{id} {entry}"));
        if id.contains("rejected") {
            5
        } else {
            0
        }
    }

    static HANDLER: ParsecOrbitKindHandlerV1 = ParsecOrbitKindHandlerV1 {
        header: ParsecOrbitInterfaceHeader::new(
            std::mem::size_of::<ParsecOrbitKindHandlerV1>(),
            1,
            0,
        ),
        load_plugin: Some(handle),
        unload_plugin: None,
    };

    fn handled(id: &str) -> Option<String> {
        HANDLED
            .lock()
            .unwrap()
            .iter()
            .find(|call| call.starts_with(&format!("{id} ")))
            .cloned()
    }

    #[test]
    fn a_plugin_of_another_kind_goes_to_the_registered_handler() {
        assert_eq!(
            register(
                "com.example.sc-handler",
                "parsec-orbit.kind.sc-lua",
                &HANDLER.header
            ),
            0
        );
        let dir = TempDir::new("scan-kind-ok");
        dir.write(
            "a/plugin.toml",
            "id = \"sc.lua-ok\"\nkind = \"sc-lua\"\nentry = \"main.lua\"\n",
        );
        dir.write(
            "b/plugin.toml",
            "id = \"sc.lua-rejected\"\nkind = \"sc-lua\"\nentry = \"main.lua\"\n",
        );
        dir.write(
            "c/plugin.toml",
            "id = \"sc.lua-noentry\"\nkind = \"sc-lua\"\n",
        );
        let (result, log) = run(&Unloadable, &[dir.path()]);
        assert_eq!(counts(&result), (1, 0, 2));
        let call = handled("sc.lua-ok").expect("the handler was called");
        assert!(call.ends_with("main.lua"), "{call}");
        assert!(log.mentions("loaded sc.lua-ok through the \"sc-lua\" handler"));
        assert!(log.mentions("kind handler returned 5"));
        assert!(log.mentions("needs an `entry`"));
    }

    #[test]
    fn a_kind_without_a_handler_is_skipped_with_a_hint() {
        let dir = TempDir::new("scan-kind-none");
        dir.write(
            "a/plugin.toml",
            "id = \"sc.nobody\"\nkind = \"sc-nobody\"\nentry = \"main.x\"\n",
        );
        let (result, log) = run(&Unloadable, &[dir.path()]);
        assert_eq!(counts(&result), (0, 1, 0));
        assert!(
            log.mentions("no handler for kind \"sc-nobody\" (is the runtime plugin installed?)"),
            "{:?}",
            log.0.borrow()
        );
    }

    #[test]
    fn the_handler_plugin_loads_first_when_it_is_required() {
        // The handler of this kind is registered by the plugin below while it
        // initialises; the plugin of that kind requires it.
        static REGISTERED: Mutex<bool> = Mutex::new(false);
        unsafe extern "system" fn register_handler(host: *const ParsecOrbitHost) -> i32 {
            let host = unsafe { &*host };
            let result = host.register(c"parsec-orbit.kind.sc-late", &HANDLER.header);
            *REGISTERED.lock().unwrap() = result == 0;
            result
        }
        let dir = TempDir::new("scan-kind-late");
        // Discovered first, loaded second.
        dir.write(
            "a-script/plugin.toml",
            "id = \"sc.script\"\nkind = \"sc-late\"\nentry = \"main.x\"\nrequires = [\"sc.runtime\"]\n",
        );
        dll_plugin(&dir, "b-runtime", "id = \"sc.runtime\"\n");
        let platform = Fake {
            exports: Exports {
                query: Some(query),
                init: Some(register_handler),
                ..Exports::default()
            },
            seen: RefCell::default(),
        };
        let (result, _) = run(&platform, &[dir.path()]);
        assert_eq!(counts(&result), (2, 0, 0));
        assert!(*REGISTERED.lock().unwrap());
        assert!(handled("sc.script").is_some());
    }
}

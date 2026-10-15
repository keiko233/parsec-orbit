//! The market's worker: the registry client, the page and the actions, on
//! one thread.
//!
//! [`Worker::handle`] takes a UI action, asks the [`Controller`] what it means,
//! runs the resulting commands (network and disk: the only place this plugin
//! blocks) and pushes the page state back to the UI. The UI callback only enqueues
//! the action; [`Worker::run`] drains the queue on the worker's own thread.
//!
//! Installs go to `<root>/plugins`, which the loader reads at start-up. An
//! update is staged in `<root>/pending`: the loader swaps it in before it maps
//! anything (`docs/spec/loading.md` section 0), so an update of a loaded
//! plugin takes effect at the next start.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use parsec_orbit_layout::Layout;
use parsec_orbit_registry::index::{Index, PlatformId};
use parsec_orbit_registry::install::{InstallOptions, InstalledPlugin, LoadedIndex, Market};
use parsec_orbit_registry::source::Fetch;
use parsec_orbit_registry::sources::Source;
use parsec_orbit_registry::{trust, LOADER_API};

use crate::controller::{Action, Command, Controller, Entry, Event, State, PAGE_ID};

/// Folder of the market's own files in the Orbit root: keys, cache.
const MARKET_DIR: &str = "market";

/// Puts the page on the UI. The Orbit UI in production; a recorder in tests.
pub trait PagePublisher {
    /// Registers the page, which must not be registered yet.
    fn register(&self, page: &str) -> Result<(), String>;
    /// Applies a patch of values and presentation to the registered page.
    fn update(&self, page_id: &str, patch: &str) -> Result<(), String>;
}

/// The plugins of one index, and the index itself, as last loaded.
struct Loaded {
    index: LoadedIndex,
}

pub struct Worker<P: PagePublisher> {
    root: PathBuf,
    plugins_dir: PathBuf,
    pending_dir: PathBuf,
    sources: Vec<Source>,
    platform: Option<PlatformId>,
    fetch: Box<dyn Fetch + Send>,
    publisher: P,
    controller: Controller,
    /// Whether the UI has the page; it is registered once, then patched.
    published: bool,
    loaded: Option<Loaded>,
}

impl<P: PagePublisher> Worker<P> {
    /// A worker for the Orbit root `root`. `platform` is `None` on a computer
    /// the market serves no packages for. The page is shown at once.
    pub fn new(
        root: PathBuf,
        sources: Vec<Source>,
        platform: Option<PlatformId>,
        fetch: Box<dyn Fetch + Send>,
        publisher: P,
    ) -> Worker<P> {
        let layout = Layout::for_root(root.clone());
        let plugins_dir = layout
            .managed_plugins_dir()
            .expect("a layout made from a root has a plugins folder");
        let pending_dir = layout
            .pending_dir()
            .expect("a layout made from a root has a pending folder");
        let controller = Controller::new(sources.iter().map(|s| s.name.clone()).collect());
        let mut worker = Worker {
            root,
            plugins_dir,
            pending_dir,
            sources,
            platform,
            fetch,
            publisher,
            controller,
            published: false,
            loaded: None,
        };
        worker.publish();
        worker
    }

    /// Serves actions until every sender of `actions` is gone. This is the body
    /// of the worker thread.
    pub fn run(mut self, actions: Receiver<Action>) {
        for action in actions {
            self.handle(&action);
        }
    }

    /// Applies one UI action: the controller decides, the commands run, and the
    /// page state is pushed after each step.
    pub fn handle(&mut self, action: &Action) {
        let commands = self.controller.handle(action);
        self.publish();
        for command in commands {
            self.execute(command);
            self.publish();
        }
    }

    fn execute(&mut self, command: Command) {
        match command {
            Command::Refresh(source) => self.refresh(&source),
            Command::Install(id) => {
                let result = self.install(&id);
                self.finish(result);
            }
            Command::Update(id) => {
                let result = self.stage_update(&id);
                self.finish(result);
            }
            Command::Remove(id) => {
                let result = self.remove(&id);
                self.finish(result);
            }
        }
    }

    /// Shows the outcome of a command and lists the plugins again, since an
    /// install or a removal changes their states.
    fn finish(&mut self, result: Result<String, String>) {
        let text = match result {
            Ok(text) | Err(text) => text,
        };
        self.controller.apply(Event::Status(text));
        self.relist();
    }

    fn refresh(&mut self, source_name: &str) {
        self.loaded = None;
        let Some(source) = self
            .sources
            .iter()
            .find(|source| source.name == source_name)
            .cloned()
        else {
            self.controller.apply(Event::Listing(Vec::new()));
            self.controller.apply(Event::Status(format!(
                "There is no source named {source_name:?}."
            )));
            return;
        };
        match self.load(&source) {
            Ok((index, entries)) => {
                let count = entries.len();
                self.loaded = Some(Loaded { index });
                self.controller.apply(Event::Listing(entries));
                self.controller.apply(Event::Status(format!(
                    "Listed {count} plugin(s) from {}.",
                    source.name
                )));
            }
            Err(error) => {
                self.controller.apply(Event::Listing(Vec::new()));
                self.controller.apply(Event::Status(format!(
                    "Could not load {}: {error}",
                    source.name
                )));
            }
        }
    }

    /// Fetches and checks the index, then lists it.
    fn load(&self, source: &Source) -> Result<(LoadedIndex, Vec<Entry>), String> {
        let market = self.market(self.plugins_dir.clone())?;
        let index = market
            .load_index(&source.location)
            .map_err(|error| error.to_string())?;
        let entries = self.listing(&market, &index)?;
        Ok((index, entries))
    }

    fn listing(&self, market: &Market<'_>, index: &LoadedIndex) -> Result<Vec<Entry>, String> {
        let installed = market.installed().map_err(|error| error.to_string())?;
        let updates: Vec<String> = market
            .updates(index)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|(plugin, _)| plugin.id)
            .collect();
        let platform = self.platform()?;
        Ok(build_entries(
            &index.index,
            &installed,
            &updates,
            LOADER_API,
            &platform,
        ))
    }

    fn relist(&mut self) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let result = self
            .market(self.plugins_dir.clone())
            .and_then(|market| self.listing(&market, &loaded.index));
        match result {
            Ok(entries) => self.controller.apply(Event::Listing(entries)),
            Err(error) => self.controller.apply(Event::Status(error)),
        }
    }

    fn install(&self, id: &str) -> Result<String, String> {
        let loaded = self.loaded_index()?;
        let market = self.market(self.plugins_dir.clone())?;
        let installed = market
            .install(
                loaded,
                id,
                None,
                InstallOptions {
                    force: false,
                    with_deps: true,
                },
            )
            .map_err(|error| error.to_string())?;
        let names: Vec<String> = installed
            .iter()
            .map(|done| format!("{} {}", done.id, done.version))
            .collect();
        Ok(format!(
            "Installed {}. Restart Parsec to load it.",
            names.join(", ")
        ))
    }

    fn stage_update(&self, id: &str) -> Result<String, String> {
        let loaded = self.loaded_index()?;
        let installed = self
            .market(self.plugins_dir.clone())?
            .installed()
            .map_err(|error| error.to_string())?;
        match installed.iter().find(|plugin| plugin.id == id) {
            Some(InstalledPlugin {
                origin: Some(_), ..
            }) => {}
            Some(_) => {
                return Err(format!(
                    "{id} was not installed through the market, so the market does not update it."
                ))
            }
            None => return Err(format!("{id} is not installed.")),
        }
        // Staged, not written over the installed folder: that one may be mapped
        // by the running client, and the loader swaps the staged copy in at the
        // next start.
        let staged = self
            .market(self.pending_dir.clone())?
            .install(loaded, id, None, InstallOptions::default())
            .map_err(|error| error.to_string())?;
        let version = staged
            .iter()
            .find(|done| done.id == id)
            .map(|done| done.version.clone())
            .unwrap_or_default();
        Ok(format!(
            "Staged {id} {version}. Restart Parsec to apply the update."
        ))
    }

    fn remove(&self, id: &str) -> Result<String, String> {
        self.market(self.plugins_dir.clone())?
            .remove(id, false)
            .map_err(|error| error.to_string())?;
        // A staged update would be promoted at the next start and bring the
        // plugin back, so it goes too.
        let staged = self.pending_dir.join(id);
        if let Err(error) = std::fs::remove_dir_all(&staged) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("cannot remove the staged {id}: {error}"));
            }
        }
        Ok(format!(
            "Removed {id}. A copy that is loaded stays loaded until Parsec restarts."
        ))
    }

    fn loaded_index(&self) -> Result<&LoadedIndex, String> {
        self.loaded
            .as_ref()
            .map(|loaded| &loaded.index)
            .ok_or_else(|| "Press Refresh to load the list first.".to_string())
    }

    fn platform(&self) -> Result<PlatformId, String> {
        self.platform
            .clone()
            .ok_or_else(|| "This computer has no plugin packages for the market.".to_string())
    }

    /// A registry client over the user's keys and cache. Built per command, so a
    /// key added to `trusted-keys.toml` counts at the next refresh.
    fn market(&self, plugins_dir: PathBuf) -> Result<Market<'_>, String> {
        let platform = self.platform()?;
        let market_dir = self.root.join(MARKET_DIR);
        let keys = trust::load_trusted_keys(&market_dir).map_err(|error| error.to_string())?;
        Ok(Market::new(&*self.fetch, plugins_dir, keys, platform)
            .with_cache(market_dir.join("cache")))
    }

    /// Pushes the page to the UI: registers it the first time, then applies the
    /// current state as a patch. A failed registration is shown in the status
    /// line; the next push tries again.
    fn publish(&mut self) {
        if !self.published {
            match self.publisher.register(&self.controller.page()) {
                Ok(()) => self.published = true,
                Err(error) => {
                    self.controller.apply(Event::Status(format!(
                        "The Orbit UI did not take the market page: {error}"
                    )));
                    return;
                }
            }
        }
        let _ = self.publisher.update(PAGE_ID, &self.controller.patch());
    }
}

/// The list the page shows: the plugins of `index` that this computer can
/// install, with their states. A plugin with no release for `platform` and
/// `api`, or with only withdrawn ones, is not listed. `updates` are the ids of
/// the installed plugins with a newer release ([`Market::updates`]).
pub fn build_entries(
    index: &Index,
    installed: &[InstalledPlugin],
    updates: &[String],
    api: u32,
    platform: &PlatformId,
) -> Vec<Entry> {
    index
        .plugins
        .iter()
        .filter_map(|plugin| {
            let release = plugin.select(None, api, platform).ok()?;
            let on_disk = installed.iter().find(|found| found.id == plugin.id);
            let state = match on_disk {
                None => State::Available,
                Some(_) if updates.contains(&plugin.id) => State::Update,
                Some(found) => {
                    let withdrawn = plugin.versions.iter().any(|offered| {
                        offered.yanked
                            && offered.platform == *platform
                            && offered.version == found.version
                    });
                    if withdrawn {
                        State::Yanked
                    } else {
                        State::Installed
                    }
                }
            };
            Some(Entry {
                id: plugin.id.clone(),
                name: if plugin.name.is_empty() {
                    plugin.id.clone()
                } else {
                    plugin.name.clone()
                },
                version: release.version.clone(),
                state,
                installed: on_disk.map(|found| found.version.clone()),
                library: plugin.role.as_deref() == Some("lib"),
                official: plugin.official,
                summary: plugin
                    .summary
                    .clone()
                    .unwrap_or_else(|| plugin.description.clone()),
                author: plugin.author.clone(),
                license: plugin.license.clone(),
                requires: release.requires.clone(),
                size: release.size,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use parsec_orbit_registry::index::{Plugin, Release, FORMAT};
    use parsec_orbit_registry::install::Origin;
    use parsec_orbit_registry::{package, Error as RegistryError};
    use serde_json::{json, Value};

    const LOCATION: &str = "https://market.example/index.json";

    // ---- doubles -------------------------------------------------------------

    /// Serves bytes by location. Cloned handles share the files, so a test can
    /// change the registry after the worker has been given it.
    #[derive(Clone, Default)]
    struct Files(Arc<Mutex<HashMap<String, Vec<u8>>>>);

    impl Files {
        fn put(&self, location: &str, bytes: Vec<u8>) {
            self.0.lock().unwrap().insert(location.to_string(), bytes);
        }

        fn remove(&self, location: &str) {
            self.0.lock().unwrap().remove(location);
        }

        fn get(&self, location: &str) -> Option<Vec<u8>> {
            self.0.lock().unwrap().get(location).cloned()
        }
    }

    impl Fetch for Files {
        fn fetch(&self, location: &str, limit: u64) -> Result<Vec<u8>, RegistryError> {
            let bytes = self
                .get(location)
                .ok_or_else(|| RegistryError::Io(format!("{location}: not found")))?;
            if bytes.len() as u64 > limit {
                return Err(RegistryError::Io(format!("{location} is too large")));
            }
            Ok(bytes)
        }
    }

    #[derive(Default)]
    struct Calls {
        pages: Vec<String>,
        patches: Vec<String>,
        /// Registrations to refuse before accepting one.
        refuse: usize,
    }

    /// Records what the UI was given.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Calls>>);

    impl Recorder {
        fn last_patch(&self) -> Value {
            let calls = self.0.lock().unwrap();
            serde_json::from_str(calls.patches.last().expect("a patch was pushed")).unwrap()
        }

        /// The values of the last patch, with the status line as `status`.
        fn last_state(&self) -> Value {
            let patch = self.last_patch();
            let mut state = patch["values"].clone();
            state["status"] = patch["props"]["status"]["message"].clone();
            state
        }

        fn status(&self) -> String {
            self.last_state()["status"].as_str().unwrap().to_string()
        }

        fn page_count(&self) -> usize {
            self.0.lock().unwrap().pages.len()
        }

        fn patch_count(&self) -> usize {
            self.0.lock().unwrap().patches.len()
        }

        fn refuse_next(&self, times: usize) {
            self.0.lock().unwrap().refuse = times;
        }

        /// The plugin choices of the last patch, as `(id, label)`.
        fn choices(&self) -> Vec<(String, String)> {
            self.last_patch()["props"]["plugin"]["options"]
                .as_array()
                .unwrap()
                .iter()
                .map(|option| {
                    (
                        option["value"].as_str().unwrap().to_string(),
                        option["label"].as_str().unwrap().to_string(),
                    )
                })
                .collect()
        }

        /// The plugin ids offered by the picker in the last patch.
        fn offered(&self) -> Vec<String> {
            self.choices()
                .into_iter()
                .map(|(id, _)| id)
                .filter(|id| !id.is_empty())
                .collect()
        }
    }

    impl PagePublisher for Recorder {
        fn register(&self, page: &str) -> Result<(), String> {
            let mut calls = self.0.lock().unwrap();
            if calls.refuse > 0 {
                calls.refuse -= 1;
                return Err("refused by the test".to_string());
            }
            calls.pages.push(page.to_string());
            Ok(())
        }

        fn update(&self, _page_id: &str, patch: &str) -> Result<(), String> {
            self.0.lock().unwrap().patches.push(patch.to_string());
            Ok(())
        }
    }

    // ---- an Orbit root, a signed registry and a worker -------------------------------

    /// A temporary Orbit root with the market's key list.
    struct Root {
        path: PathBuf,
        secret: String,
    }

    impl Root {
        fn new(label: &str) -> Root {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "parsec-orbit-market-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(path.join(MARKET_DIR)).unwrap();
            let (secret, public) = trust::generate_keypair().unwrap();
            std::fs::write(
                path.join(MARKET_DIR).join(trust::KEYS_TOML),
                format!("[[key]]\nalgorithm = \"ed25519\"\npublic = \"{public}\"\n"),
            )
            .unwrap();
            Root { path, secret }
        }

        fn plugins(&self, id: &str) -> PathBuf {
            self.path.join("plugins").join(id)
        }

        fn pending(&self, id: &str) -> PathBuf {
            self.path.join("pending").join(id)
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// A plugin of a test registry: its id, role, requirements and releases
    /// (version, yanked).
    struct Listed {
        id: &'static str,
        role: Option<&'static str>,
        requires: &'static [&'static str],
        releases: &'static [(&'static str, bool)],
    }

    fn plain(id: &'static str, releases: &'static [(&'static str, bool)]) -> Listed {
        Listed {
            id,
            role: None,
            requires: &[],
            releases,
        }
    }

    /// A plugin folder zipped the way the publisher zips it.
    fn package_bytes(root: &Root, id: &str, version: &str) -> Vec<u8> {
        let dir = root.path.join("packages").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            format!(
                "id = \"{id}\"\nname = \"Plugin {id}\"\nversion = \"{version}\"\nkind = \"dll\"\nentry = \"{id}.dll\"\napi = 1\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join(format!("{id}.dll")), b"MZ").unwrap();
        package::pack(&dir).unwrap()
    }

    /// Writes a registry index of `listed`, its packages and a signature by the
    /// root's key, all as `files` serves them.
    fn publish(root: &Root, files: &Files, serial: u64, listed: &[Listed]) {
        let mut plugins = Vec::new();
        for item in listed {
            let mut versions = Vec::new();
            for (version, yanked) in item.releases {
                let zip = package_bytes(root, item.id, version);
                let url = format!("{}-{}.zip", item.id, version);
                files.put(&format!("https://market.example/{url}"), zip.clone());
                versions.push(Release {
                    version: version.to_string(),
                    api: 1,
                    platform: PlatformId::windows_x86_64(),
                    url,
                    sha256: trust::sha256_hex(&zip),
                    size: zip.len() as u64,
                    requires: item.requires.iter().map(|id| id.to_string()).collect(),
                    published: None,
                    yanked: *yanked,
                    mirrors: Vec::new(),
                });
            }
            plugins.push(Plugin {
                id: item.id.to_string(),
                name: format!("Plugin {}", item.id),
                description: "A test plugin".to_string(),
                author: "Test".to_string(),
                license: "MIT".to_string(),
                role: item.role.map(str::to_string),
                summary: Some(format!("Summary of {}", item.id)),
                versions,
                ..Default::default()
            });
        }
        let index = Index {
            format: FORMAT,
            name: "Test".to_string(),
            serial: Some(serial),
            generated: None,
            plugins,
        };
        let bytes = index.to_json().into_bytes();
        files.put(LOCATION, bytes.clone());
        files.put(
            &format!("{LOCATION}.sig"),
            trust::sign(&bytes, &trust::signing_key(&root.secret).unwrap()).into_bytes(),
        );
    }

    fn worker(root: &Root, files: &Files, recorder: &Recorder) -> Worker<Recorder> {
        Worker::new(
            root.path.clone(),
            vec![Source {
                name: "Test".to_string(),
                location: LOCATION.to_string(),
            }],
            Some(PlatformId::windows_x86_64()),
            Box::new(files.clone()),
            recorder.clone(),
        )
    }

    fn act(key: &str, kind: &str, value: Value) -> Action {
        Action {
            key: key.to_string(),
            kind: kind.to_string(),
            value,
        }
    }

    fn click(worker: &mut Worker<Recorder>, key: &str) {
        worker.handle(&act(key, "click", Value::Null));
    }

    fn choose(worker: &mut Worker<Recorder>, id: &str) {
        worker.handle(&act("plugin", "change", json!(id)));
    }

    fn refresh(worker: &mut Worker<Recorder>) {
        click(worker, "refresh");
    }

    /// The first registry: `alpha` and `app` (which needs the library `beta`),
    /// with `gamma` withdrawn everywhere and `delta` with a withdrawn newer release.
    fn first_registry(root: &Root, files: &Files) {
        publish(
            root,
            files,
            1,
            &[
                plain("com.example.alpha", &[("1.0.0", false)]),
                Listed {
                    id: "com.example.beta",
                    role: Some("lib"),
                    requires: &[],
                    releases: &[("1.0.0", false)],
                },
                Listed {
                    id: "com.example.app",
                    role: None,
                    requires: &["com.example.beta"],
                    releases: &[("1.0.0", false)],
                },
                plain("com.example.gamma", &[("2.0.0", true)]),
                plain("com.example.delta", &[("1.0.0", false), ("2.0.0", true)]),
            ],
        );
    }

    // ---- the list ---------------------------------------------------------------

    #[test]
    fn refresh_lists_the_signed_plugins_without_libraries_or_withdrawn_ones() {
        let root = Root::new("refresh");
        let files = Files::default();
        first_registry(&root, &files);
        let recorder = Recorder::default();
        let mut worker = worker(&root, &files, &recorder);
        refresh(&mut worker);

        assert_eq!(
            recorder.offered(),
            ["com.example.alpha", "com.example.app", "com.example.delta"]
        );
        assert_eq!(recorder.status(), "Listed 4 plugin(s) from Test.");
        let delta = recorder
            .choices()
            .into_iter()
            .find(|(id, _)| id == "com.example.delta")
            .unwrap();
        assert_eq!(delta.1, "Plugin com.example.delta v1.0.0 (available)");
    }

    #[test]
    fn a_plugin_is_listed_only_when_it_has_a_release_for_this_computer() {
        let mut macos_only = Plugin {
            id: "com.example.mac".into(),
            ..Default::default()
        };
        macos_only.versions.push(Release {
            version: "1.0.0".into(),
            api: 1,
            platform: PlatformId::macos_arm64(),
            url: "m.zip".into(),
            sha256: "0".repeat(64),
            size: 1,
            requires: Vec::new(),
            published: None,
            yanked: false,
            mirrors: Vec::new(),
        });
        let index = Index {
            format: FORMAT,
            name: String::new(),
            serial: None,
            generated: None,
            plugins: vec![macos_only],
        };
        let windows = PlatformId::windows_x86_64();
        assert!(build_entries(&index, &[], &[], LOADER_API, &windows).is_empty());
        assert_eq!(
            build_entries(&index, &[], &[], LOADER_API, &PlatformId::macos_arm64()).len(),
            1
        );
    }

    #[test]
    fn installed_plugins_are_shown_as_installed_updates_or_withdrawn() {
        let origin = |id: &str, version: &str| {
            Some(Origin {
                id: id.to_string(),
                version: version.to_string(),
                sha256: String::new(),
                index: LOCATION.to_string(),
                installed_unix: 0,
            })
        };
        let plugin = |id: &str, versions: &[(&str, bool)]| {
            let mut plugin = Plugin {
                id: id.to_string(),
                ..Default::default()
            };
            for (version, yanked) in versions {
                plugin.versions.push(Release {
                    version: version.to_string(),
                    api: 1,
                    platform: PlatformId::windows_x86_64(),
                    url: format!("{id}.zip"),
                    sha256: "0".repeat(64),
                    size: 10,
                    requires: Vec::new(),
                    published: None,
                    yanked: *yanked,
                    mirrors: Vec::new(),
                });
            }
            plugin
        };
        let index = Index {
            format: FORMAT,
            name: String::new(),
            serial: None,
            generated: None,
            plugins: vec![
                plugin("a.new", &[("1.0.0", false)]),
                plugin("b.same", &[("1.0.0", false)]),
                plugin("c.old", &[("1.0.0", false), ("1.1.0", false)]),
                plugin("d.withdrawn", &[("1.0.0", true), ("1.1.0", false)]),
            ],
        };
        let installed = |id: &str, version: &str| InstalledPlugin {
            id: id.to_string(),
            version: version.to_string(),
            dir: PathBuf::from(id),
            origin: origin(id, version),
        };
        let on_disk = [
            installed("b.same", "1.0.0"),
            installed("c.old", "1.0.0"),
            installed("d.withdrawn", "1.0.0"),
        ];
        let windows = PlatformId::windows_x86_64();
        let entries = build_entries(
            &index,
            &on_disk,
            &["c.old".to_string()],
            LOADER_API,
            &windows,
        );
        let state = |id: &str| entries.iter().find(|e| e.id == id).unwrap().state;
        assert_eq!(state("a.new"), State::Available);
        assert_eq!(state("b.same"), State::Installed);
        assert_eq!(state("c.old"), State::Update);
        assert_eq!(state("d.withdrawn"), State::Yanked);

        // A hand-installed plugin is installed, never offered an update.
        let by_hand = [InstalledPlugin {
            origin: None,
            ..installed("c.old", "1.0.0")
        }];
        let entries = build_entries(&index, &by_hand, &[], LOADER_API, &windows);
        let c = entries.iter().find(|e| e.id == "c.old").unwrap();
        assert_eq!(c.state, State::Installed);
        assert_eq!(c.installed.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn a_tampered_index_is_refused_and_reported() {
        let root = Root::new("tampered");
        let files = Files::default();
        first_registry(&root, &files);
        let text = String::from_utf8(files.get(LOCATION).unwrap()).unwrap();
        files.put(
            LOCATION,
            text.replace("com.example.alpha", "com.example.alphx")
                .into_bytes(),
        );
        let recorder = Recorder::default();
        let mut worker = worker(&root, &files, &recorder);
        refresh(&mut worker);
        assert!(
            recorder.status().starts_with("Could not load Test:"),
            "{}",
            recorder.status()
        );
        assert!(recorder.status().contains("no trusted key signed"));
        assert!(recorder.offered().is_empty());
    }

    #[test]
    fn an_unsigned_index_is_refused_and_reported() {
        let root = Root::new("unsigned");
        let files = Files::default();
        first_registry(&root, &files);
        files.remove(&format!("{LOCATION}.sig"));
        let recorder = Recorder::default();
        let mut worker = worker(&root, &files, &recorder);
        refresh(&mut worker);
        assert!(
            recorder.status().contains("without a signature"),
            "{}",
            recorder.status()
        );
        assert!(recorder.offered().is_empty());
    }

    // ---- actions ----------------------------------------------------------------

    #[test]
    fn install_puts_the_plugin_and_its_library_below_plugins() {
        let root = Root::new("install");
        let files = Files::default();
        first_registry(&root, &files);
        let recorder = Recorder::default();
        let mut worker = worker(&root, &files, &recorder);
        refresh(&mut worker);
        choose(&mut worker, "com.example.app");
        click(&mut worker, "install");

        assert!(root.plugins("com.example.app").join("plugin.toml").exists());
        assert!(root.plugins("com.example.app").join("market.json").exists());
        assert!(root
            .plugins("com.example.beta")
            .join("market.json")
            .exists());
        assert!(
            recorder.status().starts_with("Installed"),
            "{}",
            recorder.status()
        );
        assert!(recorder.status().contains("Restart Parsec"));
        // The list now shows the plugin as installed.
        assert_eq!(
            recorder.choices()[1].1,
            "Plugin com.example.app v1.0.0 (installed)"
        );
    }

    #[test]
    fn an_update_is_staged_below_pending_and_the_installed_copy_stays() {
        let root = Root::new("update");
        let files = Files::default();
        first_registry(&root, &files);
        let recorder = Recorder::default();
        let mut worker = worker(&root, &files, &recorder);
        refresh(&mut worker);
        choose(&mut worker, "com.example.alpha");
        click(&mut worker, "install");

        publish(
            &root,
            &files,
            2,
            &[plain(
                "com.example.alpha",
                &[("1.1.0", false), ("1.0.0", false)],
            )],
        );
        refresh(&mut worker);
        choose(&mut worker, "com.example.alpha");
        assert!(recorder.last_state()["details"]
            .as_str()
            .unwrap()
            .contains("Trust:"));
        assert!(recorder.choices()[0].1.ends_with("(update)"));

        click(&mut worker, "update");
        assert!(
            recorder
                .status()
                .starts_with("Staged com.example.alpha 1.1.0"),
            "{}",
            recorder.status()
        );
        let staged =
            std::fs::read_to_string(root.pending("com.example.alpha").join("plugin.toml")).unwrap();
        assert!(staged.contains("version = \"1.1.0\""), "{staged}");
        assert!(root
            .pending("com.example.alpha")
            .join("market.json")
            .exists());
        let installed =
            std::fs::read_to_string(root.plugins("com.example.alpha").join("plugin.toml")).unwrap();
        assert!(installed.contains("version = \"1.0.0\""), "{installed}");
    }

    #[test]
    fn remove_deletes_a_plugin_the_market_installed() {
        let root = Root::new("remove");
        let files = Files::default();
        first_registry(&root, &files);
        let recorder = Recorder::default();
        let mut worker = worker(&root, &files, &recorder);
        refresh(&mut worker);
        choose(&mut worker, "com.example.alpha");
        click(&mut worker, "install");
        assert!(root.plugins("com.example.alpha").exists());

        click(&mut worker, "remove");
        assert!(!root.plugins("com.example.alpha").exists());
        assert!(recorder.status().starts_with("Removed com.example.alpha"));
    }

    #[test]
    fn remove_refuses_a_plugin_the_market_did_not_install() {
        let root = Root::new("hand");
        let files = Files::default();
        first_registry(&root, &files);
        std::fs::create_dir_all(root.plugins("com.example.alpha")).unwrap();
        std::fs::write(
            root.plugins("com.example.alpha").join("plugin.toml"),
            "id = \"com.example.alpha\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        let recorder = Recorder::default();
        let mut worker = worker(&root, &files, &recorder);
        refresh(&mut worker);
        choose(&mut worker, "com.example.alpha");
        click(&mut worker, "remove");

        assert!(root
            .plugins("com.example.alpha")
            .join("plugin.toml")
            .exists());
        assert!(
            recorder
                .status()
                .contains("was not installed through the market"),
            "{}",
            recorder.status()
        );
    }

    #[test]
    fn a_hand_installed_plugin_is_never_staged_as_an_update() {
        let root = Root::new("hand-update");
        let files = Files::default();
        first_registry(&root, &files);
        std::fs::create_dir_all(root.plugins("com.example.alpha")).unwrap();
        std::fs::write(
            root.plugins("com.example.alpha").join("plugin.toml"),
            "id = \"com.example.alpha\"\nversion = \"0.9.0\"\n",
        )
        .unwrap();
        let recorder = Recorder::default();
        let mut worker = worker(&root, &files, &recorder);
        refresh(&mut worker);
        choose(&mut worker, "com.example.alpha");
        // The list says installed, not update: nothing is offered.
        click(&mut worker, "update");
        assert!(!root.pending("com.example.alpha").exists());
        assert_eq!(recorder.status(), "Plugin com.example.alpha has no update.");
    }

    #[test]
    fn removing_a_plugin_also_drops_its_staged_update() {
        let root = Root::new("staged-remove");
        let files = Files::default();
        first_registry(&root, &files);
        let recorder = Recorder::default();
        let mut worker = worker(&root, &files, &recorder);
        refresh(&mut worker);
        choose(&mut worker, "com.example.alpha");
        click(&mut worker, "install");
        publish(
            &root,
            &files,
            2,
            &[plain(
                "com.example.alpha",
                &[("1.1.0", false), ("1.0.0", false)],
            )],
        );
        refresh(&mut worker);
        choose(&mut worker, "com.example.alpha");
        click(&mut worker, "update");
        assert!(root.pending("com.example.alpha").exists());

        click(&mut worker, "remove");
        assert!(!root.pending("com.example.alpha").exists());
    }

    // ---- the page ----------------------------------------------------------------

    #[test]
    fn the_page_is_registered_once_and_every_change_is_a_patch() {
        let root = Root::new("register");
        let files = Files::default();
        first_registry(&root, &files);
        let recorder = Recorder::default();
        let mut worker = worker(&root, &files, &recorder);
        assert_eq!(recorder.page_count(), 1, "shown at start-up");
        assert_eq!(recorder.patch_count(), 1);

        // A new list changes the choices, not the registration.
        refresh(&mut worker);
        assert_eq!(recorder.page_count(), 1);
        assert!(recorder.patch_count() > 1);
        assert_eq!(recorder.offered().len(), 3);

        // Selection and status are patches too.
        let patches = recorder.patch_count();
        choose(&mut worker, "com.example.alpha");
        assert_eq!(recorder.page_count(), 1);
        assert!(recorder.patch_count() > patches);
    }

    #[test]
    fn a_refused_registration_is_reported_and_tried_again() {
        let root = Root::new("refused");
        let files = Files::default();
        first_registry(&root, &files);
        let recorder = Recorder::default();
        recorder.refuse_next(1);
        let mut worker = worker(&root, &files, &recorder);
        assert_eq!(recorder.page_count(), 0);

        // The next push registers the page and shows the report.
        choose(&mut worker, "com.example.alpha");
        assert_eq!(recorder.page_count(), 1);
        assert!(recorder.status().contains("did not take the market page"));
    }

    #[test]
    fn the_worker_thread_serves_the_actions_it_is_sent() {
        let root = Root::new("thread");
        let files = Files::default();
        first_registry(&root, &files);
        let recorder = Recorder::default();
        let worker = worker(&root, &files, &recorder);
        let (sender, actions) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("com.parsec-orbit.market:market".to_string())
            .spawn(move || worker.run(actions))
            .unwrap();
        sender.send(act("refresh", "click", Value::Null)).unwrap();
        drop(sender);
        thread.join().unwrap();
        assert_eq!(recorder.status(), "Listed 4 plugin(s) from Test.");
    }

    #[test]
    fn a_source_that_is_not_listed_is_ignored_or_reported() {
        let root = Root::new("source");
        let files = Files::default();
        let recorder = Recorder::default();
        let mut worker = worker(&root, &files, &recorder);
        worker.handle(&act("source", "change", json!("Nowhere")));
        assert_eq!(
            recorder.last_state()["source"],
            "Test",
            "an unknown source is ignored"
        );

        worker.execute(Command::Refresh("Nowhere".to_string()));
        worker.publish();
        assert_eq!(recorder.status(), "There is no source named \"Nowhere\".");
    }
}

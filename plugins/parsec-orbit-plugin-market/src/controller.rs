//! The Market page: what it shows, and what a user's action asks for.
//!
//! [`Controller`] holds the state of the page. A UI action
//! ([`Action`], the `{"page","key","type","value"}` event of
//! `docs/spec/ui-service.md`) becomes zero or more [`Command`]s for the worker,
//! and the worker's results come back as [`Event`]s. Nothing here does I/O or
//! starts a thread, so every rule is a plain test.
//!
//! Two outputs go to the UI. [`Controller::page`] is the page for
//! `register_page`; it depends only on the configured sources and is registered
//! once. [`Controller::patch`] is the patch for `update_page`: the values, the
//! plugin choices, the status line and which buttons apply to the selected
//! plugin. The keys are in `docs/spec/registry.md` ("In-client market plugin").

use serde_json::{json, Value};

/// The page this plugin registers.
pub const PAGE_ID: &str = "com.parsec-orbit.market";

/// The trust line in the details. The market only loads indexes that a trusted
/// key signed, so every listed plugin is trusted in this sense.
pub const TRUST: &str = "signed by a trusted key";

/// The value of the placeholder option shown when a dropdown has nothing to offer.
const NOTHING: &str = "";

/// One UI event: the JSON `type` is kept as `kind`.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub key: String,
    pub kind: String,
    pub value: Value,
}

impl Action {
    /// Parses a delivered event. `None` for text that is not JSON, for an event of
    /// another page, or for one without a key and a type.
    pub fn parse(text: &str) -> Option<Action> {
        let event: Value = serde_json::from_str(text).ok()?;
        if event.get("page")?.as_str()? != PAGE_ID {
            return None;
        }
        Some(Action {
            key: event.get("key")?.as_str()?.to_string(),
            kind: event.get("type")?.as_str()?.to_string(),
            value: event.get("value").cloned().unwrap_or(Value::Null),
        })
    }
}

/// Work for the worker. Each carries what the worker needs; nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Load the index of the named source.
    Refresh(String),
    /// Install the plugin (and the libraries it requires) into the plugins folder.
    Install(String),
    /// Stage the newest release of an installed plugin for the next start.
    Update(String),
    /// Remove a plugin the market installed.
    Remove(String),
}

/// A result of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// What happened, in a sentence the user can read.
    Status(String),
    /// The plugins of the source that was loaded (replaces the list).
    Listing(Vec<Entry>),
}

/// Where a plugin stands relative to this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not installed; can be installed.
    Available,
    /// Installed and the newest release for this computer.
    Installed,
    /// Installed; a newer release can be staged.
    Update,
    /// Installed, and the installed version was withdrawn by its publisher.
    Yanked,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Available => "available",
            State::Installed => "installed",
            State::Update => "update",
            State::Yanked => "yanked",
        }
    }
}

/// A plugin as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// The version the market offers: the newest that can be installed here.
    pub version: String,
    pub state: State,
    /// The version on disk, when the plugin is installed.
    pub installed: Option<String>,
    /// `role = "lib"`: hidden unless the user shows libraries.
    pub library: bool,
    pub official: bool,
    pub summary: String,
    pub author: String,
    pub license: String,
    pub requires: Vec<String>,
    /// Bytes of the offered package.
    pub size: u64,
}

/// What the list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Installed,
    Updates,
    Official,
}

impl Filter {
    fn from_key(key: &str) -> Option<Filter> {
        match key {
            "all" => Some(Filter::All),
            "installed" => Some(Filter::Installed),
            "updates" => Some(Filter::Updates),
            "official" => Some(Filter::Official),
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Installed => "installed",
            Filter::Updates => "updates",
            Filter::Official => "official",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Installed => "Installed",
            Filter::Updates => "Updates",
            Filter::Official => "Official",
        }
    }

    fn matches(self, entry: &Entry) -> bool {
        match self {
            Filter::All => true,
            Filter::Installed => entry.installed.is_some(),
            Filter::Updates => entry.state == State::Update,
            Filter::Official => entry.official,
        }
    }
}

/// The state of the Market page.
#[derive(Debug, Clone)]
pub struct Controller {
    /// Names of the configured sources, in file order.
    sources: Vec<String>,
    source: Option<String>,
    filter: Filter,
    show_libs: bool,
    entries: Vec<Entry>,
    selected: Option<String>,
    status: String,
}

impl Controller {
    /// A controller for the named sources; the first one is selected.
    pub fn new(source_names: Vec<String>) -> Controller {
        let status = if source_names.is_empty() {
            "No sources are listed in market/sources.toml."
        } else {
            "Press Refresh to load the list of plugins."
        };
        Controller {
            source: source_names.first().cloned(),
            sources: source_names,
            filter: Filter::All,
            show_libs: false,
            entries: Vec::new(),
            selected: None,
            status: status.to_string(),
        }
    }

    /// Applies a user action and returns the commands it asks for.
    pub fn handle(&mut self, action: &Action) -> Vec<Command> {
        let value = &action.value;
        match (action.key.as_str(), action.kind.as_str()) {
            ("source", "change") => {
                let Some(name) = value
                    .as_str()
                    .filter(|name| self.sources.iter().any(|source| source == name))
                else {
                    return Vec::new();
                };
                self.source = Some(name.to_string());
                self.entries.clear();
                self.selected = None;
                self.status = format!("Loading {name}...");
                vec![Command::Refresh(name.to_string())]
            }
            ("filter", "change") => {
                if let Some(filter) = value.as_str().and_then(Filter::from_key) {
                    self.filter = filter;
                    self.reconcile();
                }
                Vec::new()
            }
            ("show_libs", "change") => {
                if let Some(show) = value.as_bool() {
                    self.show_libs = show;
                    self.reconcile();
                }
                Vec::new()
            }
            ("plugin", "change") => {
                if let Some(id) = value.as_str() {
                    if self.visible().iter().any(|entry| entry.id == id) {
                        self.selected = Some(id.to_string());
                    }
                }
                Vec::new()
            }
            ("install", "click") => self.install(),
            ("update", "click") => self.update(),
            ("remove", "click") => self.remove(),
            ("refresh", "click") => match self.source.clone() {
                Some(name) => {
                    self.status = format!("Loading {name}...");
                    vec![Command::Refresh(name)]
                }
                None => {
                    self.status = "No source is listed in market/sources.toml.".to_string();
                    Vec::new()
                }
            },
            _ => Vec::new(),
        }
    }

    /// Applies the result of a command.
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Status(text) => self.status = text,
            Event::Listing(entries) => {
                self.entries = entries;
                self.reconcile();
            }
        }
    }

    /// The page for `register_page`. Everything that changes goes through
    /// [`Controller::patch`] (the status line included, which the first patch
    /// fills in), so the page is the same for the life of the plugin.
    pub fn page(&self) -> String {
        let sources = if self.sources.is_empty() {
            vec![json!({"label": "No source configured", "value": NOTHING})]
        } else {
            self.sources
                .iter()
                .map(|name| json!({"label": name, "value": name}))
                .collect()
        };
        let filters: Vec<Value> = [
            Filter::All,
            Filter::Installed,
            Filter::Updates,
            Filter::Official,
        ]
        .iter()
        .map(|filter| json!({"label": filter.label(), "value": filter.key()}))
        .collect();
        json!({
            "schema": 2,
            "id": PAGE_ID,
            "title": "Market",
            "icon": "download",
            "order": 100,
            "description": "Browse, install, update and remove plugins from the signed registry.",
            "blocks": [
                {"type": "banner", "key": "status", "severity": "info", "message": ""},
                {"type": "group", "title": "Source", "settings": [
                    {
                        "key": "source",
                        "label": "Source",
                        "description": "The registry the list comes from (market/sources.toml).",
                        "control": {
                            "type": "select",
                            "default": self.sources.first().cloned().unwrap_or_default(),
                            "options": sources,
                        },
                    },
                    {
                        "key": "filter",
                        "label": "Show",
                        "control": {"type": "select", "default": Filter::All.key(), "options": filters},
                    },
                    {
                        "key": "show_libs",
                        "label": "Show libraries",
                        "description": "Libraries are installed as dependencies of the plugins that need them.",
                        "control": {"type": "toggle", "default": false},
                    },
                ]},
                {"type": "group", "title": "Plugins", "settings": [
                    {
                        "key": "plugin",
                        "label": "Plugin",
                        "control": {
                            "type": "select",
                            "default": NOTHING,
                            "options": [{"label": "No plugins listed", "value": NOTHING}],
                        },
                    },
                    {
                        "key": "details",
                        "label": "Details",
                        "control": {"type": "value", "default": "Select a plugin to see its details."},
                    },
                ]},
                {"type": "actions", "buttons": [
                    {"key": "install", "text": "Install", "icon": "download", "style": "accent"},
                    {"key": "update", "text": "Update"},
                    {"key": "remove", "text": "Remove", "icon": "delete", "style": "destructive"},
                    {"key": "refresh", "text": "Refresh", "icon": "refresh"},
                ]},
            ],
        })
        .to_string()
    }

    /// The patch for `update_page`: values, the plugin choices, the status line,
    /// and which buttons the selected plugin allows.
    pub fn patch(&self) -> String {
        let plugins = {
            let visible = self.visible();
            if visible.is_empty() {
                vec![json!({"label": "No plugins listed", "value": NOTHING})]
            } else {
                visible
                    .iter()
                    .map(|entry| {
                        json!({
                            "label": format!("{} v{} ({})", entry.name, entry.version, entry.state.label()),
                            "value": entry.id,
                        })
                    })
                    .collect()
            }
        };
        let selected = self.selected_entry();
        json!({
            "values": {
                "source": self.source.clone().unwrap_or_else(|| NOTHING.to_string()),
                "filter": self.filter.key(),
                "show_libs": self.show_libs,
                "plugin": self.selected.clone().unwrap_or_else(|| NOTHING.to_string()),
                "details": self.details(),
            },
            "props": {
                "plugin": {"options": plugins},
                "status": {"message": self.status, "visible": !self.status.is_empty()},
                "install": {"enabled": selected.is_some_and(|entry| entry.state == State::Available)},
                "update": {"enabled": selected.is_some_and(|entry| entry.state == State::Update)},
                "remove": {"enabled": selected.is_some_and(|entry| entry.installed.is_some())},
                "refresh": {"enabled": self.source.is_some()},
            },
        })
        .to_string()
    }

    /// The details of the selected plugin, one fact per line.
    pub fn details(&self) -> String {
        let Some(entry) = self.selected_entry() else {
            return "Select a plugin to see its details.".to_string();
        };
        let text = |value: &str| {
            if value.is_empty() {
                "-".to_string()
            } else {
                value.to_string()
            }
        };
        let requires = if entry.requires.is_empty() {
            "-".to_string()
        } else {
            entry.requires.join(", ")
        };
        let withdrawn = if entry.state == State::Yanked {
            "the installed version was withdrawn"
        } else {
            "no"
        };
        format!(
            "Summary: {}\nAuthor: {}\nLicense: {}\nRequires: {}\nSize: {}\nSource: {}\nTrust: {}\nYanked: {}",
            text(&entry.summary),
            text(&entry.author),
            text(&entry.license),
            requires,
            size_text(entry.size),
            self.source.as_deref().unwrap_or("-"),
            TRUST,
            withdrawn,
        )
    }

    fn visible(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|entry| self.show_libs || !entry.library)
            .filter(|entry| self.filter.matches(entry))
            .collect()
    }

    fn selected_entry(&self) -> Option<&Entry> {
        let id = self.selected.as_ref()?;
        self.entries.iter().find(|entry| &entry.id == id)
    }

    /// Keeps the selection on a plugin the list still shows.
    fn reconcile(&mut self) {
        let still_shown = self
            .selected
            .as_ref()
            .is_some_and(|id| self.visible().iter().any(|entry| &entry.id == id));
        if !still_shown {
            self.selected = self.visible().first().map(|entry| entry.id.clone());
        }
    }

    fn install(&mut self) -> Vec<Command> {
        let Some(entry) = self.selected_entry().cloned() else {
            self.status = "Choose a plugin first.".to_string();
            return Vec::new();
        };
        if entry.state != State::Available {
            self.status = match entry.state {
                State::Update => format!(
                    "{} is installed; use Update for the newer version.",
                    entry.name
                ),
                _ => format!("{} is already installed.", entry.name),
            };
            return Vec::new();
        }
        self.status = format!("Installing {} {}...", entry.name, entry.version);
        vec![Command::Install(entry.id)]
    }

    fn update(&mut self) -> Vec<Command> {
        let Some(entry) = self.selected_entry().cloned() else {
            self.status = "Choose a plugin first.".to_string();
            return Vec::new();
        };
        if entry.state != State::Update {
            self.status = format!("{} has no update.", entry.name);
            return Vec::new();
        }
        self.status = format!(
            "Staging {} {} for the next start...",
            entry.name, entry.version
        );
        vec![Command::Update(entry.id)]
    }

    fn remove(&mut self) -> Vec<Command> {
        let Some(entry) = self.selected_entry().cloned() else {
            self.status = "Choose a plugin first.".to_string();
            return Vec::new();
        };
        if entry.installed.is_none() {
            self.status = format!("{} is not installed.", entry.name);
            return Vec::new();
        }
        self.status = format!("Removing {}...", entry.name);
        vec![Command::Remove(entry.id)]
    }
}

/// `bytes` for people: KiB or MiB, one decimal.
fn size_text(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    let bytes = bytes as f64;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else {
        format!("{:.1} KiB", bytes / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, state: State) -> Entry {
        let installed = match state {
            State::Available => None,
            _ => Some("1.0.0".to_string()),
        };
        Entry {
            id: id.to_string(),
            name: format!("Plugin {id}"),
            version: "1.1.0".to_string(),
            state,
            installed,
            library: false,
            official: false,
            summary: "Does a thing".to_string(),
            author: "Someone".to_string(),
            license: "MIT".to_string(),
            requires: Vec::new(),
            size: 2048,
        }
    }

    fn act(key: &str, kind: &str, value: Value) -> Action {
        Action {
            key: key.to_string(),
            kind: kind.to_string(),
            value,
        }
    }

    fn controller(entries: Vec<Entry>) -> Controller {
        let mut controller = Controller::new(vec!["Main".to_string(), "Mirror".to_string()]);
        controller.apply(Event::Listing(entries));
        controller
    }

    fn select(controller: &mut Controller, id: &str) {
        controller.handle(&act("plugin", "change", json!(id)));
    }

    fn page_json(controller: &Controller) -> Value {
        serde_json::from_str(&controller.page()).expect("the page is JSON")
    }

    fn patch_json(controller: &Controller) -> Value {
        serde_json::from_str(&controller.patch()).expect("the patch is JSON")
    }

    fn plugin_options(controller: &Controller) -> Vec<(String, String)> {
        patch_json(controller)["props"]["plugin"]["options"]
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

    /// The values the page shows, with the status line, as one object.
    fn state_json(controller: &Controller) -> Value {
        let patch = patch_json(controller);
        let mut state = patch["values"].clone();
        state["status"] = patch["props"]["status"]["message"].clone();
        state
    }

    fn enabled(controller: &Controller, button: &str) -> bool {
        patch_json(controller)["props"][button]["enabled"]
            .as_bool()
            .unwrap()
    }

    /// The page as the service would validate and register it.
    fn registered(controller: &Controller) -> parsec_orbit_ui_protocol::PageState {
        parsec_orbit_ui_protocol::PageState::new(
            parsec_orbit_ui_protocol::UiPage::parse(&controller.page(), false)
                .unwrap_or_else(|error| panic!("the page breaks the schema: {error}")),
        )
    }

    fn apply_patch(page: &mut parsec_orbit_ui_protocol::PageState, controller: &Controller) {
        let patch = serde_json::from_str(&controller.patch()).unwrap();
        page.apply_patch(patch)
            .unwrap_or_else(|error| panic!("the service refuses the patch: {error}"));
    }

    // ---- the page ------------------------------------------------------------------

    /// The service validates the page the market registers; this is its rulebook,
    /// shared through the protocol crate.
    #[test]
    fn the_page_satisfies_the_service_schema_with_the_keys_of_the_design() {
        let page = registered(&controller(vec![entry("a", State::Available)]));
        assert_eq!(page.page.id, PAGE_ID);
        let keys: Vec<_> = page
            .page
            .elements()
            .iter()
            .map(|element| match element {
                parsec_orbit_ui_protocol::schema::Element::Banner(b) => b.key.clone(),
                parsec_orbit_ui_protocol::schema::Element::Setting(s) => s.key.clone(),
                parsec_orbit_ui_protocol::schema::Element::Button(b) => b.key.clone(),
            })
            .collect();
        assert_eq!(
            keys,
            [
                "status",
                "source",
                "filter",
                "show_libs",
                "plugin",
                "details",
                "install",
                "update",
                "remove",
                "refresh"
            ]
        );
        assert_eq!(page.values["source"], "Main");
        assert_eq!(page.values["filter"], "all");
    }

    #[test]
    fn every_patch_is_accepted_by_the_registered_page() {
        let mut library = entry("lib", State::Yanked);
        library.library = true;
        let mut controller = controller(vec![
            entry("new", State::Available),
            entry("upd", State::Update),
            library,
        ]);
        let mut page = registered(&controller);
        apply_patch(&mut page, &controller);
        for action in [
            act("plugin", "change", json!("upd")),
            act("show_libs", "change", json!(true)),
            act("filter", "change", json!("installed")),
            act("install", "click", Value::Null),
            act("source", "change", json!("Mirror")),
        ] {
            controller.handle(&action);
            apply_patch(&mut page, &controller);
        }
        controller.apply(Event::Listing(Vec::new()));
        apply_patch(&mut page, &controller);
        assert_eq!(page.values["plugin"], NOTHING);
        assert_eq!(page.props["plugin"].options.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn libraries_are_hidden_unless_shown() {
        let mut library = entry("lib", State::Available);
        library.library = true;
        let mut controller = controller(vec![entry("a", State::Available), library]);
        assert_eq!(plugin_options(&controller).len(), 1);

        controller.handle(&act("show_libs", "change", json!(true)));
        let options = plugin_options(&controller);
        assert_eq!(options.len(), 2);
        assert!(options.iter().any(|(id, _)| id == "lib"));

        controller.handle(&act("show_libs", "change", json!(false)));
        assert_eq!(plugin_options(&controller).len(), 1);
    }

    #[test]
    fn plugin_options_read_name_version_and_state() {
        let controller = controller(vec![
            entry("a", State::Available),
            entry("b", State::Update),
        ]);
        assert_eq!(
            plugin_options(&controller),
            [
                ("a".to_string(), "Plugin a v1.1.0 (available)".to_string()),
                ("b".to_string(), "Plugin b v1.1.0 (update)".to_string()),
            ]
        );
    }

    #[test]
    fn an_empty_list_offers_a_placeholder() {
        let controller = controller(Vec::new());
        assert_eq!(
            plugin_options(&controller),
            [(NOTHING.to_string(), "No plugins listed".to_string())]
        );
    }

    #[test]
    fn status_and_details_never_change_the_registered_page() {
        // The page is registered once; selection, status and the list are patches.
        let mut controller = controller(vec![entry("a", State::Available)]);
        let before = controller.page();
        controller.apply(Event::Status("Loading Main...".to_string()));
        controller.handle(&act("plugin", "change", json!("a")));
        controller.handle(&act("install", "click", Value::Null));
        assert_eq!(controller.page(), before);
    }

    #[test]
    fn the_page_fixes_the_source_names_and_the_default() {
        let page = page_json(&controller(Vec::new()));
        let sources = &page["blocks"][1]["settings"][0]["control"];
        assert_eq!(sources["options"][0]["label"], "Main");
        assert_eq!(sources["options"][1]["label"], "Mirror");
        assert_eq!(sources["default"], "Main");

        let page = page_json(&Controller::new(Vec::new()));
        assert_eq!(
            page["blocks"][1]["settings"][0]["control"]["options"][0]["label"],
            "No source configured"
        );
    }

    #[test]
    fn the_buttons_follow_the_selected_plugin() {
        let mut controller = controller(vec![
            entry("new", State::Available),
            entry("upd", State::Update),
            entry("ins", State::Installed),
        ]);
        let enabled_set = |controller: &Controller| -> [bool; 4] {
            ["install", "update", "remove", "refresh"].map(|key| enabled(controller, key))
        };
        select(&mut controller, "new");
        assert_eq!(enabled_set(&controller), [true, false, false, true]);
        select(&mut controller, "upd");
        assert_eq!(enabled_set(&controller), [false, true, true, true]);
        select(&mut controller, "ins");
        assert_eq!(enabled_set(&controller), [false, false, true, true]);

        let none = Controller::new(Vec::new());
        assert_eq!(enabled_set(&none), [false, false, false, false]);
    }

    // ---- actions -----------------------------------------------------------------

    #[test]
    fn an_action_is_read_from_the_ui_event() {
        let action = Action::parse(
            r#"{"page":"com.parsec-orbit.market","key":"filter","type":"change","value":"updates"}"#,
        )
        .unwrap();
        assert_eq!(action, act("filter", "change", json!("updates")));

        let click = Action::parse(
            r#"{"page":"com.parsec-orbit.market","key":"install","type":"click","value":null}"#,
        )
        .unwrap();
        assert_eq!(click.value, Value::Null);

        assert!(Action::parse(
            r#"{"page":"com.parsec-orbit.native","key":"filter","type":"change","value":"all"}"#
        )
        .is_none());
        assert!(Action::parse("not json").is_none());
        assert!(Action::parse(r#"{"page":"com.parsec-orbit.market","key":"filter"}"#).is_none());
    }

    #[test]
    fn choosing_a_source_loads_it_and_clears_the_list() {
        let mut controller = controller(vec![entry("a", State::Available)]);
        let commands = controller.handle(&act("source", "change", json!("Mirror")));
        assert_eq!(commands, [Command::Refresh("Mirror".to_string())]);
        assert!(plugin_options(&controller)[0].0 == NOTHING);
        assert_eq!(state_json(&controller)["source"], "Mirror");
    }

    #[test]
    fn an_unknown_source_is_ignored() {
        let mut controller = controller(vec![entry("a", State::Available)]);
        assert!(controller
            .handle(&act("source", "change", json!("Nowhere")))
            .is_empty());
        assert_eq!(state_json(&controller)["source"], "Main");
        assert_eq!(plugin_options(&controller).len(), 1);
    }

    #[test]
    fn refresh_loads_the_selected_source() {
        let mut controller = controller(Vec::new());
        assert_eq!(
            controller.handle(&act("refresh", "click", Value::Null)),
            [Command::Refresh("Main".to_string())]
        );

        let mut none = Controller::new(Vec::new());
        assert!(none
            .handle(&act("refresh", "click", Value::Null))
            .is_empty());
        assert!(state_json(&none)["status"]
            .as_str()
            .unwrap()
            .contains("sources.toml"));
    }

    #[test]
    fn filters_show_what_their_name_says() {
        let mut library = entry("lib", State::Installed);
        library.library = true;
        let mut official = entry("off", State::Available);
        official.official = true;
        let mut controller = controller(vec![
            entry("new", State::Available),
            entry("ins", State::Installed),
            entry("upd", State::Update),
            official,
            library,
        ]);
        controller.handle(&act("show_libs", "change", json!(true)));
        let shown = |controller: &mut Controller, filter: &str| -> Vec<String> {
            controller.handle(&act("filter", "change", json!(filter)));
            plugin_options(controller)
                .into_iter()
                .map(|(id, _)| id)
                .filter(|id| !id.is_empty())
                .collect()
        };
        assert_eq!(shown(&mut controller, "all").len(), 5);
        assert_eq!(shown(&mut controller, "installed"), ["ins", "upd", "lib"]);
        assert_eq!(shown(&mut controller, "updates"), ["upd"]);
        assert_eq!(shown(&mut controller, "official"), ["off"]);
        assert!(controller
            .handle(&act("filter", "change", json!("sideways")))
            .is_empty());
        assert_eq!(state_json(&controller)["filter"], "official");
    }

    #[test]
    fn the_selection_follows_the_filter() {
        let mut controller = controller(vec![
            entry("new", State::Available),
            entry("upd", State::Update),
        ]);
        select(&mut controller, "new");
        controller.handle(&act("filter", "change", json!("updates")));
        assert_eq!(state_json(&controller)["plugin"], "upd");

        // An id the list does not show is not selected.
        select(&mut controller, "new");
        assert_eq!(state_json(&controller)["plugin"], "upd");
    }

    #[test]
    fn install_is_offered_only_for_plugins_not_yet_installed() {
        let mut controller = controller(vec![
            entry("new", State::Available),
            entry("ins", State::Installed),
        ]);
        select(&mut controller, "new");
        assert_eq!(
            controller.handle(&act("install", "click", Value::Null)),
            [Command::Install("new".to_string())]
        );
        assert!(state_json(&controller)["status"]
            .as_str()
            .unwrap()
            .starts_with("Installing"));

        select(&mut controller, "ins");
        assert!(controller
            .handle(&act("install", "click", Value::Null))
            .is_empty());
        assert_eq!(
            state_json(&controller)["status"],
            "Plugin ins is already installed."
        );
    }

    #[test]
    fn install_without_a_choice_says_so() {
        let mut controller = controller(Vec::new());
        assert!(controller
            .handle(&act("install", "click", Value::Null))
            .is_empty());
        assert_eq!(state_json(&controller)["status"], "Choose a plugin first.");
    }

    #[test]
    fn update_is_offered_only_for_a_newer_release() {
        let mut controller = controller(vec![
            entry("upd", State::Update),
            entry("ins", State::Installed),
        ]);
        select(&mut controller, "upd");
        assert_eq!(
            controller.handle(&act("update", "click", Value::Null)),
            [Command::Update("upd".to_string())]
        );
        select(&mut controller, "ins");
        assert!(controller
            .handle(&act("update", "click", Value::Null))
            .is_empty());
        assert_eq!(
            state_json(&controller)["status"],
            "Plugin ins has no update."
        );
    }

    #[test]
    fn remove_needs_an_installed_plugin() {
        let mut controller = controller(vec![
            entry("new", State::Available),
            entry("ins", State::Installed),
        ]);
        select(&mut controller, "new");
        assert!(controller
            .handle(&act("remove", "click", Value::Null))
            .is_empty());
        select(&mut controller, "ins");
        assert_eq!(
            controller.handle(&act("remove", "click", Value::Null)),
            [Command::Remove("ins".to_string())]
        );
    }

    #[test]
    fn a_withdrawn_installed_version_is_shown_as_yanked() {
        let mut controller = controller(vec![entry("y", State::Yanked)]);
        select(&mut controller, "y");
        let details = controller.details();
        assert!(
            details.contains("Yanked: the installed version was withdrawn"),
            "{details}"
        );
        assert!(plugin_options(&controller)[0].1.ends_with("(yanked)"));
    }

    // ---- results and state -----------------------------------------------------------

    #[test]
    fn the_details_describe_the_selected_plugin() {
        let mut plugin = entry("a", State::Available);
        plugin.requires = vec!["com.parsec-orbit.ui".to_string(), "x.y".to_string()];
        plugin.size = 3 * 1024 * 1024 / 2;
        assert_eq!(
            controller(Vec::new()).details(),
            "Select a plugin to see its details."
        );
        let mut controller = controller(vec![plugin]);
        select(&mut controller, "a");
        assert_eq!(
            controller.details(),
            "Summary: Does a thing\nAuthor: Someone\nLicense: MIT\nRequires: com.parsec-orbit.ui, x.y\nSize: 1.5 MiB\nSource: Main\nTrust: signed by a trusted key\nYanked: no"
        );
    }

    #[test]
    fn a_small_package_is_shown_in_kibibytes() {
        assert_eq!(size_text(2048), "2.0 KiB");
        assert_eq!(size_text(0), "0.0 KiB");
    }

    #[test]
    fn a_new_listing_keeps_the_selection_when_it_still_has_the_plugin() {
        let mut controller = controller(vec![
            entry("a", State::Available),
            entry("b", State::Available),
        ]);
        select(&mut controller, "b");
        controller.apply(Event::Listing(vec![
            entry("c", State::Available),
            entry("b", State::Installed),
        ]));
        assert_eq!(state_json(&controller)["plugin"], "b");

        controller.apply(Event::Listing(vec![entry("c", State::Available)]));
        assert_eq!(state_json(&controller)["plugin"], "c");

        controller.apply(Event::Listing(Vec::new()));
        assert_eq!(state_json(&controller)["plugin"], NOTHING);
    }

    #[test]
    fn the_state_carries_every_value_the_page_shows() {
        let mut controller = controller(vec![entry("a", State::Available)]);
        select(&mut controller, "a");
        controller.apply(Event::Status("Listed 1 plugin from Main.".to_string()));
        let state = state_json(&controller);
        for key in [
            "source",
            "filter",
            "show_libs",
            "plugin",
            "details",
            "status",
        ] {
            assert!(state.get(key).is_some(), "{key} is missing: {state}");
        }
        assert_eq!(state["plugin"], "a");
        assert_eq!(state["status"], "Listed 1 plugin from Main.");
        assert_eq!(state["show_libs"], false);
    }

    #[test]
    fn a_fresh_controller_says_how_to_start() {
        assert_eq!(
            state_json(&Controller::new(vec!["Main".to_string()]))["status"],
            "Press Refresh to load the list of plugins."
        );
    }
}

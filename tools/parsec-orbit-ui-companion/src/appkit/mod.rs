//! The macOS renderer: a native AppKit shell hosted in a winio window.
//!
//! winio owns the process, the event loop and the window. The window's content is
//! a system split view: a source-list sidebar of pages beside the page being shown,
//! so the system draws its own materials (Liquid Glass on macOS 26 and later).
//! The page schema's controls are real AppKit controls: [`page`] builds them,
//! applies the plugin's values and properties, and turns their events into
//! [`OrbitUiMessage`]s, so nothing runs plugin or pipe work inside an AppKit
//! callback.

mod control;
mod icons;
mod kit;
mod page;
mod shell;
mod target;

use std::collections::HashMap;
use std::sync::Arc;

use objc2::MainThreadMarker;
use parsec_orbit_ui_protocol::schema::{Element, UiControl};
use parsec_orbit_ui_protocol::{ChildMessage, PageState, ParentMessage, UiAction};
use serde_json::Value;
use winio::handle::AsWindow;
use winio::prelude::*;

use crate::model::{self, NavEntry, SLIDER_SETTLE};
use crate::platform::{activate, WakeTarget};
use crate::{Inbound, Shared};
use page::PageView;
use shell::Shell;
use target::Env;

pub(crate) enum OrbitUiMessage {
    InputAvailable,
    UserClose,
    Refresh,
    /// The user chose a page in the sidebar.
    Selected(String),
    /// A control reported a value (`Some`) or a click (`None`).
    Edited {
        page: String,
        key: String,
        value: Option<Value>,
    },
    /// A control holds input that cannot be a value.
    Invalid {
        page: String,
        key: String,
        reason: String,
    },
    /// A slider stayed where it was for [`SLIDER_SETTLE`].
    SliderSettled(u64),
    RestartAndApply,
}

pub(crate) struct OrbitUi {
    context: Arc<Shared>,
    mtm: MainThreadMarker,
    env: Env,
    window: Child<Window>,
    shell: Shell,
    pages: Vec<PageState>,
    views: HashMap<String, PageView>,
    nav_entries: Vec<NavEntry>,
    selected: Option<String>,
    visible: bool,
    revision: u64,
    slider_sequence: u64,
    pending_slider: Option<UiAction>,
}

/// Diagnostics go to standard error; the pipe's standard output carries the protocol.
fn report(message: &str) {
    eprintln!("parsec-orbit-ui-companion: {message}");
}

impl OrbitUi {
    /// Shows a page: builds it on first use and puts it in the page area.
    fn show_page(&mut self, id: &str) {
        let Some(state) = self.pages.iter().find(|state| state.page.id == id) else {
            return;
        };
        if !self.views.contains_key(id) {
            let (mtm, env) = (self.mtm, self.env.clone());
            let view = env.updating(|| {
                let mut view = PageView::build(mtm, &env, state);
                view.apply(state);
                view
            });
            self.views.insert(id.to_owned(), view);
        }
        self.shell.show(Some(&self.views[id].root));
        self.shell.scroll_to_top();
        self.window
            .as_window()
            .as_app_kit()
            .setTitle(&kit::string(&state.page.title));
        self.selected = Some(id.to_owned());
    }

    fn set_visible(&mut self, visible: bool) -> winio::Result<bool> {
        if self.visible == visible {
            return Ok(false);
        }
        self.window.set_visible(visible)?;
        self.visible = visible;
        if visible {
            activate(&self.window)?;
        }
        Ok(true)
    }

    fn request_visibility(&mut self, visible: bool, revision: u64) -> winio::Result<bool> {
        if revision < self.revision {
            return Ok(false);
        }
        self.revision = revision;
        if visible && self.visible {
            activate(&self.window)?;
            Ok(false)
        } else {
            self.set_visible(visible)
        }
    }

    fn navigate(&mut self, page: &str, revision: u64) -> bool {
        if revision < self.revision {
            return false;
        }
        self.revision = revision;
        if !self.pages.iter().any(|state| state.page.id == page) {
            return false;
        }
        self.show_page(page);
        self.shell.select(page);
        true
    }

    fn apply_snapshot(
        &mut self,
        pages: Vec<PageState>,
        visible: bool,
        revision: u64,
    ) -> winio::Result<bool> {
        if revision < self.revision {
            return Ok(false);
        }
        self.revision = revision;

        // A page whose structure changed gets a new view; the rest keep theirs
        // and only receive the new values and properties.
        let previous = std::mem::replace(&mut self.pages, pages);
        self.views.retain(|id, _| {
            let old = previous.iter().find(|state| &state.page.id == id);
            let new = self.pages.iter().find(|state| &state.page.id == id);
            matches!((old, new), (Some(old), Some(new)) if old.page == new.page)
        });
        let selected = self
            .selected
            .clone()
            .filter(|id| self.pages.iter().any(|state| &state.page.id == id))
            .or_else(|| self.pages.first().map(|state| state.page.id.clone()));
        let wanted = model::nav_entries(&self.pages);
        if wanted != self.nav_entries {
            self.nav_entries = wanted;
            self.shell
                .set_nav(&model::nav_rows(&self.nav_entries), selected.as_deref());
        }
        for state in &self.pages {
            if let Some(view) = self.views.get_mut(&state.page.id) {
                self.env.updating(|| view.apply(state));
            }
        }
        match selected {
            Some(id) => {
                // The page view may be the one just dropped; show it again.
                if self.views.contains_key(&id) && self.selected.as_deref() == Some(id.as_str()) {
                    self.shell.show(Some(&self.views[&id].root));
                } else {
                    self.show_page(&id);
                }
                self.shell.select(&id);
            }
            None => {
                self.selected = None;
                self.shell.show(None);
            }
        }
        self.shell.set_notice(model::restart_offered(&self.pages));
        Ok(self.set_visible(visible)? | true)
    }

    /// Sends a user's edit to the plugin, once the page agrees it is a value of
    /// that control.
    fn send_edit(&mut self, page: &str, key: &str, value: Option<Value>) {
        let Some(state) = self.pages.iter().find(|state| state.page.id == page) else {
            return;
        };
        let action = match model::user_action(state, key, value) {
            Ok(action) => action,
            Err(reason) => {
                if let Some(view) = self.views.get_mut(page) {
                    view.set_local_error(key, Some(&reason));
                }
                return;
            }
        };
        if let Some(view) = self.views.get_mut(page) {
            view.set_local_error(key, None);
        }
        let slider = matches!(
            state.page.element(key),
            Some(Element::Setting(setting)) if matches!(setting.control, UiControl::Slider { .. })
        );
        if slider {
            // Report the value a drag ends on, not every step of it.
            self.slider_sequence += 1;
            self.pending_slider = Some(action);
            return;
        }
        self.context.write(ChildMessage::Action(action));
    }
}

impl Component for OrbitUi {
    type Init<'a> = Arc<Shared>;
    type Message = OrbitUiMessage;
    type Event = ();
    type Error = winio::Error;

    async fn init(context: Self::Init<'_>, sender: &ComponentSender<Self>) -> winio::Result<Self> {
        let mut window = Child::<Window>::init(()).await?;
        window.set_text("Parsec Orbit")?;
        window.set_visible(false)?;

        let mtm = MainThreadMarker::new().expect("winio window is not on the main thread");
        let env = Env::new(sender.clone());
        let shell = Shell::install(mtm, window.as_window().as_app_kit(), &env);

        let ui = Self {
            context: Arc::clone(&context),
            mtm,
            env,
            window,
            shell,
            pages: Vec::new(),
            views: HashMap::new(),
            nav_entries: Vec::new(),
            selected: None,
            visible: false,
            revision: 0,
            slider_sequence: 0,
            pending_slider: None,
        };
        let wake = WakeTarget::for_window(&ui.window)?;
        context.connect(sender, wake);
        context.write(ChildMessage::Ready {
            version: parsec_orbit_ui_protocol::VERSION,
        });
        Ok(ui)
    }

    async fn start(&mut self, sender: &ComponentSender<Self>) -> ! {
        self.window
            .start(
                sender,
                |event| {
                    Some(match event {
                        WindowEvent::Close => OrbitUiMessage::UserClose,
                        _ => OrbitUiMessage::Refresh,
                    })
                },
                || OrbitUiMessage::Refresh,
            )
            .await
    }

    async fn update(
        &mut self,
        message: OrbitUiMessage,
        sender: &ComponentSender<Self>,
    ) -> winio::Result<bool> {
        match message {
            OrbitUiMessage::InputAvailable => {
                let mut changed = false;
                for message in self.context.take_input() {
                    match message {
                        Inbound::Eof | Inbound::Frame(ParentMessage::Shutdown) => {
                            sender.output(());
                            return Ok(false);
                        }
                        Inbound::Frame(ParentMessage::Snapshot {
                            pages,
                            visible,
                            revision,
                        }) => match self.apply_snapshot(pages, visible, revision) {
                            Ok(applied) => changed |= applied,
                            Err(error) => report(&format!("could not show the pages: {error:?}")),
                        },
                        Inbound::Frame(ParentMessage::Navigate { page, revision }) => {
                            changed |= self.navigate(&page, revision);
                        }
                        Inbound::Frame(ParentMessage::Visibility { visible, revision }) => {
                            changed |= self.request_visibility(visible, revision)?;
                        }
                    }
                }
                Ok(changed)
            }
            OrbitUiMessage::UserClose => {
                self.set_visible(false)?;
                self.context.write(ChildMessage::Visibility {
                    visible: false,
                    revision: self.revision,
                });
                Ok(true)
            }
            OrbitUiMessage::Selected(id) => {
                if self.selected.as_deref() != Some(id.as_str()) {
                    self.show_page(&id);
                }
                Ok(false)
            }
            OrbitUiMessage::Edited { page, key, value } => {
                self.send_edit(&page, &key, value);
                if self.pending_slider.is_some() {
                    let (sequence, sender) = (self.slider_sequence, sender.clone());
                    std::thread::spawn(move || {
                        std::thread::sleep(SLIDER_SETTLE);
                        sender.post(OrbitUiMessage::SliderSettled(sequence));
                    });
                }
                Ok(false)
            }
            OrbitUiMessage::SliderSettled(sequence) => {
                if sequence == self.slider_sequence {
                    if let Some(action) = self.pending_slider.take() {
                        self.context.write(ChildMessage::Action(action));
                    }
                }
                Ok(false)
            }
            OrbitUiMessage::Invalid { page, key, reason } => {
                if let Some(view) = self.views.get_mut(&page) {
                    view.set_local_error(&key, Some(&reason));
                }
                Ok(false)
            }
            OrbitUiMessage::RestartAndApply => {
                self.context.write(ChildMessage::Action(UiAction {
                    page: model::OVERVIEW.to_string(),
                    key: "restart".to_string(),
                    action_type: "click".to_string(),
                    value: Some(Value::Null),
                }));
                Ok(false)
            }
            OrbitUiMessage::Refresh => Ok(true),
        }
    }

    async fn update_children(&mut self) -> winio::Result<bool> {
        self.window.update().await
    }

    fn render(&mut self, _: &ComponentSender<Self>) -> winio::Result<()> {
        Ok(())
    }

    fn render_children(&mut self) -> winio::Result<()> {
        // The window's content is the system split view, not winio's content view,
        // so there is no winio layout to run.
        Ok(())
    }
}

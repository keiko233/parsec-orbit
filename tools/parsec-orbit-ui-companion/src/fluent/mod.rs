//! The Windows renderer: a Fluent XAML shell hosted in a winio window
//! ([ADR 0027](../../../../docs/design/decisions/0027-windows-fluent-xaml-shell.md)).
//!
//! winio owns the process, the event loop, the window and its Mica backdrop.
//! The window's content is a `NavigationView` built from [`xaml`] templates.
//! The page schema's controls are real WinUI controls: [`view`] builds them,
//! applies the plugin's values and properties, and turns their events into
//! [`OrbitUiMessage`]s, so nothing runs plugin or pipe work inside a XAML
//! callback.

mod icons;
mod view;
mod xaml;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parsec_orbit_ui_protocol::schema::{check_value, Element, UiControl, UiIcon, UiNavGroup};
use parsec_orbit_ui_protocol::{ChildMessage, PageState, ParentMessage, UiAction};
use parsec_orbit_winui_bindings::{
    FrameworkElement, InfoBar, NavigationView, NavigationViewItem, NavigationViewItemHeader,
    RoutedEventHandler, ScrollViewer,
};
use serde_json::Value;
use windows::Foundation::{IReference, PropertyValue, TypedEventHandler};
use windows_core::{Interface, Ref, HSTRING};
use winio::handle::AsContainer;
use winio::prelude::*;

use crate::platform::{activate, WakeTarget};
use crate::{Inbound, Shared};
use view::{Env, PageView};

/// The page whose `restart` button the restart notice presses.
const OVERVIEW: &str = "orbit.overview";
/// A slider reports once it has been still this long, not on every step of a drag.
const SLIDER_SETTLE: Duration = Duration::from_millis(250);

pub(crate) enum OrbitUiMessage {
    InputAvailable,
    UserClose,
    Refresh,
    /// The user chose a page in the navigation pane.
    Selected(String),
    /// A control reported a value (`Some`) or a click (`None`).
    Edited {
        page: String,
        key: String,
        value: Option<Value>,
    },
    /// A select reported the index of its new choice.
    Chosen {
        page: String,
        key: String,
        index: usize,
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

/// What the navigation pane lists, to tell when it has to be rebuilt.
#[derive(Clone, PartialEq)]
struct NavEntry {
    id: String,
    title: String,
    icon: Option<UiIcon>,
    group: UiNavGroup,
}

pub(crate) struct OrbitUi {
    context: Arc<Shared>,
    env: Env,
    window: Child<Window>,
    root: FrameworkElement,
    nav: NavigationView,
    /// The pane's content; its height is the window's, or the page would not scroll.
    content: FrameworkElement,
    page_scroll: ScrollViewer,
    restart_notice: InfoBar,
    pages: Vec<PageState>,
    views: HashMap<String, PageView>,
    nav_entries: Vec<NavEntry>,
    nav_items: Vec<(String, NavigationViewItem)>,
    selected: Option<String>,
    visible: bool,
    revision: u64,
    slider_sequence: u64,
    pending_slider: Option<UiAction>,
}

fn boxed(text: &str) -> windows_core::Result<windows_core::IInspectable> {
    PropertyValue::CreateString(&HSTRING::from(text))
}

/// The page id a navigation item stands for.
fn tag_of(item: &NavigationViewItem) -> Option<String> {
    let tag = item.Tag().ok()?;
    let reference = tag.cast::<IReference<HSTRING>>().ok()?;
    Some(reference.Value().ok()?.to_string())
}

fn entries(pages: &[PageState]) -> Vec<NavEntry> {
    pages
        .iter()
        .map(|state| NavEntry {
            id: state.page.id.clone(),
            title: state.page.title.clone(),
            icon: state.page.icon,
            group: state.page.nav_group(),
        })
        .collect()
}

/// Whether the restart notice has something to offer: a page needs a restart
/// and the Overview has the button that performs it.
fn restart_offered(pages: &[PageState]) -> bool {
    pages.iter().any(|state| state.restart_required)
        && pages.iter().any(|state| {
            state.page.id == OVERVIEW
                && state.page.element("restart").is_some_and(|element| {
                    matches!(element, Element::Setting(_) | Element::Button(_))
                })
        })
}

impl OrbitUi {
    /// Lists the pages in the pane: the `orbit` group, then a "Plugins" heading
    /// over the rest.
    fn rebuild_nav(&mut self) -> windows_core::Result<()> {
        let menu = self.nav.MenuItems()?;
        menu.Clear()?;
        self.nav_items.clear();
        let (orbit, plugins): (Vec<_>, Vec<_>) = self
            .nav_entries
            .iter()
            .partition(|entry| entry.group == UiNavGroup::Orbit);
        let mut items = Vec::new();
        for entry in &orbit {
            let item: NavigationViewItem = xaml::load(&xaml::nav_item(entry.icon))?.cast()?;
            item.SetContent(&boxed(&entry.title)?)?;
            item.SetTag(&boxed(&entry.id)?)?;
            menu.Append(&item.cast::<windows_core::IInspectable>()?)?;
            items.push((entry.id.clone(), item));
        }
        if !plugins.is_empty() {
            if !orbit.is_empty() {
                let separator = xaml::load(&xaml::nav_separator())?;
                menu.Append(&separator.cast::<windows_core::IInspectable>()?)?;
            }
            let header: NavigationViewItemHeader = xaml::load(&xaml::nav_header())?.cast()?;
            header.SetContent(&boxed("Plugins")?)?;
            menu.Append(&header.cast::<windows_core::IInspectable>()?)?;
            for entry in &plugins {
                let item: NavigationViewItem = xaml::load(&xaml::nav_item(entry.icon))?.cast()?;
                item.SetContent(&boxed(&entry.title)?)?;
                item.SetTag(&boxed(&entry.id)?)?;
                menu.Append(&item.cast::<windows_core::IInspectable>()?)?;
                items.push((entry.id.clone(), item));
            }
        }
        self.nav_items = items;
        Ok(())
    }

    /// Shows a page: builds it on first use and puts it in the page area.
    fn show_page(&mut self, id: &str) -> windows_core::Result<()> {
        let Some(state) = self.pages.iter().find(|state| state.page.id == id) else {
            return Ok(());
        };
        if !self.views.contains_key(id) {
            let env = self.env.clone();
            let view = env.updating(|| -> windows_core::Result<PageView> {
                let mut view = PageView::build(&env, state)?;
                view.apply(state);
                Ok(view)
            })?;
            self.views.insert(id.to_owned(), view);
        }
        let view = &self.views[id];
        self.page_scroll
            .SetContent(&view.root.cast::<windows_core::IInspectable>()?)?;
        let _ = self.page_scroll.ScrollToVerticalOffset(0.0);
        self.selected = Some(id.to_owned());
        Ok(())
    }

    /// Selects the pane item of a page without telling the component.
    fn select_nav(&self, id: &str) {
        if let Some((_, item)) = self.nav_items.iter().find(|(page, _)| page == id) {
            self.env.updating(|| {
                let _ = self.nav.SetSelectedItem(item);
            });
        }
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

    fn navigate(&mut self, page: &str, revision: u64) -> winio::Result<bool> {
        if revision < self.revision {
            return Ok(false);
        }
        self.revision = revision;
        if !self.pages.iter().any(|state| state.page.id == page) {
            return Ok(false);
        }
        self.show_page(page)?;
        self.select_nav(page);
        Ok(true)
    }

    async fn apply_snapshot(
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
        let wanted = entries(&self.pages);
        if wanted != self.nav_entries {
            self.nav_entries = wanted;
            self.rebuild_nav()?;
        }
        for state in &self.pages {
            if let Some(view) = self.views.get_mut(&state.page.id) {
                self.env.updating(|| view.apply(state));
            }
        }

        let selected = self
            .selected
            .clone()
            .filter(|id| self.pages.iter().any(|state| &state.page.id == id))
            .or_else(|| self.pages.first().map(|state| state.page.id.clone()));
        match selected {
            Some(id) => {
                self.show_page(&id)?;
                self.select_nav(&id);
            }
            None => {
                self.selected = None;
                let empty = xaml::load(&xaml::empty_state())?;
                self.page_scroll
                    .SetContent(&empty.cast::<windows_core::IInspectable>()?)?;
            }
        }
        let _ = self.restart_notice.SetIsOpen(restart_offered(&self.pages));
        Ok(self.set_visible(visible)? | true)
    }

    /// Sends a user's edit to the plugin, once the page agrees it is a value of
    /// that control.
    fn send_edit(&mut self, page: &str, key: &str, value: Option<Value>) {
        let Some(state) = self.pages.iter().find(|state| state.page.id == page) else {
            return;
        };
        let action = UiAction {
            page: page.to_owned(),
            key: key.to_owned(),
            action_type: if value.is_some() { "change" } else { "click" }.to_owned(),
            value: Some(value.unwrap_or(Value::Null)),
        };
        if !state.accepts_action(&action) {
            let reason = match (state.page.element(key), &action.value) {
                (Some(Element::Setting(setting)), Some(value)) => {
                    check_value(&setting.control, state.effective_options(key), value).err()
                }
                _ => None,
            };
            let reason = reason.unwrap_or_else(|| "That value is not accepted.".into());
            if let Some(view) = self.views.get_mut(page) {
                view.set_local_error(key, Some(&capitalized(&reason)));
            }
            return;
        }
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

/// Diagnostics go to standard error; the pipe's standard output carries the protocol.
fn report(message: &str) {
    eprintln!("parsec-orbit-ui-companion: {message}");
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => {
            let rest = chars.as_str();
            let sentence = format!("{}{rest}", first.to_uppercase());
            if sentence.ends_with('.') {
                sentence
            } else {
                format!("{sentence}.")
            }
        }
        None => String::new(),
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
        window.set_size(Size::new(980.0, 640.0))?;
        window.set_backdrop(Backdrop::Mica)?;
        window.set_visible(false)?;

        let root = xaml::load(&xaml::shell())?;
        window
            .as_container()
            .as_winui()
            .Children()?
            .Append(&root.cast::<winui3::Microsoft::UI::Xaml::UIElement>()?)?;
        let nav: NavigationView = root.cast()?;
        let content: FrameworkElement = xaml::find(&root, "ContentGrid")?;
        let page_scroll: ScrollViewer = xaml::find(&root, "PageScroll")?;
        let restart_notice: InfoBar = xaml::find(&root, "RestartNotice")?;
        let env = Env::new(sender.clone());

        {
            let (sender, env) = (sender.clone(), env.clone());
            nav.SelectionChanged(&TypedEventHandler::new(
                move |nav: Ref<NavigationView>, _| {
                    if env.is_updating() {
                        return Ok(());
                    }
                    let item = nav.ok()?.SelectedItem()?.cast::<NavigationViewItem>()?;
                    if let Some(id) = tag_of(&item) {
                        sender.post(OrbitUiMessage::Selected(id));
                    }
                    Ok(())
                },
            ))?;
        }
        {
            let sender = sender.clone();
            let restart: parsec_orbit_winui_bindings::Button = xaml::find(&root, "RestartButton")?;
            restart.Click(&RoutedEventHandler::new(move |_, _| {
                sender.post(OrbitUiMessage::RestartAndApply);
                Ok(())
            }))?;
        }
        let empty = xaml::load(&xaml::empty_state())?;
        page_scroll.SetContent(&empty.cast::<windows_core::IInspectable>()?)?;

        let ui = Self {
            context: Arc::clone(&context),
            env,
            window,
            root,
            nav,
            content,
            page_scroll,
            restart_notice,
            pages: Vec::new(),
            views: HashMap::new(),
            nav_entries: Vec::new(),
            nav_items: Vec::new(),
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
                        }) => {
                            // A page that cannot be drawn must not take the window down.
                            match self.apply_snapshot(pages, visible, revision).await {
                                Ok(applied) => changed |= applied,
                                Err(error) => {
                                    report(&format!("could not show the pages: {error:?}"))
                                }
                            }
                        }
                        Inbound::Frame(ParentMessage::Navigate { page, revision }) => {
                            match self.navigate(&page, revision) {
                                Ok(applied) => changed |= applied,
                                Err(error) => {
                                    report(&format!("could not select {page:?}: {error:?}"))
                                }
                            }
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
                    self.show_page(&id)?;
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
            OrbitUiMessage::Chosen { page, key, index } => {
                let value = self
                    .views
                    .get(&page)
                    .and_then(|view| view.choice(&key, index));
                if let Some(value) = value {
                    self.send_edit(&page, &key, Some(value));
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
                    page: OVERVIEW.to_string(),
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
        let size = self.window.client_size()?;
        self.root.SetWidth(size.width)?;
        self.root.SetHeight(size.height)?;
        self.content.SetHeight(size.height)?;
        Ok(())
    }

    fn render_children(&mut self) -> winio::Result<()> {
        self.window.render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_becomes_a_sentence() {
        assert_eq!(
            capitalized("number is outside the allowed range"),
            "Number is outside the allowed range."
        );
        assert_eq!(capitalized("Enter a number."), "Enter a number.");
        assert_eq!(capitalized(""), "");
    }
}

//! The companion window, drawn with `winio` on its own thread.
//!
//! Every panel comes from the registry: plugins register declarative sections
//! (`parsec-orbit.ui`), the window turns each section into a tab and each item
//! into a native control, and edits go back through `dispatch_action`. No
//! plugin links a GUI toolkit, so Rust and JavaScript plugins share the ABI
//! described in `docs/spec/ui-service.md`.
//!
//! The window is created hidden: it costs Parsec's start-up nothing to look at,
//! and [`show`], the hotkeys or `scripts/orbit-manager.ps1 ui` bring it up.

use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::task::Poll;

use serde_json::Value;
use winio::handle::AsWindow;
use winio::prelude::*;

use parsec_orbit_plugin_sdk::prelude::*;

use crate::schema::{UiAction, UiItem};

/// Messages the window handles; anything may post them from any thread.
#[derive(Clone, Debug)]
pub enum OrbitMsg {
    Show,
    Hide,
    Toggle,
    Close,
    Refresh,
    SectionsChanged,
    StateUpdated(String),
    Action { panel: usize, row: usize },
}

/// One native control per declarative item type.
enum Control {
    Toggle(Child<CheckBox>),
    Slider(Child<Slider>),
    Select(Child<ComboBox>),
    Input(Child<Edit>),
    Json(Child<TextBox>),
    Button(Child<Button>),
    Text(Child<Label>),
}

/// Runs `$body` against the widget inside a control. Every arm binds the same
/// name, and only one arm runs, so the body is written once.
macro_rules! with_control {
    ($control:expr, $widget:ident, $body:expr) => {
        match $control {
            Control::Toggle($widget) => $body,
            Control::Slider($widget) => $body,
            Control::Select($widget) => $body,
            Control::Input($widget) => $body,
            Control::Json($widget) => $body,
            Control::Button($widget) => $body,
            Control::Text($widget) => $body,
        }
    };
}

/// One registered item: its label, its control and the last known value.
struct Row {
    item: UiItem,
    label: Child<Label>,
    control: Control,
    value: Value,
}

impl Row {
    async fn new(parent: &Child<ScrollView>, item: UiItem, value: Value) -> winio::Result<Self> {
        let mut label = Child::<Label>::init(parent).await?;
        label.set_text(&item.label)?;
        let control = match item.item_type.as_str() {
            "toggle" => Control::Toggle(Child::init(parent).await?),
            "slider" => {
                let mut slider = Child::<Slider>::init(parent).await?;
                slider.set_minimum(item.min.unwrap_or(0.0).max(0.0) as usize)?;
                slider.set_maximum(item.max.unwrap_or(100.0).max(0.0) as usize)?;
                slider.set_freq(item.step.unwrap_or(1.0).max(1.0) as usize)?;
                Control::Slider(slider)
            }
            "select" => {
                let mut select = Child::<ComboBox>::init(parent).await?;
                select.set_editable(false)?;
                for option in item.options.as_deref().unwrap_or_default() {
                    select.push(&option.label)?;
                }
                Control::Select(select)
            }
            "text_input" | "number_input" => Control::Input(Child::init(parent).await?),
            "json_editor" => Control::Json(Child::init(parent).await?),
            "button" => {
                let mut button = Child::<Button>::init(parent).await?;
                button.set_text(&item.label)?;
                Control::Button(button)
            }
            // `readonly_text`, the schemas' `text`, and anything unknown.
            _ => Control::Text(Child::init(parent).await?),
        };
        let mut row = Self {
            item,
            label,
            control,
            value: Value::Null,
        };
        if let Some(description) = row.item.description.clone() {
            with_control!(&mut row.control, widget, widget.set_tooltip(description))?;
        }
        row.set_value(value)?;
        Ok(row)
    }

    /// Writes a value into the control without emitting an action.
    fn set_value(&mut self, value: Value) -> winio::Result<()> {
        match &mut self.control {
            Control::Toggle(widget) => widget.set_checked(value.as_bool().unwrap_or(false))?,
            Control::Slider(widget) => {
                widget.set_pos(value.as_f64().unwrap_or(0.0).max(0.0) as usize)?;
                self.label
                    .set_text(format!("{}: {}", self.item.label, widget.pos()?))?;
            }
            Control::Select(widget) => {
                let options = self.item.options.as_deref().unwrap_or_default();
                if let Some(index) = options.iter().position(|option| option.value == value) {
                    widget.set_selection(index)?;
                }
            }
            Control::Input(widget) => widget.set_text(display_value(&value))?,
            Control::Json(widget) => {
                widget.set_text(serde_json::to_string_pretty(&value).unwrap_or_default())?
            }
            Control::Text(widget) => widget.set_text(display_value(&value))?,
            Control::Button(_) => {}
        }
        self.value = value;
        Ok(())
    }

    /// Reads what the user has in the control, or `None` when it is not a
    /// usable value (an unparsable number, broken JSON, a read-only row).
    fn edited_value(&self) -> winio::Result<Option<Value>> {
        Ok(match &self.control {
            Control::Toggle(widget) => Some(Value::Bool(widget.is_checked()?)),
            Control::Slider(widget) => Some(Value::from(widget.pos()?)),
            Control::Select(widget) => widget.selection()?.and_then(|index| {
                self.item
                    .options
                    .as_ref()?
                    .get(index)
                    .map(|option| option.value.clone())
            }),
            Control::Input(widget) if self.item.item_type == "number_input" => widget
                .text()?
                .parse::<f64>()
                .ok()
                .filter(|number| {
                    number.is_finite()
                        && self.item.min.is_none_or(|min| *number >= min)
                        && self.item.max.is_none_or(|max| *number <= max)
                })
                .map(Value::from),
            Control::Input(widget) => Some(Value::String(widget.text()?)),
            Control::Json(widget) => serde_json::from_str::<Value>(&widget.text()?)
                .ok()
                .filter(|value| value.is_object() || value.is_array()),
            Control::Button(_) => Some(Value::Null),
            Control::Text(_) => None,
        })
    }

    /// Forwards this control's events to the window until it is dropped. The
    /// root loop restarts every listener after each message.
    async fn listen(&mut self, sender: &ComponentSender<OrbitUi>, panel: usize, row: usize) {
        /// A control's own event type is irrelevant here: any interaction on
        /// this row means "the user touched row `row` of panel `panel`".
        fn touched<E>(panel: usize, row: usize) -> impl FnMut(E) -> Option<OrbitMsg> {
            move |_| Some(OrbitMsg::Action { panel, row })
        }
        match &mut self.control {
            Control::Toggle(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitMsg::Refresh)
                    .await
            }
            Control::Slider(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitMsg::Refresh)
                    .await
            }
            Control::Select(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitMsg::Refresh)
                    .await
            }
            Control::Input(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitMsg::Refresh)
                    .await
            }
            Control::Json(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitMsg::Refresh)
                    .await
            }
            Control::Button(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitMsg::Refresh)
                    .await
            }
            Control::Text(_) => std::future::pending().await,
        }
    }
}

fn display_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A registered section: its tab, the scrolling area and its rows.
struct Panel {
    id: String,
    tab: Child<TabViewItem>,
    scroll: Child<ScrollView>,
    rows: Vec<Row>,
}

pub struct OrbitUi {
    window: Child<Window>,
    tabs: Child<TabView>,
    overview: Child<TabViewItem>,
    overview_text: Child<Label>,
    panels: Vec<Panel>,
}

/// The window's message queue. `None` until the UI thread has built the
/// window, and again after it has gone away.
static UI_SENDER: Mutex<Option<ComponentSender<OrbitUi>>> = Mutex::new(None);
/// The window itself, so another thread can end the event loop's wait.
static UI_HWND: AtomicIsize = AtomicIsize::new(0);
/// Kept here rather than asked of `winio`, so `show`/`hide`/`is_visible` answer
/// from any thread without touching the window.
static IS_VISIBLE: AtomicBool = AtomicBool::new(false);
static STARTED: OnceLock<()> = OnceLock::new();

impl OrbitUi {
    /// Rebuilds every tab from the registry. Cheap enough to run whenever a
    /// plugin registers or removes a section, which happens during start-up.
    async fn rebuild(&mut self) -> winio::Result<()> {
        self.tabs.clear()?;
        self.panels.clear();
        // The first item of a `TabView` is the one it shows, so the overview
        // goes in first and the panels stay hidden until the user picks one.
        self.tabs.push(&self.overview)?;
        let sections = crate::service::registry().list_sections();
        self.overview_text.set_text(format!(
            "Parsec Orbit {}\n\
             \n\
             {} configuration section(s) registered by plugins.\n\
             \n\
             Ctrl+Alt+O or Ctrl+F12 shows and hides this window.\n\
             Closing it hides it; the plugins keep running.",
            env!("CARGO_PKG_VERSION"),
            sections.len()
        ))?;
        for section in sections {
            let mut tab = Child::<TabViewItem>::init(()).await?;
            tab.set_text(&section.title)?;
            self.tabs.push(&tab)?;
            let mut scroll = Child::<ScrollView>::init(&tab).await?;
            scroll.set_vscroll(true)?;
            let state = crate::service::registry()
                .get_state(&section.id)
                .unwrap_or_default();
            let mut rows = Vec::new();
            for item in section.items {
                let value = state
                    .get(&item.key)
                    .cloned()
                    .or_else(|| item.default.clone())
                    .unwrap_or(Value::Null);
                rows.push(Row::new(&scroll, item, value).await?);
            }
            self.panels.push(Panel {
                id: section.id,
                tab,
                scroll,
                rows,
            });
        }
        self.tabs.set_selection(0)?;
        Ok(())
    }

    fn set_visible(&mut self, visible: bool) -> winio::Result<()> {
        self.window.set_visible(visible)?;
        if visible {
            // Safety: the UI thread owns this live window and asks Win32 to
            // restore and focus it so a hotkey brings it to the front.
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    SetForegroundWindow, ShowWindow, SW_RESTORE,
                };
                let hwnd = self.window.as_window().as_win32();
                ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
            }
        }
        IS_VISIBLE.store(visible, Ordering::SeqCst);
        Ok(())
    }
}

impl Component for OrbitUi {
    type Init<'a> = ();
    type Message = OrbitMsg;
    type Event = ();
    type Error = winio::Error;

    async fn init(_: Self::Init<'_>, sender: &ComponentSender<Self>) -> winio::Result<Self> {
        let mut window = Child::<Window>::init(()).await?;
        window.set_text("Parsec Orbit")?;
        window.set_size(Size::new(880.0, 580.0))?;
        window.set_visible(false)?;
        let tabs = Child::<TabView>::init(&window).await?;
        let mut overview = Child::<TabViewItem>::init(()).await?;
        overview.set_text("Overview")?;
        let overview_text = Child::<Label>::init(&overview).await?;
        let mut ui = Self {
            window,
            tabs,
            overview,
            overview_text,
            panels: Vec::new(),
        };
        // Published before the first snapshot, so a section registered while
        // the window is being built still lands as a rebuild message.
        UI_HWND.store(ui.window.as_window().as_win32() as isize, Ordering::SeqCst);
        *UI_SENDER.lock().unwrap() = Some(sender.clone());
        ui.rebuild().await?;
        ui.set_visible(IS_VISIBLE.load(Ordering::SeqCst))?;
        Ok(ui)
    }

    async fn start(&mut self, sender: &ComponentSender<Self>) -> ! {
        // The root loop drops `start` whenever a message arrives and calls it
        // again, so these listeners are rebuilt on every round.
        let Self {
            window,
            tabs,
            panels,
            ..
        } = self;
        let mut listeners: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = Vec::new();
        listeners.push(Box::pin(async {
            window
                .start(
                    sender,
                    |event| {
                        Some(match event {
                            WindowEvent::Close => OrbitMsg::Close,
                            _ => OrbitMsg::Refresh,
                        })
                    },
                    || OrbitMsg::Refresh,
                )
                .await;
        }));
        listeners.push(Box::pin(async {
            tabs.start(sender, |_| Some(OrbitMsg::Refresh), || OrbitMsg::Refresh)
                .await;
        }));
        for (index, panel) in panels.iter_mut().enumerate() {
            let scroll = &mut panel.scroll;
            listeners.push(Box::pin(async {
                scroll.start(sender, |_| None, || OrbitMsg::Refresh).await;
            }));
            for (row, model) in panel.rows.iter_mut().enumerate() {
                listeners.push(Box::pin(model.listen(sender, index, row)));
            }
        }
        poll_fn(|context| {
            for listener in &mut listeners {
                let _ = listener.as_mut().poll(context);
            }
            Poll::<()>::Pending
        })
        .await;
        unreachable!("the listeners never finish")
    }

    async fn update(
        &mut self,
        message: OrbitMsg,
        _: &ComponentSender<Self>,
    ) -> winio::Result<bool> {
        match message {
            OrbitMsg::Show => self.set_visible(true)?,
            OrbitMsg::Hide | OrbitMsg::Close => self.set_visible(false)?,
            OrbitMsg::Toggle => self.set_visible(!IS_VISIBLE.load(Ordering::SeqCst))?,
            OrbitMsg::SectionsChanged => self.rebuild().await?,
            OrbitMsg::StateUpdated(id) => {
                let Some(state) = crate::service::registry().get_state(&id) else {
                    return Ok(false);
                };
                for panel in self.panels.iter_mut().filter(|panel| panel.id == id) {
                    for row in &mut panel.rows {
                        if let Some(value) = state.get(&row.item.key) {
                            if row.value != *value {
                                row.set_value(value.clone())?;
                            }
                        }
                    }
                }
            }
            OrbitMsg::Action { panel, row } => {
                let Some(section) = self.panels.get_mut(panel) else {
                    return Ok(false);
                };
                let section_id = section.id.clone();
                let Some(model) = section.rows.get_mut(row) else {
                    return Ok(false);
                };
                // A button reports every click; a control only reports a value
                // the user actually changed.
                let clicked = model.item.item_type == "button";
                if let Some(value) = model.edited_value()? {
                    if clicked || model.value != value {
                        model.set_value(value.clone())?;
                        crate::service::registry().dispatch_action(&UiAction {
                            section: section_id,
                            key: model.item.key.clone(),
                            action_type: if clicked { "click" } else { "change" }.to_string(),
                            value: Some(value),
                        });
                    }
                }
            }
            OrbitMsg::Refresh => {}
        }
        Ok(true)
    }

    async fn update_children(&mut self) -> winio::Result<bool> {
        let mut changed = self.window.update().await? | self.tabs.update().await?;
        changed |= self.overview.update().await? | self.overview_text.update().await?;
        for panel in &mut self.panels {
            changed |= panel.tab.update().await? | panel.scroll.update().await?;
            for row in &mut panel.rows {
                changed |= row.label.update().await?;
                changed |= with_control!(&mut row.control, widget, widget.update().await)?;
            }
        }
        Ok(changed)
    }

    fn render(&mut self, _: &ComponentSender<Self>) -> winio::Result<()> {
        let size = self.window.client_size()?;
        self.tabs.set_loc(Point::new(0.0, 0.0))?;
        self.tabs.set_size(size)?;
        self.overview_text.set_loc(Point::new(24.0, 24.0))?;
        self.overview_text
            .set_size(Size::new((size.width - 64.0).max(100.0), 260.0))?;
        for panel in &mut self.panels {
            let size = panel.tab.size()?;
            panel.scroll.set_size(size)?;
            let width = (size.width - 64.0).max(100.0);
            let mut y = 20.0;
            for row in &mut panel.rows {
                row.label.set_loc(Point::new(24.0, y))?;
                row.label.set_size(Size::new(width, 24.0))?;
                let height = if matches!(row.control, Control::Json(_)) {
                    120.0
                } else {
                    32.0
                };
                with_control!(
                    &mut row.control,
                    widget,
                    widget.set_loc(Point::new(24.0, y + 28.0))
                )?;
                with_control!(
                    &mut row.control,
                    widget,
                    widget.set_size(Size::new(width, height))
                )?;
                y += height + 48.0;
            }
        }
        Ok(())
    }

    fn render_children(&mut self) -> winio::Result<()> {
        self.window.render()?;
        self.tabs.render()?;
        self.overview.render()?;
        self.overview_text.render()?;
        for panel in &mut self.panels {
            panel.tab.render()?;
            panel.scroll.render()?;
            for row in &mut panel.rows {
                row.label.render()?;
                with_control!(&mut row.control, widget, widget.render())?;
            }
        }
        Ok(())
    }
}

/// Tells the window a section's values changed, if it is running.
pub fn notify_state_updated(section: &str) {
    post(OrbitMsg::StateUpdated(section.to_string()));
}

/// Tells the window the set of registered sections changed, if it is running.
pub fn notify_sections_changed() {
    post(OrbitMsg::SectionsChanged);
}

fn post(message: OrbitMsg) {
    if let Some(sender) = UI_SENDER.lock().unwrap().as_ref() {
        sender.post(message);
        wake();
    }
}

/// Wakes the window thread after a message was queued.
///
/// The window runs `winio`'s event loop, which sits in
/// `MsgWaitForMultipleObjectsEx` when it has nothing to do; the queue's own
/// waker only counts while the loop is inside a poll, so a post from another
/// thread can go unnoticed until the next window message. Parking an empty
/// message on the window is what ends the wait, and the loop then drains the
/// queue, including the message posted just before.
fn wake() {
    let handle = UI_HWND.load(Ordering::SeqCst);
    if handle == 0 {
        return;
    }
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_NULL};
        PostMessageW(handle as _, WM_NULL, 0, 0);
    }
}

pub fn show() {
    if UI_SENDER.lock().unwrap().is_some() {
        post(OrbitMsg::Show);
    } else {
        IS_VISIBLE.store(true, Ordering::SeqCst);
    }
}

pub fn hide() {
    if UI_SENDER.lock().unwrap().is_some() {
        post(OrbitMsg::Hide);
    } else {
        IS_VISIBLE.store(false, Ordering::SeqCst);
    }
}

pub fn toggle() {
    if UI_SENDER.lock().unwrap().is_some() {
        post(OrbitMsg::Toggle);
    } else {
        IS_VISIBLE.fetch_xor(true, Ordering::SeqCst);
    }
}

pub fn is_visible() -> bool {
    IS_VISIBLE.load(Ordering::SeqCst)
}

/// Starts the window thread once. Called from the plugin's `init`, so the
/// loader's worker thread is never blocked by a window.
pub fn ensure_started(cx: parsec_orbit_plugin_sdk::Context) {
    STARTED.get_or_init(|| {
        let result = std::thread::Builder::new()
            .name(format!("{}:window", cx.plugin_id()))
            .spawn(move || {
                let result = App::builder()
                    .name("Parsec Orbit")
                    .build()
                    .and_then(|app| app.block_on(OrbitUi::run_until_event(())));
                UI_HWND.store(0, Ordering::SeqCst);
                *UI_SENDER.lock().unwrap() = None;
                IS_VISIBLE.store(false, Ordering::SeqCst);
                match result {
                    Ok(()) => info!(cx, "orbit ui: window thread finished"),
                    Err(error) => error!(cx, "orbit ui: window thread failed: {error}"),
                }
            });
        if let Err(error) = result {
            error!(cx, "orbit ui: could not start the window thread: {error}");
        }
    });
}

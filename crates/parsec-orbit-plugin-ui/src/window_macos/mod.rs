//! The companion window on macOS, drawn with AppKit on the host's main thread.
//!
//! Windows hosts the window on a thread of its own and runs `winio`'s event
//! loop there. macOS cannot: `winio`'s AppKit backend refuses any thread that
//! is not the process main thread, and inside the client that thread belongs to
//! the vendor run loop. So this backend creates plain AppKit views *on the main
//! thread*: a worker posts a block to the main queue, the host's own run loop
//! runs it, and the window then lives beside the client's windows. Nothing here
//! pumps events - the host does.
//!
//! The visible contract is the one in `docs/spec/ui-service.md`: the window is
//! built hidden at start-up, `show`, `hide` and `is_visible` answer from any
//! thread, and every registered section becomes a tab whose items are native
//! controls.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, msg_send, sel, AnyThread, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSControl, NSScrollView, NSTabView, NSTabViewItem, NSTextField, NSTextView, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSNotification, NSPoint, NSRect, NSSize, NSString};
use serde_json::Value;

use parsec_orbit_plugin_sdk_std::prelude::*;

use crate::schema::{UiAction, UiItem};

use controls::Control;

mod controls;
mod hotkey;

/// The window the user sees; the same name the Windows backend uses.
const WINDOW_TITLE: &str = "Parsec Orbit";
const WINDOW_WIDTH: f64 = 880.0;
const WINDOW_HEIGHT: f64 = 580.0;
/// Inset of the label and control column from the panel edges.
const MARGIN: f64 = 24.0;
/// Room the vertical scroller takes from the column.
const SCROLLER: f64 = 20.0;
/// Height of a row's label.
const LABEL_HEIGHT: f64 = 18.0;
/// Distance between a row's label and its control.
const LABEL_GAP: f64 = 4.0;
/// Distance between two rows.
const ROW_GAP: f64 = 14.0;
/// Height of the overview text block.
const OVERVIEW_HEIGHT: f64 = 240.0;

/// What another thread asks the window to do. Every command runs inside a
/// main-queue block.
enum Command {
    Show,
    Hide,
    Toggle,
    Rebuild,
    StateUpdated(String),
}

/// The window's visibility. `show`, `hide` and `is_visible` answer from any
/// thread without touching AppKit.
static IS_VISIBLE: AtomicBool = AtomicBool::new(false);
static STARTED: OnceLock<()> = OnceLock::new();

/// The live window.
///
/// Every field is an AppKit object, so the value is only ever created, read or
/// mutated inside a main-queue block. `Send` is asserted for the mutex that
/// holds it, not to move the objects between threads.
static UI: Mutex<Option<Ui>> = Mutex::new(None);

/// One registered item: its label, its control and the last known value.
struct Row {
    item: UiItem,
    /// `None` for a toggle, whose checkbox carries the label itself.
    label: Option<Retained<NSTextField>>,
    control: Control,
    value: Value,
}

impl Row {
    /// Builds the row inside `parent`; the caller lays it out.
    fn new(
        mtm: MainThreadMarker,
        item: UiItem,
        value: Value,
        target: &AnyObject,
        parent: &NSView,
    ) -> Self {
        let control = Control::new(&item, mtm, target, sel!(orbitControlAction:));
        let label = if item.item_type == "toggle" {
            None
        } else {
            let label = NSTextField::labelWithString(&NSString::from_str(&item.label), mtm);
            parent.addSubview(&label);
            Some(label)
        };
        parent.addSubview(control.view());
        if let Some(description) = item.description.as_deref() {
            control.set_tooltip(description);
        }
        control.set_value(&item, &value);
        Self {
            item,
            label,
            control,
            value,
        }
    }

    /// Writes a new value into the control and remembers it.
    fn write(&mut self, value: &Value) {
        self.control.set_value(&self.item, value);
        if self.item.item_type == "slider" {
            if let Some(label) = &self.label {
                label.setStringValue(&NSString::from_str(&format!(
                    "{}: {}",
                    self.item.label,
                    display_value(value)
                )));
            }
        }
        self.value = value.clone();
    }

    /// Places the row `top` points below from the panel's top edge and returns
    /// the top of the next row.
    fn layout(&self, top: f64, width: f64) -> f64 {
        let mut top = top;
        if let Some(label) = &self.label {
            label.setFrame(rect(MARGIN, top, width, LABEL_HEIGHT));
            top += LABEL_HEIGHT + LABEL_GAP;
        }
        let height = self.control.height();
        self.control
            .view()
            .setFrame(rect(MARGIN, top, width, height));
        top + height + ROW_GAP
    }
}

/// A registered section: its tab and the rows inside it.
struct Panel {
    id: String,
    tab: Retained<NSTabViewItem>,
    content: Retained<PanelView>,
    rows: Vec<Row>,
}

struct Ui {
    window: Retained<NSWindow>,
    tabs: Retained<NSTabView>,
    overview: Retained<NSTabViewItem>,
    overview_text: Retained<NSTextField>,
    target: Retained<UiTarget>,
    panels: Vec<Panel>,
}

// Safety: the AppKit objects are only ever touched inside a main-queue block,
// which the `UI` mutex serialises. Nothing hands them to another thread.
unsafe impl Send for Ui {}

impl Ui {
    /// Builds the window, hidden, with the sections that are registered now.
    fn create(mtm: MainThreadMarker) -> Self {
        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(WINDOW_WIDTH, WINDOW_HEIGHT),
        );
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str(WINDOW_TITLE));
        // Closing the window hides it: the plugins keep running and the same
        // window comes back, so it must not release itself.
        unsafe { window.setReleasedWhenClosed(false) };
        window.center();

        let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
        let tabs = NSTabView::initWithFrame(NSTabView::alloc(mtm), frame);
        tabs.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&tabs);
        window.setContentView(Some(&content));

        let overview = new_tab("Overview");
        let overview_view = PanelView::new(mtm, frame);
        let overview_text = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        overview_text.setFrame(rect(MARGIN, MARGIN, column_width(), OVERVIEW_HEIGHT));
        overview_view.addSubview(&overview_text);
        overview.setView(Some(&overview_view));

        let mut ui = Self {
            window,
            tabs,
            overview,
            overview_text,
            target: UiTarget::new(),
            panels: Vec::new(),
        };
        ui.rebuild(mtm);
        ui.set_visible(mtm, IS_VISIBLE.load(Ordering::SeqCst));
        ui
    }

    /// Rebuilds every tab from the registry. Cheap enough to run whenever a
    /// plugin registers or removes a section, which happens during start-up.
    fn rebuild(&mut self, mtm: MainThreadMarker) {
        for item in self.tabs.tabViewItems().iter() {
            self.tabs.removeTabViewItem(&item);
        }
        self.panels.clear();
        // The first item of a `NSTabView` is the one it shows, so the overview
        // goes in first and the panels stay hidden until the user picks one.
        self.tabs.addTabViewItem(&self.overview);
        let sections = crate::service::registry().list_sections();
        self.overview_text
            .setStringValue(&NSString::from_str(&format!(
                "Parsec Orbit {}\n\
             \n\
             {} configuration section(s) registered by plugins.\n\
             \n\
             Control+Option+O or Control+F12 shows and hides this window.\n\
             Closing it hides it; the plugins keep running.",
                env!("CARGO_PKG_VERSION"),
                sections.len()
            )));

        let mut tag = 0;
        for section in sections {
            let tab = new_tab(&section.title);
            let scroll = NSScrollView::initWithFrame(
                NSScrollView::alloc(mtm),
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(WINDOW_WIDTH, WINDOW_HEIGHT),
                ),
            );
            scroll.setHasVerticalScroller(true);
            let content = PanelView::new(
                mtm,
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
            );
            content.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
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
                let row = Row::new(mtm, item, value, &self.target, &content);
                row.control.set_tag(tag);
                tag += 1;
                rows.push(row);
            }
            let mut panel = Panel {
                id: section.id,
                tab,
                content,
                rows,
            };
            layout_panel(&mut panel);
            scroll.setDocumentView(Some(&panel.content));
            panel.tab.setView(Some(&scroll));
            self.tabs.addTabViewItem(&panel.tab);
            self.panels.push(panel);
        }
        self.tabs.selectTabViewItemAtIndex(0);
    }

    /// Shows or hides the window. Runs on the main thread.
    fn set_visible(&mut self, mtm: MainThreadMarker, visible: bool) {
        if visible {
            let app = NSApplication::sharedApplication(mtm);
            if app.activationPolicy() == NSApplicationActivationPolicy::Prohibited {
                // A background-only host cannot order a window front. The
                // client's stub starts that way until it shows its own window,
                // so a companion window promotes the policy to `Accessory`
                // (windows without a second Dock icon) and leaves every other
                // policy to the vendor.
                app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            }
            self.window.makeKeyAndOrderFront(None);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        } else {
            self.window.orderOut(None);
        }
        IS_VISIBLE.store(visible, Ordering::SeqCst);
    }

    /// Pushes the registry's values into the window for one section.
    fn update_state(&mut self, id: &str) {
        let Some(state) = crate::service::registry().get_state(id) else {
            return;
        };
        for panel in self.panels.iter_mut().filter(|panel| panel.id == id) {
            for row in &mut panel.rows {
                if let Some(value) = state.get(&row.item.key) {
                    if row.value != *value {
                        row.write(value);
                    }
                }
            }
        }
    }
}

/// Places every row of a panel and sizes the panel to hold them.
fn layout_panel(panel: &mut Panel) {
    let width = column_width();
    let mut top = MARGIN;
    for row in &mut panel.rows {
        top = row.layout(top, width);
    }
    panel.content.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(WINDOW_WIDTH, top + MARGIN),
    ));
}

/// A rectangle `top` pixels below the top of a flipped view.
fn rect(x: f64, top: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, top), NSSize::new(width, height))
}

/// The width of the label and control column.
fn column_width() -> f64 {
    WINDOW_WIDTH - 2.0 * MARGIN - SCROLLER
}

fn display_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A new tab whose title is `title`.
fn new_tab(title: &str) -> Retained<NSTabViewItem> {
    let tab = unsafe { NSTabViewItem::initWithIdentifier(NSTabViewItem::alloc(), None) };
    tab.setLabel(&NSString::from_str(title));
    tab
}

/// Runs when a control reports a user interaction. AppKit calls this on the
/// main thread.
fn control_action(sender: &NSControl) {
    let tag = sender.tag();
    let mut guard = UI.lock().unwrap();
    let Some(ui) = guard.as_mut() else {
        return;
    };
    for panel in ui.panels.iter_mut() {
        let section = panel.id.clone();
        for row in panel.rows.iter_mut() {
            if row.control.tag() != tag {
                continue;
            }
            dispatch_row(&section, row, false);
            return;
        }
    }
}

/// Runs when the JSON editor loses focus, which is when it reports its value.
fn text_editing_ended(text_view: &NSTextView) {
    let mut guard = UI.lock().unwrap();
    let Some(ui) = guard.as_mut() else {
        return;
    };
    for panel in ui.panels.iter_mut() {
        let section = panel.id.clone();
        for row in panel.rows.iter_mut() {
            let Some(view) = row.control.text_view() else {
                continue;
            };
            if !std::ptr::eq(view, text_view) {
                continue;
            }
            dispatch_row(&section, row, false);
            return;
        }
    }
}

/// Reports one row's current value to the section's subscribers.
fn dispatch_row(section: &str, row: &mut Row, clicked: bool) {
    let clicked = clicked || row.item.item_type == "button";
    let Some(value) = row.control.read_value(&row.item) else {
        return;
    };
    if !clicked && row.value == value {
        return;
    }
    row.write(&value);
    crate::service::registry().dispatch_action(&UiAction {
        section: section.to_string(),
        key: row.item.key.clone(),
        action_type: if clicked { "click" } else { "change" }.to_string(),
        value: Some(value),
    });
}

define_class!(
    /// The target of every control action in the window.
    ///
    /// It holds no state: the handler looks the sender up in the live window,
    /// which only exists on the main thread, and the action runs there.
    #[unsafe(super(NSObject))]
    #[name = "ParsecOrbitUiTarget"]
    #[ivars = ()]
    struct UiTarget;

    impl UiTarget {
        #[unsafe(method(orbitControlAction:))]
        fn orbit_control_action(&self, sender: &NSControl) {
            control_action(sender);
        }

        /// The JSON editor's delegate callback: it reports its value when it
        /// stops being edited, which is the AppKit equivalent of the change
        /// events the other controls send.
        #[unsafe(method(textDidEndEditing:))]
        fn text_did_end_editing(&self, notification: &NSNotification) {
            if let Some(view) = notification.object() {
                if let Ok(view) = view.downcast::<NSTextView>() {
                    text_editing_ended(&view);
                }
            }
        }
    }
);

impl UiTarget {
    fn new() -> Retained<Self> {
        // Safety: the class has no ivars, so `new` needs no further setup.
        unsafe { msg_send![Self::class(), new] }
    }
}

define_class!(
    /// A panel whose origin is its top-left corner, so rows are laid out
    /// downwards from the top.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "ParsecOrbitUiPanel"]
    #[ivars = ()]
    struct PanelView;

    impl PanelView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl PanelView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// Tells the window a section's values changed, if it is running.
pub fn notify_state_updated(section: &str) {
    post(Command::StateUpdated(section.to_string()));
}

/// Tells the window the set of registered sections changed, if it is running.
pub fn notify_sections_changed() {
    post(Command::Rebuild);
}

fn post(command: Command) {
    dispatch_main(move || handle(command));
}

/// Runs one command inside a main-queue block.
fn handle(command: Command) {
    let Some(mtm) = MainThreadMarker::new() else {
        // The main queue block always runs on the main thread; a missing
        // marker means the process has no main-thread AppKit session.
        return;
    };
    let mut guard = UI.lock().unwrap();
    let Some(ui) = guard.as_mut() else {
        // The window is still being built: remember the visibility the caller
        // asked for, which `Ui::create` applies to the finished window.
        match command {
            Command::Show => IS_VISIBLE.store(true, Ordering::SeqCst),
            Command::Hide => IS_VISIBLE.store(false, Ordering::SeqCst),
            Command::Toggle => {
                IS_VISIBLE.fetch_xor(true, Ordering::SeqCst);
            }
            Command::Rebuild | Command::StateUpdated(_) => {}
        }
        return;
    };
    match command {
        Command::Show => ui.set_visible(mtm, true),
        Command::Hide => ui.set_visible(mtm, false),
        Command::Toggle => ui.set_visible(mtm, !IS_VISIBLE.load(Ordering::SeqCst)),
        Command::Rebuild => ui.rebuild(mtm),
        Command::StateUpdated(id) => ui.update_state(&id),
    }
}

/// Runs `work` on the host's main thread, where AppKit may be touched.
fn dispatch_main(work: impl FnOnce() + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(work);
}

pub fn show() {
    post(Command::Show);
}

pub fn hide() {
    post(Command::Hide);
}

pub fn toggle() {
    post(Command::Toggle);
}

pub fn is_visible() -> bool {
    IS_VISIBLE.load(Ordering::SeqCst)
}

/// Builds the window once, on the main thread. Called from the plugin's `init`,
/// so the loader's worker thread never waits for AppKit.
pub fn ensure_started(cx: parsec_orbit_plugin_sdk_std::Context) {
    STARTED.get_or_init(|| {
        dispatch_main(move || {
            let Some(mtm) = MainThreadMarker::new() else {
                error!(
                    cx,
                    "orbit ui: the main queue block is not on the main thread"
                );
                return;
            };
            *UI.lock().unwrap() = Some(Ui::create(mtm));
            if let Err(error) = hotkey::install(toggle) {
                error!(cx, "orbit ui: {error}");
            }
        });
    });
}

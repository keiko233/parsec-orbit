//! Real WinUI renderer regression. Requires an interactive desktop session.
//! Run explicitly with `cargo test -p parsec-orbit-ui-companion --test windows_desktop -- --ignored`.
#![cfg(windows)]

use parsec_orbit_ui_protocol::{
    read_message, write_message, ChildMessage, PageState, ParentMessage, UiAction, UiPage, UiProps,
    VERSION,
};
use std::io::BufReader;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationElement,
    IUIAutomationInvokePattern, IUIAutomationSelectionItemPattern, IUIAutomationSelectionPattern,
    IUIAutomationValuePattern, TreeScope_Descendants, UIA_ComboBoxControlTypeId,
    UIA_InvokePatternId, UIA_ListItemControlTypeId, UIA_SelectionItemPatternId,
    UIA_SelectionPatternId, UIA_TextControlTypeId, UIA_ValuePatternId,
};
use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW,
    ShowWindow, SW_MINIMIZE, WM_CLOSE,
};

const WAIT: Duration = Duration::from_secs(5);
static DESKTOP_TEST_LOCK: Mutex<()> = Mutex::new(());

struct Companion {
    child: Child,
    input: ChildStdin,
    output: Receiver<ChildMessage>,
    reader: Option<JoinHandle<()>>,
}

impl Companion {
    fn launch() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_parsec-orbit-ui-companion"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("launch the built UI companion");
        let input = child.stdin.take().expect("piped companion stdin");
        let stdout = child.stdout.take().expect("piped companion stdout");
        let (send, output) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(message)) = read_message::<ChildMessage>(&mut reader) {
                if send.send(message).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            output,
            reader: Some(reader),
        }
    }

    fn send(&mut self, message: &ParentMessage) {
        write_message(&mut self.input, message).expect("write companion protocol frame");
    }

    fn receive(&self) -> ChildMessage {
        self.output
            .recv_timeout(WAIT)
            .expect("receive companion protocol frame before deadline")
    }
}

impl Drop for Companion {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

struct WindowSearch {
    process_id: u32,
    window: Option<HWND>,
}

unsafe extern "system" fn find_companion_window(hwnd: HWND, data: LPARAM) -> i32 {
    // SAFETY: EnumWindows invokes this synchronously with the live stack pointer
    // passed by find_window.
    let search = unsafe { &mut *(data as *mut WindowSearch) };
    let mut process_id = 0;
    // SAFETY: hwnd is supplied by EnumWindows and process_id is writable.
    unsafe { GetWindowThreadProcessId(hwnd, &mut process_id) };
    if process_id == search.process_id {
        let mut title = [0u16; 128];
        // SAFETY: title is a writable buffer of the supplied length.
        let length = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
        if String::from_utf16_lossy(&title[..length.max(0) as usize]) == "Parsec Orbit" {
            search.window = Some(hwnd);
            return 0;
        }
    }
    1
}

fn find_window(process_id: u32) -> Option<HWND> {
    let mut search = WindowSearch {
        process_id,
        window: None,
    };
    // SAFETY: EnumWindows invokes the callback before returning, while search is
    // alive; the callback only records a matching HWND owned by process_id.
    unsafe {
        EnumWindows(
            Some(find_companion_window),
            &mut search as *mut WindowSearch as LPARAM,
        );
    }
    search.window
}

struct ComApartment;

impl ComApartment {
    fn initialize() -> Self {
        // SAFETY: this test initializes COM on its own test thread and balances
        // the successful initialization in Drop.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .expect("initialize COM for UI Automation");
        Self
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: initialize succeeded once on this thread in initialize().
        unsafe { CoUninitialize() };
    }
}

fn automation() -> IUIAutomation {
    // SAFETY: COM has been initialized on this test thread.
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }
            .expect("create UI Automation client");
    let automation2: IUIAutomation2 = automation.cast().expect("query UI Automation 2");
    // SAFETY: these configure finite waits for provider calls made by this test.
    unsafe {
        automation2
            .SetConnectionTimeout(1_000)
            .expect("set UIA connection timeout");
        automation2
            .SetTransactionTimeout(1_000)
            .expect("set UIA transaction timeout");
    }
    automation
}

fn find_element(
    automation: &IUIAutomation,
    root: &IUIAutomationElement,
    control_type: i32,
    name: Option<&str>,
) -> Option<IUIAutomationElement> {
    // SAFETY: the condition is owned by this automation client and FindAll is
    // scoped to descendants of the owned window or selected section tab.
    let condition = unsafe { automation.CreateTrueCondition() }.expect("create UIA condition");
    let elements = unsafe { root.FindAll(TreeScope_Descendants, &condition) }
        .expect("enumerate owned UIA descendants");
    let length = unsafe { elements.Length() }.expect("read UIA element count");
    (0..length).find_map(|index| {
        let element = unsafe { elements.GetElement(index) }.ok()?;
        let kind = unsafe { element.CurrentControlType() }.ok()?;
        let element_name = unsafe { element.CurrentName() }.ok()?;
        (kind.0 == control_type && name.is_none_or(|name| element_name == name)).then_some(element)
    })
}

/// The element whose `AutomationId` is `id`: the renderer names every control
/// after its key.
fn find_id(
    automation: &IUIAutomation,
    root: &IUIAutomationElement,
    id: &str,
) -> Option<IUIAutomationElement> {
    // SAFETY: the condition is owned by this automation client and FindAll is
    // scoped to descendants of the owned window.
    let condition = unsafe { automation.CreateTrueCondition() }.expect("create UIA condition");
    let elements = unsafe { root.FindAll(TreeScope_Descendants, &condition) }
        .expect("enumerate owned UIA descendants");
    let length = unsafe { elements.Length() }.expect("read UIA element count");
    (0..length).find_map(|index| {
        let element = unsafe { elements.GetElement(index) }.ok()?;
        let element_id = unsafe { element.CurrentAutomationId() }.ok()?;
        (element_id == id).then_some(element)
    })
}

/// The navigation item of a page.
fn find_nav_item(
    automation: &IUIAutomation,
    root: &IUIAutomationElement,
    title: &str,
) -> Option<IUIAutomationElement> {
    find_element(automation, root, UIA_ListItemControlTypeId.0, Some(title))
}

fn select_nav_item(item: &IUIAutomationElement) {
    // SAFETY: the item is a live navigation entry of the owned window.
    let selection = unsafe {
        item.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
    }
    .expect("navigation item exposes SelectionItem pattern");
    unsafe { selection.Select() }.expect("select the navigation item");
}

fn name_of(element: &IUIAutomationElement) -> String {
    // SAFETY: the element was found beneath the owned window.
    unsafe { element.CurrentName() }
        .expect("read element name")
        .to_string()
}

fn value(element: &IUIAutomationElement) -> String {
    // SAFETY: caller found a text edit, which exposes the UIA Value pattern.
    let pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
            .expect("text edit exposes Value pattern");
    unsafe { pattern.CurrentValue() }
        .expect("read text edit value")
        .to_string()
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

fn set_value(element: &IUIAutomationElement, text: &str) {
    // SAFETY: the element is an editable text control of the owned window.
    let pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
            .expect("text edit exposes Value pattern");
    unsafe { pattern.SetValue(&windows::core::BSTR::from(text)) }.expect("edit the text control");
}

fn invoke(element: &IUIAutomationElement) {
    // SAFETY: the element is a button of the owned window.
    let pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) }
            .expect("button exposes Invoke pattern");
    unsafe { pattern.Invoke() }.expect("invoke the button");
}

fn selected_item_name(automation: &IUIAutomation, root: &IUIAutomationElement) -> Option<String> {
    let combo = find_element(automation, root, UIA_ComboBoxControlTypeId.0, None)?;
    // SAFETY: the found combo box is a live descendant of the owned window.
    let selection = unsafe {
        combo.GetCurrentPatternAs::<IUIAutomationSelectionPattern>(UIA_SelectionPatternId)
    }
    .ok()?;
    // SAFETY: the selection pattern was queried from the live combo box.
    let selected = unsafe { selection.GetCurrentSelection() }.ok()?;
    // SAFETY: the returned selection array belongs to this UI Automation client.
    if unsafe { selected.Length() }.ok()? == 0 {
        return None;
    }
    // SAFETY: element zero exists because Length returned a positive count.
    let selected = unsafe { selected.GetElement(0) }.ok()?;
    // SAFETY: the selected element is owned by the companion window tree.
    Some(unsafe { selected.CurrentName() }.ok()?.to_string())
}

fn page(json: &str, reserved: bool) -> PageState {
    PageState::new(UiPage::parse(json, reserved).expect("valid test page"))
}

fn form_page(profile: &str, status: &str) -> PageState {
    let mut state = page(
        r#"{
            "schema":2,
            "id":"com.parsec-orbit.desktop-test",
            "title":"Desktop regression",
            "blocks":[{"type":"group","settings":[
                {"key":"codec","label":"Codec","control":{"type":"select","default":"old","options":[{"label":"Old","value":"old"}]}},
                {"key":"profile","label":"Profile","control":{"type":"text","commit":"change"}},
                {"key":"status","label":"Status","control":{"type":"value"}},
                {"key":"apply","label":"Apply","control":{"type":"button","text":"Run regression action"}}
            ]}]
        }"#,
        false,
    );
    state
        .values
        .insert("profile".to_string(), serde_json::json!(profile));
    state
        .values
        .insert("status".to_string(), serde_json::json!(status));
    state
}

fn multiline_page() -> PageState {
    let mut state = page(
        r#"{
            "schema":2,
            "id":"com.parsec-orbit.multiline-test",
            "title":"Multiline controls",
            "blocks":[{"type":"group","settings":[
                {"key":"notes","label":"Notes","control":{"type":"text","multiline":true,"commit":"change"}},
                {"key":"log","label":"Log","control":{"type":"log"}}
            ]}]
        }"#,
        false,
    );
    state
        .values
        .insert("notes".to_string(), serde_json::json!("line one\nline two"));
    state.values.insert(
        "log".to_string(),
        serde_json::json!("log line one\nlog line two"),
    );
    state
}

fn wait_until(description: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(condition(), "timed out waiting for {description}");
}

fn is_visible(hwnd: HWND) -> bool {
    // SAFETY: hwnd is the companion's HWND found by its child PID.
    unsafe { IsWindowVisible(hwnd) != 0 }
}

fn is_minimized(hwnd: HWND) -> bool {
    // SAFETY: hwnd is the companion's HWND found by its child PID.
    unsafe { IsIconic(hwnd) != 0 }
}

fn send_shutdown_and_wait(companion: &mut Companion) {
    companion.send(&ParentMessage::Shutdown);
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = companion.child.try_wait().expect("poll companion exit") {
            assert!(status.success(), "companion exited with {status}");
            return;
        }
        assert!(Instant::now() < deadline, "companion did not shut down");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Launches the companion and returns it with its window and that window's UIA element.
fn start(automation: &IUIAutomation) -> (Companion, HWND, IUIAutomationElement) {
    let companion = Companion::launch();
    assert_eq!(
        companion.receive(),
        ChildMessage::Ready { version: VERSION }
    );
    let process_id = companion.child.id();
    let mut window = None;
    wait_until("the companion HWND", || {
        window = find_window(process_id);
        window.is_some()
    });
    let window = window.expect("window found for the companion PID");
    assert!(!is_visible(window), "companion starts hidden");
    // UIA starts from the HWND found by the launched child's PID, so its
    // provider tree cannot match an unrelated process or desktop window.
    let element = unsafe { automation.ElementFromHandle(windows::Win32::Foundation::HWND(window)) }
        .expect("get UIA element for owned companion window");
    (companion, window, element)
}

#[test]
#[ignore = "requires an interactive Windows desktop session"]
fn owned_window_tracks_visibility_close_and_reactivation() {
    let _desktop = DESKTOP_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _com = ComApartment::initialize();
    let automation = automation();
    let (mut companion, window, window_element) = start(&automation);

    companion.send(&ParentMessage::Snapshot {
        pages: vec![
            form_page("profile from snapshot", "ready from snapshot"),
            multiline_page(),
        ],
        visible: true,
        revision: 1,
    });
    wait_until("the pages to be listed", || {
        find_nav_item(&automation, &window_element, "Desktop regression").is_some()
            && find_nav_item(&automation, &window_element, "Multiline controls").is_some()
    });
    // Without the Overview's restart button there is no restart notice to show.
    assert!(
        find_id(&automation, &window_element, "RestartButton").is_none_or(|button| unsafe {
            button.CurrentIsOffscreen()
        }
        .expect("restart button state")
        .as_bool())
    );

    // The first page is shown without being chosen.
    let mut profile = None;
    let mut status = None;
    let mut button = None;
    wait_until("the first page's controls to appear", || {
        profile = find_id(&automation, &window_element, "profile");
        status = find_id(&automation, &window_element, "status");
        button = find_id(&automation, &window_element, "apply");
        profile.is_some() && status.is_some() && button.is_some()
    });
    let (profile, status, button) = (profile.unwrap(), status.unwrap(), button.unwrap());
    assert_eq!(value(&profile), "profile from snapshot");
    assert_eq!(name_of(&status), "ready from snapshot");
    // The setting's label names its button; the button's own text is "Run regression action".
    assert_eq!(name_of(&button), "Apply");
    for control in [&profile, &status, &button] {
        // SAFETY: these elements belong to the shown page of the owned child.
        let bounds =
            unsafe { control.CurrentBoundingRectangle() }.expect("read rendered control bounds");
        assert!(bounds.right > bounds.left && bounds.bottom > bounds.top);
    }

    set_value(&profile, "edited through UI Automation");
    assert_eq!(
        companion.receive(),
        ChildMessage::Action(UiAction {
            page: "com.parsec-orbit.desktop-test".to_string(),
            key: "profile".to_string(),
            action_type: "change".to_string(),
            value: Some(serde_json::json!("edited through UI Automation")),
        })
    );

    // New choices and a new selection arrive as properties and values.
    let mut updated = form_page("updated by snapshot", "updated status");
    updated.props.insert(
        "codec".to_string(),
        UiProps {
            options: Some(vec![
                parsec_orbit_ui_protocol::UiOption {
                    label: "Fresh".into(),
                    value: serde_json::json!("new"),
                    description: None,
                },
                parsec_orbit_ui_protocol::UiOption {
                    label: "Legacy".into(),
                    value: serde_json::json!("legacy"),
                    description: None,
                },
            ]),
            ..UiProps::default()
        },
    );
    updated
        .values
        .insert("codec".to_string(), serde_json::json!("new"));
    companion.send(&ParentMessage::Snapshot {
        pages: vec![updated, multiline_page()],
        visible: true,
        revision: 2,
    });
    wait_until("snapshot state to update the controls", || {
        value(&profile) == "updated by snapshot" && name_of(&status) == "updated status"
    });
    wait_until("dynamic select options and value to update", || {
        selected_item_name(&automation, &window_element).as_deref() == Some("Fresh")
    });
    assert!(matches!(
        companion.output.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    // Another page: its text is editable, its log is not, and both have height.
    select_nav_item(
        &find_nav_item(&automation, &window_element, "Multiline controls")
            .expect("multiline page item"),
    );
    let mut notes = None;
    let mut log = None;
    wait_until("the multiline controls to appear", || {
        notes = find_id(&automation, &window_element, "notes");
        log = find_id(&automation, &window_element, "log");
        notes.is_some() && log.is_some()
    });
    let (notes, log) = (notes.unwrap(), log.unwrap());
    assert_eq!(value(&notes), "line one\nline two");
    assert_eq!(
        name_of(&log).replace("\r\n", "\n"),
        "log line one\nlog line two"
    );
    // SAFETY: the notes edit was found beneath the shown page.
    let height = unsafe { notes.CurrentBoundingRectangle() }
        .map(|bounds| bounds.bottom - bounds.top)
        .expect("read multiline bounds");
    assert!(height >= 100, "multiline control has row height");
    set_value(&notes, "edited multiline\ncontent");
    assert_eq!(
        companion.receive(),
        ChildMessage::Action(UiAction {
            page: "com.parsec-orbit.multiline-test".to_string(),
            key: "notes".to_string(),
            action_type: "change".to_string(),
            value: Some(serde_json::json!("edited multiline\ncontent")),
        })
    );

    // A snapshot that does not echo the draft keeps it, on this page and the other.
    companion.send(&ParentMessage::Snapshot {
        pages: vec![
            form_page("profile from snapshot", "probe updated"),
            multiline_page(),
        ],
        visible: true,
        revision: 3,
    });
    select_nav_item(
        &find_nav_item(&automation, &window_element, "Desktop regression")
            .expect("regression page item"),
    );
    wait_until("an unrelated status snapshot to update", || {
        find_id(&automation, &window_element, "status")
            .is_some_and(|status| name_of(&status) == "probe updated")
    });
    select_nav_item(
        &find_nav_item(&automation, &window_element, "Multiline controls")
            .expect("multiline page item"),
    );
    wait_until("the multiline page to return", || {
        find_id(&automation, &window_element, "notes").is_some()
    });
    assert_eq!(
        value(&find_id(&automation, &window_element, "notes").unwrap()),
        "edited multiline\ncontent",
        "a snapshot that does not echo the draft must preserve the local edit"
    );
    assert!(matches!(
        companion.output.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    select_nav_item(
        &find_nav_item(&automation, &window_element, "Desktop regression")
            .expect("regression page item"),
    );
    companion.send(&ParentMessage::Visibility {
        visible: true,
        revision: 3,
    });
    wait_until("protocol show", || is_visible(window));
    let mut button = None;
    wait_until("the action button", || {
        button = find_id(&automation, &window_element, "apply");
        button.is_some()
    });
    invoke(&button.unwrap());
    assert_eq!(
        companion.receive(),
        ChildMessage::Action(UiAction {
            page: "com.parsec-orbit.desktop-test".to_string(),
            key: "apply".to_string(),
            action_type: "click".to_string(),
            // The wire's JSON null decodes as None for Option<Value>.
            value: None,
        }),
        "button invocation must emit the apply click action"
    );

    for revision in [4, 5] {
        // SAFETY: minimize only the HWND owned by the child process.
        unsafe { ShowWindow(window, SW_MINIMIZE) };
        wait_until("the companion window to minimize", || is_minimized(window));
        companion.send(&ParentMessage::Visibility {
            visible: true,
            revision,
        });
        wait_until("protocol visibility to restore the window", || {
            is_visible(window) && !is_minimized(window)
        });
    }

    // SAFETY: post WM_CLOSE only to the companion's HWND.
    assert_ne!(unsafe { PostMessageW(window, WM_CLOSE, 0, 0) }, 0);
    assert_eq!(
        companion.receive(),
        ChildMessage::Visibility {
            visible: false,
            revision: 5,
        }
    );
    wait_until("close to hide the window", || !is_visible(window));
    assert!(
        companion
            .child
            .try_wait()
            .expect("poll companion process")
            .is_none(),
        "closing the window keeps the companion process alive"
    );

    companion.send(&ParentMessage::Visibility {
        visible: true,
        revision: 6,
    });
    wait_until("protocol show after close", || is_visible(window));
    send_shutdown_and_wait(&mut companion);
}

#[test]
#[ignore = "requires an interactive Windows desktop session"]
fn the_restart_notice_appears_with_the_overview_button_and_presses_it() {
    let _desktop = DESKTOP_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _com = ComApartment::initialize();
    let automation = automation();
    let (mut companion, _window, window_element) = start(&automation);

    let overview = page(
        r#"{"schema":2,"id":"orbit.overview","title":"Orbit Overview","group":"orbit","blocks":[{"type":"group","settings":[{"key":"restart","label":"Restart from Overview","control":{"type":"button","text":"Restart from Overview"}}]}]}"#,
        true,
    );
    let plugin = |restart: bool| {
        let mut state = page(
            r#"{"schema":2,"id":"com.example.settings","title":"Plugin settings","blocks":[]}"#,
            false,
        );
        state.restart_required = restart;
        state
    };
    companion.send(&ParentMessage::Snapshot {
        pages: vec![overview.clone(), plugin(false)],
        visible: true,
        revision: 1,
    });
    wait_until("the pages to be listed", || {
        find_nav_item(&automation, &window_element, "Orbit Overview").is_some()
            && find_nav_item(&automation, &window_element, "Plugin settings").is_some()
    });
    // Navigation lists the orbit group first, then a heading over the plugins.
    assert!(find_nav_item(&automation, &window_element, "Overview").is_none());
    assert!(find_element(
        &automation,
        &window_element,
        UIA_TextControlTypeId.0,
        Some("Plugins")
    )
    .is_some());
    assert!(
        find_id(&automation, &window_element, "RestartButton").is_none_or(|button| unsafe {
            button.CurrentIsOffscreen()
        }
        .expect("restart button state")
        .as_bool()),
        "no page needs a restart yet"
    );

    companion.send(&ParentMessage::Snapshot {
        pages: vec![overview, plugin(true)],
        visible: true,
        revision: 2,
    });
    let mut restart = None;
    wait_until("the restart notice", || {
        restart = find_id(&automation, &window_element, "RestartButton");
        restart.as_ref().is_some_and(|button| {
            !unsafe { button.CurrentIsOffscreen() }
                .expect("restart button state")
                .as_bool()
        })
    });
    let restart = restart.unwrap();
    assert_eq!(name_of(&restart), "Restart and apply");
    invoke(&restart);
    assert_eq!(
        companion.receive(),
        ChildMessage::Action(UiAction {
            page: "orbit.overview".to_string(),
            key: "restart".to_string(),
            action_type: "click".to_string(),
            // The wire's JSON null decodes as None for Option<Value>.
            value: None,
        })
    );
    send_shutdown_and_wait(&mut companion);
}

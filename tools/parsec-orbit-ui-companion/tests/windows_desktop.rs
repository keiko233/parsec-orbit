//! Real WinUI renderer regression. Requires an interactive desktop session.
//! Run explicitly with `cargo test -p parsec-orbit-ui-companion --test windows_desktop -- --ignored`.
#![cfg(windows)]

use parsec_orbit_ui_protocol::{
    read_message, write_message, ChildMessage, ParentMessage, SectionState, UiAction, VERSION,
};
use std::collections::HashMap;
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
    IUIAutomationValuePattern, TreeScope_Descendants, UIA_ButtonControlTypeId,
    UIA_ComboBoxControlTypeId, UIA_EditControlTypeId, UIA_InvokePatternId,
    UIA_SelectionItemPatternId, UIA_SelectionPatternId, UIA_TabItemControlTypeId,
    UIA_TextControlTypeId, UIA_ValuePatternId,
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

fn find_tab(
    automation: &IUIAutomation,
    root: &IUIAutomationElement,
    title: &str,
) -> Option<IUIAutomationElement> {
    // WinUI exposes tab labels as child Text elements; TabItem names are empty.
    let condition = unsafe { automation.CreateTrueCondition() }.expect("create UIA condition");
    let elements = unsafe { root.FindAll(TreeScope_Descendants, &condition) }
        .expect("enumerate owned UIA descendants");
    let length = unsafe { elements.Length() }.expect("read UIA element count");
    (0..length).find_map(|index| {
        let element = unsafe { elements.GetElement(index) }.ok()?;
        let kind = unsafe { element.CurrentControlType() }.ok()?;
        (kind == UIA_TabItemControlTypeId
            && find_element(automation, &element, UIA_TextControlTypeId.0, Some(title)).is_some())
        .then_some(element)
    })
}

fn value(element: &IUIAutomationElement) -> String {
    // SAFETY: caller found a text edit, which exposes the UIA Value pattern.
    let pattern =
        unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
            .expect("text edit exposes Value pattern");
    unsafe { pattern.CurrentValue() }
        .expect("read text edit value")
        .to_string()
}

fn find_edit_with_value(
    automation: &IUIAutomation,
    root: &IUIAutomationElement,
    expected: &str,
) -> Option<IUIAutomationElement> {
    let condition = unsafe { automation.CreateTrueCondition() }.expect("create UIA condition");
    let elements = unsafe { root.FindAll(TreeScope_Descendants, &condition) }
        .expect("enumerate owned UIA descendants");
    let length = unsafe { elements.Length() }.expect("read UIA element count");
    (0..length).find_map(|index| {
        let element = unsafe { elements.GetElement(index) }.ok()?;
        let kind = unsafe { element.CurrentControlType() }.ok()?;
        if kind != UIA_EditControlTypeId {
            return None;
        }
        // Elements can disappear between FindAll and reading their pattern.
        let pattern =
            unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
                .ok()?;
        let current = unsafe { pattern.CurrentValue() }.ok()?.to_string();
        (current.replace('\r', "\n") == expected.replace('\r', "\n")).then_some(element)
    })
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

fn form_section(profile: &str, status: &str) -> SectionState {
    SectionState {
        schema: serde_json::from_str(
            r#"{
                "id":"com.parsec-orbit.desktop-test",
                "title":"Desktop regression",
                "items":[
                    {"key":"codec","type":"select","label":"Codec","options":[{"label":"Old","value":"old"}]},
                    {"key":"profile","type":"text_input","label":"Profile"},
                    {"key":"status","type":"readonly_text","label":"Status"},
                    {"key":"apply","type":"button","label":"Run regression action"}
                ]
            }"#,
        )
        .expect("valid test section"),
        values: HashMap::from([
            ("profile".to_string(), serde_json::json!(profile)),
            (
                "codec".to_string(),
                serde_json::json!({"value":"old","options":[{"label":"Old","value":"old"}]}),
            ),
            ("status".to_string(), serde_json::json!(status)),
        ]),
    }
}

fn multiline_section() -> SectionState {
    SectionState {
        schema: serde_json::from_str(
            r#"{
                "id":"com.parsec-orbit.multiline-test",
                "title":"Multiline controls",
                "items":[
                    {"key":"notes","type":"text_input","label":"Notes","multiline":true},
                    {"key":"log","type":"text_view","label":"Log"}
                ]
            }"#,
        )
        .expect("valid multiline section"),
        values: HashMap::from([
            ("notes".to_string(), serde_json::json!("line one\nline two")),
            (
                "log".to_string(),
                serde_json::json!("log line one\nlog line two"),
            ),
        ]),
    }
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

#[test]
#[ignore = "requires an interactive Windows desktop session"]
fn owned_window_tracks_visibility_close_and_reactivation() {
    let _desktop = DESKTOP_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _com = ComApartment::initialize();
    let automation = automation();
    let mut companion = Companion::launch();
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
    let window_element =
        unsafe { automation.ElementFromHandle(windows::Win32::Foundation::HWND(window)) }
            .expect("get UIA element for owned companion window");

    companion.send(&ParentMessage::Snapshot {
        sections: vec![
            form_section("profile from snapshot", "ready from snapshot"),
            multiline_section(),
        ],
        visible: true,
        revision: 1,
    });
    if let Some(restart) = find_element(
        &automation,
        &window_element,
        UIA_ButtonControlTypeId.0,
        Some("Restart and apply"),
    ) {
        // SAFETY: the matched button, if present in the UIA tree, belongs to this window.
        assert!(unsafe { restart.CurrentIsOffscreen() }
            .expect("hidden restart button state")
            .as_bool());
    }
    let mut section = None;
    wait_until("snapshot controls to appear", || {
        section = find_tab(&automation, &window_element, "Desktop regression");
        section.is_some() && find_tab(&automation, &window_element, "Overview").is_some()
    });
    let section = section.expect("owned section tab");
    let selection = unsafe {
        section.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
    }
    .expect("section tab exposes SelectionItem pattern");
    unsafe { selection.Select() }.expect("select test section tab");

    let mut profile = None;
    let mut status = None;
    let mut button = None;
    wait_until("section controls to appear", || {
        profile = find_element(&automation, &window_element, UIA_EditControlTypeId.0, None);
        status = find_element(
            &automation,
            &window_element,
            UIA_TextControlTypeId.0,
            Some("ready from snapshot"),
        );
        button = find_element(
            &automation,
            &window_element,
            UIA_ButtonControlTypeId.0,
            Some("Run regression action"),
        );
        profile.is_some() && status.is_some() && button.is_some()
    });
    assert_eq!(value(profile.as_ref().unwrap()), "profile from snapshot");
    assert_eq!(
        unsafe { status.as_ref().unwrap().CurrentName() }
            .expect("read initial status")
            .to_string(),
        "ready from snapshot"
    );

    for control in [profile.as_ref(), status.as_ref(), button.as_ref()] {
        // SAFETY: these elements belong to the selected tab of the owned child.
        let bounds = unsafe { control.unwrap().CurrentBoundingRectangle() }
            .expect("read rendered control bounds");
        assert!(bounds.right > bounds.left && bounds.bottom > bounds.top);
    }
    // SAFETY: the selected owned text control exposes the Value pattern.
    let edit = unsafe {
        profile
            .as_ref()
            .unwrap()
            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
    }
    .expect("text edit exposes Value pattern");
    unsafe { edit.SetValue(&windows::core::BSTR::from("edited through UI Automation")) }
        .expect("edit the owned text control");
    assert_eq!(
        companion.receive(),
        ChildMessage::Action(UiAction {
            section: "com.parsec-orbit.desktop-test".to_string(),
            key: "profile".to_string(),
            action_type: "change".to_string(),
            value: Some(serde_json::json!("edited through UI Automation")),
        })
    );

    let mut updated = form_section("updated by snapshot", "updated status");
    updated.values.insert(
        "codec".to_string(),
        serde_json::json!({
            "value":"new",
            "options":[
                {"label":"Fresh","value":"new"},
                {"label":"Legacy","value":"legacy"}
            ]
        }),
    );
    companion.send(&ParentMessage::Snapshot {
        sections: vec![updated, multiline_section()],
        visible: true,
        revision: 2,
    });
    wait_until("snapshot state to update the native controls", || {
        profile
            .as_ref()
            .is_some_and(|control| value(control) == "updated by snapshot")
            && status.as_ref().is_some_and(|control| unsafe {
                control
                    .CurrentName()
                    .is_ok_and(|name| name == "updated status")
            })
    });
    wait_until("dynamic select options and value to update", || {
        selected_item_name(&automation, &window_element).as_deref() == Some("Fresh")
    });
    assert!(matches!(
        companion.output.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    let multiline_tab = find_tab(&automation, &window_element, "Multiline controls")
        .expect("multiline section tab");
    // SAFETY: the selected tab belongs to the companion's live window tree.
    let multiline_selection = unsafe {
        multiline_tab
            .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
    }
    .expect("multiline tab exposes SelectionItem pattern");
    // SAFETY: the tab exposes the queried SelectionItem pattern.
    unsafe { multiline_selection.Select() }.expect("select multiline section tab");
    let mut notes = None;
    let mut log = None;
    wait_until("multiline controls to appear", || {
        notes = find_edit_with_value(&automation, &window_element, "line one\nline two");
        log = find_edit_with_value(&automation, &window_element, "log line one\nlog line two");
        notes.is_some() && log.is_some()
    });
    for control in [notes.as_ref().unwrap(), log.as_ref().unwrap()] {
        // SAFETY: both controls were found beneath the selected companion tab.
        let bounds =
            unsafe { control.CurrentBoundingRectangle() }.expect("read multiline control bounds");
        assert!(
            bounds.bottom - bounds.top >= 100,
            "multiline control has row height"
        );
    }
    // SAFETY: the notes edit was found beneath the selected companion tab.
    let notes_value = unsafe {
        notes
            .as_ref()
            .unwrap()
            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
    }
    .expect("multiline input exposes Value pattern");
    // SAFETY: the Value pattern was queried from the live multiline edit.
    assert!(!unsafe { notes_value.CurrentIsReadOnly() }
        .expect("editable multiline input")
        .as_bool());
    // SAFETY: the Value pattern belongs to the owned multiline edit.
    unsafe { notes_value.SetValue(&windows::core::BSTR::from("edited multiline\ncontent")) }
        .expect("edit multiline input");
    assert_eq!(
        companion.receive(),
        ChildMessage::Action(UiAction {
            section: "com.parsec-orbit.multiline-test".to_string(),
            key: "notes".to_string(),
            action_type: "change".to_string(),
            value: Some(serde_json::json!("edited multiline\ncontent")),
        })
    );
    companion.send(&ParentMessage::Snapshot {
        sections: vec![
            form_section("profile from snapshot", "probe updated"),
            multiline_section(),
        ],
        visible: true,
        revision: 3,
    });
    let form_tab = find_tab(&automation, &window_element, "Desktop regression")
        .expect("regression tab after status snapshot");
    // SAFETY: the tab belongs to the companion's live window tree.
    let form_selection = unsafe {
        form_tab
            .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
    }
    .expect("regression tab exposes SelectionItem pattern");
    // SAFETY: the tab exposes the queried SelectionItem pattern.
    unsafe { form_selection.Select() }.expect("select regression tab to observe status update");
    wait_until("unrelated status snapshot to update", || {
        find_element(
            &automation,
            &window_element,
            UIA_TextControlTypeId.0,
            Some("probe updated"),
        )
        .is_some()
    });
    let multiline_tab = find_tab(&automation, &window_element, "Multiline controls")
        .expect("multiline tab after status snapshot");
    // SAFETY: the tab belongs to the companion's live window tree.
    let multiline_selection = unsafe {
        multiline_tab
            .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
    }
    .expect("multiline tab exposes SelectionItem pattern");
    // SAFETY: the tab exposes the queried SelectionItem pattern.
    unsafe { multiline_selection.Select() }.expect("return to multiline tab after snapshot");
    let notes = find_edit_with_value(&automation, &window_element, "edited multiline\ncontent");
    assert_eq!(
        notes
            .as_ref()
            .map(value)
            .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"))
            .as_deref(),
        Some("edited multiline\ncontent"),
        "a snapshot that does not echo the draft must preserve the local edit"
    );
    assert!(matches!(
        companion.output.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    // SAFETY: the log edit was found beneath the selected companion tab.
    let log_value = unsafe {
        log.as_ref()
            .unwrap()
            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
    }
    .expect("text view exposes Value pattern");
    // SAFETY: the Value pattern was queried from the live read-only text view.
    assert!(unsafe { log_value.CurrentIsReadOnly() }
        .expect("read-only text view")
        .as_bool());
    // SAFETY: the Value pattern belongs to the owned read-only text view.
    assert!(unsafe { log_value.SetValue(&windows::core::BSTR::from("forbidden edit")) }.is_err());
    assert!(matches!(
        companion.output.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    let section = find_tab(&automation, &window_element, "Desktop regression")
        .expect("regression section tab");
    // SAFETY: the regression tab belongs to the companion's live window tree.
    let selection = unsafe {
        section.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
    }
    .expect("section tab exposes SelectionItem pattern");
    // SAFETY: the tab exposes the queried SelectionItem pattern.
    unsafe { selection.Select() }.expect("return to test section tab");

    companion.send(&ParentMessage::Visibility {
        visible: true,
        revision: 3,
    });
    wait_until("protocol show", || is_visible(window));

    let button = button.expect("section action button");
    // SAFETY: caller found the named button beneath the selected owned tab.
    let invoke =
        unsafe { button.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) }
            .expect("button exposes Invoke pattern");
    unsafe { invoke.Invoke() }.expect("invoke test action button");
    let action = companion.receive();
    assert_eq!(
        action,
        ChildMessage::Action(UiAction {
            section: "com.parsec-orbit.desktop-test".to_string(),
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
fn registered_overview_replaces_the_builtin_tab() {
    let _desktop = DESKTOP_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _com = ComApartment::initialize();
    let automation = automation();
    let mut companion = Companion::launch();
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
    // SAFETY: this HWND belongs to the launched companion process.
    let window_element = unsafe {
        automation.ElementFromHandle(windows::Win32::Foundation::HWND(
            window.expect("companion window found"),
        ))
    }
    .expect("get UIA element for owned companion window");

    companion.send(&ParentMessage::Snapshot {
        sections: vec![
            SectionState {
                schema: serde_json::from_str(
                    r#"{"id":"orbit.overview","title":"Orbit Overview","items":[{"key":"restart","type":"button","label":"Restart from Overview"}]}"#,
                )
                .expect("valid overview section"),
                values: HashMap::new(),
            },
            SectionState {
                schema: serde_json::from_str(
                    r#"{"id":"com.example.settings","title":"Plugin settings","items":[]}"#,
                )
                .expect("valid plugin section"),
                values: HashMap::new(),
            },
        ],
        visible: true,
        revision: 1,
    });
    wait_until("registered overview tab to appear", || {
        find_tab(&automation, &window_element, "Orbit Overview").is_some()
    });
    assert!(find_tab(&automation, &window_element, "Overview").is_none());
    let plugin_tab =
        find_tab(&automation, &window_element, "Plugin settings").expect("plugin settings tab");
    // SAFETY: the tab is a live descendant of the companion window.
    let plugin_selection = unsafe {
        plugin_tab
            .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
    }
    .expect("plugin tab exposes SelectionItem pattern");
    // SAFETY: the tab exposes the queried SelectionItem pattern.
    unsafe { plugin_selection.Select() }.expect("select plugin tab before restart");
    let restart = find_element(
        &automation,
        &window_element,
        UIA_ButtonControlTypeId.0,
        Some("Restart and apply"),
    )
    .expect("shared restart button appears with registered overview");
    assert!(find_element(
        &automation,
        &window_element,
        UIA_ButtonControlTypeId.0,
        Some("Restart from Overview"),
    )
    .is_none());
    // SAFETY: the matched button is a live descendant of the companion window.
    let invoke =
        unsafe { restart.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) }
            .expect("shared restart button exposes Invoke pattern");
    // SAFETY: the button exposes the queried Invoke pattern.
    unsafe { invoke.Invoke() }.expect("invoke shared restart button");
    assert_eq!(
        companion.receive(),
        ChildMessage::Action(UiAction {
            section: "orbit.overview".to_string(),
            key: "restart".to_string(),
            action_type: "click".to_string(),
            value: None,
        })
    );

    send_shutdown_and_wait(&mut companion);
}

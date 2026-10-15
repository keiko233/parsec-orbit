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
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationElement,
    IUIAutomationInvokePattern, IUIAutomationSelectionItemPattern, IUIAutomationValuePattern,
    TreeScope_Descendants, UIA_ButtonControlTypeId, UIA_EditControlTypeId, UIA_InvokePatternId,
    UIA_SelectionItemPatternId, UIA_TabItemControlTypeId, UIA_TextControlTypeId,
    UIA_ValuePatternId,
};
use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW,
    ShowWindow, SW_MINIMIZE, WM_CLOSE,
};

const WAIT: Duration = Duration::from_secs(5);

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
        let element = unsafe { elements.GetElement(index) }.expect("read UIA element");
        let kind = unsafe { element.CurrentControlType() }.expect("read UIA control type");
        let element_name = unsafe { element.CurrentName() }.expect("read UIA name");
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
        let element = unsafe { elements.GetElement(index) }.expect("read UIA element");
        let kind = unsafe { element.CurrentControlType() }.expect("read UIA control type");
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

fn form_section(profile: &str, status: &str) -> SectionState {
    SectionState {
        schema: serde_json::from_str(
            r#"{
                "id":"com.parsec-orbit.desktop-test",
                "title":"Desktop regression",
                "items":[
                    {"key":"profile","type":"text_input","label":"Profile"},
                    {"key":"status","type":"readonly_text","label":"Status"},
                    {"key":"apply","type":"button","label":"Run regression action"}
                ]
            }"#,
        )
        .expect("valid test section"),
        values: HashMap::from([
            ("profile".to_string(), serde_json::json!(profile)),
            ("status".to_string(), serde_json::json!(status)),
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
        sections: vec![form_section("profile from snapshot", "ready from snapshot")],
        visible: true,
        revision: 1,
    });
    let mut section = None;
    wait_until("snapshot controls to appear", || {
        section = find_tab(&automation, &window_element, "Desktop regression");
        section.is_some()
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

    companion.send(&ParentMessage::Snapshot {
        sections: vec![form_section("updated by snapshot", "updated status")],
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

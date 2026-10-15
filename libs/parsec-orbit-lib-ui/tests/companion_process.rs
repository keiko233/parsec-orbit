//! Owned headless child processes exercise IPC and supervision, not GUI rendering.
use parsec_orbit_lib_ui::{companion::Companion, registry::Registry};
use serde_json::Value;
use std::ffi::c_void;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

fn fixture() -> &'static PathBuf {
    static EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();
    EXECUTABLE.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let output = Command::new(env!("CARGO"))
            .current_dir(&root)
            .args([
                "build",
                "--locked",
                "-p",
                "parsec-orbit-lib-ui",
                "--example",
                "companion-fixture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        root.join("target/debug/examples").join(if cfg!(windows) {
            "companion-fixture.exe"
        } else {
            "companion-fixture"
        })
    })
}

struct Sink {
    values: mpsc::Sender<Value>,
    gate: Option<Mutex<mpsc::Receiver<()>>>,
}
unsafe extern "system" fn callback(user: *mut c_void, bytes: *const u8, length: usize) {
    // Safety: each test holds Sink until off_action has quiesced the callback.
    let sink = unsafe { &*user.cast::<Sink>() };
    let bytes = unsafe { std::slice::from_raw_parts(bytes, length) };
    let value = serde_json::from_slice::<Value>(bytes).unwrap();
    let _ = sink.values.send(value);
    if let Some(gate) = &sink.gate {
        let _ = gate.lock().unwrap().recv();
    }
}

fn registry() -> Arc<Registry> {
    let registry = Arc::new(Registry::new());
    registry
        .register_page(
            r#"{"schema":2,"id":"fixture","title":"Fixture","blocks":[{"type":"group","settings":[
                {"key":"observed","label":"Observed","control":{"type":"number","default":42}},
                {"key":"route","label":"Route","control":{"type":"text"}},
                {"key":"crash_once","label":"Crash once","control":{"type":"text"}},
                {"key":"report_hidden","label":"Report hidden","control":{"type":"toggle","default":false}},
                {"key":"stale_close","label":"Stale close","control":{"type":"toggle","default":false}}
            ]}]}"#,
            false,
        )
        .unwrap();
    registry
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < deadline, "condition timed out");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn crashed_child_restarts_and_replays_registry() {
    let registry = registry();
    let marker = std::env::temp_dir().join(format!(
        "orbit-ui-fixture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let state = serde_json::json!({"values": {"crash_once": marker.to_string_lossy()}});
    registry.update_page("fixture", &state.to_string()).unwrap();
    let (tx, rx) = mpsc::channel();
    let mut sink = Box::new(Sink {
        values: tx,
        gate: None,
    });
    let subscription = registry.on_action("fixture", callback, (&mut *sink as *mut Sink).cast());
    let visible = Arc::new(AtomicBool::new(true));
    let companion = Companion::start(
        fixture().clone(),
        registry.clone(),
        visible.clone(),
        Arc::new(|_| {}),
    )
    .unwrap();
    let event = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(event["value"], 42);
    assert!(marker.exists(), "first generation did not crash");
    assert!(visible.load(Ordering::SeqCst));
    registry
        .update_page("fixture", r#"{"values":{"observed":99}}"#)
        .unwrap();
    companion.refresh();
    wait_until(|| rx.try_recv().is_ok_and(|value| value["value"] == 99));
    assert_eq!(registry.off_action(subscription), 0);
    drop(companion);
    std::fs::remove_file(marker).unwrap();
}

#[test]
fn navigation_reaches_the_renderer_and_is_replayed_after_a_restart() {
    let registry = registry();
    let marker = std::env::temp_dir().join(format!(
        "orbit-ui-navigation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // The first generation crashes after its handshake; the selection made
    // before launch must still reach the second one.
    let state = serde_json::json!({"values": {"crash_once": marker.to_string_lossy()}});
    registry.update_page("fixture", &state.to_string()).unwrap();
    assert_eq!(registry.navigate("fixture"), 0);
    let (tx, rx) = mpsc::channel();
    let mut sink = Box::new(Sink {
        values: tx,
        gate: None,
    });
    let subscription = registry.on_action("fixture", callback, (&mut *sink as *mut Sink).cast());
    let companion = Companion::start(
        fixture().clone(),
        registry.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(|_| {}),
    )
    .unwrap();
    wait_until(|| rx.try_recv().is_ok_and(|event| event["key"] == "route"));
    assert!(marker.exists(), "first generation did not crash");

    // A later selection is delivered to the running renderer.
    assert_eq!(registry.navigate("fixture"), 0);
    companion.navigate();
    wait_until(|| rx.try_recv().is_ok_and(|event| event["key"] == "route"));
    assert_eq!(registry.off_action(subscription), 0);
    drop(companion);
    std::fs::remove_file(marker).unwrap();
}

#[test]
fn slow_callback_does_not_block_pipe_events() {
    let registry = registry();
    registry
        .update_page("fixture", r#"{"values":{"report_hidden":true}}"#)
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let mut sink = Box::new(Sink {
        values: tx,
        gate: Some(Mutex::new(gate)),
    });
    let subscription = registry.on_action("fixture", callback, (&mut *sink as *mut Sink).cast());
    let visible = Arc::new(AtomicBool::new(true));
    let companion = Companion::start(
        fixture().clone(),
        registry.clone(),
        visible.clone(),
        Arc::new(|_| {}),
    )
    .unwrap();
    rx.recv_timeout(Duration::from_secs(10)).unwrap();
    wait_until(|| !visible.load(Ordering::SeqCst));
    // Disconnecting the release channel unblocks every queued invocation.
    drop(release);
    assert_eq!(registry.off_action(subscription), 0);
    drop(companion);
}

#[test]
fn dropping_supervisor_reaps_its_child() {
    let companion = Companion::start(
        fixture().clone(),
        registry(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(|_| {}),
    )
    .unwrap();
    wait_until(|| companion.child_pid() != 0);
    let pid = companion.child_pid();
    #[cfg(windows)]
    let process = {
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};

        // SAFETY: `pid` identifies the live child owned by `companion`, and
        // PROCESS_SYNCHRONIZE requests only the right needed to wait for it.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(
            !handle.is_null(),
            "could not open companion process for wait"
        );
        OwnedProcessHandle(handle)
    };
    drop(companion);
    #[cfg(unix)]
    assert!(!Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success());
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;

        // SAFETY: `process` owns the valid process handle returned above.
        let result = unsafe { WaitForSingleObject(process.0, 5_000) };
        assert_eq!(result, WAIT_OBJECT_0, "companion process was not reaped");
    }
    #[cfg(not(any(unix, windows)))]
    let _ = pid;
}

#[cfg(windows)]
struct OwnedProcessHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for OwnedProcessHandle {
    fn drop(&mut self) {
        // SAFETY: this wrapper exclusively owns a valid process handle.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(unix)]
#[test]
fn incompatible_peer_exhausts_bounded_retries() {
    use std::os::unix::fs::PermissionsExt;
    let script = std::env::temp_dir().join(format!(
        "orbit-ui-incompatible-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&script, b"#!/bin/sh\nprintf '%s\\n' '{\"type\":\"ready\",\"payload\":{\"version\":999}}'\nexec cat >/dev/null\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = errors.clone();
    let companion = Companion::start(
        script.clone(),
        registry(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(move |text| captured.lock().unwrap().push(text.into())),
    )
    .unwrap();
    wait_until(|| {
        errors
            .lock()
            .unwrap()
            .iter()
            .filter(|error| error.contains("handshake mismatch"))
            .count()
            == 3
            && companion.child_pid() == 0
    });
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(
        errors
            .lock()
            .unwrap()
            .iter()
            .filter(|error| error.contains("handshake mismatch"))
            .count(),
        3
    );
    drop(companion);
    std::fs::remove_file(script).unwrap();
}

#[test]
fn stale_close_does_not_override_a_new_show_request() {
    let registry = registry();
    registry
        .update_page("fixture", r#"{"values":{"stale_close":true}}"#)
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let mut sink = Box::new(Sink {
        values: tx,
        gate: None,
    });
    let subscription = registry.on_action("fixture", callback, (&mut *sink as *mut Sink).cast());
    let visible = Arc::new(AtomicBool::new(false));
    let companion = Companion::start(
        fixture().clone(),
        registry.clone(),
        visible.clone(),
        Arc::new(|_| {}),
    )
    .unwrap();
    companion.set_visible(true);
    rx.recv_timeout(Duration::from_secs(10)).unwrap();
    // A second accepted action proves the reader processed the earlier close.
    registry
        .update_page("fixture", r#"{"values":{"observed":99}}"#)
        .unwrap();
    companion.refresh();
    wait_until(|| rx.try_recv().is_ok_and(|event| event["value"] == 99));
    assert!(visible.load(Ordering::SeqCst));
    assert_eq!(registry.off_action(subscription), 0);
    drop(companion);
}

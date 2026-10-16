//! Built-in panels run on a plugin-owned worker, using the same UI ABI as plugins.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};

#[cfg(not(test))]
use parsec_orbit_layout::{Inputs, Layout, Os};
use parsec_orbit_log_tail::{Follower, FOLLOW_INTERVAL};
use parsec_orbit_plugin_sdk::{Context, Error};
use serde_json::{json, Value};

use crate::service;

const OVERVIEW: &str = "orbit.overview";
const LOG: &str = "orbit.log";
const MAX_LINES: usize = 1000;
const MAX_BYTES: usize = 256 * 1024;

pub(crate) struct Builtins {
    subscriptions: Vec<i64>,
    // The pointee remains stable until every callback has been cancelled.
    _sender: Box<Sender<Value>>,
}

impl Drop for Builtins {
    fn drop(&mut self) {
        for id in &self.subscriptions {
            service::registry().off_action(*id);
        }
    }
}

unsafe extern "system" fn action(user: *mut c_void, bytes: *const u8, len: usize) {
    // Safety: Builtins owns this boxed sender and cancels subscriptions before freeing it.
    let sender = unsafe { &*user.cast::<Sender<Value>>() };
    let bytes = unsafe { std::slice::from_raw_parts(bytes, len) };
    if let Ok(value) = serde_json::from_slice(bytes) {
        let _ = sender.send(value);
    }
}

fn update(page: &str, patch: Value) {
    service::update_page(page, &patch.to_string());
}

/// A patch that shows the outcome of an action beside the setting that caused it.
fn outcome(key: &str, result: Result<String, String>) -> Value {
    let status = match result {
        Ok(text) if text.is_empty() => Value::Null,
        Ok(text) => json!({"severity": "success", "text": text}),
        Err(error) => json!({"severity": "error", "text": format!("Failed: {error}")}),
    };
    json!({"props": {key: {"status": status}}})
}

fn console_mode(value: &str) -> Option<&str> {
    matches!(value, "off" | "1" | "client" | "host").then_some(value)
}

fn read_console(path: &Path) -> Option<String> {
    let value: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if value["format"] != 1 {
        return None;
    }
    console_mode(value["console"].as_str()?).map(str::to_owned)
}

fn save_console(path: &Path, mode: &str) -> std::io::Result<()> {
    if console_mode(mode).is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Invalid console mode",
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json!({"format":1,"console":mode}).to_string())
}

#[derive(Default)]
struct Tail {
    lines: VecDeque<String>,
    bytes: usize,
}

impl Tail {
    fn push(&mut self, mut line: String) {
        if line.len() > MAX_BYTES {
            let mut end = MAX_BYTES;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line.truncate(end);
        }
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.lines.len() > MAX_LINES || self.bytes > MAX_BYTES {
            self.bytes -= self.lines.pop_front().unwrap().len();
        }
    }

    fn visible(&self, level: &str, source: &str) -> String {
        self.lines
            .iter()
            .filter(|line| {
                let parsec = line.starts_with("[parsec] ");
                let source_matches = match source {
                    "orbit" => !parsec,
                    "parsec" => parsec,
                    _ => true,
                };
                let lower = line.to_ascii_lowercase();
                let severity_matches = match level {
                    "error" => lower.contains("[error]"),
                    "warn" => lower.contains("[warn]") || lower.contains("[error]"),
                    "info" => !lower.contains("[debug]") && !lower.contains("[trace]"),
                    _ => true,
                };
                source_matches && severity_matches
            })
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(not(test))]
pub(crate) fn start(cx: Context) -> parsec_orbit_plugin_sdk::Result<Builtins> {
    let data = PathBuf::from(
        cx.data_dir()
            .ok_or(Error::with_message(-4, "UI data directory unavailable"))?,
    );
    let loader_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let layout = Layout::resolve(
        Os::current().ok_or(Error::with_message(-4, "Unsupported UI host"))?,
        Inputs {
            loader_dir: loader_dir.as_deref(),
            home: home.as_deref(),
        },
        |key| std::env::var(key).ok(),
    );
    let log = layout
        .log_candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .or_else(|| layout.log_candidates.first().cloned())
        .unwrap_or_else(|| data.join("parsec-orbit.log"));
    let parsec_log = if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("Parsec").join("log_cl.txt"))
    } else {
        // The macOS host does not yet expose a verified client-log path.
        None
    };
    let preferences = data.join("ui-preferences.json");
    let console = read_console(&preferences).unwrap_or_else(|| {
        let env = std::env::var("PARSEC_ORBIT_CONSOLE").unwrap_or_default();
        match env.as_str() {
            "1" | "on" | "true" | "yes" => "1",
            "client" => "client",
            "host" => "host",
            _ => "off",
        }
        .into()
    });
    let helper = cx
        .plugin_dir()
        .map(|p| Path::new(p).join("parsec-orbit-ui-companion.exe"));
    start_panels(cx, data, log, parsec_log, helper, console)
}

fn overview_page(console: &str, cx: &Context, data: &Path) -> Value {
    let row = |key: &str, label: &str, value: String, monospace: bool| {
        json!({"key": key, "label": label,
            "control": {"type": "value", "default": value, "monospace": monospace, "copyable": true}})
    };
    let button =
        |key: &str, label: &str, text: &str, icon: &str, style: &str, description: Option<&str>| {
            let mut setting = json!({"key": key, "label": label,
            "control": {"type": "button", "text": text, "icon": icon, "style": style}});
            if let Some(description) = description {
                setting["description"] = json!(description);
            }
            setting
        };
    json!({
        "schema": 2, "id": OVERVIEW, "title": "Overview", "icon": "home",
        "group": "orbit", "order": -1000,
        "description": "Versions, console and maintenance for Parsec Orbit.",
        "blocks": [
            {"type": "group", "title": "About", "settings": [
                row("loader", "Orbit Loader", cx.loader_version().to_string(), false),
                row("abi", "Plugin ABI", cx.abi_version().to_string(), false),
                row("ui", "Orbit UI", env!("CARGO_PKG_VERSION").to_string(), false),
                row("instance", "Instance",
                    format!("Interactive client (PID {})", std::process::id()), false),
                row("plugins", "Plugins", cx.plugin_dir().unwrap_or("unavailable").to_string(), true),
                row("data", "Data", data.display().to_string(), true),
            ]},
            {"type": "group", "title": "Console", "settings": [
                {"key": "console", "label": "Console after restart", "icon": "tools",
                 "description": "Opens a debug console for the selected Parsec processes the next time they start.",
                 "control": {"type": "select", "default": console, "options": [
                     {"label": "Off", "value": "off"},
                     {"label": "Both instances", "value": "1"},
                     {"label": "Client only", "value": "client"},
                     {"label": "Host only", "value": "host"}]}}
            ]},
            {"type": "group", "title": "Maintenance", "settings": [
                button("restart", "Restart and apply", "Restart", "restart", "accent",
                    Some("Restarts Parsec and its service. Active connections will disconnect. Windows requests administrator approval.")),
                button("open_log", "Log file", "Open", "log", "standard", None),
                button("open_data", "Data directory", "Open", "folder", "standard", None),
                button("copy_versions", "Version information", "Copy", "copy", "standard", None),
            ]}
        ]
    })
}

fn log_page(log: &Path, following_client: bool) -> Value {
    json!({
        "schema": 2, "id": LOG, "title": "Log", "icon": "log",
        "group": "orbit", "order": -900,
        "blocks": [
            {"type": "banner", "key": "following", "severity": "info",
             "message": if following_client {
                 "Following the Orbit and Parsec client logs; a separate console remains optional."
             } else {
                 "Following the Orbit log. The Parsec client log is unavailable on this platform."
             }},
            {"type": "group", "title": "Filter", "settings": [
                {"key": "path", "label": "Orbit log (client instance)",
                 "control": {"type": "value", "default": log.display().to_string(),
                             "monospace": true, "copyable": true}},
                {"key": "source", "label": "Source", "control": {"type": "select", "default": "all",
                    "options": [{"label": "All", "value": "all"},
                                {"label": "Orbit", "value": "orbit"},
                                {"label": "Parsec", "value": "parsec"}]}},
                {"key": "level", "label": "Minimum level", "control": {"type": "select", "default": "all",
                    "options": [{"label": "All", "value": "all"}, {"label": "Info", "value": "info"},
                                {"label": "Warn", "value": "warn"}, {"label": "Error", "value": "error"}]}}
            ]},
            {"type": "group", "title": "Live log",
             "description": "The last 1000 lines, up to 256 KiB.",
             "settings": [
                {"key": "tail", "label": "Output", "control": {"type": "log", "default": "", "rows": 20}},
                {"key": "copy_log", "label": "Visible log",
                 "control": {"type": "button", "text": "Copy", "icon": "copy"}}
            ]}
        ]
    })
}

fn start_panels(
    cx: Context,
    data: PathBuf,
    log: PathBuf,
    parsec_log: Option<PathBuf>,
    helper: Option<PathBuf>,
    console: String,
) -> parsec_orbit_plugin_sdk::Result<Builtins> {
    let preferences = data.join("ui-preferences.json");
    let version = format!("Orbit Loader {}\nABI {}\nUI {}\nInstance: interactive client (PID {})\nPlugins: {}\nData: {}",
        cx.loader_version(), cx.abi_version(), env!("CARGO_PKG_VERSION"), std::process::id(),
        cx.plugin_dir().unwrap_or("unavailable"), data.display());
    for page in [
        overview_page(&console, &cx, &data),
        log_page(&log, parsec_log.is_some()),
    ] {
        if service::register_page(&page.to_string(), true) != 0 {
            return Err(Error::with_message(-4, "UI built-in page was refused"));
        }
    }
    let (sender, receiver) = mpsc::channel();
    let mut owner = Builtins {
        subscriptions: Vec::new(),
        _sender: Box::new(sender),
    };
    for page in [OVERVIEW, LOG] {
        // Safety: stable sender storage is retained by the plugin and cancelled in Drop.
        owner.subscriptions.push(service::registry().on_action(
            page,
            action,
            (&mut *owner._sender as *mut Sender<Value>).cast(),
        ));
    }
    std::thread::Builder::new()
        .name("orbit-ui:panels".into())
        .spawn(move || {
            let mut console = console;
            let mut followers = vec![(false, Follower::new(log.clone()))];
            if let Some(path) = parsec_log {
                followers.push((true, Follower::new(path)));
            }
            let mut tail = Tail::default();
            let mut level = "all".to_string();
            let mut source = "all".to_string();
            let mut previous = String::new();
            loop {
                match receiver.recv_timeout(FOLLOW_INTERVAL) {
                    Ok(action) => {
                        let page = action["page"].as_str().unwrap_or("");
                        let key = action["key"].as_str().unwrap_or("");
                        let value = action["value"].as_str().unwrap_or("");
                        let (target, result) = match (page, key) {
                            (OVERVIEW, "console") => (
                                "console",
                                save_console(&preferences, value)
                                    .map_err(|e| e.to_string())
                                    .and_then(|()| persist_user_console(value))
                                    .map(|()| {
                                        console = value.to_owned();
                                        "Saved. Restart to open or close the console.".to_string()
                                    }),
                            ),
                            (OVERVIEW, "restart") => {
                                ("restart", restart(helper.as_deref(), &console))
                            }
                            (OVERVIEW, "open_log") => ("open_log", open(&log)),
                            (OVERVIEW, "open_data") => ("open_data", open(&data)),
                            (OVERVIEW, "copy_versions") => ("copy_versions", copy(&version)),
                            (LOG, "level") if ["all", "info", "warn", "error"].contains(&value) => {
                                level = value.into();
                                ("level", Ok(String::new()))
                            }
                            (LOG, "source") if ["all", "orbit", "parsec"].contains(&value) => {
                                source = value.into();
                                ("source", Ok(String::new()))
                            }
                            (LOG, "copy_log") => ("copy_log", copy(&tail.visible(&level, &source))),
                            _ => continue,
                        };
                        let saved_console = page == OVERVIEW && key == "console" && result.is_ok();
                        let mut patch = outcome(target, result);
                        if saved_console {
                            patch["restart_required"] = json!(true);
                        }
                        update(page, patch);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                for (parsec, follower) in &mut followers {
                    for line in follower.poll() {
                        tail.push(if *parsec {
                            format!("[parsec] {line}")
                        } else {
                            line
                        });
                    }
                }
                let visible = tail.visible(&level, &source);
                if visible != previous {
                    update(LOG, json!({"values": {"tail": visible}}));
                    previous = visible;
                }
            }
        })?;
    Ok(owner)
}

fn restart(helper: Option<&Path>, console: &str) -> Result<String, String> {
    #[cfg(windows)]
    {
        persist_user_console(console)?;
        crate::maintenance::launch(helper.ok_or("Companion path unavailable")?, console)?;
        Ok("Restart requested. Complete the Windows administrator prompt.".into())
    }
    #[cfg(not(windows))]
    {
        let _ = (helper, console);
        Err(
            "Restart and console selection currently require Windows; restart Parsec manually."
                .into(),
        )
    }
}

/// The interactive client's environment can be rebuilt from the user's profile
/// by the Parsec service rather than inherited from the service process.
fn persist_user_console(mode: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::*;
        let key_name: Vec<u16> = "Environment".encode_utf16().chain(Some(0)).collect();
        let name: Vec<u16> = "PARSEC_ORBIT_CONSOLE"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let value: Vec<u16> = mode.encode_utf16().chain(Some(0)).collect();
        let mut key = std::ptr::null_mut();
        // Safety: buffers are terminated and key is a writable output handle.
        unsafe {
            let status = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                key_name.as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut key,
            );
            if status != 0 {
                return Err(format!("Could not open user environment ({status})"));
            }
            let status = RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            );
            RegCloseKey(key);
            if status != 0 {
                return Err(format!("Could not save console environment ({status})"));
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = mode;
        return Err("Console selection currently requires Windows".into());
    }
    #[cfg(windows)]
    Ok(())
}

fn open(path: &Path) -> Result<String, String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // Safety: path is a terminated UTF-16 string retained for the call.
        let result = unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                std::ptr::null_mut(),
                std::ptr::null(),
                wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        } as isize;
        if result <= 32 {
            return Err(format!(
                "Could not open {} (Windows error {result})",
                path.display()
            ));
        }
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok("Opened".into())
}

fn copy(text: &str) -> Result<String, String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::GlobalFree,
            System::{DataExchange::*, Memory::*},
        };
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        // Safety: movable memory is transferred to Windows only after SetClipboardData succeeds.
        unsafe {
            if OpenClipboard(windows_sys::Win32::UI::WindowsAndMessaging::GetDesktopWindow()) == 0 {
                return Err("Clipboard is busy".into());
            }
            let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
            if memory.is_null() {
                CloseClipboard();
                return Err("Clipboard allocation failed".into());
            }
            let pointer = GlobalLock(memory);
            if pointer.is_null() {
                GlobalFree(memory);
                CloseClipboard();
                return Err("Clipboard allocation failed".into());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), pointer.cast(), wide.len());
            GlobalUnlock(memory);
            let ok = EmptyClipboard() != 0 && !SetClipboardData(13, memory).is_null();
            if !ok {
                GlobalFree(memory);
            }
            CloseClipboard();
            if !ok {
                return Err("Could not copy text".into());
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        let mut child = std::process::Command::new("/usr/bin/pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .map_err(|e| e.to_string())?;
        if !child.wait().map_err(|e| e.to_string())?.success() {
            return Err("Clipboard command failed".into());
        }
    }
    Ok("Copied".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_follow_isolated_logs_and_filter_through_the_service() {
        use parsec_orbit_plugin_sdk_testing::MockHost;
        let dir = std::env::temp_dir().join(format!("orbit-ui-panels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let orbit = dir.join("orbit.log");
        let parsec = dir.join("parsec.log");
        std::fs::write(
            &orbit,
            "[info] startup
[error] failed
",
        )
        .unwrap();
        std::fs::write(
            &parsec,
            "connected
",
        )
        .unwrap();
        let host = MockHost::new("com.parsec-orbit.ui");
        let panels = start_panels(
            host.context(),
            dir.clone(),
            orbit.clone(),
            Some(parsec.clone()),
            None,
            "off".into(),
        )
        .unwrap();
        let registry = crate::service::registry();
        let wait_for = |expected: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                let state = registry.get_page(LOG).unwrap();
                if state.values.get("tail").and_then(Value::as_str) == Some(expected) {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "log state: {state:?}");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        wait_for(
            "[info] startup
[error] failed
[parsec] connected",
        );
        let overview = registry.get_page(OVERVIEW).unwrap();
        assert_eq!(overview.values["console"], json!("off"));
        assert_eq!(
            overview.page.group,
            Some(parsec_orbit_ui_protocol::schema::UiNavGroup::Orbit)
        );
        registry.dispatch_action(&crate::schema::UiAction {
            page: LOG.into(),
            key: "level".into(),
            action_type: "change".into(),
            value: Some(json!("error")),
        });
        wait_for("[error] failed");
        std::fs::write(
            &orbit,
            "[error] rotated
",
        )
        .unwrap();
        wait_for(
            "[error] failed
[error] rotated",
        );
        drop(panels);
        registry.unregister_page(OVERVIEW);
        registry.unregister_page(LOG);
        std::fs::remove_file(orbit).unwrap();
        std::fs::remove_file(parsec).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn outcomes_become_a_status_beside_the_setting() {
        let ok = outcome("restart", Ok("Done".into()));
        assert_eq!(
            ok["props"]["restart"]["status"],
            json!({"severity": "success", "text": "Done"})
        );
        let quiet = outcome("level", Ok(String::new()));
        assert!(quiet["props"]["level"]["status"].is_null());
        let failed = outcome("open_log", Err("no shell".into()));
        assert_eq!(
            failed["props"]["open_log"]["status"]["severity"],
            json!("error")
        );
        assert_eq!(
            failed["props"]["open_log"]["status"]["text"],
            json!("Failed: no shell")
        );
    }

    #[test]
    fn tail_is_bounded_and_filters_sources_and_levels() {
        let mut tail = Tail::default();
        for n in 0..1100 {
            tail.push(format!("[info] {n}"));
        }
        assert_eq!(tail.lines.len(), MAX_LINES);
        assert!(!tail.visible("all", "all").contains("[info] 99\n"));
        tail.push("[warn] warning".into());
        tail.push("[error] failure".into());
        tail.push("[parsec] [error] client failure".into());
        assert_eq!(tail.visible("error", "orbit"), "[error] failure");
        assert_eq!(
            tail.visible("warn", "parsec"),
            "[parsec] [error] client failure"
        );
        tail.push("界".repeat(MAX_BYTES));
        assert!(tail.bytes <= MAX_BYTES);
    }

    #[test]
    fn console_preferences_validate_and_survive_reopening() {
        let dir = std::env::temp_dir().join(format!("orbit-ui-prefs-{}", std::process::id()));
        let path = dir.join("preferences.json");
        save_console(&path, "host").unwrap();
        assert_eq!(read_console(&path).as_deref(), Some("host"));
        assert!(save_console(&path, "invalid").is_err());
        assert_eq!(read_console(&path).as_deref(), Some("host"));
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}

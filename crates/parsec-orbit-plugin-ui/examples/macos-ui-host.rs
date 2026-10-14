//! Standalone host probe for the macOS UI companion window.
//!
//! Simulates the host environment: runs an AppKit event loop on the main thread,
//! drives the loader via harness exports from a background thread, activates the
//! companion window, verifies Window Server state, captures a screenshot, and exits 0.

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("probe: macos-ui-host is only supported on macOS");
}

#[cfg(target_os = "macos")]
fn main() {
    macos_main();
}

#[cfg(target_os = "macos")]
use std::ffi::{c_char, c_void, CStr, CString};
#[cfg(target_os = "macos")]
use std::sync::mpsc::channel;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
use objc2::MainThreadMarker;
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

#[cfg(target_os = "macos")]
const RTLD_NOW: i32 = 0x2;
#[cfg(target_os = "macos")]
const RTLD_LOCAL: i32 = 0x4;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

#[cfg(target_os = "macos")]
type HarnessRun = unsafe extern "C" fn(
    log: *const c_char,
    plugin_dirs: *const c_char,
    data_root: *const c_char,
) -> i32;

#[cfg(target_os = "macos")]
type HarnessFind = unsafe extern "C" fn(name: *const c_char) -> i32;

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFNotificationCenterGetDistributedCenter() -> *mut c_void;
    fn CFNotificationCenterPostNotification(
        center: *mut c_void,
        name: *const c_void,
        object: *const c_void,
        user_info: *const c_void,
        deliver_immediately: u8,
    );
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const c_char,
        encoding: u32,
    ) -> *mut c_void;
    fn CFRelease(cf: *const c_void);
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, idx: isize) -> *const c_void;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetValue(number: *const c_void, the_type: isize, value_ptr: *mut c_void) -> bool;
    fn CFStringGetCString(
        the_string: *const c_void,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> *mut c_void;
}

#[cfg(target_os = "macos")]
const K_CF_NUMBER_SINT32_TYPE: isize = 3;
#[cfg(target_os = "macos")]
const K_CF_NUMBER_DOUBLE_TYPE: isize = 13;
#[cfg(target_os = "macos")]
const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;
#[cfg(target_os = "macos")]
const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1;
#[cfg(target_os = "macos")]
const K_CG_NULL_WINDOW_ID: u32 = 0;

#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
struct WindowInfo {
    window_number: u32,
    title: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    layer: i32,
}

#[cfg(target_os = "macos")]
fn cf_str(s: &str) -> *mut c_void {
    let c = CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8) }
}

#[cfg(target_os = "macos")]
fn get_cf_dict_i32(dict: *const c_void, key_str: &str) -> Option<i32> {
    let k = cf_str(key_str);
    let val_ptr = unsafe { CFDictionaryGetValue(dict, k) };
    unsafe { CFRelease(k) };
    if val_ptr.is_null() {
        return None;
    }
    let mut val: i32 = 0;
    let ok = unsafe {
        CFNumberGetValue(
            val_ptr,
            K_CF_NUMBER_SINT32_TYPE,
            &mut val as *mut i32 as *mut c_void,
        )
    };
    if ok {
        Some(val)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn get_cf_dict_f64(dict: *const c_void, key_str: &str) -> Option<f64> {
    let k = cf_str(key_str);
    let val_ptr = unsafe { CFDictionaryGetValue(dict, k) };
    unsafe { CFRelease(k) };
    if val_ptr.is_null() {
        return None;
    }
    let mut val: f64 = 0.0;
    let ok = unsafe {
        CFNumberGetValue(
            val_ptr,
            K_CF_NUMBER_DOUBLE_TYPE,
            &mut val as *mut f64 as *mut c_void,
        )
    };
    if ok {
        Some(val)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn get_cf_dict_string(dict: *const c_void, key_str: &str) -> Option<String> {
    let k = cf_str(key_str);
    let val_ptr = unsafe { CFDictionaryGetValue(dict, k) };
    unsafe { CFRelease(k) };
    if val_ptr.is_null() {
        return None;
    }
    let mut buf = [0 as c_char; 256];
    let ok = unsafe {
        CFStringGetCString(
            val_ptr,
            buf.as_mut_ptr(),
            buf.len() as isize,
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    if ok {
        let cstr = unsafe { CStr::from_ptr(buf.as_ptr()) };
        Some(cstr.to_string_lossy().into_owned())
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn query_on_screen_windows() -> Vec<WindowInfo> {
    let mut results = Vec::new();
    let my_pid = std::process::id() as i32;

    let array = unsafe {
        CGWindowListCopyWindowInfo(K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY, K_CG_NULL_WINDOW_ID)
    };
    if array.is_null() {
        return results;
    }

    let count = unsafe { CFArrayGetCount(array) };
    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() {
            continue;
        }

        let pid = get_cf_dict_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if pid != my_pid {
            continue;
        }

        let window_number = get_cf_dict_i32(dict, "kCGWindowNumber").unwrap_or(0) as u32;
        let layer = get_cf_dict_i32(dict, "kCGWindowLayer").unwrap_or(0);
        let title = get_cf_dict_string(dict, "kCGWindowName").unwrap_or_default();

        let bounds_key = cf_str("kCGWindowBounds");
        let bounds_dict = unsafe { CFDictionaryGetValue(dict, bounds_key) };
        unsafe { CFRelease(bounds_key) };

        let (x, y, w, h) = if !bounds_dict.is_null() {
            (
                get_cf_dict_f64(bounds_dict, "X").unwrap_or(0.0),
                get_cf_dict_f64(bounds_dict, "Y").unwrap_or(0.0),
                get_cf_dict_f64(bounds_dict, "Width").unwrap_or(0.0),
                get_cf_dict_f64(bounds_dict, "Height").unwrap_or(0.0),
            )
        } else {
            (0.0, 0.0, 0.0, 0.0)
        };

        results.push(WindowInfo {
            window_number,
            title,
            x,
            y,
            width: w,
            height: h,
            layer,
        });
    }

    unsafe { CFRelease(array) };
    results
}

#[cfg(target_os = "macos")]
fn post_distributed_toggle_notification() {
    let name_cf = cf_str("com.parsec-orbit.ui.toggle");
    let center = unsafe { CFNotificationCenterGetDistributedCenter() };
    if !center.is_null() && !name_cf.is_null() {
        unsafe {
            CFNotificationCenterPostNotification(
                center,
                name_cf,
                std::ptr::null(),
                std::ptr::null(),
                1,
            );
        }
    }
    if !name_cf.is_null() {
        unsafe { CFRelease(name_cf) };
    }
}

#[cfg(target_os = "macos")]
fn inspect_app_windows() -> Vec<(u32, String, bool)> {
    let (tx, rx) = channel();
    dispatch2::DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            tx.send(Vec::new()).ok();
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let windows = app.windows();
        let mut list = Vec::new();
        for w in windows.iter() {
            let num = w.windowNumber() as u32;
            let title = w.title().to_string();
            let is_visible = w.isVisible();
            list.push((num, title, is_visible));
        }
        tx.send(list).ok();
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn macos_main() {
    let mtm = MainThreadMarker::new().expect("must start on main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    std::thread::spawn(move || {
        worker_run();
    });

    app.run();
}

#[cfg(target_os = "macos")]
fn worker_run() {
    // 1. Give the main thread runloop time to spin up.
    std::thread::sleep(Duration::from_millis(200));

    // 2. Discover built dylibs relative to current executable.
    let exe = std::env::current_exe().expect("resolve current_exe");
    let target_debug = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("resolve target/debug directory");

    let loader_dylib = target_debug.join("libparsec_orbit_loader.dylib");
    let ui_dylib = target_debug.join("libparsec_orbit_plugin_ui.dylib");

    assert!(
        loader_dylib.is_file(),
        "missing loader dylib at {}",
        loader_dylib.display()
    );
    assert!(
        ui_dylib.is_file(),
        "missing ui plugin dylib at {}",
        ui_dylib.display()
    );

    // Build scratch environment under target/.
    let pid = std::process::id();
    let scratch_root = target_debug.join(format!("macos-ui-probe-{}", pid));
    let plugins_dir = scratch_root.join("plugins");
    let plugin_sub = plugins_dir.join("com.parsec-orbit.ui");
    std::fs::create_dir_all(&plugin_sub).expect("create scratch plugin dir");

    let plugin_copy = plugin_sub.join("libparsec_orbit_plugin_ui.dylib");
    std::fs::copy(&ui_dylib, &plugin_copy).expect("copy plugin dylib");

    std::fs::write(
        plugin_sub.join("plugin.toml"),
        r#"id = "com.parsec-orbit.ui"
name = "Orbit UI Service"
version = "0.1.1"
kind = "dylib"
entry = "libparsec_orbit_plugin_ui.dylib"
"#,
    )
    .expect("write plugin.toml");

    let data_root = scratch_root.join("data");
    std::fs::create_dir_all(&data_root).expect("create scratch data dir");
    let log_path = scratch_root.join("parsec-orbit.log");

    println!("probe: scratch_root={}", scratch_root.display());
    println!("probe: scratch_plugin_dir={}", plugin_sub.display());
    println!("probe: scratch_data_root={}", data_root.display());
    println!("probe: scratch_log={}", log_path.display());

    // dlopen the loader dylib
    let loader_c = CString::new(loader_dylib.to_string_lossy().as_bytes()).unwrap();
    let handle = unsafe { dlopen(loader_c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
    assert!(!handle.is_null(), "dlopen failed for loader dylib");

    let run_sym = unsafe { dlsym(handle, c"ParsecOrbitMacosHarnessRun".as_ptr()) };
    assert!(!run_sym.is_null(), "ParsecOrbitMacosHarnessRun missing");
    let harness_run: HarnessRun = unsafe { std::mem::transmute(run_sym) };

    let find_sym = unsafe { dlsym(handle, c"ParsecOrbitMacosHarnessFind".as_ptr()) };
    assert!(!find_sym.is_null(), "ParsecOrbitMacosHarnessFind missing");
    let harness_find: HarnessFind = unsafe { std::mem::transmute(find_sym) };

    // Execute harness scan
    let log_c = CString::new(log_path.to_string_lossy().as_bytes()).unwrap();
    let dirs_c = CString::new(plugins_dir.to_string_lossy().as_bytes()).unwrap();
    let data_c = CString::new(data_root.to_string_lossy().as_bytes()).unwrap();

    let init_failures = unsafe { harness_run(log_c.as_ptr(), dirs_c.as_ptr(), data_c.as_ptr()) };
    println!("probe: init_failures={init_failures}");

    let iface_c = CString::new("parsec-orbit.ui").unwrap();
    let interface_found = unsafe { harness_find(iface_c.as_ptr()) };
    println!("probe: interface_found={interface_found}");

    // Read and print plugin log lines
    let log_content = std::fs::read_to_string(&log_path).unwrap_or_default();
    for line in log_content.lines() {
        if line.contains("com.parsec-orbit.ui") {
            println!("probe: plugin_log={line}");
        }
    }

    // Give main queue time to execute Ui::create
    std::thread::sleep(Duration::from_millis(300));

    // Inspect NSApp.windows() in the host
    let app_windows_initial = inspect_app_windows();
    println!("probe: nsapp_windows_count={}", app_windows_initial.len());
    for (num, title, is_visible) in &app_windows_initial {
        println!(
            "probe: nsapp_window num={} title={:?} is_visible={}",
            num, title, is_visible
        );
    }

    // Check on-screen windows initially (should be 0 because window starts hidden)
    let initial_windows = query_on_screen_windows();
    println!("probe: initial_on_screen_windows={}", initial_windows.len());

    // Post the distributed activation notification
    println!("probe: posting_notification=com.parsec-orbit.ui.toggle");
    post_distributed_toggle_notification();
    std::thread::sleep(Duration::from_millis(400));

    let windows_after_toggle1 = query_on_screen_windows();
    println!(
        "probe: on_screen_windows_after_toggle1={}",
        windows_after_toggle1.len()
    );
    for w in &windows_after_toggle1 {
        println!(
            "probe: window_info_1 number={} title={:?} bounds=({:.1}, {:.1}, {:.1}, {:.1}) layer={}",
            w.window_number, w.title, w.x, w.y, w.width, w.height, w.layer
        );
    }

    let app_windows_after_toggle1 = inspect_app_windows();
    for (num, title, is_visible) in &app_windows_after_toggle1 {
        println!(
            "probe: nsapp_window_after_toggle1 num={} title={:?} is_visible={}",
            num, title, is_visible
        );
    }

    // Post toggle notification second time (to hide)
    post_distributed_toggle_notification();
    std::thread::sleep(Duration::from_millis(400));
    let windows_after_toggle2 = query_on_screen_windows();
    println!(
        "probe: on_screen_windows_after_toggle2={}",
        windows_after_toggle2.len()
    );

    // Post toggle notification third time (to show)
    post_distributed_toggle_notification();
    std::thread::sleep(Duration::from_millis(400));
    let windows_after_toggle3 = query_on_screen_windows();
    println!(
        "probe: on_screen_windows_after_toggle3={}",
        windows_after_toggle3.len()
    );
    for w in &windows_after_toggle3 {
        println!(
            "probe: window_info_3 number={} title={:?} bounds=({:.1}, {:.1}, {:.1}, {:.1}) layer={}",
            w.window_number, w.title, w.x, w.y, w.width, w.height, w.layer
        );
    }

    // Capture screenshot
    let screenshot_path = scratch_root.join("macos-ui-window.png");
    let screencapture_status = std::process::Command::new("screencapture")
        .arg("-x")
        .arg("-o")
        .arg(&screenshot_path)
        .status();
    match screencapture_status {
        Ok(status) => {
            println!(
                "probe: screenshot_path={} status={:?}",
                screenshot_path.display(),
                status.code()
            );
        }
        Err(e) => {
            println!("probe: screencapture_error={e}");
        }
    }

    // Cleanly terminate NSApplication from main queue
    dispatch2::DispatchQueue::main().exec_async(|| {
        let Some(mtm) = MainThreadMarker::new() else {
            std::process::exit(0);
        };
        let app = NSApplication::sharedApplication(mtm);
        app.terminate(None);
    });
}

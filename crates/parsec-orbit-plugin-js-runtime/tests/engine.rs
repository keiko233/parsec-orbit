//! The engine against real scripts: loading a plugin, `host`, timers,
//! permissions, the webview binding and native contexts.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use parsec_orbit_plugin_js_runtime::engine::{spawn, Handle, PluginInfo, Spec};
use parsec_orbit_plugin_sdk_std::services::WebViewClient;
use parsec_orbit_plugin_sdk_std::testing::MockHost;
use parsec_orbit_plugin_sdk_std::{
    Level, MessageCallback, ParsecOrbitInterfaceHeader, ParsecOrbitWebViewV1, ERR_EXCEPTION,
};

// ---- helpers ----------------------------------------------------------------------

struct Dir(PathBuf);

impl Dir {
    fn new(label: &str) -> Dir {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "parsec-orbit-js-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Dir(path)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Lines = Arc<Mutex<Vec<(Level, String)>>>;

fn logger(lines: &Lines) -> Arc<dyn Fn(Level, &str) + Send + Sync> {
    let lines = lines.clone();
    Arc::new(move |level, message| lines.lock().unwrap().push((level, message.to_string())))
}

fn said(lines: &Lines, needle: &str) -> bool {
    lines
        .lock()
        .unwrap()
        .iter()
        .any(|(_, message)| message.contains(needle))
}

fn wait_for(what: &str, lines: &Lines, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if said(lines, needle) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "never saw {needle:?} ({what}); log: {:#?}",
        lines.lock().unwrap()
    );
}

struct Started {
    handle: Handle,
    lines: Lines,
    dir: Dir,
}

fn plugin_spec(
    dir: &Dir,
    id: &str,
    permissions: &[&str],
    webview: Option<WebViewClient>,
    lines: &Lines,
) -> Spec {
    Spec {
        name: format!("test:{id}"),
        plugin: Some(PluginInfo {
            id: id.to_string(),
            version: "1.2.3".to_string(),
            dir: dir.0.clone(),
            data_dir: Some(dir.0.join("data")),
            entry: dir.0.join("main.js"),
            permissions: permissions.iter().map(|p| p.to_string()).collect(),
        }),
        log: logger(lines),
        webview,
        config: None,
        ui: None,
    }
}

/// Starts `main.js` with the given source as a plugin and loads it.
fn load(source: &str, permissions: &[&str]) -> (Started, Result<(), String>) {
    load_with(source, permissions, None)
}

fn load_with(
    source: &str,
    permissions: &[&str],
    webview: Option<WebViewClient>,
) -> (Started, Result<(), String>) {
    let dir = Dir::new("plugin");
    dir.write("main.js", source);
    let lines: Lines = Arc::default();
    let handle = spawn(plugin_spec(
        &dir,
        "com.test.plugin",
        permissions,
        webview,
        &lines,
    ))
    .unwrap();
    let result = handle.load();
    (Started { handle, lines, dir }, result)
}

fn native_context() -> (Handle, Lines) {
    let lines: Lines = Arc::default();
    let handle = spawn(Spec {
        name: "test:native".to_string(),
        plugin: None,
        log: logger(&lines),
        webview: None,
        config: None,
        ui: None,
    })
    .unwrap();
    (handle, lines)
}

// ---- loading ------------------------------------------------------------------------

#[test]
fn a_plugin_is_loaded_and_set_up_with_the_host_object() {
    let (plugin, result) = load(
        r#"
        export default {
          setup(host) {
            host.log.info("hello", host.plugin.id, host.plugin.version, 1 + 1, { a: [1, "x"] });
            host.log.warn("careful");
            host.log.error("bad");
            host.log.debug("detail");
            host.log.trace("fine");
            console.log("from console");
          },
        };
        "#,
        &[],
    );
    result.unwrap();
    let lines = plugin.lines.lock().unwrap().clone();
    assert!(lines.contains(&(
        Level::Info,
        "hello com.test.plugin 1.2.3 2 {\"a\":[1,\"x\"]}".into()
    )));
    assert!(lines.contains(&(Level::Warn, "careful".into())));
    assert!(lines.contains(&(Level::Error, "bad".into())));
    assert!(lines.contains(&(Level::Debug, "detail".into())));
    assert!(lines.contains(&(Level::Trace, "fine".into())));
    assert!(lines.contains(&(Level::Info, "from console".into())));
}

#[test]
fn host_describes_the_plugin_directories() {
    let (plugin, result) = load(
        r#"export default { setup(host) { host.log.info(host.plugin.dir, "|", host.plugin.dataDir); } };"#,
        &[],
    );
    result.unwrap();
    let dir = plugin.dir.0.to_string_lossy().into_owned();
    assert!(said(&plugin.lines, &format!("{dir} | {dir}")) || said(&plugin.lines, &dir));
    assert!(said(&plugin.lines, "data"));
}

#[test]
fn an_async_setup_is_awaited_including_timers() {
    let (plugin, result) = load(
        r#"
        export default {
          async setup(host) {
            host.log.info("before");
            await new Promise((resolve) => host.setTimeout(resolve, 60));
            const value = await Promise.resolve(41);
            host.log.info("after", value + 1);
          },
        };
        "#,
        &[],
    );
    result.unwrap();
    assert!(said(&plugin.lines, "before"));
    assert!(
        said(&plugin.lines, "after 42"),
        "setup finished before it was reported"
    );
}

#[test]
fn a_plugin_without_setup_still_loads() {
    let (_plugin, result) = load("export default {};", &[]);
    result.unwrap();
}

#[test]
fn failures_to_load_are_reported_with_their_reason() {
    let (_, result) = load(
        "export default { setup() { throw new Error('boom'); } };",
        &[],
    );
    let error = result.unwrap_err();
    assert!(
        error.contains("setup threw") && error.contains("boom"),
        "{error}"
    );

    let (_, result) = load(
        "export default { async setup() { throw new TypeError('later'); } };",
        &[],
    );
    let error = result.unwrap_err();
    assert!(
        error.contains("setup failed") && error.contains("TypeError: later"),
        "{error}"
    );

    let (_, result) = load("export default {", &[]);
    assert!(result.unwrap_err().contains("main.js"));

    let (_, result) = load("export const nothing = 1;", &[]);
    assert!(result.unwrap_err().contains("default export"));

    let (_, result) = load("export default 5;", &[]);
    assert!(result.unwrap_err().contains("default export"));

    let (_, result) = load("export default { setup: 5 };", &[]);
    assert!(result.unwrap_err().contains("not a function"));

    let (_, result) = load("throw new Error('at top level');", &[]);
    let error = result.unwrap_err();
    assert!(
        error.contains("the module failed") && error.contains("at top level"),
        "{error}"
    );
}

#[test]
fn a_missing_entry_is_an_error_not_a_hang() {
    let dir = Dir::new("missing");
    let lines: Lines = Arc::default();
    let handle = spawn(plugin_spec(&dir, "com.test.missing", &[], None, &lines)).unwrap();
    let error = handle.load().unwrap_err();
    assert!(error.contains("cannot read"), "{error}");
}

#[test]
fn a_context_can_be_stopped_and_answers_nothing_afterwards() {
    let (plugin, result) = load("export default {};", &[]);
    result.unwrap();
    plugin.handle.stop();
    std::thread::sleep(Duration::from_millis(100));
    let error = plugin.handle.eval("1").unwrap_err();
    assert!(error.message.contains("gone"), "{}", error.message);
}

// ---- timers and promises -----------------------------------------------------------------

#[test]
fn timers_fire_in_order_repeat_and_can_be_cleared() {
    let (plugin, result) = load(
        r#"
        export default {
          setup(host) {
            const order = [];
            host.setTimeout(() => order.push("c"), 90);
            host.setTimeout(() => order.push("a"), 10);
            host.setTimeout((x, y) => order.push("b" + x + y), 40, 1, 2);
            const cancelled = host.setTimeout(() => order.push("never"), 20);
            host.clearTimeout(cancelled);
            let ticks = 0;
            const interval = host.setInterval(() => {
              ticks += 1;
              if (ticks === 3) { host.clearInterval(interval); order.push("ticks"); }
            }, 15);
            host.setTimeout(() => host.log.info("order:", order.join(",")), 250);
          },
        };
        "#,
        &[],
    );
    result.unwrap();
    wait_for("the timers", &plugin.lines, "order:");
    let line = plugin
        .lines
        .lock()
        .unwrap()
        .iter()
        .find(|(_, m)| m.starts_with("order:"))
        .unwrap()
        .1
        .clone();
    assert_eq!(line, "order: a,b12,ticks,c");
}

#[test]
fn a_timer_that_throws_is_logged_and_the_others_still_run() {
    let (plugin, result) = load(
        r#"
        export default {
          setup(host) {
            host.setTimeout(() => { throw new Error("in a timer"); }, 5);
            host.setTimeout(() => host.log.info("second timer ran"), 30);
          },
        };
        "#,
        &[],
    );
    result.unwrap();
    wait_for("the second timer", &plugin.lines, "second timer ran");
    assert!(said(
        &plugin.lines,
        "uncaught error in a timer: Error: in a timer"
    ));
}

#[test]
fn unhandled_rejections_are_logged_handled_ones_are_not() {
    let (plugin, result) = load(
        r#"
        export default {
          setup(host) {
            Promise.reject(new Error("nobody catches me"));
            Promise.reject(new Error("caught in time")).catch(() => {});
            host.setTimeout(() => host.log.info("tick"), 40);
          },
        };
        "#,
        &[],
    );
    result.unwrap();
    wait_for(
        "the rejection report",
        &plugin.lines,
        "unhandled promise rejection",
    );
    assert!(said(&plugin.lines, "Error: nobody catches me"));
    assert!(!said(&plugin.lines, "caught in time"));
}

// ---- permissions ------------------------------------------------------------------------------

#[test]
fn capabilities_are_absent_without_their_permission() {
    let (plugin, result) = load(
        r#"
        export default {
          setup(host) {
            host.log.info("webview:", typeof host.webview, "config:", typeof host.config, "ui:", typeof host.ui);
          },
        };
        "#,
        &[],
    );
    result.unwrap();
    assert!(said(
        &plugin.lines,
        "webview: undefined config: undefined ui: undefined"
    ));

    let (plugin, result) = load(
        r#"
        export default {
          setup(host) {
            host.log.info("webview:", typeof host.webview, "config:", typeof host.config, "ui:", typeof host.ui);
          },
        };
        "#,
        &["webview", "config", "ui"],
    );
    result.unwrap();
    assert!(said(
        &plugin.lines,
        "webview: object config: object ui: object"
    ));
}

#[test]
fn the_script_cannot_reach_the_outside_world_on_its_own() {
    let (plugin, result) = load(
        r#"
        export default {
          setup(host) {
            const names = ["require", "process", "fetch", "XMLHttpRequest", "Deno", "Bun", "readFile", "window", "document"];
            host.log.info("globals:", names.filter((name) => typeof globalThis[name] !== "undefined").join(",") || "none");
          },
        };
        "#,
        &["webview", "config"],
    );
    result.unwrap();
    assert!(said(&plugin.lines, "globals: none"));
}

// ---- host.config -------------------------------------------------------------------------------

#[test]
fn config_round_trips_through_a_file_in_the_data_directory() {
    let (plugin, result) = load(
        r#"
        export default {
          async setup(host) {
            host.log.info("first:", String(await host.config.get("answer")));
            await host.config.set("answer", { n: 42, list: [1, 2] });
            await host.config.set("name", "orbit");
            host.log.info("second:", JSON.stringify(await host.config.get("answer")));
            await host.config.set("name", undefined);
            host.log.info("third:", String(await host.config.get("name")));
          },
        };
        "#,
        &["config"],
    );
    result.unwrap();
    assert!(said(&plugin.lines, "first: undefined"));
    assert!(said(&plugin.lines, "second: {\"list\":[1,2],\"n\":42}"));
    assert!(said(&plugin.lines, "third: undefined"));
    let file = std::fs::read_to_string(plugin.dir.0.join("data").join("config.json")).unwrap();
    assert_eq!(file, "{\"answer\":{\"list\":[1,2],\"n\":42}}");
}

#[test]
fn a_broken_config_file_rejects_instead_of_being_overwritten() {
    let dir = Dir::new("config-broken");
    dir.write("main.js", r#"export default { async setup(host) { try { await host.config.set("k", 1); host.log.info("set worked"); } catch (error) { host.log.info("rejected:", error.message); } } };"#);
    dir.write("data/config.json", "not json");
    let lines: Lines = Arc::default();
    let handle = spawn(plugin_spec(
        &dir,
        "com.test.broken",
        &["config"],
        None,
        &lines,
    ))
    .unwrap();
    handle.load().unwrap();
    assert!(said(&lines, "rejected:"));
    assert_eq!(
        std::fs::read_to_string(dir.0.join("data/config.json")).unwrap(),
        "not json"
    );
}

// ---- host.webview, against a fake bridge -----------------------------------------------------

static CALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static SUBSCRIBER: Mutex<Option<(usize, usize)>> = Mutex::new(None);

fn note(text: String) {
    CALLS.lock().unwrap().push(text);
}

fn text(pointer: *const u8, len: usize) -> String {
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(pointer, len) }).into_owned()
}

unsafe extern "system" fn bridge_is_ready() -> i32 {
    1
}

unsafe extern "system" fn bridge_add_script(
    key: *const u8,
    key_len: usize,
    script: *const u8,
    script_len: usize,
) -> i64 {
    note(format!(
        "add {} {}",
        text(key, key_len),
        text(script, script_len)
    ));
    11
}

unsafe extern "system" fn bridge_remove_script(id: i64) -> i32 {
    note(format!("remove {id}"));
    0
}

unsafe extern "system" fn bridge_evaluate(
    expression: *const u8,
    len: usize,
    out: Option<MessageCallback>,
    user: *mut c_void,
) -> i32 {
    let expression = text(expression, len);
    note(format!("evaluate {expression}"));
    if expression == "fail" {
        return parsec_orbit_plugin_sdk_std::ERR_NOT_READY;
    }
    let reply = format!(
        "{{\"echo\":{}}}",
        serde_json::to_string(&expression).unwrap()
    );
    unsafe { out.unwrap()(user, reply.as_ptr(), reply.len()) };
    0
}

unsafe extern "system" fn bridge_subscribe(
    channel: *const u8,
    len: usize,
    callback: Option<MessageCallback>,
    user: *mut c_void,
) -> i64 {
    note(format!("subscribe {}", text(channel, len)));
    *SUBSCRIBER.lock().unwrap() = Some((callback.unwrap() as usize, user as usize));
    5
}

unsafe extern "system" fn bridge_unsubscribe(id: i64) -> i32 {
    note(format!("unsubscribe {id}"));
    0
}

unsafe extern "system" fn bridge_post(
    channel: *const u8,
    channel_len: usize,
    payload: *const u8,
    len: usize,
) -> i32 {
    note(format!(
        "post {} {}",
        text(channel, channel_len),
        text(payload, len)
    ));
    0
}

static BRIDGE: ParsecOrbitWebViewV1 = ParsecOrbitWebViewV1 {
    header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitWebViewV1>(), 1, 0),
    is_ready: Some(bridge_is_ready),
    add_script: Some(bridge_add_script),
    remove_script: Some(bridge_remove_script),
    evaluate: Some(bridge_evaluate),
    subscribe: Some(bridge_subscribe),
    unsubscribe: Some(bridge_unsubscribe),
    post: Some(bridge_post),
};

fn bridge_client() -> WebViewClient {
    let host = MockHost::new("com.parsec-orbit.js-runtime");
    host.provide("parsec-orbit.webview", &BRIDGE.header);
    WebViewClient::new(&host.context()).expect("the fake bridge")
}

/// All tests that use the fake bridge share its globals: one at a time.
static BRIDGE_TESTS: Mutex<()> = Mutex::new(());

#[test]
fn the_webview_api_maps_onto_the_bridge() {
    let _one_at_a_time = BRIDGE_TESTS.lock().unwrap_or_else(|p| p.into_inner());
    CALLS.lock().unwrap().clear();
    let (plugin, result) = load_with(
        r#"
        export default {
          async setup(host) {
            await host.webview.ready;
            host.log.info("ready");
            const handle = await host.webview.inject("window.x = 1;", { key: "main" });
            host.log.info("injected", typeof handle.id, typeof handle.remove);
            await host.webview.inject("window.y = 2;");
            await handle.remove();
            const result = await host.webview.evaluate("1 + 1");
            host.log.info("evaluated", JSON.stringify(result));
            try { await host.webview.evaluate("fail"); } catch (error) { host.log.info("evaluate failed:", error.message); }
            await host.webview.post("greeting", { hello: [1, 2] });
            host.log.info("posted");
          },
        };
        "#,
        &["webview"],
        Some(bridge_client()),
    );
    result.unwrap();
    assert!(said(&plugin.lines, "ready"));
    assert!(said(&plugin.lines, "injected number function"));
    assert!(said(&plugin.lines, "evaluated {\"echo\":\"1 + 1\"}"));
    assert!(said(
        &plugin.lines,
        "evaluate failed: the page is not ready"
    ));
    assert!(said(&plugin.lines, "posted"));

    let calls = CALLS.lock().unwrap().clone();
    // The key is the plugin's, then the bridge client adds the runtime's id.
    assert!(
        calls.contains(
            &"add com.parsec-orbit.js-runtime/com.test.plugin/main window.x = 1;".to_string()
        ),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("add com.parsec-orbit.js-runtime/com.test.plugin/inline-")),
        "{calls:?}"
    );
    assert!(calls.contains(&"remove 11".to_string()));
    assert!(calls.contains(&"post greeting {\"hello\":[1,2]}".to_string()));
}

#[test]
fn inject_file_reads_inside_the_plugin_directory_only() {
    let _one_at_a_time = BRIDGE_TESTS.lock().unwrap_or_else(|p| p.into_inner());
    CALLS.lock().unwrap().clear();
    let dir = Dir::new("inject-file");
    dir.write("ui.js", "window.fromFile = true;");
    dir.write("sub/more.js", "window.more = 1;");
    std::fs::write(dir.0.parent().unwrap().join("outside-secret.js"), "secret").unwrap();
    dir.write(
        "main.js",
        r#"
        export default {
          async setup(host) {
            await host.webview.injectFile("ui.js");
            await host.webview.injectFile("sub/more.js", { key: "more" });
            for (const path of ["../outside-secret.js", "missing.js", "..\\outside-secret.js"]) {
              try { await host.webview.injectFile(path); host.log.info("read", path); }
              catch (error) { host.log.info("refused", path); }
            }
          },
        };
        "#,
    );
    let lines: Lines = Arc::default();
    let handle = spawn(plugin_spec(
        &dir,
        "com.test.files",
        &["webview"],
        Some(bridge_client()),
        &lines,
    ))
    .unwrap();
    handle.load().unwrap();
    let _ = std::fs::remove_file(dir.0.parent().unwrap().join("outside-secret.js"));

    assert!(said(&lines, "refused ../outside-secret.js"));
    assert!(said(&lines, "refused missing.js"));
    assert!(!said(&lines, "read "));
    let calls = CALLS.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|c| c.ends_with("com.test.files/ui.js window.fromFile = true;")),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.ends_with("com.test.files/more window.more = 1;")),
        "{calls:?}"
    );
    assert!(!calls.iter().any(|c| c.contains("secret")));
}

#[test]
fn page_messages_reach_the_handlers_and_unsubscribing_stops_them() {
    let _one_at_a_time = BRIDGE_TESTS.lock().unwrap_or_else(|p| p.into_inner());
    CALLS.lock().unwrap().clear();
    *SUBSCRIBER.lock().unwrap() = None;
    let (plugin, result) = load_with(
        r#"
        export default {
          setup(host) {
            const off = host.webview.on("news", (payload) => host.log.info("first got", JSON.stringify(payload)));
            host.webview.on("news", (payload) => host.log.info("second got", payload.n));
            globalThis.off = off;
            host.setTimeout(() => off(), 200);
          },
        };
        "#,
        &["webview"],
        Some(bridge_client()),
    );
    result.unwrap();

    let (callback, user) = SUBSCRIBER.lock().unwrap().expect("subscribed");
    // One bridge subscription serves both handlers.
    assert_eq!(
        CALLS
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("subscribe"))
            .count(),
        1
    );
    let send = |json: &str| {
        let callback: MessageCallback = unsafe { std::mem::transmute(callback) };
        unsafe { callback(user as *mut c_void, json.as_ptr(), json.len()) };
    };
    send("{\"n\":7}");
    wait_for("both handlers", &plugin.lines, "second got 7");
    assert!(said(&plugin.lines, "first got {\"n\":7}"));

    // After the first handler is removed only the second one hears.
    std::thread::sleep(Duration::from_millis(400));
    send("{\"n\":8}");
    wait_for("the remaining handler", &plugin.lines, "second got 8");
    assert!(!said(&plugin.lines, "first got {\"n\":8}"));
    assert!(
        !CALLS
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("unsubscribe")),
        "the channel is still in use"
    );

    // Stopping the context drops the bridge subscription.
    plugin.handle.stop();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !CALLS.lock().unwrap().iter().any(|c| c == "unsubscribe 5") {
        assert!(
            Instant::now() < deadline,
            "no unsubscribe: {:?}",
            CALLS.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn without_the_bridge_the_webview_calls_reject_with_a_hint() {
    let (plugin, result) = load(
        r#"
        export default {
          async setup(host) {
            try { await host.webview.inject("x"); } catch (error) { host.log.info("inject:", error.message); }
            try { await host.webview.evaluate("x"); } catch (error) { host.log.info("evaluate:", error.message); }
            try { await host.webview.ready; } catch (error) { host.log.info("ready:", error.message); }
            try { host.webview.on("c", () => {}); } catch (error) { host.log.info("on:", error.message); }
          },
        };
        "#,
        &["webview"],
    );
    result.unwrap();
    for name in ["inject", "evaluate", "ready", "on"] {
        assert!(
            said(
                &plugin.lines,
                &format!("{name}: parsec-orbit.webview is not available")
            ),
            "{name}: {:?}",
            plugin.lines.lock().unwrap()
        );
    }
}

// ---- native contexts (parsec-orbit.js) ---------------------------------------------------------

#[test]
fn a_native_context_evaluates_scripts_and_keeps_its_globals() {
    let (context, _lines) = native_context();
    assert_eq!(context.eval("1 + 2").as_deref(), Ok("3"));
    assert_eq!(
        context.eval("globalThis.kept = 'x'; kept + 'y'").as_deref(),
        Ok("\"xy\"")
    );
    assert_eq!(context.eval("kept").as_deref(), Ok("\"x\""));
    assert_eq!(
        context.eval("({ list: [1, 2, { a: null }] })").as_deref(),
        Ok("{\"list\":[1,2,{\"a\":null}]}")
    );
    assert_eq!(context.eval("undefined").as_deref(), Ok("null"));
    assert_eq!(
        context.eval("typeof host").as_deref(),
        Ok("\"undefined\""),
        "no host object here"
    );
}

#[test]
fn a_native_context_awaits_promises_and_reports_errors() {
    let (context, lines) = native_context();
    assert_eq!(
        context
            .eval("new Promise((resolve) => setTimeout(() => resolve('later'), 30))")
            .as_deref(),
        Ok("\"later\"")
    );
    assert_eq!(context.eval("Promise.resolve(7)").as_deref(), Ok("7"));

    let error = context.eval("throw new RangeError('nope')").unwrap_err();
    assert_eq!(error.code, ERR_EXCEPTION);
    assert!(
        error.message.contains("RangeError: nope"),
        "{}",
        error.message
    );
    let error = context
        .eval("Promise.reject(new Error('rejected'))")
        .unwrap_err();
    assert_eq!(error.code, ERR_EXCEPTION);
    assert!(error.message.contains("rejected"));
    let error = context.eval("syntax error here").unwrap_err();
    assert_eq!(error.code, ERR_EXCEPTION);

    // The context is still fine, and console goes to the log.
    assert_eq!(context.eval("console.log('seen'); 1").as_deref(), Ok("1"));
    assert!(said(&lines, "seen"));
}

#[test]
fn a_runaway_loop_ends_with_an_error_instead_of_hanging_the_context() {
    let (context, _) = native_context();
    let started = Instant::now();
    let error = context.eval("while (true) {}").unwrap_err();
    assert_eq!(error.code, ERR_EXCEPTION);
    assert!(started.elapsed() < Duration::from_secs(60));
    assert_eq!(context.eval("1").as_deref(), Ok("1"));
}

#[allow(dead_code)]
fn unused(_: &Path) {}

// ---- host.config through the config service ------------------------------------------------

static CONFIG_STORE: Mutex<Vec<(String, String, String)>> = Mutex::new(Vec::new());

unsafe extern "system" fn svc_get(
    owner: *const u8,
    owner_len: usize,
    key: *const u8,
    key_len: usize,
    out: Option<MessageCallback>,
    user: *mut c_void,
) -> i32 {
    let (owner, key) = (text(owner, owner_len), text(key, key_len));
    let store = CONFIG_STORE.lock().unwrap();
    match store.iter().find(|(o, k, _)| *o == owner && *k == key) {
        Some((_, _, json)) => {
            unsafe { out.unwrap()(user, json.as_ptr(), json.len()) };
            0
        }
        None => parsec_orbit_plugin_sdk_std::ERR_NOT_FOUND,
    }
}

unsafe extern "system" fn svc_set(
    owner: *const u8,
    owner_len: usize,
    key: *const u8,
    key_len: usize,
    json: *const u8,
    json_len: usize,
) -> i32 {
    let entry = (
        text(owner, owner_len),
        text(key, key_len),
        text(json, json_len),
    );
    let mut store = CONFIG_STORE.lock().unwrap();
    store.retain(|(o, k, _)| !(*o == entry.0 && *k == entry.1));
    store.push(entry);
    0
}

unsafe extern "system" fn svc_remove(
    owner: *const u8,
    owner_len: usize,
    key: *const u8,
    key_len: usize,
) -> i32 {
    let (owner, key) = (text(owner, owner_len), text(key, key_len));
    let mut store = CONFIG_STORE.lock().unwrap();
    let before = store.len();
    store.retain(|(o, k, _)| !(*o == owner && *k == key));
    if store.len() < before {
        0
    } else {
        parsec_orbit_plugin_sdk_std::ERR_NOT_FOUND
    }
}

static CONFIG_SERVICE: parsec_orbit_plugin_sdk_std::ParsecOrbitConfigV1 =
    parsec_orbit_plugin_sdk_std::ParsecOrbitConfigV1 {
        header: ParsecOrbitInterfaceHeader::new(
            std::mem::size_of::<parsec_orbit_plugin_sdk_std::ParsecOrbitConfigV1>(),
            1,
            0,
        ),
        get: Some(svc_get),
        set: Some(svc_set),
        remove: Some(svc_remove),
        keys: None,
    };

#[test]
fn host_config_uses_the_config_service_when_it_is_there() {
    use parsec_orbit_plugin_sdk_std::services::ConfigClient;
    let host = MockHost::new("com.parsec-orbit.js-runtime");
    host.provide("parsec-orbit.config", &CONFIG_SERVICE.header);
    let client = ConfigClient::new(&host.context())
        .unwrap()
        .for_owner("com.test.plugin");

    let dir = Dir::new("config-service");
    dir.write(
        "main.js",
        r#"export default { async setup(host) {
          host.log.info("before:", String(await host.config.get("answer")));
          await host.config.set("answer", { n: 42 });
          host.log.info("after:", JSON.stringify(await host.config.get("answer")));
          await host.config.set("answer", undefined);
          host.log.info("removed:", String(await host.config.get("answer")));
        } };"#,
    );
    let lines: Lines = Arc::default();
    let mut spec = plugin_spec(&dir, "com.test.plugin", &["config"], None, &lines);
    spec.config = Some(client);
    let handle = spawn(spec).unwrap();
    handle.load().unwrap();

    assert!(said(&lines, "before: undefined"));
    assert!(said(&lines, "after: {\"n\":42}"));
    assert!(said(&lines, "removed: undefined"));
    assert!(
        !dir.0.join("data").join("config.json").exists(),
        "the file fallback is not used"
    );
}

static TEST_UI_SHOW: Mutex<bool> = Mutex::new(false);
static TEST_UI_HIDE: Mutex<bool> = Mutex::new(false);
static TEST_UI_SECTIONS: Mutex<Vec<String>> = Mutex::new(Vec::new());

unsafe extern "system" fn test_ui_show() -> i32 {
    *TEST_UI_SHOW.lock().unwrap() = true;
    0
}

unsafe extern "system" fn test_ui_hide() -> i32 {
    *TEST_UI_HIDE.lock().unwrap() = true;
    0
}

unsafe extern "system" fn test_ui_is_visible() -> i32 {
    1
}

unsafe extern "system" fn test_ui_register_section(schema: *const u8, len: usize) -> i32 {
    let slice = unsafe { core::slice::from_raw_parts(schema, len) };
    TEST_UI_SECTIONS
        .lock()
        .unwrap()
        .push(String::from_utf8_lossy(slice).into_owned());
    0
}

unsafe extern "system" fn test_ui_unregister_section(_: *const u8, _: usize) -> i32 {
    0
}

unsafe extern "system" fn test_ui_update_state(
    _: *const u8,
    _: usize,
    _: *const u8,
    _: usize,
) -> i32 {
    0
}

static TEST_UI_SERVICE: parsec_orbit_plugin_sdk_std::ParsecOrbitUiV1 =
    parsec_orbit_plugin_sdk_std::ParsecOrbitUiV1 {
        header: parsec_orbit_plugin_sdk_std::ParsecOrbitInterfaceHeader::new(
            std::mem::size_of::<parsec_orbit_plugin_sdk_std::ParsecOrbitUiV1>(),
            1,
            0,
        ),
        show: Some(test_ui_show),
        hide: Some(test_ui_hide),
        is_visible: Some(test_ui_is_visible),
        register_section: Some(test_ui_register_section),
        unregister_section: Some(test_ui_unregister_section),
        update_state: Some(test_ui_update_state),
        on_action: None,
        off_action: None,
    };

#[test]
fn host_ui_uses_the_ui_service_when_it_is_there() {
    use parsec_orbit_plugin_sdk_std::services::UiClient;
    let host = MockHost::new("com.parsec-orbit.js-runtime");
    host.provide("parsec-orbit.ui", &TEST_UI_SERVICE.header);
    let client = UiClient::new(&host.context()).unwrap();

    let dir = Dir::new("ui-service");
    dir.write(
        "main.js",
        r#"export default { async setup(host) {
          host.log.info("visible:", await host.ui.isVisible());
          await host.ui.show();
          await host.ui.registerSection({ id: "my-section", title: "My Section", items: [] });
          await host.ui.hide();
        } };"#,
    );
    let lines: Lines = Arc::default();
    let mut spec = plugin_spec(&dir, "com.test.plugin", &["ui"], None, &lines);
    spec.ui = Some(client);
    let handle = spawn(spec).unwrap();
    handle.load().unwrap();

    assert!(said(&lines, "visible: true"));
    assert!(*TEST_UI_SHOW.lock().unwrap());
    assert!(*TEST_UI_HIDE.lock().unwrap());
    assert!(TEST_UI_SECTIONS
        .lock()
        .unwrap()
        .iter()
        .any(|s| s.contains("my-section")));
}

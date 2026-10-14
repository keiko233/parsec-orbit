//! The typed bindings of the core services, against fake providers.

use std::ffi::c_void;
use std::sync::Mutex;

use parsec_orbit_plugin_sdk_std::services::{ClientClient, Js, JsClient, WebView, WebViewClient};
use parsec_orbit_plugin_sdk_std::testing::MockHost;
use parsec_orbit_plugin_sdk_std::*;

static CALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn note(text: String) {
    CALLS.lock().unwrap().push(text);
}

fn text(pointer: *const u8, len: usize) -> String {
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(pointer, len) }).into_owned()
}

unsafe extern "system" fn is_ready() -> i32 {
    1
}

unsafe extern "system" fn add_script(
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
    if text(script, script_len).is_empty() {
        ERR_INVALID as i64
    } else {
        7
    }
}

unsafe extern "system" fn remove_script(id: i64) -> i32 {
    note(format!("remove {id}"));
    if id == 7 {
        OK
    } else {
        ERR_NOT_FOUND
    }
}

unsafe extern "system" fn evaluate(
    expression: *const u8,
    len: usize,
    out: Option<MessageCallback>,
    user: *mut c_void,
) -> i32 {
    let expression = text(expression, len);
    if expression == "throw" {
        return ERR_EXCEPTION;
    }
    let reply = format!("\"{expression}!\"");
    if let Some(out) = out {
        unsafe { out(user, reply.as_ptr(), reply.len()) };
    }
    OK
}

unsafe extern "system" fn subscribe(
    channel: *const u8,
    len: usize,
    callback: Option<MessageCallback>,
    user: *mut c_void,
) -> i64 {
    note(format!("subscribe {}", text(channel, len)));
    let message = "{\"n\":1}";
    unsafe { callback.unwrap()(user, message.as_ptr(), message.len()) };
    9
}

unsafe extern "system" fn unsubscribe(id: i64) -> i32 {
    note(format!("unsubscribe {id}"));
    OK
}

unsafe extern "system" fn post(
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
    OK
}

static WEBVIEW: ParsecOrbitWebViewV1 = ParsecOrbitWebViewV1 {
    header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitWebViewV1>(), 1, 0),
    is_ready: Some(is_ready),
    add_script: Some(add_script),
    remove_script: Some(remove_script),
    evaluate: Some(evaluate),
    subscribe: Some(subscribe),
    unsubscribe: Some(unsubscribe),
    post: Some(post),
};

unsafe extern "system" fn client_info(out: Option<MessageCallback>, user: *mut c_void) -> i32 {
    let Some(out) = out else {
        return ERR_INVALID;
    };
    let json = r#"{"platform":"macos","arch":"arm64","status":"not-verified"}"#;
    unsafe { out(user, json.as_ptr(), json.len()) };
    OK
}

static CLIENT: ParsecOrbitClientV1 = ParsecOrbitClientV1 {
    header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitClientV1>(), 1, 0),
    info: Some(client_info),
};

#[test]
fn the_clients_need_their_services() {
    let host = MockHost::new("com.example.js-less");
    assert!(WebViewClient::new(&host.context()).is_none());
    assert!(JsClient::new(&host.context()).is_none());
    assert!(ClientClient::new(&host.context()).is_none());
}

#[test]
fn the_client_binding_rejects_truncated_and_unknown_tables() {
    static TRUNCATED: ParsecOrbitClientV1 = ParsecOrbitClientV1 {
        header: ParsecOrbitInterfaceHeader::new(8, 1, 0),
        info: Some(client_info),
    };
    static UNKNOWN_MAJOR: ParsecOrbitClientV1 = ParsecOrbitClientV1 {
        header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitClientV1>(), 2, 0),
        info: Some(client_info),
    };

    let host = MockHost::new("com.example.client-truncated");
    host.provide("parsec-orbit.client", &TRUNCATED.header);
    assert!(ClientClient::new(&host.context()).is_none());

    let host = MockHost::new("com.example.client-unknown");
    host.provide("parsec-orbit.client", &UNKNOWN_MAJOR.header);
    assert!(ClientClient::new(&host.context()).is_none());
}

#[test]
fn the_client_binding_receives_json_from_the_callback() {
    let host = MockHost::new("com.example.client");
    host.provide("parsec-orbit.client", &CLIENT.header);
    let client = ClientClient::new(&host.context()).expect("client service");

    let mut seen = String::new();
    client.info(|json| seen = json.to_owned()).unwrap();
    assert_eq!(
        seen,
        r#"{"platform":"macos","arch":"arm64","status":"not-verified"}"#
    );
}

#[test]
fn a_missing_client_callback_is_not_ready() {
    static MISSING_INFO: ParsecOrbitClientV1 = ParsecOrbitClientV1 {
        header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitClientV1>(), 1, 0),
        info: None,
    };
    let host = MockHost::new("com.example.client-no-info");
    host.provide("parsec-orbit.client", &MISSING_INFO.header);
    let client = ClientClient::new(&host.context()).expect("client service table");
    assert_eq!(client.info(|_| {}).unwrap_err().code(), ERR_NOT_READY);
}

#[test]
fn the_webview_client_calls_through_the_table() {
    let host = MockHost::new("com.example.ui");
    host.provide("parsec-orbit.webview", &WEBVIEW.header);
    let webview = WebViewClient::new(&host.context()).expect("service");
    assert!(webview.is_ready());

    // The key gets the plugin id in front: registry keys are global.
    assert_eq!(webview.add_script("main", "console.log(1)").unwrap(), 7);
    assert_eq!(
        webview.add_script("main", "").unwrap_err().code(),
        ERR_INVALID
    );
    assert_eq!(
        webview
            .add_script(&"k".repeat(300), "x")
            .unwrap_err()
            .code(),
        ERR_INVALID
    );
    webview.remove_script(7).unwrap();
    assert_eq!(webview.remove_script(8).unwrap_err().code(), ERR_NOT_FOUND);

    let mut seen = Vec::new();
    webview
        .evaluate("1+1", |json| seen.push(json.to_string()))
        .unwrap();
    assert_eq!(seen, ["\"1+1!\""]);
    assert_eq!(
        webview.evaluate("throw", |_| {}).unwrap_err().code(),
        ERR_EXCEPTION
    );

    webview.post("greeting", "{\"hello\":true}").unwrap();

    static GOT: Mutex<Vec<String>> = Mutex::new(Vec::new());
    unsafe extern "system" fn on_message(_user: *mut c_void, message: *const u8, len: usize) {
        GOT.lock().unwrap().push(text(message, len));
    }
    let subscription =
        unsafe { webview.subscribe("bandwidth:applied", on_message, std::ptr::null_mut()) }
            .unwrap();
    assert_eq!(subscription, 9);
    webview.unsubscribe(subscription).unwrap();
    assert_eq!(*GOT.lock().unwrap(), ["{\"n\":1}"]);

    let calls = CALLS.lock().unwrap();
    assert!(calls.contains(&"add com.example.ui/main console.log(1)".to_string()));
    assert!(calls.contains(&"post greeting {\"hello\":true}".to_string()));
    assert!(calls.contains(&"subscribe bandwidth:applied".to_string()));
    assert!(calls.contains(&"unsubscribe 9".to_string()));
}

fn empty_table(major: u16) -> ParsecOrbitWebViewV1 {
    ParsecOrbitWebViewV1 {
        header: ParsecOrbitInterfaceHeader::new(
            std::mem::size_of::<ParsecOrbitWebViewV1>(),
            major,
            0,
        ),
        is_ready: None,
        add_script: None,
        remove_script: None,
        evaluate: None,
        subscribe: None,
        unsubscribe: None,
        post: None,
    }
}

#[test]
fn a_table_of_another_major_version_is_not_served() {
    static V2: ParsecOrbitWebViewV1 = ParsecOrbitWebViewV1 {
        header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitWebViewV1>(), 2, 0),
        is_ready: None,
        add_script: None,
        remove_script: None,
        evaluate: None,
        subscribe: None,
        unsubscribe: None,
        post: None,
    };
    let _ = empty_table(2);
    let host = MockHost::new("com.example.ui-v2");
    host.provide("parsec-orbit.webview", &V2.header);
    assert!(host.context().service::<WebView>().is_none());
}

#[test]
fn missing_functions_are_reported_as_not_ready() {
    static EMPTY: ParsecOrbitWebViewV1 = ParsecOrbitWebViewV1 {
        header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitWebViewV1>(), 1, 0),
        is_ready: None,
        add_script: None,
        remove_script: None,
        evaluate: None,
        subscribe: None,
        unsubscribe: None,
        post: None,
    };
    let host = MockHost::new("com.example.ui-empty");
    host.provide("parsec-orbit.webview", &EMPTY.header);
    let webview = WebViewClient::new(&host.context()).unwrap();
    assert!(!webview.is_ready());
    assert_eq!(
        webview.add_script("k", "x").unwrap_err().code(),
        ERR_NOT_READY
    );
    assert_eq!(webview.post("c", "{}").unwrap_err().code(), ERR_NOT_READY);
}

unsafe extern "system" fn create_context(name: *const u8, len: usize) -> i64 {
    if text(name, len) == "taken" {
        ERR_EXISTS as i64
    } else {
        3
    }
}

unsafe extern "system" fn eval(
    context: i64,
    source: *const u8,
    len: usize,
    out: Option<MessageCallback>,
    user: *mut c_void,
) -> i32 {
    if context != 3 {
        return ERR_NOT_FOUND;
    }
    let reply = text(source, len).len().to_string();
    unsafe { out.unwrap()(user, reply.as_ptr(), reply.len()) };
    OK
}

unsafe extern "system" fn destroy_context(context: i64) -> i32 {
    if context == 3 {
        OK
    } else {
        ERR_NOT_FOUND
    }
}

static JS: ParsecOrbitJsV1 = ParsecOrbitJsV1 {
    header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitJsV1>(), 1, 0),
    create_context: Some(create_context),
    eval: Some(eval),
    destroy_context: Some(destroy_context),
};

#[test]
fn the_js_client_creates_evaluates_and_destroys_contexts() {
    let host = MockHost::new("com.example.native-js");
    host.provide("parsec-orbit.js", &JS.header);
    assert!(host.context().service::<Js>().is_some());
    let js = JsClient::new(&host.context()).unwrap();

    assert_eq!(js.create_context("taken").unwrap_err().code(), ERR_EXISTS);
    let context = js.create_context("mine").unwrap();
    let mut seen = String::new();
    js.eval(context, "1 + 2", |json| seen = json.to_string())
        .unwrap();
    assert_eq!(seen, "5", "the fake answers with the source length");
    assert_eq!(js.eval(99, "x", |_| {}).unwrap_err().code(), ERR_NOT_FOUND);
    js.destroy_context(context).unwrap();
    assert_eq!(js.destroy_context(99).unwrap_err().code(), ERR_NOT_FOUND);
}

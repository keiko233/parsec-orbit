//! The bridge session against a fake DevTools endpoint: attach, install,
//! evaluate, messages in both directions, reattach.

mod common;

use std::ffi::c_void;
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use parsec_orbit_plugin_sdk_std::{ERR_EXCEPTION, ERR_NOT_FOUND, ERR_NOT_READY, ERR_TIMEOUT};
use parsec_orbit_plugin_webview_bridge::bridge::{Bridge, ORBIT_SCRIPT};
use serde_json::{json, Value};

/// A page that answers everything the bridge sends. Evaluating one of the
/// magic expressions makes it do something special.
fn page(stream: &mut TcpStream, command: &Value, session: usize) -> bool {
    let id = command["id"].as_u64().unwrap();
    let method = command["method"].as_str().unwrap();
    let reply = |stream: &mut TcpStream, result: Value| {
        write_text(stream, &json!({"id": id, "result": result}).to_string());
    };
    match method {
        "Page.addScriptToEvaluateOnNewDocument" => {
            reply(stream, json!({"identifier": format!("reg-{session}-{id}")}));
        }
        "Runtime.evaluate" => {
            let expression = command["params"]["expression"].as_str().unwrap();
            match expression {
                "1+1" => reply(stream, json!({"result": {"type": "number", "value": 2}})),
                "an object" => reply(
                    stream,
                    json!({"result": {"type": "object", "value": {"list": [1, "two"]}}}),
                ),
                "nothing" => reply(stream, json!({"result": {"type": "undefined"}})),
                "throw" => reply(
                    stream,
                    json!({"result": {"type": "object"}, "exceptionDetails": {"text": "Uncaught", "exception": {"description": "Error: boom"}}}),
                ),
                "silence" => {} // never answers
                "drop" => {
                    write_frame(stream, 0x8, true, b"");
                    return false;
                }
                "page message" => {
                    // The page calls the binding, then the evaluation returns.
                    write_text(
                        stream,
                        &json!({"method": "Runtime.bindingCalled", "params": {
                            "name": "__orbitSend",
                            "payload": json!({"channel": "news", "payload": {"n": 7}}).to_string(),
                            "executionContextId": 1}})
                        .to_string(),
                    );
                    write_text(
                        stream,
                        &json!({"method": "Runtime.bindingCalled", "params": {
                            "name": "__orbitSend",
                            "payload": json!({"channel": "elsewhere"}).to_string()}})
                        .to_string(),
                    );
                    write_text(
                        stream,
                        &json!({"method": "Runtime.bindingCalled", "params": {
                            "name": "someone-elses-binding", "payload": "{}"}})
                        .to_string(),
                    );
                    reply(
                        stream,
                        json!({"result": {"type": "string", "value": "sent"}}),
                    );
                }
                _ => reply(
                    stream,
                    json!({"result": {"type": "string", "value": "installed"}}),
                ),
            }
        }
        // Runtime.enable, Runtime.addBinding, Page.removeScript...
        _ => reply(stream, json!({})),
    }
    true
}

struct Fixture {
    bridge: &'static Bridge,
    state: std::sync::Arc<State>,
    logs: mpsc::Receiver<String>,
}

fn start_bridge(
    on_command: impl Fn(&mut TcpStream, &Value, usize) -> bool + Send + Sync + 'static,
) -> Fixture {
    let (port, state) = start(chrome_listing, on_command);
    start_bridge_at(port, state, Bridge::new())
}

fn start_bridge_at(port: u16, state: std::sync::Arc<State>, bridge: Bridge) -> Fixture {
    let bridge: &'static Bridge = Box::leak(Box::new(bridge));
    let (tx, logs) = mpsc::channel();
    let tx = Mutex::new(tx);
    bridge
        .start(
            port,
            "test-bridge",
            Box::new(move |level, message| {
                let _ = tx.lock().unwrap().send(format!("{level:?}: {message}"));
            }),
        )
        .unwrap();
    Fixture {
        bridge,
        state,
        logs,
    }
}

fn wait_until(what: &str, seconds: u64, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

fn methods(state: &State) -> Vec<String> {
    state
        .commands
        .lock()
        .unwrap()
        .iter()
        .map(|command| command["method"].as_str().unwrap().to_string())
        .collect()
}

fn expressions(state: &State) -> Vec<String> {
    state
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|command| command["method"] == "Runtime.evaluate")
        .map(|command| {
            command["params"]["expression"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

fn ready(fixture: &Fixture) {
    wait_until("the page to attach", 10, || fixture.bridge.is_ready());
}

#[test]
fn the_session_enables_the_binding_and_installs_the_page_helper_first() {
    let fixture = start_bridge(page);
    ready(&fixture);

    let commands = fixture.state.commands.lock().unwrap().clone();
    let order: Vec<&str> = commands
        .iter()
        .map(|command| command["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        [
            "Runtime.enable",
            "Runtime.addBinding",
            "Page.addScriptToEvaluateOnNewDocument",
            "Runtime.evaluate"
        ]
    );
    assert_eq!(commands[1]["params"]["name"], "__orbitSend");
    assert_eq!(commands[2]["params"]["source"], ORBIT_SCRIPT);
    assert_eq!(commands[3]["params"]["expression"], ORBIT_SCRIPT);
}

#[test]
fn scripts_registered_before_the_page_exists_are_installed_on_attach() {
    // The endpoint is not up yet: reserve a port by starting nothing on it,
    // register, and only then bring the fake page up on that port.
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let bridge: &'static Bridge = Box::leak(Box::new(Bridge::new()));
    let id = bridge.add_script("early", "window.early = 1;");
    assert!(id > 0);
    assert!(!bridge.is_ready());

    let (tx, _logs) = mpsc::channel::<String>();
    let tx = Mutex::new(tx);
    bridge
        .start(
            port,
            "test-bridge",
            Box::new(move |level, message| {
                let _ = tx.lock().unwrap().send(format!("{level:?}: {message}"));
            }),
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    assert!(!bridge.is_ready());

    let state = common::start_on(port, chrome_listing, page);
    wait_until("the page to attach", 10, || bridge.is_ready());
    assert!(expressions(&state).contains(&"window.early = 1;".to_string()));
    assert!(methods(&state)
        .iter()
        .any(|m| m == "Page.addScriptToEvaluateOnNewDocument"));
}

#[test]
fn a_script_added_while_attached_is_installed_and_replacing_it_removes_the_old_one() {
    let fixture = start_bridge(page);
    ready(&fixture);

    let id = fixture.bridge.add_script("ui", "window.version = 1;");
    wait_until("the script to be installed", 5, || {
        expressions(&fixture.state).contains(&"window.version = 1;".to_string())
    });

    // Same key: same id, the old registration goes away, the new one comes.
    assert_eq!(fixture.bridge.add_script("ui", "window.version = 2;"), id);
    wait_until("the replacement to be installed", 5, || {
        expressions(&fixture.state).contains(&"window.version = 2;".to_string())
    });
    let commands = fixture.state.commands.lock().unwrap().clone();
    let removal = commands
        .iter()
        .find(|command| command["method"] == "Page.removeScriptToEvaluateOnNewDocument")
        .expect("the replaced registration is removed");
    assert!(removal["params"]["identifier"]
        .as_str()
        .unwrap()
        .starts_with("reg-0-"));
    drop(commands);

    assert_eq!(fixture.bridge.remove_script(id), 0);
    wait_until("the removal", 5, || {
        methods(&fixture.state)
            .iter()
            .filter(|m| *m == "Page.removeScriptToEvaluateOnNewDocument")
            .count()
            == 2
    });
    assert_eq!(fixture.bridge.remove_script(id), ERR_NOT_FOUND);
}

#[test]
fn a_script_that_throws_when_run_now_is_still_registered_for_new_documents() {
    let fixture = start_bridge(|stream, command, session| {
        if command["method"] == "Runtime.evaluate" && command["params"]["expression"] == "bad()" {
            let id = command["id"].as_u64().unwrap();
            write_text(
                stream,
                &json!({"id": id, "result": {"exceptionDetails": {"text": "Uncaught"}}})
                    .to_string(),
            );
            return true;
        }
        page(stream, command, session)
    });
    ready(&fixture);
    fixture.bridge.add_script("bad", "bad()");
    let mut warned = false;
    wait_until("the warning", 5, || {
        while let Ok(line) = fixture.logs.try_recv() {
            warned |= line.contains("script bad failed to run now");
        }
        warned
    });
    assert!(fixture.bridge.is_ready(), "the session carries on");
    assert_eq!(fixture.bridge.evaluate("1+1").as_deref(), Ok("2"));
}

#[test]
fn evaluate_returns_json_and_maps_failures_to_codes() {
    let fixture = start_bridge(page);
    ready(&fixture);

    assert_eq!(fixture.bridge.evaluate("1+1").as_deref(), Ok("2"));
    assert_eq!(
        fixture.bridge.evaluate("an object").as_deref(),
        Ok("{\"list\":[1,\"two\"]}")
    );
    assert_eq!(fixture.bridge.evaluate("nothing").as_deref(), Ok("null"));
    assert_eq!(fixture.bridge.evaluate("throw"), Err(ERR_EXCEPTION));
}

#[test]
fn evaluate_gives_up_when_the_page_never_answers() {
    let (port, state) = start(chrome_listing, page);
    let fixture = start_bridge_at(
        port,
        state,
        Bridge::new().with_evaluate_timeout(Duration::from_millis(600)),
    );
    ready(&fixture);
    let started = Instant::now();
    assert_eq!(fixture.bridge.evaluate("silence"), Err(ERR_TIMEOUT));
    assert!(started.elapsed() < Duration::from_secs(5));
    // The session is fine afterwards.
    assert_eq!(fixture.bridge.evaluate("1+1").as_deref(), Ok("2"));
}

static RECEIVED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static USER_SEEN: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn on_news(user: *mut c_void, message: *const u8, len: usize) {
    USER_SEEN.store(user as usize, Ordering::SeqCst);
    let text = unsafe { std::slice::from_raw_parts(message, len) };
    RECEIVED
        .lock()
        .unwrap()
        .push(String::from_utf8_lossy(text).into_owned());
}

#[test]
fn page_messages_reach_the_subscribers_of_their_channel() {
    let fixture = start_bridge(page);
    ready(&fixture);

    let subscription = fixture
        .bridge
        .subscribe("news", on_news, 4242 as *mut c_void);
    assert!(subscription > 0);

    assert_eq!(
        fixture.bridge.evaluate("page message").as_deref(),
        Ok("\"sent\"")
    );
    wait_until("the message", 5, || !RECEIVED.lock().unwrap().is_empty());
    assert_eq!(*RECEIVED.lock().unwrap(), ["{\"n\":7}"]);
    assert_eq!(USER_SEEN.load(Ordering::SeqCst), 4242);

    // After unsubscribing the same message goes nowhere.
    assert_eq!(fixture.bridge.unsubscribe(subscription), 0);
    assert_eq!(
        fixture.bridge.evaluate("page message").as_deref(),
        Ok("\"sent\"")
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(RECEIVED.lock().unwrap().len(), 1);
}

#[test]
fn post_delivers_the_payload_through_the_page_helper() {
    let fixture = start_bridge(page);
    ready(&fixture);

    assert_eq!(fixture.bridge.post("greeting", "{\"hello\":[1,2]}"), 0);
    wait_until("the delivery", 5, || {
        expressions(&fixture.state).iter().any(|e| {
            e == "window.__orbit && window.__orbit.__deliver(\"greeting\", {\"hello\":[1,2]})"
        })
    });
}

#[test]
fn the_session_attaches_again_after_the_page_goes_away_and_replays_the_scripts() {
    let fixture = start_bridge(page);
    ready(&fixture);
    fixture
        .bridge
        .add_script("persistent", "window.persistent = true;");
    wait_until("the first install", 5, || {
        expressions(&fixture.state).contains(&"window.persistent = true;".to_string())
    });

    // The page hangs up while a request is pending.
    assert_eq!(fixture.bridge.evaluate("drop"), Err(ERR_NOT_READY));
    wait_until("the bridge to notice", 5, || !fixture.bridge.is_ready());
    assert_eq!(fixture.bridge.evaluate("1+1"), Err(ERR_NOT_READY));

    // About a second later it is back, with the helper and the script.
    wait_until("the second session", 10, || fixture.bridge.is_ready());
    assert!(fixture.state.sessions.load(Ordering::SeqCst) >= 2);
    let installs = expressions(&fixture.state)
        .iter()
        .filter(|e| *e == "window.persistent = true;")
        .count();
    assert_eq!(installs, 2, "once per session");
    assert_eq!(fixture.bridge.evaluate("1+1").as_deref(), Ok("2"));
}

#[test]
fn a_missing_endpoint_is_reported_once_and_calls_say_not_ready() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let bridge: &'static Bridge = Box::leak(Box::new(Bridge::new()));
    let (tx, logs) = mpsc::channel::<String>();
    let tx = Mutex::new(tx);
    bridge
        .start(
            port,
            "test-bridge",
            Box::new(move |level, message| {
                let _ = tx.lock().unwrap().send(format!("{level:?}: {message}"));
            }),
        )
        .unwrap();

    let first = logs.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        first.contains("devtools endpoint not available yet"),
        "{first}"
    );
    assert_eq!(bridge.post("c", "{}"), ERR_NOT_READY);
    assert_eq!(bridge.evaluate("1"), Err(ERR_NOT_READY));
    std::thread::sleep(Duration::from_millis(2500));
    assert!(logs.try_recv().is_err(), "not repeated while it stays away");
}

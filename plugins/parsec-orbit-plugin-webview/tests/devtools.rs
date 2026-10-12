//! The DevTools client against a fake endpoint.
//!
//! The fake speaks just enough HTTP and WebSocket to look like the WebView2
//! DevTools endpoint: `GET /json` for the target list and an upgrade to a page
//! socket that answers the two commands the plugin sends.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use parsec_orbit_plugin_webview::webview::{inject_once, inject_script_forever, BITRATE_SCRIPT};
use serde_json::Value;

// ---- wire helpers -----------------------------------------------------------

fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => bytes.push(byte[0]),
            _ => return None,
        }
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn http_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// Writes one unmasked server frame.
fn write_frame(stream: &mut TcpStream, opcode: u8, fin: bool, payload: &[u8]) {
    let mut out = vec![(if fin { 0x80 } else { 0 }) | opcode];
    match payload.len() {
        length if length < 126 => out.push(length as u8),
        length if length <= 0xFFFF => {
            out.push(126);
            out.extend_from_slice(&(length as u16).to_be_bytes());
        }
        length => {
            out.push(127);
            out.extend_from_slice(&(length as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    let _ = stream.write_all(&out);
}

fn write_text(stream: &mut TcpStream, text: &str) {
    write_frame(stream, 0x1, true, text.as_bytes());
}

/// Reads one client frame, asserting that it is masked as RFC 6455 requires.
fn read_frame(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).ok()?;
    assert_ne!(head[1] & 0x80, 0, "client frames must be masked");
    let length = match head[1] & 0x7F {
        126 => {
            let mut bytes = [0u8; 2];
            stream.read_exact(&mut bytes).ok()?;
            u16::from_be_bytes(bytes) as usize
        }
        127 => {
            let mut bytes = [0u8; 8];
            stream.read_exact(&mut bytes).ok()?;
            u64::from_be_bytes(bytes) as usize
        }
        length => length as usize,
    };
    let mut mask = [0u8; 4];
    stream.read_exact(&mut mask).ok()?;
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload).ok()?;
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    Some((head[0] & 0x0F, payload))
}

// ---- the fake endpoint ----------------------------------------------------------

#[derive(Default)]
struct State {
    /// Every JSON command a client sent, in order.
    commands: Mutex<Vec<Value>>,
    /// Handshake request heads.
    heads: Mutex<Vec<String>>,
    /// Client control frames (opcode, payload), e.g. pongs.
    control: Mutex<Vec<(u8, Vec<u8>)>>,
    sessions: AtomicUsize,
}

/// Called for each command with the session index; returns `false` to end the
/// session (the socket is dropped).
type OnCommand = dyn Fn(&mut TcpStream, &Value, usize) -> bool + Send + Sync;

fn chrome_listing(port: u16) -> String {
    format!(
        "[ {{\n   \"description\": \"\",\n   \"devtoolsFrontendUrl\": \"/devtools/inspector.html?ws=127.0.0.1:{port}/devtools/page/PAGE\",\n   \"id\": \"PAGE\",\n   \"title\": \"Parsec\",\n   \"type\": \"page\",\n   \"url\": \"https://web.parsec.test/\",\n   \"webSocketDebuggerUrl\": \"ws://127.0.0.1:{port}/devtools/page/PAGE\"\n}} ]"
    )
}

/// The replies a healthy page gives.
fn healthy(stream: &mut TcpStream, command: &Value, _session: usize) -> bool {
    let id = command["id"].as_u64().unwrap();
    match command["method"].as_str().unwrap() {
        "Page.addScriptToEvaluateOnNewDocument" => {
            write_text(stream, &format!("{{\"id\":{id},\"result\":{{\"identifier\":\"1\"}}}}"));
        }
        "Runtime.evaluate" => write_text(
            stream,
            &format!("{{\"id\":{id},\"result\":{{\"result\":{{\"type\":\"string\",\"value\":\"installed\"}}}}}}"),
        ),
        other => panic!("unexpected method {other}"),
    }
    true
}

fn start(
    listing: impl Fn(u16) -> String + Send + Sync + 'static,
    on_command: impl Fn(&mut TcpStream, &Value, usize) -> bool + Send + Sync + 'static,
) -> (u16, Arc<State>) {
    start_with(listing, Arc::new(on_command), |_| {})
}

fn start_with(
    listing: impl Fn(u16) -> String + Send + Sync + 'static,
    on_command: Arc<OnCommand>,
    on_upgrade: impl Fn(&mut TcpStream) + Send + Sync + 'static,
) -> (u16, Arc<State>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(State::default());
    let listing = Arc::new(listing);
    let on_upgrade = Arc::new(on_upgrade);

    let shared = state.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (state, listing, on_command, on_upgrade) = (
                shared.clone(),
                listing.clone(),
                on_command.clone(),
                on_upgrade.clone(),
            );
            std::thread::spawn(move || {
                let mut stream = stream;
                let Some(head) = read_head(&mut stream) else {
                    return;
                };
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                if path == "/json" {
                    let _ = stream.write_all(http_response("200 OK", &listing(port)).as_bytes());
                } else if head.to_ascii_lowercase().contains("upgrade: websocket") {
                    state.heads.lock().unwrap().push(head);
                    let session = state.sessions.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(
                        b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n",
                    );
                    on_upgrade(&mut stream);
                    while let Some((opcode, payload)) = read_frame(&mut stream) {
                        if opcode == 0x1 {
                            let command: Value = serde_json::from_slice(&payload).unwrap();
                            state.commands.lock().unwrap().push(command.clone());
                            if !on_command(&mut stream, &command, session) {
                                return;
                            }
                        } else {
                            state.control.lock().unwrap().push((opcode, payload));
                        }
                    }
                } else {
                    let _ = stream.write_all(http_response("404 Not Found", "{}").as_bytes());
                }
            });
        }
    });
    (port, state)
}

fn start_healthy() -> (u16, Arc<State>) {
    start(chrome_listing, healthy)
}

// ---- inject_once -------------------------------------------------------------------

#[test]
fn injects_the_script_into_the_page_and_reports_the_outcome() {
    let (port, state) = start_healthy();

    let injection = inject_once(port).expect("injection against a healthy endpoint");

    assert_eq!(injection.target_url, "https://web.parsec.test/");
    assert!(injection.new_document_script);
    assert_eq!(injection.evaluation_result.as_deref(), Some("installed"));

    let commands = state.commands.lock().unwrap();
    assert_eq!(commands.len(), 2);

    assert_eq!(commands[0]["id"], 1);
    assert_eq!(
        commands[0]["method"],
        "Page.addScriptToEvaluateOnNewDocument"
    );
    assert_eq!(
        commands[0]["params"]["source"], BITRATE_SCRIPT,
        "the script survives JSON escaping"
    );

    assert_eq!(commands[1]["id"], 2);
    assert_eq!(commands[1]["method"], "Runtime.evaluate");
    assert_eq!(commands[1]["params"]["expression"], BITRATE_SCRIPT);
    assert_eq!(commands[1]["params"]["returnByValue"], true);
    assert_eq!(commands[1]["params"]["awaitPromise"], true);
}

#[test]
fn the_websocket_handshake_is_a_valid_rfc6455_upgrade() {
    let (port, state) = start_healthy();
    inject_once(port).unwrap();

    let heads = state.heads.lock().unwrap();
    let head = &heads[0];
    assert!(
        head.starts_with("GET /devtools/page/PAGE HTTP/1.1\r\n"),
        "{head}"
    );
    assert!(
        head.contains(&format!("Host: 127.0.0.1:{port}\r\n")),
        "{head}"
    );
    assert!(head.contains("Upgrade: websocket\r\n"), "{head}");
    assert!(head.contains("Connection: Upgrade\r\n"), "{head}");
    assert!(
        head.contains("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"),
        "{head}"
    );
    assert!(head.contains("Sec-WebSocket-Version: 13\r\n"), "{head}");
}

#[test]
fn replies_buried_in_events_pings_and_fragments_are_still_found() {
    let (port, state) = start(chrome_listing, |stream, command, _| {
        let id = command["id"].as_u64().unwrap();
        // Noise first: an event, a reply to somebody else, a ping, and then the
        // real reply split over two fragments with a ping in the middle.
        write_text(stream, "{\"method\":\"Page.frameNavigated\",\"params\":{}}");
        write_text(stream, &format!("{{\"id\":{},\"result\":{{}}}}", id + 100));
        write_frame(stream, 0x9, true, b"are-you-there");
        let reply = if id == 1 {
            format!("{{\"id\":{id},\"result\":{{\"identifier\":\"9\"}}}}")
        } else {
            format!("{{\"id\":{id},\"result\":{{\"result\":{{\"value\":\"frag\\\"mented\"}}}}}}")
        };
        let (first, second) = reply.split_at(reply.len() / 2);
        write_frame(stream, 0x1, false, first.as_bytes());
        write_frame(stream, 0x9, true, b"mid-message");
        write_frame(stream, 0x0, true, second.as_bytes());
        true
    });

    let injection = inject_once(port).unwrap();
    assert!(injection.new_document_script);
    assert_eq!(injection.evaluation_result.as_deref(), Some("frag\"mented"));

    // Give the server thread a moment to record the pongs, then check them.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let control = state.control.lock().unwrap().clone();
        if control.len() >= 4 || Instant::now() > deadline {
            let pongs: Vec<_> = control.iter().filter(|(op, _)| *op == 0xA).collect();
            assert!(pongs.iter().any(|(_, p)| p == b"are-you-there"));
            assert!(pongs.iter().any(|(_, p)| p == b"mid-message"));
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_very_large_evaluation_result_comes_back_intact() {
    let big = "x".repeat(150_000);
    let (port, _) = start(chrome_listing, {
        let big = big.clone();
        move |stream, command, _| {
            let id = command["id"].as_u64().unwrap();
            let reply = if id == 1 {
                format!("{{\"id\":{id},\"result\":{{}}}}")
            } else {
                format!("{{\"id\":{id},\"result\":{{\"result\":{{\"value\":\"{big}\"}}}}}}")
            };
            // 64-bit length encoding on the wire.
            write_text(stream, &reply);
            true
        }
    });

    let injection = inject_once(port).unwrap();
    assert_eq!(injection.evaluation_result.as_deref(), Some(big.as_str()));
}

#[test]
fn an_error_reply_means_the_script_was_not_registered() {
    let (port, _) = start(chrome_listing, |stream, command, _| {
        let id = command["id"].as_u64().unwrap();
        if id == 1 {
            write_text(
                stream,
                &format!("{{\"id\":{id},\"error\":{{\"code\":-32601,\"message\":\"nope\"}}}}"),
            );
        } else {
            write_text(
                stream,
                &format!("{{\"id\":{id},\"result\":{{\"result\":{{\"type\":\"undefined\"}}}}}}"),
            );
        }
        true
    });

    let injection = inject_once(port).unwrap();
    assert!(!injection.new_document_script);
    assert_eq!(injection.evaluation_result, None);
}

#[test]
fn a_script_exception_is_an_error() {
    let (port, _) = start(chrome_listing, |stream, command, _| {
        let id = command["id"].as_u64().unwrap();
        if id == 1 {
            write_text(stream, &format!("{{\"id\":{id},\"result\":{{}}}}"));
        } else {
            write_text(
                stream,
                &format!(
                    "{{\"id\":{id},\"result\":{{\"exceptionDetails\":{{\"text\":\"Uncaught\"}}}}}}"
                ),
            );
        }
        true
    });

    let error = inject_once(port).err().expect("exception");
    assert!(error.starts_with("script threw:"), "{error}");
    assert!(error.contains("Uncaught"), "{error}");
}

// ---- failures before the socket exists -------------------------------------------------

fn error_of(port: u16) -> String {
    inject_once(port).err().expect("this setup must fail")
}

#[test]
fn nothing_listening_is_a_connect_error() {
    // Port 1 is never bound in tests, so the connection is refused.
    let error = error_of(1);
    assert!(error.starts_with("connect 127.0.0.1:1"), "{error}");
}

#[test]
fn an_http_error_status_is_reported_with_its_status_line() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            read_head(&mut stream);
            let _ = stream.write_all(http_response("503 Service Unavailable", "busy").as_bytes());
        }
    });
    let error = error_of(port);
    assert!(
        error.contains("unexpected response: HTTP/1.1 503"),
        "{error}"
    );
}

#[test]
fn a_target_list_without_a_usable_page_is_reported() {
    for listing in [
        "[]",
        "[{\"type\":\"service_worker\",\"webSocketDebuggerUrl\":\"ws://h/x\"}]",
        "garbage",
    ] {
        let (port, _) = start(move |_| listing.to_string(), healthy);
        assert_eq!(error_of(port), "no page target in /json", "{listing}");
    }
}

#[test]
fn a_page_that_another_debugger_holds_is_skipped_for_the_next_one() {
    let (port, state) = start(
        |port| {
            format!(
                "[{{\"type\":\"page\",\"url\":\"https://busy/\"}},{{\"type\":\"page\",\"url\":\"https://free/\",\"webSocketDebuggerUrl\":\"ws://127.0.0.1:{port}/devtools/page/FREE\"}}]"
            )
        },
        healthy,
    );
    let injection = inject_once(port).unwrap();
    assert_eq!(injection.target_url, "https://free/");
    assert!(state.heads.lock().unwrap()[0].starts_with("GET /devtools/page/FREE "));
}

#[test]
fn an_unusable_socket_url_is_reported() {
    let (port, _) = start(
        |_| {
            "[{\"type\":\"page\",\"url\":\"u\",\"webSocketDebuggerUrl\":\"https://not-a-websocket/\"}]".to_string()
        },
        healthy,
    );
    assert_eq!(error_of(port), "unparsable webSocketDebuggerUrl");
}

#[test]
fn http_responses_that_arrive_in_pieces_are_assembled() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (socket_tx, socket_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let head = read_head(&mut stream).unwrap();
            if head.starts_with("GET /json") {
                let body = chrome_listing(port);
                let response = http_response("200 OK", &body);
                // Byte by byte for the header, then the body in two parts.
                let split = response.find("\r\n\r\n").unwrap() + 4;
                for byte in response[..split].bytes() {
                    stream.write_all(&[byte]).unwrap();
                    stream.flush().unwrap();
                }
                let (first, second) = response[split..].split_at(20);
                stream.write_all(first.as_bytes()).unwrap();
                stream.flush().unwrap();
                std::thread::sleep(Duration::from_millis(100));
                stream.write_all(second.as_bytes()).unwrap();
            } else {
                // Reaching the upgrade means the listing was parsed correctly.
                let _ = socket_tx.send(());
                let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n");
            }
        }
    });

    let error = error_of(port);
    socket_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the page target was found");
    assert!(
        error.contains("websocket upgrade rejected: HTTP/1.1 403"),
        "{error}"
    );
}

#[test]
fn a_listing_without_content_length_is_read_until_the_connection_closes() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let head = read_head(&mut stream).unwrap();
            if head.starts_with("GET /json") {
                let body = chrome_listing(port);
                let _ = stream.write_all(
                    format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{body}").as_bytes(),
                );
            } else {
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
            }
        }
    });
    let error = error_of(port);
    assert!(error.contains("websocket upgrade rejected"), "{error}");
}

// ---- failures on the socket --------------------------------------------------------------

#[test]
fn a_rejected_upgrade_is_reported_with_its_status_line() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let head = read_head(&mut stream).unwrap();
            let response = if head.starts_with("GET /json") {
                http_response("200 OK", &chrome_listing(port))
            } else {
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_string()
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
    let error = error_of(port);
    assert!(
        error.contains("websocket upgrade rejected: HTTP/1.1 403 Forbidden"),
        "{error}"
    );
}

#[test]
fn a_connection_closed_during_the_handshake_is_reported() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let head = read_head(&mut stream).unwrap();
            if head.starts_with("GET /json") {
                let _ = stream.write_all(http_response("200 OK", &chrome_listing(port)).as_bytes());
            }
            // The upgrade request gets no answer: the socket is just closed.
        }
    });
    let error = error_of(port);
    assert!(error.contains("closed during handshake"), "{error}");
}

#[test]
fn the_server_closing_the_socket_is_an_error_not_a_hang() {
    let (port, _) = start(chrome_listing, |stream, _, _| {
        write_frame(stream, 0x8, true, b"");
        false
    });
    assert_eq!(error_of(port), "websocket closed");
}

#[test]
fn the_server_dropping_the_connection_is_an_error_not_a_hang() {
    let (port, _) = start(chrome_listing, |_, _, _| false);
    let error = error_of(port);
    assert!(error.contains("closed"), "{error}");
}

#[test]
fn a_page_that_never_answers_times_out() {
    let (port, _) = start(chrome_listing, |_, _, _| true);
    let started = Instant::now();
    let error = error_of(port);
    assert!(
        error.contains("timed out waiting for a DevTools reply"),
        "{error}"
    );
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_secs(4) && waited < Duration::from_secs(15),
        "{waited:?}"
    );
}

#[test]
fn an_absurd_frame_length_ends_the_session_instead_of_crashing() {
    // A hostile process squatting on the DevTools port must not be able to
    // take the client down with a bogus length field.
    let (port, _) = start_with(chrome_listing, Arc::new(|_, _, _| true), |stream| {
        let mut frame = vec![0x81, 127];
        frame.extend_from_slice(&u64::MAX.to_be_bytes());
        frame.extend_from_slice(b"junk");
        let _ = stream.write_all(&frame);
    });
    let error = error_of(port);
    assert!(error.contains("too large"), "{error}");
}

// ---- inject_script_forever ------------------------------------------------------------------

/// Starts the forever loop on a detached thread and returns its log lines.
fn run_forever(port: u16) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        inject_script_forever(port, move |line| {
            let _ = tx.send(line.to_string());
        });
    });
    rx
}

fn wait_for(rx: &mpsc::Receiver<String>, needle: &str, seconds: u64) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut seen = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(line) => {
                let found = line.contains(needle);
                seen.push(line);
                if found {
                    return seen;
                }
            }
            Err(_) => break,
        }
    }
    panic!("never saw {needle:?}; got {seen:#?}");
}

#[test]
fn the_forever_loop_registers_evaluates_and_then_idles() {
    let (port, state) = start_healthy();
    let log = run_forever(port);

    let lines = wait_for(&log, "webview bandwidth list extended: installed", 10);
    assert!(lines
        .iter()
        .any(|line| line.contains("registered for future documents (https://web.parsec.test/)")));
    assert_eq!(state.commands.lock().unwrap().len(), 2);
}

#[test]
fn the_forever_loop_reattaches_after_the_page_goes_away() {
    // Session 0 answers and then hangs up; session 1 stays.
    let (port, state) = start(chrome_listing, |stream, command, session| {
        healthy(stream, command, session);
        let evaluated = command["method"] == "Runtime.evaluate";
        !(session == 0 && evaluated && {
            write_frame(stream, 0x8, true, b"");
            true
        })
    });
    let log = run_forever(port);

    let lines = wait_for(&log, "webview devtools session ended: websocket closed", 10);
    assert!(lines
        .iter()
        .any(|line| line.contains("extended: installed")));

    // About a second later it attaches again and injects once more.
    wait_for(&log, "webview bandwidth list extended: installed", 10);
    assert!(state.sessions.load(Ordering::SeqCst) >= 2);
    assert!(state.commands.lock().unwrap().len() >= 4);
}

#[test]
fn the_forever_loop_reports_a_missing_endpoint_only_once() {
    let log = run_forever(1);
    let first = wait_for(&log, "webview devtools endpoint not available yet", 5);
    assert_eq!(first.len(), 1);

    // It keeps retrying every second without repeating itself.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        log.try_recv().is_err(),
        "no further log lines while the endpoint stays away"
    );
}

#[test]
fn the_forever_loop_logs_registration_and_script_failures() {
    let (port, _) = start(chrome_listing, |stream, command, _| {
        let id = command["id"].as_u64().unwrap();
        if id == 1 {
            write_text(
                stream,
                &format!("{{\"id\":{id},\"error\":{{\"message\":\"no\"}}}}"),
            );
        } else {
            write_text(
                stream,
                &format!("{{\"id\":{id},\"result\":{{\"exceptionDetails\":{{}}}}}}"),
            );
        }
        true
    });
    let log = run_forever(port);
    let lines = wait_for(&log, "webview bandwidth patch failed", 10);
    assert!(lines
        .iter()
        .any(|line| line.contains("returned no identifier")));
}

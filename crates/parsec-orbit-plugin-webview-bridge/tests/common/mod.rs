//! A fake WebView2 DevTools endpoint shared by the integration tests.
//!
//! It speaks just enough HTTP and WebSocket to look like the real one: `GET
//! /json` for the target list and an upgrade to a page socket that answers
//! the commands the bridge sends.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

// ---- wire helpers -----------------------------------------------------------

pub fn read_head(stream: &mut TcpStream) -> Option<String> {
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

pub fn http_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// Writes one unmasked server frame.
pub fn write_frame(stream: &mut TcpStream, opcode: u8, fin: bool, payload: &[u8]) {
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

pub fn write_text(stream: &mut TcpStream, text: &str) {
    write_frame(stream, 0x1, true, text.as_bytes());
}

/// Reads one client frame, asserting that it is masked as RFC 6455 requires.
pub fn read_frame(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
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
pub struct State {
    /// Every JSON command a client sent, in order.
    pub commands: Mutex<Vec<Value>>,
    /// Handshake request heads.
    pub heads: Mutex<Vec<String>>,
    /// Client control frames (opcode, payload), e.g. pongs.
    pub control: Mutex<Vec<(u8, Vec<u8>)>>,
    pub sessions: AtomicUsize,
}

/// Called for each command with the session index; returns `false` to end the
/// session (the socket is dropped).
pub type OnCommand = dyn Fn(&mut TcpStream, &Value, usize) -> bool + Send + Sync;

pub fn chrome_listing(port: u16) -> String {
    format!(
        "[ {{\n   \"description\": \"\",\n   \"devtoolsFrontendUrl\": \"/devtools/inspector.html?ws=127.0.0.1:{port}/devtools/page/PAGE\",\n   \"id\": \"PAGE\",\n   \"title\": \"Parsec\",\n   \"type\": \"page\",\n   \"url\": \"https://web.parsec.test/\",\n   \"webSocketDebuggerUrl\": \"ws://127.0.0.1:{port}/devtools/page/PAGE\"\n}} ]"
    )
}

/// The replies a healthy page gives.
pub fn healthy(stream: &mut TcpStream, command: &Value, _session: usize) -> bool {
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

pub fn start(
    listing: impl Fn(u16) -> String + Send + Sync + 'static,
    on_command: impl Fn(&mut TcpStream, &Value, usize) -> bool + Send + Sync + 'static,
) -> (u16, Arc<State>) {
    start_with(listing, Arc::new(on_command), |_| {})
}

pub fn start_with(
    listing: impl Fn(u16) -> String + Send + Sync + 'static,
    on_command: Arc<OnCommand>,
    on_upgrade: impl Fn(&mut TcpStream) + Send + Sync + 'static,
) -> (u16, Arc<State>) {
    serve(0, listing, on_command, on_upgrade)
}

/// Starts the fake on a port that was reserved earlier.
pub fn start_on(
    port: u16,
    listing: impl Fn(u16) -> String + Send + Sync + 'static,
    on_command: impl Fn(&mut TcpStream, &Value, usize) -> bool + Send + Sync + 'static,
) -> Arc<State> {
    serve(port, listing, Arc::new(on_command), |_| {}).1
}

fn serve(
    bind: u16,
    listing: impl Fn(u16) -> String + Send + Sync + 'static,
    on_command: Arc<OnCommand>,
    on_upgrade: impl Fn(&mut TcpStream) + Send + Sync + 'static,
) -> (u16, Arc<State>) {
    let listener = TcpListener::bind(("127.0.0.1", bind)).unwrap();
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

pub fn start_healthy() -> (u16, Arc<State>) {
    start(chrome_listing, healthy)
}

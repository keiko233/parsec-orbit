//! WebView2 patch delivery over the DevTools protocol.
//!
//! The loader sets `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, which opens a
//! loopback DevTools endpoint when the client uses its WebView UI. This module
//! speaks just enough of HTTP and WebSocket to drive that endpoint:
//!
//! 1. `GET /json` to find the page target's WebSocket URL.
//! 2. `Page.addScriptToEvaluateOnNewDocument` so the patch also survives
//!    reloads of the web UI.
//! 3. `Runtime.evaluate` so the patch lands in the already-loaded document.
//!
//! The connection is then kept open (answering pings) so the registered script
//! stays alive for the lifetime of the page.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// Injected into the web UI; see `scripts/webview-bitrate-extend.js`.
pub const BITRATE_SCRIPT: &str = include_str!("../../scripts/webview-bitrate-extend.js");

/// RFC 6455 sample key. The server response is not verified; the endpoint is
/// loopback-only and speaks to a known local process.
const WS_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// What an [`inject_once`] call achieved.
pub struct Injection {
    pub target_url: String,
    pub new_document_script: bool,
    pub evaluation_result: Option<String>,
}

/// Result of attaching and keeping the session alive.
fn attach(port: u16) -> Result<(Ws, String), String> {
    let listing = http_get(port, "/json")?;
    let (target_url, ws_url) = pick_page(&listing).ok_or("no page target in /json")?;
    let path = ws_path(&ws_url).ok_or("unparsable webSocketDebuggerUrl")?;
    let socket = Ws::connect(port, &path)?;
    Ok((socket, target_url))
}

/// Injects the bandwidth script into the current document and closes the
/// connection again. Used by the verification tooling and by callers that only
/// need a one-shot patch.
pub fn inject_once(port: u16) -> Result<Injection, String> {
    let (mut socket, target_url) = attach(port)?;
    let mut injection = Injection {
        target_url,
        new_document_script: false,
        evaluation_result: None,
    };
    injection.new_document_script = socket.add_new_document_script(BITRATE_SCRIPT, 1)?;
    injection.evaluation_result = socket.evaluate(BITRATE_SCRIPT, 2)?;
    Ok(injection)
}

/// Keeps a patch session alive forever: attach, inject, then idle until the
/// connection drops (page reload keeps the same target, a recreated webview
/// produces a new one) and start over.
pub fn inject_script_forever<F: Fn(&str)>(port: u16, log: F) {
    let mut waiting_logged = false;
    loop {
        match attach(port) {
            Ok((mut socket, target_url)) => {
                waiting_logged = false;
                match socket.add_new_document_script(BITRATE_SCRIPT, 1) {
                    Ok(true) => log(&format!(
                        "webview patch registered for future documents ({target_url})"
                    )),
                    Ok(false) => log("webview patch registration returned no identifier"),
                    Err(err) => log(&format!("webview patch registration failed: {err}")),
                }
                match socket.evaluate(BITRATE_SCRIPT, 2) {
                    Ok(Some(result)) => log(&format!("webview bandwidth list extended: {result}")),
                    Ok(None) => log("webview bandwidth patch evaluated"),
                    Err(err) => log(&format!("webview bandwidth patch failed: {err}")),
                }
                if let Err(err) = socket.idle() {
                    log(&format!("webview devtools session ended: {err}"));
                }
            }
            Err(err) => {
                if !waiting_logged {
                    log(&format!(
                        "webview devtools endpoint not available yet: {err}"
                    ));
                    waiting_logged = true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
}

fn http_get(port: u16, path: &str) -> Result<String, String> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
        .map_err(|err| format!("connect {address}: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(1500)))
        .map_err(|err| err.to_string())?;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|err| err.to_string())?;

    let mut response = Vec::new();
    let mut header_end: Option<usize> = None;
    let mut content_length: Option<usize> = None;
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(err)
                if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut =>
            {
                if header_end.is_some() {
                    break;
                }
                return Err(format!("no response from 127.0.0.1:{port}"));
            }
            Err(err) => return Err(err.to_string()),
        }
        if header_end.is_none() {
            if let Some(position) = response.windows(4).position(|w| w == b"\r\n\r\n") {
                let end = position + 4;
                header_end = Some(end);
                content_length = parse_content_length(&response[..end]);
            }
        }
        if let (Some(start), Some(length)) = (header_end, content_length) {
            if response.len() - start >= length {
                break;
            }
        }
    }

    let text = String::from_utf8_lossy(&response).into_owned();
    if !text.starts_with("HTTP/1.1 200") {
        return Err(format!(
            "unexpected response: {}",
            text.lines().next().unwrap_or("<empty>")
        ));
    }
    Ok(match header_end {
        Some(start) => text.get(start..).unwrap_or_default().to_string(),
        None => text,
    })
}

fn parse_content_length(header: &[u8]) -> Option<usize> {
    let text = String::from_utf8_lossy(header);
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// Picks the first page target and returns `(url, webSocketDebuggerUrl)`.
fn pick_page(listing: &str) -> Option<(String, String)> {
    for chunk in listing.split('{') {
        let is_page = chunk.contains("\"type\": \"page\"") || chunk.contains("\"type\":\"page\"");
        if !is_page {
            continue;
        }
        if let Some(socket) = json_field(chunk, "webSocketDebuggerUrl") {
            let url = json_field(chunk, "url").unwrap_or_default();
            return Some((url, socket));
        }
    }
    None
}

/// Extracts a flat string field from a JSON fragment.
fn json_field(fragment: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\"");
    let start = fragment.find(&key)? + key.len();
    let rest = &fragment[start..];
    let colon = rest.find(':')?;
    let mut chars = rest[colon + 1..].chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
    if chars.next() != Some('"') {
        return None;
    }
    let mut value = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(value),
            '\\' => value.push(chars.next()?),
            c => value.push(c),
        }
    }
    None
}

fn ws_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("ws://")?;
    let slash = rest.find('/')?;
    Some(rest[slash..].to_string())
}

struct Ws {
    stream: TcpStream,
    pending: Vec<u8>,
}

impl Ws {
    fn connect(port: u16, path: &str) -> Result<Ws, String> {
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
            .map_err(|err| format!("connect {address}: {err}"))?;
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .map_err(|err| err.to_string())?;
        let _ = stream.set_nodelay(true);

        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {WS_KEY}\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        (&stream)
            .write_all(request.as_bytes())
            .map_err(|err| err.to_string())?;

        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let mut buffer = Vec::new();
        while !buffer.windows(4).any(|w| w == b"\r\n\r\n") {
            if Instant::now() > deadline {
                return Err("websocket handshake timed out".into());
            }
            let mut chunk = [0u8; 1024];
            match (&stream).read(&mut chunk) {
                Ok(0) => return Err("websocket closed during handshake".into()),
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                Err(err)
                    if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut =>
                {
                    continue
                }
                Err(err) => return Err(err.to_string()),
            }
            if buffer.len() > 64 * 1024 {
                return Err("websocket handshake response too large".into());
            }
        }
        let header_end = buffer
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| i + 4)
            .unwrap_or(0);
        let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
        if !head.starts_with("HTTP/1.1 101") {
            return Err(format!(
                "websocket upgrade rejected: {}",
                head.lines().next().unwrap_or("<empty>")
            ));
        }
        let pending = buffer[header_end..].to_vec();
        Ok(Ws { stream, pending })
    }

    fn add_new_document_script(&mut self, source: &str, id: u64) -> Result<bool, String> {
        let command = format!(
            "{{\"id\":{id},\"method\":\"Page.addScriptToEvaluateOnNewDocument\",\"params\":{{\"source\":{}}}}}",
            json_string(source)
        );
        self.send_text(&command)?;
        let reply = self.read_until_reply(id, Duration::from_secs(5))?;
        Ok(!reply.contains("\"error\""))
    }

    fn evaluate(&mut self, expression: &str, id: u64) -> Result<Option<String>, String> {
        let command = format!(
            "{{\"id\":{id},\"method\":\"Runtime.evaluate\",\"params\":{{\"expression\":{},\"returnByValue\":true,\"awaitPromise\":true}}}}",
            json_string(expression)
        );
        self.send_text(&command)?;
        let reply = self.read_until_reply(id, Duration::from_secs(5))?;
        if reply.contains("\"exceptionDetails\"") {
            return Err(format!("script threw: {}", truncate(&reply, 300)));
        }
        Ok(json_field(&reply, "value"))
    }

    fn read_until_reply(&mut self, id: u64, timeout: Duration) -> Result<String, String> {
        let needle = format!("\"id\":{id}");
        let deadline = Instant::now() + timeout;
        loop {
            let message = self
                .read_message(deadline)?
                .ok_or("timed out waiting for a DevTools reply")?;
            match message {
                Message::Text(text) => {
                    if text.contains(&needle) {
                        return Ok(text);
                    }
                }
                Message::Close => return Err("websocket closed".into()),
                Message::Ignored => {}
            }
        }
    }

    /// Keeps the session alive, answering pings, until it closes or times out
    /// after prolonged silence.
    fn idle(&mut self) -> Result<(), String> {
        loop {
            let deadline = Instant::now() + IDLE_TIMEOUT;
            match self.read_message(deadline) {
                Ok(Some(Message::Text(_))) | Ok(Some(Message::Ignored)) | Ok(None) => continue,
                Ok(Some(Message::Close)) => return Err("websocket closed".into()),
                Err(err) => return Err(err),
            }
        }
    }

    fn send_text(&mut self, payload: &str) -> Result<(), String> {
        self.send_frame(0x1, payload.as_bytes())
    }

    fn send_pong(&mut self, payload: &[u8]) -> Result<(), String> {
        self.send_frame(0xA, payload)
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode);
        let length = payload.len();
        if length < 126 {
            frame.push(0x80 | length as u8);
        } else if length <= u16::MAX as usize {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(length as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(length as u64).to_be_bytes());
        }
        let mask = [0x51, 0x75, 0x19, 0x77];
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.stream.write_all(&frame).map_err(|err| err.to_string())
    }

    fn read_message(&mut self, deadline: Instant) -> Result<Option<Message>, String> {
        let mut buffer: Option<Vec<u8>> = None;
        loop {
            let frame = match self.read_frame(deadline)? {
                Some(frame) => frame,
                None => return Ok(None),
            };
            match frame.opcode {
                0x0 | 0x1 | 0x2 => {
                    let target = buffer.get_or_insert_with(Vec::new);
                    target.extend_from_slice(&frame.payload);
                    if frame.fin {
                        return Ok(Some(Message::Text(
                            String::from_utf8_lossy(target).into_owned(),
                        )));
                    }
                }
                0x8 => return Ok(Some(Message::Close)),
                0x9 => {
                    self.send_pong(&frame.payload)?;
                }
                0xA => {}
                _ => return Ok(Some(Message::Ignored)),
            }
        }
    }

    fn read_frame(&mut self, deadline: Instant) -> Result<Option<RawFrame>, String> {
        loop {
            if let Some(frame) = parse_frame(&mut self.pending)? {
                return Ok(Some(frame));
            }
            if Instant::now() > deadline {
                return Ok(None);
            }
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("websocket closed".into()),
                Ok(read) => self.pending.extend_from_slice(&chunk[..read]),
                Err(err)
                    if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut =>
                {
                    continue
                }
                Err(err) => return Err(err.to_string()),
            }
        }
    }
}

struct RawFrame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// Tries to take one frame out of `buffer`; incomplete data is left in place.
fn parse_frame(buffer: &mut Vec<u8>) -> Result<Option<RawFrame>, String> {
    if buffer.len() < 2 {
        return Ok(None);
    }
    let fin = buffer[0] & 0x80 != 0;
    let opcode = buffer[0] & 0x0F;
    let masked = buffer[1] & 0x80 != 0;
    let length_byte = buffer[1] & 0x7F;
    let mut offset = 2;
    let length = match length_byte {
        126 => {
            if buffer.len() < offset + 2 {
                return Ok(None);
            }
            let value = u16::from_be_bytes([buffer[offset], buffer[offset + 1]]) as usize;
            offset += 2;
            value
        }
        127 => {
            if buffer.len() < offset + 8 {
                return Ok(None);
            }
            let mut value = 0u64;
            for byte in &buffer[offset..offset + 8] {
                value = (value << 8) | *byte as u64;
            }
            offset += 8;
            value as usize
        }
        value => value as usize,
    };

    let mask = if masked {
        if buffer.len() < offset + 4 {
            return Ok(None);
        }
        let mask = [
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
        ];
        offset += 4;
        Some(mask)
    } else {
        None
    };

    if buffer.len() < offset + length {
        return Ok(None);
    }
    let mut payload = buffer[offset..offset + length].to_vec();
    buffer.drain(..offset + length);
    if let Some(mask) = mask {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i % 4];
        }
    }
    Ok(Some(RawFrame {
        fin,
        opcode,
        payload,
    }))
}

enum Message {
    Text(String),
    Close,
    Ignored,
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x80 => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

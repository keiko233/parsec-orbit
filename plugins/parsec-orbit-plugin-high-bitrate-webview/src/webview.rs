//! WebView2 patch delivery over the DevTools protocol.
//!
//! The loader sets `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, which opens a
//! loopback DevTools endpoint when the client uses its WebView UI; this plugin
//! then speaks just enough of HTTP and WebSocket to drive that endpoint:
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

/// Injected into the web UI; see `assets/bandwidth.js`.
pub const BITRATE_SCRIPT: &str = include_str!("../assets/bandwidth.js");

/// RFC 6455 sample key. The server response is not verified; the endpoint is
/// loopback-only and speaks to a known local process.
const WS_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Largest WebSocket frame accepted from the endpoint.
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

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
    // A DevTools reply is small. Without a bound, a bogus length field would
    // overflow `offset + length` below (a panic, which aborts the client) or
    // make the buffer grow while waiting for bytes that never come.
    if length > MAX_FRAME_BYTES {
        return Err(format!("websocket frame of {length} bytes is too large"));
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    // ---- json_string -------------------------------------------------------

    fn decode(encoded: &str) -> String {
        serde_json::from_str(encoded).expect("json_string must produce valid JSON")
    }

    #[test]
    fn json_string_escapes_the_special_characters() {
        assert_eq!(json_string(""), "\"\"");
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_string("l1\nl2\r\n\t"), "\"l1\\nl2\\r\\n\\t\"");
        assert_eq!(json_string("\u{1}\u{1f}"), "\"\\u0001\\u001f\"");
    }

    #[test]
    fn json_string_keeps_the_output_ascii() {
        let encoded = json_string("\u{e9}\u{30ed}\u{1F600}");
        assert!(encoded.is_ascii());
        // The emoji is a surrogate pair.
        assert_eq!(encoded, "\"\\u00e9\\u30ed\\ud83d\\ude00\"");
    }

    #[test]
    fn json_string_round_trips_every_bmp_character_class() {
        for code in (0u32..0x3100).chain(0xD7F0..0xD800).chain(0xE000..0xE100) {
            let Some(c) = char::from_u32(code) else {
                continue;
            };
            let text = format!("<{c}>");
            assert_eq!(decode(&json_string(&text)), text, "U+{code:04X}");
        }
        for text in ["\u{1F600}", "\u{10FFFF}", "a\u{10000}b", "\u{7f}"] {
            assert_eq!(decode(&json_string(text)), text);
        }
    }

    #[test]
    fn json_string_round_trips_the_injected_script() {
        assert_eq!(decode(&json_string(BITRATE_SCRIPT)), BITRATE_SCRIPT);
    }

    // ---- truncate ----------------------------------------------------------

    #[test]
    fn truncate_leaves_short_text_alone() {
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("", 0), "");
    }

    #[test]
    fn truncate_cuts_long_text_and_marks_it() {
        assert_eq!(truncate("abcdef", 3), "abc...");
        assert_eq!(truncate("abc", 0), "...");
    }

    #[test]
    fn truncate_never_splits_a_multibyte_character() {
        // Each character is three bytes; a limit inside one backs up.
        assert_eq!(truncate("\u{30ed}\u{30ed}\u{30ed}", 4), "\u{30ed}...");
        assert_eq!(truncate("\u{30ed}\u{30ed}", 3), "\u{30ed}...");
        assert_eq!(truncate("\u{30ed}\u{30ed}", 6), "\u{30ed}\u{30ed}");
        assert_eq!(truncate("\u{30ed}", 1), "...");
    }

    // ---- json_field ----------------------------------------------------------

    #[test]
    fn json_field_reads_flat_string_fields() {
        let fragment = r#""id": "A1", "title":"Parsec", "url" :   "https://x/y""#;
        assert_eq!(json_field(fragment, "id").as_deref(), Some("A1"));
        assert_eq!(json_field(fragment, "title").as_deref(), Some("Parsec"));
        assert_eq!(json_field(fragment, "url").as_deref(), Some("https://x/y"));
    }

    #[test]
    fn json_field_unescapes_quotes_and_backslashes() {
        assert_eq!(
            json_field(r#""value":"say \"hi\" C:\\temp \/ done""#, "value").as_deref(),
            Some("say \"hi\" C:\\temp / done")
        );
    }

    #[test]
    fn json_field_returns_none_when_it_cannot_answer() {
        assert_eq!(json_field(r#""a":"b""#, "missing"), None);
        assert_eq!(json_field(r#""a": 5"#, "a"), None, "not a string");
        assert_eq!(json_field(r#""a": null"#, "a"), None);
        assert_eq!(json_field(r#""a": "unterminated"#, "a"), None);
        assert_eq!(json_field(r#""a": "dangling\"#, "a"), None);
        assert_eq!(json_field(r#""a""#, "a"), None, "no colon");
        assert_eq!(json_field("", "a"), None);
    }

    #[test]
    fn json_field_requires_the_exact_key() {
        assert_eq!(
            json_field(r#""myurl":"no","url":"yes""#, "url").as_deref(),
            Some("yes")
        );
    }

    // ---- ws_path / content length -------------------------------------------

    #[test]
    fn ws_path_keeps_everything_after_the_authority() {
        assert_eq!(
            ws_path("ws://127.0.0.1:9222/devtools/page/ABC").as_deref(),
            Some("/devtools/page/ABC")
        );
        assert_eq!(ws_path("ws://host/").as_deref(), Some("/"));
        assert_eq!(ws_path("ws://host/a?b=c").as_deref(), Some("/a?b=c"));
    }

    #[test]
    fn ws_path_rejects_other_schemes_and_missing_paths() {
        assert_eq!(ws_path("wss://host/x"), None);
        assert_eq!(ws_path("http://host/x"), None);
        assert_eq!(ws_path("ws://host"), None);
        assert_eq!(ws_path(""), None);
    }

    #[test]
    fn content_length_is_found_in_any_case() {
        let header = |line: &str| format!("HTTP/1.1 200 OK\r\n{line}\r\nServer: x\r\n\r\n");
        for line in [
            "Content-Length: 42",
            "content-length:42",
            "CONTENT-LENGTH:   42  ",
        ] {
            assert_eq!(
                parse_content_length(header(line).as_bytes()),
                Some(42),
                "{line}"
            );
        }
    }

    #[test]
    fn content_length_is_none_when_absent_or_broken() {
        assert_eq!(parse_content_length(b"HTTP/1.1 200 OK\r\n\r\n"), None);
        assert_eq!(
            parse_content_length(b"HTTP/1.1 200 OK\r\nContent-Length: abc\r\n\r\n"),
            None
        );
        assert_eq!(
            parse_content_length(b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n"),
            None
        );
        assert_eq!(parse_content_length(b""), None);
    }

    // ---- pick_page -------------------------------------------------------------

    fn target(kind: &str, url: &str, socket: Option<&str>) -> String {
        let socket = socket
            .map(|socket| format!(",\n   \"webSocketDebuggerUrl\": \"{socket}\""))
            .unwrap_or_default();
        format!(
            "{{\n   \"description\": \"\",\n   \"id\": \"ID\",\n   \"title\": \"T\",\n   \"type\": \"{kind}\",\n   \"url\": \"{url}\"{socket}\n}}"
        )
    }

    #[test]
    fn pick_page_returns_url_and_socket_of_the_first_page() {
        let listing = format!(
            "[ {}, {} ]",
            target(
                "page",
                "https://one/",
                Some("ws://127.0.0.1:9222/devtools/page/1")
            ),
            target(
                "page",
                "https://two/",
                Some("ws://127.0.0.1:9222/devtools/page/2")
            )
        );
        assert_eq!(
            pick_page(&listing),
            Some((
                "https://one/".to_string(),
                "ws://127.0.0.1:9222/devtools/page/1".to_string()
            ))
        );
    }

    #[test]
    fn pick_page_skips_other_target_types() {
        let listing = format!(
            "[ {}, {}, {} ]",
            target("service_worker", "https://sw/", Some("ws://h/sw")),
            target(
                "background_page",
                "chrome-extension://bg/",
                Some("ws://h/bg")
            ),
            target("page", "https://real/", Some("ws://h/real"))
        );
        assert_eq!(
            pick_page(&listing),
            Some(("https://real/".to_string(), "ws://h/real".to_string()))
        );
    }

    #[test]
    fn pick_page_skips_a_page_somebody_else_is_attached_to() {
        // DevTools omits webSocketDebuggerUrl while another client is attached.
        let listing = format!(
            "[ {}, {} ]",
            target("page", "https://busy/", None),
            target("page", "https://free/", Some("ws://h/free"))
        );
        assert_eq!(
            pick_page(&listing),
            Some(("https://free/".to_string(), "ws://h/free".to_string()))
        );
    }

    #[test]
    fn pick_page_understands_compact_json() {
        let listing =
            r#"[{"id":"1","type":"page","url":"https://c/","webSocketDebuggerUrl":"ws://h/c"}]"#;
        assert_eq!(
            pick_page(listing),
            Some(("https://c/".to_string(), "ws://h/c".to_string()))
        );
    }

    #[test]
    fn pick_page_finds_nothing_in_empty_or_unrelated_listings() {
        assert_eq!(pick_page("[]"), None);
        assert_eq!(pick_page(""), None);
        assert_eq!(pick_page("not json at all"), None);
        assert_eq!(
            pick_page(&format!("[{}]", target("worker", "u", Some("ws://h/w")))),
            None
        );
        assert_eq!(pick_page(&format!("[{}]", target("page", "u", None))), None);
    }

    // ---- frames ------------------------------------------------------------------

    fn frame(fin: bool, opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
        let mut out = vec![(if fin { 0x80 } else { 0 }) | opcode];
        let mask_bit = if mask.is_some() { 0x80 } else { 0 };
        match payload.len() {
            length if length < 126 => out.push(mask_bit | length as u8),
            length if length <= 0xFFFF => {
                out.push(mask_bit | 126);
                out.extend_from_slice(&(length as u16).to_be_bytes());
            }
            length => {
                out.push(mask_bit | 127);
                out.extend_from_slice(&(length as u64).to_be_bytes());
            }
        }
        match mask {
            Some(mask) => {
                out.extend_from_slice(&mask);
                out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
            }
            None => out.extend_from_slice(payload),
        }
        out
    }

    #[test]
    fn parse_frame_reads_an_unmasked_text_frame() {
        let mut buffer = frame(true, 0x1, b"hello", None);
        let parsed = parse_frame(&mut buffer).unwrap().unwrap();
        assert!(parsed.fin);
        assert_eq!(parsed.opcode, 0x1);
        assert_eq!(parsed.payload, b"hello");
        assert!(buffer.is_empty(), "the frame is consumed");
    }

    #[test]
    fn parse_frame_unmasks_payloads() {
        let mut buffer = frame(true, 0x2, b"masked payload!", Some([1, 2, 3, 4]));
        let parsed = parse_frame(&mut buffer).unwrap().unwrap();
        assert_eq!(parsed.payload, b"masked payload!");
    }

    #[test]
    fn parse_frame_handles_every_length_encoding() {
        for length in [0usize, 1, 125, 126, 127, 65535, 65536, 70_000] {
            let payload: Vec<u8> = (0..length).map(|i| (i % 251) as u8).collect();
            let mut buffer = frame(true, 0x1, &payload, None);
            let parsed = parse_frame(&mut buffer).unwrap().unwrap();
            assert_eq!(parsed.payload, payload, "length {length}");
            assert!(buffer.is_empty());
        }
    }

    #[test]
    fn parse_frame_reports_fin_and_opcode() {
        let mut buffer = frame(false, 0x1, b"a", None);
        let parsed = parse_frame(&mut buffer).unwrap().unwrap();
        assert!(!parsed.fin);
        let mut buffer = frame(true, 0x9, b"", None);
        assert_eq!(parse_frame(&mut buffer).unwrap().unwrap().opcode, 0x9);
    }

    #[test]
    fn parse_frame_waits_for_more_data_at_every_cut_point() {
        for payload_length in [5usize, 200, 70_000] {
            let payload = vec![b'x'; payload_length];
            let whole = frame(true, 0x1, &payload, Some([9, 8, 7, 6]));
            for cut in 0..whole.len() {
                let mut buffer = whole[..cut].to_vec();
                assert!(
                    parse_frame(&mut buffer).unwrap().is_none(),
                    "a {cut} byte prefix of a {payload_length} byte frame is incomplete"
                );
                assert_eq!(buffer, &whole[..cut], "incomplete data must stay in place");
            }
        }
    }

    #[test]
    fn parse_frame_takes_one_frame_at_a_time() {
        let mut buffer = frame(true, 0x1, b"one", None);
        buffer.extend(frame(true, 0x1, b"two", None));
        buffer.extend_from_slice(&[0x81]); // start of a third
        assert_eq!(parse_frame(&mut buffer).unwrap().unwrap().payload, b"one");
        assert_eq!(parse_frame(&mut buffer).unwrap().unwrap().payload, b"two");
        assert!(parse_frame(&mut buffer).unwrap().is_none());
        assert_eq!(buffer, [0x81]);
    }

    #[test]
    fn parse_frame_rejects_absurd_lengths_instead_of_panicking() {
        // 2^64 - 1 used to overflow `offset + length` and abort the client.
        for length in [
            u64::MAX,
            u64::MAX - 5,
            1 << 63,
            (MAX_FRAME_BYTES as u64) + 1,
        ] {
            let mut buffer = vec![0x81, 127];
            buffer.extend_from_slice(&length.to_be_bytes());
            buffer.extend_from_slice(&[1, 2, 3]);
            let error = parse_frame(&mut buffer)
                .err()
                .unwrap_or_else(|| panic!("length {length}"));
            assert!(error.contains("too large"), "{error}");
        }
    }

    #[test]
    fn parse_frame_accepts_a_frame_at_the_size_limit() {
        let mut buffer = vec![0x82, 127];
        buffer.extend_from_slice(&(MAX_FRAME_BYTES as u64).to_be_bytes());
        // Header only: legal, and just not complete yet.
        assert!(parse_frame(&mut buffer).unwrap().is_none());
    }

    // ---- Ws over a local socket ---------------------------------------------------

    fn pair() -> (Ws, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let (server, _) = listener.accept().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        (
            Ws {
                stream: client,
                pending: Vec::new(),
            },
            server,
        )
    }

    /// Reads one client frame from the server side and unmasks it.
    fn read_client_frame(server: &mut TcpStream) -> (u8, u8, Vec<u8>) {
        let mut head = [0u8; 2];
        server.read_exact(&mut head).unwrap();
        assert_ne!(head[1] & 0x80, 0, "client frames must be masked");
        let length = match head[1] & 0x7F {
            126 => {
                let mut bytes = [0u8; 2];
                server.read_exact(&mut bytes).unwrap();
                u16::from_be_bytes(bytes) as usize
            }
            127 => {
                let mut bytes = [0u8; 8];
                server.read_exact(&mut bytes).unwrap();
                u64::from_be_bytes(bytes) as usize
            }
            length => length as usize,
        };
        let mut mask = [0u8; 4];
        server.read_exact(&mut mask).unwrap();
        let mut payload = vec![0u8; length];
        server.read_exact(&mut payload).unwrap();
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i % 4];
        }
        (head[0] >> 7, head[0] & 0x0F, payload)
    }

    #[test]
    fn send_frame_encodes_masked_frames_at_every_length_boundary() {
        for length in [0usize, 1, 125, 126, 127, 65535, 65536, 100_000] {
            let (mut ws, mut server) = pair();
            let payload: Vec<u8> = (0..length).map(|i| (i % 253) as u8).collect();
            let sender = std::thread::spawn(move || {
                ws.send_frame(0x2, &payload).unwrap();
                payload
            });
            let (fin, opcode, received) = read_client_frame(&mut server);
            let payload = sender.join().unwrap();
            assert_eq!(fin, 1);
            assert_eq!(opcode, 0x2);
            assert_eq!(received, payload, "length {length}");
        }
    }

    #[test]
    fn send_text_uses_the_text_opcode_and_pong_the_pong_opcode() {
        let (mut ws, mut server) = pair();
        ws.send_text("{\"id\":1}").unwrap();
        ws.send_pong(b"ping-data").unwrap();
        assert_eq!(
            read_client_frame(&mut server),
            (1, 0x1, b"{\"id\":1}".to_vec())
        );
        assert_eq!(
            read_client_frame(&mut server),
            (1, 0xA, b"ping-data".to_vec())
        );
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn read_message_returns_text_messages() {
        let (mut ws, mut server) = pair();
        server.write_all(&frame(true, 0x1, b"hello", None)).unwrap();
        match ws.read_message(deadline()).unwrap() {
            Some(Message::Text(text)) => assert_eq!(text, "hello"),
            _ => panic!("expected a text message"),
        }
    }

    #[test]
    fn read_message_reassembles_fragments_even_with_a_ping_in_between() {
        let (mut ws, mut server) = pair();
        server.write_all(&frame(false, 0x1, b"Hel", None)).unwrap();
        server
            .write_all(&frame(true, 0x9, b"are-you-there", None))
            .unwrap();
        server.write_all(&frame(false, 0x0, b"lo, ", None)).unwrap();
        server.write_all(&frame(true, 0x0, b"world", None)).unwrap();
        match ws.read_message(deadline()).unwrap() {
            Some(Message::Text(text)) => assert_eq!(text, "Hello, world"),
            _ => panic!("expected a text message"),
        }
        // The ping in the middle was answered with a pong carrying its payload.
        assert_eq!(
            read_client_frame(&mut server),
            (1, 0xA, b"are-you-there".to_vec())
        );
    }

    #[test]
    fn read_message_survives_a_frame_split_across_reads() {
        let (mut ws, mut server) = pair();
        let bytes = frame(true, 0x1, &vec![b'z'; 300], None);
        let (head, tail) = bytes.split_at(3);
        let (head, tail) = (head.to_vec(), tail.to_vec());
        let writer = std::thread::spawn(move || {
            server.write_all(&head).unwrap();
            server.flush().unwrap();
            std::thread::sleep(Duration::from_millis(150));
            server.write_all(&tail).unwrap();
            server
        });
        match ws.read_message(deadline()).unwrap() {
            Some(Message::Text(text)) => assert_eq!(text.len(), 300),
            _ => panic!("expected a text message"),
        }
        drop(writer.join().unwrap());
    }

    #[test]
    fn read_message_reports_close_frames_and_ignores_unknown_opcodes() {
        let (mut ws, mut server) = pair();
        server.write_all(&frame(true, 0x3, b"", None)).unwrap();
        assert!(matches!(
            ws.read_message(deadline()).unwrap(),
            Some(Message::Ignored)
        ));
        server.write_all(&frame(true, 0xA, b"", None)).unwrap(); // unsolicited pong
        server.write_all(&frame(true, 0x8, b"", None)).unwrap();
        assert!(matches!(
            ws.read_message(deadline()).unwrap(),
            Some(Message::Close)
        ));
    }

    #[test]
    fn read_message_times_out_quietly_and_errors_when_the_peer_goes_away() {
        let (mut ws, server) = pair();
        let started = Instant::now();
        assert!(ws
            .read_message(Instant::now() + Duration::from_millis(150))
            .unwrap()
            .is_none());
        assert!(started.elapsed() >= Duration::from_millis(100));

        drop(server);
        let error = ws.read_message(deadline()).err().expect("closed peer");
        assert!(error.contains("closed"), "{error}");
    }

    #[test]
    fn read_message_surfaces_an_oversized_frame_as_an_error() {
        let (mut ws, mut server) = pair();
        let mut bytes = vec![0x81, 127];
        bytes.extend_from_slice(&u64::MAX.to_be_bytes());
        server.write_all(&bytes).unwrap();
        let error = ws.read_message(deadline()).err().expect("oversized frame");
        assert!(error.contains("too large"), "{error}");
    }

    #[test]
    fn read_until_reply_skips_events_and_other_replies() {
        let (mut ws, mut server) = pair();
        for text in [
            r#"{"method":"Page.frameNavigated","params":{}}"#,
            r#"{"id":1,"result":{}}"#,
            r#"{"id":12,"result":{"decoy":true}}"#,
            r#"{"id":2,"result":{"wanted":true}}"#,
        ] {
            server
                .write_all(&frame(true, 0x1, text.as_bytes(), None))
                .unwrap();
        }
        let reply = ws.read_until_reply(2, Duration::from_secs(5)).unwrap();
        assert!(reply.contains("wanted"), "{reply}");
    }

    #[test]
    fn read_until_reply_fails_on_close_and_on_silence() {
        let (mut ws, mut server) = pair();
        server.write_all(&frame(true, 0x8, b"", None)).unwrap();
        assert_eq!(
            ws.read_until_reply(1, Duration::from_secs(5)).unwrap_err(),
            "websocket closed"
        );

        let (mut ws, _server) = pair();
        let error = ws
            .read_until_reply(1, Duration::from_millis(200))
            .unwrap_err();
        assert!(error.contains("timed out"), "{error}");
    }

    #[test]
    fn idle_answers_pings_and_ends_on_close() {
        let (mut ws, mut server) = pair();
        server
            .write_all(&frame(true, 0x9, b"keepalive", None))
            .unwrap();
        server
            .write_all(&frame(true, 0x1, b"{\"event\":1}", None))
            .unwrap();
        server.write_all(&frame(true, 0x8, b"", None)).unwrap();
        assert_eq!(ws.idle().unwrap_err(), "websocket closed");
        assert_eq!(
            read_client_frame(&mut server),
            (1, 0xA, b"keepalive".to_vec())
        );
    }

    #[test]
    fn commands_carry_the_id_method_and_escaped_source() {
        let (mut ws, mut server) = pair();
        // Replies are canned; only the outgoing commands are inspected.
        server
            .write_all(&frame(
                true,
                0x1,
                br#"{"id":7,"result":{"identifier":"1"}}"#,
                None,
            ))
            .unwrap();
        server
            .write_all(&frame(
                true,
                0x1,
                br#"{"id":8,"result":{"result":{"type":"string","value":"done"}}}"#,
                None,
            ))
            .unwrap();

        assert!(ws.add_new_document_script("let a = \"q\";\n", 7).unwrap());
        assert_eq!(ws.evaluate("1 + 1", 8).unwrap().as_deref(), Some("done"));

        let (_, _, first) = read_client_frame(&mut server);
        let first: serde_json::Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(first["id"], 7);
        assert_eq!(first["method"], "Page.addScriptToEvaluateOnNewDocument");
        assert_eq!(first["params"]["source"], "let a = \"q\";\n");

        let (_, _, second) = read_client_frame(&mut server);
        let second: serde_json::Value = serde_json::from_slice(&second).unwrap();
        assert_eq!(second["id"], 8);
        assert_eq!(second["method"], "Runtime.evaluate");
        assert_eq!(second["params"]["expression"], "1 + 1");
        assert_eq!(second["params"]["returnByValue"], true);
        assert_eq!(second["params"]["awaitPromise"], true);
    }

    #[test]
    fn replies_are_interpreted() {
        let (mut ws, mut server) = pair();
        for text in [
            r#"{"id":1,"error":{"code":-32000,"message":"nope"}}"#,
            r#"{"id":2,"result":{"result":{"type":"undefined"}}}"#,
            r#"{"id":3,"result":{"exceptionDetails":{"text":"Uncaught"}}}"#,
        ] {
            server
                .write_all(&frame(true, 0x1, text.as_bytes(), None))
                .unwrap();
        }
        assert!(
            !ws.add_new_document_script("x", 1).unwrap(),
            "an error reply is not a registration"
        );
        assert_eq!(ws.evaluate("x", 2).unwrap(), None, "no string value");
        let error = ws.evaluate("throw 1", 3).unwrap_err();
        assert!(error.starts_with("script threw:"), "{error}");
    }

    #[test]
    fn script_errors_are_truncated_in_the_message() {
        let (mut ws, mut server) = pair();
        let long = format!(
            r#"{{"id":1,"result":{{"exceptionDetails":{{"text":"{}"}}}}}}"#,
            "e".repeat(2000)
        );
        server
            .write_all(&frame(true, 0x1, long.as_bytes(), None))
            .unwrap();
        let error = ws.evaluate("x", 1).unwrap_err();
        assert!(error.len() < 400, "{} bytes", error.len());
        assert!(error.ends_with("..."));
    }

    #[test]
    fn the_injected_script_is_the_asset() {
        assert!(BITRATE_SCRIPT.contains("__orbitBitrateExtend"));
        assert!(BITRATE_SCRIPT.trim_end().ends_with("})();"));
    }
}

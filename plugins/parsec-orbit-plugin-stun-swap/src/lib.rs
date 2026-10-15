//! CN STUN Swap - redirect Parsec's STUN hostname to a healthy endpoint.
//!
//! Parsec resolves `stun.parsec.app` through `ws2_32!getaddrinfo`. This
//! plugin probes a small list of candidates with a standard STUN Binding
//! Request, chooses the fastest healthy one, and answers only that hostname
//! with the chosen IPv4 address. Everything else passes through unchanged.

// Windows-only; the macOS port needs the M2 hooks backend.
#![cfg(windows)]

use std::cell::Cell;
use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parsec_orbit_plugin_sdk::prelude::*;
use parsec_orbit_plugin_sdk::services::{HooksClient, UiClient};
use parsec_orbit_plugin_sdk::{ERR_NOT_FOUND, ERR_NOT_READY};
use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

const PARSEC_STUN_V4: &str = "stun.parsec.app";
const PARSEC_STUN_V6: &str = "stun6.parsec.app";

/// Kept in preference order. `stun.parsec.app` is only a last resort: when
/// every domestic candidate fails, rewriting to it changes nothing and leaves
/// Parsec's default behaviour intact.
const DEFAULT_SERVERS: &[&str] = &[
    "stun.hitv.com@3478",
    "stun.miwifi.com@3478",
    "stun.chat.bilibili.com@3478",
    "stun.qq.com@3478",
    "stun.cloudflare.com@3478",
    "stun.parsec.app@3478",
];

const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_SUCCESS: u16 = 0x0101;
const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const MAX_ADDRESSES_PER_CANDIDATE: usize = 3;
const DNS_RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);
const WSAHOST_NOT_FOUND: c_int = 11001;
const PARSEC_STUN_PORT: u16 = 3478;

static CONTEXT: OnceLock<Context> = OnceLock::new();
static ORIGINAL_GETADDRINFO: AtomicUsize = AtomicUsize::new(0);
static HOOK_ID: AtomicI64 = AtomicI64::new(0);
static COUNTER: AtomicU64 = AtomicU64::new(0);
static UI_ENABLED: AtomicBool = AtomicBool::new(true);
static PROBE_INTERVAL_OVERRIDE: AtomicU64 = AtomicU64::new(0);
static PROBE_WAKER: OnceLock<mpsc::SyncSender<()>> = OnceLock::new();

thread_local! {
    // The probe resolves hostnames too. Without this, a probe of
    // `stun.parsec.app` (our own last-resort candidate) would be rewritten to
    // the currently selected IP and would never test the real endpoint.
    static PROBING: Cell<bool> = const { Cell::new(false) };
}

fn selected() -> &'static RwLock<Option<String>> {
    static SELECTED: OnceLock<RwLock<Option<String>>> = OnceLock::new();
    SELECTED.get_or_init(|| RwLock::new(None))
}

fn enabled() -> bool {
    !matches!(
        std::env::var("PARSEC_ORBIT_STUN_SWAP")
            .ok()
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("0" | "off" | "false" | "no")
    )
}

fn probe_interval() -> Duration {
    let overridden = PROBE_INTERVAL_OVERRIDE.load(Ordering::Acquire);
    if overridden >= 10 {
        return Duration::from_secs(overridden);
    }
    let seconds = std::env::var("PARSEC_ORBIT_STUN_SWAP_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 10)
        .unwrap_or(300);
    Duration::from_secs(seconds)
}

fn environment_probe_interval() -> Option<u64> {
    std::env::var("PARSEC_ORBIT_STUN_SWAP_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 10)
}

fn trigger_probe() {
    if let Some(waker) = PROBE_WAKER.get() {
        let _ = waker.try_send(());
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    display: String,
    host: String,
    port: u16,
}

impl Candidate {
    fn new(display: String, host: String, port: u16) -> Option<Self> {
        let valid_host = !host.is_empty()
            && !host.chars().any(|character| {
                character.is_whitespace()
                    || character.is_control()
                    || matches!(character, '/' | '\\')
            });
        (port == PARSEC_STUN_PORT && valid_host).then_some(Candidate {
            display,
            host,
            port,
        })
    }

    fn from_text(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() || text.starts_with('#') {
            return None;
        }

        // [v6]:port and host@port are unambiguous. Bare host:port is accepted
        // for IPv4, which is all this plugin probes.
        let (host, port) = if let Some(rest) = text.strip_prefix('[') {
            let end = rest.find(']')?;
            let host = &rest[..end];
            let port = rest[end + 1..].strip_prefix(':')?;
            (host, port)
        } else if let Some((host, port)) = text.rsplit_once('@') {
            (host, port)
        } else if let Some((host, port)) = text.rsplit_once(':') {
            (host, port)
        } else {
            (text, "3478")
        };

        let port = port.trim().parse::<u16>().ok()?;
        Candidate::new(text.to_string(), host.to_string(), port)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionMode {
    Auto,
    Manual,
}

impl SelectionMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ServerStatus {
    candidate: Candidate,
    result: Option<(Ipv4Addr, Duration)>,
}

#[derive(Debug)]
struct PluginState {
    context: Option<Context>,
    mode: SelectionMode,
    manual_server: String,
    custom_servers: String,
    custom_servers_draft: String,
    custom_servers_dirty: bool,
    enabled: bool,
    interval_secs: u64,
    source: String,
    status: String,
    validation_error: String,
    settings_writable: bool,
    servers: Vec<ServerStatus>,
    settings_path: Option<PathBuf>,
}

impl Default for PluginState {
    fn default() -> Self {
        Self {
            context: None,
            mode: SelectionMode::Auto,
            manual_server: String::new(),
            custom_servers: String::new(),
            custom_servers_draft: String::new(),
            custom_servers_dirty: false,
            enabled: true,
            interval_secs: 300,
            source: "Built-in defaults".to_string(),
            status: "Waiting for first probe".to_string(),
            validation_error: String::new(),
            settings_writable: true,
            servers: Vec::new(),
            settings_path: None,
        }
    }
}

fn with_probe_bypass<T>(function: impl FnOnce() -> T) -> T {
    let previous = PROBING.with(Cell::get);
    PROBING.with(|probing| probing.set(true));
    let result = function();
    PROBING.with(|probing| probing.set(previous));
    result
}

fn last_log() -> &'static Mutex<Option<(String, String)>> {
    static LAST_LOG: OnceLock<Mutex<Option<(String, String)>>> = OnceLock::new();
    LAST_LOG.get_or_init(|| Mutex::new(None))
}

fn dns_timed_out() -> &'static Mutex<HashSet<String>> {
    static DNS_TIMED_OUT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    DNS_TIMED_OUT.get_or_init(|| Mutex::new(HashSet::new()))
}

fn data_path(cx: &Context, name: &str) -> Option<PathBuf> {
    cx.data_dir()
        .map(PathBuf::from)
        .or_else(|| std::env::var("APPDATA").ok().map(PathBuf::from))
        .map(|dir| dir.join(name))
}

fn read_settings(path: &PathBuf, state: &mut PluginState) {
    let text = match std::fs::read(path) {
        Ok(text) => text,
        Err(error) => {
            state.source = "settings.json (unreadable)".into();
            state.validation_error = format!("Could not read settings.json: {error}");
            state.settings_writable = false;
            return;
        }
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&text) else {
        state.source = "settings.json (invalid)".into();
        state.validation_error = "Could not parse settings.json".into();
        state.settings_writable = false;
        return;
    };
    if value.get("format").and_then(|value| value.as_u64()) != Some(1) {
        state.source = "settings.json (unsupported format)".into();
        state.validation_error = "Unsupported settings.json format; expected format 1".into();
        state.settings_writable = false;
        return;
    }
    state.settings_writable = true;
    state.source = "settings.json".to_string();
    match value.get("mode").and_then(|value| value.as_str()) {
        Some("manual") => state.mode = SelectionMode::Manual,
        Some("auto") => state.mode = SelectionMode::Auto,
        Some(_) => state.validation_error = "Invalid mode in settings.json".into(),
        None => {}
    }
    if let Some(selected) = value.get("selected").and_then(|value| value.as_str()) {
        state.manual_server = selected.to_string();
    }
    if let Some(servers) = value.get("custom_servers") {
        if let Some(servers) = servers.as_array() {
            if servers.iter().all(serde_json::Value::is_string) {
                state.custom_servers = servers
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n");
                if !state.custom_servers_dirty {
                    state.custom_servers_draft = state.custom_servers.clone();
                }
            } else {
                state.validation_error =
                    "custom_servers in settings.json must contain only strings".into();
            }
        } else {
            state.validation_error = "custom_servers in settings.json must be an array".into();
        }
    }
    if let Some(enabled) = value.get("enabled").and_then(|value| value.as_bool()) {
        state.enabled = enabled;
    }
    if let Some(interval) = value.get("interval").and_then(|value| value.as_u64()) {
        if interval >= 10 {
            state.interval_secs = interval;
        } else {
            state.validation_error = "interval in settings.json must be at least 10".into();
        }
    }
}

fn save_settings(path: &PathBuf, state: &PluginState) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "settings path has no parent",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let custom = state
        .custom_servers
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let value = serde_json::json!({
        "format": 1,
        "mode": state.mode.as_str(),
        "selected": state.manual_server,
        "custom_servers": custom,
        "enabled": state.enabled,
        "interval": state.interval_secs,
    });
    std::fs::write(path, serde_json::to_vec_pretty(&value)?)
}

fn persist_settings(state: &mut PluginState) {
    state.validation_error.clear();
    if !state.settings_writable {
        state.validation_error =
            "Refusing to overwrite invalid or unsupported settings.json".into();
        return;
    }
    match state.settings_path.as_ref() {
        Some(path) => match save_settings(path, state) {
            Ok(()) => state.status = "Settings saved".to_string(),
            Err(error) => state.validation_error = format!("Could not save settings.json: {error}"),
        },
        None => {
            state.validation_error =
                "Could not save settings.json: plugin data directory is unavailable".into()
        }
    }
}

fn read_legacy_servers(path: &PathBuf, state: &mut PluginState) -> Vec<Candidate> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    state.source = "servers.txt (legacy)".to_string();
    let (servers, invalid) = parse_candidate_list(text.lines());
    if !invalid.is_empty() {
        state.validation_error = format!(
            "Rejected invalid legacy server(s): {}. Use host@3478; other ports cannot be redirected.",
            invalid.join(", ")
        );
    }
    servers
}

fn load_candidates(cx: &Context, state: &mut PluginState) -> Vec<Candidate> {
    let settings_path = data_path(cx, "settings.json");
    if let Some(path) = &settings_path {
        if path.exists() {
            read_settings(path, state);
        }
    }
    let (_, invalid_custom) = parse_candidate_list(state.custom_servers.lines());
    if !invalid_custom.is_empty() {
        state.validation_error = format!(
            "Rejected invalid server(s): {}. Use host@3478; other ports cannot be redirected.",
            invalid_custom.join(", ")
        );
    }
    if let Some(value) = nonempty_override(std::env::var("PARSEC_ORBIT_STUN_SERVERS").ok()) {
        state.source = "PARSEC_ORBIT_STUN_SERVERS".to_string();
        let (candidates, invalid) = parse_candidate_list(value.split(','));
        if !invalid.is_empty() {
            state.validation_error = format!(
                "Rejected invalid server(s): {}. Use host@3478; other ports cannot be redirected.",
                invalid.join(", ")
            );
        }
        return candidates;
    }

    let mut candidates = if let Some(path) = &settings_path {
        if path.exists() {
            let legacy = data_path(cx, "servers.txt")
                .map(|legacy_path| read_legacy_servers(&legacy_path, state))
                .unwrap_or_default();
            if legacy.is_empty() {
                state.source = "settings.json".to_string();
                DEFAULT_SERVERS
                    .iter()
                    .filter_map(|text| Candidate::from_text(text))
                    .collect()
            } else {
                state.source = "settings.json + servers.txt (legacy)".to_string();
                legacy
            }
        } else if let Some(legacy_path) = data_path(cx, "servers.txt") {
            let legacy = read_legacy_servers(&legacy_path, state);
            if legacy.is_empty() {
                DEFAULT_SERVERS
                    .iter()
                    .filter_map(|text| Candidate::from_text(text))
                    .collect()
            } else {
                legacy
            }
        } else {
            DEFAULT_SERVERS
                .iter()
                .filter_map(|text| Candidate::from_text(text))
                .collect()
        }
    } else {
        DEFAULT_SERVERS
            .iter()
            .filter_map(|text| Candidate::from_text(text))
            .collect()
    };

    let (custom, invalid) = parse_candidate_list(state.custom_servers.lines());
    if invalid.is_empty() {
        if !state.custom_servers_dirty
            && state
                .validation_error
                .starts_with("Rejected invalid server(s):")
        {
            state.validation_error.clear();
        }
    } else {
        state.validation_error = format!(
            "Rejected invalid server(s): {}. Use host@3478; other ports cannot be redirected.",
            invalid.join(", ")
        );
    }
    candidates.extend(custom);
    let mut seen = HashSet::new();
    candidates.retain(|candidate| seen.insert(candidate.display.to_ascii_lowercase()));
    candidates
}

fn nonempty_override(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn parse_candidate_list<'a>(
    entries: impl IntoIterator<Item = &'a str>,
) -> (Vec<Candidate>, Vec<String>) {
    let mut candidates = Vec::new();
    let mut invalid = Vec::new();
    for entry in entries {
        let entry = entry.trim();
        if entry.is_empty() || entry.starts_with('#') {
            continue;
        }
        match Candidate::from_text(entry) {
            Some(candidate) => candidates.push(candidate),
            None => invalid.push(entry.to_string()),
        }
    }
    (candidates, invalid)
}

fn transaction_id() -> [u8; 12] {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed) as u32;
    let mut id = [0u8; 12];
    id[..8].copy_from_slice(&nanos.to_le_bytes());
    id[8..].copy_from_slice(&counter.to_le_bytes());
    id
}

fn parse_binding_response(buffer: &[u8], transaction: &[u8; 12]) -> Option<(Ipv4Addr, u16)> {
    if buffer.len() < 20 {
        return None;
    }
    let message_type = u16::from_be_bytes([buffer[0], buffer[1]]);
    let attribute_length = u16::from_be_bytes([buffer[2], buffer[3]]) as usize;
    let magic = u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]);
    if message_type != STUN_BINDING_SUCCESS || magic != STUN_MAGIC_COOKIE {
        return None;
    }
    if buffer[8..20] != transaction[..] {
        return None;
    }

    let end = 20usize.saturating_add(attribute_length).min(buffer.len());
    let mut offset = 20;
    while offset + 4 <= end {
        let attribute_type = u16::from_be_bytes([buffer[offset], buffer[offset + 1]]);
        let length = u16::from_be_bytes([buffer[offset + 2], buffer[offset + 3]]) as usize;
        let value_start = offset + 4;
        let value_end = value_start.saturating_add(length).min(end);
        let value = &buffer[value_start..value_end];
        if matches!(
            attribute_type,
            ATTR_MAPPED_ADDRESS | ATTR_XOR_MAPPED_ADDRESS
        ) && value.len() >= 8
            && value[1] == 0x01
        {
            let port = u16::from_be_bytes([value[2], value[3]]);
            let mut address = u32::from_be_bytes([value[4], value[5], value[6], value[7]]);
            if attribute_type == ATTR_XOR_MAPPED_ADDRESS {
                address ^= STUN_MAGIC_COOKIE;
            }
            return Some((Ipv4Addr::from(address), port));
        }
        // STUN attributes are padded to a four-byte boundary.
        let padding = (4 - (length % 4)) % 4;
        offset = value_end.saturating_add(padding);
    }
    None
}

fn binding_request(addr: SocketAddrV4, timeout: Duration) -> Option<(Ipv4Addr, u16, Duration)> {
    let transaction = transaction_id();
    let mut request = [0u8; 20];
    request[0..2].copy_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
    request[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    request[8..].copy_from_slice(&transaction);

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.set_read_timeout(Some(timeout)).ok()?;
    socket.set_write_timeout(Some(timeout)).ok()?;
    let started = SystemTime::now();
    socket.send_to(&request, addr).ok()?;
    let mut response = [0u8; 2048];
    let (size, peer) = socket.recv_from(&mut response).ok()?;
    if peer != SocketAddr::V4(addr) {
        return None;
    }
    let (mapped, _) = parse_binding_response(&response[..size], &transaction)?;
    let rtt = started.elapsed().ok().unwrap_or_default();
    Some((mapped, addr.port(), rtt))
}

fn resolve_candidate(host: &str, port: u16) -> Option<Vec<SocketAddrV4>> {
    let host = host.to_string();
    if dns_timed_out()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(&host)
    {
        return None;
    }

    // DNS cannot be cancelled once the system resolver starts, so run it in a
    // throwaway thread and bound how long a refresh may wait for it. A host
    // that times out is skipped for the rest of the process to avoid leaking
    // one blocked thread per probe pass.
    let (sender, receiver) = mpsc::channel();
    let lookup_host = host.clone();
    std::thread::spawn(move || {
        let addresses = with_probe_bypass(|| (lookup_host.as_str(), port).to_socket_addrs().ok());
        let _ = sender.send(addresses);
    });

    match receiver.recv_timeout(DNS_RESOLVE_TIMEOUT) {
        Ok(Some(addresses)) => {
            let addresses = addresses
                .filter_map(|address| match address {
                    SocketAddr::V4(address) => Some(address),
                    SocketAddr::V6(_) => None,
                })
                .take(MAX_ADDRESSES_PER_CANDIDATE)
                .collect::<Vec<_>>();
            dns_timed_out()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&host);
            (!addresses.is_empty()).then_some(addresses)
        }
        _ => {
            dns_timed_out()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(host);
            None
        }
    }
}

fn probe(candidate: &Candidate, timeout: Duration) -> Option<(Ipv4Addr, Duration)> {
    let addresses = resolve_candidate(&candidate.host, candidate.port)?;
    if addresses.is_empty() {
        return None;
    }

    addresses
        .into_iter()
        .filter_map(|address| {
            binding_request(address, timeout).map(|(_, _, rtt)| (*address.ip(), rtt))
        })
        .min_by_key(|(_, rtt)| rtt.as_millis())
}

fn update_ui(cx: &Context, state: &PluginState) {
    let Some(ui) = UiClient::new(cx) else {
        return;
    };
    let options = server_options(state);
    let server_status = state
        .servers
        .iter()
        .map(|server| match server.result {
            Some((ip, rtt)) => format!(
                "{} — healthy, {} ms, {ip}",
                server.candidate.display,
                rtt.as_millis()
            ),
            None => format!("{} — unhealthy", server.candidate.display),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let json = serde_json::json!({
        "enabled": state.enabled,
        "interval": state.interval_secs,
        "mode": {"value": state.mode.as_str(), "options": [
            {"label": "Automatic (lowest latency)", "value": "auto"},
            {"label": "Manual", "value": "manual"}
        ]},
        "server": {"value": state.manual_server, "options": options},
        "source": state.source,
        "active_server": if state.validation_error.is_empty() {
            state.status.clone()
        } else {
            format!("{} — {}", state.status, state.validation_error)
        },
        "server_health": server_status,
    });
    let _ = ui.update_state("com.keiko233.stun-swap", &json.to_string());
}

fn server_options(state: &PluginState) -> Vec<serde_json::Value> {
    let mut options = vec![serde_json::json!({
        "label": "Choose a server",
        "value": "",
    })];
    options.extend(state.servers.iter().map(|server| {
        serde_json::json!({
            "label": format!(
                "{} [{}]",
                server.candidate.display,
                if server.result.is_some() { "healthy" } else { "unhealthy" }
            ),
            "value": server.candidate.display,
        })
    }));
    if !state.manual_server.is_empty()
        && !state
            .servers
            .iter()
            .any(|server| server.candidate.display == state.manual_server)
    {
        options.push(serde_json::json!({
            "label": format!("{} [unavailable]", state.manual_server),
            "value": state.manual_server,
        }));
    }
    options
}

fn update_ui_with_draft(cx: &Context, state: &PluginState) {
    let Some(ui) = UiClient::new(cx) else {
        return;
    };
    let json = serde_json::json!({"custom_servers": state.custom_servers_draft});
    let _ = ui.update_state("com.keiko233.stun-swap", &json.to_string());
}

fn choose_server<'a>(
    statuses: &'a [ServerStatus],
    mode: SelectionMode,
    manual_server: &str,
) -> Option<(&'a ServerStatus, (Ipv4Addr, Duration))> {
    match mode {
        SelectionMode::Auto => statuses
            .iter()
            .filter_map(|server| server.result.map(|result| (server, result)))
            .min_by_key(|(_, (_, rtt))| *rtt),
        SelectionMode::Manual => statuses
            .iter()
            .find(|server| server.candidate.display == manual_server)
            .and_then(|server| server.result.map(|result| (server, result))),
    }
}

fn refresh(cx: &Context, candidates: &[Candidate], state: &Arc<Mutex<PluginState>>) {
    let mut statuses = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let result = probe(candidate, Duration::from_millis(1200));
        if let Some((ip, rtt)) = result {
            info!(
                cx,
                "stun swap: {} healthy, {} ms ({ip})",
                candidate.display,
                rtt.as_millis()
            );
        } else {
            info!(cx, "stun swap: {} unhealthy", candidate.display);
        }
        statuses.push(ServerStatus {
            candidate: candidate.clone(),
            result,
        });
    }

    let mut guard = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let chosen = choose_server(&statuses, guard.mode, &guard.manual_server);
    let message = match chosen {
        Some((server, (_ip, rtt)))
            if server.candidate.host.eq_ignore_ascii_case(PARSEC_STUN_V4)
                || server.candidate.host.eq_ignore_ascii_case(PARSEC_STUN_V6) =>
        {
            if let Ok(mut active) = selected().write() {
                *active = None;
            }
            format!("Parsec default is healthy ({} ms)", rtt.as_millis())
        }
        Some((server, (ip, rtt))) => {
            if let Ok(mut active) = selected().write() {
                *active = Some(ip.to_string());
            }
            format!(
                "{} → {ip} ({} ms)",
                server.candidate.display,
                rtt.as_millis()
            )
        }
        None if guard.mode == SelectionMode::Manual => {
            if let Ok(mut active) = selected().write() {
                *active = None;
            }
            if guard.manual_server.is_empty() {
                "No manual server selected; using Parsec DNS".to_string()
            } else {
                format!(
                    "Manual server {} is unhealthy; using Parsec DNS",
                    guard.manual_server
                )
            }
        }
        None => {
            if let Ok(mut active) = selected().write() {
                *active = None;
            }
            warn!(cx, "stun swap: no healthy candidate; using Parsec DNS");
            "No healthy server; using Parsec DNS".to_string()
        }
    };
    guard.servers = statuses;
    guard.status = message;
    update_ui(cx, &guard);
}

fn probe_loop(cx: Context, rx: mpsc::Receiver<()>, state: Arc<Mutex<PluginState>>) {
    loop {
        let (candidates, validation_error) = {
            let mut guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let candidates = load_candidates(&cx, &mut guard);
            UI_ENABLED.store(guard.enabled, Ordering::Release);
            PROBE_INTERVAL_OVERRIDE.store(guard.interval_secs, Ordering::Release);
            (candidates, guard.validation_error.clone())
        };
        if candidates.is_empty() {
            if !validation_error.is_empty() {
                error!(cx, "stun swap: {validation_error}");
            }
            error!(cx, "stun swap: no candidates configured");
            refresh(&cx, &candidates, &state);
            let interval = probe_interval();
            let _ = rx.recv_timeout(interval);
            continue;
        }
        refresh(&cx, &candidates, &state);
        let interval = probe_interval();
        let _ = rx.recv_timeout(interval);
    }
}

type GetAddrInfoFn = unsafe extern "system" fn(
    *const c_char,
    *const c_char,
    *const c_void,
    *mut *mut c_void,
) -> c_int;

unsafe extern "system" fn getaddrinfo_detour(
    node: *const c_char,
    service: *const c_char,
    hints: *const c_void,
    result: *mut *mut c_void,
) -> c_int {
    let original_address = ORIGINAL_GETADDRINFO.load(Ordering::Acquire);
    if original_address == 0 {
        // Should not happen: the detour is enabled only after the trampoline
        // was stored. Return an error instead of crashing.
        return WSAHOST_NOT_FOUND;
    }
    let original: GetAddrInfoFn =
        unsafe { std::mem::transmute::<usize, GetAddrInfoFn>(original_address) };

    if !node.is_null() {
        let name = unsafe { CStr::from_ptr(node) };
        if let Ok(name) = name.to_str() {
            let probing = PROBING.with(Cell::get);
            let name = name.trim_end_matches('.');
            if !probing
                && UI_ENABLED.load(Ordering::Acquire)
                && name.eq_ignore_ascii_case(PARSEC_STUN_V4)
            {
                if let Some(replacement) = selected().read().ok().and_then(|guard| guard.clone()) {
                    if let Ok(replacement) = CString::new(replacement) {
                        if let Some(cx) = CONTEXT.get() {
                            let text = replacement.to_str().unwrap_or("<invalid>");
                            let pair = (name.to_string(), text.to_string());
                            let mut last = last_log()
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            if last.as_ref() != Some(&pair) {
                                *last = Some(pair);
                                info!(cx, "stun swap: {name} -> {text}");
                            }
                        }
                        return unsafe { original(replacement.as_ptr(), service, hints, result) };
                    }
                }
            }
        }
    }

    unsafe { original(node, service, hints, result) }
}

const STUN_UI_SCHEMA: &str = r#"{
    "id": "com.keiko233.stun-swap",
    "title": "STUN Swap",
    "description": "Redirects Parsec STUN lookups to healthy mainland China STUN servers",
    "order": 20,
    "items": [
        {
            "key": "enabled",
            "type": "toggle",
            "label": "Enable STUN Redirection",
            "description": "Intercept stun.parsec.app lookups and answer with chosen low-latency server",
            "default": true
        },
        {
            "key": "interval",
            "type": "slider",
            "label": "Probe Interval (seconds)",
            "description": "Frequency of background latency probes",
            "min": 10.0,
            "max": 600.0,
            "step": 10.0,
            "default": 300.0
        },
        {
            "key": "mode",
            "type": "select",
            "label": "Selection mode",
            "default": "auto",
            "options": [
                {"label": "Automatic (lowest latency)", "value": "auto"},
                {"label": "Manual", "value": "manual"}
            ]
        },
        {
            "key": "server",
            "type": "select",
            "label": "Manual server",
            "description": "Used when selection mode is Manual",
            "default": "",
            "options": [{"label": "Choose a server", "value": ""}]
        },
        {
            "key": "custom_servers",
            "type": "text_input",
            "label": "Custom servers (one host@3478 per line)",
            "description": "Enter one server per line, then save. Only port 3478 can be redirected.",
            "multiline": true,
            "default": ""
        },
        {
            "key": "save_custom_servers",
            "type": "button",
            "label": "Save custom servers"
        },
        {
            "key": "source",
            "type": "readonly_text",
            "label": "Server list source",
            "default": "Built-in defaults"
        },
        {
            "key": "active_server",
            "type": "text_view",
            "label": "Active STUN target",
            "default": "Probing..."
        },
        {
            "key": "server_health",
            "type": "text_view",
            "label": "Server health",
            "default": "Waiting for first probe"
        },
        {
            "key": "probe_now",
            "type": "button",
            "label": "Probe Now",
            "description": "Trigger an immediate round of latency probes across all servers"
        }
    ]
}"#;

unsafe extern "system" fn on_ui_action(user: *mut c_void, msg: *const u8, len: usize) {
    if user.is_null() {
        return;
    }
    // SAFETY: init retains a strong Arc reference to this Mutex for the subscription lifetime.
    let state_arc = unsafe { &*(user as *const Mutex<PluginState>) };
    let slice = unsafe { std::slice::from_raw_parts(msg, len) };
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(slice) {
        let key = val.get("key").and_then(|v| v.as_str()).unwrap_or_default();
        match key {
            "enabled" => {
                if let Some(enabled) = val.get("value").and_then(|v| v.as_bool()) {
                    UI_ENABLED.store(enabled, Ordering::Release);
                    if let Ok(mut state) = state_arc.lock() {
                        state.enabled = enabled;
                        persist_settings(&mut state);
                        if let Some(cx) = state.context {
                            update_ui(&cx, &state);
                        }
                    }
                }
            }
            "interval" => {
                if let Some(sec) = val.get("value").and_then(|v| v.as_f64()) {
                    let seconds = sec as u64;
                    if seconds >= 10 {
                        PROBE_INTERVAL_OVERRIDE.store(seconds, Ordering::Release);
                        if let Ok(mut state) = state_arc.lock() {
                            state.interval_secs = seconds;
                            persist_settings(&mut state);
                            if let Some(cx) = state.context {
                                update_ui(&cx, &state);
                            }
                        }
                    }
                }
            }
            "probe_now" => {
                trigger_probe();
            }
            "custom_servers" => {
                let Some(value) = val.get("value").and_then(|value| value.as_str()) else {
                    return;
                };
                if let Ok(mut state) = state_arc.lock() {
                    state.custom_servers_draft = value.to_string();
                    state.custom_servers_dirty = true;
                }
            }
            "save_custom_servers" => {
                let mut state = state_arc
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let mut should_probe = false;
                let (candidates, invalid) =
                    parse_candidate_list(state.custom_servers_draft.lines());
                if !invalid.is_empty() {
                    state.validation_error = format!(
                        "Rejected invalid server(s): {}. Use host@3478; other ports cannot be redirected.",
                        invalid.join(", ")
                    );
                } else {
                    let previous = state.custom_servers.clone();
                    state.custom_servers = candidates
                        .iter()
                        .map(|candidate| candidate.display.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    persist_settings(&mut state);
                    if !state.validation_error.is_empty() {
                        state.custom_servers = previous;
                    } else {
                        state.custom_servers_draft = state.custom_servers.clone();
                        state.custom_servers_dirty = false;
                        should_probe = true;
                    }
                }
                let cx = state.context;
                drop(state);
                if let Some(cx) = cx {
                    if let Ok(state) = state_arc.lock() {
                        update_ui(&cx, &state);
                        if should_probe {
                            update_ui_with_draft(&cx, &state);
                        }
                    }
                }
                if should_probe {
                    trigger_probe();
                }
            }
            "mode" | "server" => {
                let Some(value) = val.get("value").and_then(|value| value.as_str()) else {
                    return;
                };
                let mut state = state_arc
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match key {
                    "mode" => match value {
                        "auto" => state.mode = SelectionMode::Auto,
                        "manual" => state.mode = SelectionMode::Manual,
                        _ => return,
                    },
                    "server" => state.manual_server = value.to_string(),
                    _ => unreachable!(),
                }
                if let Ok(mut active) = selected().write() {
                    *active = None;
                }
                persist_settings(&mut state);
                let cx = state.context;
                drop(state);
                if let Some(cx) = cx {
                    if let Ok(state) = state_arc.lock() {
                        update_ui(&cx, &state);
                    }
                }
                trigger_probe();
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct StunSwap;

impl Plugin for StunSwap {
    fn init(&mut self, cx: &Context) -> Result {
        // Panics go to the loader log, tagged with the plugin id, instead of
        // the standard error stream the client does not have.
        let panic_cx = *cx;
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            panic_cx.log(Level::Error, format_args!("panic: {info}"));
            previous(info);
        }));

        if !enabled() {
            info!(cx, "stun swap: disabled by PARSEC_ORBIT_STUN_SWAP");
            return Ok(());
        }

        let hooks = HooksClient::new(cx).ok_or_else(|| {
            Error::with_message(ERR_NOT_READY, "com.parsec-orbit.hooks is not installed")
        })?;

        // The loader can scan plugins before the client has imported WS2_32.
        // GetModuleHandleW in the hooks service only finds loaded modules, so
        // make sure ws2_32 is mapped before resolving getaddrinfo in it.
        let ws2 = std::ffi::OsStr::new("ws2_32.dll")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        unsafe { LoadLibraryW(ws2.as_ptr()) };

        let target = hooks.resolve("ws2_32.dll", "getaddrinfo").ok_or_else(|| {
            Error::with_message(ERR_NOT_FOUND, "ws2_32.dll!getaddrinfo was not found")
        })?;

        let _ = CONTEXT.set(*cx);
        let (hook, original) =
            unsafe { hooks.install(target, getaddrinfo_detour as *const c_void)? };
        ORIGINAL_GETADDRINFO.store(original as usize, Ordering::Release);
        HOOK_ID.store(hook, Ordering::Release);
        if let Err(error) = hooks.enable(hook) {
            let _ = hooks.remove(hook);
            HOOK_ID.store(0, Ordering::Release);
            ORIGINAL_GETADDRINFO.store(0, Ordering::Release);
            return Err(error);
        }

        let mut initial_state = PluginState {
            context: Some(*cx),
            settings_path: data_path(cx, "settings.json"),
            ..PluginState::default()
        };
        if initial_state
            .settings_path
            .as_ref()
            .is_none_or(|path| !path.exists())
        {
            if let Some(seconds) = environment_probe_interval() {
                initial_state.interval_secs = seconds;
            }
        }
        let _ = load_candidates(cx, &mut initial_state);
        UI_ENABLED.store(initial_state.enabled, Ordering::Release);
        PROBE_INTERVAL_OVERRIDE.store(initial_state.interval_secs, Ordering::Release);
        let state = Arc::new(Mutex::new(initial_state));
        let (tx, rx) = mpsc::sync_channel(1);
        let _ = PROBE_WAKER.set(tx);

        if let Some(ui) = UiClient::new(cx) {
            let _ = ui.register_section(STUN_UI_SCHEMA);
            if let Ok(state_guard) = state.lock() {
                update_ui(cx, &state_guard);
                update_ui_with_draft(cx, &state_guard);
            }
            let callback_state = Arc::into_raw(Arc::clone(&state));
            if unsafe {
                ui.on_action(
                    "com.keiko233.stun-swap",
                    on_ui_action,
                    callback_state as *mut c_void,
                )
            }
            .is_err()
            {
                // SAFETY: failed subscription means the service retained no callback user pointer.
                drop(unsafe { Arc::from_raw(callback_state) });
            }
        }

        let cx = *cx;
        let worker_state = Arc::clone(&state);
        if let Err(error) = std::thread::Builder::new()
            .name(format!("{}:probe", cx.plugin_id()))
            .spawn(move || probe_loop(cx, rx, worker_state))
        {
            error!(cx, "stun swap: could not start probe thread: {error}");
            let _ = hooks.disable(hook);
            let _ = hooks.remove(hook);
            HOOK_ID.store(0, Ordering::Release);
            ORIGINAL_GETADDRINFO.store(0, Ordering::Release);
            return Err(error.into());
        }
        info!(cx, "stun swap: hook installed and probe started");
        Ok(())
    }
}

parsec_orbit_plugin_sdk::declare_plugin! {
    id = "com.keiko233.stun-swap",
    name = "CN STUN Swap",
    version = "0.1.1",
    author = "keiko",
    description = "Redirects stun.parsec.app to healthy mainland-China STUN servers",
    license = "MIT",
    plugin = StunSwap,
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk_testing::MockHost;

    #[test]
    fn parses_host_port_candidates() {
        assert_eq!(
            Candidate::from_text("stun.hitv.com@3478"),
            Some(Candidate {
                display: "stun.hitv.com@3478".into(),
                host: "stun.hitv.com".into(),
                port: 3478,
            })
        );
        assert_eq!(
            Candidate::from_text(" 111.206.174.2:3478 "),
            Some(Candidate {
                display: "111.206.174.2:3478".into(),
                host: "111.206.174.2".into(),
                port: 3478,
            })
        );
        assert_eq!(
            Candidate::from_text("stun.qq.com"),
            Some(Candidate {
                display: "stun.qq.com".into(),
                host: "stun.qq.com".into(),
                port: 3478,
            })
        );
        assert_eq!(Candidate::from_text("# comment"), None);
        assert_eq!(Candidate::from_text(""), None);
        assert_eq!(Candidate::from_text("bad@0"), None);
        assert_eq!(Candidate::from_text("custom.example@3479"), None);
        assert_eq!(Candidate::from_text("custom host@3478"), None);
        assert_eq!(Candidate::from_text("custom host @3478"), None);
        assert_eq!(Candidate::from_text("folder/server@3478"), None);
        assert_eq!(Candidate::from_text("folder\\server@3478"), None);
        assert_eq!(Candidate::from_text("bad\u{7}host@3478"), None);
    }

    #[test]
    fn empty_environment_override_is_treated_as_unset() {
        assert_eq!(nonempty_override(None), None);
        assert_eq!(nonempty_override(Some(String::new())), None);
        assert_eq!(nonempty_override(Some("  \t\r\n".into())), None);
        assert_eq!(
            nonempty_override(Some("127.0.0.2@3478".into())),
            Some("127.0.0.2@3478".into())
        );
    }

    #[test]
    fn parses_a_standard_stun_response() {
        // A real XOR-MAPPED-ADDRESS response shape: mapped 220.197.181.193.
        let transaction = [7u8; 12];
        let mut buffer = [0u8; 32];
        buffer[0..2].copy_from_slice(&STUN_BINDING_SUCCESS.to_be_bytes());
        buffer[2..4].copy_from_slice(&12u16.to_be_bytes());
        buffer[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        buffer[8..20].copy_from_slice(&transaction);
        buffer[20..22].copy_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        buffer[22..24].copy_from_slice(&8u16.to_be_bytes());
        buffer[25] = 0x01;
        buffer[26..28].copy_from_slice(&61121u16.to_be_bytes());
        let xor = u32::from_be_bytes([220, 197, 181, 193]) ^ STUN_MAGIC_COOKIE;
        buffer[28..32].copy_from_slice(&xor.to_be_bytes());

        assert_eq!(
            parse_binding_response(&buffer, &transaction),
            Some((Ipv4Addr::new(220, 197, 181, 193), 61121))
        );
    }

    #[test]
    fn parses_xor_mapped_after_an_odd_length_attribute() {
        let transaction = [7u8; 12];
        let mut buffer = [0u8; 44];
        buffer[0..2].copy_from_slice(&STUN_BINDING_SUCCESS.to_be_bytes());
        buffer[2..4].copy_from_slice(&24u16.to_be_bytes());
        buffer[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        buffer[8..20].copy_from_slice(&transaction);

        // SOFTWARE with a five-byte value needs three bytes of padding.
        buffer[20..22].copy_from_slice(&0x8022u16.to_be_bytes());
        buffer[22..24].copy_from_slice(&5u16.to_be_bytes());
        buffer[24..29].copy_from_slice(b"abcde");

        buffer[32..34].copy_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        buffer[34..36].copy_from_slice(&8u16.to_be_bytes());
        buffer[37] = 0x01;
        buffer[38..40].copy_from_slice(&61121u16.to_be_bytes());
        let xor = u32::from_be_bytes([220, 197, 181, 193]) ^ STUN_MAGIC_COOKIE;
        buffer[40..44].copy_from_slice(&xor.to_be_bytes());

        assert_eq!(
            parse_binding_response(&buffer, &transaction),
            Some((Ipv4Addr::new(220, 197, 181, 193), 61121))
        );
    }

    #[test]
    fn rejects_wrong_transaction_and_wrong_type() {
        let transaction = [7u8; 12];
        let mut buffer = [0u8; 20];
        buffer[0..2].copy_from_slice(&STUN_BINDING_SUCCESS.to_be_bytes());
        buffer[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        buffer[8..20].copy_from_slice(&transaction);
        assert_eq!(parse_binding_response(&buffer, &[9u8; 12]), None);

        buffer[0..2].copy_from_slice(&0x0111u16.to_be_bytes());
        assert_eq!(parse_binding_response(&buffer, &transaction), None);
    }

    #[test]
    fn detour_rewrites_only_parsec_stun_names() {
        use std::ffi::CString;
        use std::sync::Mutex;

        static DETOUR_LOCK: Mutex<()> = Mutex::new(());
        let _guard = DETOUR_LOCK.lock().unwrap();
        static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
        unsafe extern "system" fn fake_original(
            node: *const c_char,
            _service: *const c_char,
            _hints: *const c_void,
            _result: *mut *mut c_void,
        ) -> c_int {
            let name = if node.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(node) }
                    .to_string_lossy()
                    .into_owned()
            };
            SEEN.lock().unwrap().push(name);
            0
        }

        let original: GetAddrInfoFn = fake_original;
        ORIGINAL_GETADDRINFO.store(original as usize, Ordering::SeqCst);
        *selected().write().unwrap() = Some("127.0.0.1".to_string());

        let parsec = CString::new(PARSEC_STUN_V4).unwrap();
        let parsec_trailing_dot = CString::new("stun.parsec.app.").unwrap();
        let parsec_v6 = CString::new(PARSEC_STUN_V6).unwrap();
        let unrelated = CString::new("builds.parsec.app").unwrap();
        unsafe {
            getaddrinfo_detour(
                parsec.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
            );
            getaddrinfo_detour(
                parsec_trailing_dot.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
            );
            getaddrinfo_detour(
                unrelated.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
            );
            getaddrinfo_detour(
                parsec_v6.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
            );
            PROBING.with(|probing| probing.set(true));
            getaddrinfo_detour(
                parsec.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
            );
            PROBING.with(|probing| probing.set(false));
        }

        assert_eq!(
            *SEEN.lock().unwrap(),
            [
                "127.0.0.1".to_string(),
                "127.0.0.1".to_string(),
                "builds.parsec.app".to_string(),
                "stun6.parsec.app".to_string(),
                "stun.parsec.app".to_string()
            ]
        );
        *selected().write().unwrap() = None;
        ORIGINAL_GETADDRINFO.store(0, Ordering::SeqCst);
    }

    #[test]
    fn defaults_are_valid_candidates() {
        assert!(DEFAULT_SERVERS
            .iter()
            .all(|text| Candidate::from_text(text).is_some()));
    }

    #[test]
    fn automatic_and_manual_modes_choose_only_healthy_servers() {
        let server = |text: &str, ip: &str, millis: u64| ServerStatus {
            candidate: Candidate::from_text(text).unwrap(),
            result: Some((ip.parse().unwrap(), Duration::from_millis(millis))),
        };
        let statuses = [
            server("slow.example@3478", "192.0.2.1", 30),
            server("fast.example@3478", "192.0.2.2", 10),
            ServerStatus {
                candidate: Candidate::from_text("down.example@3478").unwrap(),
                result: None,
            },
        ];

        assert_eq!(
            choose_server(&statuses, SelectionMode::Auto, "")
                .unwrap()
                .0
                .candidate
                .display,
            "fast.example@3478"
        );
        assert_eq!(
            choose_server(&statuses, SelectionMode::Manual, "slow.example@3478")
                .unwrap()
                .0
                .candidate
                .display,
            "slow.example@3478"
        );
        assert!(choose_server(&statuses, SelectionMode::Manual, "down.example@3478").is_none());
    }

    #[test]
    fn settings_json_persists_user_controls_and_reports_write_errors() {
        let dir = std::env::temp_dir().join(format!(
            "parsec-orbit-stun-swap-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let state = PluginState {
            mode: SelectionMode::Manual,
            manual_server: "custom.example@3478".to_string(),
            custom_servers: "custom.example@3478\n".to_string(),
            enabled: false,
            interval_secs: 90,
            ..PluginState::default()
        };

        save_settings(&path, &state).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["format"], 1);
        assert_eq!(saved["mode"], "manual");
        assert_eq!(saved["selected"], "custom.example@3478");
        assert_eq!(saved["custom_servers"][0], "custom.example@3478");
        assert_eq!(saved["enabled"], false);
        assert_eq!(saved["interval"], 90);
        let mut restored = PluginState::default();
        read_settings(&path, &mut restored);
        assert_eq!(restored.mode, SelectionMode::Manual);
        assert_eq!(restored.manual_server, "custom.example@3478");
        assert_eq!(restored.custom_servers, "custom.example@3478");
        assert!(!restored.enabled);
        assert_eq!(restored.interval_secs, 90);

        let blocked = dir.join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        let mut failed = PluginState {
            settings_path: Some(blocked),
            ..PluginState::default()
        };
        persist_settings(&mut failed);
        assert!(failed
            .validation_error
            .contains("Could not save settings.json"));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dynamic_server_options_keep_placeholder_and_unavailable_selection() {
        let state = PluginState {
            manual_server: "saved.example@3478".to_string(),
            ..PluginState::default()
        };
        let options = server_options(&state);
        assert_eq!(options[0]["label"], "Choose a server");
        assert_eq!(options[0]["value"], "");
        assert_eq!(options[1]["label"], "saved.example@3478 [unavailable]");
        assert_eq!(options[1]["value"], "saved.example@3478");
    }

    #[test]
    fn descriptor_and_manifest_agree() {
        use parsec_orbit_plugin_sdk::ABI_VERSION;
        use std::ffi::CStr;

        let manifest = include_str!("../plugin.toml");
        let value = |key: &str| {
            manifest
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once('=')?;
                    (name.trim() == key).then(|| value.trim().trim_matches('"').to_string())
                })
                .unwrap()
        };
        let text = |pointer: *const u8| {
            unsafe { CStr::from_ptr(pointer as *const _) }
                .to_str()
                .unwrap()
                .to_string()
        };

        let desc = unsafe { &*super::ParsecOrbitPluginQuery(ABI_VERSION) };
        assert_eq!(text(desc.id), value("id"));
        assert_eq!(text(desc.name), value("name"));
        assert_eq!(text(desc.version), value("version"));
        assert_eq!(text(desc.author), value("author"));
        assert_eq!(text(desc.description), value("description"));
        assert_eq!(text(desc.license), value("license"));
        assert_eq!(value("requires"), "[\"com.parsec-orbit.hooks\"]");
    }

    #[test]
    fn init_refuses_a_missing_host_table() {
        assert_eq!(
            unsafe { super::ParsecOrbitPluginInit(std::ptr::null()) },
            parsec_orbit_plugin_sdk::ERR_INVALID
        );
        let host = MockHost::new("com.keiko233.stun-swap");
        assert!(host.logs().is_empty());
    }

    #[test]
    fn stun_swap_registers_ui_and_handles_actions() {
        static REG_SCHEMA: Mutex<Option<String>> = Mutex::new(None);
        static ACTION_CB: Mutex<Option<parsec_orbit_plugin_sdk::MessageCallback>> =
            Mutex::new(None);
        static ACTION_USER: Mutex<usize> = Mutex::new(0);

        unsafe extern "system" fn test_reg(schema: *const u8, len: usize) -> i32 {
            let s = String::from_utf8_lossy(core::slice::from_raw_parts(schema, len)).into_owned();
            *REG_SCHEMA.lock().unwrap() = Some(s);
            0
        }
        unsafe extern "system" fn test_on_action(
            _id: *const u8,
            _len: usize,
            cb: Option<parsec_orbit_plugin_sdk::MessageCallback>,
            user: *mut c_void,
        ) -> i64 {
            *ACTION_CB.lock().unwrap() = cb;
            *ACTION_USER.lock().unwrap() = user as usize;
            42
        }

        static MOCK_UI: parsec_orbit_plugin_sdk::ParsecOrbitUiV1 =
            parsec_orbit_plugin_sdk::ParsecOrbitUiV1 {
                header: parsec_orbit_plugin_sdk::ParsecOrbitInterfaceHeader::new(
                    std::mem::size_of::<parsec_orbit_plugin_sdk::ParsecOrbitUiV1>(),
                    1,
                    0,
                ),
                show: None,
                hide: None,
                is_visible: None,
                register_section: Some(test_reg),
                unregister_section: None,
                update_state: None,
                on_action: Some(test_on_action),
                off_action: None,
            };

        let host = MockHost::new("com.keiko233.stun-swap");
        host.provide("parsec-orbit.ui", &MOCK_UI.header);

        let ui = UiClient::new(&host.context()).expect("ui client available");
        assert!(ui.register_section(STUN_UI_SCHEMA).is_ok());
        let reg = REG_SCHEMA.lock().unwrap().take().unwrap();
        assert!(reg.contains("com.keiko233.stun-swap"));
        assert!(reg.contains("STUN Swap"));

        let state = Arc::new(Mutex::new(PluginState::default()));
        let user = Arc::as_ptr(&state) as *mut c_void;
        // Initial state
        assert!(UI_ENABLED.load(Ordering::Acquire));
        assert!(unsafe { ui.on_action("com.keiko233.stun-swap", on_ui_action, user) }.is_ok());

        // Action callback for enabled toggle
        let action_json = r#"{"key":"enabled","type":"change","value":false}"#;
        unsafe {
            on_ui_action(
                *ACTION_USER.lock().unwrap() as *mut c_void,
                action_json.as_ptr(),
                action_json.len(),
            )
        };
        assert!(!UI_ENABLED.load(Ordering::Acquire));

        // Action callback for interval slider
        let action_json2 = r#"{"key":"interval","type":"change","value":60.0}"#;
        unsafe {
            on_ui_action(
                *ACTION_USER.lock().unwrap() as *mut c_void,
                action_json2.as_ptr(),
                action_json2.len(),
            )
        };
        assert_eq!(PROBE_INTERVAL_OVERRIDE.load(Ordering::Acquire), 60);

        // Action callback for probe_now button
        let action_json3 = r#"{"key":"probe_now","type":"click"}"#;
        unsafe {
            on_ui_action(
                *ACTION_USER.lock().unwrap() as *mut c_void,
                action_json3.as_ptr(),
                action_json3.len(),
            )
        };

        // Reset back
        UI_ENABLED.store(true, Ordering::Release);
    }
}

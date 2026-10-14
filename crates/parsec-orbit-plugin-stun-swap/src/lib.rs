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
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex, OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parsec_orbit_plugin_sdk_std::prelude::*;
use parsec_orbit_plugin_sdk_std::services::HooksClient;
use parsec_orbit_plugin_sdk_std::{ERR_NOT_FOUND, ERR_NOT_READY};
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

static CONTEXT: OnceLock<Context> = OnceLock::new();
static ORIGINAL_GETADDRINFO: AtomicUsize = AtomicUsize::new(0);
static HOOK_ID: AtomicI64 = AtomicI64::new(0);
static COUNTER: AtomicU64 = AtomicU64::new(0);

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
    let seconds = std::env::var("PARSEC_ORBIT_STUN_SWAP_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 10)
        .unwrap_or(300);
    Duration::from_secs(seconds)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    display: String,
    host: String,
    port: u16,
}

impl Candidate {
    fn new(display: String, host: String, port: u16) -> Option<Self> {
        (port != 0 && !host.is_empty() && !host.contains('\0')).then_some(Candidate {
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
        Candidate::new(text.to_string(), host.trim().to_string(), port)
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

fn environment_candidates() -> Option<Vec<Candidate>> {
    let value = std::env::var("PARSEC_ORBIT_STUN_SERVERS").ok()?;
    let candidates = value
        .split(',')
        .filter_map(Candidate::from_text)
        .collect::<Vec<_>>();
    (!candidates.is_empty()).then_some(candidates)
}

fn file_candidates(cx: &Context) -> Option<Vec<Candidate>> {
    let path = cx
        .data_dir()
        .map(PathBuf::from)
        .or_else(|| std::env::var("APPDATA").ok().map(PathBuf::from))
        .map(|dir| dir.join("servers.txt"))?;
    let text = std::fs::read_to_string(path).ok()?;
    let candidates = text
        .lines()
        .filter_map(Candidate::from_text)
        .collect::<Vec<_>>();
    (!candidates.is_empty()).then_some(candidates)
}

fn load_candidates(cx: &Context) -> Vec<Candidate> {
    environment_candidates()
        .or_else(|| file_candidates(cx))
        .unwrap_or_else(|| {
            DEFAULT_SERVERS
                .iter()
                .filter_map(|text| Candidate::from_text(text))
                .collect()
        })
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

fn refresh(cx: &Context, candidates: &[Candidate]) {
    let mut best: Option<(Ipv4Addr, Duration)> = None;
    let mut best_is_parsec = false;
    for candidate in candidates {
        let Some(result) = probe(candidate, Duration::from_millis(1200)) else {
            continue;
        };
        info!(
            cx,
            "stun swap: {} -> {} ({} ms)",
            candidate.display,
            result.0,
            result.1.as_millis()
        );

        // Publish the first healthy candidate immediately so a connection
        // started during the first probe is already covered.
        if best.as_ref().is_none_or(|current| result.1 < current.1) {
            best = Some(result);
            best_is_parsec = candidate.host.eq_ignore_ascii_case(PARSEC_STUN_V4)
                || candidate.host.eq_ignore_ascii_case(PARSEC_STUN_V6);
            if let Ok(mut guard) = selected().write() {
                *guard = Some(result.0.to_string());
            }
        }
    }

    match best {
        Some((_ip, _)) if best_is_parsec => {
            if let Ok(mut guard) = selected().write() {
                *guard = None;
            }
            info!(
                cx,
                "stun swap: Parsec's own STUN is healthy, leaving DNS untouched"
            );
        }
        Some((ip, _)) => info!(cx, "stun swap: selected {ip}"),
        None => {
            let previous = selected().read().ok().and_then(|guard| guard.clone());
            if let Some(ip) = previous {
                warn!(
                    cx,
                    "stun swap: no healthy candidate, keeping previous selection {ip}"
                );
            } else {
                warn!(cx, "stun swap: no healthy candidate, leaving DNS untouched");
            }
        }
    }
}

fn probe_loop(cx: Context) {
    if load_candidates(&cx).is_empty() {
        error!(cx, "stun swap: no candidates configured");
        return;
    }
    let interval = probe_interval();
    loop {
        // Reload every pass so a servers.txt edit takes effect without a
        // Parsec restart.
        let candidates = load_candidates(&cx);
        if candidates.is_empty() {
            error!(cx, "stun swap: no candidates configured");
            std::thread::sleep(interval);
            continue;
        }
        refresh(&cx, &candidates);
        std::thread::sleep(interval);
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
            if !probing && name.eq_ignore_ascii_case(PARSEC_STUN_V4) {
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

#[derive(Default)]
struct StunSwap;

impl Plugin for StunSwap {
    fn init(&mut self, cx: &Context) -> Result {
        cx.install_panic_hook();

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

        let cx = *cx;
        if let Err(error) = cx.spawn("probe", move || probe_loop(cx)) {
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

parsec_orbit_plugin_sdk_std::declare_plugin! {
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
    use parsec_orbit_plugin_sdk_std::testing::MockHost;

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
    fn descriptor_and_manifest_agree() {
        use parsec_orbit_plugin_sdk_std::ABI_VERSION;
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
            parsec_orbit_plugin_sdk_std::ERR_INVALID
        );
        let host = MockHost::new("com.keiko233.stun-swap");
        assert!(host.logs().is_empty());
    }
}

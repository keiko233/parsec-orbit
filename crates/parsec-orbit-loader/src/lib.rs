//! Parsec Orbit loader - the `bcrypt.dll` proxy that carries the plugin system.
//!
//! Why `bcrypt.dll`: the client executable statically imports it, and Windows
//! resolves static imports during process initialisation - before the client
//! restricts its own DLL search path to `System32`. A `bcrypt.dll` next to the
//! executable therefore wins over the system copy, without patching anything
//! and without injecting into a running process. Other DLLs the client loads
//! later (`dxgi.dll`, for example) are resolved after that restriction and are
//! no longer replaceable this way.
//!
//! The proxy is transparent: all 60 exports of the real `bcrypt.dll` are
//! mirrored as naked thunks that jump to the real implementation in
//! `%SystemRoot%\System32\bcrypt.dll`. Nothing is patched or replaced; the
//! loader only adds behaviour on top (WebView2 devtools arguments and plugin
//! loading).
//!
//! `DllMain` does the absolute minimum - record the module handle and start a
//! worker thread - because it runs while the loader lock is held. Everything
//! else happens on that worker thread (or lazily on the first forwarded call),
//! never inside `DllMain`.

mod config;
mod exports;
mod log;
mod plugin;
#[cfg(test)]
mod testutil;
mod webview;

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{
    DisableThreadLibraryCalls, GetModuleFileNameW, GetProcAddress, LoadLibraryExW,
};
use windows_sys::Win32::System::Threading::CreateThread;

use exports::{NAMES, SLOTS};

/// `LOAD_LIBRARY_SEARCH_SYSTEM32`: loads the real bcrypt without ever picking
/// this proxy up again.
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

/// Grace period before plugin images are mapped, so the client can finish its
/// own start-up first.
const PLUGIN_WARMUP: Duration = Duration::from_millis(300);

static SELF_MODULE: AtomicIsize = AtomicIsize::new(0);
static REAL_BCRYPT: OnceLock<usize> = OnceLock::new();

/// The four argument registers exactly as they are handed to the real
/// function. Recorded on every forwarded call (four stores) and readable
/// through `ParsecOrbitLoaderLastArgs` in `diagnostics` builds.
#[allow(dead_code)]
static LAST_ARGS: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];

/// Address of [`LAST_ARGS`], used by `verify/parsec-orbit-verify-callpaths`.
#[cfg(feature = "diagnostics")]
#[no_mangle]
pub extern "system" fn ParsecOrbitLoaderLastArgs() -> *const AtomicUsize {
    LAST_ARGS.as_ptr()
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn own_path() -> Option<PathBuf> {
    let module = SELF_MODULE.load(Ordering::SeqCst) as HMODULE;
    if module.is_null() {
        return None;
    }
    let mut buffer = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) };
    if len == 0 {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(
        &buffer[..len as usize],
    )))
}

/// Directory holding this `bcrypt.dll`, i.e. the client directory.
fn loader_dir() -> Option<PathBuf> {
    own_path().and_then(|path| path.parent().map(Path::to_path_buf))
}

/// Loads `%SystemRoot%\System32\bcrypt.dll`.
fn real_bcrypt() -> HMODULE {
    let address = *REAL_BCRYPT.get_or_init(|| {
        let path = std::env::var("SystemRoot")
            .map(|root| format!("{root}\\System32\\bcrypt.dll"))
            .unwrap_or_else(|_| "C:\\Windows\\System32\\bcrypt.dll".to_string());
        let name = wide(&path);
        let module = unsafe {
            LoadLibraryExW(
                name.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            log::error(&format!("could not load the real bcrypt.dll from {path}"));
            return 0usize;
        }
        module as usize
    });
    address as HMODULE
}

/// Fills one thunk slot; called through [`resolve_stub`] with the slot address.
///
/// Already resolved slots short-circuit here, so a call only pays for an
/// atomic load after the first time.
unsafe extern "system" fn resolve_one(slot: *mut AtomicUsize) -> *const c_void {
    let existing = (*slot).load(Ordering::SeqCst);
    if existing != 0 {
        return existing as *const c_void;
    }

    let base = SLOTS.as_ptr() as usize;
    let index = (slot as usize).wrapping_sub(base) / std::mem::size_of::<AtomicUsize>();
    let name = NAMES.get(index).copied().unwrap_or("");

    let real = real_bcrypt();
    if real.is_null() {
        return std::ptr::null();
    }

    let symbol = format!("{name}\0");
    match GetProcAddress(real, symbol.as_ptr()) {
        Some(function) => {
            (*slot).store(function as usize, Ordering::SeqCst);
            log::debug(&format!("resolved {name} -> 0x{:X}", function as usize));
            function as *const c_void
        }
        None => {
            log::error(&format!("the real bcrypt.dll has no export named {name}"));
            std::ptr::null()
        }
    }
}

/// Saves the volatile argument registers, resolves the slot and jumps to the
/// real function.
///
/// Stack layout after `sub rsp, 0x68`: shadow space `[rsp, rsp+0x20)`, xmm
/// saves `[rsp+0x20, rsp+0x60)`, pushed arguments above that (`r9` at
/// `rsp+0x68`). The reservation must stay above `0x60`, otherwise the `xmm3`
/// save slot overlaps the saved `r9` and silently zeroes the fourth argument of
/// every forwarded call - which is exactly how
/// `BCryptGenRandom(NULL, buf, len, BCRYPT_USE_SYSTEM_PREFERRED_RNG)` used to
/// fail with `STATUS_INVALID_HANDLE` while a direct call to the same address
/// worked.
#[unsafe(naked)]
unsafe extern "system" fn resolve_stub() -> usize {
    core::arch::naked_asm!(
        "push rcx",
        "push rdx",
        "push r8",
        "push r9",
        "sub rsp, 0x68",
        "movdqu xmmword ptr [rsp + 0x20], xmm0",
        "movdqu xmmword ptr [rsp + 0x30], xmm1",
        "movdqu xmmword ptr [rsp + 0x40], xmm2",
        "movdqu xmmword ptr [rsp + 0x50], xmm3",
        "mov rcx, rax",
        "call {resolve}",
        "movdqu xmm0, xmmword ptr [rsp + 0x20]",
        "movdqu xmm1, xmmword ptr [rsp + 0x30]",
        "movdqu xmm2, xmmword ptr [rsp + 0x40]",
        "movdqu xmm3, xmmword ptr [rsp + 0x50]",
        "add rsp, 0x68",
        "pop r9",
        "pop r8",
        "pop rdx",
        "pop rcx",
        "mov qword ptr [rip + {args} + 0], rcx",
        "mov qword ptr [rip + {args} + 8], rdx",
        "mov qword ptr [rip + {args} + 16], r8",
        "mov qword ptr [rip + {args} + 24], r9",
        "jmp rax",
        args = sym LAST_ARGS,
        resolve = sym resolve_one,
    )
}

/// Resolves every export up front so the first call of each function does not
/// have to do it, and so the log shows whether the proxy and the system DLL
/// agree on the export set.
fn pre_resolve() -> usize {
    let real = real_bcrypt();
    if real.is_null() {
        return 0;
    }
    let mut resolved = 0usize;
    for (index, slot) in SLOTS.iter().enumerate() {
        if slot.load(Ordering::SeqCst) != 0 {
            resolved += 1;
            continue;
        }
        let symbol = format!("{}\0", NAMES[index]);
        if let Some(function) = unsafe { GetProcAddress(real, symbol.as_ptr()) } {
            slot.store(function as usize, Ordering::SeqCst);
            resolved += 1;
        }
    }
    resolved
}

unsafe extern "system" fn worker(_parameter: *mut c_void) -> u32 {
    let directory = loader_dir();
    let config = config::Config::load(directory.as_deref());
    let logging = log::init(&config.log_candidates);

    let version = env!("CARGO_PKG_VERSION");
    log::info(&format!("Parsec Orbit {version} loader mapped"));
    for (path, code) in &logging.rejected {
        log::warn(&format!(
            "log path {} not writable (Win32 error {code}), trying the next one",
            path.display()
        ));
    }
    match &logging.path {
        Some(path) => log::debug(&format!("logging to {}", path.display())),
        None => log::error("no writable log file found; lines go to the debugger output only"),
    }

    if !log::process_name().eq_ignore_ascii_case(config::CLIENT_PROCESS) && !config.any_host {
        log::info(&format!(
            "host is {}, not {}: forwarding bcrypt without loading plugins (PARSEC_ORBIT_HOST=any overrides)",
            log::process_name(),
            config::CLIENT_PROCESS
        ));
        return 0;
    }

    if let Some(port) = config.devtools_port {
        webview::prepare(port);
    }

    let resolved = pre_resolve();
    log::info(&format!(
        "real bcrypt.dll at 0x{:X}, {resolved}/{} exports ready",
        real_bcrypt() as usize,
        SLOTS.len()
    ));

    // Let the client finish its own start-up before plugin images are mapped.
    std::thread::sleep(PLUGIN_WARMUP);

    let summary = plugin::scan(&config);
    log::info(&format!(
        "ready: {} loaded, {} skipped, {} failed",
        summary.loaded, summary.skipped, summary.failed
    ));
    0
}

#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn DllMain(module: HMODULE, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == 1 {
        SELF_MODULE.store(module as isize, Ordering::SeqCst);
        unsafe {
            DisableThreadLibraryCalls(module);
            // No allocation, no I/O and no module loading here: this runs with
            // the loader lock held.
            CreateThread(
                std::ptr::null(),
                0,
                Some(worker),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
            );
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{pe_export_names, system32};

    type OpenAlgorithm =
        unsafe extern "system" fn(*mut *mut c_void, *const u16, *const u16, u32) -> i32;
    type CloseAlgorithm = unsafe extern "system" fn(*mut c_void, u32) -> i32;
    type GenRandom = unsafe extern "system" fn(*mut c_void, *mut u8, u32, u32) -> i32;
    type Hash =
        unsafe extern "system" fn(*mut c_void, *mut u8, u32, *mut u8, u32, *mut u8, u32) -> i32;

    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 2;
    const SHA256_ABC: [u8; 32] = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
        0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00,
        0x15, 0xad,
    ];

    fn wide_str(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    // ---- export table -----------------------------------------------------

    #[test]
    fn export_tables_have_matching_lengths_and_sane_names() {
        assert_eq!(NAMES.len(), SLOTS.len());
        assert_eq!(NAMES.len(), 60, "the header comment of exports.rs says 60");
        let mut seen = std::collections::HashSet::new();
        for name in NAMES {
            assert!(name.starts_with("BCrypt"), "{name}");
            assert!(name.chars().all(|c| c.is_ascii_alphanumeric()), "{name}");
            assert!(seen.insert(name), "{name} is listed twice");
        }
    }

    /// The linker sorts a DLL's export name table; the thunk checks in
    /// `parsec-orbit-e2e` rely on `NAMES` being in that same order.
    #[test]
    fn export_names_are_sorted_like_a_pe_name_table() {
        for pair in NAMES.windows(2) {
            assert!(
                pair[0] < pair[1],
                "{} must sort before {}",
                pair[0],
                pair[1]
            );
        }
    }

    /// Every export of the real bcrypt.dll must exist in the proxy: a module
    /// in the client that imports one the proxy lacks fails to start.
    #[test]
    fn every_export_of_the_system_bcrypt_is_mirrored() {
        let system = pe_export_names(&system32("bcrypt.dll"));
        assert!(!system.is_empty());
        let missing: Vec<&String> = system
            .iter()
            .filter(|name| !NAMES.contains(&name.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "bcrypt.dll gained exports the loader does not forward: {missing:?}; \
             run scripts/gen-bcrypt-exports.py"
        );

        // The other direction is tolerated (older Windows builds lack the
        // newest functions; the resolver reports those lazily), but say so.
        for name in NAMES {
            if !system.iter().any(|existing| existing == name) {
                eprintln!("note: this Windows build has no {name}");
            }
        }
    }

    // ---- DllMain -----------------------------------------------------------

    #[test]
    fn dll_main_only_starts_work_for_process_attach() {
        for reason in [0u32, 2, 3] {
            assert_eq!(
                DllMain(std::ptr::null_mut(), reason, std::ptr::null_mut()),
                1
            );
        }
    }

    #[test]
    fn own_path_is_unknown_until_dll_main_recorded_the_module() {
        // The unit tests run in an executable, so DllMain never ran.
        assert_eq!(SELF_MODULE.load(Ordering::SeqCst), 0);
        assert_eq!(own_path(), None);
        assert_eq!(loader_dir(), None);
    }

    #[test]
    fn wide_is_nul_terminated() {
        assert_eq!(wide("ab"), [0x61, 0x62, 0]);
        assert_eq!(wide(""), [0]);
    }

    // ---- resolver -----------------------------------------------------------

    fn index_of(name: &str) -> usize {
        NAMES
            .iter()
            .position(|candidate| *candidate == name)
            .unwrap()
    }

    fn real_address(name: &str) -> usize {
        let real = real_bcrypt();
        assert!(!real.is_null(), "System32\\bcrypt.dll must load");
        let symbol = format!("{name}\0");
        unsafe { GetProcAddress(real, symbol.as_ptr()) }.expect("export of the real bcrypt")
            as usize
    }

    /// One test, because the slots are process-wide and the steps depend on
    /// each other.
    #[test]
    fn slots_are_filled_lazily_and_pre_resolve_fills_the_rest() {
        let real = real_bcrypt();
        assert!(!real.is_null());
        assert_eq!(real, real_bcrypt(), "the real module is loaded once");
        assert_ne!(real as usize, 0);
        // Not this proxy: the real module lives in System32.
        assert_ne!(real as usize, SELF_MODULE.load(Ordering::SeqCst) as usize);

        for slot in SLOTS.iter() {
            slot.store(0, Ordering::SeqCst);
        }

        // A single slot is resolved on demand, to the real function.
        let index = index_of("BCryptGenRandom");
        let slot = &SLOTS[index] as *const AtomicUsize as *mut AtomicUsize;
        let expected = real_address("BCryptGenRandom");
        assert_eq!(unsafe { resolve_one(slot) } as usize, expected);
        assert_eq!(SLOTS[index].load(Ordering::SeqCst), expected);

        // An already resolved slot short-circuits. Uses a function no other
        // test calls: a thunk that jumped to this fake address would crash.
        let unused = index_of("BCryptUnregisterProvider");
        let unused_slot = &SLOTS[unused] as *const AtomicUsize as *mut AtomicUsize;
        SLOTS[unused].store(0x1234, Ordering::SeqCst);
        assert_eq!(unsafe { resolve_one(unused_slot) } as usize, 0x1234);
        SLOTS[unused].store(0, Ordering::SeqCst);

        // Everything else is filled by the up-front pass.
        let resolved = pre_resolve();
        let system = pe_export_names(&system32("bcrypt.dll"));
        let available = NAMES
            .iter()
            .filter(|name| system.iter().any(|s| s == *name))
            .count();
        assert_eq!(resolved, available);
        for (name, slot) in NAMES.iter().zip(SLOTS.iter()) {
            let value = slot.load(Ordering::SeqCst);
            if system.iter().any(|s| s == name) {
                assert_eq!(value, real_address(name), "{name}");
            } else {
                assert_eq!(value, 0, "{name} does not exist on this Windows build");
            }
        }
    }

    // ---- the naked thunks, called in-process -------------------------------

    /// The fourth argument travels in `r9`; the stub once clobbered it, which
    /// made the system-preferred-RNG call fail with `STATUS_INVALID_HANDLE`.
    #[test]
    fn thunk_keeps_the_fourth_argument_register() {
        let gen: GenRandom = unsafe { std::mem::transmute(exports::BCryptGenRandom as *const ()) };
        let mut buffer = [0u8; 64];
        let status = unsafe {
            gen(
                std::ptr::null_mut(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        assert_eq!(
            status, 0,
            "BCryptGenRandom through the thunk returned 0x{status:X}"
        );
        assert!(
            buffer.iter().any(|byte| *byte != 0),
            "no random bytes were written"
        );

        // Without the flag the real function demands a valid provider handle.
        // Getting that error (and not a crash) proves the arguments arrive
        // untouched in both situations.
        let status = unsafe { gen(std::ptr::null_mut(), buffer.as_mut_ptr(), 4, 0) };
        assert_ne!(status, 0);
    }

    /// `BCryptHash` takes seven arguments, three of them on the stack. The
    /// SHA-256 of "abc" is a known answer, so a wrong stack layout shows up as
    /// a wrong digest or an error status.
    #[test]
    fn thunk_forwards_stack_arguments_and_returns_the_known_digest() {
        let open: OpenAlgorithm =
            unsafe { std::mem::transmute(exports::BCryptOpenAlgorithmProvider as *const ()) };
        let hash: Hash = unsafe { std::mem::transmute(exports::BCryptHash as *const ()) };
        let close: CloseAlgorithm =
            unsafe { std::mem::transmute(exports::BCryptCloseAlgorithmProvider as *const ()) };

        let mut algorithm: *mut c_void = std::ptr::null_mut();
        let name = wide_str("SHA256");
        assert_eq!(
            unsafe { open(&mut algorithm, name.as_ptr(), std::ptr::null(), 0) },
            0
        );
        assert!(!algorithm.is_null());

        let mut digest = [0u8; 32];
        let mut message = *b"abc";
        let status = unsafe {
            hash(
                algorithm,
                std::ptr::null_mut(),
                0,
                message.as_mut_ptr(),
                message.len() as u32,
                digest.as_mut_ptr(),
                digest.len() as u32,
            )
        };
        assert_eq!(status, 0, "BCryptHash returned 0x{status:X}");
        assert_eq!(digest, SHA256_ABC);

        assert_eq!(unsafe { close(algorithm, 0) }, 0);
    }

    #[test]
    fn thunks_keep_working_after_the_slot_is_resolved() {
        // Second call takes the fast path through the resolved slot.
        for _ in 0..3 {
            thunk_keeps_the_fourth_argument_register();
        }
    }
}

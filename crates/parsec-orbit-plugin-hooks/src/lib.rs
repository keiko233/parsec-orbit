//! Hooks - official Parsec Orbit plugin.
//!
//! Provides `parsec-orbit.hooks`: inline hooks on functions of the client
//! process. A hook overwrites the start of the target with a jump to a
//! detour and builds a trampoline that runs the displaced instructions and
//! continues in the original function ([`retour`] does the instruction
//! relocation). Hooks are installed disabled and cost nothing until enabled.
//!
//! This is the sharpest tool a plugin can ask for: a wrong target or detour
//! crashes the client. The plugin only provides the mechanism.

// Windows-only; the macOS import-rebind backend lands in M2 (ADR 0009).
#![cfg(windows)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard, OnceLock};

use parsec_orbit_plugin_sdk_std::prelude::*;
use parsec_orbit_plugin_sdk_std::services::Hooks;
use parsec_orbit_plugin_sdk_std::{
    ParsecOrbitHooksV1, ParsecOrbitInterfaceHeader, ERR_EXCEPTION, ERR_EXISTS, ERR_INVALID,
    ERR_NOT_FOUND, OK,
};
use retour::RawDetour;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

struct Hook {
    detour: RawDetour,
    target: usize,
}

// Safety: a `RawDetour` is plain addresses and the code patch they describe;
// the registry lock serialises every use.
unsafe impl Send for Hook {}

#[derive(Default)]
struct Registry {
    next: i64,
    hooks: HashMap<i64, Hook>,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Address of `symbol` in the loaded module `module`.
pub fn resolve_symbol(module: &str, symbol: &str) -> Option<*const c_void> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = std::ffi::OsStr::new(module)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let symbol = std::ffi::CString::new(symbol).ok()?;
    // Safety: both strings are NUL terminated; the module stays loaded
    // because `GetModuleHandleW` does not hand out a reference.
    let handle = unsafe { GetModuleHandleW(wide.as_ptr()) };
    if handle.is_null() {
        return None;
    }
    unsafe { GetProcAddress(handle, symbol.as_ptr() as *const u8) }
        .map(|function| function as *const c_void)
}

/// Prepares a hook; returns its id and the trampoline.
///
/// # Safety
/// `target` is the start of a function and `detour` has its exact signature
/// and calling convention.
pub unsafe fn install_hook(
    target: *const c_void,
    detour: *const c_void,
) -> Result<(i64, *const c_void), i32> {
    if target.is_null() || detour.is_null() || target == detour {
        return Err(ERR_INVALID);
    }
    let mut registry = registry();
    if registry
        .hooks
        .values()
        .any(|hook| hook.target == target as usize)
    {
        return Err(ERR_EXISTS);
    }
    let raw = unsafe { RawDetour::new(target as *const (), detour as *const ()) }
        .map_err(|_| ERR_EXCEPTION)?;
    let trampoline = raw.trampoline() as *const () as *const c_void;
    registry.next += 1;
    let id = registry.next;
    registry.hooks.insert(
        id,
        Hook {
            detour: raw,
            target: target as usize,
        },
    );
    Ok((id, trampoline))
}

fn with_hook(id: i64, f: impl FnOnce(&Hook) -> Result<(), retour::Error>) -> i32 {
    let registry = registry();
    match registry.hooks.get(&id) {
        Some(hook) => f(hook).map_or(ERR_EXCEPTION, |()| OK),
        None => ERR_NOT_FOUND,
    }
}

pub fn enable_hook(id: i64) -> i32 {
    // Safety: the detour was accepted by `install_hook`.
    with_hook(id, |hook| unsafe { hook.detour.enable() })
}

pub fn disable_hook(id: i64) -> i32 {
    with_hook(id, |hook| unsafe { hook.detour.disable() })
}

pub fn remove_hook(id: i64) -> i32 {
    let mut registry = registry();
    match registry.hooks.remove(&id) {
        Some(hook) => {
            // Dropping a `RawDetour` disables it.
            drop(hook);
            OK
        }
        None => ERR_NOT_FOUND,
    }
}

unsafe fn utf8<'a>(pointer: *const u8, len: usize) -> Option<&'a str> {
    if pointer.is_null() {
        return None;
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(pointer, len) }).ok()
}

unsafe extern "system" fn resolve(
    module: *const u8,
    module_len: usize,
    symbol: *const u8,
    symbol_len: usize,
) -> *const c_void {
    let (Some(module), Some(symbol)) = (unsafe { utf8(module, module_len) }, unsafe {
        utf8(symbol, symbol_len)
    }) else {
        return std::ptr::null();
    };
    resolve_symbol(module, symbol).unwrap_or(std::ptr::null())
}

unsafe extern "system" fn install(
    target: *const c_void,
    detour: *const c_void,
    original: *mut *const c_void,
) -> i64 {
    match unsafe { install_hook(target, detour) } {
        Ok((id, trampoline)) => {
            if !original.is_null() {
                unsafe { *original = trampoline };
            }
            id
        }
        Err(code) => code as i64,
    }
}

unsafe extern "system" fn enable(hook: i64) -> i32 {
    enable_hook(hook)
}

unsafe extern "system" fn disable(hook: i64) -> i32 {
    disable_hook(hook)
}

unsafe extern "system" fn remove(hook: i64) -> i32 {
    remove_hook(hook)
}

/// The table registered under `parsec-orbit.hooks`.
pub static HOOKS: ParsecOrbitHooksV1 = ParsecOrbitHooksV1 {
    header: ParsecOrbitInterfaceHeader::new(std::mem::size_of::<ParsecOrbitHooksV1>(), 1, 0),
    resolve: Some(resolve),
    install: Some(install),
    enable: Some(enable),
    disable: Some(disable),
    remove: Some(remove),
};

#[derive(Default)]
struct HooksPlugin;

impl Plugin for HooksPlugin {
    fn init(&mut self, cx: &Context) -> Result {
        info!(cx, "hooks {} starting", env!("CARGO_PKG_VERSION"));
        cx.register::<Hooks>(&HOOKS)?;
        Ok(())
    }
}

parsec_orbit_plugin_sdk_std::declare_plugin! {
    id = "com.parsec-orbit.hooks",
    name = "Hooks",
    version = "0.1.0",
    author = "Parsec Orbit contributors",
    description = "Provides parsec-orbit.hooks: inline hooks on functions of the client process",
    license = "MIT",
    plugin = HooksPlugin,
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk_std::services::HooksClient;
    use parsec_orbit_plugin_sdk_std::testing::MockHost;
    use std::sync::atomic::{AtomicI32, Ordering};

    // Targets the optimiser must neither inline nor merge: each has its own
    // body and is only called through a pointer.
    #[inline(never)]
    extern "system" fn add_one(x: i32) -> i32 {
        std::hint::black_box(x) + 1
    }

    #[inline(never)]
    extern "system" fn add_ten(x: i32) -> i32 {
        std::hint::black_box(x) + 10
    }

    #[inline(never)]
    extern "system" fn times_two(x: i32) -> i32 {
        std::hint::black_box(x) * 2
    }

    #[inline(never)]
    extern "system" fn minus_one(x: i32) -> i32 {
        std::hint::black_box(x) - 1
    }

    /// Calls through a pointer so the call cannot be resolved at compile time.
    fn call(function: extern "system" fn(i32) -> i32, x: i32) -> i32 {
        let pointer: extern "system" fn(i32) -> i32 = std::hint::black_box(function);
        pointer(x)
    }

    static ORIGINAL_OF_ADD_ONE: AtomicI32 = AtomicI32::new(0);
    static ORIGINAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    extern "system" fn detour_of_add_one(x: i32) -> i32 {
        ORIGINAL_OF_ADD_ONE.fetch_add(1, Ordering::SeqCst);
        // The original still works through the trampoline.
        let original: extern "system" fn(i32) -> i32 =
            unsafe { std::mem::transmute(ORIGINAL.load(Ordering::SeqCst)) };
        original(x) + 1000
    }

    #[test]
    fn a_hook_redirects_calls_and_the_trampoline_reaches_the_original() {
        let target = add_one as extern "system" fn(i32) -> i32;
        assert_eq!(call(target, 1), 2);
        let (id, trampoline) = unsafe {
            install_hook(target as *const c_void, detour_of_add_one as *const c_void).unwrap()
        };
        ORIGINAL.store(trampoline as usize, Ordering::SeqCst);

        // Installed, not enabled: nothing changes.
        assert_eq!(call(target, 1), 2);
        assert_eq!(enable_hook(id), OK);
        assert_eq!(call(target, 1), 1002, "detour(original(1) + 1000)");
        assert!(ORIGINAL_OF_ADD_ONE.load(Ordering::SeqCst) >= 1);
        assert_eq!(disable_hook(id), OK);
        assert_eq!(call(target, 1), 2, "disabled again");
        assert_eq!(enable_hook(id), OK);
        assert_eq!(call(target, 5), 1006);
        assert_eq!(remove_hook(id), OK);
        assert_eq!(call(target, 1), 2, "removing disables");
        assert_eq!(enable_hook(id), ERR_NOT_FOUND);
        assert_eq!(remove_hook(id), ERR_NOT_FOUND);
    }

    extern "system" fn detour_of_add_ten(_: i32) -> i32 {
        -1
    }

    #[test]
    fn a_function_can_only_be_hooked_once_and_bad_arguments_are_refused() {
        let target = add_ten as extern "system" fn(i32) -> i32;
        let detour = detour_of_add_ten as *const c_void;
        let (id, _) = unsafe { install_hook(target as *const c_void, detour).unwrap() };
        assert_eq!(
            unsafe { install_hook(target as *const c_void, detour) }.unwrap_err(),
            ERR_EXISTS
        );
        assert_eq!(
            unsafe { install_hook(std::ptr::null(), detour) }.unwrap_err(),
            ERR_INVALID
        );
        assert_eq!(
            unsafe { install_hook(target as *const c_void, std::ptr::null()) }.unwrap_err(),
            ERR_INVALID
        );
        assert_eq!(
            unsafe { install_hook(target as *const c_void, target as *const c_void) }.unwrap_err(),
            ERR_INVALID
        );
        assert_eq!(remove_hook(id), OK);
        // Once removed the function can be hooked again.
        let (again, _) = unsafe { install_hook(target as *const c_void, detour).unwrap() };
        assert_eq!(enable_hook(again), OK);
        assert_eq!(call(target, 0), -1);
        assert_eq!(remove_hook(again), OK);
        assert_eq!(call(target, 0), 10);
    }

    extern "system" fn detour_of_times_two(x: i32) -> i32 {
        x
    }

    #[test]
    fn the_service_works_through_the_sdk_client() {
        let host = MockHost::new("com.parsec-orbit.hooks");
        host.init::<HooksPlugin>().1.unwrap();
        let consumer = MockHost::new("com.example.hooker");
        let hooks = HooksClient::new(&consumer.context()).expect("the service");

        let target = times_two as extern "system" fn(i32) -> i32;
        let (id, original) = unsafe {
            hooks
                .install(
                    target as *const c_void,
                    detour_of_times_two as *const c_void,
                )
                .unwrap()
        };
        assert!(!original.is_null());
        let original: extern "system" fn(i32) -> i32 = unsafe { std::mem::transmute(original) };
        hooks.enable(id).unwrap();
        assert_eq!(call(target, 21), 21, "the detour answers");
        assert_eq!(original(21), 42, "the trampoline still doubles");
        hooks.disable(id).unwrap();
        assert_eq!(call(target, 21), 42);
        hooks.remove(id).unwrap();
        assert_eq!(hooks.enable(id).unwrap_err().code(), ERR_NOT_FOUND);
    }

    #[test]
    fn symbols_are_resolved_in_loaded_modules() {
        let address = resolve_symbol("kernel32.dll", "GetCurrentProcessId").expect("exported");
        let function: extern "system" fn() -> u32 = unsafe { std::mem::transmute(address) };
        assert_eq!(function(), std::process::id());
        assert!(resolve_symbol("kernel32.dll", "NoSuchFunction").is_none());
        assert!(resolve_symbol("no-such-module.dll", "GetCurrentProcessId").is_none());

        let host = MockHost::new("com.parsec-orbit.hooks");
        host.init::<HooksPlugin>().1.unwrap();
        let consumer = MockHost::new("com.example.hooker");
        let hooks = HooksClient::new(&consumer.context()).unwrap();
        assert_eq!(
            hooks.resolve("kernel32.dll", "GetCurrentProcessId"),
            Some(address)
        );
        assert_eq!(hooks.resolve("kernel32.dll", "NoSuchFunction"), None);
    }

    extern "system" fn detour_of_minus_one(_: i32) -> i32 {
        0
    }

    /// A real system function, hooked from a thread other than the one that
    /// installed the hook: the patch is process wide. (An obscure function:
    /// the other tests, and the test harness, must not call it.)
    #[test]
    fn a_system_function_can_be_hooked_and_restored() {
        let target = resolve_symbol("kernel32.dll", "GetCurrentProcessorNumber").unwrap();
        extern "system" fn fake_processor() -> u32 {
            777
        }
        let (id, _) = unsafe { install_hook(target, fake_processor as *const c_void).unwrap() };
        let function: extern "system" fn() -> u32 = unsafe { std::mem::transmute(target) };
        assert!(function() < 4096, "the real function answers first");
        assert_eq!(enable_hook(id), OK);
        let seen = std::thread::spawn(move || function()).join().unwrap();
        assert_eq!(seen, 777);
        assert_eq!(remove_hook(id), OK);
        assert!(function() < 4096, "and again after the hook is gone");
        let _ = minus_one as extern "system" fn(i32) -> i32;
        let _ = detour_of_minus_one as extern "system" fn(i32) -> i32;
    }

    fn manifest_value(key: &str) -> String {
        include_str!("../plugin.toml")
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once('=')?;
                (name.trim() == key).then(|| value.trim().trim_matches('"').to_string())
            })
            .unwrap_or_else(|| panic!("plugin.toml has no `{key}`"))
    }

    #[test]
    fn descriptor_and_manifest_agree() {
        let text = |pointer: *const u8| {
            unsafe { std::ffi::CStr::from_ptr(pointer as *const _) }
                .to_str()
                .unwrap()
                .to_string()
        };
        let desc = unsafe { &*ParsecOrbitPluginQuery(parsec_orbit_plugin_sdk_std::ABI_VERSION) };
        assert_eq!(text(desc.id), manifest_value("id"));
        assert_eq!(text(desc.name), manifest_value("name"));
        assert_eq!(text(desc.version), manifest_value("version"));
        assert_eq!(text(desc.description), manifest_value("description"));
        assert_eq!(manifest_value("entry"), "parsec_orbit_plugin_hooks.dll");
    }
}

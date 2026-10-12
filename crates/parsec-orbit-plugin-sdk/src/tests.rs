use super::*;
use core::mem::size_of;
use std::ffi::CStr as StdCStr;
use std::sync::Mutex;

fn host_with(log: Option<unsafe extern "system" fn(u32, *const u8, usize)>) -> ParsecOrbitHost {
    ParsecOrbitHost {
        log,
        loader_version: b"1.2.3\0".as_ptr(),
        ..ParsecOrbitHost::blank()
    }
}

#[test]
fn the_abi_crate_is_reexported_both_ways() {
    assert_eq!(abi::ABI_VERSION, ABI_VERSION);
    assert_eq!(
        size_of::<abi::ParsecOrbitHost>(),
        size_of::<ParsecOrbitHost>()
    );
}

static INIT_SAW_HOST: Mutex<Option<u32>> = Mutex::new(None);
static SHUTDOWN_CALLS: Mutex<u32> = Mutex::new(0);

unsafe extern "system" fn sample_init(host: *const ParsecOrbitHost) -> i32 {
    *INIT_SAW_HOST.lock().unwrap() = Some(unsafe { (*host).abi_version });
    7
}

fn sample_shutdown() -> i32 {
    *SHUTDOWN_CALLS.lock().unwrap() += 1;
    9
}

mod exported {
    crate::parsec_orbit_plugin! {
        id = "com.example.sdk-test",
        name = "SDK Test",
        version = "9.8.7",
        author = "Tester",
        description = "Declared by the macro under test",
        license = "MIT",
        init = super::sample_init,
        shutdown = super::sample_shutdown,
        capabilities = 0xA5,
    }
}

fn text(pointer: *const u8) -> &'static str {
    unsafe { StdCStr::from_ptr(pointer as *const _) }
        .to_str()
        .unwrap()
}

#[test]
fn macro_exports_a_valid_descriptor() {
    let query: QueryFn = exported::ParsecOrbitPluginQuery;
    let desc = unsafe { &*query(ABI_VERSION) };
    assert_eq!(desc.abi_version, ABI_VERSION);
    assert_eq!(
        desc.struct_size as usize,
        size_of::<ParsecOrbitPluginDesc>()
    );
    assert_eq!(text(desc.id), "com.example.sdk-test");
    assert_eq!(text(desc.name), "SDK Test");
    assert_eq!(text(desc.version), "9.8.7");
    assert_eq!(text(desc.author), "Tester");
    assert_eq!(text(desc.description), "Declared by the macro under test");
    assert_eq!(text(desc.license), "MIT");
    assert_eq!(desc.capabilities, 0xA5);
}

#[test]
fn macro_descriptor_is_stable_across_calls_and_host_versions() {
    let first = unsafe { exported::ParsecOrbitPluginQuery(ABI_VERSION) };
    let second = unsafe { exported::ParsecOrbitPluginQuery(ABI_VERSION + 41) };
    // A host from the future still gets the descriptor, so the loader can
    // explain the mismatch itself.
    assert!(!first.is_null());
    assert_eq!(first, second);
}

#[test]
fn macro_init_and_shutdown_forward_their_arguments_and_results() {
    let host = host_with(None);
    let init: InitFn = exported::ParsecOrbitPluginInit;
    assert_eq!(unsafe { init(&host) }, 7);
    assert_eq!(*INIT_SAW_HOST.lock().unwrap(), Some(ABI_VERSION));

    let shutdown: ShutdownFn = exported::ParsecOrbitPluginShutdown;
    assert_eq!(unsafe { shutdown() }, 9);
    assert_eq!(*SHUTDOWN_CALLS.lock().unwrap(), 1);
}

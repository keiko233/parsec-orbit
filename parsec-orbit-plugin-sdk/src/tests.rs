use super::*;
use core::mem::{align_of, offset_of, size_of};
use std::ffi::CStr as StdCStr;
use std::string::String;
use std::sync::Mutex;
use std::vec::Vec;

// The layouts below are the ABI. Plugins built against an older SDK read these
// offsets directly, so a failing assertion here means a breaking change, not a
// test that needs updating.
#[test]
#[cfg(target_pointer_width = "64")]
fn descriptor_layout_is_frozen() {
    assert_eq!(offset_of!(ParsecOrbitPluginDesc, abi_version), 0);
    assert_eq!(offset_of!(ParsecOrbitPluginDesc, struct_size), 4);
    assert_eq!(offset_of!(ParsecOrbitPluginDesc, id), 8);
    assert_eq!(offset_of!(ParsecOrbitPluginDesc, name), 16);
    assert_eq!(offset_of!(ParsecOrbitPluginDesc, version), 24);
    assert_eq!(offset_of!(ParsecOrbitPluginDesc, author), 32);
    assert_eq!(offset_of!(ParsecOrbitPluginDesc, description), 40);
    assert_eq!(offset_of!(ParsecOrbitPluginDesc, license), 48);
    assert_eq!(offset_of!(ParsecOrbitPluginDesc, capabilities), 56);
    assert_eq!(size_of::<ParsecOrbitPluginDesc>(), 64);
    assert_eq!(align_of::<ParsecOrbitPluginDesc>(), 8);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn host_layout_is_frozen() {
    assert_eq!(offset_of!(ParsecOrbitHost, abi_version), 0);
    assert_eq!(offset_of!(ParsecOrbitHost, struct_size), 4);
    assert_eq!(offset_of!(ParsecOrbitHost, log), 8);
    assert_eq!(offset_of!(ParsecOrbitHost, query_interface), 16);
    assert_eq!(offset_of!(ParsecOrbitHost, loader_version), 24);
    assert_eq!(offset_of!(ParsecOrbitHost, reserved), 32);
    assert_eq!(size_of::<ParsecOrbitHost>(), 96);
}

#[test]
fn optional_function_pointers_use_the_null_pointer_niche() {
    // The C side passes plain (nullable) function pointers.
    assert_eq!(
        size_of::<Option<unsafe extern "system" fn(u32, *const u8, usize)>>(),
        size_of::<usize>()
    );
    assert_eq!(
        size_of::<Option<unsafe extern "system" fn(*const u8) -> *mut c_void>>(),
        size_of::<usize>()
    );
}

#[test]
fn abi_constants_are_stable() {
    assert_eq!(ABI_VERSION, 1);
    assert_eq!(
        [LOG_TRACE, LOG_DEBUG, LOG_INFO, LOG_WARN, LOG_ERROR],
        [0, 1, 2, 3, 4]
    );
    assert_eq!(EXPORT_QUERY, "ParsecOrbitPluginQuery");
    assert_eq!(EXPORT_INIT, "ParsecOrbitPluginInit");
    assert_eq!(EXPORT_SHUTDOWN, "ParsecOrbitPluginShutdown");
    assert_eq!(EXPORT_LEGACY_INIT, "ParsecPluginInit");
}

/// `docs/spec/plugin-abi.md` is the reference other languages implement against, so
/// it must not drift from the constants.
#[test]
fn abi_document_matches_the_sdk() {
    let doc = include_str!("../../docs/spec/plugin-abi.md");
    for export in [
        EXPORT_QUERY,
        EXPORT_INIT,
        EXPORT_SHUTDOWN,
        EXPORT_LEGACY_INIT,
    ] {
        assert!(
            doc.contains(export),
            "docs/spec/plugin-abi.md never mentions {export}"
        );
    }
    let version_line = std::format!("#define PARSEC_ORBIT_ABI_VERSION {ABI_VERSION}");
    assert!(doc.contains(&version_line), "missing `{version_line}`");
    assert!(doc.contains("0=trace 1=debug 2=info 3=warn 4=error"));

    // Every descriptor field is documented, in declaration order.
    let mut from = doc
        .find("typedef struct ParsecOrbitPluginDesc")
        .expect("descriptor struct is documented");
    for field in [
        "abi_version;",
        "struct_size;",
        "*id;",
        "*name;",
        "*version;",
        "*author;",
        "*description;",
        "*license;",
        "capabilities;",
    ] {
        let found = doc[from..]
            .find(field)
            .unwrap_or_else(|| panic!("descriptor field `{field}` missing or out of order"));
        from += found + field.len();
    }
}

static LOGGED: Mutex<Vec<(u32, String)>> = Mutex::new(Vec::new());

unsafe extern "system" fn record(level: u32, message: *const u8, len: usize) {
    let bytes = unsafe { core::slice::from_raw_parts(message, len) };
    LOGGED
        .lock()
        .unwrap()
        .push((level, String::from(core::str::from_utf8(bytes).unwrap())));
}

fn host_with(log: Option<unsafe extern "system" fn(u32, *const u8, usize)>) -> ParsecOrbitHost {
    ParsecOrbitHost {
        abi_version: ABI_VERSION,
        struct_size: size_of::<ParsecOrbitHost>() as u32,
        log,
        query_interface: None,
        loader_version: b"1.2.3\0".as_ptr(),
        reserved: [0; 8],
    }
}

#[test]
fn host_log_passes_level_and_exact_bytes() {
    let host = host_with(Some(record));
    // Not NUL terminated, multi-byte UTF-8: the callback gets (ptr, len).
    host.log(LOG_WARN, "log \u{30ed}\u{30b0} line");
    host.log(LOG_ERROR, "");
    let logged = LOGGED.lock().unwrap();
    assert!(logged.contains(&(LOG_WARN, String::from("log \u{30ed}\u{30b0} line"))));
    assert!(logged.contains(&(LOG_ERROR, String::new())));
}

#[test]
fn host_log_without_callback_is_a_no_op() {
    host_with(None).log(LOG_INFO, "nobody is listening");
}

#[test]
fn loader_version_reads_the_c_string() {
    assert_eq!(host_with(None).loader_version(), Some("1.2.3"));
}

#[test]
fn loader_version_is_none_for_null_or_invalid_utf8() {
    let mut host = host_with(None);
    host.loader_version = core::ptr::null();
    assert_eq!(host.loader_version(), None);

    host.loader_version = b"\xff\xfe\0".as_ptr();
    assert_eq!(host.loader_version(), None);
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

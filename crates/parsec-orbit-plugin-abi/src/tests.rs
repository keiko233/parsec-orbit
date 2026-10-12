use super::*;
use core::mem::{align_of, offset_of, size_of};
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
    // Revision 1 ended here; everything below is appended after it.
    assert_eq!(offset_of!(ParsecOrbitHost, plugin_id), HOST_REVISION_1_SIZE);
    assert_eq!(HOST_REVISION_1_SIZE, 96);
    assert_eq!(offset_of!(ParsecOrbitHost, plugin_dir), 104);
    assert_eq!(offset_of!(ParsecOrbitHost, data_dir), 112);
    assert_eq!(offset_of!(ParsecOrbitHost, log_ex), 120);
    assert_eq!(offset_of!(ParsecOrbitHost, register_interface), 128);
    assert_eq!(offset_of!(ParsecOrbitHost, unregister_interface), 136);
    assert_eq!(offset_of!(ParsecOrbitHost, reserved2), 144);
    assert_eq!(size_of::<ParsecOrbitHost>(), 208);
    assert_eq!(HOST_REVISION_2_SIZE, 208);
}

#[test]
#[cfg(target_pointer_width = "32")]
fn host_size_constants_match_the_32_bit_layout() {
    assert_eq!(HOST_REVISION_1_SIZE, 52);
    assert_eq!(HOST_REVISION_2_SIZE, 108);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn interface_layouts_are_frozen() {
    assert_eq!(size_of::<ParsecOrbitInterfaceHeader>(), 8);
    assert_eq!(offset_of!(ParsecOrbitInterfaceHeader, size), 0);
    assert_eq!(offset_of!(ParsecOrbitInterfaceHeader, version), 4);

    assert_eq!(offset_of!(ParsecOrbitPluginLoadInfo, size), 0);
    assert_eq!(offset_of!(ParsecOrbitPluginLoadInfo, id), 8);
    assert_eq!(offset_of!(ParsecOrbitPluginLoadInfo, dir), 16);
    assert_eq!(offset_of!(ParsecOrbitPluginLoadInfo, entry), 24);
    assert_eq!(offset_of!(ParsecOrbitPluginLoadInfo, data_dir), 32);
    assert_eq!(offset_of!(ParsecOrbitPluginLoadInfo, manifest), 40);
    assert_eq!(offset_of!(ParsecOrbitPluginLoadInfo, manifest_len), 48);
    assert_eq!(size_of::<ParsecOrbitPluginLoadInfo>(), 56);

    assert_eq!(offset_of!(ParsecOrbitKindHandlerV1, header), 0);
    assert_eq!(offset_of!(ParsecOrbitKindHandlerV1, load_plugin), 8);
    assert_eq!(offset_of!(ParsecOrbitKindHandlerV1, unload_plugin), 16);
    assert_eq!(size_of::<ParsecOrbitKindHandlerV1>(), 24);
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
    assert_eq!(
        [
            OK,
            ERR_INVALID,
            ERR_EXISTS,
            ERR_NOT_FOUND,
            ERR_NOT_READY,
            ERR_TIMEOUT,
            ERR_EXCEPTION
        ],
        [0, -1, -2, -3, -4, -5, -6]
    );
    assert_eq!(INTERFACE_KIND_PREFIX, "parsec-orbit.kind.");
}

/// `docs/spec/plugin-abi.md` is the reference other languages implement against, so
/// it must not drift from the constants.
#[test]
fn abi_document_matches_the_definitions() {
    let doc = include_str!("../../../docs/spec/plugin-abi.md");
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

    // The host table and the revision 2 additions, in declaration order.
    let mut from = doc
        .find("typedef struct ParsecOrbitHost")
        .expect("host struct is documented");
    for field in [
        "abi_version;",
        "struct_size;",
        "(*log)",
        "(*query_interface)",
        "*loader_version;",
        "reserved[8];",
        "*plugin_id;",
        "*plugin_dir;",
        "*data_dir;",
        "(*log_ex)",
        "(*register_interface)",
        "(*unregister_interface)",
        "reserved2[8];",
    ] {
        let found = doc[from..]
            .find(field)
            .unwrap_or_else(|| panic!("host field `{field}` missing or out of order"));
        from += found + field.len();
    }
    for name in [
        "ParsecOrbitInterfaceHeader",
        "ParsecOrbitPluginLoadInfo",
        "ParsecOrbitKindHandlerV1",
        "ERR_NOT_FOUND",
        INTERFACE_KIND_PREFIX,
        INTERFACE_WEBVIEW,
        INTERFACE_JS,
        INTERFACE_HOOKS,
        INTERFACE_CONFIG,
        INTERFACE_MARKET,
    ] {
        assert!(
            doc.contains(name),
            "docs/spec/plugin-abi.md never mentions {name}"
        );
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
        log,
        loader_version: b"1.2.3\0".as_ptr(),
        ..ParsecOrbitHost::blank()
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

#[test]
fn a_blank_host_is_the_current_revision_with_nothing_filled_in() {
    let mut host = ParsecOrbitHost::blank();
    assert_eq!(host.abi_version, ABI_VERSION);
    assert_eq!(host.struct_size as usize, size_of::<ParsecOrbitHost>());
    assert!(host.has_revision_2());
    // A future SDK can still be talking to this exact revision 2 loader.
    host.struct_size = (HOST_REVISION_2_SIZE + 64) as u32;
    assert!(host.has_revision_2());
    assert!(host.log.is_none() && host.log_ex.is_none());
    assert!(host.query_interface.is_none());
    assert_eq!(host.plugin_id(), None);
    assert_eq!(host.reserved, [0; 8]);
    assert_eq!(host.reserved2, [0; 8]);
}

#[test]
fn revision_2_fields_are_hidden_from_a_revision_1_loader() {
    let mut host = ParsecOrbitHost {
        plugin_id: b"com.example.p\0".as_ptr(),
        plugin_dir: b"C:/p\0".as_ptr(),
        data_dir: b"C:/d\0".as_ptr(),
        ..ParsecOrbitHost::blank()
    };
    assert_eq!(host.plugin_id(), Some("com.example.p"));
    assert_eq!(host.plugin_dir(), Some("C:/p"));
    assert_eq!(host.data_dir(), Some("C:/d"));

    // An old loader hands over 96 bytes: whatever follows is not its memory.
    host.struct_size = HOST_REVISION_1_SIZE as u32;
    assert!(!host.has_revision_2());
    assert_eq!(host.plugin_id(), None);
    assert_eq!(host.plugin_dir(), None);
    assert_eq!(host.data_dir(), None);
    assert_eq!(host.register(c"x.y", &HEADER), ERR_NOT_READY);
}

static HEADER: ParsecOrbitInterfaceHeader = ParsecOrbitInterfaceHeader::new(8, 1, 0);

static EX_LOGGED: Mutex<Vec<(u32, String, String)>> = Mutex::new(Vec::new());

unsafe extern "system" fn record_ex(
    level: u32,
    target: *const u8,
    target_len: usize,
    message: *const u8,
    len: usize,
) {
    let target = unsafe { core::slice::from_raw_parts(target, target_len) };
    let message = unsafe { core::slice::from_raw_parts(message, len) };
    EX_LOGGED.lock().unwrap().push((
        level,
        String::from(core::str::from_utf8(target).unwrap()),
        String::from(core::str::from_utf8(message).unwrap()),
    ));
}

#[test]
fn log_with_target_uses_log_ex_and_falls_back_to_log() {
    let mut host = ParsecOrbitHost {
        log: Some(record),
        log_ex: Some(record_ex),
        ..ParsecOrbitHost::blank()
    };
    host.log_with_target(LOG_INFO, "com.example.t", "with a target");
    assert!(EX_LOGGED.lock().unwrap().contains(&(
        LOG_INFO,
        String::from("com.example.t"),
        String::from("with a target")
    )));

    host.struct_size = HOST_REVISION_1_SIZE as u32;
    host.log_with_target(LOG_WARN, "ignored", "plain fallback");
    assert!(LOGGED
        .lock()
        .unwrap()
        .contains(&(LOG_WARN, String::from("plain fallback"))));
}

unsafe extern "system" fn register_ok(
    host: *const ParsecOrbitHost,
    name: *const u8,
    table: *const ParsecOrbitInterfaceHeader,
) -> i32 {
    let name = unsafe { CStr::from_ptr(name as *const _) };
    assert!(!host.is_null() && !table.is_null());
    if name.to_bytes() == b"taken.name" {
        ERR_EXISTS
    } else {
        OK
    }
}

#[test]
fn register_passes_the_callers_own_table_and_reports_the_result() {
    let host = ParsecOrbitHost {
        register_interface: Some(register_ok),
        ..ParsecOrbitHost::blank()
    };
    assert_eq!(host.register(c"fresh.name", &HEADER), OK);
    assert_eq!(host.register(c"taken.name", &HEADER), ERR_EXISTS);
    assert_eq!(
        ParsecOrbitHost::blank().register(c"fresh.name", &HEADER),
        ERR_NOT_READY
    );
}

unsafe extern "system" fn query_known(name: *const u8) -> *mut c_void {
    let name = unsafe { CStr::from_ptr(name as *const _) };
    if name.to_bytes() == b"known.name" {
        &HEADER as *const ParsecOrbitInterfaceHeader as *mut c_void
    } else {
        core::ptr::null_mut()
    }
}

#[test]
fn interface_lookup_returns_the_header_or_none() {
    let host = ParsecOrbitHost {
        query_interface: Some(query_known),
        ..ParsecOrbitHost::blank()
    };
    let found = unsafe { host.interface(c"known.name") }.unwrap();
    assert_eq!((found.major(), found.minor()), (1, 0));
    assert!(unsafe { host.interface(c"other.name") }.is_none());
    assert!(unsafe { ParsecOrbitHost::blank().interface(c"known.name") }.is_none());
}

#[test]
fn interface_headers_pack_the_version_and_check_the_size() {
    let header = ParsecOrbitInterfaceHeader::new(40, 2, 7);
    assert_eq!(header.version, (2 << 16) | 7);
    assert_eq!((header.major(), header.minor()), (2, 7));
    assert!(header.covers(40));
    assert!(header.covers(8));
    assert!(!header.covers(41));
}

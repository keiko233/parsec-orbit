use super::*;
use core::fmt::Write as _;
use core::mem::size_of;
use std::ffi::CStr as StdCStr;
use std::string::ToString;
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

// ---- the deprecated export macro ---------------------------------------------

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

#[allow(deprecated)]
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

// ---- StackBuf -----------------------------------------------------------------

#[test]
fn stack_buffers_hold_what_fits() {
    let mut buffer = StackBuf::<32>::new();
    write!(buffer, "{} + {} = {}", 1, 2, 3).unwrap();
    assert_eq!(buffer.as_str(), "1 + 2 = 3");
    assert!(!buffer.truncated());
    assert_eq!(buffer.as_c_str().unwrap().to_bytes(), b"1 + 2 = 3");
}

#[test]
fn stack_buffers_cut_with_an_ellipsis() {
    let mut buffer = StackBuf::<16>::new();
    write!(buffer, "{}", "abcdefghijklmnopqrstuvwxyz").unwrap();
    assert!(buffer.truncated());
    assert_eq!(buffer.as_str(), "abcdefghijkl...");
    assert_eq!(buffer.as_str().len(), StackBuf::<16>::CAPACITY);
    // Later writes are ignored, the NUL terminator survives.
    write!(buffer, "more").unwrap();
    assert_eq!(buffer.as_str(), "abcdefghijkl...");
    assert_eq!(buffer.as_c_str().unwrap().to_bytes().len(), 15);
}

#[test]
fn stack_buffers_never_split_a_character() {
    // Each of these is three bytes; the cut must land between them.
    let mut buffer = StackBuf::<16>::new();
    write!(
        buffer,
        "{}",
        "\u{30ed}\u{30b0}\u{30ed}\u{30b0}\u{30ed}\u{30b0}"
    )
    .unwrap();
    assert!(buffer.truncated());
    assert!(core::str::from_utf8(buffer.as_str().as_bytes()).is_ok());
    assert!(buffer.as_str().ends_with("..."));
    assert!(buffer.as_str().len() <= StackBuf::<16>::CAPACITY);

    let mut exact = StackBuf::<16>::new();
    write!(exact, "{}", "x".repeat(15)).unwrap();
    assert!(!exact.truncated(), "exactly the capacity still fits");
    write!(exact, "y").unwrap();
    assert!(exact.truncated());
}

#[test]
fn stack_buffers_with_a_nul_have_no_c_string() {
    let mut buffer = StackBuf::<16>::new();
    write!(buffer, "a\0b").unwrap();
    assert!(buffer.as_c_str().is_none());
}

// ---- Error and Level ----------------------------------------------------------

#[test]
fn errors_never_carry_the_success_code() {
    assert_eq!(Error::new(0).code(), 1);
    assert_eq!(Error::with_message(0, "x").code(), 1);
    assert_eq!(Error::new(-3).code(), -3);
    assert_eq!(Error::new(5).message(), None);
    assert_eq!(Error::with_message(5, "nope").message(), Some("nope"));
}

#[test]
fn errors_display_their_message_and_code() {
    assert_eq!(Error::new(5).to_string(), "error code 5");
    assert_eq!(Error::with_message(5, "nope").to_string(), "nope (code 5)");
}

#[test]
fn levels_map_onto_the_abi_values() {
    assert_eq!(
        [
            Level::Trace,
            Level::Debug,
            Level::Info,
            Level::Warn,
            Level::Error
        ]
        .map(Level::to_abi),
        [LOG_TRACE, LOG_DEBUG, LOG_INFO, LOG_WARN, LOG_ERROR]
    );
    assert!(Level::Error > Level::Warn && Level::Trace < Level::Debug);
}

// ---- Context --------------------------------------------------------------------

#[test]
fn a_context_needs_at_least_the_revision_1_table() {
    let mut host = ParsecOrbitHost::blank();
    host.struct_size = HOST_REVISION_1_SIZE as u32 - 1;
    assert!(unsafe { Context::from_host(&host) }.is_none());
    assert!(unsafe { Context::from_host(core::ptr::null()) }.is_none());
    host.struct_size = HOST_REVISION_1_SIZE as u32;
    assert!(unsafe { Context::from_host(&host) }.is_some());
}

#[derive(Debug)]
struct Broken;

impl core::fmt::Display for Broken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("broken")
    }
}

impl core::error::Error for Broken {}

fn fails() -> Result {
    Err(Broken)?;
    Ok(())
}

#[test]
fn any_core_error_converts_with_the_question_mark() {
    let error = fails().unwrap_err();
    assert_eq!(error.code(), 1);
    assert_eq!(error.message(), Some("operation failed"));
}

// ---- UiClient -----------------------------------------------------------------

static UI2_SHOW_CALLED: Mutex<bool> = Mutex::new(false);
static UI2_HIDE_CALLED: Mutex<bool> = Mutex::new(false);
static UI2_LAST_PAGE: Mutex<Option<std::string::String>> = Mutex::new(None);

unsafe extern "system" fn fake_ui2_show() -> i32 {
    *UI2_SHOW_CALLED.lock().unwrap() = true;
    0
}

unsafe extern "system" fn fake_ui2_hide() -> i32 {
    *UI2_HIDE_CALLED.lock().unwrap() = true;
    0
}

unsafe extern "system" fn fake_ui2_is_visible() -> i32 {
    1
}

unsafe extern "system" fn fake_ui2_register_page(schema: *const u8, len: usize) -> i32 {
    let slice = unsafe { core::slice::from_raw_parts(schema, len) };
    *UI2_LAST_PAGE.lock().unwrap() = Some(std::string::String::from_utf8_lossy(slice).into_owned());
    0
}

#[test]
fn ui_client_v2_forwards_calls() {
    let table = ParsecOrbitUiV2 {
        header: ParsecOrbitInterfaceHeader::new(size_of::<ParsecOrbitUiV2>(), 2, 0),
        show: Some(fake_ui2_show),
        hide: Some(fake_ui2_hide),
        is_visible: Some(fake_ui2_is_visible),
        navigate: None,
        register_page: Some(fake_ui2_register_page),
        unregister_page: None,
        update_page: None,
        on_action: None,
        off_action: None,
    };
    let client =
        services::ui::UiClient::from_table(std::boxed::Box::leak(std::boxed::Box::new(table)));
    assert_eq!(client.is_visible().unwrap(), true);
    assert!(client.show().is_ok());
    assert!(*UI2_SHOW_CALLED.lock().unwrap());
    assert!(client.hide().is_ok());
    assert!(*UI2_HIDE_CALLED.lock().unwrap());
    assert!(client.register_page(r#"{"id":"test"}"#).is_ok());
    assert_eq!(
        UI2_LAST_PAGE.lock().unwrap().as_deref(),
        Some(r#"{"id":"test"}"#)
    );
}

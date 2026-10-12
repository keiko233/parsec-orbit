//! The SDK's `Context`, services and plugin glue, exercised through the mock
//! host (the SDK itself cannot link `std`, so these tests live here).

use core::mem::size_of;
use std::ffi::CStr as StdCStr;

use parsec_orbit_plugin_sdk_std::prelude::*;
use parsec_orbit_plugin_sdk_std::testing::MockHost;
use parsec_orbit_plugin_sdk_std::*;

#[test]
fn a_context_describes_its_plugin() {
    let host = MockHost::new("com.example.ctx")
        .with_plugin_dir(std::path::Path::new("C:/plugins/ctx"))
        .with_data_dir(std::path::Path::new("C:/data/ctx"));
    let cx = host.context();
    assert_eq!(cx.plugin_id(), "com.example.ctx");
    assert_eq!(cx.plugin_dir(), Some("C:/plugins/ctx"));
    assert_eq!(cx.data_dir(), Some("C:/data/ctx"));
    assert_eq!(cx.loader_version(), "0.0.0-mock");
    assert!(cx.abi_matches());
    assert!(cx.has_revision(1) && cx.has_revision(2) && !cx.has_revision(3));
}

#[test]
fn logging_goes_through_log_ex_with_the_plugin_id() {
    let host = MockHost::new("com.example.log");
    let cx = host.context();
    info!(cx, "{} and {}", "this", 2);
    warn!(cx, "careful");
    error!(cx, "bad");
    let logs = host.logs();
    assert_eq!(logs.len(), 3);
    assert_eq!(logs[0].level, LOG_INFO);
    assert_eq!(logs[0].target.as_deref(), Some("com.example.log"));
    assert_eq!(logs[0].message, "this and 2");
    assert_eq!(logs[1].level, LOG_WARN);
    assert_eq!(logs[2].level, LOG_ERROR);
}

#[test]
fn long_log_lines_are_cut_not_dropped() {
    let host = MockHost::new("com.example.long");
    let cx = host.context();
    info!(cx, "{}", "x".repeat(5000));
    let line = &host.logs()[0];
    assert_eq!(line.message.len(), 1023);
    assert!(line.message.ends_with("..."));
}

#[test]
fn a_revision_1_loader_still_gets_plain_logs_and_no_services() {
    let host = MockHost::revision_1("com.example.old");
    let cx = host.context();
    assert!(!cx.has_revision(2));
    assert_eq!(cx.plugin_id(), "");
    assert_eq!(cx.plugin_dir(), None);
    assert_eq!(cx.data_dir(), None);
    info!(cx, "still works");
    let logs = host.logs();
    assert_eq!(logs[0].target, None, "log_ex does not exist there");
    assert_eq!(logs[0].message, "still works");

    assert!(cx.service::<Echo>().is_none());
    let error = cx.register::<Echo>(&ECHO).unwrap_err();
    assert_eq!(error.code(), ERR_NOT_READY);
    assert_eq!(
        cx.unregister_as(c"com.example.echo").unwrap_err().code(),
        ERR_NOT_READY
    );
}

#[repr(C)]
struct EchoTable {
    header: ParsecOrbitInterfaceHeader,
    echo: Option<unsafe extern "system" fn(u32) -> u32>,
    later: Option<unsafe extern "system" fn()>,
}

unsafe extern "system" fn echo(value: u32) -> u32 {
    value
}

static ECHO: EchoTable = EchoTable {
    header: ParsecOrbitInterfaceHeader::new(size_of::<EchoTable>(), 2, 1),
    echo: Some(echo),
    later: None,
};

struct Echo;
impl Interface for Echo {
    const NAME: &'static StdCStr = c"com.example.echo";
    const MAJOR: u16 = 2;
    type Table = EchoTable;
    fn header(table: &EchoTable) -> &ParsecOrbitInterfaceHeader {
        &table.header
    }
}

/// Wants a different major version of the same name.
struct EchoV3;
impl Interface for EchoV3 {
    const NAME: &'static StdCStr = c"com.example.echo";
    const MAJOR: u16 = 3;
    type Table = EchoTable;
    fn header(table: &EchoTable) -> &ParsecOrbitInterfaceHeader {
        &table.header
    }
}

/// Wants more bytes than the registered table has.
struct EchoBig;
impl Interface for EchoBig {
    const NAME: &'static StdCStr = c"com.example.echo";
    const MAJOR: u16 = 2;
    const MIN_SIZE: usize = size_of::<EchoTable>() + 8;
    type Table = EchoTable;
    fn header(table: &EchoTable) -> &ParsecOrbitInterfaceHeader {
        &table.header
    }
}

#[test]
fn services_are_found_by_name_major_and_size() {
    let host = MockHost::new("com.example.consumer");
    let cx = host.context();
    assert!(cx.service::<Echo>().is_none(), "nothing registered yet");

    host.provide("com.example.echo", &ECHO.header);
    let table = cx.service::<Echo>().expect("registered");
    assert_eq!(unsafe { (table.echo.unwrap())(41) }, 41);
    assert!(table.later.is_none());

    assert!(cx.service::<EchoV3>().is_none(), "other major version");
    assert!(cx.service::<EchoBig>().is_none(), "table too small");
}

#[test]
fn a_plugin_registers_and_unregisters_its_own_service() {
    let host = MockHost::new("com.example.provider");
    let cx = host.context();
    cx.register::<Echo>(&ECHO).unwrap();
    assert!(host.registered("com.example.echo"));
    assert_eq!(
        host.owner_of("com.example.echo").as_deref(),
        Some("com.example.provider")
    );
    assert!(cx.service::<Echo>().is_some());

    let again = cx.register::<Echo>(&ECHO).unwrap_err();
    assert_eq!(again.code(), ERR_EXISTS);
    assert_eq!(again.message(), Some("interface already registered"));

    cx.unregister_as(c"com.example.echo").unwrap();
    assert!(!host.registered("com.example.echo"));
    let missing = cx.unregister_as(c"com.example.echo").unwrap_err();
    assert_eq!(missing.code(), ERR_NOT_FOUND);
}

unsafe extern "system" fn load(_info: *const ParsecOrbitPluginLoadInfo) -> i32 {
    0
}
static HANDLER: ParsecOrbitKindHandlerV1 = services::kind_handler(load, None);

#[test]
fn a_kind_handler_is_registered_under_its_kind() {
    let host = MockHost::new("com.example.runtime");
    let cx = host.context();
    cx.register_kind_handler("lua", &HANDLER).unwrap();
    assert!(host.registered("parsec-orbit.kind.lua"));
    assert_eq!(HANDLER.header.major(), 1);
    assert_eq!(
        HANDLER.header.size as usize,
        size_of::<ParsecOrbitKindHandlerV1>()
    );
    assert!(HANDLER.load_plugin.is_some() && HANDLER.unload_plugin.is_none());

    let long = "k".repeat(200);
    assert_eq!(
        cx.register_kind_handler(&long, &HANDLER)
            .unwrap_err()
            .code(),
        ERR_INVALID
    );
    assert_eq!(
        cx.register_kind_handler("a\0b", &HANDLER)
            .unwrap_err()
            .code(),
        ERR_INVALID
    );
}

// ---- Plugin glue ----

use core::sync::atomic::{AtomicU32, Ordering};

static DROPS: AtomicU32 = AtomicU32::new(0);

#[derive(Default)]
struct Counted {
    inits: u32,
}
impl Drop for Counted {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::SeqCst);
    }
}
impl Plugin for Counted {
    fn init(&mut self, cx: &Context) -> Result {
        self.inits += 1;
        info!(cx, "counted init {}", self.inits);
        Ok(())
    }
    fn shutdown(&mut self) {
        DROPS.fetch_add(100, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Refuses;
impl Plugin for Refuses {
    fn init(&mut self, _: &Context) -> Result {
        Err(Error::with_message(5, "not today"))
    }
}

#[test]
fn init_creates_the_plugin_once_and_shutdown_retires_it() {
    static SLOT: plugin::Slot<Counted> = plugin::Slot::new();
    let host = MockHost::new("com.example.slot");

    assert_eq!(unsafe { plugin::init(&SLOT, host.host()) }, 0);
    assert!(host.logged("counted init 1"));
    // A second init must not run the plugin again.
    assert_eq!(unsafe { plugin::init(&SLOT, host.host()) }, ERR_EXISTS);
    assert!(host.logged("init called twice"));
    assert!(!host.logged("counted init 2"));

    assert_eq!(plugin::shutdown(&SLOT), 0);
    assert_eq!(DROPS.load(Ordering::SeqCst), 101, "shutdown, then drop");
    assert_ne!(plugin::shutdown(&SLOT), 0, "only once");
    assert_eq!(DROPS.load(Ordering::SeqCst), 101);
    assert_eq!(
        unsafe { plugin::init(&SLOT, host.host()) },
        ERR_EXISTS,
        "a retired slot is not reused"
    );
}

#[test]
fn a_failing_init_reports_its_code_and_logs_why() {
    static SLOT: plugin::Slot<Refuses> = plugin::Slot::new();
    let host = MockHost::new("com.example.refuses");
    assert_eq!(unsafe { plugin::init(&SLOT, host.host()) }, 5);
    assert!(host.logged("init failed: not today (code 5)"));
}

#[test]
fn init_refuses_a_host_table_it_cannot_use() {
    static SLOT: plugin::Slot<Refuses> = plugin::Slot::new();
    assert_eq!(
        unsafe { plugin::init(&SLOT, core::ptr::null()) },
        ERR_INVALID
    );
    let mut tiny = ParsecOrbitHost::blank();
    tiny.struct_size = 8;
    assert_eq!(unsafe { plugin::init(&SLOT, &tiny) }, ERR_INVALID);
}

#[test]
fn init_runs_against_a_revision_1_loader() {
    static SLOT: plugin::Slot<Refuses> = plugin::Slot::new();
    let host = MockHost::revision_1("ignored");
    assert_eq!(unsafe { plugin::init(&SLOT, host.host()) }, 5);
    assert!(host.logged("init failed: not today (code 5)"));
}

// ---- MockHost ----

#[test]
fn the_mock_host_runs_init_and_hands_back_the_plugin() {
    let host = MockHost::new("com.example.mock");
    let (plugin, result) = host.init::<Counted>();
    assert!(result.is_ok());
    assert_eq!(plugin.inits, 1);
    assert!(host.logged("counted init 1"));
    host.clear_logs();
    assert!(host.logs().is_empty());

    let (_, result) = host.init::<Refuses>();
    assert_eq!(result.unwrap_err().code(), 5);
}

#[test]
fn mock_state_is_per_test_thread() {
    let host = MockHost::new("com.example.threads");
    info!(host.context(), "on the test thread");
    std::thread::spawn(|| {
        let other = MockHost::new("com.example.other");
        assert!(other.logs().is_empty());
    })
    .join()
    .unwrap();
    assert!(host.logged("on the test thread"));
}

// ---- std support ----

#[test]
fn spawned_threads_are_named_after_the_plugin() {
    let host = MockHost::new("com.example.spawner");
    let handle = host
        .context()
        .spawn("worker", || {
            assert_eq!(
                std::thread::current().name(),
                Some("com.example.spawner:worker")
            );
        })
        .unwrap();
    handle.join().unwrap();
}

fn fails_with_an_io_error() -> Result {
    std::fs::read_to_string("C:/definitely/not/here/parsec-orbit.txt")?;
    Ok(())
}

#[test]
fn std_errors_convert_with_the_question_mark() {
    let error = fails_with_an_io_error().unwrap_err();
    assert_eq!(error.code(), 1);
    assert_eq!(error.message(), Some("operation failed"));
}

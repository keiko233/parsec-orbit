//! Plugins built with the Rust SDK inside a real process.
//!
//! The `no_std` plugins link neither `std` nor the C runtime, so these tests
//! are what shows that the freestanding runtime (allocator, panic handler,
//! `memcpy` and friends, stack probes) works once the DLL is mapped by the
//! Windows loader.

mod common;

use common::{Plugin, Run, Sandbox, LOADER_VERSION};

const CLIENT: &str = "parsecd.exe";

fn client(label: &str) -> Sandbox {
    let sandbox = Sandbox::new(label);
    sandbox.install_loader().install_host(CLIENT);
    sandbox
}

fn run_client(sandbox: &Sandbox) -> Run {
    sandbox.run(CLIENT, "ready:", |_| {})
}

#[test]
fn the_no_std_hello_plugin_loads_and_logs_through_the_host() {
    let sandbox = client("hello-nostd");
    sandbox.install_plugin("hello-nostd", Plugin::HelloNostd);

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("loaded com.example.hello-nostd 0.1.0 (Hello Plugin (no_std)) ABI 1")
        .assert_log_contains(&format!(
            "com.example.hello-nostd: hello-nostd has been loaded by Parsec Orbit {LOADER_VERSION}"
        ))
        .assert_log_lacks("disagree on")
        .assert_log_contains("ready: 1 loaded, 0 skipped, 0 failed");
}

#[test]
fn the_freestanding_runtime_serves_allocation_copying_and_formatting() {
    let sandbox = client("nostd-runtime");
    for (dir, suffix) in [("a", "alloc"), ("b", "cmp"), ("c", "stack"), ("d", "float")] {
        sandbox.install_nostd_fixture(dir, &format!("fx.nostd.{suffix}"));
    }

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("alloc ok sum=199990000 zeros=true text=140 copy=true aligned=true")
        .assert_log_contains(
            "cmp ok equal=true different=true forward=true backward=true less=true strlen=12",
        )
        .assert_log_contains("stack ok sum=")
        .assert_log_contains("float ok pi~3.1429 sqrtish=9.88")
        .assert_log_contains("ready: 4 loaded, 0 skipped, 0 failed");
}

#[test]
fn a_panic_in_a_no_std_plugin_is_logged_and_ends_the_process() {
    let sandbox = client("nostd-panic");
    sandbox.install_nostd_fixture("p", "fx.nostd.panic");

    let run = sandbox.run(CLIENT, "panic: boom", |_| {});
    run.assert_log_contains("fx.nostd.panic: panic: boom from fx.nostd.panic at ")
        .assert_log_contains("fixture-nostd");
    assert!(
        !run.output.status.success(),
        "the process must not survive a plugin panic\n{}",
        run.describe()
    );
}

#[test]
fn the_no_std_plugin_has_no_imports_and_exports_only_the_abi() {
    // Read the PE headers of the debug build the tests just made: no import
    // table means no kernel32 and no C runtime, and the only exports are the
    // three ABI entry points (the runtime's assembly stays private).
    let bytes = std::fs::read(Plugin::HelloNostd.dll()).unwrap();
    let names = common::pe_export_names(&bytes);
    assert_eq!(
        names,
        [
            "ParsecOrbitPluginInit",
            "ParsecOrbitPluginQuery",
            "ParsecOrbitPluginShutdown"
        ]
    );
    assert!(
        common::pe_imported_dlls(&bytes).is_empty(),
        "{:?}",
        common::pe_imported_dlls(&bytes)
    );
}

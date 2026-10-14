//! The loader and the plugins inside a real process.
//!
//! Each test builds a small fake client installation (the loader next to a
//! copy of the host binary named `parsecd.exe`, plugins under `plugins\`),
//! starts it with a clean environment, and inspects the loader's log, the
//! host's report and the files the plugins write.
#![cfg(windows)]

mod common;

use std::net::{TcpListener, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{FakeDevTools, Plugin, Run, Sandbox, LOADER_VERSION};

const CLIENT: &str = "parsecd.exe";

fn client(label: &str) -> Sandbox {
    let sandbox = Sandbox::new(label);
    sandbox.install_loader().install_host(CLIENT);
    sandbox
}

fn run_client(sandbox: &Sandbox) -> Run {
    sandbox.run(CLIENT, "ready:", |_| {})
}

// ---- plugins ----------------------------------------------------------------

#[test]
fn a_plugin_is_discovered_loaded_and_runs_inside_the_client() {
    let sandbox = client("hello");
    sandbox.install_plugin("hello", Plugin::Hello);

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains(&format!("Parsec Orbit {LOADER_VERSION} loader mapped"))
        .assert_log_contains("loaded com.example.hello 0.1.0 (Hello Plugin) ABI 1 by Parsec Orbit contributors [MIT] - Example plugin that logs and writes a report file - init returned 0")
        .assert_log_contains(&format!("hello-plugin has been loaded by Parsec Orbit {LOADER_VERSION}"))
        .assert_log_contains("ready: 1 loaded, 0 skipped, 0 failed");

    // The plugin ran in the host process and wrote through the user's APPDATA.
    let report =
        std::fs::read_to_string(sandbox.appdata.join("ParsecOrbit").join("hello-plugin.log"))
            .unwrap_or_else(|e| panic!("no report: {e}\n{}", run.describe()));
    assert!(
        report.contains(&format!("loader={LOADER_VERSION}\n")),
        "{report}"
    );
    assert!(report.contains("build=1\n"), "{report}");
    let pid = run
        .log
        .split("pid=")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap();
    assert!(
        report.contains(&format!("pid={pid}\n")),
        "{report}\n{}",
        run.describe()
    );
}

#[test]
fn log_lines_have_the_documented_shape() {
    let sandbox = client("log-shape");
    let run = run_client(&sandbox);
    run.assert_success();

    for line in run.log.lines() {
        // [MM-DD hh:mm:ss.mmm] [tag] [process pid=N] message
        let bytes = line.as_bytes();
        assert_eq!(bytes[0], b'[', "{line}");
        assert_eq!(bytes[3], b'-', "{line}");
        assert_eq!(bytes[15], b'.', "{line}");
        assert_eq!(&line[19..21], "] ", "{line}");
        assert!(line.contains(&format!("[{CLIENT} pid=")), "{line}");
    }
}

#[test]
fn the_official_plugins_load_with_their_shipped_manifests() {
    let sandbox = client("official");
    sandbox.install_plugin("webview-bridge", Plugin::WebviewBridge);
    sandbox.install_plugin("webview", Plugin::Webview);
    sandbox.install_plugin("native", Plugin::Native);

    // The official plugins share the workspace version.
    let v = env!("CARGO_PKG_VERSION");
    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains(&format!(
            "loaded com.parsec-orbit.webview {v} (WebView UI Patches) ABI 1"
        ))
        .assert_log_contains(&format!(
            "loaded com.parsec-orbit.native {v} (Native UI Patches) ABI 1"
        ))
        .assert_log_contains(&format!("webview ui patches {v} starting"))
        .assert_log_contains(&format!("native ui patches {v} starting"))
        .assert_log_contains("ready: 3 loaded, 0 skipped, 0 failed");
}

#[test]
fn the_stun_swap_plugin_installs_its_getaddrinfo_hook() {
    let sandbox = client("stun-swap");
    sandbox.install_plugin("hooks", Plugin::Hooks);
    sandbox.install_plugin("stun-swap", Plugin::StunSwap);

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("loaded com.parsec-orbit.hooks")
        .assert_log_contains("loaded com.keiko233.stun-swap")
        .assert_log_contains("stun swap: hook installed and probe started")
        .assert_log_contains("ready: 2 loaded, 0 skipped, 0 failed");
}

#[test]
fn the_stun_swap_plugin_redirects_a_real_getaddrinfo_lookup() {
    let responder = UdpSocket::bind(("127.0.0.1", 0)).expect("bind STUN responder");
    let port = responder.local_addr().expect("responder port").port();
    let responder_thread = std::thread::spawn(move || {
        let mut request = [0u8; 2048];
        loop {
            let Ok((size, peer)) = responder.recv_from(&mut request) else {
                break;
            };
            if size < 20 {
                continue;
            }

            let mut response = [0u8; 32];
            response[0..2].copy_from_slice(&0x0101u16.to_be_bytes());
            response[2..4].copy_from_slice(&12u16.to_be_bytes());
            response[4..8].copy_from_slice(&0x2112_A442u32.to_be_bytes());
            response[8..20].copy_from_slice(&request[8..20]);
            response[20..22].copy_from_slice(&0x0020u16.to_be_bytes());
            response[22..24].copy_from_slice(&8u16.to_be_bytes());
            response[25] = 0x01;
            response[26..28].copy_from_slice(&peer.port().to_be_bytes());
            let mapped = u32::from_be_bytes([127, 0, 0, 1]) ^ 0x2112_A442;
            response[28..32].copy_from_slice(&mapped.to_be_bytes());
            let _ = responder.send_to(&response, peer);
        }
    });

    let sandbox = client("stun-swap-lookup");
    sandbox.install_plugin("hooks", Plugin::Hooks);
    sandbox.install_plugin("stun-swap", Plugin::StunSwap);

    let run = sandbox.run(CLIENT, "stun swap: selected 127.0.0.1", |command| {
        command.env("PARSEC_ORBIT_STUN_SERVERS", format!("127.0.0.1@{port}"));
        command.arg("stun");
    });
    run.assert_success()
        .assert_log_contains(&format!("stun swap: 127.0.0.1@{port} -> 127.0.0.1"))
        .assert_log_contains("stun swap: selected 127.0.0.1");
    assert_eq!(
        run.value("stun_lookup").as_deref(),
        Some("127.0.0.1:3478"),
        "{}",
        run.describe()
    );
    drop(responder_thread);
}

#[test]
fn the_native_plugin_waits_for_the_client_module_without_blocking_the_loader() {
    let sandbox = client("native-wait");
    sandbox.install_plugin("native", Plugin::Native);

    let started = Instant::now();
    let run = run_client(&sandbox);
    run.assert_success().assert_log_contains("ready: 1 loaded");
    // The plugin's wait for `parsecd-*.dll` takes up to ten seconds; the host
    // was ready long before that.
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_mix_of_good_and_broken_plugins_never_stops_the_loader() {
    let sandbox = client("mixed");
    sandbox.install_plugin("good", Plugin::Hello);
    sandbox.write_plugin_file("disabled/plugin.toml", "id = \"off\"\nenabled = false\n");
    sandbox.write_plugin_file(
        "js/plugin.toml",
        "id = \"js.plugin\"\nkind = \"js\"\nentry = \"main.js\"\n",
    );
    sandbox.write_plugin_file("bad-toml/plugin.toml", "id = ");
    sandbox.write_plugin_file("future/plugin.toml", "id = \"future\"\napi = 99\n");
    sandbox.write_plugin_file("unknown-kind/plugin.toml", "id = \"k\"\nkind = \"lua\"\n");
    sandbox.write_plugin_file(
        "garbage/plugin.toml",
        "id = \"garbage\"\nentry = \"garbage.dll\"\n",
    );
    sandbox.write_plugin_file("garbage/garbage.dll", "this is not a DLL");
    sandbox.write_plugin_file("loose.dll", "neither is this");
    sandbox.write_plugin_file("notes.txt", "ignored");

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("loaded com.example.hello")
        .assert_log_contains("ready: 1 loaded, 3 skipped, 4 failed");
    run.assert_log_contains("disabled in the manifest, skipped")
        .assert_log_contains("no handler for kind \"js\" (is the runtime plugin installed?)")
        .assert_log_contains("manifest asks for API 99")
        .assert_log_contains("no handler for kind \"lua\"")
        .assert_log_contains("could not load")
        .assert_log_contains("[error]");
}

// ---- host revision 2: identity, ordering, services ---------------------------

/// Where `text` first appears in the log.
fn at(run: &Run, text: &str) -> usize {
    run.log
        .find(text)
        .unwrap_or_else(|| panic!("log lacks {text:?}\n{}", run.describe()))
}

fn fixture_manifest(id: &str, extra: &str) -> String {
    format!("id = \"{id}\"\n{extra}")
}

#[test]
fn every_plugin_gets_a_host_table_that_knows_who_it_is() {
    let sandbox = client("host-table");
    sandbox.install_fixture("one", &fixture_manifest("fx.one", ""), "");
    sandbox.install_fixture("two", &fixture_manifest("fx.two", ""), "");

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("fixture[fx.one]: revision2=true dir=one data_dir=ends-with-id")
        .assert_log_contains("fixture[fx.two]: revision2=true dir=two data_dir=ends-with-id")
        // log_ex puts the target in front of the message.
        .assert_log_contains("fx.one: fixture[fx.one]:")
        .assert_log_contains("ready: 2 loaded, 0 skipped, 0 failed");
}

#[test]
fn requires_decides_the_load_order_not_the_directory_name() {
    let sandbox = client("requires-order");
    // `a-app` is discovered first but needs `fx.base` from `z-base`.
    sandbox.install_fixture(
        "a-app",
        &fixture_manifest("fx.app", "requires = [\"fx.base\"]"),
        "",
    );
    sandbox.install_fixture("m-free", &fixture_manifest("fx.free", ""), "");
    sandbox.install_fixture("z-base", &fixture_manifest("fx.base", ""), "");

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("ready: 3 loaded, 0 skipped, 0 failed");
    let free = at(&run, "fixture[fx.free]: revision2");
    let base = at(&run, "fixture[fx.base]: revision2");
    let app = at(&run, "fixture[fx.app]: revision2");
    assert!(free < base && base < app, "{}", run.describe());
}

#[test]
fn a_service_one_plugin_registers_is_found_by_the_plugin_that_requires_it() {
    let sandbox = client("services");
    sandbox.install_fixture(
        "a-user",
        &fixture_manifest("fx.user", "requires = [\"fx.provider\"]"),
        "query=com.example.svc\nquery=com.example.absent\nregister=com.example.svc\n",
    );
    sandbox.install_fixture(
        "b-provider",
        &fixture_manifest("fx.provider", ""),
        "register=com.example.svc\n",
    );

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("fixture[fx.provider]: register com.example.svc -> 0")
        .assert_log_contains("fx.provider registered interface com.example.svc 1.2 (8 bytes)")
        .assert_log_contains("fixture[fx.user]: query com.example.svc -> 1.2 (8 bytes)")
        .assert_log_contains("fixture[fx.user]: query com.example.absent -> not found")
        // A name can be registered once.
        .assert_log_contains("fixture[fx.user]: register com.example.svc -> -2")
        .assert_log_contains("ready: 2 loaded, 0 skipped, 0 failed");
}

#[test]
fn only_official_plugins_register_official_names_and_only_owners_unregister() {
    let sandbox = client("registry-rules");
    sandbox.install_fixture(
        "a-owner",
        &fixture_manifest("fx.owner", ""),
        "register=parsec-orbit.webview\nregister=com.example.mine\n",
    );
    sandbox.install_fixture(
        "b-official",
        &fixture_manifest("com.parsec-orbit.fixture", "requires = [\"fx.owner\"]"),
        "register=parsec-orbit.webview\nunregister=com.example.mine\n",
    );
    sandbox.install_fixture(
        "c-other",
        &fixture_manifest("fx.other", "requires = [\"fx.owner\"]"),
        "unregister=com.example.mine\n",
    );

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("fixture[fx.owner]: register parsec-orbit.webview -> -1")
        .assert_log_contains("fixture[fx.owner]: register com.example.mine -> 0")
        .assert_log_contains(
            "fixture[com.parsec-orbit.fixture]: register parsec-orbit.webview -> 0",
        )
        .assert_log_contains("fixture[com.parsec-orbit.fixture]: unregister com.example.mine -> -3")
        .assert_log_contains("fixture[fx.other]: unregister com.example.mine -> -3");
}

#[test]
fn a_missing_requirement_skips_the_plugin_before_any_of_its_code_runs() {
    let sandbox = client("missing-requirement");
    sandbox.install_fixture(
        "needs-runtime",
        &fixture_manifest("fx.script", "requires = [\"com.parsec-orbit.js-runtime\"]"),
        "",
    );

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains(
            "fx.script -: skipped, needs com.parsec-orbit.js-runtime, not installed",
        )
        .assert_log_lacks("fixture[fx.script]")
        .assert_log_contains("ready: 0 loaded, 1 skipped, 0 failed");
}

#[test]
fn a_dependency_cycle_fails_its_members_and_names_the_cycle() {
    let sandbox = client("cycle");
    sandbox.install_fixture("a", &fixture_manifest("fx.a", "requires = [\"fx.b\"]"), "");
    sandbox.install_fixture("b", &fixture_manifest("fx.b", "requires = [\"fx.a\"]"), "");
    sandbox.install_fixture("c", &fixture_manifest("fx.c", ""), "");

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("dependency cycle: fx.a -> fx.b -> fx.a")
        .assert_log_contains("fixture[fx.c]: revision2=true")
        .assert_log_lacks("fixture[fx.a]")
        .assert_log_contains("ready: 1 loaded, 0 skipped, 2 failed");
}

#[test]
fn a_dependency_that_fails_its_init_takes_its_dependents_down_with_it() {
    let sandbox = client("failed-dependency");
    sandbox.install_fixture("a-base", &fixture_manifest("fx.base", ""), "fail=3\n");
    sandbox.install_fixture(
        "b-app",
        &fixture_manifest("fx.app", "requires = [\"fx.base\"]"),
        "",
    );
    sandbox.install_fixture(
        "c-soft",
        &fixture_manifest("fx.soft", "optional = [\"fx.base\"]"),
        "",
    );

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("init returned 3")
        .assert_log_contains("fx.app -: skipped, dependency fx.base did not load")
        .assert_log_lacks("fixture[fx.app]")
        // An optional dependency only orders.
        .assert_log_contains("fixture[fx.soft]: revision2=true")
        .assert_log_contains("ready: 1 loaded, 1 skipped, 1 failed");
}

#[test]
fn a_kind_handler_plugin_loads_the_plugins_of_its_kind() {
    let sandbox = client("kind-handler");
    sandbox.write_plugin_file(
        "a-toy/plugin.toml",
        "id = \"fx.toy\"\nkind = \"toy\"\nentry = \"main.toy\"\nrequires = [\"fx.runtime\"]\n",
    );
    sandbox.write_plugin_file(
        "b-rejected/plugin.toml",
        "id = \"fx.rejected\"\nkind = \"toy\"\nentry = \"main.toy\"\nrequires = [\"fx.runtime\"]\n",
    );
    sandbox.install_fixture(
        "c-runtime",
        &fixture_manifest("fx.runtime", ""),
        "handler=toy\n",
    );

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("fixture[fx.runtime]: handler toy -> 0")
        .assert_log_contains(
            "fixture-handler: load id=fx.toy entry=main.toy data_dir=set manifest_bytes=",
        )
        .assert_log_contains("loaded fx.toy through the \"toy\" handler")
        .assert_log_contains("kind handler returned 7")
        .assert_log_contains("ready: 2 loaded, 0 skipped, 1 failed");
    assert!(
        at(&run, "fixture[fx.runtime]: handler") < at(&run, "fixture-handler: load id=fx.toy"),
        "{}",
        run.describe()
    );
}

#[test]
fn a_plugin_of_a_kind_nobody_serves_is_skipped_with_a_hint() {
    let sandbox = client("kind-unserved");
    sandbox.write_plugin_file(
        "toy/plugin.toml",
        "id = \"fx.toy\"\nkind = \"toy\"\nentry = \"main.toy\"\n",
    );

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains(
            "fx.toy -: skipped, no handler for kind \"toy\" (is the runtime plugin installed?)",
        )
        .assert_log_contains("ready: 0 loaded, 1 skipped, 0 failed");
}

#[test]
fn the_official_plugins_agree_with_their_manifests() {
    let sandbox = client("manifest-agreement");
    sandbox.install_plugin("native", Plugin::Native);
    sandbox.install_plugin("webview-bridge", Plugin::WebviewBridge);
    sandbox.install_plugin("webview", Plugin::Webview);
    sandbox.install_plugin("hello", Plugin::Hello);

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_lacks("disagree on")
        .assert_log_contains("ready: 4 loaded, 0 skipped, 0 failed");
}

#[test]
fn the_same_plugin_id_is_loaded_once_and_the_override_directory_wins() {
    let sandbox = client("dedupe");
    let override_dir = sandbox.root.join("override");
    std::fs::create_dir_all(&override_dir).unwrap();
    let in_override = override_dir.join("hello");
    std::fs::create_dir_all(&in_override).unwrap();
    std::fs::copy(
        common::plugin_manifest(Plugin::Hello),
        in_override.join("plugin.toml"),
    )
    .unwrap();
    std::fs::copy(
        Plugin::Hello.dll(),
        in_override.join("parsec_orbit_plugin_hello.dll"),
    )
    .unwrap();
    // Same id again in the client's own plugin directory.
    sandbox.install_plugin("hello", Plugin::Hello);

    let run = sandbox.run(CLIENT, "ready:", |command| {
        command.env("PARSEC_ORBIT_PLUGIN_DIR", &override_dir);
    });
    run.assert_success()
        .assert_log_contains("ready: 1 loaded, 1 skipped, 0 failed");
    assert_eq!(
        run.log.matches("loaded com.example.hello").count(),
        1,
        "{}",
        run.describe()
    );
    assert!(
        run.log
            .contains("com.example.hello 0.1.0: already loaded, skipped"),
        "{}",
        run.describe()
    );
}

#[test]
fn plugins_under_appdata_are_found_too() {
    let sandbox = client("appdata-plugins");
    let dir = sandbox
        .appdata
        .join("ParsecOrbit")
        .join("plugins")
        .join("hello");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(
        common::plugin_manifest(Plugin::Hello),
        dir.join("plugin.toml"),
    )
    .unwrap();
    std::fs::copy(
        Plugin::Hello.dll(),
        dir.join("parsec_orbit_plugin_hello.dll"),
    )
    .unwrap();

    client_ready(&sandbox).assert_log_contains("ready: 1 loaded, 0 skipped, 0 failed");
}

fn client_ready(sandbox: &Sandbox) -> Run {
    let run = run_client(sandbox);
    run.assert_success();
    run
}

// ---- process gate ---------------------------------------------------------------

#[test]
fn outside_the_client_process_the_loader_only_forwards() {
    let sandbox = Sandbox::new("other-host");
    sandbox.install_loader().install_host("notepad-like.exe");
    sandbox.install_plugin("hello", Plugin::Hello);

    let run = sandbox.run(
        "notepad-like.exe",
        "forwarding bcrypt without loading plugins",
        |command| {
            // Even with devtools switched on, nothing may change in a foreign process.
            command.env("PARSEC_ORBIT_DEVTOOLS", "on");
        },
    );
    run.assert_success()
        .assert_log_contains("host is notepad-like.exe, not parsecd.exe")
        .assert_log_lacks("loaded com.example.hello")
        .assert_log_lacks("ready:");
    assert_eq!(run.value("webview2_args").as_deref(), Some("<unset>"));
    assert_eq!(
        run.value("genrandom").as_deref(),
        Some("0"),
        "forwarding still works"
    );
    assert!(!sandbox
        .appdata
        .join("ParsecOrbit")
        .join("hello-plugin.log")
        .exists());
}

#[test]
fn the_host_override_makes_any_process_a_client() {
    let sandbox = Sandbox::new("any-host");
    sandbox.install_loader().install_host("anything.exe");
    sandbox.install_plugin("hello", Plugin::Hello);

    let run = sandbox.run("anything.exe", "ready:", |command| {
        command.env("PARSEC_ORBIT_HOST", "any");
    });
    run.assert_success()
        .assert_log_contains("loaded com.example.hello");
}

#[test]
fn the_client_name_is_matched_ignoring_case() {
    let sandbox = Sandbox::new("case");
    sandbox.install_loader().install_host("ParsecD.EXE");
    let run = sandbox.run("ParsecD.EXE", "ready:", |_| {});
    run.assert_success();
}

// ---- logging ----------------------------------------------------------------------------

#[test]
fn logging_goes_to_appdata_by_default() {
    let sandbox = client("default-log");
    let default_log = sandbox.appdata.join("ParsecOrbit").join("parsec-orbit.log");

    let run = sandbox.run_watching(CLIENT, &default_log, "ready:", |command| {
        command.env_remove("PARSEC_ORBIT_LOG");
    });
    run.assert_success()
        .assert_log_contains("loader mapped")
        .assert_log_contains(&format!("logging to {}", default_log.display()));
}

#[test]
fn the_debug_console_is_opt_in() {
    // Off by default: nothing about a console in the log.
    let off = client("console-off");
    let run = run_client(&off);
    run.assert_success()
        .assert_log_contains("ready:")
        .assert_log_lacks("debug console");

    // On: the loader opens a console (the host already has one, so it is
    // reused here) and reports the outcome. A machine where the console
    // cannot be opened must report that instead - and still load plugins.
    let on = client("console-on");
    let run = on.run(CLIENT, "ready:", |command| {
        command.env("PARSEC_ORBIT_CONSOLE", "1");
    });
    run.assert_success().assert_log_contains("ready:");
    assert!(
        run.log.contains("debug console open") || run.log.contains("no console could be opened"),
        "the console attempt has to be reported either way\n{}",
        run.describe()
    );

    if run.log.contains("debug console open") {
        run.assert_log_contains(&format!(
            "following Parsec's log: {}",
            on.appdata.join("Parsec").join("log_cl.txt").display()
        ));
    }

    // The narrow values pick one of the client's two instances. This test
    // runs as the logged-in user, so it is the client instance: `client`
    // opens the window here and `host` leaves it closed, saying why.
    let only_client = client("console-client");
    let run = only_client.run(CLIENT, "ready:", |command| {
        command.env("PARSEC_ORBIT_CONSOLE", "client");
    });
    run.assert_success().assert_log_contains("ready:");
    assert!(
        run.log.contains("debug console open") || run.log.contains("no console could be opened"),
        "PARSEC_ORBIT_CONSOLE=client is this instance and has to try\n{}",
        run.describe()
    );

    let only_host = client("console-host");
    let run = only_host.run(CLIENT, "ready:", |command| {
        command.env("PARSEC_ORBIT_CONSOLE", "host");
    });
    run.assert_success()
        .assert_log_contains("ready:")
        .assert_log_contains(
            "debug console skipped: PARSEC_ORBIT_CONSOLE=host does not cover this instance (client)",
        )
        .assert_log_lacks("debug console open")
        .assert_log_lacks("following Parsec's log");
}

#[test]
fn an_unwritable_log_path_costs_only_that_candidate() {
    let sandbox = client("log-fallback");
    // A directory cannot be opened as a log file.
    let blocked = sandbox.root.join("blocked");
    std::fs::create_dir_all(&blocked).unwrap();
    let fallback = sandbox.appdata.join("ParsecOrbit").join("parsec-orbit.log");

    let run = sandbox.run_watching(CLIENT, &fallback, "ready:", |command| {
        command.env("PARSEC_ORBIT_LOG", &blocked);
    });
    run.assert_success()
        .assert_log_contains(&format!(
            "log path {} not writable (Win32 error 5), trying the next one",
            blocked.display()
        ))
        .assert_log_contains(&format!("logging to {}", fallback.display()))
        .assert_log_contains("ready:");
}

#[test]
fn the_client_keeps_running_when_no_log_file_can_be_written() {
    let sandbox = client("no-log");
    let blocked = sandbox.root.join("blocked");
    std::fs::create_dir_all(&blocked).unwrap();
    // Every candidate is a directory or unreachable, so lines only reach the
    // debugger. The host cannot see them, so it waits on something else: the
    // genrandom call it always makes after the wait times out quickly.
    let mut command = std::process::Command::new(sandbox.client.join(CLIENT));
    command
        .arg(sandbox.client.join("bcrypt.dll"))
        .arg(sandbox.root.join("never-written.log"))
        .arg("never appears")
        .arg("2");
    sandbox.base_environment(&mut command);
    command
        .env("PARSEC_ORBIT_LOG", &blocked)
        .env("APPDATA", sandbox.root.join("appdata-file"))
        .env("TEMP", &blocked);
    // %APPDATA% points at a file, so ...\ParsecOrbit cannot be created either.
    std::fs::write(sandbox.root.join("appdata-file"), "x").unwrap();
    // The client directory is the third candidate: block it with a directory.
    std::fs::create_dir_all(sandbox.client.join("parsec-orbit.log")).unwrap();

    let run = sandbox.finish(command, &sandbox.root.join("never-written.log"));
    assert_eq!(
        run.output.status.code(),
        Some(3),
        "only the wait timed out\n{}",
        run.describe()
    );
    assert_eq!(run.value("waited").as_deref(), Some("timeout"));
    assert_eq!(
        run.value("genrandom").as_deref(),
        Some("0"),
        "forwarding is unaffected\n{}",
        run.describe()
    );
}

// ---- the devtools arguments -----------------------------------------------------------------

fn webview_args(
    sandbox: &Sandbox,
    customize: impl FnOnce(&mut std::process::Command),
) -> Option<String> {
    let run = sandbox.run(CLIENT, "ready:", customize);
    run.assert_success();
    run.value("webview2_args")
}

#[test]
fn devtools_arguments_use_the_default_port() {
    let sandbox = client("devtools-default");
    let run = sandbox.run(CLIENT, "ready:", |command| {
        command.env_remove("PARSEC_ORBIT_DEVTOOLS");
    });
    run.assert_success()
        .assert_log_contains("devtools enabled: http://127.0.0.1:9222/json");
    assert_eq!(
        run.value("webview2_args").as_deref(),
        Some("--remote-debugging-port=9222 --remote-allow-origins=*")
    );
}

#[test]
fn devtools_arguments_follow_the_configured_port() {
    let sandbox = client("devtools-port");
    let args = webview_args(&sandbox, |command| {
        command
            .env_remove("PARSEC_ORBIT_DEVTOOLS")
            .env("PARSEC_ORBIT_DEVTOOLS_PORT", "9333");
    });
    assert_eq!(
        args.as_deref(),
        Some("--remote-debugging-port=9333 --remote-allow-origins=*")
    );
}

#[test]
fn devtools_can_be_switched_off() {
    let sandbox = client("devtools-off");
    for off in ["0", "off", "false"] {
        let args = webview_args(&sandbox, |command| {
            command.env("PARSEC_ORBIT_DEVTOOLS", off);
        });
        assert_eq!(
            args.as_deref(),
            Some("<unset>"),
            "PARSEC_ORBIT_DEVTOOLS={off}"
        );
    }
}

#[test]
fn the_users_own_browser_arguments_are_left_untouched() {
    let sandbox = client("devtools-user");
    let run = sandbox.run(CLIENT, "ready:", |command| {
        command
            .env_remove("PARSEC_ORBIT_DEVTOOLS")
            .env("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--my-flag");
    });
    run.assert_success()
        .assert_log_contains("already set (9 characters), left untouched");
    assert_eq!(run.value("webview2_args").as_deref(), Some("--my-flag"));
}

// ---- the webview plugin against a fake DevTools endpoint -----------------------------------------

fn bandwidth_script() -> String {
    std::fs::read_to_string(
        common::workspace_root()
            .join("crates/parsec-orbit-plugin-high-bitrate-webview/assets/bandwidth.js"),
    )
    .unwrap()
}

/// The whole chain: the loader opens the endpoint (by setting the browser
/// arguments for the configured port), the webview plugin reads the port back
/// from them, attaches to the page and delivers the script.
#[test]
fn the_webview_plugin_delivers_the_script_through_the_endpoint_the_loader_opened() {
    let devtools = FakeDevTools::start();
    let sandbox = client("webview-inject");
    sandbox.install_plugin("webview-bridge", Plugin::WebviewBridge);
    sandbox.install_plugin("webview", Plugin::Webview);

    let run = sandbox.run(
        CLIENT,
        "script com.parsec-orbit.webview/bandwidth installed",
        |command| {
            command
                .env_remove("PARSEC_ORBIT_DEVTOOLS")
                .env("PARSEC_ORBIT_DEVTOOLS_PORT", devtools.port.to_string());
        },
    );
    run.assert_success()
        .assert_log_contains(&format!(
            "devtools enabled: http://127.0.0.1:{}/json",
            devtools.port
        ))
        .assert_log_contains("devtools session attached (https://web.parsec.test/)")
        .assert_log_contains("webview bandwidth patch handed to the bridge");
    assert_eq!(
        run.value("webview2_args").unwrap(),
        format!(
            "--remote-debugging-port={} --remote-allow-origins=*",
            devtools.port
        )
    );

    // The bridge sets up the session (events, the binding, the page helper)
    // and then installs the plugin's script like any other.
    let commands = devtools.wait_for_commands(6);
    let script = bandwidth_script();
    let methods: Vec<&str> = commands
        .iter()
        .map(|command| command["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        [
            "Runtime.enable",
            "Runtime.addBinding",
            "Page.addScriptToEvaluateOnNewDocument",
            "Runtime.evaluate",
            "Page.addScriptToEvaluateOnNewDocument",
            "Runtime.evaluate"
        ]
    );
    assert_eq!(commands[1]["params"]["name"], "__orbitSend");
    assert!(commands[2]["params"]["source"]
        .as_str()
        .unwrap()
        .contains("window.__orbit"));
    assert_eq!(commands[4]["params"]["source"], script.as_str());
    assert_eq!(commands[5]["params"]["expression"], script.as_str());
}

/// A native plugin uses `parsec-orbit.webview` through the SDK client: the
/// bridge starts its session on the first call and serves script, evaluate and
/// post from there.
#[test]
fn a_native_plugin_uses_the_webview_service_of_the_bridge() {
    let devtools = FakeDevTools::start();
    let sandbox = client("webview-native-consumer");
    sandbox.install_plugin("webview-bridge", Plugin::WebviewBridge);
    sandbox.install_fixture(
        "consumer",
        &fixture_manifest("fx.ui", "requires = [\"com.parsec-orbit.webview-bridge\"]"),
        "webview_script=hello|window.hello = 1;\nwebview_eval=1+1\nwebview_post=greeting|{\"x\":1}\n",
    );

    let run = sandbox.run(CLIENT, "webview_post greeting -> ok", |command| {
        command
            .env_remove("PARSEC_ORBIT_DEVTOOLS")
            .env("PARSEC_ORBIT_DEVTOOLS_PORT", devtools.port.to_string());
    });
    run.assert_success()
        .assert_log_contains("fixture: webview_script hello -> id 1")
        .assert_log_contains("fixture: webview_eval 1+1 -> \"installed\"")
        .assert_log_contains("fixture: webview_post greeting -> ok")
        .assert_log_contains("script fx.ui/hello installed");

    let commands = devtools.wait_for_commands(8);
    let expressions: Vec<&str> = commands
        .iter()
        .filter(|command| command["method"] == "Runtime.evaluate")
        .map(|command| command["params"]["expression"].as_str().unwrap())
        .collect();
    assert!(
        expressions.contains(&"window.hello = 1;"),
        "{expressions:?}"
    );
    assert!(expressions.contains(&"1+1"), "{expressions:?}");
    assert!(
        expressions
            .iter()
            .any(|e| e.contains("__deliver(\"greeting\", {\"x\":1})")),
        "{expressions:?}"
    );
}

#[test]
fn the_webview_plugin_stays_away_when_patches_are_switched_off() {
    let devtools = FakeDevTools::start();
    let sandbox = client("webview-off");
    sandbox.install_plugin("webview-bridge", Plugin::WebviewBridge);
    sandbox.install_plugin("webview", Plugin::Webview);

    let run = sandbox.run(CLIENT, "ready:", |command| {
        command
            .env_remove("PARSEC_ORBIT_DEVTOOLS")
            .env("PARSEC_ORBIT_DEVTOOLS_PORT", devtools.port.to_string())
            .env("PARSEC_ORBIT_PATCHES", "0");
    });
    run.assert_success()
        .assert_log_contains("the webview plugin stays idle")
        .assert_log_contains("loaded com.parsec-orbit.webview");

    // The bridge connects only when somebody needs the page.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        devtools.requests.lock().unwrap().is_empty(),
        "nothing must connect"
    );
    assert!(devtools.commands.lock().unwrap().is_empty());
}

#[test]
fn the_webview_plugin_leaves_the_ui_alone_when_the_user_set_their_own_browser_arguments() {
    let devtools = FakeDevTools::start();
    let sandbox = client("webview-custom-args");
    sandbox.install_plugin("webview-bridge", Plugin::WebviewBridge);
    sandbox.install_plugin("webview", Plugin::Webview);

    let run = sandbox.run(CLIENT, "ready:", |command| {
        command
            .env_remove("PARSEC_ORBIT_DEVTOOLS")
            .env("PARSEC_ORBIT_DEVTOOLS_PORT", devtools.port.to_string())
            .env("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--my-flag");
    });
    run.assert_success().assert_log_contains(
        "no WebView2 DevTools endpoint (disabled or custom browser arguments)",
    );
    std::thread::sleep(Duration::from_millis(1500));
    assert!(devtools.requests.lock().unwrap().is_empty());
}

#[test]
fn the_webview_plugin_keeps_retrying_until_the_endpoint_appears() {
    // Reserve a port, release it, start the client, and only then listen.
    let port = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    };
    let sandbox = client("webview-late");
    sandbox.install_plugin("webview-bridge", Plugin::WebviewBridge);
    sandbox.install_plugin("webview", Plugin::Webview);

    let starter = {
        let client_exe = sandbox.client.join(CLIENT);
        let (loader, log) = (sandbox.client.join("bcrypt.dll"), sandbox.log.clone());
        let mut command = std::process::Command::new(client_exe);
        command
            .arg(loader)
            .arg(log)
            .arg("script com.parsec-orbit.webview/bandwidth installed")
            .arg("30");
        sandbox.base_environment(&mut command);
        command
            .env_remove("PARSEC_ORBIT_DEVTOOLS")
            .env("PARSEC_ORBIT_DEVTOOLS_PORT", port.to_string());
        std::thread::spawn(move || command.output().unwrap())
    };

    // Let the plugin fail at least once before the endpoint exists.
    let deadline = Instant::now() + Duration::from_secs(15);
    while !std::fs::read_to_string(&sandbox.log)
        .unwrap_or_default()
        .contains("endpoint not available yet")
    {
        assert!(
            Instant::now() < deadline,
            "the plugin never reported the missing endpoint"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // Now the "client" opens its endpoint.
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("the port is still free");
    let commands = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (shared_commands, shared_requests) = (commands.clone(), requests.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (commands, requests) = (shared_commands.clone(), shared_requests.clone());
            std::thread::spawn(move || common::serve(stream, port, commands, requests));
        }
    });

    let output = starter.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(commands.lock().unwrap().len(), 6);
}

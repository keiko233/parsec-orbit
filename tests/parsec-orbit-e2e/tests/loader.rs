//! The loader and the plugins inside a real process.
//!
//! Each test builds a small fake client installation (the loader next to a
//! copy of the host binary named `parsecd.exe`, plugins under `plugins\`),
//! starts it with a clean environment, and inspects the loader's log, the
//! host's report and the files the plugins write.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    sandbox.install_plugin("webview", Plugin::Webview);
    sandbox.install_plugin("native", Plugin::Native);

    // The official plugins share the workspace version.
    let v = env!("CARGO_PKG_VERSION");
    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains(&format!("loaded com.parsec-orbit.webview {v} (WebView UI Patches) ABI 1"))
        .assert_log_contains(&format!("loaded com.parsec-orbit.native {v} (Native UI Patches) ABI 1"))
        .assert_log_contains(&format!("webview ui patches {v} starting"))
        .assert_log_contains(&format!("native ui patches {v} starting"))
        .assert_log_contains("ready: 2 loaded, 0 skipped, 0 failed");
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
        .assert_log_contains("ready: 1 loaded, 2 skipped, 5 failed");
    run.assert_log_contains("disabled in the manifest, skipped")
        .assert_log_contains("needs the JavaScript runtime")
        .assert_log_contains("manifest asks for API 99")
        .assert_log_contains("unknown kind \"lua\"")
        .assert_log_contains("could not load")
        .assert_log_contains("[error]");
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

/// Just enough of a DevTools endpoint to receive the plugin's commands.
struct FakeDevTools {
    port: u16,
    commands: Arc<Mutex<Vec<serde_json::Value>>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FakeDevTools {
    fn start() -> FakeDevTools {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (shared_commands, shared_requests) = (commands.clone(), requests.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (commands, requests) = (shared_commands.clone(), shared_requests.clone());
                std::thread::spawn(move || serve(stream, port, commands, requests));
            }
        });
        FakeDevTools {
            port,
            commands,
            requests,
        }
    }

    fn wait_for_commands(&self, count: usize) -> Vec<serde_json::Value> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let commands = self.commands.lock().unwrap().clone();
            if commands.len() >= count {
                return commands;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!(
            "only {} commands arrived",
            self.commands.lock().unwrap().len()
        );
    }
}

fn serve(
    mut stream: TcpStream,
    port: u16,
    commands: Arc<Mutex<Vec<serde_json::Value>>>,
    requests: Arc<Mutex<Vec<String>>>,
) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read_exact(&mut byte).is_err() {
            return;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    requests
        .lock()
        .unwrap()
        .push(head.lines().next().unwrap_or("").to_string());

    if head.starts_with("GET /json") {
        let body = format!(
            "[{{\"type\":\"page\",\"url\":\"https://web.parsec.test/\",\"webSocketDebuggerUrl\":\"ws://127.0.0.1:{port}/devtools/page/P\"}}]"
        );
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        return;
    }

    let _ = stream.write_all(
        b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
    );
    loop {
        let mut header = [0u8; 2];
        if stream.read_exact(&mut header).is_err() {
            return;
        }
        let length = match header[1] & 0x7F {
            126 => {
                let mut bytes = [0u8; 2];
                stream.read_exact(&mut bytes).unwrap();
                u16::from_be_bytes(bytes) as usize
            }
            127 => {
                let mut bytes = [0u8; 8];
                stream.read_exact(&mut bytes).unwrap();
                u64::from_be_bytes(bytes) as usize
            }
            length => length as usize,
        };
        let mut mask = [0u8; 4];
        stream.read_exact(&mut mask).unwrap();
        let mut payload = vec![0u8; length];
        stream.read_exact(&mut payload).unwrap();
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
        if header[0] & 0x0F != 0x1 {
            continue;
        }
        let command: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        let id = command["id"].as_u64().unwrap();
        let reply = if command["method"] == "Runtime.evaluate" {
            format!("{{\"id\":{id},\"result\":{{\"result\":{{\"type\":\"string\",\"value\":\"installed\"}}}}}}")
        } else {
            format!("{{\"id\":{id},\"result\":{{\"identifier\":\"1\"}}}}")
        };
        commands.lock().unwrap().push(command);
        let mut frame = vec![0x81];
        if reply.len() < 126 {
            frame.push(reply.len() as u8);
        } else {
            frame.push(126);
            frame.extend_from_slice(&(reply.len() as u16).to_be_bytes());
        }
        frame.extend_from_slice(reply.as_bytes());
        let _ = stream.write_all(&frame);
    }
}

fn bandwidth_script() -> String {
    std::fs::read_to_string(
        common::workspace_root().join("plugins/parsec-orbit-plugin-webview/assets/bandwidth.js"),
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
    sandbox.install_plugin("webview", Plugin::Webview);

    let run = sandbox.run(
        CLIENT,
        "webview bandwidth list extended: installed",
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
        .assert_log_contains(
            "webview patch registered for future documents (https://web.parsec.test/)",
        );
    assert_eq!(
        run.value("webview2_args").unwrap(),
        format!(
            "--remote-debugging-port={} --remote-allow-origins=*",
            devtools.port
        )
    );

    let commands = devtools.wait_for_commands(2);
    let script = bandwidth_script();
    assert_eq!(
        commands[0]["method"],
        "Page.addScriptToEvaluateOnNewDocument"
    );
    assert_eq!(commands[0]["params"]["source"], script.as_str());
    assert_eq!(commands[1]["method"], "Runtime.evaluate");
    assert_eq!(commands[1]["params"]["expression"], script.as_str());
}

#[test]
fn the_webview_plugin_stays_away_when_patches_are_switched_off() {
    let devtools = FakeDevTools::start();
    let sandbox = client("webview-off");
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

    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        devtools.requests.lock().unwrap().is_empty(),
        "the plugin must not connect"
    );
    assert!(devtools.commands.lock().unwrap().is_empty());
}

#[test]
fn the_webview_plugin_leaves_the_ui_alone_when_the_user_set_their_own_browser_arguments() {
    let devtools = FakeDevTools::start();
    let sandbox = client("webview-custom-args");
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
    sandbox.install_plugin("webview", Plugin::Webview);

    let starter = {
        let client_exe = sandbox.client.join(CLIENT);
        let (loader, log) = (sandbox.client.join("bcrypt.dll"), sandbox.log.clone());
        let mut command = std::process::Command::new(client_exe);
        command
            .arg(loader)
            .arg(log)
            .arg("webview bandwidth list extended: installed")
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
            std::thread::spawn(move || serve(stream, port, commands, requests));
        }
    });

    let output = starter.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(commands.lock().unwrap().len(), 2);
}

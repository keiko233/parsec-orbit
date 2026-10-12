//! JavaScript plugins inside a real process: the runtime plugin loads
//! `kind = "js"` plugins through the loader's kind-handler mechanism, with
//! the webview bridge and a fake DevTools endpoint behind them.

mod common;

use common::{FakeDevTools, Plugin, Run, Sandbox};

const CLIENT: &str = "parsecd.exe";

fn client(label: &str) -> Sandbox {
    let sandbox = Sandbox::new(label);
    sandbox.install_loader().install_host(CLIENT);
    sandbox
}

fn run_client(sandbox: &Sandbox) -> Run {
    sandbox.run(CLIENT, "ready:", |_| {})
}

fn js_plugin(sandbox: &Sandbox, dir: &str, id: &str, extra: &str, main: &str) {
    sandbox.write_plugin_file(
        &format!("{dir}/plugin.toml"),
        &format!(
            "id = \"{id}\"\nversion = \"0.3.0\"\nkind = \"js\"\nentry = \"main.js\"\nrequires = [\"com.parsec-orbit.js-runtime\"]\n{extra}"
        ),
    );
    sandbox.write_plugin_file(&format!("{dir}/main.js"), main);
}

const HELLO: &str = r#"
export default {
  async setup(host) {
    host.log.info("hello from " + host.plugin.id + " " + host.plugin.version);
    await new Promise((resolve) => host.setTimeout(resolve, 30));
    host.log.info("setup finished");
  },
};
"#;

#[test]
fn a_javascript_plugin_is_loaded_by_the_runtime() {
    let sandbox = client("js-hello");
    sandbox.install_plugin("js-runtime", Plugin::JsRuntime);
    js_plugin(&sandbox, "hello-js", "com.example.js-hello", "", HELLO);

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("loaded com.example.js-hello through the \"js\" handler")
        .assert_log_contains("com.example.js-hello: hello from com.example.js-hello 0.3.0")
        .assert_log_contains("com.example.js-hello: setup finished")
        .assert_log_contains("ready: 2 loaded, 0 skipped, 0 failed");
    // The runtime is up before the plugin that needs it.
    let runtime = run.log.find("loaded com.parsec-orbit.js-runtime").unwrap();
    let plugin = run.log.find("loaded com.example.js-hello through").unwrap();
    assert!(runtime < plugin, "{}", run.describe());
}

#[test]
fn without_the_runtime_a_javascript_plugin_is_skipped_with_the_reason() {
    let sandbox = client("js-no-runtime");
    js_plugin(&sandbox, "hello-js", "com.example.js-hello", "", HELLO);

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("skipped, needs com.parsec-orbit.js-runtime, not installed")
        .assert_log_contains("ready: 0 loaded, 1 skipped, 0 failed");
}

#[test]
fn a_javascript_plugin_that_fails_in_setup_is_failed_not_fatal() {
    let sandbox = client("js-failing");
    sandbox.install_plugin("js-runtime", Plugin::JsRuntime);
    js_plugin(
        &sandbox,
        "bad",
        "com.example.js-bad",
        "",
        "export default { setup() { throw new Error('refusing to start'); } };",
    );
    js_plugin(&sandbox, "good", "com.example.js-good", "", HELLO);
    sandbox.write_plugin_file(
        "later/plugin.toml",
        "id = \"com.example.js-later\"
kind = \"js\"
entry = \"main.js\"
requires = [\"com.parsec-orbit.js-runtime\", \"com.example.js-bad\"]
",
    );
    sandbox.write_plugin_file("later/main.js", HELLO);

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("com.example.js-bad: setup threw: Error: refusing to start")
        .assert_log_contains("kind handler returned 1")
        .assert_log_contains("com.example.js-good: setup finished")
        .assert_log_contains("skipped, dependency com.example.js-bad did not load")
        .assert_log_contains("ready: 2 loaded, 1 skipped, 1 failed");
}

#[test]
fn a_javascript_plugin_with_the_permission_reaches_the_page_through_the_bridge() {
    let devtools = FakeDevTools::start();
    let sandbox = client("js-webview");
    sandbox.install_plugin("webview-bridge", Plugin::WebviewBridge);
    sandbox.install_plugin("js-runtime", Plugin::JsRuntime);
    js_plugin(
        &sandbox,
        "ui",
        "com.example.js-ui",
        "permissions = [\"webview\"]\n",
        r#"
export default {
  async setup(host) {
    await host.webview.injectFile("ui.js");
    const result = await host.webview.evaluate("1 + 1");
    host.log.info("page said " + JSON.stringify(result));
    await host.webview.post("greeting", { from: "js" });
    host.log.info("posted");
  },
};
"#,
    );
    sandbox.write_plugin_file("ui/ui.js", "window.fromJsPlugin = true;");

    let run = sandbox.run(CLIENT, "com.example.js-ui: posted", |command| {
        command
            .env_remove("PARSEC_ORBIT_DEVTOOLS")
            .env("PARSEC_ORBIT_DEVTOOLS_PORT", devtools.port.to_string());
    });
    run.assert_success()
        .assert_log_contains("com.example.js-ui: page said \"installed\"");

    let commands = devtools.wait_for_commands(8);
    let expressions: Vec<&str> = commands
        .iter()
        .filter(|command| command["method"] == "Runtime.evaluate")
        .map(|command| command["params"]["expression"].as_str().unwrap())
        .collect();
    assert!(
        expressions.contains(&"window.fromJsPlugin = true;"),
        "{expressions:?}"
    );
    assert!(expressions.contains(&"1 + 1"), "{expressions:?}");
    assert!(
        expressions
            .iter()
            .any(|e| e.contains("__deliver(\"greeting\", {\"from\":\"js\"})")),
        "{expressions:?}"
    );
}

#[test]
fn a_javascript_plugin_without_the_permission_has_no_webview() {
    let sandbox = client("js-no-permission");
    sandbox.install_plugin("js-runtime", Plugin::JsRuntime);
    js_plugin(
        &sandbox,
        "plain",
        "com.example.js-plain",
        "",
        "export default { setup(host) { host.log.info('webview is ' + typeof host.webview); } };",
    );

    let run = run_client(&sandbox);
    run.assert_success()
        .assert_log_contains("com.example.js-plain: webview is undefined");
}

#[test]
fn a_native_plugin_evaluates_script_through_parsec_orbit_js() {
    let sandbox = client("js-native-consumer");
    sandbox.install_plugin("js-runtime", Plugin::JsRuntime);
    sandbox.install_fixture(
        "consumer",
        "id = \"fx.native-js\"\nrequires = [\"com.parsec-orbit.js-runtime\"]",
        "js_eval=[1, 2, 3].map((n) => n * 2)\n",
    );

    let run = run_client(&sandbox);
    run.assert_success().assert_log_contains(
        "fixture[fx.native-js]: js_eval [1, 2, 3].map((n) => n * 2) -> [2,4,6]",
    );
}

/// The TypeScript sample, as built by `pnpm run build` in
/// `examples/parsec-orbit-plugin-bandwidth-ts`, on the real runtime and bridge.
/// CI builds it first (`PARSEC_ORBIT_REQUIRE_NODE` is set there); elsewhere the
/// test is skipped when the build output is missing.
#[test]
fn the_typescript_bandwidth_sample_runs_on_the_runtime_and_reaches_the_page() {
    let sample = common::workspace_root().join("examples/parsec-orbit-plugin-bandwidth-ts");
    let built = sample.join("dist/main.js");
    if !built.is_file() {
        if std::env::var_os("PARSEC_ORBIT_REQUIRE_NODE").is_some() {
            panic!(
                "{} is missing: run `pnpm install && pnpm run build` first",
                built.display()
            );
        }
        eprintln!("skipping: {} is not built", built.display());
        return;
    }

    let devtools = FakeDevTools::start();
    let sandbox = client("js-ts-sample");
    sandbox.install_plugin("webview-bridge", Plugin::WebviewBridge);
    sandbox.install_plugin("js-runtime", Plugin::JsRuntime);
    for file in ["plugin.toml", "dist/main.js", "dist/ui.js"] {
        let target = sandbox.client.join("plugins/bandwidth-ts").join(file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(sample.join(file), target).unwrap();
    }

    let run = sandbox.run(
        CLIENT,
        "loaded com.example.bandwidth-ts through the \"js\" handler",
        |command| {
            command
                .env_remove("PARSEC_ORBIT_DEVTOOLS")
                .env("PARSEC_ORBIT_DEVTOOLS_PORT", devtools.port.to_string());
        },
    );
    run.assert_success()
        .assert_log_contains("com.example.bandwidth-ts: bandwidth ladder 0.1.0 starting")
        .assert_log_contains("loaded com.example.bandwidth-ts through the \"js\" handler");

    let ui = std::fs::read_to_string(sample.join("dist/ui.js")).unwrap();
    let commands = devtools.wait_for_commands(6);
    assert!(
        commands
            .iter()
            .any(|c| c["method"] == "Runtime.evaluate" && c["params"]["expression"] == ui.as_str()),
        "the page script was delivered"
    );
}

// ---- the config store, across restarts ------------------------------------------------

#[test]
fn settings_survive_a_restart_through_the_config_service() {
    let sandbox = client("config-restart");
    sandbox.install_plugin("config", Plugin::Config);
    sandbox.install_fixture(
        "writer",
        "id = \"fx.writer\"\nrequires = [\"com.parsec-orbit.config\"]",
        "config_get=answer\nconfig_set=answer|{\"n\":42}\nconfig_set=bad|{nope\n",
    );

    let first = run_client(&sandbox);
    first
        .assert_success()
        .assert_log_contains("fixture[fx.writer]: config_get answer -> unset")
        .assert_log_contains("fixture[fx.writer]: config_set answer -> ok")
        .assert_log_contains("fixture[fx.writer]: config_set bad -> error -1");

    // The same installation, started again.
    // The log is appended to, so "ready:" is there already: wait for the line
    // that only the second start can write.
    let second = sandbox.run(CLIENT, "config_get answer -> {", |_| {});
    second
        .assert_success()
        .assert_log_contains("fixture[fx.writer]: config_get answer -> {\"n\":42}");

    let file = sandbox
        .appdata
        .join("ParsecOrbit/data/com.parsec-orbit.config/fx.writer.json");
    assert_eq!(
        std::fs::read_to_string(file).unwrap(),
        "{\"answer\":{\"n\":42}}"
    );
}

#[test]
fn a_javascript_plugin_keeps_its_settings_in_the_config_store() {
    let sandbox = client("js-config");
    sandbox.install_plugin("config", Plugin::Config);
    sandbox.install_plugin("js-runtime", Plugin::JsRuntime);
    js_plugin(
        &sandbox,
        "counter",
        "com.example.js-counter",
        "permissions = [\"config\"]\n",
        r#"export default { async setup(host) {
             const runs = (await host.config.get("runs")) ?? 0;
             await host.config.set("runs", runs + 1);
             host.log.info("run number " + (runs + 1));
           } };"#,
    );

    run_client(&sandbox)
        .assert_success()
        .assert_log_contains("com.example.js-counter: run number 1");
    sandbox
        .run(CLIENT, "run number 2", |_| {})
        .assert_success()
        .assert_log_contains("com.example.js-counter: run number 2");
    let file = sandbox
        .appdata
        .join("ParsecOrbit/data/com.parsec-orbit.config/com.example.js-counter.json");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "{\"runs\":2}");
}

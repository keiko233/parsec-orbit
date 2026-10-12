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

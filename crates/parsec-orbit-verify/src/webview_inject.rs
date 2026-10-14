//! Injects the WebView bandwidth patch into a running client and reports the
//! result. This is the same code path the loader runs on start-up; it exists
//! so the injection can be exercised without restarting the client.
//!
//! Usage: `webview-inject [port]` (default 9222).

fn main() {
    let port = std::env::args()
        .nth(1)
        .map(|value| value.parse().unwrap_or(9222))
        .unwrap_or(9222);

    // The patch script is read from the plugin's asset instead of linking the
    // plugin: every plugin exports the same ABI symbols, and a tool that links
    // two plugin rlibs fails the release (LTO) build with a multiply defined
    // `ParsecOrbitPluginInit`.
    match parsec_orbit_plugin_webview_bridge::inject_once(
        port,
        include_str!("../../parsec-orbit-plugin-high-bitrate-webview/assets/bandwidth.js"),
    ) {
        Ok(injection) => {
            println!("target: {}", injection.target_url);
            println!(
                "addScriptToEvaluateOnNewDocument: {}",
                injection.new_document_script
            );
            match injection.evaluation_result {
                Some(result) => println!("current document: {result}"),
                None => println!("current document: evaluated (no return value)"),
            }
        }
        Err(err) => {
            eprintln!("injection failed: {err}");
            std::process::exit(1);
        }
    }
}

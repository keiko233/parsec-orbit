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

    match parsec_orbit_plugin_webview_bridge::inject_once(
        port,
        parsec_orbit_plugin_high_bitrate_webview::BITRATE_SCRIPT,
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

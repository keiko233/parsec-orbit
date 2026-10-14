# 0010 - One WebView service, an in-process backend on macOS

## Context
The Windows bridge speaks the DevTools protocol to a port the loader opens. The macOS client uses WKWebView, which has no remote debugging port; it has `evaluateJavaScript:completionHandler:`, `loadRequest:` and a script-message handler, and the code and rollout flags for a web UI, but no navigation URL or DOM is known. The JS SDK, the JS runtime and the high-bitrate plugins are written against `parsec-orbit.webview`.

## Decision
Keep `parsec-orbit.webview` (name, header, functions, error codes) and replace its backend on macOS with an in-process one: capture the client's `WKWebView` through the Objective-C runtime, use `WKUserContentController` for scripts and messages, and `evaluateJavaScript:` for evaluation. WebKit calls are made on the main thread, and a call from the main thread returns `-4` instead of blocking. As a best-effort development aid, `PARSEC_ORBIT_DEVTOOLS=1` sets the web view `inspectable` so Safari's Web Inspector can attach; no port is opened.

## Consequences
* JS plugins and the TypeScript SDK work unchanged once the page is known.
* `high-bitrate-webview` and `orbit.rpc` are blocked on runtime discovery of the URL and DOM (M2 entry gate).
* The bridge plugin gets a macOS backend in the same crate, selected by target.
* The Windows DevTools client crate stays for Windows.

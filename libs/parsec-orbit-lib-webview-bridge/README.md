# WebView Bridge

Official [Parsec Orbit](../../README.md) plugin. It provides the
`parsec-orbit.webview` interface ([spec](../../docs/spec/services.md#parsec-orbitwebview-provided-by-the-webview-bridge-plugin-version-10)):
scripts that stay installed in the client's WebView2 UI, evaluation in the
page, and messages in both directions. Plugins use it instead of carrying an
HTTP/WebSocket client of their own - native plugins through `WebViewClient` in
the Rust SDK, JavaScript plugins through the JS runtime's `host.webview`.

Install it next to the plugins that need it (the WebView UI patch
[`parsec-orbit-plugin-high-bitrate-webview`](../parsec-orbit-plugin-high-bitrate-webview/README.md)
does: it lists this plugin in `requires`, so the loader starts it first). On
its own it does nothing and costs nothing: it connects to the page only when
a plugin first asks for something.

## What it does

The loader opens the WebView2 DevTools endpoint (`--remote-debugging-port`, see
`PARSEC_ORBIT_DEVTOOLS*` in the main README). The bridge

1. polls `http://127.0.0.1:<port>/json` until the page target exists,
2. attaches (`Runtime.enable`, `Runtime.addBinding __orbitSend`),
3. installs the page helper `window.__orbit` (`assets/orbit.js`), then every
   script plugins registered, each both with
   `Page.addScriptToEvaluateOnNewDocument` (so it survives reloads) and
   `Runtime.evaluate` (for the document that is already there),
4. keeps the WebSocket open, answers `evaluate` / `post` requests, and turns
   `Runtime.bindingCalled` events (the page's `window.__orbit.emit`) into
   subscriber callbacks,
5. when the socket drops - the page was recreated - attaches again and replays
   the scripts, so a plugin registers a script once.

If the user set `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` themselves or switched
the endpoint off, there is no endpoint that is ours to use: the interface is
still registered and answers "not ready", and the web UI is left untouched.

## Page helper

`window.__orbit` has `emit(channel, payload)`, `on(channel, handler)` (returns
an unsubscribe function), `off`, and `rpc(event, data, timeoutMs)` for the
Parsec UI/client request/reply protocol (`protocol_version: 1`).

## Files

* `src/lib.rs` - the plugin: port detection, registration.
* `src/service.rs` - the `ParsecOrbitWebViewV1` table.
* `src/bridge.rs` - scripts, subscriptions and the session thread.
* `src/devtools.rs` - HTTP and WebSocket client for the endpoint (also used by
  the `webview-inject` tool of `parsec-orbit-verify` through `inject_once`, so
  the tool links this crate's `rlib` and no other plugin: two plugin `rlib`s
  export the same ABI symbols and fail an LTO build.)
* `assets/orbit.js` - the page helper.

## Tests

`tests/session.rs` (and the DevTools crate's own tests) run the bridge against a fake
endpoint that speaks HTTP and WebSocket; the end-to-end suite
(`tests/parsec-orbit-e2e`) runs the DLL in a real process against the same
kind of fake.

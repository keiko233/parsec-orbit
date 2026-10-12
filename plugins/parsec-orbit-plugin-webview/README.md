# WebView UI Patches

Official [Parsec Orbit](../../README.md) plugin. It widens the "Bandwidth
Limit" choices in the WebView2 interface at runtime, without modifying
anything on disk.

The client configuration system is far more permissive than its user
interfaces. `encoder_bitrate` ("Bandwidth Limit"), for example, is an `int`
with `min 0` / `max 2147483647`, but the WebView UI only offers the built-in
ladder `3, 5, 7, 10, 15, 20, 25, 30, 35, 40, 45, 50 Mbps`. The plugin widens
it to `60, 75, 100, 125, 150, 200, 250, 300, 400, 500 Mbps`.

The native (libmatoya) UI is patched by the separate
[`parsec-orbit-plugin-native`](../parsec-orbit-plugin-native/README.md) plugin.

## Files

* `plugin.toml` - manifest the loader reads (`id`, `enabled`, `entry`, ...).
* `src/lib.rs` - plugin entry point; finds the DevTools endpoint.
* `src/webview.rs` - minimal HTTP/WebSocket client for the endpoint, plus the
  script injection loop.
* `assets/bandwidth.js` - the script injected into the web UI; also usable by
  hand in the DevTools console.

## Install

Copy this folder (or extract `parsec-orbit-plugin-webview.zip`) into the
client's plugin directory, so that the loader finds
`plugins\parsec-orbit-plugin-webview\plugin.toml` next to `parsecd.exe`.

## Disable

* `enabled = false` in `plugin.toml`, or
* delete the plugin folder, or
* set `PARSEC_ORBIT_PATCHES=0` (or `off`/`false`) in the client environment.

## How the patch is delivered

The new interface is a WebView2 page loaded from
`https://builds.parsec.app/webview/release-ui/index.html`. The option list is a
hard-coded array in `index.js`, and the page saves a selection through the
`cfg_seti` RPC (`encoder_bitrate`), which the native side accepts as-is.

`assets/bandwidth.js` finds the array through the React fiber tree, appends the
extra entries and replays the app's own `parsec_config_update` event so an
already-rendered settings page shows them. The script is idempotent and can be
pasted into the DevTools console.

The loader opens `http://127.0.0.1:9222/json` (unless `PARSEC_ORBIT_DEVTOOLS=0`
or `/off`/`/false`, or the user already set
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` themselves), and the plugin's worker
thread then

1. polls `/json` until the page target exists,
2. registers the script with `Page.addScriptToEvaluateOnNewDocument` so it
   survives reloads,
3. runs it once with `Runtime.evaluate` for the loaded document, and
4. keeps the WebSocket open, answering pings, until the page goes away.

`cargo run --release -p parsec-orbit-verify-webview-inject -- 9222` performs the
same steps against a running client without restarting it.

The loader log (`%APPDATA%\ParsecOrbit\parsec-orbit.log`) reports the result:

```
loaded com.parsec-orbit.webview 0.1.0 (WebView UI Patches) ABI 1 by Parsec Orbit contributors [MIT]
webview ui patches 0.1.0 starting
webview patch registered for future documents (https://builds.parsec.app/webview/release-ui/index.html)
webview bandwidth list extended: installed
```

## Keeping the two lists in sync

The extra options live in two places and must stay in sync:

* `assets/bandwidth.js` - `EXTRA_MBPS`
* `../parsec-orbit-plugin-native/src/lib.rs` - `EXTRA_BANDWIDTH_MBPS`

# Parsec Orbit

An unofficial, local-only proof of concept for loading plugins in the Parsec
client. It uses a `bcrypt.dll` proxy and does not modify Parsec files.

## Layout

* `crates/parsec-orbit-loader/` - the `bcrypt.dll` proxy. It forwards every export to
  the real `bcrypt.dll`, opens the WebView2 DevTools endpoint and hosts the
  plugin system.
* `crates/parsec-orbit-plugin-abi/`, `crates/parsec-orbit-plugin-sdk/`,
  `crates/parsec-orbit-plugin-sdk-std/` - the plugin ABI, the Rust SDK
  (`Plugin`, `Context`, a `no_std` runtime for 10 KB plugins) and its `std`
  companions; see the [plugin ABI reference](docs/spec/plugin-abi.md) and
  [Write a Rust plugin](docs/guides/rust-plugin.md).
* `crates/parsec-orbit-core/` - the loader's Win32-free logic: manifests,
  discovery, dependency ordering, the service registry, per-plugin host tables.
  Specifications, design notes and the roadmap are indexed in
  [docs/](docs/README.md).
* `crates/parsec-orbit-plugin-webview-bridge/` - official plugin that provides
  `parsec-orbit.webview` (scripts, evaluation and messages for the WebView2 UI
  over the DevTools endpoint); see its
  [README](crates/parsec-orbit-plugin-webview-bridge/README.md).
* `crates/parsec-orbit-plugin-js-runtime/` - official plugin that runs JavaScript
  and TypeScript plugins (`kind = "js"`) on Boa; see its
  [README](crates/parsec-orbit-plugin-js-runtime/README.md).
* `sdk/js/` - `@parsec-orbit/sdk` for JavaScript/TypeScript plugins;
  `examples/parsec-orbit-plugin-bandwidth-ts/` is the TypeScript sample (see
  [Write a TypeScript plugin](docs/guides/typescript-plugin.md)).
* `crates/parsec-orbit-plugin-high-bitrate-webview/` - official plugin that widens the
  "Bandwidth Limit" choices in the WebView2 UI; see its
  [README](crates/parsec-orbit-plugin-high-bitrate-webview/README.md).
* `crates/parsec-orbit-plugin-high-bitrate-native/` - official plugin for the same options
  in the native (libmatoya) UI; see its
  [README](crates/parsec-orbit-plugin-high-bitrate-native/README.md).
* `examples/parsec-orbit-plugin-hello-nostd/` - smallest possible plugin (10 KB, `no_std`).
* `examples/parsec-orbit-plugin-hello/` - the same idea with `std`.
* `crates/parsec-orbit-verify-*` - verification tools for the loader and the
  WebView patch delivery.
* `tests/parsec-orbit-e2e/` - end-to-end tests that run the built loader and
  plugins inside a real process; see [Tests](#tests).

## Build and install

```powershell
cargo build --release
scripts\package.ps1        # stages dist\ with bcrypt.dll, plugin zips, SHA256SUMS
powershell -ExecutionPolicy Bypass -File scripts\install.ps1   # from an elevated prompt
```

`scripts\install.ps1` stops the Parsec service, backs up the previous
`bcrypt.dll`, copies the loader and the official plugins into
`C:\Program Files\Parsec\plugins\` and starts the service again
(`-EnableWebViewUi` also switches the client to the WebView2 UI). To install by
hand, copy `bcrypt.dll` next to `parsecd.exe` (typically
`C:\Program Files\Parsec`) and the plugin folders into `<client>\plugins\`.

The client loads the proxy at start-up. It appends to the first writable path
of `PARSEC_ORBIT_LOG`, `%APPDATA%\ParsecOrbit\parsec-orbit.log`, the client
directory and `%TEMP%`, and mirrors every line to the debugger output.

## Tests

```powershell
pnpm install; pnpm -r build; pnpm -r test   # the JavaScript SDK and the sample
cargo test --workspace
```

Windows only (the loader is a `bcrypt.dll` proxy). Node.js is needed for the
behaviour tests of the script injected into the WebView UI; without it that one
test is skipped (set `PARSEC_ORBIT_REQUIRE_NODE=1` to make it fail instead).

* **Unit tests** sit next to the code: ABI layout (`abi`) and the export macro
  (`sdk`), settings, logging and the forwarding thunks (`loader`), manifests,
  plugin discovery and the descriptor checks (`core`), the DevTools client and
  the bridge session (`webview-bridge`) and the signature scan and in-memory
  patch, run against a fake PE image (`native`).
* **Integration tests** in `crates/parsec-orbit-plugin-webview-bridge/tests/` drive
  the DevTools client and the bridge session against a fake endpoint;
  `crates/parsec-orbit-plugin-high-bitrate-webview/tests/` runs
  `assets/bandwidth.js` in a fake browser under Node.
* **End-to-end tests** in `tests/parsec-orbit-e2e/` build the loader and the
  plugins into `target/e2e`, install them into a throwaway client directory
  next to a host process named `parsecd.exe`, and check the log, the files the
  plugins write and the environment the host ends up with. They also run
  the `crates/parsec-orbit-verify-*` tools (`callpaths`, `roundtrip`), so the forwarding checks of
  `scripts\verify.ps1` are part of `cargo test`.

The tests redirect `%APPDATA%` and `%TEMP%` into temporary directories and
never touch an installed Parsec client. CI (`.github/workflows/test.yml`) runs
the same command plus `cargo fmt --check`.

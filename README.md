# Parsec Orbit

An unofficial, local-only proof of concept for loading plugins in the Parsec
client. On Windows it uses a `bcrypt.dll` proxy and does not modify Parsec
files; on macOS arm64 it loads Orbit into a copy of `Parsec.app` through a
patched bundle (see [docs/spec/macos-host.md](docs/spec/macos-host.md)). The
macOS installation path is not usable yet: the guarded loader bootstrap (M1a)
and the transactional patcher (M1b) are implemented and measured on
disposable copies, while the signing/original-path gates (M1c) are still
open and no real vendor bundle has been applied.

## Layout

* `crates/parsec-orbit-loader/` - the `bcrypt.dll` proxy. It forwards every export to
  the real `bcrypt.dll`, opens the WebView2 DevTools endpoint and hosts the
  plugin system.
* `crates/parsec-orbit-loader-macos/` - the macOS arm64 counterpart: a
  `Platform` over `dlopen`/`dlsym`/`dlerror`, an append-only logger and the
  same plugin scan/host logic. It is a plain dylib for now; M1 loads it from a
  patched `Parsec.app`.
* `crates/parsec-orbit-installer-macos/` - the transactional patcher:
  inspect/apply/uninstall/status, a version-bound pristine backup plus
  append-only journal, a whitelisted header-space `LC_LOAD_DYLIB` patch,
  inside-out ad-hoc signing, verification, rollback and the preseed; see the
  [M1b checkpoint](docs/design/macos/installer-checkpoint.md).
* `crates/parsec-orbit-macos-state/` - the atomic isolated-state seed shared
  by the installer (as an rlib) and the macOS loader (which re-exports it).
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
* `crates/parsec-orbit-plugin-config/` and `crates/parsec-orbit-plugin-hooks/` -
  official plugins that provide `parsec-orbit.config` (settings that survive
  restarts) and `parsec-orbit.hooks` (inline function hooks).
* `crates/parsec-orbit-market/` - the `parsec-orbit-market` tool: plugin index,
  sha256 and ed25519 checks, safe installation; see
  [Marketplace](docs/spec/marketplace.md). `crates/parsec-orbit-devtools/` is
  the DevTools client the webview bridge and the verify tool share.
* `crates/parsec-orbit-plugin-js-runtime/` - official plugin that runs JavaScript
  and TypeScript plugins (`kind = "js"`) on Boa; see its
  [README](crates/parsec-orbit-plugin-js-runtime/README.md).
* `javascript-sdk/` - `@parsec-orbit/sdk` for JavaScript/TypeScript plugins;
  `examples/parsec-orbit-plugin-bandwidth-ts/` is the TypeScript sample (see
  [Write a TypeScript plugin](docs/guides/typescript-plugin.md)).
* `crates/parsec-orbit-plugin-high-bitrate-webview/` - official plugin that widens the
  "Bandwidth Limit" choices in the WebView2 UI; see its
  [README](crates/parsec-orbit-plugin-high-bitrate-webview/README.md).
* `crates/parsec-orbit-plugin-high-bitrate-native/` - official plugin for the same options
  in the native (libmatoya) UI; see its
  [README](crates/parsec-orbit-plugin-high-bitrate-native/README.md).
* `crates/parsec-orbit-plugin-stun-swap/` - experimental plugin that redirects
  `stun.parsec.app` lookups to mainland-China STUN servers; see its
  [README](crates/parsec-orbit-plugin-stun-swap/README.md).
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

The Windows suite needs Windows: the loader is a `bcrypt.dll` proxy and the
end-to-end tests build a host process named `parsecd.exe`. macOS arm64
(`macos-26`, the arm64 runner) runs the portable subset: the core, the macOS
loader platform and the std/no_std fixtures; the Windows-only crates compile
to empty libraries there. The JavaScript workspace needs Node.js 26+ and pnpm
12 (`packageManager` pins it); Node is also needed for the behaviour tests of
the script injected into the WebView UI; without it that one test is skipped
(set `PARSEC_ORBIT_REQUIRE_NODE=1` to make it fail instead).

* **Unit tests** sit next to the code: ABI layout (`abi`) and the export macro
  (`sdk`), settings, logging and the forwarding thunks (`loader`), manifests,
  plugin discovery and the descriptor checks (`core`), the DevTools client and
  the bridge session (`webview-bridge`) and the signature scan and in-memory
  patch, run against a fake PE image (`native`).
* **Integration tests** in `crates/parsec-orbit-plugin-webview-bridge/tests/` drive
  the DevTools client and the bridge session against a fake endpoint;
  `crates/parsec-orbit-plugin-high-bitrate-webview/tests/` runs
  `assets/bandwidth.js` in a fake browser under Node.
  `crates/parsec-orbit-installer-macos/tests/scratch_apply.rs` builds
  disposable arm64 toy bundles with `clang` and runs the real installer
  against them (round trip, dyld dependency, interruptions, refusals).
* **End-to-end tests** in `tests/parsec-orbit-e2e/` build the loader and the
  plugins into `target/e2e`, install them into a throwaway client directory
  next to a host process named `parsecd.exe`, and check the log, the files the
  plugins write and the environment the host ends up with. They also run
  the `crates/parsec-orbit-verify-*` tools (`callpaths`, `roundtrip`), so the forwarding checks of
  `scripts\verify.ps1` are part of `cargo test`.

The tests never touch an installed Parsec client: the Windows suite redirects
`%APPDATA%`/`%TEMP%` into temporary directories and the macOS e2e passes fresh
private plugin and log directories into the loader harness. CI
(`.github/workflows/test.yml`) runs the same command plus `cargo fmt --check`
on both platforms.

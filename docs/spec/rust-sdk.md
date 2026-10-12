# Rust SDK

Status: draft. Crates to be published on crates.io: `parsec-orbit-plugin-abi` and `parsec-orbit-plugin-sdk`.
Decision record: [ADR 0002](../design/decisions/0002-sdk-is-no-std-by-default.md).

## Crates

| Crate | Content | Dependencies |
| --- | --- | --- |
| `parsec-orbit-plugin-abi` | `repr(C)` definitions of the descriptor, host table, interface header and core service tables, constants, export names. `no_std`, no allocation. Other-language SDKs are checked against it. | none |
| `parsec-orbit-plugin-sdk` | Safe API: `Plugin`, `Context`, logging, service lookup, `declare_plugin!`. Re-exports the ABI crate as `abi`. | `-abi`; optional `tracing` |

Compatibility: SDK versions track [plugin-abi.md](plugin-abi.md). ABI-affecting changes follow its [versioning policy](plugin-abi.md#versioning-policy) and never break existing binaries without an `ABI_VERSION` bump.

## Features

| Feature | Default | Effect |
| --- | --- | --- |
| `runtime` | yes | Freestanding runtime: `#[global_allocator]` on the process heap and a `#[panic_handler]` that logs the panic through the host and aborts. Requires the plugin crate to be `#![no_std]`. |
| `std` | no | Enables `std`-only helpers (`Context::spawn`, `Error: std::error::Error`, `From<E: Error>`). Mutually exclusive with `runtime` (`compile_error!`). Use with `default-features = false`. |
| `tracing` | no | `Context::install_tracing()` installs a `tracing` subscriber forwarding events to the host `log_ex` with the plugin id as target. Implies `std`. |

## API sketch

```rust
#![no_std]
use parsec_orbit_plugin_sdk::prelude::*;

#[derive(Default)]
struct Hello;

impl Plugin for Hello {
    fn init(&mut self, cx: &Context) -> Result<(), Error> {
        info!(cx, "loaded by Parsec Orbit {}", cx.loader_version());
        if let Some(webview) = cx.service::<WebView>() {
            webview.add_script("hello", "console.log('hello from a plugin')")?;
        }
        Ok(())
    }
    fn shutdown(&mut self) {}
}

declare_plugin! {
    id = "com.example.hello",
    name = "Hello Plugin",
    version = "0.1.0",
    author = "Example",
    description = "Minimal plugin",
    license = "MIT",
    plugin = Hello,          // any `Default` type
}
```

* `Context` is `Copy + Send + Sync`. It wraps the per-plugin host table pointer (valid for the process lifetime) and replaces the hand-copied `struct Log(fn...)` the official plugins carry today.
* `Context` methods: `log(Level, fmt::Arguments)`, `plugin_id()`, `plugin_dir()`, `data_dir()`, `loader_version()`, `service::<I: Interface>()`, `register::<I>(&'static I::Table)`, `has_revision(n)` (checks `struct_size`).
* Logging macros `trace! debug! info! warn! error!` take a `Context` and `format_args!`; formatting goes through a 1 KiB stack buffer (truncating with `...`), without allocation.
* `Error` carries an `i32` code and an optional `&'static str`; `init` returning `Err` is reported to the loader as a non-zero code and logged.
* `declare_plugin!` generates `ParsecOrbitPluginQuery`, `ParsecOrbitPluginInit` and `ParsecOrbitPluginShutdown`, keeps the plugin value in a static slot (initialised once, never moved), and checks at init that the host table is large enough for what the SDK needs. The existing `parsec_orbit_plugin!` macro stays as a deprecated alias for one release.
* `trait Interface { const NAME: &'static CStr; const MAJOR: u16; type Table; }` is implemented for the core services (`WebView`, `Js`, `KindHandler`); typed wrappers expose safe methods and map error codes to `Error`.
* A plugin that talks to an older loader (host table smaller than revision 2) still initialises: `log` works, `service()` returns `None`.

## Size policy

1. Keep `runtime` on (default), make the plugin `#![no_std]`, use `core::fmt` and the SDK macros.
2. Release profile of the plugin crate: `panic = "abort"`, `opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `strip = "symbols"`, and `-C target-feature=+crt-static` (already in the workspace `.cargo/config.toml`).
3. Prefer host services over embedding functionality: ask `parsec-orbit.webview` instead of linking an HTTP/WebSocket client; ask the JS runtime instead of embedding an engine.
4. Dependencies that pull `std` (`serde_json`, `tokio`, ...) are the plugin's own choice; such plugins use the `std` feature and accept the size.

The Phase 3 spike measures and records: the current std `hello` (~210 KB), `no_std` + static CRT, and `no_std` with `/NODEFAULTLIB`. The result decides whether the CRT-free recipe becomes the documented default (amending ADR 0002).

## Testing support

A `testing` module (`std` builds only): `MockHost` builds a per-plugin host table with a recording `log_ex`, an in-memory registry and settable paths, so a plugin's `init` can be unit-tested in-process. The `hello` and `native` plugin tests do this by hand today.

## Samples

* `examples/parsec-orbit-plugin-hello` - the canonical sample; `no_std` if the spike confirms it, plus an `std` sibling showing the opt-in.
* `plugins/parsec-orbit-plugin-high-bitrate-native` - real-world `std` plugin (signature scan on a worker thread).
* `plugins/parsec-orbit-plugin-high-bitrate-webview` - real-world `std` plugin; once the webview bridge exists it shrinks to an `add_script` caller (see [js-sdk.md](js-sdk.md)).

The two official plugins are the SDK's regression baseline: their unit and e2e tests must pass unchanged after migration.

## Publishing

`-abi` first, then `-sdk`. Required metadata: `description`, `license`, `repository`, `readme`, `keywords`, `categories`, `rust-version`; `[package.metadata.docs.rs] default-target = "x86_64-pc-windows-msvc"`. CI runs `cargo publish --dry-run` for both. Actual publishing is a manual, explicit release step.

## Open questions

* Availability of the crate names on crates.io (check before Phase 3).
* Whether a proc-macro `#[plugin]` attribute is worth the compile-time cost over `declare_plugin!` (leaning: no).

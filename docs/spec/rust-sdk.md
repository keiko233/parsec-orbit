# Rust SDK

Status: implemented (`parsec-orbit-plugin-abi`, `parsec-orbit-plugin-sdk`, `parsec-orbit-plugin-sdk-std`); ready to publish to crates.io, see [Publishing](#publishing).
Decision record: [ADR 0002](../design/decisions/0002-sdk-is-no-std-by-default.md). Measurements: [size report](../design/size-report.md). A walk-through: [Write a Rust plugin](../guides/rust-plugin.md).

## Crates

| Crate | Content | Dependencies |
| --- | --- | --- |
| `parsec-orbit-plugin-abi` | `repr(C)` definitions of the descriptor, host table, interface header, kind-handler table, constants, export names. `no_std`, no allocation. Other-language SDKs are checked against it. | none |
| `parsec-orbit-plugin-sdk` | The safe API: `Plugin`, `Context`, `Error`, logging macros, `declare_plugin!`, typed service lookup, and the `freestanding!` runtime. `no_std`; re-exports the ABI crate (as `abi` and at the root). **Never links `std`.** | `-abi` |
| `parsec-orbit-plugin-sdk-std` | What needs `std`: `ContextExt` (`spawn`, `install_panic_hook`), `TracingExt` (feature `tracing`), `testing::MockHost`. Re-exports the SDK, so a `std` plugin depends on this crate only. | `-sdk`; optional `tracing-core` |

Compatibility: SDK versions track [plugin-abi.md](plugin-abi.md). ABI-affecting changes follow its [versioning policy](plugin-abi.md#versioning-policy) and never break existing binaries without an `ABI_VERSION` bump.

### Why `std` is a separate crate

A `no_std` plugin installs its own `#[panic_handler]`, which the compiler rejects as soon as the `std` crate is anywhere in the plugin's dependency graph. Cargo unifies features across one build, so a `std` cargo feature on the SDK would break every `no_std` plugin that is built together with a `std` one (a workspace, `cargo check --all-targets`, the dev-dependencies of the plugin's own tests). A crate boundary cannot be unified away; a feature can. The SDK therefore has no features at all.

## API

```rust
#![no_std]
use parsec_orbit_plugin_sdk::prelude::*;

parsec_orbit_plugin_sdk::freestanding!();   // allocator, panic handler, CRT stand-ins

#[derive(Default)]
struct Hello;

impl Plugin for Hello {
    fn init(&mut self, cx: &Context) -> Result {
        info!(cx, "loaded by Parsec Orbit {}", cx.loader_version());
        Ok(())
    }
    fn shutdown(&mut self) {}
}

parsec_orbit_plugin_sdk::declare_plugin! {
    id = "com.example.hello",
    name = "Hello Plugin",
    version = "0.1.0",
    author = "Example",
    description = "Minimal plugin",
    license = "MIT",
    plugin = Hello,          // any `Default` type; optional `capabilities = <u64>`
}
```

* `Context` is `Copy + Send + Sync`: a handle to the plugin's own host table, valid for the process lifetime. It replaces the hand-copied `struct Log(fn...)` the official plugins used to carry. Methods: `log(Level, fmt::Arguments)`, `plugin_id()`, `plugin_dir()`, `data_dir()`, `loader_version()`, `abi_version()`, `has_revision(n)`, `service::<I>()`, `register::<I>(&'static I::Table)`, `register_as(name, header)`, `unregister_as(name)`, `register_kind_handler(kind, &'static ParsecOrbitKindHandlerV1)`, `host()` (the raw table).
* `trace! debug! info! warn! error!` take a `Context` and `format_args!`. Formatting goes through a 1 KiB stack buffer; a longer line is cut at a character boundary and ends in `...`. Nothing is allocated. On a revision 2 loader the line carries the plugin id as target (`log_ex`), on revision 1 it is a plain `log`.
* `Error` is a non-zero `i32` code and an optional `&'static str`; `Error::new(0)` becomes code 1 because 0 is success. Any `core::error::Error` converts with `?` (lossy: the details are not kept, log them first). `init` returning `Err` is logged (`init failed: <message> (code N)`) and reported to the loader as that code; the loader then marks the plugin failed (`init returned N`).
* `declare_plugin!` generates `ParsecOrbitPluginQuery`, `ParsecOrbitPluginInit` and `ParsecOrbitPluginShutdown`. The plugin value is created with `Default` when `init` runs, lives in a static slot and is never moved; a second `init` is refused (`ERR_EXISTS`), `shutdown` runs `Plugin::shutdown` and drops the value once. `init` refuses a host table smaller than the revision 1 layout (`ERR_INVALID`), so a plugin built against this SDK still initialises on an old loader: `log` works, `service()` returns `None`, `register()` fails with `ERR_NOT_READY`.
* `trait Interface { const NAME: &CStr; const MAJOR: u16; const MIN_SIZE: usize; type Table; fn header(&Table) -> &ParsecOrbitInterfaceHeader }` binds a service. `service::<I>()` returns the table only if the major version matches and `header.size >= MIN_SIZE`. Bindings for the core services (`WebView`, `Js`) are added with the providers in phase 4; the kind handler is built with `services::kind_handler(load_plugin, unload_plugin)`.
* `parsec_orbit_plugin!` (the pre-`Plugin` macro that takes a raw `init`) still works and is deprecated; it goes in the release after the SDK is published.

### `freestanding!`

For `#![no_std]` plugins. Call it once at the crate root. It emits, outside the crate's own test build (which links `std`):

* a `#[global_allocator]` on the process heap (`HeapAlloc`, over-aligned blocks supported),
* a `#[panic_handler]` that writes `panic: <message> at <file>:<line>:<col>` through the host log and then fast-fails the process (there is no safe way to continue in native code inside the client),
* the C runtime functions the compiler calls (`memcpy`, `memmove`, `memset`, `memcmp`, `bcmp`, `strlen`, `__chkstk`, `_fltused`, `__CxxFrameHandler3` stub, `_DllMainCRTStartup`) as private assembly,
* the linker directive `/NODEFAULTLIB`.

The plugin crate has to be built with `panic = "abort"` in **every** profile (`[profile.dev] panic = "abort"` in the workspace manifest), and only `x86_64-pc-windows-msvc` is supported. A `std` plugin does not call it.

### `parsec-orbit-plugin-sdk-std`

```rust
use parsec_orbit_plugin_sdk_std::prelude::*;   // SDK prelude + ContextExt (+ TracingExt)

cx.spawn("worker", move || { /* long work */ })?;   // thread named "<plugin id>:worker"
cx.install_panic_hook();                             // panics go to the loader log
cx.install_tracing()?;                               // feature `tracing`: tracing events -> loader log
```

`testing::MockHost` builds the host table the loader would build for one plugin so `init` can be unit-tested in-process, `no_std` plugins included (as a dev-dependency):

```rust
let host = MockHost::new("com.example.hello").with_data_dir(dir);
let (plugin, result) = host.init::<Hello>();
assert!(host.logged("loaded by Parsec Orbit"));
host.provide("com.example.svc", &TABLE.header);   // a service some other plugin "registered"
```

`MockHost::revision_1(id)` is what an old loader hands over. Callbacks have no context argument, so the recorded log and registry are kept per test thread; work on threads the plugin starts is not recorded.

## Size policy

1. Prefer `#![no_std]` + `freestanding!`: 10 KB for a hello plugin against 210 KB with `std` ([size report](../design/size-report.md)).
2. Release profile of the plugin: `panic = "abort"`, `opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `strip = "symbols"` (already the workspace profile). The static-CRT flag in `.cargo/config.toml` matters only for `std` plugins.
3. Prefer host services over embedding functionality: ask `parsec-orbit.webview` instead of linking an HTTP/WebSocket client; ask the JS runtime instead of embedding an engine.
4. Dependencies that pull `std` (`serde_json`, `tokio`, ...) are the plugin's own choice; such plugins use `parsec-orbit-plugin-sdk-std` and accept the size.

## Samples

* `examples/parsec-orbit-plugin-hello-nostd` - the canonical sample: 10 KB, no imports.
* `examples/parsec-orbit-plugin-hello` - the `std` sibling (writes a report file).
* `plugins/parsec-orbit-plugin-high-bitrate-native` - real-world `std` plugin (signature scan on a worker thread).
* `plugins/parsec-orbit-plugin-high-bitrate-webview` - real-world `std` plugin; once the webview bridge exists it shrinks to an `add_script` caller (see [js-sdk.md](js-sdk.md)).
* `tests/parsec-orbit-fixture-nostd`, `tests/parsec-orbit-fixture-plugin` - test fixtures that exercise the runtime and the raw ABI inside a real process.

The two official plugins are the SDK's regression baseline: their unit and end-to-end tests pass with the same assertions after the migration; only the way `init` is driven changed (through `MockHost`).

## Publishing

`-abi` first, then `-sdk`, then `-sdk-std`. Required metadata is in the manifests: `description`, `license`, `repository`, `readme`, `keywords`, `categories`, `rust-version`, and `[package.metadata.docs.rs] default-target = "x86_64-pc-windows-msvc"`. CI runs `cargo package` for the three crates (see `.github/workflows/test.yml`), which builds each one from its own tarball against a local registry holding the others. Actual publishing is a manual, explicit step by the owner, described in [releasing](../design/releasing.md). The three names were free on crates.io on 2026-10-12.

## Open questions

* Whether a proc-macro `#[plugin]` attribute is worth the compile-time cost over `declare_plugin!` (leaning: no).
* `aarch64-pc-windows-msvc` support for `freestanding!` (assembly for the C runtime functions).

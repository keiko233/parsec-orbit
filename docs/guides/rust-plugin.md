# Write a Rust plugin

A Parsec Orbit plugin is a dynamic library that the loader maps into the Parsec client. This guide builds a small one with the Rust SDK, tests it, and installs it. The reference for everything used here is [spec/rust-sdk.md](../spec/rust-sdk.md).

`x86_64-pc-windows-msvc` or `aarch64-apple-darwin`, Rust 1.81 or newer. The
guide's paths and install steps are the Windows ones; on macOS the artifact is
a `.dylib` and the install path only exists once the macOS loader ships
(M1 in [macos-phases.md](../design/macos-phases.md)). Everything else - the
manifest, the SDK code and the tests - is identical apart from
`entry = "my_plugin.dylib"`.

## 1. The smallest plugin (`no_std`, about 10 KB)

```text
my-plugin/
  Cargo.toml
  plugin.toml
  src/lib.rs
```

`Cargo.toml`:

```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
parsec-orbit-plugin-sdk = "0.1"

[dev-dependencies]
# MockHost for unit tests; the plugin itself never links std.
parsec-orbit-plugin-sdk-std = "0.1"

# A no_std plugin cannot unwind, so abort in every profile.
[profile.dev]
panic = "abort"
[profile.release]
panic = "abort"
opt-level = "z"
lto = "fat"
codegen-units = 1
strip = "symbols"
```

`src/lib.rs`:

```rust
#![cfg_attr(not(test), no_std)]

use parsec_orbit_plugin_sdk::prelude::*;

// Allocator, panic handler and the C runtime stand-ins. Not in test builds.
parsec_orbit_plugin_sdk::freestanding!();

#[derive(Default)]
struct MyPlugin;

impl Plugin for MyPlugin {
    fn init(&mut self, cx: &Context) -> Result {
        info!(cx, "hello from {} (loader {})", cx.plugin_id(), cx.loader_version());
        Ok(())
    }
}

parsec_orbit_plugin_sdk::declare_plugin! {
    id = "com.example.my-plugin",
    name = "My Plugin",
    version = "0.1.0",
    author = "You",
    description = "Does a thing",
    license = "MIT",
    plugin = MyPlugin,
}
```

`plugin.toml` (see [spec/plugin-manifest.md](../spec/plugin-manifest.md)):

```toml
id = "com.example.my-plugin"
name = "My Plugin"
version = "0.1.0"
entry = "my_plugin.dll"
```

Keep `id` (and `name`, `version`, ...) equal to the values in `declare_plugin!`; the loader logs a warning when they disagree. Build for macOS and write `entry = "my_plugin.dylib"` instead - an entry without a suffix is completed with the platform's (`dll` on Windows, `dylib` on macOS).

Build with `cargo build --release`. The DLL is `target/release/my_plugin.dll`
(`libmy_plugin.dylib` on macOS).

### What `init` can do

* `info!(cx, ...)`, `warn!`, `error!`, `debug!`, `trace!` - one line in the loader log, tagged with your plugin id. Formatting needs no allocator.
* `cx.plugin_dir()`, `cx.data_dir()` - where you were loaded from, and where your data may go (`%APPDATA%\ParsecOrbit\data\<id>` on Windows, `~/Library/Application Support/ParsecOrbit/data/<id>` on macOS; create it yourself).
* `cx.service::<MyService>()` - look up an interface you bound with
  `Interface`; `None` if it is absent or incompatible. Bindings for the core
  `WebView` and `Js` services arrive with their providers in phase 4. List the
  provider in `requires` in `plugin.toml` to have it initialised first.
* `cx.register::<MyService>(&TABLE)` - offer a service to later plugins.
* Return `Err(Error::with_message(code, "why"))` to fail; the loader skips plugins that depend on you.

`init` runs on the loader's single worker thread and later plugins wait for it. Do not block there; start a thread (needs `std`, see below) and return.

## 2. Testing

`MockHost` builds the host table the loader would build for you, records the log and keeps a registry:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_plugin_sdk_std::testing::MockHost;

    #[test]
    fn greets_the_loader() {
        let host = MockHost::new("com.example.my-plugin");
        let (_plugin, result) = host.init::<MyPlugin>();
        assert!(result.is_ok());
        assert!(host.logged("hello from com.example.my-plugin"));
    }
}
```

`cargo test` links `std` for the test harness, which is why `freestanding!()` and `no_std` are switched off with `cfg(test)`. Use `MockHost::revision_1(id)` to check that your plugin still works on an old loader.

## 3. A plugin that needs `std`

Threads, files, sockets: depend on `parsec-orbit-plugin-sdk-std` instead (it re-exports the SDK), do not call `freestanding!()`, and drop `#![no_std]`. Expect about 200 KB, which is what `std` costs.

```rust
use parsec_orbit_plugin_sdk_std::prelude::*;

impl Plugin for MyPlugin {
    fn init(&mut self, cx: &Context) -> Result {
        cx.install_panic_hook();                  // panics go to the loader log
        let cx = *cx;                             // Context is Copy + Send + Sync
        cx.spawn("worker", move || {
            info!(cx, "doing the long thing on my own thread");
        })?;
        Ok(())
    }
}
```

Do not mix the two in one build graph: a `no_std` plugin and the `std` crate cannot coexist as dependencies of the same crate. They can sit in the same workspace.

## 4. Install

Copy the folder (`plugin.toml` plus the DLL) to `<Parsec client directory>\plugins\my-plugin\` or `%APPDATA%\ParsecOrbit\plugins\my-plugin\`, and start the client. The loader log (`%APPDATA%\ParsecOrbit\parsec-orbit.log`, or `PARSEC_ORBIT_LOG`) says what happened:

```text
[info] loaded com.example.my-plugin 0.1.0 (My Plugin) ABI 1 by You [MIT] - Does a thing - init returned 0
[info] com.example.my-plugin: hello from com.example.my-plugin (loader 0.1.0)
[info] ready: 1 loaded, 0 skipped, 0 failed
```

To try a plugin without installing it, point `PARSEC_ORBIT_PLUGIN_DIR` at a directory that contains it.

On macOS the loader reads `<Orbit root>/plugins` and `PARSEC_ORBIT_PLUGIN_DIR`
as well, with `:` as the separator (`PARSEC_ORBIT_HOME` moves the root from
`~/Library/Application Support/ParsecOrbit`), but there is no installed loader
to put a plugin in yet: that arrives with M1, and until then the macOS loader
is exercised by `cargo test -p parsec-orbit-loader-macos` alone.

## Samples

* [`examples/parsec-orbit-plugin-hello-nostd`](../../examples/parsec-orbit-plugin-hello-nostd) - this guide's plugin.
* [`examples/parsec-orbit-plugin-hello`](../../examples/parsec-orbit-plugin-hello) - the `std` flavour (writes a report file).
* [`crates/parsec-orbit-plugin-high-bitrate-native`](../../crates/parsec-orbit-plugin-high-bitrate-native) - a real `std` plugin with a worker thread.

## Troubleshooting

* *`unwinding panics are not supported without std`* - set `panic = "abort"` in `[profile.dev]` (and release) of the **workspace** manifest.
* *`found duplicate lang item panic_impl`* - the `std` crate is in the dependency graph of a `no_std` plugin: a dependency pulls `std`, or the plugin depends on `parsec-orbit-plugin-sdk-std` as a normal dependency. Make it a dev-dependency.
* *The plugin is skipped with `needs X, not installed`* - a `requires` entry in `plugin.toml` names a plugin that is not installed.

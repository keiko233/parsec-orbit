# parsec-orbit-plugin-sdk

Write [Parsec Orbit](https://github.com/keiko233/parsec-orbit) plugins in Rust.

```rust
#![no_std]
use parsec_orbit_plugin_sdk::prelude::*;

parsec_orbit_plugin_sdk::freestanding!();

#[derive(Default)]
struct Hello;

impl Plugin for Hello {
    fn init(&mut self, cx: &Context) -> Result {
        info!(cx, "loaded by Parsec Orbit {}", cx.loader_version());
        Ok(())
    }
}

parsec_orbit_plugin_sdk::declare_plugin! {
    id = "com.example.hello",
    name = "Hello Plugin",
    version = "0.1.0",
    author = "Example",
    description = "Minimal plugin",
    license = "MIT",
    plugin = Hello,
}
```

That plugin is about 10 KB: `freestanding!()` supplies an allocator on the
process heap, a panic handler that logs through the loader and the few C
runtime functions compiled code calls, and keeps the C runtime out of the DLL.
Build it as a `cdylib` with `panic = "abort"` in every profile.

* `Plugin`, `Context`, `Error`, `info!`/`warn!`/..., typed service lookup and
  registration, kind-handler registration.
* `no_std` and **never links `std`**; what needs `std` (threads, `tracing`
  bridge, a mock host for unit tests) is in
  [`parsec-orbit-plugin-sdk-std`](https://crates.io/crates/parsec-orbit-plugin-sdk-std).
* Re-exports [`parsec-orbit-plugin-abi`](https://crates.io/crates/parsec-orbit-plugin-abi).

Specification: [`docs/spec/rust-sdk.md`](https://github.com/keiko233/parsec-orbit/blob/main/docs/spec/rust-sdk.md).
Guide: [Write a Rust plugin](https://github.com/keiko233/parsec-orbit/blob/main/docs/guides/rust-plugin.md).

`freestanding!` supports `x86_64-pc-windows-msvc` and (with a separate
libSystem flavour) `aarch64-apple-darwin`.

## License

MIT

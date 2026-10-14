# 0002 - The Rust SDK is `no_std` by default

## Context
A Rust `cdylib` that links `std` carries ~200 KB before doing anything (the current `hello` plugin is ~210 KB). Plugins are numerous and small; the size is almost entirely runtime, not logic.

## Decision
`parsec-orbit-plugin-sdk` is `no_std` and brings its own freestanding runtime, installed by calling `freestanding!()`: a `#[global_allocator]` on the process heap, a `#[panic_handler]` that logs through the host and aborts, the few C runtime functions compiled code calls, and `/NODEFAULTLIB`; logging is `core::fmt` into a stack buffer. What needs `std` (threads, the `tracing` bridge, a mock host for tests) is in a separate crate, `parsec-orbit-plugin-sdk-std`.

## Consequences
* Smallest plugins by default; opting into `std` is a different dependency.
* Existing std-based code (`webview` plugin's HTTP/WebSocket client, `native` plugin's scan thread) depends on `parsec-orbit-plugin-sdk-std`.

## Amendment (phase 3): spike result and two changes

**The CRT-free variant is practical** and is the default recipe. Measured ([size report](../size-report.md)): `std` hello 210 KB, `no_std` + static CRT 103 to 129 KB, `no_std` without the CRT 3.5 KB (bare) to 28 KB (with `core::fmt` and floats); the SDK's `hello-nostd` is 10.7 KB. The C runtime pieces are `global_asm!` (as Rust `#[no_mangle]` functions they would be exported from the DLL), and `/NODEFAULTLIB` travels in the object file's `.drectve`, so a plugin crate needs no build script. It needs `panic = "abort"` in every profile and is x86_64 only.

**The runtime is a macro, not a cargo feature, and `std` is a crate, not a feature.** The original text had features `runtime` (default) and `std`, mutually exclusive. That cannot work: a `no_std` plugin's `#[panic_handler]` is a compile error as soon as the `std` crate is anywhere in its dependency graph, and Cargo unifies features across one build - so one `std` plugin in a workspace (or in `cargo check --all-targets`, through the dev-dependencies of the plugin's own tests) would break every `no_std` plugin. A crate boundary cannot be unified away. The SDK has no features; `freestanding!()` expands the runtime in the plugin crate (and not in its test build).

## Amendment (restructure)

The separate `sdk-std` crate is replaced by one SDK package for both kinds of plugin and a test-only `parsec-orbit-plugin-sdk-testing`; the feature question was re-run and fails the same way. See [0017](0017-one-sdk-package-no-features.md).

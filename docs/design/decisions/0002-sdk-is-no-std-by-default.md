# 0002 - The Rust SDK is `no_std` by default

## Context
A Rust `cdylib` that links `std` carries ~200 KB before doing anything (the current `hello` plugin is ~210 KB). Plugins are numerous and small; the size is almost entirely runtime, not logic.

## Decision
`parsec-orbit-plugin-sdk` defaults to `no_std` and brings its own freestanding runtime (feature `runtime`, on by default): a `#[global_allocator]` on the process heap, a `#[panic_handler]` that logs through the host and aborts, and `core::fmt`-based logging into a stack buffer. Feature `std` switches the runtime off and enables `std` conveniences (threads, `std::error::Error`); feature `tracing` adds the `tracing` bridge. `runtime` and `std` are mutually exclusive.

## Consequences
* Smallest plugins by default; opting into `std` is one line.
* Existing std-based code (`webview` plugin's HTTP/WebSocket client, `native` plugin's scan thread) keeps using `std` via `default-features = false, features = ["std"]`.
* Needs a **spike before implementation**: measure std vs `no_std`+CRT vs `no_std` without CRT on MSVC (memcpy/entry point). If the CRT-free variant is not practical, the default is `no_std` + static CRT and this record is amended.

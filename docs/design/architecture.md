# Architecture

## Goals

1. A **standard SDK for Rust** (crates.io) and **TypeScript** (npm) so plugins are small, uniform and easy to write.
2. A loader that stays **tiny and boring**: it sits in front of a foreign process (`bcrypt.dll` proxy) and must not grow into a platform.
3. **Plugins do not pay for what they do not use**: no statically linked `std`/async stack by default, no JS engine unless a JS plugin is installed.
4. Reuse established crates (`tracing`, `ractor`, ...) where they pay for themselves - in plugins and tools, not in the loader's hot path.

## Components

```text
                      parsecd.exe (Parsec client process)
 +-----------------------------------------------------------------------+
 |  bcrypt.dll  =  parsec-orbit-loader  (cdylib, thin)                    |
 |    thunks -> real bcrypt            Win32 Platform impl                |
 |    +----------------------------------------------------------+       |
 |    | parsec-orbit-core (rlib): manifest, resolver, registry,   |       |
 |    | service registry, host tables                             |       |
 |    +----------------------------------------------------------+       |
 |        | ParsecOrbitHost (per plugin)          ^ services              |
 |        v                                       |                       |
 |  [native plugin]  [webview-bridge plugin] [js-runtime plugin]  ...      |
 |   Rust SDK / C      provides                provides kind.js,          |
 |                     parsec-orbit.webview    parsec-orbit.js            |
 |                              ^                     ^                   |
 |                              |   requires          |  hosts            |
 |                              +-----[js plugin: plugin.toml + main.js]  |
 +-----------------------------------------------------------------------+
```

* **Loader** (`parsec-orbit-loader`): `DllMain`, bcrypt thunks, a `Platform` implementation on `windows-sys`. Nothing else.
* **Core** (`parsec-orbit-core`): everything that does not need Win32 - manifest parsing, dependency resolution, load decisions, service registry, per-plugin host tables. Written against two traits (`Platform`: map a library / resolve a symbol; `Logger`), so it is unit-tested without mapping DLLs.
* **Plugins** are DLLs speaking the C ABI in [spec/plugin-abi.md](../spec/plugin-abi.md). The same ABI is used for *services*: a plugin can register typed vtables that other plugins query ([spec/services.md](../spec/services.md)).
* **JS runtime is a plugin** (`kind.js` handler). A JS plugin declares `requires = ["com.parsec-orbit.js-runtime"]`; with the runtime absent (the default install) the loader skips it with a clear message. See [ADR 0003](decisions/0003-js-runtime-is-a-plugin.md).
* **WebView operations are a plugin too** (`parsec-orbit.webview`), because the DevTools client (HTTP + WebSocket) is not something the bcrypt proxy should carry. The loader keeps only the existing job of opening the DevTools port.

## Crate graph

```text
parsec-orbit-plugin-abi      no_std, zero deps. repr(C) types, constants, service structs.      [crates.io]
        ^
        |-- parsec-orbit-plugin-sdk   safe Rust API on top of the ABI; no_std by default.        [crates.io]
        |-- parsec-orbit-core         loader logic (std; toml parse-only)
                ^
                +-- parsec-orbit-loader   cdylib named `bcrypt`

plugins/    parsec-orbit-plugin-native, parsec-orbit-plugin-webview (official)   -> sdk
            (Phase 4) js-runtime, webview-bridge                                  -> sdk + heavy deps
examples/   parsec-orbit-plugin-hello  (+ nostd variant)                          -> sdk
sdk/js/     @parsec-orbit/sdk (TypeScript)                                        [npm]
tests/      parsec-orbit-e2e;  verify/  verification tools
```

Directory layout: the loader and the SDK live under `crates/` (done); the SDK is split into `-abi` and `-sdk` (done); `-core` is next (Phase 1).

## Size policy

The loader was just reduced from ~520 KB to ~340 KB; that is the baseline to protect.

| Artifact | Budget | Mechanism |
| --- | --- | --- |
| loader `bcrypt.dll` | <= 360 KB (baseline 346,624 B) | no async runtime, no serde, no `tracing` unless measured to fit |
| minimal Rust plugin | goal: well below the current ~210 KB std `hello` | `no_std` SDK default, `panic=abort`, `opt-level=z`, LTO, strip, CRT static |
| JS runtime plugin | unconstrained (separate download) | Boa + tokio/ractor live here only |

How the SDK keeps plugins small is specified in [spec/rust-sdk.md](../spec/rust-sdk.md#size-policy): `no_std` + a small freestanding runtime (allocator on the process heap, panic handler that logs and aborts), and *services over statically linked functionality* - the host and service plugins expose vtables so a plugin does not embed its own HTTP/WebSocket/JS stack.

## Concurrency and logging policy

* The loader has one worker thread and runs plugin `init` on it sequentially in dependency order. Plugins that need to run long must spawn their own thread (`Context::spawn`, `std` feature) and return from `init`.
* **Logging**: the loader owns the log file. Plugins log through the host (`log_ex`, with the plugin id as target). The SDK's own macros need no dependencies; the `tracing` feature adds a `tracing::Subscriber` that forwards to the host so ecosystem crates' events end up in the same file. The loader itself adopts `tracing-core` only if the size budget allows ([ADR 0004](decisions/0004-loader-stays-light.md)).
* **Actors**: `ractor` is adopted in the heavy plugins. The JS runtime runs each JS plugin's Boa context (which is not `Send`) inside its own actor on a dedicated thread; the webview bridge runs the DevTools session as an actor with supervised reconnects. Neither the loader nor the Rust SDK depends on `ractor`.

## Failure model

A plugin can never take the loader down by failing to load: every failure (missing file, bad descriptor, unmet dependency, non-zero `init`) is logged and the plugin is skipped; dependents of a failed plugin are skipped too. A *panic or fault inside plugin code* is not recoverable - plugins are native code in the client process ([security notes](../spec/plugin-abi.md#security-notes)) - so the SDK's panic policy is log-then-abort.

## Open questions

1. Whether the JS runtime and the webview bridge ship as one DLL (shared tokio/ractor, one download) or two. Leaning towards two plugins first, merge if the duplicated runtime proves too heavy.
2. Plugin unload/reload: still out of scope; interfaces registered by a plugin live for the process lifetime.

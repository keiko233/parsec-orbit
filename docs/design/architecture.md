# Architecture

## Goals

1. A **standard SDK for Rust** (crates.io) and **TypeScript** (npm) so plugins are small, uniform and easy to write.
2. A loader that stays **tiny and boring**: it sits in front of a foreign process (`bcrypt.dll` proxy on Windows; a dependency-loaded dylib in a locally patched app on macOS) and must not grow into a platform.
3. **Plugins do not pay for what they do not use**: no statically linked `std`/async stack by default, no JS engine unless a JS plugin is installed.
4. Reuse established crates (`tracing`, `serde_json`, Boa, ...) where they pay for themselves - in plugins and tools, not in the loader's hot path.

## Components

```text
                      parsecd.exe (Parsec client process)
 +-----------------------------------------------------------------------+
 |  bcrypt.dll  =  parsec-orbit-loader  (cdylib, thin)                    |
 |    thunks -> real bcrypt            Win32 Platform impl                |
 |    +----------------------------------------------------------+       |
|    | parsec-orbit-core (rlib): manifest, resolver, registry,   |       |
|    | service registry, host tables (resolver/registry: done)   |       |
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

* **Loader** (`parsec-orbit-loader`): `DllMain`, bcrypt thunks, a `Platform`
  implementation on `windows-sys` and the opt-in debug console window. Nothing
  else.
* **Core** (`parsec-orbit-core`): everything that does not need Win32: manifest
  parsing, discovery, dependency resolution, the service registry and
  per-plugin host tables. Written against traits (`Platform`: map a library /
  resolve a symbol; `Logger`; `console::Console`: show a log line), so it is
  unit-tested without mapping DLLs.
* **Plugins** are DLLs speaking the C ABI in [spec/plugin-abi.md](../spec/plugin-abi.md). The same ABI is used for *services*: a plugin can register typed vtables that other plugins query ([spec/services.md](../spec/services.md)).
* **JS runtime is a plugin** (`kind.js` handler). A JS plugin declares `requires = ["com.parsec-orbit.js-runtime"]`; with the runtime absent (the default install) the loader skips it with a clear message. See [ADR 0003](decisions/0003-js-runtime-is-a-plugin.md).
* **WebView operations are a plugin too** (`parsec-orbit.webview`), because the DevTools client (HTTP + WebSocket) is not something the bcrypt proxy should carry. The loader keeps only the existing job of opening the DevTools port.

### macOS

The pristine macOS app has no compatible proxy slot. The target is a supported local bootstrap dependency patch, keeping the installed app path/id and loading the Orbit dylib on normal launch ([ADR 0014](decisions/0014-macos-patched-bundle.md), [spec](../spec/macos-host.md)). The disposable-copy PoC passed; original-path identity and distribution remain gates. Loader-as-Host is optional fallback/development mode.

```text
 /Applications/Parsec.app   (locally patched and re-signed)
 +-----------------------------------------------------------------------+
 | parsecd = vendor stub + supported LC_LOAD_DYLIB command                 |
 |   original instructions: isolated state -> hash -> client -> wx_main   |
 | libparsec_orbit_loader.dylib (dependency, no DYLD env launcher)         |
 |   activation/isolation -> Platform/Logger -> core -> native plugins    |
 +-----------------------------------------------------------------------+
 HR retained + disable LV; vendor outer signing identity is replaced
 Orbit state/plugins/backups live outside the bundle in the private root
```

The shared parts are `parsec-orbit-core` and the plugin ABI; the Win32 `Platform` and the thunks have macOS counterparts in a separate loader crate. The size and concurrency policies below apply to both.

## Crate graph

```text
parsec-orbit-plugin-abi      no_std, zero deps. repr(C) types, constants, service structs.      [crates.io]
        ^
        |-- parsec-orbit-plugin-sdk   safe Rust API + freestanding runtime; no_std, never std. [crates.io]
        |       ^
        |       +-- parsec-orbit-plugin-sdk-std   threads, panic hook, tracing bridge, MockHost   [crates.io]
        |-- parsec-orbit-core         loader logic (std; toml parse-only)
        |-- parsec-orbit-devtools     HTTP + WebSocket + DevTools protocol client (std)
        |-- parsec-orbit-market       index, sha256/ed25519, safe installation, the market tool
                ^
                +-- parsec-orbit-loader   cdylib named `bcrypt`

crates/     every Rust crate: the loader and SDK above, the official plugins and
            the verification tools; high-bitrate-native and high-bitrate-webview -> sdk-std
            (official), webview-bridge (-> devtools), js-runtime (Boa), config,
            hooks (retour)                                                          -> sdk-std + heavy deps
examples/   parsec-orbit-plugin-hello (-> sdk-std), parsec-orbit-plugin-hello-nostd (-> sdk)
javascript-sdk/  @parsec-orbit/sdk (TypeScript: types, definePlugin, ui helper, mock host)  [npm]
tests/      parsec-orbit-e2e;  crates/parsec-orbit-verify-*  verification tools
```

Directory layout: every Rust crate - the loader, the SDKs, the official plugins and the verification tools - lives under `crates/` (done); the SDK is split into `-abi` and `-sdk` (done); `-core` holds the Win32-free loading logic (done).

## Size policy

The loader was just reduced from ~520 KB to ~340 KB; that is the baseline to protect.

| Artifact | Budget | Mechanism |
| --- | --- | --- |
| loader `bcrypt.dll` | <= 360 KiB (368,640 B). Phase 2 stayed just under it (368,128 B, 512 B headroom); phase 5 is over it at 374,272 B, and the debug console takes that to 381,952 B - see the [size report](../design/size-report.md) | no async runtime, no serde, no `tracing` unless measured to fit |
| minimal Rust plugin | goal: well below the current ~210 KB std `hello` | CRT-free `no_std` recipe (`freestanding!`), `panic=abort`, `opt-level=z`, LTO, strip |
| JS runtime plugin | unconstrained (separate download) | Boa lives here only |

How the SDK keeps plugins small is specified in [spec/rust-sdk.md](../spec/rust-sdk.md#size-policy): `no_std` + a small freestanding runtime (allocator on the process heap, panic handler that logs and fast-fails), and *services over statically linked functionality* - the host and service plugins expose vtables so a plugin does not embed its own HTTP/WebSocket/JS stack.

## Concurrency and logging policy

* The loader has one worker thread and runs plugin `init` on it sequentially in
  dependency order. Plugins that need to run long must spawn their own thread
  (`ContextExt::spawn` from `parsec-orbit-plugin-sdk-std`) and return from
  `init`.
* **Logging**: the loader owns the log file. Plugins log through the host (`log_ex`, with the plugin id as target). The SDK's own macros need no dependencies; the `tracing` feature adds a `tracing::Subscriber` that forwards to the host so ecosystem crates' events end up in the same file. The loader itself adopts `tracing-core` only if the size budget allows ([ADR 0004](decisions/0004-loader-stays-light.md)).
* **Debug console**: `PARSEC_ORBIT_CONSOLE=1` makes the loader open a console window at start-up and mirror every line of that log into it, plus Parsec's own log, which `parsec-orbit-core` follows (the window and its Win32 calls stay in the loader). `client` and `host` open it in one of the client's two instances instead of both, because a console belongs to a process and the service's instance is a second one. It is opt-in because the client is a GUI process that normally has no console at all, and best effort because a debug aid must never change the load.
* **Actors, without a runtime**: the heavy plugins are one thread that owns a resource plus a command channel - an actor in shape only, no `ractor` and no async runtime ([ADR 0004](decisions/0004-loader-stays-light.md#amendment-phase-4)). The JS runtime gives each Boa context (which is not `Send`) a thread of its own; the webview bridge gives the DevTools session one thread that reconnects and replays the scripts, and runs subscriber callbacks on a second thread so a callback can call back in.

## Failure model

A plugin can never take the loader down by failing to load. In ABI 1 failures
such as a bad descriptor or non-zero `init` are logged and counted as failed;
disabled, duplicate and not-yet-runtime-backed kinds are skipped. Dependency
ordering (Phase 2) adds load order and skips dependents of failed plugins. A
*panic or fault inside plugin code* is not recoverable - plugins are native
code in the client process ([security notes](../spec/plugin-abi.md#security-notes))
- so the SDK's panic policy is log-then-abort.

## Open questions

1. Whether the JS runtime and the webview bridge should ship as one download. They are two plugins today; they share no runtime, so merging is a packaging question only.
2. Plugin unload/reload: still out of scope; interfaces registered by a plugin live for the process lifetime.

# 0004 - The loader stays light

## Context
The loader is mapped into every Parsec client start, was recently shrunk 520 -> 340 KB, and any crash in it crashes the client. Heavy crates (`tokio`, `ractor`, `serde`) would undo that.

## Decision
No async runtime, no `ractor`, no `serde` in `parsec-orbit-loader` or `parsec-orbit-core`. `tracing` is allowed in the loader only as `tracing-core` + a small file subscriber, and only if the DLL grows by < 30 KB; otherwise the existing `log` module is kept behind the `Logger` trait and the measurement is recorded here. Established crates are used freely in plugins and tools: `serde_json` in the webview bridge, Boa in the JS runtime, the `tracing` bridge in `parsec-orbit-plugin-sdk-std`.

### Amendment (phase 4)

`ractor` (and with it `tokio`) was planned for the webview bridge and the JS runtime. It was not needed: both are one thread that owns a socket or a Boa context plus a command channel, which is what an actor is, without an async runtime, its glue code and its size. See [ADR 0003](0003-js-runtime-is-a-plugin.md).

## Consequences
Standardisation inside the loader is by traits (`Platform`, `Logger`), not by dependencies.

## Outcome
Phase 1 kept the existing `log` module behind the `Logger` trait and did not
adopt `tracing`: the release `bcrypt.dll` stayed at 346,624 B, the baseline
recorded in [architecture.md](../architecture.md).

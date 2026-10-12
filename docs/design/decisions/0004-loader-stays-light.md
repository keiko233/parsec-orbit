# 0004 - The loader stays light

## Context
The loader is mapped into every Parsec client start, was recently shrunk 520 -> 340 KB, and any crash in it crashes the client. Heavy crates (`tokio`, `ractor`, `serde`) would undo that.

## Decision
No async runtime, no `ractor`, no `serde` in `parsec-orbit-loader` or `parsec-orbit-core`. `tracing` is allowed in the loader only as `tracing-core` + a small file subscriber, and only if the DLL grows by < 30 KB; otherwise the existing `log` module is kept behind the `Logger` trait and the measurement is recorded here. Established crates are used freely in plugins and tools: `ractor` and `tracing` in the JS runtime and the webview bridge, `tracing` bridge in the Rust SDK.

## Consequences
Standardisation inside the loader is by traits (`Platform`, `Logger`), not by dependencies.

## Outcome
Phase 1 kept the existing `log` module behind the `Logger` trait and did not
adopt `tracing`: the release `bcrypt.dll` stayed at 346,624 B, the baseline
recorded in [architecture.md](../architecture.md).

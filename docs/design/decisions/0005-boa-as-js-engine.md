# 0005 - Boa as the JS engine

## Context
Candidates: Boa (pure Rust), QuickJS via `rquickjs` (needs a C toolchain), V8 via `deno_core` (tens of MB). The roadmap already named Boa.

## Decision
Boa, inside the JS runtime plugin. One `Context` per JS plugin, no file-system or network access unless a capability is declared in the manifest and granted by the runtime.

## Implementation notes (phase 4)
Boa 0.22 (needs Rust 1.91), default features off. ES modules, promises and `async`/`await` work; the plugin entry is a bundled ES module. Runtime limits (20 million loop iterations, 400 recursion levels) turn runaway scripts into errors. Boa has no interrupt for a script that runs inside a single native call.

## Consequences
* No C toolchain in CI; a multi-MB runtime DLL that only JS-plugin users download.
* Boa contexts are not `Send`: each lives in a dedicated actor/thread.
* ECMAScript coverage is Boa's; the TypeScript SDK compiles to a conservative target (see `spec/js-sdk.md`).

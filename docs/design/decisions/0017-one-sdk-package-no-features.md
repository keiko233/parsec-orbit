# 0017 - One SDK package without features; `sdk-std` dissolved

## Context
[ADR 0002](0002-sdk-is-no-std-by-default.md) put everything that needs `std` in a second crate, `parsec-orbit-plugin-sdk-std`, because a `std` feature cannot work: a `no_std` plugin's `#[panic_handler]` is a compile error as soon as `std` is anywhere in its dependency graph, and Cargo unifies features across one build. The restructure review asked whether a feature could still replace the crate.

It was tried again in a scratch workspace (an SDK copy with `std = []` doing `extern crate std`, one `freestanding!` plugin, one plugin enabling the feature, resolver 2): `cargo build -p nostd` builds, `cargo build --workspace` and `cargo build -p nostd -p stdp` fail with `E0152: found duplicate lang item panic_impl`. Dev-dependencies unify the same way under `cargo test --workspace`. No `cfg`, macro or resolver setting avoids it.

`sdk-std` held five things: a re-export and prelude of the SDK (13 importing files), `ContextExt::spawn` (5 calls), `ContextExt::install_panic_hook` (1 call), `TracingExt` (no user outside its own tests) and `testing::MockHost`.

## Decision
`parsec-orbit-plugin-sdk` is the only SDK package, with no features, for `std` and `no_std` plugins alike. A `std` plugin links `std` itself and imports the SDK directly.

* `ContextExt::spawn` and `install_panic_hook` are deleted; their call sites use `std::thread::Builder` and `std::panic::set_hook`.
* The `tracing` bridge is removed (no users). `tracing-core` turns `std` on through `once_cell` whenever something else in the build enables `tracing`'s `std`, which is the same trap. It comes back as its own crate if a plugin needs it.
* `MockHost` needs a `thread_local` and stays `std`, so it cannot join the `no_std` package: it becomes `parsec-orbit-plugin-sdk-testing`, a dev-dependency.

A crate is split only when the toolchain forces it or when two consumers must not depend on each other's half; see [restructure.md](../restructure.md#25-when-a-crate-is-split).

## Consequences
* One import path for every plugin; no `-std` flavour to choose.
* The workspace still builds and tests with `--workspace`; the guard against `std` in a `no_std` plugin stays the build itself (`hello-nostd`, `fixture-nostd`).
* `parsec-orbit-plugin-sdk-std` is not published again; the published name for the test double is `parsec-orbit-plugin-sdk-testing`.

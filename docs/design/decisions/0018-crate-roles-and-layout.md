# 0018 - Crate roles and directory layout

## Context
Ten crates shared the `parsec-orbit-plugin-` prefix but were two different things: five only provide a service or kind handler (config, hooks, js-runtime, webview-bridge, ui), three are features (the bandwidth patches and stun-swap). Loaders, shared libraries and executables sat in the same flat `crates/`. Nothing recorded the difference, so installer policy kept its own hand-written lists.

## Decision
Crates are grouped by directory and may only depend downwards:

| Directory | Role | Examples |
| --- | --- | --- |
| `crates/` | `sdk`, `host` (core, loaders), `shared` libraries | abi, sdk, sdk-testing, core, loader-windows, loader-macos, macos-state, market |
| `libs/` | `lib`: provides a service or kind handler, no feature of its own; still an ordinary ABI plugin at run time | `parsec-orbit-lib-{config,hooks,js-runtime,webview-bridge,ui}` |
| `plugins/` | `plugin`: a feature | `parsec-orbit-plugin-{high-bitrate-native,high-bitrate-webview,stun-swap}` |
| `tools/` | executables for the host machine | `parsec-orbit-verify` |

A module declares `role = "lib"` in `plugin.toml` (default `plugin`); the loader ignores it, installers and the market use it. The ABI, plugin ids and loading rules do not change; only package names and DLL file names of the five libs do (`parsec_orbit_lib_<name>`), which hides behind `entry`.

The dependency law is checked by `tests/parsec-orbit-e2e/tests/architecture.rs` from `cargo metadata`: the ABI crate depends on nothing, the SDK only on it, `core` and the loaders never on the author API, libs and plugins only on the SDK, the test double only as a dev-dependency, and a tool links at most one plugin `rlib`.

## Consequences
* The tree shows the role of a crate; installers can hide libs and resolve them as dependencies.
* Crate splits follow the criteria of [restructure.md](../restructure.md#25-when-a-crate-is-split).

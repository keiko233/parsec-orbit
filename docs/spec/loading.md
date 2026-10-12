# Loading rules

Status: draft (host revision 2). The v1 behaviour (scan, validate, `init`) is documented in [plugin-abi.md](plugin-abi.md#lifecycle).

## 1. Discovery

Plugin directories, in priority order: `PARSEC_ORBIT_PLUGIN_DIR` (`;`-separated), `<client dir>\plugins`, `%APPDATA%\ParsecOrbit\plugins`. A plugin is a subdirectory with `plugin.toml`, or a loose `*.dll` without a manifest (legacy layout, no dependencies). The first occurrence of an `id` wins; later duplicates are logged and ignored.

## 2. Selection

A candidate is dropped (logged, counted as *skipped*) when `enabled = false`, when its `api` is newer than the loader, or when its id duplicates an earlier one.

## 3. Ordering

1. Build a graph from `requires` and `optional` over ids and `provides` aliases.
2. Topologically sort. Ties keep discovery order (priority, then directory name), so load order is deterministic.
3. A cycle marks every plugin on it as *failed* with the cycle printed (`a -> b -> a`).
4. A `requires` entry that no candidate satisfies marks the plugin as *skipped*: `needs com.parsec-orbit.js-runtime, not installed`. `optional` entries that are absent are ignored.

## 4. Per-plugin load

For each plugin in order:

1. If any `requires` dependency was skipped or failed: skip, `dependency X did not load`.
2. `kind = dll`: map the library, run `Query` / validation / `Init` as in the ABI document, passing that plugin's own host table.
3. Other kinds: look up the interface `parsec-orbit.kind.<kind>`; if missing, skip with `no handler for kind "<kind>" (is the runtime plugin installed?)`; otherwise call its `load_plugin`.
4. A non-zero result is a failure. Interfaces the plugin registered stay registered (there is no unload), but the failed id is recorded so dependents are skipped.

Everything runs sequentially on the loader worker thread after the warm-up delay. A plugin that needs to do work later must spawn its own thread.

## 5. Summary

The loader logs `ready: N loaded, M skipped, K failed` and, per decision, one line naming the plugin and the reason. The e2e suite asserts these lines.

## Open questions

* Per-plugin `init` timeout/watchdog (currently none; a hung `init` blocks later plugins).

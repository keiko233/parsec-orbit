# 0003 - The JS runtime is a plugin

## Context
JS plugins need an engine (Boa), an async story, and WebView access. None of that belongs in the `bcrypt.dll` proxy, and most users will never install a JS plugin.

## Decision
The runtime is an ordinary plugin (`com.parsec-orbit.js-runtime`) that registers the kind handler `parsec-orbit.kind.js` and the service `parsec-orbit.js`. JS plugins list it in `requires`. The loader knows about *kinds* and *dependencies*, not about JavaScript: for a plugin whose `kind` is not `dll` it asks the registry for `parsec-orbit.kind.<kind>` and skips the plugin with an actionable message when absent.

## Implementation notes (phase 4)
One Boa context per JS plugin on a thread of its own with a command inbox; no `ractor`/`tokio` (see the amendment in [ADR 0004](0004-loader-stays-light.md)). The runtime DLL is 4.2 MB in release; Boa runs without its `temporal`/`intl` features.

## Consequences
* Default install does not support JS plugins, by construction.
* The same mechanism admits other kinds (for example `wasm`) later.
* Load order matters: dependency resolution is a loader feature, specified in `spec/loading.md`.

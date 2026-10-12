# Native UI Patches

Official [Parsec Orbit](../../README.md) plugin. It widens the "Bandwidth
Limit" choices in the native (libmatoya) interface in memory, without modifying
anything on disk.

The client configuration system is far more permissive than its user
interfaces. `encoder_bitrate` ("Bandwidth Limit"), for example, is an `int`
with `min 0` / `max 2147483647`, but the native UI only offers the built-in
ladder `3, 5, 7, 10, 15, 20, 25, 30, 35, 40, 45, 50 Mbps`. The plugin widens
it to `60, 75, 100, 125, 150, 200, 250, 300, 400, 500 Mbps`.

The WebView2 UI is patched by the separate
[`parsec-orbit-plugin-high-bitrate-webview`](../parsec-orbit-plugin-high-bitrate-webview/README.md)
plugin.

## Files

* `plugin.toml` - manifest the loader reads (`id`, `enabled`, `entry`, ...).
* `src/lib.rs` - plugin entry point and `EXTRA_BANDWIDTH_MBPS`.
* `src/legacy.rs` - the in-memory table patch.

## Install

Copy this folder (or extract `parsec-orbit-plugin-high-bitrate-native.zip`) into the
client's plugin directory, so that the loader finds
`plugins\parsec-orbit-plugin-high-bitrate-native\plugin.toml` next to `parsecd.exe`.

## Disable

* `enabled = false` in `plugin.toml`, or
* delete the plugin folder, or
* set `PARSEC_ORBIT_PATCHES=0` (or `off`/`false`) in the client environment.

## How the patch works

The old interface is built with libmatoya inside `parsecd-<version>.dll`. Its
dropdowns come from static option tables: arrays of `0x144`-byte records with a
NUL-terminated label at offset `0` and the numeric value as an `int32` at
offset `0x40`. The bandwidth table contains the twelve built-in entries and is
referenced from exactly two code sites (one builds the settings row, one finds
the selected entry); both carry the entry count `12` as a separate immediate.

The patch:

1. finds the table by signature (the twelve labels and values), so it does not
   depend on file offsets of a specific build,
2. allocates an extended copy of the table near the module (the rewritten
   `lea` displacements are RIP-relative and need to stay in reach),
3. rewrites the two `lea` instructions to point at the copy, and
4. patches the two count immediates from `12` to the new length.

Every edited location is read back before the result is reported; on unknown
builds the signature does not match and the patch is skipped with a log line.
The plugin waits up to ten seconds for the client module to appear, so it runs
on its own thread.

The loader log (`%APPDATA%\ParsecOrbit\parsec-orbit.log`) reports the result:

```
loaded com.parsec-orbit.native 0.1.0 (Native UI Patches) ABI 1 by Parsec Orbit contributors [MIT]
native ui patches 0.1.0 starting
legacy ui patch: bandwidth table at +0x337B70
legacy ui patch: 12 -> 22 options, 2 references and 2 count immediates patched
```

## Keeping the two lists in sync

The extra options live in two places and must stay in sync:

* `src/lib.rs` - `EXTRA_BANDWIDTH_MBPS`
* `../parsec-orbit-plugin-high-bitrate-webview/assets/bandwidth.js` - `EXTRA_MBPS`

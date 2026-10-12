# Interface patches

The client configuration system is far more permissive than its user
interfaces. `encoder_bitrate` ("Bandwidth Limit"), for example, is an `int`
with `min 0` / `max 2147483647`, but both interfaces only offer the built-in
ladder `3, 5, 7, 10, 15, 20, 25, 30, 35, 40, 45, 50 Mbps`.

The patches package (`patches/`) widens that ladder to
`60, 75, 100, 125, 150, 200, 250, 300, 400, 500 Mbps` in both interfaces at
runtime. Nothing on disk is modified; the loader applies the patches in memory
when the client starts. `PARSEC_ORBIT_PATCHES=0` (or `off`/`false`) disables
them.

## WebView UI

The new interface is a WebView2 page loaded from
`https://builds.parsec.app/webview/release-ui/index.html`. The option list is a
hard-coded array in `index.js`, and the page saves a selection through the
`cfg_seti` RPC (`encoder_bitrate`), which the native side accepts as-is.

`scripts/webview-bitrate-extend.js` finds the array through the React fiber
tree, appends the extra entries and replays the app's own
`parsec_config_update` event so an already-rendered settings page shows them.
The script is idempotent and can be pasted into the DevTools console.

The loader delivers the same script over the DevTools protocol: the `bcrypt.dll`
proxy already opens `http://127.0.0.1:9222/json` (unless
`PARSEC_ORBIT_DEVTOOLS=0`), and a worker thread then

1. polls `/json` until the page target exists,
2. registers the script with `Page.addScriptToEvaluateOnNewDocument` so it
   survives reloads,
3. runs it once with `Runtime.evaluate` for the loaded document, and
4. keeps the WebSocket open, answering pings, until the page goes away.

`cargo run --release -p parsec-orbit-verify-webview-inject -- 9222` performs the
same steps against a running client without restarting it.

## Native (legacy) UI

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

Both patches report into the loader log
(`%APPDATA%\ParsecOrbit\parsec-orbit.log`), for example:

```
webview patch registered for future documents (https://builds.parsec.app/webview/release-ui/index.html)
webview bandwidth list extended: installed
legacy ui patch: bandwidth table at +0x337B70
legacy ui patch: 12 -> 22 options, 2 references and 2 count immediates patched
```

## Keeping the two lists in sync

The extra options live in two places and must stay in sync:

* `patches/src/lib.rs` - `EXTRA_BANDWIDTH_MBPS`
* `scripts/webview-bitrate-extend.js` - `EXTRA_MBPS`

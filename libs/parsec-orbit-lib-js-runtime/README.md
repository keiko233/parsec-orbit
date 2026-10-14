# JavaScript Runtime

Official [Parsec Orbit](../../README.md) plugin. It runs plugins whose
`plugin.toml` says `kind = "js"` on the [Boa](https://boajs.dev) engine and
provides `parsec-orbit.js` to native plugins. It is not part of the default
install: without it the loader skips JavaScript plugins with
`needs com.parsec-orbit.js-runtime, not installed`.

* Each JavaScript plugin gets a context of its own on a thread of its own. The
  entry is an ES module; its default export's `setup(host)` is awaited before
  the plugin counts as loaded.
* `host` has `log`, `plugin`, timers, and - with the matching `permissions`
  entry - `webview` (over the webview bridge) and `config`. There is no file,
  network or process access.
* `parsec-orbit.js` gives native plugins named contexts: `create_context`,
  `eval` (result as JSON, promises awaited), `destroy_context`.
* An error in a callback or an unhandled rejection is logged under the
  plugin id and never reaches the client; runaway loops and recursion end with
  an error. An engine bug would still be a bug in the client process.

Write plugins with [`@parsec-orbit/sdk`](../../javascript-sdk/README.md); the guide is
[Write a TypeScript plugin](../../docs/guides/typescript-plugin.md), the
specification [spec/js-sdk.md](../../docs/spec/js-sdk.md). The DLL is about
4 MB, which is why it is a separate download.

Files: `src/lib.rs` (registration), `src/engine.rs` (the context thread),
`src/host.rs` (what JavaScript sees), `src/service.rs` (the two interfaces),
`src/loader.rs` (bookkeeping). Building needs Rust 1.91 (Boa).

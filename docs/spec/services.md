# Host services

Status: the registry, interface headers and kind handlers are implemented
(host revision 2). The core service tables below are fixed in shape except for
`parsec-orbit.js`, whose signatures stay reserved until its runtime provider is
implemented. Implements [ADR 0001](../design/decisions/0001-typed-service-vtables.md).

A *service* (interface) is a `repr(C)` function table that a plugin, or the loader, registers under a name and other plugins look up. This is the one mechanism for everything a plugin offers to other plugins: WebView operations, the JS runtime, kind handlers, hooks.

## Interface header

The C definition of `ParsecOrbitInterfaceHeader` (and of `ParsecOrbitPluginLoadInfo` and `ParsecOrbitKindHandlerV1`, used below) is in [plugin-abi.md](plugin-abi.md#interfaces); the Rust definitions are in `parsec-orbit-plugin-abi`.

Every interface table starts with this header. Compatibility rules:

* A **minor** bump only appends functions. A consumer must check `size >= offsetof(function) + sizeof(pointer)` before calling a function newer than the minimum it was written for, and treat a null pointer as "not supported".
* A **major** bump is a different contract; consumers compare `version >> 16` for equality and ignore the interface otherwise.
* Tables are immutable after registration and must stay valid for the lifetime of the process (there is no unload).
* Functions use `extern "system"`, take UTF-8 as `(ptr, len)` unless documented as NUL-terminated, never unwind across the boundary, and are thread-safe unless the interface says otherwise.

## Registry

Reached through the per-plugin host table ([plugin-abi.md](plugin-abi.md#host-revision-2)):

| Host function | Behaviour |
| --- | --- |
| `register_interface(host, name, table)` | `host` must be the caller's own loader-issued table (it names the owner). `name` is NUL-terminated ASCII, 1 to 128 of `[A-Za-z0-9._-]` with no empty dot part. Returns `0`, `-1` invalid argument (bad name, unknown `host`, null or pointer-misaligned table, header smaller than 8 bytes, or a reserved `parsec-orbit.*` name from a plugin whose id does not start with `com.parsec-orbit.`), `-2` name already registered. |
| `query_interface(name)` | Returns the registered `ParsecOrbitInterfaceHeader *` or null. Revision 1 always returned null; revision 2 resolves names through the registry. |
| `unregister_interface(host, name)` | Only the registering plugin may remove it; returns `0` or `-3` not found (also for somebody else's name). Reserved for the future unload. |

Lookups are valid from `init` onward. Because `requires` guarantees that a dependency finished `init` first, a plugin that declares `requires = ["com.parsec-orbit.webview-bridge"]` can query `parsec-orbit.webview` in its own `init`.

The names `parsec-orbit.*` are reserved for official services, that is for
plugins whose host table carries an id under `com.parsec-orbit.`; the loader
enforces it for cooperative plugins. The one exception is
`parsec-orbit.kind.<kind>`, open to whoever implements a kind. Third parties
use `<reverse.dns>.<name>`. DLL plugins are native code in the client process,
so they can bypass any in-process check; the prefix is a naming convention, not
a sandbox boundary.

## Kind handlers

Name: `parsec-orbit.kind.<kind>` (for example `parsec-orbit.kind.js`). Registered by the plugin that implements the kind. The table is `ParsecOrbitKindHandlerV1`: the header (version 1.0; the loader serves any 1.x), then `load_plugin(info)` and an optional `unload_plugin(id)`.

`load_plugin` is called on the loader worker thread, after the handler plugin and all of the target's other dependencies are initialised. The handler logs its own failure reasons; a non-zero return marks the plugin *failed*.

## Core services

The C layouts are in [plugin-abi.md](plugin-abi.md#core-service-tables) and in `parsec-orbit-plugin-abi`; the typed Rust bindings are `services::{WebView, WebViewClient, Js, JsClient}` in the SDK. This section is what the functions mean.

### `parsec-orbit.webview` (provided by the webview-bridge plugin), version 1.0

Wraps the WebView2 UI of the client through the DevTools endpoint the loader opens. All strings are UTF-8 `(ptr, len)`. Every function is thread-safe; callbacks run on the bridge's thread.

| Function | Behaviour |
| --- | --- |
| `is_ready()` | `1` when a page target is attached, `0` otherwise. |
| `add_script(key, script)` | Registers `script` to run in every new document (it survives reloads) and also in the current one. Returns a script id `> 0` or a negative error. **Idempotent per `key`**: registering a key again replaces the script. Keys are global, so a client prefixes its plugin id (`WebViewClient` does: `<plugin id>/<key>`); an earlier draft promised idempotence per (plugin id, key) but the functions carry no caller identity. Scripts registered before a page is attached are kept and run when it attaches. |
| `remove_script(id)` | Stops the script from running in new documents. `0` or `-3`. |
| `evaluate(expression, out, user)` | Evaluates in the current document (`awaitPromise`, `returnByValue`) and passes the JSON result to `out`, valid only during the call. `0`, `-4` not ready, `-5` timeout (10 s), `-6` the script threw. |
| `subscribe(channel, cb, user)` | Calls `cb` with the JSON payload for each `window.__orbit.emit(channel, payload)` of the page. Returns a subscription id `> 0` or an error. Subscriptions survive page reloads. |
| `unsubscribe(id)` | `0` or `-3`. |
| `post(channel, payload)` | Delivers the JSON `payload` to `window.__orbit.on(channel, handler)` handlers. `-4` when no page is attached. |

**Page side.** The bridge registers a helper script before any plugin script. It defines `window.__orbit`:

* `emit(channel, payload)` - to the plugin side (`subscribe`),
* `on(channel, handler)` / `off(channel, handler)` - from the plugin side (`post`),
* `rpc(event, data)` - the Parsec UI/client request/reply protocol of the web UI (`protocol_version: 1` envelope on `window.MTY_NativeSendText`, reply through the same channel, 10 s timeout),
* `version` - the helper's version.

The page talks to the bridge through a CDP binding (`Runtime.addBinding` named `__orbitSend`), so no network port beyond the DevTools endpoint is involved.

### `parsec-orbit.js` (provided by the JS runtime plugin), version 1.0

For native plugins that want to evaluate script. `create_context(name)` makes a context of its own (`-2` if the name is taken), `eval(context, source, out, user)` evaluates a script and passes the result as JSON, `destroy_context(context)` drops it. Contexts are independent of those of JavaScript plugins, run on a thread of their own and have no `host` object; they have the timers and nothing else.

### Reserved

`parsec-orbit.hooks` (function hook registration), `parsec-orbit.config` (per-plugin settings storage), `parsec-orbit.market` (install source and update metadata).

## Error codes

`0` success; `-1` invalid argument; `-2` already exists; `-3` not found; `-4` not ready; `-5` timeout; `-6` script/remote exception; `<= -1000` interface specific.

## Open questions

* Whether `evaluate` should be asynchronous (completion callback) instead of blocking up to a timeout. Blocking is simpler for native callers; the JS runtime wraps it in a promise on its own threads.
* Subscription callbacks run on the bridge's thread; whether the SDK should marshal them onto an executor the plugin chooses.

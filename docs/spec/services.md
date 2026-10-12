# Host services

Status: the registry, interface headers and kind handlers are implemented (host revision 2); the core service tables below are fixed in shape but have no provider yet. Implements [ADR 0001](../design/decisions/0001-typed-service-vtables.md).

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
| `register_interface(host, name, table)` | `host` is the caller's own table (it names the owner). `name` is NUL-terminated ASCII, 1 to 128 of `[A-Za-z0-9._-]` with no empty dot part. Returns `0`, `-1` invalid argument (bad name, null or misaligned table, header smaller than 8 bytes, or a `parsec-orbit.*` name from a plugin whose id does not start with `com.parsec-orbit.`), `-2` name already registered. |
| `query_interface(name)` | Returns the registered `ParsecOrbitInterfaceHeader *` or null. (Same function as in revision 1, which always returned null.) |
| `unregister_interface(host, name)` | Only the registering plugin may remove it; returns `0` or `-3` not found (also for somebody else's name). Reserved for the future unload. |

Lookups are valid from `init` onward. Because `requires` guarantees that a dependency finished `init` first, a plugin that declares `requires = ["com.parsec-orbit.webview-bridge"]` can query `parsec-orbit.webview` in its own `init`.

The names `parsec-orbit.*` are reserved for official services, that is for plugins whose id is under `com.parsec-orbit.`; the loader enforces it. The one exception is `parsec-orbit.kind.<kind>`, open to whoever implements a kind. Third parties use `<reverse.dns>.<name>`.

## Kind handlers

Name: `parsec-orbit.kind.<kind>` (for example `parsec-orbit.kind.js`). Registered by the plugin that implements the kind. The table is `ParsecOrbitKindHandlerV1`: the header (version 1.0; the loader serves any 1.x), then `load_plugin(info)` and an optional `unload_plugin(id)`.

`load_plugin` is called on the loader worker thread, after the handler plugin and all of the target's other dependencies are initialised. The handler logs its own failure reasons; a non-zero return marks the plugin *failed*.

## Core services

The names and shapes are fixed here so SDKs can be written ahead of the providers.

### `parsec-orbit.webview` (provided by the webview-bridge plugin), version 1.0

Wraps the WebView2 UI of the client (the DevTools endpoint the loader opens). All strings are UTF-8 `(ptr, len)`.

```c
typedef void (*ParsecOrbitMessageCallback)(void *user, const uint8_t *msg, size_t len);

typedef struct ParsecOrbitWebViewV1 {
    ParsecOrbitInterfaceHeader header;
    /* 1 when a page target is attached, 0 otherwise */
    int32_t (*is_ready)(void);
    /* Registers `script` to run in every new document (survives reloads) and also in the current
       one. Returns a script id > 0, or a negative error. Idempotent per (plugin id, key). */
    int64_t (*add_script)(const uint8_t *key, size_t key_len, const uint8_t *script, size_t script_len);
    int32_t (*remove_script)(int64_t script_id);
    /* Evaluates `expression` in the current document. The JSON result is passed to `out`
       (valid only during the call). Returns 0 or a negative error
       (-4 not ready, -5 timeout, -6 script exception). */
    int32_t (*evaluate)(const uint8_t *expression, size_t len, ParsecOrbitMessageCallback out, void *user);
    /* Subscribes to messages the page sends with `window.__orbit.emit(channel, payload)`. */
    int64_t (*subscribe)(const uint8_t *channel, size_t channel_len, ParsecOrbitMessageCallback cb, void *user);
    int32_t (*unsubscribe)(int64_t subscription);
    /* Sends a message to the page, delivered to `window.__orbit.on(channel, handler)`. */
    int32_t (*post)(const uint8_t *channel, size_t channel_len, const uint8_t *payload, size_t len);
} ParsecOrbitWebViewV1;
```

The page-side helper `window.__orbit` (`emit`, `on`, and `rpc(event, data)` for the Parsec UI/client RPC protocol) is installed by the bridge before any plugin script.

### `parsec-orbit.js` (provided by the JS runtime plugin), version 1.0

For native plugins that want to evaluate script: create/destroy a named context and `eval(context, source) -> json`. The signatures are fixed when the runtime is implemented; the name is reserved.

### Reserved

`parsec-orbit.hooks` (function hook registration), `parsec-orbit.config` (per-plugin settings storage), `parsec-orbit.market` (install source and update metadata).

## Error codes

`0` success; `-1` invalid argument; `-2` already exists; `-3` not found; `-4` not ready; `-5` timeout; `-6` script/remote exception; `<= -1000` interface specific.

## Open questions

* Whether `evaluate` should be asynchronous (completion callback) instead of blocking up to a timeout. Blocking is simpler for native callers; the JS runtime can wrap it in a promise.
* Subscription callbacks run on the bridge's thread; whether the SDK should marshal them onto an executor the plugin chooses.

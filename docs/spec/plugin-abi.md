# Parsec Orbit plugin ABI reference

This document is the normative description of the ABI implemented by
`parsec-orbit-plugin-abi` (re-exported by `parsec-orbit-plugin-sdk`, see
[rust-sdk.md](rust-sdk.md)); the two must stay in sync, and a test in the ABI
crate fails when they drift. Change the ABI, change this document. This ABI uses
the `ParsecOrbit*` export and type names and the `parsec_orbit_plugin!` SDK
macro.

Status: implemented, ABI 1 with host revision 2. The ABI is the same on macOS (draft, [macos-platform.md](macos-platform.md)); where the text says DLL, `LoadLibraryW` or `%APPDATA%`, the macOS counterpart is a dylib, `dlopen` and the Orbit root.

## Lifecycle

```text
client starts
  -> loader is loaded (bcrypt.dll proxy; on macOS: loader dylib injected into the host)
       -> plugin directories are scanned, manifests read, load order decided
            -> LoadLibraryW(plugin)        (macOS: dlopen)
                 -> ParsecOrbitPluginQuery(loader ABI version) -> plugin descriptor
                 -> abi_version / struct_size are validated
                 -> ParsecOrbitPluginInit(&ParsecOrbitHost) -> 0 means success
                 -> ParsecOrbitPluginShutdown()   (optional; only called by a future unload/reload)
```

Plugins without `ParsecOrbitPluginQuery` fall back to
`ParsecPluginInit(void*) -> u32` (the early PoC layout).

## Exports

| Export | Required | Signature |
| --- | --- | --- |
| `ParsecOrbitPluginQuery` | yes | `const ParsecOrbitPluginDesc *ParsecOrbitPluginQuery(uint32_t loader_abi)` |
| `ParsecOrbitPluginInit` | yes | `int32_t ParsecOrbitPluginInit(const ParsecOrbitHost *host)`, 0 means success |
| `ParsecOrbitPluginShutdown` | no | `int32_t ParsecOrbitPluginShutdown(void)` |
| `ParsecPluginInit` | compatibility | `uint32_t ParsecPluginInit(void *)` |

## Structs

```c
#define PARSEC_ORBIT_ABI_VERSION 1

typedef struct ParsecOrbitPluginDesc {
    uint32_t    abi_version;   /* ABI version the plugin was built against */
    uint32_t    struct_size;   /* size of this struct in that version */
    const char *id;            /* unique ID, reverse-DNS style, UTF-8, NUL terminated */
    const char *name;          /* display name */
    const char *version;       /* plugin version (semver) */
    const char *author;
    const char *description;
    const char *license;       /* SPDX identifier */
    uint64_t    capabilities;  /* reserved capability bits; the loader only records them */
} ParsecOrbitPluginDesc;
```

### Host table

```c
typedef struct ParsecOrbitHost {
    /* ---- revision 1 (96 bytes on x64) ---- */
    uint32_t abi_version;      /* ABI version implemented by the loader */
    uint32_t struct_size;      /* struct size on the loader side */
    void (*log)(uint32_t level, const uint8_t *message, size_t len);
    void *(*query_interface)(const char *name);   /* ParsecOrbitInterfaceHeader * or NULL */
    const char *loader_version; /* loader version, UTF-8, NUL terminated */
    uintptr_t reserved[8];      /* stays zero */

    /* ---- revision 2 (208 bytes on x64) ---- */
    const char *plugin_id;        /* manifest id (descriptor id for manifest-less plugins) */
    const char *plugin_dir;       /* absolute, UTF-8 */
    const char *data_dir;         /* <data root>\<id>, data root = %APPDATA%\ParsecOrbit\data (macOS: <Orbit root>/data); may not exist yet; NULL when unknown */
    void (*log_ex)(uint32_t level, const uint8_t *target, size_t target_len,
                   const uint8_t *message, size_t len);
    int32_t (*register_interface)(const ParsecOrbitHost *host, const char *name,
                                  const ParsecOrbitInterfaceHeader *table);
    int32_t (*unregister_interface)(const ParsecOrbitHost *host, const char *name);
    uintptr_t reserved2[8];     /* stays zero */
} ParsecOrbitHost;
```

Conventions:

- All strings are **UTF-8, NUL terminated** and must live for the whole module
  lifetime (the macro generates static literals with `concat!(..., "\0")`).
  `log` and `log_ex` take `(pointer, length)` pairs that do **not** need to be
  NUL terminated and are only guaranteed to be valid for the duration of the
  call.
- Log levels: `0=trace 1=debug 2=info 3=warn 4=error`.
- Structs only grow at the tail; nothing before `reserved` is changed, removed,
  or reordered.
- Callbacks use `extern "system"` (the platform C ABI on macOS), never unwind across the boundary and may be
  called from any thread unless an interface says otherwise.

## Host revision 2

Revision 2 is an *additive* change within ABI 1: the fields after `reserved`
were appended, so old plugins keep working (they read the first 96 bytes) and
new plugins detect support from `struct_size`. `ABI_VERSION` stays `1`.

* A plugin may use a revision 2 field only if `host->struct_size >= 208` on
  x64 (`108` on x86; the SDK exports this as `HOST_REVISION_2_SIZE`) and the
  value is non-null. A plugin that talks to a revision 1 loader still
  initialises: `log` works, the registry is simply absent.
* The loader passes **each plugin its own `ParsecOrbitHost`** (allocated once,
  valid for the process lifetime); revision 1 passed one shared table.
* `log_ex` writes `target` (usually the plugin id) alongside the message into
  the loader log; `log` keeps working and logs without a target.
* `register_interface` and `unregister_interface` take the caller's own host
  table as their first argument: the callbacks are shared functions, so the
  loader reads `plugin_id` out of that table to learn who is calling. (The
  early draft had `(name, table)`; the extra argument is what makes ownership
  checkable without a trampoline per plugin.)
* The descriptor is unchanged in revision 2: dependencies and ordering come
  from the manifest ([loading.md](loading.md)).

### Interfaces

Everything a plugin offers to other plugins is an *interface*: a `repr(C)`
function table that starts with a header. Semantics, the registry and the core
services are in [services.md](services.md).

```c
typedef struct ParsecOrbitInterfaceHeader {
    uint32_t size;      /* sizeof the whole table, header included */
    uint32_t version;   /* (major << 16) | minor */
} ParsecOrbitInterfaceHeader;

typedef struct ParsecOrbitPluginLoadInfo {
    uint32_t    size;          /* sizeof(ParsecOrbitPluginLoadInfo) */
    const char *id;            /* manifest id */
    const char *dir;           /* plugin directory, absolute */
    const char *entry;         /* entry file, absolute */
    const char *data_dir;      /* %APPDATA%\ParsecOrbit\data\<id>, NULL if unknown */
    const uint8_t *manifest;   /* raw plugin.toml text, UTF-8 */
    size_t      manifest_len;
} ParsecOrbitPluginLoadInfo;

/* parsec-orbit.kind.<kind>, version 1.0 */
typedef struct ParsecOrbitKindHandlerV1 {
    ParsecOrbitInterfaceHeader header;
    int32_t (*load_plugin)(const ParsecOrbitPluginLoadInfo *info);   /* 0 = success */
    int32_t (*unload_plugin)(const char *id);                        /* may be NULL */
} ParsecOrbitKindHandlerV1;
```

### Core service tables

The behaviour of these is in [services.md](services.md); the layouts are frozen here and in `parsec-orbit-plugin-abi`.

```c
typedef void (*ParsecOrbitMessageCallback)(void *user, const uint8_t *msg, size_t len);

/* parsec-orbit.webview, version 1.0 */
typedef struct ParsecOrbitWebViewV1 {
    ParsecOrbitInterfaceHeader header;
    int32_t (*is_ready)(void);
    int64_t (*add_script)(const uint8_t *key, size_t key_len, const uint8_t *script, size_t script_len);
    int32_t (*remove_script)(int64_t script_id);
    int32_t (*evaluate)(const uint8_t *expression, size_t len, ParsecOrbitMessageCallback out, void *user);
    int64_t (*subscribe)(const uint8_t *channel, size_t channel_len, ParsecOrbitMessageCallback cb, void *user);
    int32_t (*unsubscribe)(int64_t subscription);
    int32_t (*post)(const uint8_t *channel, size_t channel_len, const uint8_t *payload, size_t len);
} ParsecOrbitWebViewV1;

/* parsec-orbit.js, version 1.0 */
typedef struct ParsecOrbitJsV1 {
    ParsecOrbitInterfaceHeader header;
    int64_t (*create_context)(const uint8_t *name, size_t name_len);
    int32_t (*eval)(int64_t context, const uint8_t *source, size_t len, ParsecOrbitMessageCallback out, void *user);
    int32_t (*destroy_context)(int64_t context);
} ParsecOrbitJsV1;

/* parsec-orbit.config, version 1.0 */
typedef struct ParsecOrbitConfigV1 {
    ParsecOrbitInterfaceHeader header;
    int32_t (*get)(const uint8_t *owner, size_t owner_len, const uint8_t *key, size_t key_len,
                   ParsecOrbitMessageCallback out, void *user);
    int32_t (*set)(const uint8_t *owner, size_t owner_len, const uint8_t *key, size_t key_len,
                   const uint8_t *json, size_t json_len);
    int32_t (*remove)(const uint8_t *owner, size_t owner_len, const uint8_t *key, size_t key_len);
    int32_t (*keys)(const uint8_t *owner, size_t owner_len, ParsecOrbitMessageCallback out, void *user);
} ParsecOrbitConfigV1;

/* parsec-orbit.hooks, version 1.0 */
typedef struct ParsecOrbitHooksV1 {
    ParsecOrbitInterfaceHeader header;
    const void *(*resolve)(const uint8_t *module, size_t module_len, const uint8_t *symbol, size_t symbol_len);
    int64_t (*install)(const void *target, const void *detour, const void **original);
    int32_t (*enable)(int64_t hook);
    int32_t (*disable)(int64_t hook);
    int32_t (*remove)(int64_t hook);
} ParsecOrbitHooksV1;

/* parsec-orbit.ui, version 2.0 */
typedef struct ParsecOrbitUiV2 {
    ParsecOrbitInterfaceHeader header;
    int32_t (*show)(void);
    int32_t (*hide)(void);
    int32_t (*is_visible)(void);
    int32_t (*navigate)(const uint8_t *page_id, size_t id_len);
    int32_t (*register_page)(const uint8_t *page_json, size_t page_len);
    int32_t (*unregister_page)(const uint8_t *page_id, size_t id_len);
    int32_t (*update_page)(const uint8_t *page_id, size_t id_len,
                           const uint8_t *patch_json, size_t patch_len);
    int64_t (*on_action)(const uint8_t *page_id, size_t id_len,
                         ParsecOrbitMessageCallback cb, void *user);
    int32_t (*off_action)(int64_t subscription);
} ParsecOrbitUiV2;
```

Error codes returned by the host and by interfaces:

| Code | Name | Meaning |
| --- | --- | --- |
| `0` | `OK` | success |
| `-1` | `ERR_INVALID` | invalid argument |
| `-2` | `ERR_EXISTS` | already exists |
| `-3` | `ERR_NOT_FOUND` | not found |
| `-4` | `ERR_NOT_READY` | not ready, or not supported by this loader |
| `-5` | `ERR_TIMEOUT` | timed out |
| `-6` | `ERR_EXCEPTION` | script or remote exception |

`<= -1000` is interface specific.

### Interface names

| Name | Service |
| --- | --- |
| `parsec-orbit.hooks` | Function hook registration/unregistration (reserved) |
| `parsec-orbit.webview` | WebView bridge (inject scripts, evaluate, messages); WebView2 on Windows, WKWebView on macOS |
| `parsec-orbit.js` | JS runtime handle, for DLL plugins that need eval |
| `parsec-orbit.ui` | Declarative UI service (winio companion window) |
| `parsec-orbit.kind.<kind>` | Handler that loads plugins of a kind that is not a native kind (`dll`, `dylib`), for example `kind.js` |
| `parsec-orbit.client` | macOS only (draft): which client build is running ([macos-platform.md](macos-platform.md#parsec-orbitclient)) |
| `parsec-orbit.config`, `parsec-orbit.market` | Settings storage, marketplace metadata (reserved) |

`parsec-orbit.*` is reserved for official services; the one exception is
`parsec-orbit.kind.<kind>`, which is open to any plugin that implements that
kind. Third parties use `<reverse.dns>.<name>`.

## Versioning policy

- A plugin states the `abi_version` it needs in its descriptor; the loader only
  rejects requests **newer** than itself.
- The loader also passes its own ABI version to `ParsecOrbitPluginQuery`, so a
  plugin can branch on it for compatibility.
- Adding fields (inside `reserved` or at the tail) is not a breaking change and
  keeps the same `ABI_VERSION`.
- Only a change that would make old plugins misbehave bumps `ABI_VERSION`, and
  the migration path is recorded in [the roadmap](../design/roadmap.md).

## C example

```c
#include <windows.h>

static const char ID[] = "com.example.c";
static const ParsecOrbitPluginDesc DESC = {
    .abi_version = 1,
    .struct_size = sizeof(ParsecOrbitPluginDesc),
    .id = ID, .name = "C Example", .version = "0.1.0",
    .author = "someone", .description = "written in C", .license = "MIT",
    .capabilities = 0,
};

__declspec(dllexport) const ParsecOrbitPluginDesc *ParsecOrbitPluginQuery(uint32_t abi) {
    (void)abi;
    return &DESC;
}

__declspec(dllexport) int32_t ParsecOrbitPluginInit(const ParsecOrbitHost *host) {
    const char message[] = "hello from C";
    host->log(2, (const uint8_t *)message, sizeof(message) - 1);
    return 0;
}
```

Rust plugins generate these three exports with the `parsec_orbit_plugin!` macro; see
`examples/parsec-orbit-plugin-hello`.

## Platforms

| | Windows | macOS (draft) |
| --- | --- | --- |
| Library | PE `.dll`, kind `dll` | Mach-O `.dylib`, kind `dylib`, `arm64`, at least ad-hoc signed |
| Mapping | `LoadLibraryW`, then `LoadLibraryExW` fallback | `dlopen(RTLD_NOW|RTLD_LOCAL)` |
| Exports | undecorated names from the export table | the same C names (`dlsym` without the leading underscore) |
| Loader | `bcrypt.dll` proxy in the client directory | dylib injected into a host process ([macos-host.md](macos-host.md)) |
| `HOST_REVISION_*_SIZE` | 96 / 208 on x64 | 96 / 208 (64-bit) |

The structs, constants, error codes and the versioning policy are identical.

## Security notes

DLL plugins are native code and run in the client process with the same
privileges: they can read and write all of the client's memory. Only install
plugins you trust; the loader does no sandboxing or signature checking
(marketplace signature verification is on the roadmap).

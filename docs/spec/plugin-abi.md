# Parsec Orbit plugin ABI reference

This document is the normative description of the ABI implemented by
`parsec-orbit-plugin-abi` (re-exported by `parsec-orbit-plugin-sdk`, see
[rust-sdk.md](rust-sdk.md)); the two must stay in sync, and a test in the ABI
crate fails when they drift. Change the ABI, change this document. This ABI uses
the `ParsecOrbit*` export and type names and the `parsec_orbit_plugin!` SDK
macro.

Status: implemented, ABI 1 with host revision 2.

## Lifecycle

```text
client starts
  -> loader is loaded (bcrypt.dll proxy)
       -> plugin directories are scanned, manifests read, load order decided
            -> LoadLibraryW(plugin)
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
    const char *data_dir;         /* %APPDATA%\ParsecOrbit\data\<id>; may not exist yet; NULL without %APPDATA% */
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
- Callbacks use `extern "system"`, never unwind across the boundary and may be
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
| `parsec-orbit.webview` | WebView2 bridge (inject scripts, evaluate, messages) |
| `parsec-orbit.js` | JS runtime handle, for DLL plugins that need eval |
| `parsec-orbit.kind.<kind>` | Handler that loads plugins of a non-`dll` kind (`kind.js`) |
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

## Security notes

DLL plugins are native code and run in the client process with the same
privileges: they can read and write all of the client's memory. Only install
plugins you trust; the loader does no sandboxing or signature checking
(marketplace signature verification is on the roadmap).

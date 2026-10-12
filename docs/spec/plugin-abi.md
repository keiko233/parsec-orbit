# Parsec Orbit plugin ABI reference

This document is the normative description of the ABI implemented by
`parsec-orbit-plugin-abi` (re-exported by `parsec-orbit-plugin-sdk`, see
[rust-sdk.md](rust-sdk.md)); the two must stay in sync, and a test in the ABI
crate fails when they drift. Change the ABI, change this document. This ABI uses
the `ParsecOrbit*` export and type names and the `parsec_orbit_plugin!` SDK
macro.

Status: everything up to "Reserved extension points" is implemented (ABI 1).
[Host revision 2](#host-revision-2-draft) is specified here but not yet
implemented.

## Lifecycle

```text
client starts
  -> loader is loaded (bcrypt.dll proxy)
       -> plugin directories are scanned
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

typedef struct ParsecOrbitHost {
    uint32_t abi_version;      /* ABI version implemented by the loader */
    uint32_t struct_size;      /* struct size on the loader side */
    void (*log)(uint32_t level, const uint8_t *message, size_t len);
    void *(*query_interface)(const char *name);
    const char *loader_version; /* loader version, UTF-8, NUL terminated */
    uintptr_t reserved[8];      /* future host services */
} ParsecOrbitHost;
```

Conventions:

- All strings are **UTF-8, NUL terminated** and must live for the whole module
  lifetime (the macro generates static literals with `concat!(..., "\0")`).
- The `log` string is a `(pointer, length)` pair and does **not** need to be NUL
  terminated.
- Log levels: `0=trace 1=debug 2=info 3=warn 4=error`.
- Structs only grow at the tail; nothing before `reserved` is changed, removed,
  or reordered.

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

## Reserved extension points

`ParsecOrbitHost::query_interface` is the entry point for host and plugin
services. In revision 1 it always returns null; the names below are reserved and
revision 2 gives them semantics (see [services.md](services.md)):

| Name | Service |
| --- | --- |
| `parsec-orbit.hooks` | Function hook registration/unregistration |
| `parsec-orbit.webview` | WebView2 bridge (inject scripts, evaluate, messages) |
| `parsec-orbit.js` | JS runtime handle, for DLL plugins that need eval |
| `parsec-orbit.kind.<kind>` | Handler that loads plugins of a non-`dll` kind (`kind.js`) |
| `parsec-orbit.config`, `parsec-orbit.market` | Settings storage, marketplace metadata |

## Host revision 2 (draft)

Revision 2 is an *additive* change within ABI 1: fields are appended to
`ParsecOrbitHost` **after** `reserved` (the revision 1 size is 96 bytes on
x64), so old plugins keep working and new plugins detect support from
`struct_size`. `reserved` stays zero and unused. `ABI_VERSION` stays `1`.

```c
typedef struct ParsecOrbitHost {
    /* ---- revision 1 (96 bytes) ---- */
    uint32_t abi_version;
    uint32_t struct_size;
    void (*log)(uint32_t level, const uint8_t *message, size_t len);
    void *(*query_interface)(const char *name);   /* rev 2: ParsecOrbitInterfaceHeader * or NULL */
    const char *loader_version;
    uintptr_t reserved[8];

    /* ---- revision 2: one table per plugin, so identity is data, not a callback argument ---- */
    const char *plugin_id;        /* manifest id (or the DLL path for manifest-less plugins) */
    const char *plugin_dir;       /* absolute, UTF-8 */
    const char *data_dir;         /* %APPDATA%\ParsecOrbit\data\<id>; may not exist yet */
    void (*log_ex)(uint32_t level, const uint8_t *target, size_t target_len,
                   const uint8_t *message, size_t len);
    int32_t (*register_interface)(const char *name, const ParsecOrbitInterfaceHeader *table);
    int32_t (*unregister_interface)(const char *name);
    uintptr_t reserved2[8];
} ParsecOrbitHost;
```

Rules:

* A plugin may use a revision 2 field only if
  `host->struct_size >= offsetof(ParsecOrbitHost, <field>) + sizeof(<field>)`
  and the pointer is non-null.
* The loader passes **each plugin its own `ParsecOrbitHost`** (allocated once,
  valid for the process lifetime); revision 1 passed one shared table.
* `log_ex` writes `target` (usually the plugin id) alongside the message into
  the loader log; `log` keeps working and logs without a target.
* The descriptor is unchanged in revision 2: dependencies and ordering come
  from the manifest ([loading.md](loading.md)).

Service registration, interface headers and the kind-handler contract are in
[services.md](services.md). The manifest is specified in
[plugin-manifest.md](plugin-manifest.md).

## Security notes

DLL plugins are native code and run in the client process with the same
privileges: they can read and write all of the client's memory. Only install
plugins you trust; the loader does no sandboxing or signature checking
(marketplace signature verification is on the roadmap).

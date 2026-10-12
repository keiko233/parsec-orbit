# Parsec Orbit plugin ABI reference

This document describes the `parsec-orbit-plugin-sdk` in
`parsec-orbit-plugin-sdk/`; the two must
stay in sync. Change the ABI, change this document. This ABI uses the
`ParsecOrbit*` export and type names and the `parsec_orbit_plugin!` SDK macro.

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
  the migration path is recorded in `docs/roadmap.md`.

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

`ParsecOrbitHost::query_interface` is the entry point for future host services.
Today it always returns null, but the names are fixed and implementations must
keep their semantics:

| Name | Planned service |
| --- | --- |
| `parsec-orbit.hooks` | Function hook registration/unregistration |
| `parsec-orbit.webview` | WebView2 bridge (run scripts in the UI, listen for messages) |
| `parsec-orbit.js` | JS runtime (Boa) handle, for JS plugins and DLL plugins that need eval |
| `parsec-orbit.market` | Marketplace metadata (install source, available updates) |

## Plugin manifest

`plugin.toml` fields (all optional; missing values fall back to defaults):

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `id` | string | directory name | unique ID, used for de-duplication |
| `name` | string | empty | display name |
| `version` | string | `-` | version |
| `kind` | `dll` / `js` | `dll` | `js` needs the JS runtime; currently only logged and skipped |
| `entry` | string | first dll in the directory | entry file (relative to the plugin directory) |
| `author` / `description` / `license` | string | empty | metadata |
| `api` | integer | the loader's ABI | required loader ABI version |
| `enabled` | bool | `true` | set `false` to disable a plugin temporarily |

## Security notes

DLL plugins are native code and run in the client process with the same
privileges: they can read and write all of the client's memory. Only install
plugins you trust; the loader does no sandboxing or signature checking
(marketplace signature verification is on the roadmap).

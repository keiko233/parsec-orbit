# Plugin manifest (`plugin.toml`)

Status: fields marked **(draft)** are specified but not yet implemented.

A plugin lives in its own directory containing `plugin.toml` and the entry file. All fields are optional unless stated; unknown keys are ignored (forward compatibility).

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `id` | string | directory name | Unique id, reverse-DNS style. Used for de-duplication and dependencies. |
| `name` | string | empty | Display name. |
| `version` | string | `-` | Semver. |
| `kind` | string | `dll` | `dll` is handled by the loader; any other value is handled by the plugin registered as `parsec-orbit.kind.<kind>` (see [services](services.md#kind-handlers)). `js` is served by the JS runtime plugin. |
| `entry` | string | first `*.dll` in the directory | Entry file, relative to the plugin directory. |
| `author`, `description`, `license` | string | empty | Metadata. |
| `api` | integer | loader's `ABI_VERSION` | Required loader ABI version; newer than the loader means skip. |
| `enabled` | bool | `true` | `false` disables the plugin without deleting it. |
| `requires` **(draft)** | array of string | `[]` | Plugin ids (or ids in `provides`) that must be loaded *successfully before* this plugin. Missing or failed dependency => this plugin is skipped. |
| `optional` **(draft)** | array of string | `[]` | Soft dependencies: only influence load order. |
| `provides` **(draft)** | array of string | `[]` | Virtual ids this plugin satisfies in addition to its own `id` (lets alternative implementations of a service exist, e.g. another JS runtime). |
| `permissions` **(draft)** | array of string | `[]` | Capabilities requested by sandboxed kinds (`webview`, `config`, `net`, `fs`). Ignored for `dll`, which is unrestricted native code. |

Rules:

* Version constraints in `requires` (for example `"id@^0.1"`) are **reserved syntax**: the loader logs a warning and treats the entry as the bare id until constraint support is specified.
* The manifest is authoritative for dependencies and ordering. The descriptor in a DLL is not consulted for them, because ordering must be decided before any plugin code runs.
* For `dll` plugins the descriptor's `id`, `name`, `version`, `author`, `description`, `license` must equal the manifest values (a mismatch is logged as a warning; the descriptor wins for display).

## Examples

Native plugin:

```toml
id = "com.example.hello"
name = "Hello Plugin"
version = "0.1.0"
kind = "dll"
entry = "parsec_orbit_plugin_hello.dll"
api = 1
```

JavaScript / TypeScript plugin (bundled output as `entry`):

```toml
id = "com.example.bandwidth"
name = "Bandwidth Ladder"
version = "0.1.0"
kind = "js"
entry = "main.js"
requires = ["com.parsec-orbit.js-runtime"]
permissions = ["webview"]
api = 1
```

## Open questions

* Whether `permissions` for `js` should be enforced by the runtime from day one or only declared (leaning: enforced, default-deny).
* Version-constraint syntax for `requires`.

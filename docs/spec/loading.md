# Loading rules

Status: implemented on Windows (`parsec-orbit-core`: `scan`, `plan`, `kind`) and for the macOS platform crate (`crates/parsec-orbit-loader-macos`, M0). The entries marked **macOS** are the platform differences; the launch bootstrap that installs them end-to-end is M1 ([macos-platform.md](macos-platform.md)). The per-plugin `init` contract is in [plugin-abi.md](plugin-abi.md#lifecycle).

## 0. Staged updates

Before it discovers anything, the loader promotes updates that a program running inside the client staged (the market plugin cannot replace a mapped DLL). `<Orbit root>/pending/<id>/` holds a complete plugin folder; for each such folder whose name is a valid plugin id and that contains `plugin.toml`, `<Orbit root>/plugins/<id>/` is replaced by it: the installed folder is renamed to `<id>.orbit-old`, the staged folder is renamed in, and the old one is deleted. If the second rename fails the old folder is put back and the failure is logged; leftovers `<id>.orbit-old` are deleted by the next start. Names starting with `.` are ignored (a writer's unfinished copy). Nothing is mapped at that point, so the rename cannot collide with a loaded image. Without an Orbit root (a Windows service environment without `%APPDATA%`) nothing is promoted. Implemented in `parsec_orbit_core::staging`, called by both loaders ([ADR 0023](../design/decisions/0023-staged-update-promotion.md)).

## 1. Discovery

Plugin directories, in priority order: `PARSEC_ORBIT_PLUGIN_DIR`
(`;`-separated), `<client dir>\plugins`, `%APPDATA%\ParsecOrbit\plugins`.
Duplicate directories are dropped case-insensitively, first spelling wins. A
plugin is a subdirectory with `plugin.toml`, or a loose `*.dll` without a
manifest (legacy layout, no dependencies). The first occurrence of an `id`
wins; later duplicates are logged and ignored. Loose DLLs are deduplicated by
their absolute path.

**macOS.** The directories are `PARSEC_ORBIT_PLUGIN_DIR` (`:`-separated) and
`<Orbit root>/plugins` (`~/Library/Application Support/ParsecOrbit/plugins`);
there is no client-directory entry. A plugin is a subdirectory with
`plugin.toml`; loose libraries are not supported. Duplicates are dropped by
canonicalised path.

## 2. Selection

A candidate is dropped when `enabled = false` (*skipped*), when its `api` is
newer than the loader (*failed*, with a warning), when its manifest does not
parse (*failed*), or when its id duplicates an earlier one (*skipped*, even if
the earlier one was disabled, rejected or failed to load). An explicit id is
also checked before anything else: it must be a valid dotted identifier and
not a Windows device name (`CON`, `NUL`, `COM1`...). A manifest without `id` is
known by the name of its directory. A plugin directory without `plugin.toml`
is counted as *skipped*. Constraint syntax in `requires` and `optional` entries
is reserved: the bare id is used and a warning is logged.

## 3. Ordering

1. Build a graph from `requires` and `optional` over ids and `provides` aliases.
2. Topologically sort. Ties keep discovery order (priority, then directory name), so load order is deterministic. A loose `*.dll` takes part with no edges and cannot be named as a dependency.
3. A cycle marks every plugin on it as *failed* with the cycle printed (`dependency cycle: a -> b -> a`, following `requires` from a plugin to its dependency). Plugins that merely wait for a cycle are ordered after it and skipped because their dependency did not load. `optional` edges take part in cycle detection too.
4. A `requires` entry that no candidate satisfies marks the plugin as *skipped*: `skipped, needs com.parsec-orbit.js-runtime, not installed`. `optional` entries that are absent are ignored. When several plugins provide the same alias, all of them are ordered before the dependent and any one that loaded satisfies it.

## 4. Per-plugin load

For each plugin in order:

1. If a `requires` dependency was skipped or failed (no provider of it loaded): `skipped, dependency X did not load`.
2. `kind = dll` (Windows) or `kind = dylib` (macOS), the *native kinds*: map the library, run `Query` / validation / `Init` as in the ABI document, passing that plugin's own host table. A loader handles only the native kind of its own platform and never asks the registry about either native kind. A plugin whose kind is the other platform's native kind is skipped: `skipped, kind "dylib" is not supported on Windows` / `skipped, kind "dll" is not supported on macOS (use "dylib")`. The plugin id is the manifest id (the descriptor id, then the library path, for a loose DLL); a manifest field that differs from the descriptor is logged once (`manifest and descriptor disagree on ...`) and the descriptor is shown.
3. Other kinds (not `dll` and not `dylib`): look up the interface `parsec-orbit.kind.<kind>`; if missing, skip with `no handler for kind "<kind>" (is the runtime plugin installed?)`; if it is not a usable 1.x handler, fail; otherwise call its `load_plugin` (the manifest needs an `entry`). Success is logged as `loaded <id> through the "<kind>" handler`.
4. A non-zero result is a failure (`init returned N`, `kind handler returned N`). Interfaces the plugin registered stay registered (there is no unload), but the failed id is recorded so dependents are skipped.

Everything runs sequentially on the loader worker thread after the warm-up delay. A plugin that needs to do work later must spawn its own thread.

## 5. Summary

The loader logs `ready: N loaded, M skipped, K failed` and, per decision, one line naming the plugin and the reason (`<id> <version>: skipped, <reason>`). The e2e suite asserts these lines.

## 6. Debug console

`PARSEC_ORBIT_CONSOLE` makes the loader open a console window on its worker
thread before the first line is logged and mirror every line of the loader log
into it, unchanged and in the same order. It picks which instances do that:

| Value | Opens a window in |
| --- | --- |
| unset, or anything else | no instance (the default) |
| `1`, `on`, `true`, `yes` | every instance |
| `client` | only the instance the user started |
| `host` | only the instance the service started |

The values are spelled exactly: `ON` and `Client` are not recognised. The
client runs twice on a normal machine - the Parsec service starts one instance
for hosting, which runs as `LocalSystem` (`S-1-5-18`), and the user starts the
other - and a Windows console belongs to a single process, so `1` means one
window per instance while `client` and `host` keep a debug session to one. The
two are told apart by the process token's user, and that is the only thing
consulted: a service configured to run under another account would count as a
client instance, and a token that cannot be read counts as a client's. An
instance the setting does not cover logs that it was skipped. A process that
already has a console - a debugger, a host started from a terminal - reuses it
instead of opening a second window.

The console never changes the load: one that cannot be opened is a warning in
the log file, and nothing else about the load differs. Ctrl+C and Ctrl+Break
typed into the window are handled and leave the client running, so a stray
keypress cannot end a session.

Closing the window is the one event that cannot be survived: Windows sends
`CTRL_CLOSE_EVENT` so that the process can clean up and then terminates it
whatever the handler returns (verified on Windows 11, with and without a
detach attempt). Closing the debug window therefore ends the client, and the
log file is where the session is read afterwards. The loader does not install
a handler that pretends otherwise.

The window also follows Parsec's own log, `%APPDATA%\Parsec\log_cl.txt` (the
client instance) and then `%APPDATA%\Parsec\log.txt` (the instance that
hosts), and prints appended lines with a `[parsec] ` prefix, so both sides of
a session are visible in one place. Each file is opened for reading on every
poll and closed again, which never blocks Parsec's own rotation; a file that
was replaced or truncated is followed from its beginning again. A log that is
already there is followed from its end - the lines of previous runs are not
repeated - except for one of at most 64 KiB written within the last 15 seconds,
which is a rotation of the current run and is shown from its beginning, so its
first lines are not lost. A line that grows past 64 KiB without a line break is
printed as it stands instead of being buffered further.

## Open questions

* Per-plugin `init` timeout/watchdog (currently none; a hung `init` blocks later plugins).

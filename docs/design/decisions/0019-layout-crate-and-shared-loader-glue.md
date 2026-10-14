# 0019 - One `layout` crate and shared loader glue

## Context
macOS was added by copying the Windows loader's shape. The two loaders each re-implemented the path rules (plugin directories, log candidates, data root, duplicate removal, list splitting), the log line format, the list of mapped modules and the `ready:` line. The installer, the market and the scripts repeated the paths a third to sixth time.

## Decision
* `parsec-orbit-layout` (std only, no dependencies) owns the path rules per OS. `Layout::resolve(os, inputs, lookup)` is a pure function of the OS, the client directory or home, and an injected environment lookup; both loaders' `Config` are built on it and keep only what is theirs (the Windows DevTools port, host check and console; the macOS orbit root record). The variables and their precedence are unchanged.
* `parsec_orbit_core::log::format_line` is the one log line format; the sinks stay OS-specific (`WriteFile`, `open(O_APPEND)`).
* `parsec_orbit_core::Session` owns the mapped modules and logs the `ready:` summary; each loader holds one `static SESSION`.

## Consequences
* The path rules of both OSes are unit-tested on every host, including the Windows ones.
* The installer and the market plugin depend on `layout` instead of copying paths, without linking `core`'s host state.
* Adding an OS adds an `Os` variant and its rules in one place.

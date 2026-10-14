# 0008 - Native plugins are `kind = "dylib"` on macOS

## Context
Manifests name the native kind `dll`, the Windows library type. The first plan kept the value `dll` as a wire value on macOS and resolved only the file suffix per platform. The maintainer decided against it: a name that says `dll` over a Mach-O library is misleading.

## Decision
`dylib` is the native kind of macOS and `dll` the native kind of Windows. Each loader handles only its own native kind and never looks up a kind handler for either. A manifest with the other platform's native kind is skipped with an explicit message. The default `kind` and the default `entry` suffix are those of the platform (`dylib`/`*.dylib` on macOS). Loose libraries without a manifest are not supported on macOS. The kind names `dll` and `dylib` are not served by `parsec-orbit.kind.*` handlers.

## Consequences
* A native plugin package is per platform; the marketplace index gains an optional `platform` field (default `windows-x86_64`) and the macOS tool signs `.dylib` files ad-hoc before installing ([macos-platform.md](../../spec/macos-platform.md#9-marketplace-on-macos)).
* `js` plugins stay portable.
* An old Windows loader meeting `kind = "dylib"` reports `no handler for kind "dylib"` and skips the plugin; the new Windows loader says the kind is unsupported. Both skip; neither fails.
* Manifest tests, `scan` and the market's default kind move from a constant to a per-platform value (M0).
* A single manifest serving both platforms is an open question.

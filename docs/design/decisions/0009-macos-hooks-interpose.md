# 0009 - macOS hooks rebind imported symbols

## Context
`parsec-orbit.hooks` is backed on Windows by `retour` inline detours. On arm64 macOS `retour 0.3.1` has no backend (52 compile errors, upstream is x86-only); `mprotect` on `__TEXT` is refused (EACCES) even for a plain process; fresh RX pages work in a non-hardened ad-hoc process but give no way to patch existing code. The client dylib is stripped, with `wx_main` as its only export.

## Decision
The macOS hooks backend is dyld interposition plus runtime rebinding of imported-symbol pointers (a fishhook-style rebind of lazy and non-lazy bindings across all images, including images loaded later). The interface and the error codes stay; the semantics are narrower: only imported functions can be hooked, `*original` is the unmodified target, and unsupported targets return `-4`. Static interposition is used only by the loader for its own fixed set ([macos-host.md](../../spec/macos-host.md#62-interposition)). Inline hooks are not provided now.

## Consequences
* Hooks on system and library functions the client imports (network, file, Objective-C runtime) work in principle; hooks on the client's own functions do not.
* A plugin must treat "not supported" as a normal answer.
* Writing import pages (`__DATA_CONST`) with `vm_protect` is unverified and is a gate of M2.
* Inline hooks can be added later behind the same interface without an ABI change if a safe write path is found.

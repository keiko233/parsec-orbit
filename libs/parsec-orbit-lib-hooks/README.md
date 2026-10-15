# Hooks

Official Parsec Orbit plugin providing the parsec-orbit.hooks service.

On Windows, hooks use retour inline detours. The target function body is
patched, a trampoline calls the unhooked function, and hooks start disabled.

On arm64 macOS, hooks rebind imported function pointers in loaded Mach-O
images. The target function body remains unchanged, and the original pointer
returned by install is the target itself. Resolved image paths are copied by
dyld callbacks, and each resolved image is pinned for the process lifetime.
The original pointer remains valid after disable and remove. Images loaded later
are processed by dyld callbacks.

The macOS backend accepts classic 64-bit arm64 Mach-O images with indirect
symbol tables and `S_LAZY_SYMBOL_POINTERS` or `S_NON_LAZY_SYMBOL_POINTERS`
sections. It rejects images with chained fixups, unknown required load
commands, or malformed/unreadable metadata. A slot is eligible only when its
current value exactly equals the resolved target. Unresolved lazy stubs are
left untouched; install returns `ERR_NOT_READY` when no already-bound matching
slot exists. Late images use the same bound-pointer check from dyld callbacks.
A target without a matching slot in a supported loaded image returns
`ERR_NOT_READY`.

Parser input is capped at 16 MiB of load commands, 64 MiB per linkedit read,
512-byte symbol and library names, and 16,384 pointer slots per image. The
registry holds at most 256 registered hooks, 65,536 rebound slots per hook,
and 4,096 permanently pinned resolved images.

Direct calls, unresolved lazy stubs, chained-fixup images, and client-local
functions without an imported pointer are outside coverage. A macOS unit
regression verifies that an executable address used as an unresolved stub is
not accepted as the original target. The scratch integration test exercises a
Hardened Runtime executable signed ad hoc with only
`com.apple.security.cs.disable-library-validation`, plus owned ad-hoc-signed
dylibs. It covers an `RTLD_LAZY` late image, symbol lifetime after the caller
closes its dylib handle, enable/disable/remove, and an unsupported local target.
The same lifecycle passed for an owned linked function in guarded official
101a, 104a and 105c client processes (`scripts/fixtures/macos-hooks-probe.c`).
These measurements do not establish hooks on vendor internals. The backend
does not patch executable bytes, use JIT permissions, or
request additional entitlements.

A detour must have the exact signature and calling convention of the target
and must never unwind. Hooks are process-wide; plugins remain responsible for
what their detours do and for application-level synchronization.

The checked-in plugin.toml is the Windows package manifest. For a macOS
package, use plugin.macos.toml and name the built dylib
libparsec_orbit_lib_hooks.dylib. Both manifests provide the same service.

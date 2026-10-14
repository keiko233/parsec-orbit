# Owned macOS client probes

These are development fixtures for an explicitly selected disposable client,
not product plugins or installation scripts. Use fresh private Orbit state,
ad-hoc-sign each dylib, and install a test manifest with `kind = "dylib"`.
The existing acceptance controller stops only the run-owned identity.

`macos-hooks-probe.c` requires `parsec-orbit.hooks`. Build an owned helper
exporting `int orbit_probe_target(int value) { return value + 1; }` as
`liborbit_probe_owned.dylib`, with install name
`@loader_path/liborbit_probe_owned.dylib`. Build both helper and probe with
`clang -arch arm64 -dynamiclib -Wl,-no_fixup_chains`; link the probe to the
helper, keep them in the same plugin directory and sign both. The manifest
id is `com.example.hooks-probe`, with dependency `com.parsec-orbit.hooks`.
It queries the shared client ABI and measures only the owned imported
function's install/enable/disable/re-enable/remove lifecycle. No vendor
function or executable byte is changed.

`macos-ui-probe.m` is a legacy-export inventory fixture. Build using
`clang -arch arm64 -dynamiclib -fobjc-arc -framework AppKit -framework Foundation`,
with `PROBE_RESULT` defined as a C string naming a result file inside the
run-owned private directory. It runs on the main queue, observes only view
class names and loaded image names, and retries an early empty tree for up
to 15 attempts. It does not read page content, infer a URL, implement a
WebView backend, or inspect credentials. Internal NSApp windows do not
necessarily have on-screen CGWindowList entries.

Evidence and limits: [client checkpoint](../../docs/design/macos/client-checkpoint.md).

# Build and stage the UI companion for development

The UI plugin owns the `parsec-orbit.ui` ABI and registry. The adjacent
`parsec-orbit-ui-companion[.exe]` process owns the native `winio` window. This
guide stages both artifacts in a disposable plugin directory for development;
it does not define public release packaging. The service contract is in
[spec/ui-service.md](../spec/ui-service.md), and the process choice is in
[ADR 0018](../design/decisions/0018-ui-companion-process.md).

## Build

Build both packages for the target platform from the workspace root:

```sh
cargo build --release -p parsec-orbit-plugin-ui -p parsec-orbit-ui-companion
```

On macOS, build for the supported arm64 target if it is not the Rust host:

```sh
cargo build --release --target aarch64-apple-darwin \
  -p parsec-orbit-plugin-ui -p parsec-orbit-ui-companion
```

The UI protocol uses ordinary `serde`/`serde_json` in the plugin. GUI
dependencies belong to the companion crate; the protocol does not need to be
`no_std`.

The Windows helper uses winio's `winui` backend (WinUI 3). The pinned winio
0.12.0 release documents Windows 11 21H2+ as its baseline. Install a compatible
x64 [Windows App Runtime](https://learn.microsoft.com/en-us/windows/apps/windows-app-sdk/downloads)
before running the helper. Winio discovers the installed runtime; a successful
Rust build alone does not prove this runtime prerequisite is present. Orbit
does not download or install the runtime automatically. Neither the loader nor
the UI service plugin links the WinUI GUI stack.

## Stage a disposable plugin directory

Create a scratch directory outside any installed Parsec bundle, then place the
platform manifest, plugin library and helper executable together:

| Platform | Manifest | Plugin library | Helper |
| --- | --- | --- | --- |
| Windows x64 | `plugin.toml` | `parsec_orbit_plugin_ui.dll` | `parsec-orbit-ui-companion.exe` |
| macOS arm64 | `plugin.macos.toml` renamed to `plugin.toml` | `libparsec_orbit_plugin_ui.dylib` | `parsec-orbit-ui-companion` |

For example, on macOS after the explicit arm64-target build:

```sh
scratch="${TMPDIR:-/tmp}/parsec-orbit-ui-dev"
plugin="$scratch/parsec-orbit-plugin-ui"
mkdir -p "$plugin"
cp crates/parsec-orbit-plugin-ui/plugin.macos.toml "$plugin/plugin.toml"
cp target/aarch64-apple-darwin/release/libparsec_orbit_plugin_ui.dylib "$plugin/"
cp target/aarch64-apple-darwin/release/parsec-orbit-ui-companion "$plugin/"
codesign --force --sign - "$plugin/libparsec_orbit_plugin_ui.dylib"
codesign --force --sign - "$plugin/parsec-orbit-ui-companion"
```

Point `PARSEC_ORBIT_PLUGIN_DIR` at the scratch parent directory when launching a
disposable development host. Keep the plugin library and helper side by side so
the plugin can resolve and launch the matching renderer.

On Windows, stage the release artifacts in a disposable directory with PowerShell:

```powershell
$scratch = Join-Path $env:TEMP ('orbit-ui-dev-' + [guid]::NewGuid())
$plugin = Join-Path $scratch 'parsec-orbit-plugin-ui'
New-Item -ItemType Directory -Path $plugin | Out-Null
Copy-Item -LiteralPath crates/parsec-orbit-plugin-ui/plugin.toml -Destination $plugin
Copy-Item -LiteralPath target/release/parsec_orbit_plugin_ui.dll -Destination $plugin
Copy-Item -LiteralPath target/release/parsec-orbit-ui-companion.exe -Destination $plugin
```

These files are development staging; running `scripts/install.ps1` instead
modifies an installed client and is not part of the owned-host tests below.

## Validation boundary

This is development staging only. It does not modify the macOS release artifact
allowlist or make a public packaging claim. Before treating the macOS helper as
supported, verify its code signature and runtime-state access from the actual
parent client process. Codesigning behavior and compatibility with the client's
runtime state sandbox are currently unverified. Use an isolated disposable
Parsec state and bundle for that validation.

## Exercise a standalone host

The owned host example loads no Parsec code and uses no installed client state:

```sh
cargo build --locked -p parsec-orbit-plugin-ui --example companion-host \
  -p parsec-orbit-ui-companion
cargo run --locked -p parsec-orbit-plugin-ui --example companion-host -- \
  "$PWD/target/debug/parsec-orbit-ui-companion"
```

It shows all supported control types and prints returned action JSON. It exits
after 60 seconds and reaps its helper; set `ORBIT_UI_PROBE_SECONDS` to change the
probe duration. This demonstrates the renderer and pipes in an owned host, not
compatibility with a real signed-in client or the production sandbox.

On Windows, the ABI host can use the staged directory:

```powershell
cargo run --locked -p parsec-orbit-plugin-ui --example service-host -- $plugin
```

The real event-loop regression requires an interactive Windows or macOS desktop
session and is explicit:

```sh
cargo test --locked -p parsec-orbit-ui-companion --test pipe_lifecycle -- --ignored
```

It checks idle shutdown, pipe EOF after startup and parent exit before window
construction. The standard workspace tests use headless peers for crash/replay,
callback isolation, cancellation and stale visibility events.

Windows also has an explicit WinUI 3 desktop regression:

```powershell
cargo test --locked -p parsec-orbit-ui-companion --test windows_desktop -- --ignored
```

It locates only its owned child's window, selects the configuration tab through
UI Automation and checks rendered bounds, initial text/readonly values, text
editing and button actions over IPC, and state updates. It also checks hidden
startup, visibility requests, minimized-window restoration, close-to-hide and
reactivation. It does not require Parsec or assert foreground ownership against
Windows focus policy. UI Automation does not establish physical mouse or IME
behavior.

The ABI host example also exercises plugin `init` and the SDK service table:

```sh
cargo run --locked -p parsec-orbit-plugin-ui --example service-host -- \
  "$PWD/target/debug"
```

This uses `MockHost` with an explicitly selected plugin directory, updates a
section through the real service methods and exits without loading Parsec.

Current measured results and remaining acceptance gates are recorded in the
[UI companion checkpoint](../design/ui-companion-checkpoint.md).

# Decision records

One file per decision, numbered, never rewritten after acceptance (supersede with a new record).
Format: Context, Decision, Consequences.

| # | Title | Status |
| --- | --- | --- |

| [0001](0001-typed-service-vtables.md) | Host services are typed, size-prefixed vtables | Accepted |
| [0002](0002-sdk-is-no-std-by-default.md) | The Rust SDK is `no_std` by default | Accepted (subject to the size spike) |
| [0003](0003-js-runtime-is-a-plugin.md) | The JS runtime is a plugin | Accepted |
| [0004](0004-loader-stays-light.md) | The loader stays light | Accepted |
| [0005](0005-boa-as-js-engine.md) | Boa as the JS engine | Accepted |
| [0006](0006-network-installer.md) | Orbit Manager: PowerShell orchestration, Rust verification | Accepted |
| [0007](0007-macos-isolated-state.md) | The macOS host has its own state directory | Accepted |
| [0008](0008-macos-dylib-kind.md) | Native plugins are `kind = "dylib"` on macOS | Accepted |
| [0009](0009-macos-hooks-interpose.md) | macOS hooks rebind imported symbols | Accepted |
| [0010](0010-macos-webview-backend.md) | One WebView service, an in-process backend on macOS | Accepted |
| [0011](0011-macos-scope.md) | macOS scope: arm64, client mode | Partially superseded by 0014 (signing scope) |
| [0012](0012-macos-launch-and-install.md) | macOS launch is a normal app launch of `Parsec Orbit.app`; the installer signs | Superseded by 0014 |
| [0013](0013-macos-client-compatibility.md) | Client builds are detected, logged and checked against a verified list | Accepted |
| [0014](0014-macos-patched-bundle.md) | macOS loads Orbit through a patched official app bundle | Accepted direction; product gates open |
| [0015](0015-macos-loader-as-host.md) | macOS runs the client from a host we control | Superseded by 0014 |
| [0016](0016-orbit-ui-winio.md) | Orbit UI: External winio companion window with unified declarative ABI | Accepted |
| [0017](0017-one-sdk-package-no-features.md) | One SDK package without features; `sdk-std` dissolved | Accepted |
| [0018](0018-crate-roles-and-layout.md) | Crate roles and directory layout | Accepted |
| [0019](0019-layout-crate-and-shared-loader-glue.md) | One `layout` crate and shared loader glue | Accepted |
| [0022](0022-official-registry-in-manifest.md) | The official registry is `manifest/` on `main` | Accepted |
| [0023](0023-staged-update-promotion.md) | Staged updates are promoted by the loader | Accepted |

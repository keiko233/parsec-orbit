# UI companion checkpoint

Date: 2026-10-15. Branch: `codex/ui-companion`, based on `main` at `6fc5c26`.
This checkpoint covers the independent winio renderer selected in
[ADR 0018](decisions/0018-ui-companion-process.md). It is development evidence,
not a release or installed-client acceptance record.

## Implemented behavior

The UI plugin keeps the unchanged version 1.0 service ABI, schemas, authoritative
state and subscriptions. The adjacent renderer owns the main-thread winio window.
Only bounded, versioned JSON frames cross inherited stdin/stdout pipes. There is
no socket listener and no GUI dependency in the service plugin or loader.

The supervisor returns promptly from init, mirrors the latest state, uses finite
restart attempts and ignores events from previous children. Visibility revisions
prevent delayed close events from overriding newer requests. Callback delivery
runs separately, binds subscriptions when actions are accepted and permits
cancellation without calling plugin code under registry locks. Renderer input
bursts coalesce to the latest snapshot instead of retaining an unbounded queue.

The renderer preserves editable text during user input, keeps scroll position on
state-only updates, lays out new macOS sections from their actual viewport and
starts them at the top. macOS has a signaled run-loop wake source, explicit
AppKit launch completion and native Edit shortcuts. These platform calls support
winio; they do not introduce a second control renderer.

## Measured evidence

All GUI observations used owned standalone hosts and generated probe bundles.
No installed Parsec application, real Parsec configuration or account was used.

| Check | Result and scope |
| --- | --- |
| `cargo fmt --all -- --check` | Passed. |
| `cargo test --workspace --locked` | 514 passed, 0 failed, 2 ignored; the real desktop lifecycle test is explicitly ignored in the default gate. |
| `pnpm install --frozen-lockfile`, recursive build and tests | Passed; 26 JavaScript tests passed. |
| Targeted Clippy, all targets | Completed; only the pre-existing boolean-assert style warning in the UI service library test remains. |
| Windows service plugin, all-targets `cargo check` | Passed for `x86_64-pc-windows-msvc`. This is compile-only. |
| Headless process integration | Crash/restart/state replay, slow callback isolation, child reaping, incompatible protocol retry exhaustion and stale close suppression passed. |
| Callback tests | Self-cancellation, cancelling another snapshot entry, concurrent in-flight cancellation, reentrant state updates and old queued events excluding new subscribers passed. |
| Real AppKit lifecycle regression, explicit `--ignored` run | Idle shutdown, EOF after readiness and EOF before GUI construction passed. |
| Owned custom SBPL host | The same real child lifecycle regression passed under an allow-default profile denying a generated protected scratch directory. This is not the production client profile. |
| Native rendered form | Initial scroll at top, visible controls, checkbox, button, codec selection, slider, JSON editing and Unicode paste verified. Host logs contain corresponding action payloads. |
| Numeric keyboard entry | Command+A followed by typing `75` retained `75` and emitted numeric `75.0`; editing does not rewrite the caret position. |
| Window resize and close | Native zoom preserved content and edits. Closing hid the window, returned desired visibility false and kept the child alive. |
| SDK ABI host | Owned `MockHost` exercised real plugin init, service registration, show/hide and state updates. It does not load vendor code. |
| Release artifact signatures | Both generated arm64 artifacts passed `codesign --verify --strict`. This does not establish notarization or distribution acceptance. |

## Release-profile sizes

Measured on `aarch64-apple-darwin`, `rustc 1.99.0-nightly (c4af71034 2026-07-06)`,
using the workspace release profile. Digests identify these local build artifacts,
including their generated signatures; a build at another path may differ.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `libparsec_orbit_plugin_ui.dylib` | 555,904 | `af0b083cf79c0a62de1b779eca4b139dc4794b8640aa1137406fc35353457695` |
| `parsec-orbit-ui-companion` | 524,032 | `49e39aa24bdfcf0ae247737a7af7b1938e2f4b57fec5f867af07aa9c983ab77e` |

The transport and callback lifecycle account for the service plugin's std/JSON
cost. GUI code lives only in the renderer. Neither the loader nor core changes
or acquires a GUI dependency; no Windows loader size claim is inferred from
these arm64 measurements.

## Unverified and deferred

* The Windows renderer cross-check is blocked on this macOS host by missing
  `llvm-rc` in winio's Windows resource build. No stub resource compiler was used.
  Windows runtime, activation and window behavior require Windows verification.
* Launching from the real patched client, its inherited production isolation
  policy, client full-screen/streaming focus and real keyboard IME composition
  remain unverified. Unicode paste is not an IME acceptance test.
* macOS external CLI/hotkey activation is not implemented by this branch.
  Windows retains the existing host hotkeys and CLI channel.
* Public release packaging and the macOS release artifact whitelist stay deferred.
  Development staging must install the matching helper beside the plugin library;
  the Windows development installer checks and copies that helper.

See the [development guide](../guides/ui-companion.md) for build, staging and
owned-host commands.

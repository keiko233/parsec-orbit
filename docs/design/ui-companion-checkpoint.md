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

* The original macOS-host Windows renderer cross-check was blocked by missing
  `llvm-rc`. Native Windows follow-up evidence is recorded below.
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

## Windows follow-up (2026-10-15)

The native Windows build exposed a `Result` name collision in winio's Windows
prelude; the process entry point now explicitly uses `std::result::Result`.
The UI plugin checks the process session ID instead of assuming that only
LocalSystem runs in session 0. Session 0 and a failed session lookup keep the
service table without launching desktop UI or hotkeys.

The Windows helper now selects winio's WinUI 3 backend, without the Win32
backend or its optional theme hooks ([ADR 0019](decisions/0019-windows-ui-winui3.md)).
This host already has x64 Windows App Runtime 1.8 and 2 packages; no runtime
installation or system configuration change was made. The helper's installed
runtime dependency is separate from its executable size.

All follow-up runs use owned test processes and synthetic forms, without an
installed Parsec client or real user configuration. The real event-loop
regression now also runs explicitly on Windows: idle shutdown, EOF after
readiness and EOF before GUI construction passed. The SDK `service-host`
registered the service, showed/hid the helper and updated state through the ABI.

| Check | Windows result and scope |
| --- | --- |
| `cargo check --locked -p parsec-orbit-ui-companion --all-targets` | Passed with the native Windows resources and backend. |
| `cargo test --workspace --locked` with `PARSEC_ORBIT_REQUIRE_NODE=1` | 550 passed, 0 failed, 3 ignored, including the two explicitly run desktop regressions. |
| `pnpm install --frozen-lockfile`, recursive build and tests | Passed; 26 JavaScript tests passed. |
| `companion_process` | All four Windows process tests passed, including a process-handle wait proving the supervisor terminates its owned child. |
| Real Windows event-loop regression | Ten consecutive WinUI runs passed idle shutdown, EOF after readiness and EOF before GUI construction. |
| Real WinUI desktop regression | Ten consecutive runs passed hidden startup, UI Automation tab selection, nonempty rendered control bounds, initial text/readonly values, text editing and button invocation returning IPC actions, state updates, repeated minimized-window restoration, close-to-hide, reactivation and shutdown. This is not physical mouse or IME acceptance. |
| Owned all-control host | The release helper stayed connected while the existing `companion-host` constructed all eight supported control types. This is a startup smoke test, not interaction coverage of every type. |
| Targeted Clippy, all targets | Passed with the pre-existing UI library boolean-assert style warning. |
| New desktop regression Clippy | Passed with `-D warnings`. |

Rust emitted incremental-cache finalization notes (`Access denied`) on the ReFS
checkout. The commands above still completed successfully; no toolchain or
filesystem setting was changed to suppress those notes.

Release artifacts built on `x86_64-pc-windows-msvc`, with
`rustc 1.100.0-nightly (a36d05efa 2026-09-09)` and the workspace static-CRT release
profile:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `parsec_orbit_plugin_ui.dll` | 563,200 | `985b341a57e6c0445b99fb337a64ec1101b50b124797f6678e919c9843a669ef` |
| `parsec-orbit-ui-companion.exe` (WinUI 3) | 542,720 | `59598c76ccf43c4e2cc9c43c6b48b37700c9a67838b8a5a37ef1f369262955e9` |

Neither the loader nor core gains a dependency. These sizes describe the
separate UI artifacts, not the loader budget. Real client streaming/full-screen
focus, keyboard IME composition and installed-client acceptance remain
unverified. Windows foreground focus remains subject to OS restrictions.

The superseded Win32 desktop probe observed one teardown CFG fast-fail
(`0xc0000409`, subcode 10). WER listed an injected RTSS module, but that does
not establish its cause. Fifteen immediate desktop reruns and 100 owned
debugger lifecycle runs did not reproduce it. This is not evidence that the
WinUI switch diagnoses or fixes that Win32 failure; the final backend has its
own acceptance results above.

## Settings follow-up (2026-10-16)

The companion now renders dynamic select options, read-only multiline text,
multiline drafts and a common restart footer. The UI service contributes the
Overview and Log panels. The bandwidth and STUN panels persist their settings;
their normative contracts are [bandwidth](../spec/high-bitrate.md),
[STUN](../spec/stun-swap.md) and [built-in settings](../spec/ui-settings.md).

Validation on Windows:

- `cargo test --workspace --locked` with `PARSEC_ORBIT_REQUIRE_NODE=1`: 719
  passed, zero failures, four ignored (including explicit desktop tests and a
  documentation example).
- Both WinUI desktop tests passed with the default test scheduling, including
  multiline controls, dynamic options without echo actions, drafts surviving
  unrelated snapshots, one Overview, and the shared restart action.
- The explicit pipe-lifecycle test passed, covering shutdown and parent EOF.
- Recursive TypeScript build and all 26 JavaScript tests passed.
- Formatting and relevant Clippy checks passed; existing workspace/legacy lint
  warnings and ReFS incremental-cache finalization notes remain.
- UI library tests follow and rotate temporary logs through the real ABI and
  validate filtering, text bounds and generated preference persistence.
- Maintenance tests validate argument/environment parsing and open an owned
  test-process handle to verify synchronization access and rejection before
  touching a service. They never stop an installed service or change a real
  user's environment.

The UI library, WebView plugin and log-tail crate pass `cargo check` for
`aarch64-apple-darwin`. Cross-checking the AppKit companion from this Windows
host stops in its existing C build dependency because `cc` is unavailable;
no macOS renderer runtime acceptance is claimed.

Release sizes and their reasons are in the [size report](size-report.md).
Actual UAC cancellation, restarting an installed Parsec service, propagation
of its console environment to real clients, and real streaming/full-screen
interaction remain manual acceptance gates. The restart action is implemented
for Windows service installations only. STUN custom endpoints currently require
port 3478 because the DNS hook preserves the requested service port.

### Same-session host activation fix

Read-only diagnostics found the service's System host and its user client in
session 1, each owning an activation window. The host log identified itself as
`host` and repeatedly reported `UI pipe closed`; the user's companion was
running but hidden. Posting activation directly to the user client's window
showed it successfully. A nonzero session alone therefore does not establish
UI ownership.

ADR 0026 requires a readable non-System token as well as a nonzero session.
The regression first failed with the old session-only decision and then passed
with the identity guard. The UI library's 17 unit and four process tests pass;
the release service DLL builds at 640,512 B. Installed files and running Parsec
processes were not replaced or restarted during verification. Applying this
fix requires restarting the old host to release its hotkey registrations.
